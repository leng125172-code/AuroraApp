//! Host-only Canonical Workflow IR lowering, resource planning, and R2-05 typed binding closure.
//!
//! The pass consumes explicit task roots. It never infers roots from unreferenced documents and
//! never unrolls branches, waits, or backedges. A subworkflow creates exactly one isolated
//! instance per call site. Runtime execution remains the responsibility of R2-03 and later work.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, Write};
use std::str;

use serde::{Serialize, Serializer};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::diagnostic::{make_diagnostic, sort_diagnostics};
use crate::{
    Backedge, Edge, ExpandedActionBindingInput, JoinMode, JoinPolicy, Node, NodeKind,
    PlannedConditionBinding, PlannedTraceValue, StableId, TaskBindingImageInput, WaitMode,
    WorkflowConditionBindingInput, WorkflowConditionHandle, WorkflowDiagnostic,
    WorkflowDiagnosticCode, WorkflowDocument, WorkflowPortDirection, WorkflowProjectInput,
    WorkflowSource, WorkflowTraceValueHandle, WorkflowTraceValueSource, WorkflowValidationLimits,
    WorkflowValueArea, WorkflowValueSlot, WorkflowValueType, WorkflowWatchBindingInput,
    validate_project,
};

/// Canonical Workflow IR writer major version.
pub const CANONICAL_WORKFLOW_IR_MAJOR: u16 = 1;
/// Canonical Workflow IR writer minor version.
pub const CANONICAL_WORKFLOW_IR_MINOR: u16 = 0;
/// Static Workflow plan writer major version.
pub const STATIC_WORKFLOW_PLAN_MAJOR: u16 = 1;
/// Static Workflow plan writer minor version.
pub const STATIC_WORKFLOW_PLAN_MINOR: u16 = 1;
/// Trace-catalog Static Workflow plan writer minor version.
pub const STATIC_WORKFLOW_PLAN_TRACED_MINOR: u16 = 3;

/// Version carried by compiler-internal R2-02 artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WorkflowArtifactVersion {
    /// Reader-incompatible version.
    pub major: u16,
    /// Backward-compatible additive version.
    pub minor: u16,
}

impl WorkflowArtifactVersion {
    const fn canonical_ir() -> Self {
        Self {
            major: CANONICAL_WORKFLOW_IR_MAJOR,
            minor: CANONICAL_WORKFLOW_IR_MINOR,
        }
    }

    const fn static_plan() -> Self {
        Self {
            major: STATIC_WORKFLOW_PLAN_MAJOR,
            minor: STATIC_WORKFLOW_PLAN_MINOR,
        }
    }

    const fn traced_static_plan() -> Self {
        Self {
            major: STATIC_WORKFLOW_PLAN_MAJOR,
            minor: STATIC_WORKFLOW_PLAN_TRACED_MINOR,
        }
    }
}

macro_rules! handle_type {
    ($name:ident, $documentation:literal) => {
        #[doc = $documentation]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(pub u32);
    };
}

handle_type!(
    WorkflowHandle,
    "Dense payload-local Workflow template handle."
);
handle_type!(
    WorkflowNodeHandle,
    "Dense payload-local template node handle."
);
handle_type!(
    WorkflowEdgeHandle,
    "Dense payload-local template edge handle."
);
handle_type!(
    WorkflowInstanceHandle,
    "Dense expanded Workflow instance handle."
);
handle_type!(WorkflowStepHandle, "Dense static execution-step handle.");
handle_type!(ExpandedEdgeHandle, "Dense expanded control-edge handle.");
handle_type!(WorkflowWatchHandle, "Dense compiled watch handle.");

/// Raw mandatory Target Profile values used to construct [`WorkflowTargetLimits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WorkflowTargetLimitValues {
    /// Maximum explicit root Workflow assignments in one task.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_workflows_per_task: u64,
    /// Maximum source nodes in one reachable Workflow template.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_source_nodes_per_workflow: u64,
    /// Maximum source edges in one reachable Workflow template.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_source_edges_per_workflow: u64,
    /// Maximum root plus call-site-expanded instances in one task.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_expanded_workflow_instances: u64,
    /// Maximum expanded nodes in one task.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_expanded_nodes_per_task: u64,
    /// Maximum expanded edges in one task.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_expanded_edges_per_task: u64,
    /// Maximum simultaneous active-set capacity in one task.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_active_nodes_per_task: u64,
    /// Maximum node executions in one release.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_node_executions_per_release: u64,
    /// Maximum conservative Fork nesting proof.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_fork_nesting_depth: u64,
    /// Maximum outgoing branches from one Fork.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_branches_per_fork: u64,
    /// Maximum pending cancellation slots.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_pending_cancellations: u64,
    /// Maximum root-to-leaf subworkflow instance depth.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_subworkflow_expansion_depth: u64,
    /// Maximum declared traversal count on one backedge.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_backedge_traversals_per_run: u64,
    /// Maximum declared Wait count or timeout.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_wait_cycles: u64,
    /// Maximum committed Workflow state bytes in one task.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_workflow_state_bytes_per_task: u64,
    /// Maximum staging Workflow state bytes in one task.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_workflow_staging_bytes_per_task: u64,
    /// Maximum compiled watch values in one task.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_watch_handles_per_task: u64,
    /// Maximum fixed typed ports on one Action binding.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_action_ports_per_node: u64,
    /// Maximum distinct BOOL condition bindings in one task.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_condition_bindings_per_task: u64,
    /// Maximum staged Workflow Trace events in one release.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub max_trace_events_per_release: u64,
    /// Maximum configured Trace ring slots.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub workflow_trace_ring_capacity: u64,
}

/// Fully validated non-zero R2-02 Target Profile capacities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WorkflowTargetLimits(WorkflowTargetLimitValues);

impl WorkflowTargetLimits {
    /// Validates every mandatory Target Profile field before planning.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowPlanningLimitError`] naming the first zero field.
    pub fn new(values: WorkflowTargetLimitValues) -> Result<Self, WorkflowPlanningLimitError> {
        let fields = [
            ("max_workflows_per_task", values.max_workflows_per_task),
            (
                "max_source_nodes_per_workflow",
                values.max_source_nodes_per_workflow,
            ),
            (
                "max_source_edges_per_workflow",
                values.max_source_edges_per_workflow,
            ),
            (
                "max_expanded_workflow_instances",
                values.max_expanded_workflow_instances,
            ),
            (
                "max_expanded_nodes_per_task",
                values.max_expanded_nodes_per_task,
            ),
            (
                "max_expanded_edges_per_task",
                values.max_expanded_edges_per_task,
            ),
            (
                "max_active_nodes_per_task",
                values.max_active_nodes_per_task,
            ),
            (
                "max_node_executions_per_release",
                values.max_node_executions_per_release,
            ),
            ("max_fork_nesting_depth", values.max_fork_nesting_depth),
            ("max_branches_per_fork", values.max_branches_per_fork),
            (
                "max_pending_cancellations",
                values.max_pending_cancellations,
            ),
            (
                "max_subworkflow_expansion_depth",
                values.max_subworkflow_expansion_depth,
            ),
            (
                "max_backedge_traversals_per_run",
                values.max_backedge_traversals_per_run,
            ),
            ("max_wait_cycles", values.max_wait_cycles),
            (
                "max_workflow_state_bytes_per_task",
                values.max_workflow_state_bytes_per_task,
            ),
            (
                "max_workflow_staging_bytes_per_task",
                values.max_workflow_staging_bytes_per_task,
            ),
            (
                "max_watch_handles_per_task",
                values.max_watch_handles_per_task,
            ),
            (
                "max_action_ports_per_node",
                values.max_action_ports_per_node,
            ),
            (
                "max_condition_bindings_per_task",
                values.max_condition_bindings_per_task,
            ),
            (
                "max_trace_events_per_release",
                values.max_trace_events_per_release,
            ),
            (
                "workflow_trace_ring_capacity",
                values.workflow_trace_ring_capacity,
            ),
        ];
        if let Some((name, _)) = fields.into_iter().find(|(_, value)| *value == 0) {
            return Err(WorkflowPlanningLimitError::Zero(name));
        }
        Ok(Self(values))
    }

    /// Returns the validated raw values used in the plan and its digest.
    #[must_use]
    pub const fn values(self) -> WorkflowTargetLimitValues {
        self.0
    }
}

/// Host-only artifact byte capacities, separate from Target Runtime capacities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowArtifactLimits {
    canonical_ir_bytes: usize,
    static_plan_bytes: usize,
}

impl WorkflowArtifactLimits {
    /// Creates non-zero in-memory publication limits.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowPlanningLimitError`] when either byte capacity is zero.
    pub const fn new(
        max_canonical_ir_bytes: usize,
        max_static_plan_bytes: usize,
    ) -> Result<Self, WorkflowPlanningLimitError> {
        if max_canonical_ir_bytes == 0 {
            return Err(WorkflowPlanningLimitError::Zero("max_canonical_ir_bytes"));
        }
        if max_static_plan_bytes == 0 {
            return Err(WorkflowPlanningLimitError::Zero("max_static_plan_bytes"));
        }
        Ok(Self {
            canonical_ir_bytes: max_canonical_ir_bytes,
            static_plan_bytes: max_static_plan_bytes,
        })
    }
}

/// Invalid mandatory planning capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkflowPlanningLimitError {
    /// A named capacity is zero.
    #[error("Workflow planning limit `{0}` must be non-zero")]
    Zero(&'static str),
}

/// One compiled watch value and its canonical storage width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowWatchInput {
    /// Stable value identity assigned by the later binding pass.
    pub value_id: StableId,
    /// Canonical storage bytes fragmented into 32-byte Trace payloads.
    pub encoded_bytes: u64,
}

/// Explicit root assignments and fixed Trace capacities for one R0 task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskWorkflowPlanningInput {
    /// Existing R0 task handle; `u32::MAX` is reserved and rejected.
    pub task_handle: u32,
    /// Explicit root templates. Unlisted Workflow documents are never inferred as roots.
    pub root_workflow_ids: Vec<StableId>,
    /// Fixed, sorted-by-ID watch descriptors.
    pub watches: Vec<WorkflowWatchInput>,
    /// Actual preallocated Trace ring slots for this task.
    pub trace_ring_capacity: u64,
}

/// One exact byte range within a stable storage target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WorkflowWriteRegion {
    /// Stable variable/output/state ownership identity.
    pub target_id: StableId,
    /// Byte offset within the target.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub offset_bytes: u64,
    /// Non-zero byte width.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub size_bytes: u64,
}

/// Per-expanded-node resources and optional R2-05 typed Action binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedNodeResourceInput {
    /// Owning task.
    pub task_handle: u32,
    /// Root `WorkflowId` followed by each Subworkflow call-site `NodeId`.
    pub instance_path: Vec<StableId>,
    /// Expanded Action or Subworkflow call node.
    pub node_id: StableId,
    /// Additional committed bytes owned by this expanded node.
    pub committed_state_bytes: u64,
    /// Additional staging bytes owned by this expanded node.
    pub staging_state_bytes: u64,
    /// Extra Trace events contributed by the later binding implementation.
    pub trace_events_per_release: u64,
    /// Complete static write footprint; overlapping writers are rejected across all tasks.
    pub writes: Vec<WorkflowWriteRegion>,
    /// Exact typed Action binding; required only by the R2-05 bound compiler for Action nodes.
    pub action_binding: Option<ExpandedActionBindingInput>,
    /// Exact subworkflow state-copy contract; required by the Plan 1.3 traced compiler.
    pub subworkflow_binding: Option<ExpandedSubworkflowBindingInput>,
}

/// One byte copied at a signed subworkflow activation or completion boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WorkflowStateCopyInput {
    /// Source byte offset within the task application-state image.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub source_offset_bytes: u64,
    /// Target byte offset within the task application-state image.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub target_offset_bytes: u64,
}

/// Complete ordered state-copy contract for one expanded subworkflow call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedSubworkflowBindingInput {
    /// Copies applied exactly once when the child instance is activated.
    pub input_copies: Vec<WorkflowStateCopyInput>,
    /// Copies applied exactly once when the child instance completes successfully.
    pub output_copies: Vec<WorkflowStateCopyInput>,
}

/// Canonical node category with all execution-affecting attributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CanonicalWorkflowNodeKind {
    /// Structural Entry marker.
    Entry,
    /// Binding supplied in R2-05.
    Action,
    /// Priority Decision.
    Decision,
    /// Ordered logical Fork.
    Fork,
    /// Explicit Join.
    Join {
        /// Frozen merge behavior.
        mode: CanonicalJoinMode,
        /// Paired Fork node handle when required.
        #[serde(skip_serializing_if = "Option::is_none")]
        fork_node: Option<WorkflowNodeHandle>,
        /// `JoinAny` loser behavior.
        #[serde(skip_serializing_if = "Option::is_none")]
        loser_policy: Option<CanonicalJoinPolicy>,
    },
    /// Release-counted Wait.
    WaitCycles {
        /// Exact configured release count.
        #[serde(serialize_with = "serialize_u64_decimal")]
        wait_cycles: u64,
    },
    /// Condition Wait.
    WaitCondition {
        /// Stable BOOL condition identity.
        condition_id: StableId,
        /// Finite timeout or absent for permanent Wait.
        #[serde(
            skip_serializing_if = "Option::is_none",
            serialize_with = "serialize_optional_u64_decimal"
        )]
        timeout_cycles: Option<u64>,
        /// Explicit permanent policy.
        permanent: bool,
    },
    /// Compile-time expanded call site.
    Subworkflow {
        /// Referenced template handle.
        target_workflow: WorkflowHandle,
    },
    /// Structural End marker.
    End,
}

/// Canonical Join behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalJoinMode {
    /// Exclusive Merge.
    Merge,
    /// Join all Fork branches.
    JoinAll,
    /// Join the first Fork branch.
    JoinAny,
}

/// Canonical `JoinAny` loser behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalJoinPolicy {
    /// Cancel at commit.
    CancelOthers,
    /// Keep losers running.
    KeepRunning,
    /// Cancel at a declared boundary.
    WaitAtBoundary,
}

/// One canonical template node without source/layout state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalWorkflowNode {
    /// Dense template node handle.
    pub handle: WorkflowNodeHandle,
    /// Stable author identity.
    pub node_id: StableId,
    /// Canonical machine name.
    pub canonical_name: String,
    /// Dense execution order; absent only for Entry/End.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_order: Option<u32>,
    /// Explicit cancellation boundary marker.
    pub cancellation_boundary: bool,
    /// Execution-affecting kind data.
    pub node_kind: CanonicalWorkflowNodeKind,
}

/// One canonical template edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalWorkflowEdge {
    /// Dense template edge handle.
    pub handle: WorkflowEdgeHandle,
    /// Stable author identity.
    pub edge_id: StableId,
    /// Source template node.
    pub source_node: WorkflowNodeHandle,
    /// Target template node.
    pub target_node: WorkflowNodeHandle,
    /// Optional BOOL guard identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition_id: Option<StableId>,
    /// Decision order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<u32>,
    /// Fork branch order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_order: Option<u32>,
    /// Declared traversal bound; absent for a forward edge.
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_u64_decimal"
    )]
    pub max_traversals_per_run: Option<u64>,
}

/// One canonical Workflow template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalWorkflowTemplate {
    /// Dense template handle.
    pub handle: WorkflowHandle,
    /// Stable Workflow identity.
    pub workflow_id: StableId,
    /// Canonical machine name.
    pub canonical_name: String,
    /// Explicit permanent lifecycle policy.
    pub permanent: bool,
    /// Nodes sorted by dense handle.
    pub nodes: Vec<CanonicalWorkflowNode>,
    /// Edges sorted by dense handle.
    pub edges: Vec<CanonicalWorkflowEdge>,
}

/// Canonical Workflow IR for exactly the templates reachable from explicit task roots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalWorkflowIr {
    /// Exact compiler-internal writer version.
    pub schema_version: WorkflowArtifactVersion,
    /// Reachable templates sorted by stable `WorkflowId`.
    pub workflows: Vec<CanonicalWorkflowTemplate>,
}

/// One expanded and state-isolated call instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedWorkflowInstance {
    /// Dense expanded handle.
    pub handle: WorkflowInstanceHandle,
    /// Owning R0 task.
    pub task_handle: u32,
    /// Referenced canonical template.
    pub workflow: WorkflowHandle,
    /// Root `WorkflowId` followed by Subworkflow call-site `NodeId` values.
    pub instance_path: Vec<StableId>,
}

/// One executable node in the single-thread static order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowPlanStep {
    /// Dense global step handle.
    pub handle: WorkflowStepHandle,
    /// Owning task.
    pub task_handle: u32,
    /// Expanded instance.
    pub instance: WorkflowInstanceHandle,
    /// Canonical template node.
    pub node: WorkflowNodeHandle,
    /// Dense order within the owning task.
    pub task_execution_order: u32,
    /// Child instance created by a Subworkflow node, otherwise absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_instance: Option<WorkflowInstanceHandle>,
}

/// Trace-visible structural kind retained only by Static Workflow Plan 1.3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlannedTraceNodeKind {
    /// Cyclic Action callback.
    Action,
    /// Priority Decision callback.
    Decision,
    /// Logical parallel split and its dense branch orders.
    Fork {
        /// Exact valid branch orders.
        branch_orders: Vec<u32>,
    },
    /// Non-parallel merge.
    Merge,
    /// Join waiting for every paired branch.
    JoinAll {
        /// Exact valid branch orders.
        branch_orders: Vec<u32>,
    },
    /// Join selecting the first paired branch.
    JoinAny {
        /// Frozen loser policy.
        loser_policy: CanonicalJoinPolicy,
        /// Exact valid branch orders.
        branch_orders: Vec<u32>,
    },
    /// Release-counted Wait.
    WaitCycles,
    /// Condition Wait, distinguishing finite timeout from permanent waiting.
    WaitCondition {
        /// Whether `TimedOut` is a valid observation.
        has_timeout: bool,
    },
    /// Compile-time-expanded call site.
    Subworkflow {
        /// Dense task-local call handle used by Trace `SourceHandle`.
        call_handle: u32,
        /// Exact child instance activated and completed by this call.
        child_instance: WorkflowInstanceHandle,
        /// Ordered signed input-copy table applied on activation.
        input_copies: Vec<WorkflowStateCopyInput>,
        /// Ordered signed output-copy table applied on successful completion.
        output_copies: Vec<WorkflowStateCopyInput>,
    },
}

/// One executable step's complete structural Trace audit descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedTraceNode {
    /// Exact owning static step.
    pub step: WorkflowStepHandle,
    /// Event-producing structural kind.
    pub node_kind: PlannedTraceNodeKind,
    /// Whether `CancelApplied(AtDeclaredBoundary)` may originate here.
    pub cancellation_boundary: bool,
    /// Branch orders whose pending cancellation may terminate here.
    pub cancellation_branch_orders: Vec<u32>,
    /// Exact scoped branch memberships used to remove canceled future active nodes.
    pub branch_memberships: Vec<PlannedTraceBranchMembership>,
}

/// One node's membership in a branch paired with a specific Join.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PlannedTraceBranchMembership {
    /// Expanded Join step that owns the branch order.
    pub join_step: WorkflowStepHandle,
    /// Branch order scoped to `join_step`.
    pub branch_order: u32,
}

/// Trace-visible target of one task-local Runtime edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlannedTraceEdgeTarget {
    /// Activates one executable step on the next scan.
    Step {
        /// Exact target step.
        step: WorkflowStepHandle,
    },
    /// Requests completion through a structural End marker.
    Complete,
}

/// One Runtime-ordered edge descriptor retained for Trace replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PlannedTraceEdge {
    /// Owning R0 task.
    pub task_handle: u32,
    /// Dense task-local Runtime edge handle used by Trace records.
    pub runtime_edge_handle: u32,
    /// Corresponding globally expanded plan edge.
    pub expanded_edge: ExpandedEdgeHandle,
    /// Exact emitting step.
    pub source_step: WorkflowStepHandle,
    /// Runtime target.
    pub target: PlannedTraceEdgeTarget,
    /// Fork branch order; absent for non-Fork edges.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_order: Option<u32>,
}

/// Complete Plan 1.3 structure required to prove Workflow Trace event provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedTraceStructure {
    /// Entry-selected executable steps for every expanded instance, in step-handle order.
    pub initial_active: Vec<WorkflowStepHandle>,
    /// All and only top-level Workflow instances.
    pub root_instances: Vec<WorkflowInstanceHandle>,
    /// Exactly one descriptor per executable step, in step-handle order.
    pub nodes: Vec<PlannedTraceNode>,
    /// Every executable edge in task-local Runtime handle order.
    pub edges: Vec<PlannedTraceEdge>,
}

/// One edge copied exactly once for one expanded instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PlannedWorkflowEdge {
    /// Dense expanded edge handle.
    pub handle: ExpandedEdgeHandle,
    /// Owning expanded instance.
    pub instance: WorkflowInstanceHandle,
    /// Canonical template edge.
    pub edge: WorkflowEdgeHandle,
    /// Expanded executable source step; absent only when the canonical source is not executable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_step: Option<WorkflowStepHandle>,
}

/// Exact per-node resource and ownership declaration retained in the static plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedNodeResources {
    /// Expanded Action/Subworkflow step owning these resources.
    pub step: WorkflowStepHandle,
    /// Additional committed state bytes.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub committed_state_bytes: u64,
    /// Additional staging state bytes.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub staging_state_bytes: u64,
    /// Additional binding-level Trace events per release.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub trace_events_per_release: u64,
    /// Complete normalized-order write footprint for this one static writer.
    pub writes: Vec<WorkflowWriteRegion>,
    /// Canonical typed Action binding, absent only for Subworkflow call resources.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action_binding: Option<ExpandedActionBindingInput>,
}

/// One fixed watch descriptor retained in the static plan and its digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedWorkflowWatch {
    /// Dense plan-local watch handle.
    pub handle: WorkflowWatchHandle,
    /// Owning task.
    pub task_handle: u32,
    /// Stable observed value identity.
    pub value_id: StableId,
    /// Canonical encoded value width in bytes.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub encoded_bytes: u64,
    /// Exact number of 32-byte Trace fragments.
    pub fragment_count: u16,
}

/// Exact actual-versus-limit proof for one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskWorkflowResourceProof {
    /// Owning task.
    pub task_handle: u32,
    /// Explicit root assignments.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub root_workflows: u64,
    /// Distinct templates reachable from those roots.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub reachable_workflow_templates: u64,
    /// Expanded root and call-site instances.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub expanded_workflow_instances: u64,
    /// Expanded nodes, including Entry/End.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub expanded_nodes: u64,
    /// Expanded edges.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub expanded_edges: u64,
    /// Conservative active-set capacity.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub active_nodes: u64,
    /// Conservative executable-node work per release.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub node_executions_per_release: u64,
    /// Conservative Fork nesting upper bound.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub fork_nesting_depth: u64,
    /// Largest branch count of a Fork.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub branches_per_fork: u64,
    /// Required pending-cancellation slots.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub pending_cancellations: u64,
    /// Deepest expanded instance path in Workflow units.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub subworkflow_expansion_depth: u64,
    /// Largest declared backedge traversal bound.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub backedge_traversals_per_run: u64,
    /// Largest Wait count/timeout.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub wait_cycles: u64,
    /// Fixed committed bank bytes.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub workflow_state_bytes: u64,
    /// Fixed staging bank bytes.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub workflow_staging_bytes: u64,
    /// Fixed watch descriptor count.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub watch_handles: u64,
    /// Worst-case staged Trace records per release.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub trace_events_per_release: u64,
    /// Actual preallocated Trace ring slots.
    #[serde(serialize_with = "serialize_u64_decimal")]
    pub trace_ring_capacity: u64,
}

/// Complete target-bound resource proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowResourceProof {
    /// Exact Target Profile values included in `plan_digest`.
    pub target_limits: WorkflowTargetLimits,
    /// Proofs sorted by task handle.
    pub tasks: Vec<TaskWorkflowResourceProof>,
}

/// Complete immutable static plan. Layout and author formatting are intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaticWorkflowPlan {
    /// Exact writer version.
    pub schema_version: WorkflowArtifactVersion,
    /// Digest of the paired Canonical Workflow IR bytes.
    pub semantic_digest: String,
    /// Expanded instances in dense handle order.
    pub instances: Vec<PlannedWorkflowInstance>,
    /// Single-thread steps in task/order sequence.
    pub steps: Vec<WorkflowPlanStep>,
    /// Expanded edges in dense handle order.
    pub edges: Vec<PlannedWorkflowEdge>,
    /// Exact resource declarations sorted by owning step.
    pub node_resources: Vec<PlannedNodeResources>,
    /// Exact task-owned BOOL condition catalog in dense handle order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub condition_bindings: Vec<PlannedConditionBinding>,
    /// Fixed watch descriptors sorted by task and stable value identity.
    pub watches: Vec<PlannedWorkflowWatch>,
    /// Exact dense Output/Watch value catalog; present only in Static Plan 1.3.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trace_values: Vec<PlannedTraceValue>,
    /// Exact structural provenance catalog; present only in Static Plan 1.3.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_structure: Option<PlannedTraceStructure>,
    /// Fixed resource proof consumed by runtime allocation.
    pub resources: WorkflowResourceProof,
}

/// Original source digest retained for audit but excluded from semantic/plan digests.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowSourceDigest {
    /// Normalized project-relative source path.
    pub source_path: String,
    /// SHA-256 of the exact source bytes.
    pub source_digest: String,
}

/// Atomic R2-02 artifact bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPlanArtifacts {
    /// Canonical reachable semantic templates.
    pub canonical_ir: CanonicalWorkflowIr,
    /// Static expanded execution plan.
    pub static_plan: StaticWorkflowPlan,
    /// Bounded RFC 8785 bytes used for `semantic_digest`.
    pub canonical_ir_json: Vec<u8>,
    /// Bounded RFC 8785 bytes used for `plan_digest`.
    pub static_plan_json: Vec<u8>,
    /// `sha256:<lowercase-hex>` of `canonical_ir_json`.
    pub semantic_digest: String,
    /// `sha256:<lowercase-hex>` of `static_plan_json`.
    pub plan_digest: String,
    /// Exact source-byte digests for reachable templates, sorted by path.
    pub source_digests: Vec<WorkflowSourceDigest>,
}

/// Atomic planning output. Any semantic/resource diagnostic suppresses every artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPlanOutput {
    /// Complete artifacts only when every proof and exact-generation audit passes.
    pub artifacts: Option<WorkflowPlanArtifacts>,
    /// Deterministic locale-neutral diagnostics.
    pub diagnostics: Vec<WorkflowDiagnostic>,
}

/// Corrupt or ambiguous caller-owned planning input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorkflowPlanInputError {
    /// R2-01 accepted model cannot be reproduced.
    #[error("Workflow validation did not publish an accepted model")]
    InvalidValidatedInput,
    /// No R0 task was supplied as a planning root owner.
    #[error("Workflow planning requires at least one task")]
    EmptyTaskSet,
    /// A task handle is duplicated or uses the reserved sentinel.
    #[error("task handle {0} is invalid or duplicated")]
    InvalidTask(u32),
    /// A task root is missing, repeated, or references no accepted template.
    #[error("task {task} has an invalid root Workflow {workflow}")]
    InvalidTaskRoot {
        /// Task handle.
        task: u32,
        /// Root Workflow identity.
        workflow: StableId,
    },
    /// A task has no explicit root Workflow.
    #[error("task {0} has no explicit root Workflow")]
    MissingTaskRoot(u32),
    /// A watch ID is duplicated or its encoded width is zero.
    #[error("task {task} has invalid watch {watch}")]
    InvalidWatch {
        /// Task handle.
        task: u32,
        /// Watch identity.
        watch: StableId,
    },
    /// Resource claim is missing, repeated, extra, or structurally invalid.
    #[error("expanded resource claim is not represented exactly once")]
    InvalidResourceClaim,
    /// Action binding is missing, repeated, extra, version-incompatible, or inconsistent.
    #[error("expanded Action binding is not represented exactly once")]
    InvalidActionBinding,
    /// Condition binding is missing, repeated, extra, non-BOOL, or inconsistent.
    #[error("Workflow condition binding is not represented exactly once")]
    InvalidConditionBinding,
    /// Bound task image catalog is missing, repeated, extra, zero-sized, or inconsistent.
    #[error("bound task image catalog is not represented exactly once")]
    InvalidBindingImage,
    /// Trace value/watch catalog is missing, repeated, extra, merged, or inconsistent.
    #[error("Workflow Trace value catalog is not represented exactly once")]
    InvalidTraceBinding,
    /// A dense handle or checked resource calculation cannot be represented.
    #[error("Workflow planning arithmetic is not representable")]
    ArithmeticOverflow,
    /// Canonical serialization failed for an otherwise accepted value.
    #[error("failed to serialize Workflow artifact: {0}")]
    Serialization(String),
    /// Generated tables failed the exact no-extra/no-missing audit.
    #[error("Workflow artifact generation audit failed")]
    GenerationAudit,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct InstanceKey {
    task_handle: u32,
    path: Vec<StableId>,
}

#[derive(Debug, Clone)]
struct InstanceDraft {
    key: InstanceKey,
    workflow_id: StableId,
    step_nodes: Vec<StableId>,
    child_by_node: BTreeMap<StableId, InstanceKey>,
}

#[derive(Debug, Default)]
struct ExpansionBudget {
    instances: u64,
    nodes: u64,
    edges: u64,
}

/// Validates, lowers, expands, proves, and serializes one Workflow build input.
///
/// Graph YAML is revalidated in this call. Task roots are explicit; unreferenced templates never
/// become implicit roots. Resource inputs must match every expanded Action/Subworkflow node
/// exactly once, preventing both omitted and duplicate generated state. Layout input is not
/// accepted here and therefore cannot affect any runtime artifact or digest.
///
/// # Errors
///
/// Returns [`WorkflowPlanInputError`] for caller-owned task/catalog ambiguity, arithmetic that
/// cannot be represented, serialization failure, or an internal exact-generation mismatch.
#[allow(clippy::too_many_lines)]
pub fn compile_static_workflow_plan(
    workflow_sources: &[WorkflowSource<'_>],
    validation_limits: WorkflowValidationLimits,
    task_inputs: &[TaskWorkflowPlanningInput],
    resource_inputs: &[ExpandedNodeResourceInput],
    target_limits: WorkflowTargetLimits,
    artifact_limits: WorkflowArtifactLimits,
) -> Result<WorkflowPlanOutput, WorkflowPlanInputError> {
    compile_workflow_plan(
        workflow_sources,
        validation_limits,
        task_inputs,
        resource_inputs,
        &[],
        &[],
        target_limits,
        artifact_limits,
        false,
        None,
    )
}

/// Compiles Static Workflow Plan 1.1 with an exact typed Action and BOOL condition closure.
///
/// Every expanded Action must carry one binding in its resource input, every Subworkflow resource
/// must carry none, and every expanded-instance condition identity must have exactly one BOOL
/// source. Binding-derived state, Trace, and write resources must exactly equal the independently
/// supplied resource claim; mismatches publish no partial artifact.
///
/// # Errors
///
/// Returns [`WorkflowPlanInputError`] for a missing, duplicate, extra, unsupported, or inconsistent
/// binding, in addition to the base planning errors.
#[allow(clippy::too_many_arguments)]
pub fn compile_bound_workflow_plan(
    workflow_sources: &[WorkflowSource<'_>],
    validation_limits: WorkflowValidationLimits,
    task_inputs: &[TaskWorkflowPlanningInput],
    resource_inputs: &[ExpandedNodeResourceInput],
    condition_inputs: &[WorkflowConditionBindingInput],
    image_inputs: &[TaskBindingImageInput],
    target_limits: WorkflowTargetLimits,
    artifact_limits: WorkflowArtifactLimits,
) -> Result<WorkflowPlanOutput, WorkflowPlanInputError> {
    compile_workflow_plan(
        workflow_sources,
        validation_limits,
        task_inputs,
        resource_inputs,
        condition_inputs,
        image_inputs,
        target_limits,
        artifact_limits,
        true,
        None,
    )
}

/// Compiles Static Workflow Plan 1.3 with exact typed bindings and Trace audit catalogs.
///
/// # Errors
/// Returns [`WorkflowPlanInputError`] when the R2-05 closure or any Output/Watch descriptor is
/// missing, duplicated, extra, merged across call sites, out of bounds, or non-representable.
#[allow(clippy::too_many_arguments)]
pub fn compile_traced_workflow_plan(
    workflow_sources: &[WorkflowSource<'_>],
    validation_limits: WorkflowValidationLimits,
    task_inputs: &[TaskWorkflowPlanningInput],
    resource_inputs: &[ExpandedNodeResourceInput],
    condition_inputs: &[WorkflowConditionBindingInput],
    image_inputs: &[TaskBindingImageInput],
    watch_binding_inputs: &[WorkflowWatchBindingInput],
    target_limits: WorkflowTargetLimits,
    artifact_limits: WorkflowArtifactLimits,
) -> Result<WorkflowPlanOutput, WorkflowPlanInputError> {
    compile_workflow_plan(
        workflow_sources,
        validation_limits,
        task_inputs,
        resource_inputs,
        condition_inputs,
        image_inputs,
        target_limits,
        artifact_limits,
        true,
        Some(watch_binding_inputs),
    )
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn compile_workflow_plan(
    workflow_sources: &[WorkflowSource<'_>],
    validation_limits: WorkflowValidationLimits,
    task_inputs: &[TaskWorkflowPlanningInput],
    resource_inputs: &[ExpandedNodeResourceInput],
    condition_inputs: &[WorkflowConditionBindingInput],
    image_inputs: &[TaskBindingImageInput],
    target_limits: WorkflowTargetLimits,
    artifact_limits: WorkflowArtifactLimits,
    require_bindings: bool,
    trace_inputs: Option<&[WorkflowWatchBindingInput]>,
) -> Result<WorkflowPlanOutput, WorkflowPlanInputError> {
    let validation = validate_project(
        WorkflowProjectInput {
            workflows: workflow_sources,
            layouts: &[],
        },
        validation_limits,
    );
    let Some(workflows) = validation.workflows else {
        return Ok(WorkflowPlanOutput {
            artifacts: None,
            diagnostics: validation.diagnostics,
        });
    };
    if !validation.diagnostics.is_empty() {
        return Err(WorkflowPlanInputError::InvalidValidatedInput);
    }
    let sources = source_map(workflow_sources)?;
    let workflow_map = workflows
        .iter()
        .map(|workflow| (workflow.workflow_id, workflow))
        .collect::<BTreeMap<_, _>>();
    let tasks = prepare_tasks(task_inputs, &workflow_map)?;
    let binding_images = prepare_binding_images(&tasks, image_inputs, require_bindings)?;
    let mut diagnostics =
        validate_planning_graphs(&workflows, &workflow_map, &sources, target_limits)?;
    if !diagnostics.is_empty() {
        sort_diagnostics(&mut diagnostics);
        return Ok(WorkflowPlanOutput {
            artifacts: None,
            diagnostics,
        });
    }

    let mut drafts = Vec::new();
    let expansion_limits = target_limits.values();
    for task in &tasks {
        if usize_u64(task.root_workflow_ids.len())? > expansion_limits.max_workflows_per_task {
            diagnostics.push(resource_diagnostic_for_task(task, &workflow_map, &sources)?);
            continue;
        }
        let mut budget = ExpansionBudget::default();
        for root in &task.root_workflow_ids {
            if !expand_instance(
                task.task_handle,
                *root,
                &[*root],
                &workflow_map,
                expansion_limits,
                &mut budget,
                &mut drafts,
            )? {
                diagnostics.push(resource_diagnostic_for_task(task, &workflow_map, &sources)?);
                break;
            }
        }
    }
    if !diagnostics.is_empty() {
        sort_diagnostics(&mut diagnostics);
        return Ok(WorkflowPlanOutput {
            artifacts: None,
            diagnostics,
        });
    }
    drafts.sort_by(|left, right| left.key.cmp(&right.key));
    let instance_handles = drafts
        .iter()
        .enumerate()
        .map(|(index, draft)| Ok((draft.key.clone(), WorkflowInstanceHandle(dense(index)?))))
        .collect::<Result<BTreeMap<_, _>, WorkflowPlanInputError>>()?;

    let reachable = drafts
        .iter()
        .map(|draft| draft.workflow_id)
        .collect::<BTreeSet<_>>();
    let (canonical_ir, workflow_handles, node_handles, edge_handles) =
        build_canonical_ir(&reachable, &workflow_map)?;
    let Some(canonical_ir_json) =
        serialize_bounded(&canonical_ir, artifact_limits.canonical_ir_bytes)?
    else {
        let diagnostic = resource_diagnostic_for_first_task(&tasks, &workflow_map, &sources)?;
        return Ok(WorkflowPlanOutput {
            artifacts: None,
            diagnostics: vec![diagnostic],
        });
    };
    let semantic_digest = sha256_prefixed(&canonical_ir_json);

    let (instances, steps, expanded_edges, step_by_key) = build_plan_tables(
        &drafts,
        &instance_handles,
        &workflow_handles,
        &node_handles,
        &edge_handles,
        &workflow_map,
    )?;
    let claims = prepare_claims(
        resource_inputs,
        &drafts,
        &workflow_map,
        target_limits,
        require_bindings,
        trace_inputs.is_some(),
        &binding_images,
    )?;
    if trace_inputs.is_some()
        && claims
            .values()
            .any(|claim| claim.trace_events_per_release != 0)
    {
        return Err(WorkflowPlanInputError::InvalidTraceBinding);
    }
    diagnostics = validate_write_conflicts(&claims, &step_by_key, &workflow_map, &sources)?;
    let resources = prove_resources(
        &tasks,
        &drafts,
        &claims,
        &workflow_map,
        &sources,
        target_limits,
        &mut diagnostics,
    )?;
    sort_diagnostics(&mut diagnostics);
    diagnostics.dedup_by(|left, right| {
        left.source_path == right.source_path
            && left.span == right.span
            && left.code == right.code
            && left.related_id == right.related_id
    });
    if !diagnostics.is_empty() {
        return Ok(WorkflowPlanOutput {
            artifacts: None,
            diagnostics,
        });
    }

    let node_resources = build_planned_node_resources(&claims, &step_by_key);
    let watches = build_planned_watches(&tasks)?;
    let condition_bindings = build_condition_bindings(
        condition_inputs,
        &tasks,
        &drafts,
        &instance_handles,
        &workflow_map,
        target_limits,
        require_bindings,
        &binding_images,
    )?;
    validate_resolved_slot_mapping(&claims, condition_inputs, &binding_images)?;
    let trace_values = if let Some(watch_bindings) = trace_inputs {
        build_trace_values(
            &node_resources,
            &watches,
            watch_bindings,
            &steps,
            &instances,
            &binding_images,
        )?
    } else {
        Vec::new()
    };
    let trace_structure = if trace_inputs.is_some() {
        Some(build_trace_structure(
            &drafts,
            &instance_handles,
            &steps,
            &expanded_edges,
            &step_by_key,
            &edge_handles,
            &workflow_map,
            &claims,
        )?)
    } else {
        None
    };
    audit_generation(
        &drafts,
        &instances,
        &steps,
        &expanded_edges,
        &instance_handles,
        &workflow_handles,
        &node_handles,
        &edge_handles,
        &workflow_map,
    )?;
    audit_planning_inputs(
        &node_resources,
        &watches,
        &condition_bindings,
        &claims,
        &step_by_key,
        &tasks,
        condition_inputs,
        &instance_handles,
        require_bindings,
    )?;
    let static_plan = StaticWorkflowPlan {
        schema_version: if trace_inputs.is_some() {
            WorkflowArtifactVersion::traced_static_plan()
        } else {
            WorkflowArtifactVersion::static_plan()
        },
        semantic_digest: semantic_digest.clone(),
        instances,
        steps,
        edges: expanded_edges,
        node_resources,
        condition_bindings,
        watches,
        trace_values,
        trace_structure,
        resources,
    };
    let Some(static_plan_json) =
        serialize_bounded(&static_plan, artifact_limits.static_plan_bytes)?
    else {
        let diagnostic = resource_diagnostic_for_first_task(&tasks, &workflow_map, &sources)?;
        return Ok(WorkflowPlanOutput {
            artifacts: None,
            diagnostics: vec![diagnostic],
        });
    };
    let plan_digest = sha256_prefixed(&static_plan_json);
    let source_digests = source_digests(&reachable, &workflows, workflow_sources)?;
    Ok(WorkflowPlanOutput {
        artifacts: Some(WorkflowPlanArtifacts {
            canonical_ir,
            static_plan,
            canonical_ir_json,
            static_plan_json,
            semantic_digest,
            plan_digest,
            source_digests,
        }),
        diagnostics: Vec::new(),
    })
}

fn source_map<'a>(
    sources: &'a [WorkflowSource<'a>],
) -> Result<BTreeMap<&'a str, &'a str>, WorkflowPlanInputError> {
    let mut result = BTreeMap::new();
    for source in sources {
        let text = str::from_utf8(source.source_bytes)
            .map_err(|_| WorkflowPlanInputError::InvalidValidatedInput)?;
        if result.insert(source.source_path, text).is_some() {
            return Err(WorkflowPlanInputError::InvalidValidatedInput);
        }
    }
    Ok(result)
}

fn prepare_tasks<'a>(
    task_inputs: &'a [TaskWorkflowPlanningInput],
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
) -> Result<Vec<&'a TaskWorkflowPlanningInput>, WorkflowPlanInputError> {
    if task_inputs.is_empty() {
        return Err(WorkflowPlanInputError::EmptyTaskSet);
    }
    let mut tasks = task_inputs.iter().collect::<Vec<_>>();
    tasks.sort_by_key(|task| task.task_handle);
    let mut task_handles = BTreeSet::new();
    for task in &tasks {
        if task.task_handle == u32::MAX || !task_handles.insert(task.task_handle) {
            return Err(WorkflowPlanInputError::InvalidTask(task.task_handle));
        }
        if task.root_workflow_ids.is_empty() {
            return Err(WorkflowPlanInputError::MissingTaskRoot(task.task_handle));
        }
        let mut roots = BTreeSet::new();
        for root in &task.root_workflow_ids {
            if !roots.insert(*root) || !workflows.contains_key(root) {
                return Err(WorkflowPlanInputError::InvalidTaskRoot {
                    task: task.task_handle,
                    workflow: *root,
                });
            }
        }
        let mut watches = BTreeSet::new();
        for watch in &task.watches {
            if watch.encoded_bytes == 0 || !watches.insert(watch.value_id) {
                return Err(WorkflowPlanInputError::InvalidWatch {
                    task: task.task_handle,
                    watch: watch.value_id,
                });
            }
        }
    }
    Ok(tasks)
}

fn prepare_binding_images(
    tasks: &[&TaskWorkflowPlanningInput],
    inputs: &[TaskBindingImageInput],
    required: bool,
) -> Result<BTreeMap<u32, TaskBindingImageInput>, WorkflowPlanInputError> {
    if !required {
        return inputs
            .is_empty()
            .then(BTreeMap::new)
            .ok_or(WorkflowPlanInputError::InvalidBindingImage);
    }
    if inputs.len() != tasks.len() {
        return Err(WorkflowPlanInputError::InvalidBindingImage);
    }
    let expected = tasks
        .iter()
        .map(|task| task.task_handle)
        .collect::<BTreeSet<_>>();
    let mut images = BTreeMap::new();
    for input in inputs {
        if input.task_handle == u32::MAX
            || input.application_state_bytes == 0
            || input.output_bytes == 0
            || images.insert(input.task_handle, *input).is_some()
        {
            return Err(WorkflowPlanInputError::InvalidBindingImage);
        }
    }
    if images.keys().copied().collect::<BTreeSet<_>>() != expected {
        return Err(WorkflowPlanInputError::InvalidBindingImage);
    }
    Ok(images)
}

fn validate_planning_graphs(
    workflows: &[WorkflowDocument],
    workflow_map: &BTreeMap<StableId, &WorkflowDocument>,
    sources: &BTreeMap<&str, &str>,
    limits: WorkflowTargetLimits,
) -> Result<Vec<WorkflowDiagnostic>, WorkflowPlanInputError> {
    let mut diagnostics = Vec::new();
    validate_recursion(workflows, workflow_map, sources, &mut diagnostics)?;
    for workflow in workflows {
        validate_topology(workflow, sources, limits, &mut diagnostics)?;
    }
    Ok(diagnostics)
}

fn validate_recursion(
    workflows: &[WorkflowDocument],
    workflow_map: &BTreeMap<StableId, &WorkflowDocument>,
    sources: &BTreeMap<&str, &str>,
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) -> Result<(), WorkflowPlanInputError> {
    fn visit(
        id: StableId,
        workflow_map: &BTreeMap<StableId, &WorkflowDocument>,
        sources: &BTreeMap<&str, &str>,
        states: &mut BTreeMap<StableId, u8>,
        diagnostics: &mut Vec<WorkflowDiagnostic>,
    ) -> Result<(), WorkflowPlanInputError> {
        states.insert(id, 1);
        let workflow = workflow_map
            .get(&id)
            .copied()
            .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?;
        let mut calls = workflow
            .nodes
            .iter()
            .filter_map(|node| match node.kind {
                NodeKind::Subworkflow { target_workflow_id } => Some((target_workflow_id, node)),
                _ => None,
            })
            .collect::<Vec<_>>();
        calls.sort_by_key(|(target, node)| (target.network_bytes(), node.node_id.network_bytes()));
        for (target, node) in calls {
            match states.get(&target).copied().unwrap_or(0) {
                0 => visit(target, workflow_map, sources, states, diagnostics)?,
                1 => diagnostics.push(document_diagnostic(
                    workflow,
                    sources,
                    WorkflowDiagnosticCode::RecursiveSubworkflow,
                    node.span,
                    Some(node.node_id),
                )?),
                _ => {}
            }
        }
        states.insert(id, 2);
        Ok(())
    }

    let mut states = BTreeMap::new();
    let mut ordered = workflows
        .iter()
        .map(|workflow| workflow.workflow_id)
        .collect::<Vec<_>>();
    ordered.sort_by_key(|id| id.network_bytes());
    for id in ordered {
        if states.get(&id).copied().unwrap_or(0) == 0 {
            visit(id, workflow_map, sources, &mut states, diagnostics)?;
        }
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "topology validation keeps all mutually dependent graph invariants in one deterministic pass"
)]
fn validate_topology(
    workflow: &WorkflowDocument,
    sources: &BTreeMap<&str, &str>,
    limits: WorkflowTargetLimits,
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) -> Result<(), WorkflowPlanInputError> {
    let nodes = workflow
        .nodes
        .iter()
        .map(|node| (node.node_id, node))
        .collect::<BTreeMap<_, _>>();
    let entry = workflow
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::Entry)
        .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?;
    let reachable = reachable_from(entry.node_id, &workflow.edges);
    for node in &workflow.nodes {
        if !reachable.contains(&node.node_id) {
            diagnostics.push(document_diagnostic(
                workflow,
                sources,
                WorkflowDiagnosticCode::UnreachableNode,
                node.span,
                Some(node.node_id),
            )?);
        }
    }
    if !workflow.permanent
        && !workflow
            .nodes
            .iter()
            .any(|node| node.kind == NodeKind::End && reachable.contains(&node.node_id))
    {
        diagnostics.push(document_diagnostic(
            workflow,
            sources,
            WorkflowDiagnosticCode::MissingCompletionPath,
            workflow.span,
            Some(workflow.workflow_id),
        )?);
    }

    let forward = workflow
        .edges
        .iter()
        .filter(|edge| edge.backedge.is_none())
        .collect::<Vec<_>>();
    let unmarked_cycle_edges = forward
        .iter()
        .filter_map(|edge| {
            let source = nodes.get(&edge.source_node_id).copied()?;
            let target = nodes.get(&edge.target_node_id).copied()?;
            let reverses_order = matches!(
                (source.execution_order, target.execution_order),
                (Some(source_order), Some(target_order)) if source_order >= target_order
            );
            (reverses_order
                && reachable_from_refs(target.node_id, &forward).contains(&source.node_id))
            .then_some(edge.edge_id)
        })
        .collect::<BTreeSet<_>>();
    for edge in &workflow.edges {
        let source = nodes
            .get(&edge.source_node_id)
            .copied()
            .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?;
        let target = nodes
            .get(&edge.target_node_id)
            .copied()
            .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?;
        if let Some(backedge) = edge.backedge {
            let target_reaches_source =
                reachable_from_refs(target.node_id, &forward).contains(&source.node_id);
            if !target_reaches_source {
                diagnostics.push(document_diagnostic(
                    workflow,
                    sources,
                    WorkflowDiagnosticCode::UnmarkedCycleEdge,
                    edge.span,
                    Some(edge.edge_id),
                )?);
            }
            if backedge.max_traversals_per_run > limits.values().max_backedge_traversals_per_run {
                diagnostics.push(document_diagnostic(
                    workflow,
                    sources,
                    WorkflowDiagnosticCode::ResourceBudgetExceeded,
                    edge.span,
                    Some(edge.edge_id),
                )?);
            }
        } else if unmarked_cycle_edges.contains(&edge.edge_id) {
            diagnostics.push(document_diagnostic(
                workflow,
                sources,
                WorkflowDiagnosticCode::UnmarkedCycleEdge,
                edge.span,
                Some(edge.edge_id),
            )?);
        } else if let (Some(source_order), Some(target_order)) =
            (source.execution_order, target.execution_order)
            && source_order >= target_order
        {
            diagnostics.push(document_diagnostic(
                workflow,
                sources,
                WorkflowDiagnosticCode::InvalidForwardDependency,
                edge.span,
                Some(edge.edge_id),
            )?);
        }
    }
    validate_structured_regions(workflow, sources, &nodes, &forward, diagnostics)?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "structured-region validation audits each paired branch boundary as one indivisible graph proof"
)]
fn validate_structured_regions(
    workflow: &WorkflowDocument,
    sources: &BTreeMap<&str, &str>,
    nodes: &BTreeMap<StableId, &Node>,
    forward: &[&Edge],
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) -> Result<(), WorkflowPlanInputError> {
    let forks = workflow
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Fork)
        .map(|node| node.node_id)
        .collect::<BTreeSet<_>>();
    let mut paired = BTreeMap::<StableId, StableId>::new();
    let mut pair_regions = Vec::<(StableId, BTreeSet<StableId>)>::new();
    let mut allowed_cancellation_boundaries = BTreeSet::new();
    for join in &workflow.nodes {
        let NodeKind::Join {
            mode,
            fork_id: Some(fork_id),
            loser_policy,
        } = join.kind
        else {
            continue;
        };
        if mode == JoinMode::Merge || !forks.contains(&fork_id) {
            continue;
        }
        let fork = nodes
            .get(&fork_id)
            .copied()
            .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?;
        let invalid_pair = paired.insert(fork_id, join.node_id).is_some()
            || !matches!(
                (fork.execution_order, join.execution_order),
                (Some(fork_order), Some(join_order)) if fork_order < join_order
            );
        if invalid_pair {
            diagnostics.push(document_diagnostic(
                workflow,
                sources,
                WorkflowDiagnosticCode::InvalidForkJoinPair,
                join.span,
                Some(join.node_id),
            )?);
            continue;
        }

        let branches = forward
            .iter()
            .filter(|edge| edge.source_node_id == fork_id)
            .map(|edge| edge.target_node_id)
            .collect::<Vec<_>>();
        let can_reach_join = reverse_reachable_from(join.node_id, forward);
        let mut branch_regions = Vec::with_capacity(branches.len());
        let mut invalid_region = false;
        for branch in &branches {
            let reachable = reachable_from_refs(*branch, forward);
            if !reachable.contains(&join.node_id) {
                invalid_region = true;
            }
            let region = reachable
                .intersection(&can_reach_join)
                .copied()
                .filter(|id| *id != fork_id && *id != join.node_id)
                .collect::<BTreeSet<_>>();
            if branch_regions
                .iter()
                .any(|other: &BTreeSet<StableId>| !other.is_disjoint(&region))
            {
                invalid_region = true;
            }
            branch_regions.push(region);
        }
        let region = branch_regions
            .iter()
            .flat_map(|branch| branch.iter().copied())
            .collect::<BTreeSet<_>>();
        let mut join_sources = BTreeSet::new();
        for edge in forward {
            let source_in = region.contains(&edge.source_node_id);
            let target_in = region.contains(&edge.target_node_id);
            if source_in && !target_in && edge.target_node_id != join.node_id {
                invalid_region = true;
            }
            if target_in && !source_in && edge.source_node_id != fork_id {
                invalid_region = true;
            }
            if edge.target_node_id == join.node_id {
                if source_in {
                    join_sources.insert(edge.source_node_id);
                } else {
                    invalid_region = true;
                }
            }
        }
        if branch_regions.iter().any(|branch| {
            join_sources
                .iter()
                .filter(|source| branch.contains(source))
                .count()
                != 1
        }) {
            invalid_region = true;
        }
        if invalid_region {
            diagnostics.push(document_diagnostic(
                workflow,
                sources,
                WorkflowDiagnosticCode::CrossRegionJoin,
                join.span,
                Some(join.node_id),
            )?);
        } else if mode == JoinMode::JoinAny && loser_policy == Some(JoinPolicy::WaitAtBoundary) {
            for branch_region in &branch_regions {
                allowed_cancellation_boundaries.extend(
                    branch_region
                        .iter()
                        .copied()
                        .filter(|node_id| nodes[node_id].cancellation_boundary),
                );
            }
            let bounded = branches
                .iter()
                .zip(&branch_regions)
                .all(|(branch, branch_region)| {
                    cancellation_path_is_bounded(
                        *branch,
                        join.node_id,
                        branch_region,
                        nodes,
                        forward,
                    )
                });
            if !bounded {
                diagnostics.push(document_diagnostic(
                    workflow,
                    sources,
                    WorkflowDiagnosticCode::UnboundedCancellationPath,
                    join.span,
                    Some(join.node_id),
                )?);
            }
        }
        pair_regions.push((join.node_id, region));
    }
    for fork_id in forks.difference(&paired.keys().copied().collect()) {
        let fork = nodes
            .get(fork_id)
            .copied()
            .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?;
        diagnostics.push(document_diagnostic(
            workflow,
            sources,
            WorkflowDiagnosticCode::InvalidForkJoinPair,
            fork.span,
            Some(fork.node_id),
        )?);
    }
    for index in 0..pair_regions.len() {
        let (_, left) = &pair_regions[index];
        for (join_id, right) in pair_regions.iter().skip(index + 1) {
            let overlaps = !left.is_disjoint(right);
            if overlaps && !left.is_subset(right) && !right.is_subset(left) {
                let join = nodes
                    .get(join_id)
                    .copied()
                    .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?;
                diagnostics.push(document_diagnostic(
                    workflow,
                    sources,
                    WorkflowDiagnosticCode::CrossRegionJoin,
                    join.span,
                    Some(join.node_id),
                )?);
            }
        }
    }
    for node in workflow.nodes.iter().filter(|node| {
        node.cancellation_boundary && !allowed_cancellation_boundaries.contains(&node.node_id)
    }) {
        diagnostics.push(document_diagnostic(
            workflow,
            sources,
            WorkflowDiagnosticCode::InvalidCancellationBoundary,
            node.span,
            Some(node.node_id),
        )?);
    }
    Ok(())
}

fn cancellation_path_is_bounded(
    branch: StableId,
    join: StableId,
    region: &BTreeSet<StableId>,
    nodes: &BTreeMap<StableId, &Node>,
    forward: &[&Edge],
) -> bool {
    let mut visited = BTreeSet::new();
    let mut pending = VecDeque::from([branch]);
    while let Some(current) = pending.pop_front() {
        if !visited.insert(current) {
            continue;
        }
        let Some(node) = nodes.get(&current).copied() else {
            return false;
        };
        if matches!(
            node.kind,
            NodeKind::Wait(WaitMode::Condition {
                permanent: true,
                ..
            })
        ) {
            return false;
        }
        if node.cancellation_boundary {
            continue;
        }
        let mut has_forward = false;
        for edge in forward.iter().filter(|edge| edge.source_node_id == current) {
            has_forward = true;
            if edge.target_node_id == join || !region.contains(&edge.target_node_id) {
                return false;
            }
            pending.push_back(edge.target_node_id);
        }
        if !has_forward {
            return false;
        }
    }
    true
}

fn reverse_reachable_from(start: StableId, edges: &[&Edge]) -> BTreeSet<StableId> {
    let mut result = BTreeSet::from([start]);
    let mut pending = VecDeque::from([start]);
    while let Some(current) = pending.pop_front() {
        for edge in edges.iter().filter(|edge| edge.target_node_id == current) {
            if result.insert(edge.source_node_id) {
                pending.push_back(edge.source_node_id);
            }
        }
    }
    result
}

fn reachable_from(start: StableId, edges: &[Edge]) -> BTreeSet<StableId> {
    let mut result = BTreeSet::from([start]);
    let mut pending = VecDeque::from([start]);
    while let Some(current) = pending.pop_front() {
        for edge in edges.iter().filter(|edge| edge.source_node_id == current) {
            if result.insert(edge.target_node_id) {
                pending.push_back(edge.target_node_id);
            }
        }
    }
    result
}

fn reachable_from_refs(start: StableId, edges: &[&Edge]) -> BTreeSet<StableId> {
    let mut result = BTreeSet::from([start]);
    let mut pending = VecDeque::from([start]);
    while let Some(current) = pending.pop_front() {
        for edge in edges.iter().filter(|edge| edge.source_node_id == current) {
            if result.insert(edge.target_node_id) {
                pending.push_back(edge.target_node_id);
            }
        }
    }
    result
}

fn expand_instance(
    task_handle: u32,
    workflow_id: StableId,
    path: &[StableId],
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
    limits: WorkflowTargetLimitValues,
    budget: &mut ExpansionBudget,
    drafts: &mut Vec<InstanceDraft>,
) -> Result<bool, WorkflowPlanInputError> {
    let workflow = workflows
        .get(&workflow_id)
        .copied()
        .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?;
    let depth = usize_u64(path.len())?;
    let node_count = usize_u64(workflow.nodes.len())?;
    let edge_count = usize_u64(workflow.edges.len())?;
    let instances = checked_add(budget.instances, 1)?;
    let nodes = checked_add(budget.nodes, node_count)?;
    let edges = checked_add(budget.edges, edge_count)?;
    if depth > limits.max_subworkflow_expansion_depth
        || node_count > limits.max_source_nodes_per_workflow
        || edge_count > limits.max_source_edges_per_workflow
        || instances > limits.max_expanded_workflow_instances
        || nodes > limits.max_expanded_nodes_per_task
        || edges > limits.max_expanded_edges_per_task
    {
        return Ok(false);
    }
    budget.instances = instances;
    budget.nodes = nodes;
    budget.edges = edges;
    let mut ordered = workflow
        .nodes
        .iter()
        .filter_map(|node| node.execution_order.map(|order| (order, node)))
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(order, node)| (*order, node.node_id.network_bytes()));
    let key = InstanceKey {
        task_handle,
        path: path.to_owned(),
    };
    let mut child_by_node = BTreeMap::new();
    for (_, node) in &ordered {
        if let NodeKind::Subworkflow { target_workflow_id } = node.kind {
            let mut child_path = path.to_owned();
            child_path.push(node.node_id);
            child_by_node.insert(
                node.node_id,
                InstanceKey {
                    task_handle,
                    path: child_path.clone(),
                },
            );
            if !expand_instance(
                task_handle,
                target_workflow_id,
                &child_path,
                workflows,
                limits,
                budget,
                drafts,
            )? {
                return Ok(false);
            }
        }
    }
    drafts.push(InstanceDraft {
        key,
        workflow_id,
        step_nodes: ordered.into_iter().map(|(_, node)| node.node_id).collect(),
        child_by_node,
    });
    Ok(true)
}

type CanonicalBuild = (
    CanonicalWorkflowIr,
    BTreeMap<StableId, WorkflowHandle>,
    BTreeMap<StableId, WorkflowNodeHandle>,
    BTreeMap<StableId, WorkflowEdgeHandle>,
);

fn build_canonical_ir(
    reachable: &BTreeSet<StableId>,
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
) -> Result<CanonicalBuild, WorkflowPlanInputError> {
    let workflow_handles = reachable
        .iter()
        .enumerate()
        .map(|(index, id)| Ok((*id, WorkflowHandle(dense(index)?))))
        .collect::<Result<BTreeMap<_, _>, WorkflowPlanInputError>>()?;
    let mut node_ids = reachable
        .iter()
        .flat_map(|id| workflows[id].nodes.iter().map(|node| node.node_id))
        .collect::<Vec<_>>();
    node_ids.sort_by_key(|id| id.network_bytes());
    let node_handles = node_ids
        .iter()
        .enumerate()
        .map(|(index, id)| Ok((*id, WorkflowNodeHandle(dense(index)?))))
        .collect::<Result<BTreeMap<_, _>, WorkflowPlanInputError>>()?;
    let mut edge_ids = reachable
        .iter()
        .flat_map(|id| workflows[id].edges.iter().map(|edge| edge.edge_id))
        .collect::<Vec<_>>();
    edge_ids.sort_by_key(|id| id.network_bytes());
    let edge_handles = edge_ids
        .iter()
        .enumerate()
        .map(|(index, id)| Ok((*id, WorkflowEdgeHandle(dense(index)?))))
        .collect::<Result<BTreeMap<_, _>, WorkflowPlanInputError>>()?;

    let mut canonical = Vec::with_capacity(reachable.len());
    for id in reachable {
        let workflow = workflows[id];
        let mut nodes = workflow.nodes.iter().collect::<Vec<_>>();
        nodes.sort_by_key(|node| node_handles[&node.node_id]);
        let nodes = nodes
            .into_iter()
            .map(|node| canonical_node(node, &workflow_handles, &node_handles))
            .collect::<Result<Vec<_>, _>>()?;
        let mut edges = workflow.edges.iter().collect::<Vec<_>>();
        edges.sort_by_key(|edge| edge_handles[&edge.edge_id]);
        let edges = edges
            .into_iter()
            .map(|edge| CanonicalWorkflowEdge {
                handle: edge_handles[&edge.edge_id],
                edge_id: edge.edge_id,
                source_node: node_handles[&edge.source_node_id],
                target_node: node_handles[&edge.target_node_id],
                condition_id: edge.condition_id,
                priority: edge.priority,
                branch_order: edge.branch_order,
                max_traversals_per_run: edge.backedge.map(|value| value.max_traversals_per_run),
            })
            .collect();
        canonical.push(CanonicalWorkflowTemplate {
            handle: workflow_handles[id],
            workflow_id: *id,
            canonical_name: workflow.canonical_name.clone(),
            permanent: workflow.permanent,
            nodes,
            edges,
        });
    }
    Ok((
        CanonicalWorkflowIr {
            schema_version: WorkflowArtifactVersion::canonical_ir(),
            workflows: canonical,
        },
        workflow_handles,
        node_handles,
        edge_handles,
    ))
}

fn canonical_node(
    node: &Node,
    workflows: &BTreeMap<StableId, WorkflowHandle>,
    nodes: &BTreeMap<StableId, WorkflowNodeHandle>,
) -> Result<CanonicalWorkflowNode, WorkflowPlanInputError> {
    let node_kind = match node.kind {
        NodeKind::Entry => CanonicalWorkflowNodeKind::Entry,
        NodeKind::Action => CanonicalWorkflowNodeKind::Action,
        NodeKind::Decision => CanonicalWorkflowNodeKind::Decision,
        NodeKind::Fork => CanonicalWorkflowNodeKind::Fork,
        NodeKind::Join {
            mode,
            fork_id,
            loser_policy,
        } => CanonicalWorkflowNodeKind::Join {
            mode: match mode {
                JoinMode::Merge => CanonicalJoinMode::Merge,
                JoinMode::JoinAll => CanonicalJoinMode::JoinAll,
                JoinMode::JoinAny => CanonicalJoinMode::JoinAny,
            },
            fork_node: fork_id.map(|id| nodes[&id]),
            loser_policy: loser_policy.map(|policy| match policy {
                JoinPolicy::CancelOthers => CanonicalJoinPolicy::CancelOthers,
                JoinPolicy::KeepRunning => CanonicalJoinPolicy::KeepRunning,
                JoinPolicy::WaitAtBoundary => CanonicalJoinPolicy::WaitAtBoundary,
            }),
        },
        NodeKind::Wait(WaitMode::Cycles { wait_cycles }) => {
            CanonicalWorkflowNodeKind::WaitCycles { wait_cycles }
        }
        NodeKind::Wait(WaitMode::Condition {
            condition_id,
            timeout_cycles,
            permanent,
        }) => CanonicalWorkflowNodeKind::WaitCondition {
            condition_id,
            timeout_cycles,
            permanent,
        },
        NodeKind::Subworkflow { target_workflow_id } => CanonicalWorkflowNodeKind::Subworkflow {
            target_workflow: workflows
                .get(&target_workflow_id)
                .copied()
                .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?,
        },
        NodeKind::End => CanonicalWorkflowNodeKind::End,
    };
    Ok(CanonicalWorkflowNode {
        handle: nodes[&node.node_id],
        node_id: node.node_id,
        canonical_name: node.canonical_name.clone(),
        execution_order: node.execution_order,
        cancellation_boundary: node.cancellation_boundary,
        node_kind,
    })
}

type PlanTables = (
    Vec<PlannedWorkflowInstance>,
    Vec<WorkflowPlanStep>,
    Vec<PlannedWorkflowEdge>,
    BTreeMap<(InstanceKey, StableId), WorkflowStepHandle>,
);

#[allow(
    clippy::items_after_statements,
    reason = "the recursive helper is scoped to the only plan-table construction that uses it"
)]
fn build_plan_tables(
    drafts: &[InstanceDraft],
    instance_handles: &BTreeMap<InstanceKey, WorkflowInstanceHandle>,
    workflow_handles: &BTreeMap<StableId, WorkflowHandle>,
    node_handles: &BTreeMap<StableId, WorkflowNodeHandle>,
    edge_handles: &BTreeMap<StableId, WorkflowEdgeHandle>,
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
) -> Result<PlanTables, WorkflowPlanInputError> {
    let instances = drafts
        .iter()
        .map(|draft| PlannedWorkflowInstance {
            handle: instance_handles[&draft.key],
            task_handle: draft.key.task_handle,
            workflow: workflow_handles[&draft.workflow_id],
            instance_path: draft.key.path.clone(),
        })
        .collect::<Vec<_>>();
    let mut step_drafts = Vec::new();
    let mut task_orders = BTreeMap::<u32, u32>::new();
    let draft_map = drafts
        .iter()
        .map(|draft| (draft.key.clone(), draft))
        .collect::<BTreeMap<_, _>>();
    fn append_steps(
        key: &InstanceKey,
        drafts: &BTreeMap<InstanceKey, &InstanceDraft>,
        task_orders: &mut BTreeMap<u32, u32>,
        output: &mut Vec<(InstanceKey, StableId, u32, Option<InstanceKey>)>,
    ) -> Result<(), WorkflowPlanInputError> {
        let draft = drafts
            .get(key)
            .copied()
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        for node_id in &draft.step_nodes {
            let order = task_orders.entry(key.task_handle).or_default();
            let current = *order;
            *order = order
                .checked_add(1)
                .ok_or(WorkflowPlanInputError::ArithmeticOverflow)?;
            let child = draft.child_by_node.get(node_id).cloned();
            output.push((key.clone(), *node_id, current, child.clone()));
            if let Some(child) = child {
                append_steps(&child, drafts, task_orders, output)?;
            }
        }
        Ok(())
    }
    let child_keys = drafts
        .iter()
        .flat_map(|draft| draft.child_by_node.values().cloned())
        .collect::<BTreeSet<_>>();
    let roots = drafts
        .iter()
        .filter(|draft| !child_keys.contains(&draft.key))
        .map(|draft| draft.key.clone())
        .collect::<Vec<_>>();
    for root in roots {
        append_steps(&root, &draft_map, &mut task_orders, &mut step_drafts)?;
    }
    let mut steps = Vec::with_capacity(step_drafts.len());
    let mut step_by_key = BTreeMap::new();
    for (index, (instance, node_id, task_order, child)) in step_drafts.into_iter().enumerate() {
        let handle = WorkflowStepHandle(dense(index)?);
        step_by_key.insert((instance.clone(), node_id), handle);
        steps.push(WorkflowPlanStep {
            handle,
            task_handle: instance.task_handle,
            instance: instance_handles[&instance],
            node: node_handles[&node_id],
            task_execution_order: task_order,
            child_instance: child.map(|key| instance_handles[&key]),
        });
    }
    let mut expanded = Vec::new();
    for draft in drafts {
        let workflow = workflows[&draft.workflow_id];
        let mut edges = workflow.edges.iter().collect::<Vec<_>>();
        edges.sort_by_key(|edge| edge_handles[&edge.edge_id]);
        for edge in edges {
            expanded.push((
                instance_handles[&draft.key],
                edge_handles[&edge.edge_id],
                step_by_key
                    .get(&(draft.key.clone(), edge.source_node_id))
                    .copied(),
            ));
        }
    }
    expanded.sort();
    let edges = expanded
        .into_iter()
        .enumerate()
        .map(|(index, (instance, edge, source_step))| {
            Ok(PlannedWorkflowEdge {
                handle: ExpandedEdgeHandle(dense(index)?),
                instance,
                edge,
                source_step,
            })
        })
        .collect::<Result<Vec<_>, WorkflowPlanInputError>>()?;
    Ok((instances, steps, edges, step_by_key))
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Plan 1.3 structure is generated and audited as one indivisible provenance catalog"
)]
fn build_trace_structure(
    drafts: &[InstanceDraft],
    instance_handles: &BTreeMap<InstanceKey, WorkflowInstanceHandle>,
    steps: &[WorkflowPlanStep],
    expanded_edges: &[PlannedWorkflowEdge],
    step_by_key: &BTreeMap<(InstanceKey, StableId), WorkflowStepHandle>,
    edge_handles: &BTreeMap<StableId, WorkflowEdgeHandle>,
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
    claims: &BTreeMap<(InstanceKey, StableId), &ExpandedNodeResourceInput>,
) -> Result<PlannedTraceStructure, WorkflowPlanInputError> {
    let step_keys = step_by_key
        .iter()
        .map(|(key, handle)| (*handle, key.clone()))
        .collect::<BTreeMap<_, _>>();
    if step_keys.len() != steps.len() {
        return Err(WorkflowPlanInputError::GenerationAudit);
    }
    let drafts_by_instance = drafts
        .iter()
        .map(|draft| (instance_handles[&draft.key], draft))
        .collect::<BTreeMap<_, _>>();
    let mut call_handles = BTreeMap::new();
    let mut next_call = BTreeMap::<u32, u32>::new();
    for step in steps {
        let (instance_key, node_id) = step_keys
            .get(&step.handle)
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        let draft = drafts_by_instance
            .get(&step.instance)
            .copied()
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        let node = workflows[&draft.workflow_id]
            .nodes
            .iter()
            .find(|node| node.node_id == *node_id)
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        if matches!(node.kind, NodeKind::Subworkflow { .. }) {
            let handle = next_call.entry(instance_key.task_handle).or_default();
            call_handles.insert(step.handle, *handle);
            *handle = handle
                .checked_add(1)
                .ok_or(WorkflowPlanInputError::ArithmeticOverflow)?;
        }
    }

    let memberships = workflows
        .iter()
        .map(|(workflow_id, workflow)| (*workflow_id, trace_branch_memberships(workflow)))
        .collect::<BTreeMap<_, _>>();
    let mut nodes = Vec::with_capacity(steps.len());
    for (expected, step) in steps.iter().enumerate() {
        if step.handle.0 != dense(expected)? {
            return Err(WorkflowPlanInputError::GenerationAudit);
        }
        let (_, node_id) = step_keys
            .get(&step.handle)
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        let draft = drafts_by_instance
            .get(&step.instance)
            .copied()
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        let workflow = workflows[&draft.workflow_id];
        let node = workflow
            .nodes
            .iter()
            .find(|node| node.node_id == *node_id)
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        let fork_branch_orders = |fork_id: StableId| {
            let mut orders = workflow
                .edges
                .iter()
                .filter(|edge| edge.source_node_id == fork_id && edge.backedge.is_none())
                .map(|edge| {
                    edge.branch_order
                        .ok_or(WorkflowPlanInputError::GenerationAudit)
                })
                .collect::<Result<Vec<_>, _>>()?;
            orders.sort_unstable();
            Ok::<_, WorkflowPlanInputError>(orders)
        };
        let node_kind = match node.kind {
            NodeKind::Action => PlannedTraceNodeKind::Action,
            NodeKind::Decision => PlannedTraceNodeKind::Decision,
            NodeKind::Fork => PlannedTraceNodeKind::Fork {
                branch_orders: fork_branch_orders(node.node_id)?,
            },
            NodeKind::Join {
                mode: JoinMode::Merge,
                ..
            } => PlannedTraceNodeKind::Merge,
            NodeKind::Join {
                mode: JoinMode::JoinAll,
                fork_id: Some(fork_id),
                ..
            } => PlannedTraceNodeKind::JoinAll {
                branch_orders: fork_branch_orders(fork_id)?,
            },
            NodeKind::Join {
                mode: JoinMode::JoinAny,
                fork_id: Some(fork_id),
                loser_policy: Some(loser_policy),
            } => PlannedTraceNodeKind::JoinAny {
                loser_policy: canonical_join_policy(loser_policy),
                branch_orders: fork_branch_orders(fork_id)?,
            },
            NodeKind::Wait(WaitMode::Cycles { .. }) => PlannedTraceNodeKind::WaitCycles,
            NodeKind::Wait(WaitMode::Condition { timeout_cycles, .. }) => {
                PlannedTraceNodeKind::WaitCondition {
                    has_timeout: timeout_cycles.is_some(),
                }
            }
            NodeKind::Subworkflow { .. } => {
                let binding = claims
                    .get(&(draft.key.clone(), node.node_id))
                    .and_then(|claim| claim.subworkflow_binding.as_ref())
                    .ok_or(WorkflowPlanInputError::GenerationAudit)?;
                PlannedTraceNodeKind::Subworkflow {
                    call_handle: *call_handles
                        .get(&step.handle)
                        .ok_or(WorkflowPlanInputError::GenerationAudit)?,
                    child_instance: step
                        .child_instance
                        .ok_or(WorkflowPlanInputError::GenerationAudit)?,
                    input_copies: binding.input_copies.clone(),
                    output_copies: binding.output_copies.clone(),
                }
            }
            NodeKind::Entry | NodeKind::End | NodeKind::Join { .. } => {
                return Err(WorkflowPlanInputError::GenerationAudit);
            }
        };
        let branch_memberships = memberships[&draft.workflow_id]
            .get(&node.node_id)
            .into_iter()
            .flatten()
            .map(|(join_id, branch_order)| {
                Ok(PlannedTraceBranchMembership {
                    join_step: *step_by_key
                        .get(&(draft.key.clone(), *join_id))
                        .ok_or(WorkflowPlanInputError::GenerationAudit)?,
                    branch_order: *branch_order,
                })
            })
            .collect::<Result<Vec<_>, WorkflowPlanInputError>>()?;
        let cancellation_branch_orders = if node.cancellation_boundary {
            branch_memberships
                .iter()
                .map(|membership| membership.branch_order)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        nodes.push(PlannedTraceNode {
            step: step.handle,
            node_kind,
            cancellation_boundary: node.cancellation_boundary,
            cancellation_branch_orders,
            branch_memberships,
        });
    }

    let expanded_by_identity = expanded_edges
        .iter()
        .map(|edge| ((edge.instance, edge.edge), edge.handle))
        .collect::<BTreeMap<_, _>>();
    let step_by_handle = steps
        .iter()
        .map(|step| (step.handle, step))
        .collect::<BTreeMap<_, _>>();
    let mut task_edges = BTreeMap::<u32, Vec<(u32, u32, PlannedTraceEdge)>>::new();
    for draft in drafts {
        let instance = instance_handles[&draft.key];
        let workflow = workflows[&draft.workflow_id];
        for edge in &workflow.edges {
            let Some(source_step) = step_by_key
                .get(&(draft.key.clone(), edge.source_node_id))
                .copied()
            else {
                continue;
            };
            let source = step_by_handle
                .get(&source_step)
                .copied()
                .ok_or(WorkflowPlanInputError::GenerationAudit)?;
            let source_node = workflow
                .nodes
                .iter()
                .find(|node| node.node_id == edge.source_node_id)
                .ok_or(WorkflowPlanInputError::GenerationAudit)?;
            let target_node = workflow
                .nodes
                .iter()
                .find(|node| node.node_id == edge.target_node_id)
                .ok_or(WorkflowPlanInputError::GenerationAudit)?;
            let target = if target_node.kind == NodeKind::End {
                PlannedTraceEdgeTarget::Complete
            } else {
                PlannedTraceEdgeTarget::Step {
                    step: *step_by_key
                        .get(&(draft.key.clone(), edge.target_node_id))
                        .ok_or(WorkflowPlanInputError::GenerationAudit)?,
                }
            };
            let expanded_edge = *expanded_by_identity
                .get(&(instance, edge_handles[&edge.edge_id]))
                .ok_or(WorkflowPlanInputError::GenerationAudit)?;
            let local_order = match source_node.kind {
                NodeKind::Decision => edge
                    .priority
                    .ok_or(WorkflowPlanInputError::GenerationAudit)?,
                NodeKind::Fork => edge
                    .branch_order
                    .ok_or(WorkflowPlanInputError::GenerationAudit)?,
                _ => edge_handles[&edge.edge_id].0,
            };
            task_edges.entry(draft.key.task_handle).or_default().push((
                source.task_execution_order,
                local_order,
                PlannedTraceEdge {
                    task_handle: draft.key.task_handle,
                    runtime_edge_handle: 0,
                    expanded_edge,
                    source_step,
                    target,
                    branch_order: edge.branch_order,
                },
            ));
        }
    }
    let mut edges = Vec::new();
    for values in task_edges.values_mut() {
        values.sort_by_key(|(source, local, edge)| (*source, *local, edge.expanded_edge));
        for (index, (_, _, mut edge)) in values.drain(..).enumerate() {
            edge.runtime_edge_handle = dense(index)?;
            edges.push(edge);
        }
    }

    let child_instances = steps
        .iter()
        .filter_map(|step| step.child_instance)
        .collect::<BTreeSet<_>>();
    let mut root_instances = instance_handles
        .values()
        .copied()
        .filter(|handle| !child_instances.contains(handle))
        .collect::<Vec<_>>();
    root_instances.sort_unstable();
    let mut all_instances = instance_handles.values().copied().collect::<Vec<_>>();
    all_instances.sort_unstable();
    let mut initial_active = Vec::with_capacity(all_instances.len());
    for instance in &all_instances {
        let draft = drafts_by_instance
            .get(instance)
            .copied()
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        let workflow = workflows[&draft.workflow_id];
        let entry = workflow
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Entry)
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        let mut outgoing = workflow
            .edges
            .iter()
            .filter(|edge| edge.source_node_id == entry.node_id);
        let target = outgoing
            .next()
            .ok_or(WorkflowPlanInputError::GenerationAudit)?
            .target_node_id;
        if outgoing.next().is_some() {
            return Err(WorkflowPlanInputError::GenerationAudit);
        }
        let target_node = workflow
            .nodes
            .iter()
            .find(|node| node.node_id == target)
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        if target_node.kind != NodeKind::End {
            initial_active.push(
                *step_by_key
                    .get(&(draft.key.clone(), target))
                    .ok_or(WorkflowPlanInputError::GenerationAudit)?,
            );
        }
    }
    initial_active.sort_unstable();
    Ok(PlannedTraceStructure {
        initial_active,
        root_instances,
        nodes,
        edges,
    })
}

fn trace_branch_memberships(
    workflow: &WorkflowDocument,
) -> BTreeMap<StableId, BTreeSet<(StableId, u32)>> {
    let forward = workflow
        .edges
        .iter()
        .filter(|edge| edge.backedge.is_none())
        .collect::<Vec<_>>();
    let mut memberships = BTreeMap::<StableId, BTreeSet<(StableId, u32)>>::new();
    for join in &workflow.nodes {
        let NodeKind::Join {
            mode: JoinMode::JoinAll | JoinMode::JoinAny,
            fork_id: Some(fork_id),
            ..
        } = join.kind
        else {
            continue;
        };
        let can_reach_join = reverse_reachable_from(join.node_id, &forward);
        for branch in forward.iter().filter(|edge| edge.source_node_id == fork_id) {
            let Some(branch_order) = branch.branch_order else {
                continue;
            };
            let reachable = reachable_from_refs(branch.target_node_id, &forward);
            for node_id in reachable.intersection(&can_reach_join).copied() {
                if node_id != fork_id && node_id != join.node_id {
                    memberships
                        .entry(node_id)
                        .or_default()
                        .insert((join.node_id, branch_order));
                }
            }
        }
    }
    memberships
}

const fn canonical_join_policy(policy: JoinPolicy) -> CanonicalJoinPolicy {
    match policy {
        JoinPolicy::CancelOthers => CanonicalJoinPolicy::CancelOthers,
        JoinPolicy::KeepRunning => CanonicalJoinPolicy::KeepRunning,
        JoinPolicy::WaitAtBoundary => CanonicalJoinPolicy::WaitAtBoundary,
    }
}

fn prepare_claims<'a>(
    inputs: &'a [ExpandedNodeResourceInput],
    drafts: &[InstanceDraft],
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
    target_limits: WorkflowTargetLimits,
    require_bindings: bool,
    require_trace_structure: bool,
    binding_images: &BTreeMap<u32, TaskBindingImageInput>,
) -> Result<BTreeMap<(InstanceKey, StableId), &'a ExpandedNodeResourceInput>, WorkflowPlanInputError>
{
    let mut expected = BTreeMap::new();
    for draft in drafts {
        let workflow = workflows[&draft.workflow_id];
        for node in &workflow.nodes {
            if matches!(node.kind, NodeKind::Action | NodeKind::Subworkflow { .. }) {
                expected.insert(
                    (draft.key.clone(), node.node_id),
                    node.kind == NodeKind::Action,
                );
            }
        }
    }
    let mut actual = BTreeMap::new();
    let mut binding_ids = BTreeSet::new();
    if inputs.len() > expected.len() {
        return Err(WorkflowPlanInputError::InvalidResourceClaim);
    }
    for input in inputs {
        let key = (
            InstanceKey {
                task_handle: input.task_handle,
                path: input.instance_path.clone(),
            },
            input.node_id,
        );
        let Some(is_action) = expected.get(&key).copied() else {
            return Err(WorkflowPlanInputError::InvalidResourceClaim);
        };
        for region in &input.writes {
            if region.size_bytes == 0
                || region.offset_bytes.checked_add(region.size_bytes).is_none()
            {
                return Err(WorkflowPlanInputError::InvalidResourceClaim);
            }
        }
        match (require_bindings, is_action, &input.action_binding) {
            (true, true, Some(binding)) => {
                let image = binding_images
                    .get(&input.task_handle)
                    .copied()
                    .ok_or(WorkflowPlanInputError::InvalidBindingImage)?;
                validate_action_binding(input, binding, target_limits.values(), image)?;
                if !binding_ids.insert(binding.binding_id) {
                    return Err(WorkflowPlanInputError::InvalidActionBinding);
                }
            }
            (true, false, None) | (false, _, None) => {}
            _ => return Err(WorkflowPlanInputError::InvalidActionBinding),
        }
        if is_action {
            if input.subworkflow_binding.is_some() {
                return Err(WorkflowPlanInputError::InvalidTraceBinding);
            }
        } else {
            match (require_trace_structure, &input.subworkflow_binding) {
                (true, Some(binding)) => {
                    let image = binding_images
                        .get(&input.task_handle)
                        .copied()
                        .ok_or(WorkflowPlanInputError::InvalidBindingImage)?;
                    for copy in binding.input_copies.iter().chain(&binding.output_copies) {
                        if copy.source_offset_bytes >= image.application_state_bytes
                            || copy.target_offset_bytes >= image.application_state_bytes
                        {
                            return Err(WorkflowPlanInputError::InvalidTraceBinding);
                        }
                    }
                }
                (false, None) => {}
                _ => return Err(WorkflowPlanInputError::InvalidTraceBinding),
            }
        }
        if actual.insert(key, input).is_some() {
            return Err(WorkflowPlanInputError::InvalidResourceClaim);
        }
    }
    let actual_keys = actual.keys().cloned().collect::<BTreeSet<_>>();
    let expected_keys = expected.keys().cloned().collect::<BTreeSet<_>>();
    if actual_keys != expected_keys {
        return Err(WorkflowPlanInputError::InvalidResourceClaim);
    }
    Ok(actual)
}

fn validate_action_binding(
    claim: &ExpandedNodeResourceInput,
    binding: &ExpandedActionBindingInput,
    limits: WorkflowTargetLimitValues,
    image: TaskBindingImageInput,
) -> Result<(), WorkflowPlanInputError> {
    if binding.version != crate::WorkflowBindingVersion::V1_0
        || binding.target_handle == u32::MAX
        || usize_u64(binding.ports.len())? > limits.max_action_ports_per_node
        || binding.committed_state_bytes != claim.committed_state_bytes
        || binding.staging_state_bytes != claim.staging_state_bytes
        || binding.trace_events_per_release != claim.trace_events_per_release
        || binding
            .invocation_state_offset_bytes
            .checked_add(binding.committed_state_bytes)
            .is_none_or(|end| end > image.application_state_bytes)
    {
        return Err(WorkflowPlanInputError::InvalidActionBinding);
    }
    for (index, port) in binding.ports.iter().enumerate() {
        if u32::try_from(index) != Ok(port.port) || !slot_fits_image(port.slot, image) {
            return Err(WorkflowPlanInputError::InvalidActionBinding);
        }
        let valid = match binding.kind {
            crate::WorkflowActionKind::StPou => true,
            crate::WorkflowActionKind::IoImage | crate::WorkflowActionKind::TypedCommand => {
                match port.direction {
                    crate::WorkflowPortDirection::Input => {
                        port.slot.area == WorkflowValueArea::State
                    }
                    crate::WorkflowPortDirection::Output => {
                        port.slot.area == WorkflowValueArea::Output
                    }
                    crate::WorkflowPortDirection::InOut => false,
                }
            }
        };
        if !valid {
            return Err(WorkflowPlanInputError::InvalidActionBinding);
        }
    }
    for (index, left) in binding.ports.iter().enumerate() {
        if !matches!(
            left.direction,
            crate::WorkflowPortDirection::Output | crate::WorkflowPortDirection::InOut
        ) {
            continue;
        }
        for right in binding.ports.iter().skip(index + 1).filter(|port| {
            matches!(
                port.direction,
                crate::WorkflowPortDirection::Output | crate::WorkflowPortDirection::InOut
            )
        }) {
            let logical_overlap = left.slot.target_id == right.slot.target_id
                && ranges_overlap(
                    left.slot.offset_bytes,
                    left.slot.value_type.size_bytes(),
                    right.slot.offset_bytes,
                    right.slot.value_type.size_bytes(),
                );
            let physical_overlap = left.slot.area == right.slot.area
                && ranges_overlap(
                    left.slot.image_offset_bytes,
                    left.slot.value_type.size_bytes(),
                    right.slot.image_offset_bytes,
                    right.slot.value_type.size_bytes(),
                );
            if logical_overlap || physical_overlap {
                return Err(WorkflowPlanInputError::InvalidActionBinding);
            }
        }
    }
    if binding.kind == crate::WorkflowActionKind::TypedCommand
        && binding
            .ports
            .iter()
            .filter(|port| port.direction == crate::WorkflowPortDirection::Output)
            .count()
            != 1
    {
        return Err(WorkflowPlanInputError::InvalidActionBinding);
    }
    let mut claimed_writes = claim.writes.clone();
    claimed_writes.sort();
    if claimed_writes != binding.derived_writes() {
        return Err(WorkflowPlanInputError::InvalidActionBinding);
    }
    Ok(())
}

fn build_planned_node_resources(
    claims: &BTreeMap<(InstanceKey, StableId), &ExpandedNodeResourceInput>,
    steps: &BTreeMap<(InstanceKey, StableId), WorkflowStepHandle>,
) -> Vec<PlannedNodeResources> {
    let mut resources = claims
        .iter()
        .map(|(key, claim)| {
            let mut writes = claim.writes.clone();
            writes.sort();
            PlannedNodeResources {
                step: steps[key],
                committed_state_bytes: claim.committed_state_bytes,
                staging_state_bytes: claim.staging_state_bytes,
                trace_events_per_release: claim.trace_events_per_release,
                writes,
                action_binding: claim.action_binding.clone(),
            }
        })
        .collect::<Vec<_>>();
    resources.sort_by_key(|resource| resource.step);
    resources
}

fn build_planned_watches(
    tasks: &[&TaskWorkflowPlanningInput],
) -> Result<Vec<PlannedWorkflowWatch>, WorkflowPlanInputError> {
    let mut descriptors = tasks
        .iter()
        .flat_map(|task| task.watches.iter().map(|watch| (task.task_handle, watch)))
        .collect::<Vec<_>>();
    descriptors.sort_by_key(|(task, watch)| (*task, watch.value_id.network_bytes()));
    descriptors
        .into_iter()
        .enumerate()
        .map(|(index, (task_handle, watch))| {
            let fragments = trace_fragments(watch.encoded_bytes);
            Ok(PlannedWorkflowWatch {
                handle: WorkflowWatchHandle(dense(index)?),
                task_handle,
                value_id: watch.value_id,
                encoded_bytes: watch.encoded_bytes,
                fragment_count: u16::try_from(fragments)
                    .map_err(|_| WorkflowPlanInputError::GenerationAudit)?,
            })
        })
        .collect()
}

#[allow(
    clippy::too_many_lines,
    reason = "单次闭包审计必须在发布前共同核对 Output 与 Watch 的 expected/actual 集合"
)]
fn build_trace_values(
    resources: &[PlannedNodeResources],
    watches: &[PlannedWorkflowWatch],
    inputs: &[WorkflowWatchBindingInput],
    steps: &[WorkflowPlanStep],
    instances: &[PlannedWorkflowInstance],
    images: &BTreeMap<u32, TaskBindingImageInput>,
) -> Result<Vec<PlannedTraceValue>, WorkflowPlanInputError> {
    let step_map = steps
        .iter()
        .map(|step| (step.handle, step))
        .collect::<BTreeMap<_, _>>();
    let instance_map = instances
        .iter()
        .map(|instance| {
            (
                (instance.task_handle, instance.instance_path.clone()),
                instance.handle,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let expected_watches = watches
        .iter()
        .map(|watch| ((watch.task_handle, watch.value_id), watch))
        .collect::<BTreeMap<_, _>>();
    if expected_watches.len() != watches.len() || inputs.len() != watches.len() {
        return Err(WorkflowPlanInputError::InvalidTraceBinding);
    }
    let mut actual_watches = BTreeMap::new();
    for input in inputs {
        let key = (input.task_handle, input.value_id);
        let planned = expected_watches
            .get(&key)
            .copied()
            .ok_or(WorkflowPlanInputError::InvalidTraceBinding)?;
        let image = images
            .get(&input.task_handle)
            .copied()
            .ok_or(WorkflowPlanInputError::InvalidBindingImage)?;
        let end = input
            .image_offset_bytes
            .checked_add(input.encoded_bytes)
            .ok_or(WorkflowPlanInputError::InvalidTraceBinding)?;
        let capacity = match input.area {
            WorkflowValueArea::State => image.application_state_bytes,
            WorkflowValueArea::Output => image.output_bytes,
        };
        let instance = instance_map
            .get(&(input.task_handle, input.instance_path.clone()))
            .copied()
            .ok_or(WorkflowPlanInputError::InvalidTraceBinding)?;
        if input.type_handle == u32::MAX
            || input.encoded_bytes == 0
            || input.encoded_bytes != planned.encoded_bytes
            || end > capacity
            || actual_watches.insert(key, (input, instance)).is_some()
        {
            return Err(WorkflowPlanInputError::InvalidTraceBinding);
        }
    }
    if actual_watches.keys().copied().collect::<BTreeSet<_>>()
        != expected_watches.keys().copied().collect::<BTreeSet<_>>()
    {
        return Err(WorkflowPlanInputError::InvalidTraceBinding);
    }

    let mut descriptors = Vec::new();
    let mut outputs = Vec::new();
    for resource in resources {
        let Some(binding) = &resource.action_binding else {
            continue;
        };
        let step = step_map
            .get(&resource.step)
            .copied()
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        for port in &binding.ports {
            if !matches!(
                port.direction,
                WorkflowPortDirection::Output | WorkflowPortDirection::InOut
            ) {
                continue;
            }
            outputs.push((step, port));
        }
    }
    outputs.sort_by_key(|(step, port)| (step.task_handle, step.handle, port.port));
    for (step, port) in outputs {
        descriptors.push(PlannedTraceValue {
            handle: WorkflowTraceValueHandle(dense(descriptors.len())?),
            task_handle: step.task_handle,
            instance: step.instance,
            value_id: port.slot.target_id,
            source: WorkflowTraceValueSource::Output {
                step: step.handle,
                port: port.port,
            },
            type_handle: workflow_value_type_handle(port.slot.value_type),
            area: port.slot.area,
            image_offset_bytes: port.slot.image_offset_bytes,
            encoded_bytes: port.slot.value_type.size_bytes(),
            fragment_count: u16::try_from(trace_fragments(port.slot.value_type.size_bytes()))
                .map_err(|_| WorkflowPlanInputError::GenerationAudit)?,
        });
    }
    for watch in watches {
        let (input, instance) = actual_watches
            .get(&(watch.task_handle, watch.value_id))
            .copied()
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        descriptors.push(PlannedTraceValue {
            handle: WorkflowTraceValueHandle(dense(descriptors.len())?),
            task_handle: watch.task_handle,
            instance,
            value_id: watch.value_id,
            source: WorkflowTraceValueSource::Watch {
                watch: watch.handle,
            },
            type_handle: input.type_handle,
            area: input.area,
            image_offset_bytes: input.image_offset_bytes,
            encoded_bytes: input.encoded_bytes,
            fragment_count: u16::try_from(trace_fragments(input.encoded_bytes))
                .map_err(|_| WorkflowPlanInputError::InvalidTraceBinding)?,
        });
    }
    let output_count = resources
        .iter()
        .filter_map(|resource| resource.action_binding.as_ref())
        .flat_map(|binding| &binding.ports)
        .filter(|port| port.direction != WorkflowPortDirection::Input)
        .count();
    if descriptors.len() != output_count + watches.len()
        || descriptors
            .iter()
            .enumerate()
            .any(|(index, value)| u32::try_from(index) != Ok(value.handle.0))
    {
        return Err(WorkflowPlanInputError::GenerationAudit);
    }
    Ok(descriptors)
}

const fn workflow_value_type_handle(value_type: WorkflowValueType) -> u32 {
    match value_type {
        WorkflowValueType::Bool => 1,
        WorkflowValueType::Sint => 2,
        WorkflowValueType::Int => 3,
        WorkflowValueType::Dint => 4,
        WorkflowValueType::Lint => 5,
        WorkflowValueType::Usint => 6,
        WorkflowValueType::Uint => 7,
        WorkflowValueType::Udint => 8,
        WorkflowValueType::Ulint => 9,
        WorkflowValueType::Real => 10,
        WorkflowValueType::Lreal => 11,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_condition_bindings(
    inputs: &[WorkflowConditionBindingInput],
    tasks: &[&TaskWorkflowPlanningInput],
    drafts: &[InstanceDraft],
    instances: &BTreeMap<InstanceKey, WorkflowInstanceHandle>,
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
    target_limits: WorkflowTargetLimits,
    require_bindings: bool,
    binding_images: &BTreeMap<u32, TaskBindingImageInput>,
) -> Result<Vec<PlannedConditionBinding>, WorkflowPlanInputError> {
    if !require_bindings {
        return inputs
            .is_empty()
            .then(Vec::new)
            .ok_or(WorkflowPlanInputError::InvalidConditionBinding);
    }

    let task_handles = tasks
        .iter()
        .map(|task| task.task_handle)
        .collect::<BTreeSet<_>>();
    let mut expected = BTreeSet::new();
    for draft in drafts {
        let workflow = workflows[&draft.workflow_id];
        for node in &workflow.nodes {
            if let NodeKind::Wait(WaitMode::Condition { condition_id, .. }) = node.kind {
                expected.insert((draft.key.clone(), condition_id));
            }
        }
        for edge in &workflow.edges {
            if let Some(condition_id) = edge.condition_id {
                expected.insert((draft.key.clone(), condition_id));
            }
        }
    }

    if inputs.len() > expected.len() {
        return Err(WorkflowPlanInputError::InvalidConditionBinding);
    }
    let mut actual = BTreeMap::new();
    let mut counts = BTreeMap::<u32, u64>::new();
    for input in inputs {
        let instance = InstanceKey {
            task_handle: input.task_handle,
            path: input.instance_path.clone(),
        };
        let key = (instance, input.condition_id);
        let image = binding_images
            .get(&input.task_handle)
            .copied()
            .ok_or(WorkflowPlanInputError::InvalidBindingImage)?;
        if !task_handles.contains(&input.task_handle)
            || input.source.value_type != WorkflowValueType::Bool
            || !slot_fits_image(input.source, image)
            || actual.insert(key, input.source).is_some()
        {
            return Err(WorkflowPlanInputError::InvalidConditionBinding);
        }
        let count = counts.entry(input.task_handle).or_default();
        *count = count
            .checked_add(1)
            .ok_or(WorkflowPlanInputError::ArithmeticOverflow)?;
        if *count > target_limits.values().max_condition_bindings_per_task {
            return Err(WorkflowPlanInputError::InvalidConditionBinding);
        }
    }
    if actual.keys().cloned().collect::<BTreeSet<_>>() != expected {
        return Err(WorkflowPlanInputError::InvalidConditionBinding);
    }

    actual
        .into_iter()
        .enumerate()
        .map(|(index, ((instance_key, condition_id), source))| {
            Ok(PlannedConditionBinding {
                handle: WorkflowConditionHandle(dense(index)?),
                task_handle: instance_key.task_handle,
                instance: instances
                    .get(&instance_key)
                    .copied()
                    .ok_or(WorkflowPlanInputError::GenerationAudit)?,
                condition_id,
                source,
            })
        })
        .collect()
}

fn slot_fits_image(slot: WorkflowValueSlot, image: TaskBindingImageInput) -> bool {
    if !slot.end_is_representable() {
        return false;
    }
    let Some(end) = slot
        .image_offset_bytes
        .checked_add(slot.value_type.size_bytes())
    else {
        return false;
    };
    match slot.area {
        WorkflowValueArea::State => end <= image.application_state_bytes,
        WorkflowValueArea::Output => end <= image.output_bytes,
    }
}

fn validate_resolved_slot_mapping(
    claims: &BTreeMap<(InstanceKey, StableId), &ExpandedNodeResourceInput>,
    conditions: &[WorkflowConditionBindingInput],
    images: &BTreeMap<u32, TaskBindingImageInput>,
) -> Result<(), WorkflowPlanInputError> {
    let mut logical_to_physical = BTreeMap::new();
    let mut physical_to_logical = BTreeMap::new();
    let mut invocation_ranges = BTreeMap::<u32, Vec<(u64, u64)>>::new();
    let mut state_slot_ranges = BTreeMap::<u32, Vec<(u64, u64)>>::new();
    for claim in claims.values() {
        let Some(binding) = &claim.action_binding else {
            continue;
        };
        let image = images
            .get(&claim.task_handle)
            .copied()
            .ok_or(WorkflowPlanInputError::InvalidBindingImage)?;
        for port in &binding.ports {
            audit_slot_mapping(
                claim.task_handle,
                port.slot,
                image,
                &mut logical_to_physical,
                &mut physical_to_logical,
            )?;
            if port.slot.area == WorkflowValueArea::State {
                state_slot_ranges
                    .entry(claim.task_handle)
                    .or_default()
                    .push((
                        port.slot.image_offset_bytes,
                        port.slot.image_offset_bytes + port.slot.value_type.size_bytes(),
                    ));
            }
        }
        if binding.committed_state_bytes != 0 {
            let end = binding
                .invocation_state_offset_bytes
                .checked_add(binding.committed_state_bytes)
                .ok_or(WorkflowPlanInputError::InvalidBindingImage)?;
            invocation_ranges
                .entry(claim.task_handle)
                .or_default()
                .push((binding.invocation_state_offset_bytes, end));
        }
    }
    for condition in conditions {
        let image = images
            .get(&condition.task_handle)
            .copied()
            .ok_or(WorkflowPlanInputError::InvalidBindingImage)?;
        audit_slot_mapping(
            condition.task_handle,
            condition.source,
            image,
            &mut logical_to_physical,
            &mut physical_to_logical,
        )?;
        if condition.source.area == WorkflowValueArea::State {
            state_slot_ranges
                .entry(condition.task_handle)
                .or_default()
                .push((
                    condition.source.image_offset_bytes,
                    condition.source.image_offset_bytes + condition.source.value_type.size_bytes(),
                ));
        }
    }
    for ranges in invocation_ranges.values_mut() {
        ranges.sort_unstable();
        if ranges.windows(2).any(|pair| pair[1].0 < pair[0].1) {
            return Err(WorkflowPlanInputError::InvalidActionBinding);
        }
    }
    for (task, invocations) in &invocation_ranges {
        if invocations.iter().any(|invocation| {
            state_slot_ranges
                .get(task)
                .is_some_and(|slots| slots.iter().any(|slot| pair_overlaps(*invocation, *slot)))
        }) {
            return Err(WorkflowPlanInputError::InvalidActionBinding);
        }
    }
    Ok(())
}

fn ranges_overlap(left_start: u64, left_size: u64, right_start: u64, right_size: u64) -> bool {
    left_start < right_start + right_size && right_start < left_start + left_size
}

fn pair_overlaps(left: (u64, u64), right: (u64, u64)) -> bool {
    left.0 < right.1 && right.0 < left.1
}

fn audit_slot_mapping(
    task: u32,
    slot: WorkflowValueSlot,
    image: TaskBindingImageInput,
    logical_to_physical: &mut BTreeMap<(u32, StableId, u64), (WorkflowValueArea, u64)>,
    physical_to_logical: &mut BTreeMap<(u32, WorkflowValueArea, u64), (StableId, u64)>,
) -> Result<(), WorkflowPlanInputError> {
    if !slot_fits_image(slot, image) {
        return Err(WorkflowPlanInputError::InvalidBindingImage);
    }
    for byte in 0..slot.value_type.size_bytes() {
        let logical = (task, slot.target_id, slot.offset_bytes + byte);
        let physical = (task, slot.area, slot.image_offset_bytes + byte);
        let resolved = (slot.area, slot.image_offset_bytes + byte);
        if logical_to_physical
            .insert(logical, resolved)
            .is_some_and(|existing| existing != resolved)
            || physical_to_logical
                .insert(physical, (slot.target_id, slot.offset_bytes + byte))
                .is_some_and(|existing| existing != (slot.target_id, slot.offset_bytes + byte))
        {
            return Err(WorkflowPlanInputError::InvalidBindingImage);
        }
    }
    Ok(())
}

fn validate_write_conflicts(
    claims: &BTreeMap<(InstanceKey, StableId), &ExpandedNodeResourceInput>,
    steps: &BTreeMap<(InstanceKey, StableId), WorkflowStepHandle>,
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
    sources: &BTreeMap<&str, &str>,
) -> Result<Vec<WorkflowDiagnostic>, WorkflowPlanInputError> {
    let mut regions = Vec::new();
    for (key, claim) in claims {
        let step = steps
            .get(key)
            .copied()
            .ok_or(WorkflowPlanInputError::GenerationAudit)?;
        for region in &claim.writes {
            regions.push((claim.task_handle, *region, step, key));
        }
    }
    regions.sort_by_key(|(task, region, step, _)| {
        (
            region.target_id.network_bytes(),
            region.offset_bytes,
            region.size_bytes,
            *task,
            *step,
        )
    });
    let mut diagnostics = Vec::new();
    let mut reported = BTreeSet::new();
    for index in 0..regions.len() {
        let (_, region, _, key) = &regions[index];
        let end = region
            .offset_bytes
            .checked_add(region.size_bytes)
            .ok_or(WorkflowPlanInputError::ArithmeticOverflow)?;
        for (other_task, other, _, other_key) in regions.iter().skip(index + 1) {
            if other.target_id != region.target_id {
                break;
            }
            if other.offset_bytes >= end {
                break;
            }
            if other_key == key {
                continue;
            }
            if reported.insert((*other_task, other_key.0.clone(), other_key.1)) {
                let (workflow, node) = find_expanded_node(&other_key.0, other_key.1, workflows)?;
                diagnostics.push(document_diagnostic(
                    workflow,
                    sources,
                    WorkflowDiagnosticCode::WriteConflict,
                    node.span,
                    Some(node.node_id),
                )?);
            }
        }
    }
    Ok(diagnostics)
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one pass computes and compares the complete indivisible Target Profile proof"
)]
fn prove_resources(
    tasks: &[&TaskWorkflowPlanningInput],
    drafts: &[InstanceDraft],
    claims: &BTreeMap<(InstanceKey, StableId), &ExpandedNodeResourceInput>,
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
    sources: &BTreeMap<&str, &str>,
    limits: WorkflowTargetLimits,
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) -> Result<WorkflowResourceProof, WorkflowPlanInputError> {
    let limit = limits.values();
    let mut proofs = Vec::with_capacity(tasks.len());
    for task in tasks {
        let task_drafts = drafts
            .iter()
            .filter(|draft| draft.key.task_handle == task.task_handle)
            .collect::<Vec<_>>();
        let reachable = task_drafts
            .iter()
            .map(|draft| draft.workflow_id)
            .collect::<BTreeSet<_>>();
        let instances = usize_u64(task_drafts.len())?;
        let nodes = checked_sum(
            task_drafts
                .iter()
                .map(|draft| workflows[&draft.workflow_id].nodes.len()),
        )?;
        let edges = checked_sum(
            task_drafts
                .iter()
                .map(|draft| workflows[&draft.workflow_id].edges.len()),
        )?;
        let executable = checked_sum(task_drafts.iter().map(|draft| draft.step_nodes.len()))?;
        let fork_count = checked_sum(task_drafts.iter().map(|draft| {
            workflows[&draft.workflow_id]
                .nodes
                .iter()
                .filter(|node| node.kind == NodeKind::Fork)
                .count()
        }))?;
        let mut max_branches = 0_u64;
        let mut pending = 0_u64;
        let mut max_backedge = 0_u64;
        let mut max_wait = 0_u64;
        let mut join_token_bytes = 0_u64;
        let mut wait_count = 0_u64;
        let mut backedge_count = 0_u64;
        let mut fork_activations = 0_u64;
        let mut join_count = 0_u64;
        let mut cancellation_events = 0_u64;
        let mut subworkflow_count = 0_u64;
        let mut completion_requests = 0_u64;
        for draft in &task_drafts {
            let workflow = workflows[&draft.workflow_id];
            for node in &workflow.nodes {
                match node.kind {
                    NodeKind::Fork => {
                        let branches = usize_u64(
                            workflow
                                .edges
                                .iter()
                                .filter(|edge| edge.source_node_id == node.node_id)
                                .count(),
                        )?;
                        max_branches = max_branches.max(branches);
                        fork_activations = checked_add(fork_activations, branches)?;
                    }
                    NodeKind::Join {
                        mode,
                        fork_id: Some(fork_id),
                        loser_policy,
                    } if mode != JoinMode::Merge => {
                        join_count = checked_add(join_count, 1)?;
                        let branches = usize_u64(
                            workflow
                                .edges
                                .iter()
                                .filter(|edge| edge.source_node_id == fork_id)
                                .count(),
                        )?;
                        join_token_bytes = checked_add(join_token_bytes, ceil_div_8(branches)?)?;
                        if loser_policy == Some(JoinPolicy::WaitAtBoundary) {
                            pending = checked_add(pending, branches.saturating_sub(1))?;
                        }
                        if mode == JoinMode::JoinAny {
                            cancellation_events = checked_add(
                                cancellation_events,
                                checked_mul(branches.saturating_sub(1), 2)?,
                            )?;
                        }
                    }
                    NodeKind::Join { .. } => {
                        join_count = checked_add(join_count, 1)?;
                    }
                    NodeKind::Wait(WaitMode::Cycles { wait_cycles }) => {
                        wait_count = checked_add(wait_count, 1)?;
                        max_wait = max_wait.max(wait_cycles);
                    }
                    NodeKind::Wait(WaitMode::Condition { timeout_cycles, .. }) => {
                        wait_count = checked_add(wait_count, 1)?;
                        max_wait = max_wait.max(timeout_cycles.unwrap_or(0));
                    }
                    NodeKind::Subworkflow { .. } => {
                        subworkflow_count = checked_add(subworkflow_count, 1)?;
                    }
                    NodeKind::End => {
                        completion_requests = checked_add(completion_requests, 1)?;
                    }
                    _ => {}
                }
            }
            for edge in &workflow.edges {
                if let Some(Backedge {
                    max_traversals_per_run,
                }) = edge.backedge
                {
                    backedge_count = checked_add(backedge_count, 1)?;
                    max_backedge = max_backedge.max(max_traversals_per_run);
                }
            }
        }
        let depth = task_drafts
            .iter()
            .map(|draft| usize_u64(draft.key.path.len()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .max()
            .unwrap_or(0);
        let mut state_bytes = checked_add(ceil_div_8(nodes)?, instances)?;
        state_bytes = checked_add(state_bytes, checked_mul(wait_count, 8)?)?;
        state_bytes = checked_add(state_bytes, checked_mul(backedge_count, 8)?)?;
        state_bytes = checked_add(state_bytes, join_token_bytes)?;
        state_bytes = checked_add(state_bytes, ceil_div_8(pending)?)?;
        let mut staging_bytes = state_bytes;
        let task_claims = claims
            .values()
            .filter(|claim| claim.task_handle == task.task_handle)
            .collect::<Vec<_>>();
        for claim in &task_claims {
            state_bytes = checked_add(state_bytes, claim.committed_state_bytes)?;
            staging_bytes = checked_add(staging_bytes, claim.staging_state_bytes)?;
        }
        let mut trace_over = false;
        let mut watch_fragments = 0_u64;
        for watch in &task.watches {
            let fragments = trace_fragments(watch.encoded_bytes);
            if fragments == 0 || fragments > u64::from(u16::MAX) {
                trace_over = true;
            }
            watch_fragments = trace_add(watch_fragments, fragments, &mut trace_over);
        }
        let claim_trace = task_claims.iter().fold(0_u64, |total, claim| {
            trace_add(total, claim.trace_events_per_release, &mut trace_over)
        });
        let write_events = task_claims.iter().try_fold(0_u64, |total, claim| {
            checked_add(total, usize_u64(claim.writes.len())?)
        })?;
        let mut trace_events = 3_u64;
        for count in [
            instances,
            executable,
            edges,
            fork_activations,
            join_count,
            wait_count,
            cancellation_events,
            checked_mul(subworkflow_count, 2)?,
            write_events,
            watch_fragments,
            completion_requests,
            usize_u64(task.root_workflow_ids.len())?,
            claim_trace,
        ] {
            trace_events = trace_add(trace_events, count, &mut trace_over);
        }
        let proof = TaskWorkflowResourceProof {
            task_handle: task.task_handle,
            root_workflows: usize_u64(task.root_workflow_ids.len())?,
            reachable_workflow_templates: usize_u64(reachable.len())?,
            expanded_workflow_instances: instances,
            expanded_nodes: nodes,
            expanded_edges: edges,
            active_nodes: executable,
            node_executions_per_release: executable,
            fork_nesting_depth: fork_count,
            branches_per_fork: max_branches,
            pending_cancellations: pending,
            subworkflow_expansion_depth: depth,
            backedge_traversals_per_run: max_backedge,
            wait_cycles: max_wait,
            workflow_state_bytes: state_bytes,
            workflow_staging_bytes: staging_bytes,
            watch_handles: usize_u64(task.watches.len())?,
            trace_events_per_release: trace_events,
            trace_ring_capacity: task.trace_ring_capacity,
        };
        let source_over = reachable.iter().any(|id| {
            usize_u64(workflows[id].nodes.len())
                .map_or(true, |value| value > limit.max_source_nodes_per_workflow)
                || usize_u64(workflows[id].edges.len())
                    .map_or(true, |value| value > limit.max_source_edges_per_workflow)
        });
        let resource_over = source_over
            || proof.root_workflows > limit.max_workflows_per_task
            || proof.expanded_workflow_instances > limit.max_expanded_workflow_instances
            || proof.expanded_nodes > limit.max_expanded_nodes_per_task
            || proof.expanded_edges > limit.max_expanded_edges_per_task
            || proof.active_nodes > limit.max_active_nodes_per_task
            || proof.node_executions_per_release > limit.max_node_executions_per_release
            || proof.fork_nesting_depth > limit.max_fork_nesting_depth
            || proof.branches_per_fork > limit.max_branches_per_fork
            || proof.pending_cancellations > limit.max_pending_cancellations
            || proof.subworkflow_expansion_depth > limit.max_subworkflow_expansion_depth
            || proof.backedge_traversals_per_run > limit.max_backedge_traversals_per_run
            || proof.wait_cycles > limit.max_wait_cycles
            || proof.workflow_state_bytes > limit.max_workflow_state_bytes_per_task
            || proof.workflow_staging_bytes > limit.max_workflow_staging_bytes_per_task
            || proof.watch_handles > limit.max_watch_handles_per_task;
        trace_over = trace_over
            || proof.trace_events_per_release > limit.max_trace_events_per_release
            || proof.trace_ring_capacity > limit.workflow_trace_ring_capacity
            || proof.trace_ring_capacity == 0;
        if resource_over {
            diagnostics.push(resource_diagnostic_for_task(task, workflows, sources)?);
        }
        if trace_over {
            diagnostics.push(diagnostic_for_task(
                task,
                workflows,
                sources,
                WorkflowDiagnosticCode::TraceBudgetExceeded,
            )?);
        }
        proofs.push(proof);
    }
    Ok(WorkflowResourceProof {
        target_limits: limits,
        tasks: proofs,
    })
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the audit independently compares every identity used by the generated tables"
)]
fn audit_generation(
    drafts: &[InstanceDraft],
    instances: &[PlannedWorkflowInstance],
    steps: &[WorkflowPlanStep],
    edges: &[PlannedWorkflowEdge],
    instance_handles: &BTreeMap<InstanceKey, WorkflowInstanceHandle>,
    workflow_handles: &BTreeMap<StableId, WorkflowHandle>,
    node_handles: &BTreeMap<StableId, WorkflowNodeHandle>,
    edge_handles: &BTreeMap<StableId, WorkflowEdgeHandle>,
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
) -> Result<(), WorkflowPlanInputError> {
    let expected_instances = drafts
        .iter()
        .map(|draft| {
            (
                instance_handles[&draft.key],
                draft.key.task_handle,
                workflow_handles[&draft.workflow_id],
                draft.key.path.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let actual_instances = instances
        .iter()
        .map(|instance| {
            (
                instance.handle,
                instance.task_handle,
                instance.workflow,
                instance.instance_path.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let expected_steps = drafts
        .iter()
        .flat_map(|draft| {
            draft.step_nodes.iter().map(|node_id| {
                (
                    draft.key.task_handle,
                    instance_handles[&draft.key],
                    node_handles[node_id],
                    draft
                        .child_by_node
                        .get(node_id)
                        .map(|key| instance_handles[key]),
                )
            })
        })
        .collect::<BTreeSet<_>>();
    let actual_steps = steps
        .iter()
        .map(|step| {
            (
                step.task_handle,
                step.instance,
                step.node,
                step.child_instance,
            )
        })
        .collect::<BTreeSet<_>>();
    let step_handles = steps
        .iter()
        .map(|step| ((step.instance, step.node), step.handle))
        .collect::<BTreeMap<_, _>>();
    let expected_edges = drafts
        .iter()
        .flat_map(|draft| {
            workflows[&draft.workflow_id].edges.iter().map(|edge| {
                let instance = instance_handles[&draft.key];
                (
                    instance,
                    edge_handles[&edge.edge_id],
                    step_handles
                        .get(&(instance, node_handles[&edge.source_node_id]))
                        .copied(),
                )
            })
        })
        .collect::<BTreeSet<_>>();
    let actual_edges = edges
        .iter()
        .map(|edge| (edge.instance, edge.edge, edge.source_step))
        .collect::<BTreeSet<_>>();
    let dense_instances = instances
        .iter()
        .enumerate()
        .all(|(index, value)| u32::try_from(index) == Ok(value.handle.0));
    let dense_steps = steps
        .iter()
        .enumerate()
        .all(|(index, value)| u32::try_from(index) == Ok(value.handle.0));
    let dense_edges = edges
        .iter()
        .enumerate()
        .all(|(index, value)| u32::try_from(index) == Ok(value.handle.0));
    let mut task_orders = BTreeMap::<u32, Vec<u32>>::new();
    for step in steps {
        task_orders
            .entry(step.task_handle)
            .or_default()
            .push(step.task_execution_order);
    }
    let dense_task_orders = task_orders.values_mut().all(|orders| {
        orders.sort_unstable();
        orders
            .iter()
            .enumerate()
            .all(|(index, order)| u32::try_from(index) == Ok(*order))
    });
    if instances.len() != expected_instances.len()
        || actual_instances != expected_instances
        || steps.len() != expected_steps.len()
        || actual_steps != expected_steps
        || edges.len() != expected_edges.len()
        || actual_edges != expected_edges
        || !dense_instances
        || !dense_steps
        || !dense_edges
        || !dense_task_orders
    {
        return Err(WorkflowPlanInputError::GenerationAudit);
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one audit compares every generated R2-02/R2-05 table against its source closure"
)]
fn audit_planning_inputs(
    resources: &[PlannedNodeResources],
    watches: &[PlannedWorkflowWatch],
    conditions: &[PlannedConditionBinding],
    claims: &BTreeMap<(InstanceKey, StableId), &ExpandedNodeResourceInput>,
    steps: &BTreeMap<(InstanceKey, StableId), WorkflowStepHandle>,
    tasks: &[&TaskWorkflowPlanningInput],
    condition_inputs: &[WorkflowConditionBindingInput],
    instances: &BTreeMap<InstanceKey, WorkflowInstanceHandle>,
    require_bindings: bool,
) -> Result<(), WorkflowPlanInputError> {
    let expected_resources = claims
        .iter()
        .map(|(key, claim)| {
            let mut writes = claim.writes.clone();
            writes.sort();
            (
                steps[key],
                claim.committed_state_bytes,
                claim.staging_state_bytes,
                claim.trace_events_per_release,
                writes,
                claim.action_binding.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let actual_resources = resources
        .iter()
        .map(|resource| {
            (
                resource.step,
                resource.committed_state_bytes,
                resource.staging_state_bytes,
                resource.trace_events_per_release,
                resource.writes.clone(),
                resource.action_binding.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    let expected_watches = tasks
        .iter()
        .flat_map(|task| {
            task.watches.iter().map(|watch| {
                (
                    task.task_handle,
                    watch.value_id,
                    watch.encoded_bytes,
                    trace_fragments(watch.encoded_bytes),
                )
            })
        })
        .collect::<BTreeSet<_>>();
    let actual_watches = watches
        .iter()
        .map(|watch| {
            (
                watch.task_handle,
                watch.value_id,
                watch.encoded_bytes,
                u64::from(watch.fragment_count),
            )
        })
        .collect::<BTreeSet<_>>();
    let dense_watches = watches
        .iter()
        .enumerate()
        .all(|(index, watch)| u32::try_from(index) == Ok(watch.handle.0));
    let expected_conditions = if require_bindings {
        condition_inputs
            .iter()
            .map(|binding| {
                let key = InstanceKey {
                    task_handle: binding.task_handle,
                    path: binding.instance_path.clone(),
                };
                (
                    binding.task_handle,
                    instances.get(&key).copied(),
                    binding.condition_id,
                    binding.source,
                )
            })
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    let actual_conditions = conditions
        .iter()
        .map(|binding| {
            (
                binding.task_handle,
                Some(binding.instance),
                binding.condition_id,
                binding.source,
            )
        })
        .collect::<BTreeSet<_>>();
    let dense_conditions = conditions
        .iter()
        .enumerate()
        .all(|(index, binding)| u32::try_from(index) == Ok(binding.handle.0));
    if resources.len() != expected_resources.len()
        || actual_resources != expected_resources
        || watches.len() != expected_watches.len()
        || actual_watches != expected_watches
        || !dense_watches
        || conditions.len() != expected_conditions.len()
        || actual_conditions != expected_conditions
        || !dense_conditions
    {
        return Err(WorkflowPlanInputError::GenerationAudit);
    }
    Ok(())
}

fn source_digests(
    reachable: &BTreeSet<StableId>,
    workflows: &[WorkflowDocument],
    sources: &[WorkflowSource<'_>],
) -> Result<Vec<WorkflowSourceDigest>, WorkflowPlanInputError> {
    let reachable_paths = workflows
        .iter()
        .filter(|workflow| reachable.contains(&workflow.workflow_id))
        .map(|workflow| workflow.source_path.as_str())
        .collect::<BTreeSet<_>>();
    let mut result = sources
        .iter()
        .filter(|source| reachable_paths.contains(source.source_path))
        .map(|source| WorkflowSourceDigest {
            source_path: source.source_path.to_owned(),
            source_digest: sha256_prefixed(source.source_bytes),
        })
        .collect::<Vec<_>>();
    result.sort_by(|left, right| {
        left.source_path
            .as_bytes()
            .cmp(right.source_path.as_bytes())
    });
    if result.len() != reachable.len() {
        return Err(WorkflowPlanInputError::GenerationAudit);
    }
    Ok(result)
}

fn find_expanded_node<'a>(
    key: &InstanceKey,
    node_id: StableId,
    workflows: &'a BTreeMap<StableId, &'a WorkflowDocument>,
) -> Result<(&'a WorkflowDocument, &'a Node), WorkflowPlanInputError> {
    let workflow_id = key
        .path
        .last()
        .copied()
        .and_then(|last| workflows.contains_key(&last).then_some(last))
        .or_else(|| {
            workflows.values().find_map(|workflow| {
                workflow
                    .nodes
                    .iter()
                    .any(|node| node.node_id == node_id)
                    .then_some(workflow.workflow_id)
            })
        })
        .ok_or(WorkflowPlanInputError::GenerationAudit)?;
    let workflow = workflows[&workflow_id];
    let node = workflow
        .nodes
        .iter()
        .find(|node| node.node_id == node_id)
        .ok_or(WorkflowPlanInputError::GenerationAudit)?;
    Ok((workflow, node))
}

fn document_diagnostic(
    workflow: &WorkflowDocument,
    sources: &BTreeMap<&str, &str>,
    code: WorkflowDiagnosticCode,
    span: crate::SourceSpan,
    related_id: Option<StableId>,
) -> Result<WorkflowDiagnostic, WorkflowPlanInputError> {
    let source = sources
        .get(workflow.source_path.as_str())
        .copied()
        .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?;
    Ok(make_diagnostic(
        &workflow.source_path,
        source,
        code,
        span,
        "",
        related_id,
    ))
}

fn resource_diagnostic_for_task(
    task: &TaskWorkflowPlanningInput,
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
    sources: &BTreeMap<&str, &str>,
) -> Result<WorkflowDiagnostic, WorkflowPlanInputError> {
    diagnostic_for_task(
        task,
        workflows,
        sources,
        WorkflowDiagnosticCode::ResourceBudgetExceeded,
    )
}

fn diagnostic_for_task(
    task: &TaskWorkflowPlanningInput,
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
    sources: &BTreeMap<&str, &str>,
    code: WorkflowDiagnosticCode,
) -> Result<WorkflowDiagnostic, WorkflowPlanInputError> {
    let root = task
        .root_workflow_ids
        .iter()
        .min_by_key(|id| id.network_bytes())
        .and_then(|id| workflows.get(id).copied())
        .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?;
    document_diagnostic(root, sources, code, root.span, Some(root.workflow_id))
}

fn resource_diagnostic_for_first_task(
    tasks: &[&TaskWorkflowPlanningInput],
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
    sources: &BTreeMap<&str, &str>,
) -> Result<WorkflowDiagnostic, WorkflowPlanInputError> {
    let task = tasks
        .first()
        .copied()
        .ok_or(WorkflowPlanInputError::EmptyTaskSet)?;
    resource_diagnostic_for_task(task, workflows, sources)
}

fn dense(index: usize) -> Result<u32, WorkflowPlanInputError> {
    let value = u32::try_from(index).map_err(|_| WorkflowPlanInputError::ArithmeticOverflow)?;
    if value == u32::MAX {
        Err(WorkflowPlanInputError::ArithmeticOverflow)
    } else {
        Ok(value)
    }
}

fn usize_u64(value: usize) -> Result<u64, WorkflowPlanInputError> {
    u64::try_from(value).map_err(|_| WorkflowPlanInputError::ArithmeticOverflow)
}

fn checked_sum(mut values: impl Iterator<Item = usize>) -> Result<u64, WorkflowPlanInputError> {
    values.try_fold(0_u64, |total, value| checked_add(total, usize_u64(value)?))
}

fn checked_add(left: u64, right: u64) -> Result<u64, WorkflowPlanInputError> {
    left.checked_add(right)
        .ok_or(WorkflowPlanInputError::ArithmeticOverflow)
}

fn checked_mul(left: u64, right: u64) -> Result<u64, WorkflowPlanInputError> {
    left.checked_mul(right)
        .ok_or(WorkflowPlanInputError::ArithmeticOverflow)
}

fn ceil_div_8(value: u64) -> Result<u64, WorkflowPlanInputError> {
    value
        .checked_add(7)
        .map(|sum| sum / 8)
        .ok_or(WorkflowPlanInputError::ArithmeticOverflow)
}

const fn trace_fragments(encoded_bytes: u64) -> u64 {
    if encoded_bytes == 0 {
        0
    } else {
        1 + (encoded_bytes - 1) / 32
    }
}

fn trace_add(left: u64, right: u64, overflowed: &mut bool) -> u64 {
    if let Some(value) = left.checked_add(right) {
        value
    } else {
        *overflowed = true;
        u64::MAX
    }
}

fn sha256_prefixed(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(71);
    encoded.push_str("sha256:");
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde serialize_with requires a shared reference to the field"
)]
pub(crate) fn serialize_u64_decimal<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.collect_str(value)
}

#[allow(
    clippy::ref_option,
    reason = "serde serialize_with requires the exact shared-reference field signature"
)]
fn serialize_optional_u64_decimal<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(value) => serializer.serialize_some(&DecimalU64(*value)),
        None => serializer.serialize_none(),
    }
}

struct DecimalU64(u64);

impl Serialize for DecimalU64 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_u64_decimal(&self.0, serializer)
    }
}

struct BoundedArtifactWriter {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}

impl Write for BoundedArtifactWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        if buffer.len() > remaining {
            self.exceeded = true;
            return Err(io::Error::other("Workflow artifact byte limit exceeded"));
        }
        self.bytes
            .try_reserve_exact(buffer.len())
            .map_err(|error| io::Error::other(error.to_string()))?;
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialize_bounded<T: Serialize>(
    value: &T,
    limit: usize,
) -> Result<Option<Vec<u8>>, WorkflowPlanInputError> {
    let mut writer = BoundedArtifactWriter {
        bytes: Vec::new(),
        limit,
        exceeded: false,
    };
    let result = serde_jcs::to_writer(&mut writer, value);
    if writer.exceeded {
        return Ok(None);
    }
    result.map_err(|error| WorkflowPlanInputError::Serialization(error.to_string()))?;
    Ok(Some(writer.bytes))
}
