//! Host-only Canonical Workflow IR lowering and static resource planning for R2-02.
//!
//! The pass consumes explicit task roots. It never infers roots from unreferenced documents and
//! never unrolls branches, waits, or backedges. A subworkflow creates exactly one isolated
//! instance per call site. Runtime execution remains the responsibility of R2-03 and later work.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, Write};
use std::str;

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::diagnostic::{make_diagnostic, sort_diagnostics};
use crate::{
    Backedge, Edge, JoinMode, JoinPolicy, Node, NodeKind, StableId, WaitMode, WorkflowDiagnostic,
    WorkflowDiagnosticCode, WorkflowDocument, WorkflowProjectInput, WorkflowSource,
    WorkflowValidationLimits, validate_project,
};

/// Canonical Workflow IR writer major version.
pub const CANONICAL_WORKFLOW_IR_MAJOR: u16 = 1;
/// Canonical Workflow IR writer minor version.
pub const CANONICAL_WORKFLOW_IR_MINOR: u16 = 0;
/// Static Workflow plan writer major version.
pub const STATIC_WORKFLOW_PLAN_MAJOR: u16 = 1;
/// Static Workflow plan writer minor version.
pub const STATIC_WORKFLOW_PLAN_MINOR: u16 = 0;

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

/// Raw mandatory Target Profile values used to construct [`WorkflowTargetLimits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WorkflowTargetLimitValues {
    /// Maximum explicit root Workflow assignments in one task.
    pub max_workflows_per_task: u64,
    /// Maximum source nodes in one reachable Workflow template.
    pub max_source_nodes_per_workflow: u64,
    /// Maximum source edges in one reachable Workflow template.
    pub max_source_edges_per_workflow: u64,
    /// Maximum root plus call-site-expanded instances in one task.
    pub max_expanded_workflow_instances: u64,
    /// Maximum expanded nodes in one task.
    pub max_expanded_nodes_per_task: u64,
    /// Maximum expanded edges in one task.
    pub max_expanded_edges_per_task: u64,
    /// Maximum simultaneous active-set capacity in one task.
    pub max_active_nodes_per_task: u64,
    /// Maximum node executions in one release.
    pub max_node_executions_per_release: u64,
    /// Maximum conservative Fork nesting proof.
    pub max_fork_nesting_depth: u64,
    /// Maximum outgoing branches from one Fork.
    pub max_branches_per_fork: u64,
    /// Maximum pending cancellation slots.
    pub max_pending_cancellations: u64,
    /// Maximum root-to-leaf subworkflow instance depth.
    pub max_subworkflow_expansion_depth: u64,
    /// Maximum declared traversal count on one backedge.
    pub max_backedge_traversals_per_run: u64,
    /// Maximum declared Wait count or timeout.
    pub max_wait_cycles: u64,
    /// Maximum committed Workflow state bytes in one task.
    pub max_workflow_state_bytes_per_task: u64,
    /// Maximum staging Workflow state bytes in one task.
    pub max_workflow_staging_bytes_per_task: u64,
    /// Maximum compiled watch values in one task.
    pub max_watch_handles_per_task: u64,
    /// Maximum staged Workflow Trace events in one release.
    pub max_trace_events_per_release: u64,
    /// Maximum configured Trace ring slots.
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
    pub offset_bytes: u64,
    /// Non-zero byte width.
    pub size_bytes: u64,
}

/// Per-expanded-node resources supplied without freezing the R2-05 binding payload.
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
        wait_cycles: u64,
    },
    /// Condition Wait.
    WaitCondition {
        /// Stable BOOL condition identity.
        condition_id: StableId,
        /// Finite timeout or absent for permanent Wait.
        #[serde(skip_serializing_if = "Option::is_none")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
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

/// One edge copied exactly once for one expanded instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PlannedWorkflowEdge {
    /// Dense expanded edge handle.
    pub handle: ExpandedEdgeHandle,
    /// Owning expanded instance.
    pub instance: WorkflowInstanceHandle,
    /// Canonical template edge.
    pub edge: WorkflowEdgeHandle,
}

/// Exact actual-versus-limit proof for one task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskWorkflowResourceProof {
    /// Owning task.
    pub task_handle: u32,
    /// Explicit root assignments.
    pub root_workflows: u64,
    /// Distinct templates reachable from those roots.
    pub reachable_workflow_templates: u64,
    /// Expanded root and call-site instances.
    pub expanded_workflow_instances: u64,
    /// Expanded nodes, including Entry/End.
    pub expanded_nodes: u64,
    /// Expanded edges.
    pub expanded_edges: u64,
    /// Conservative active-set capacity.
    pub active_nodes: u64,
    /// Conservative executable-node work per release.
    pub node_executions_per_release: u64,
    /// Conservative Fork nesting upper bound.
    pub fork_nesting_depth: u64,
    /// Largest branch count of a Fork.
    pub branches_per_fork: u64,
    /// Required pending-cancellation slots.
    pub pending_cancellations: u64,
    /// Deepest expanded instance path in Workflow units.
    pub subworkflow_expansion_depth: u64,
    /// Largest declared backedge traversal bound.
    pub backedge_traversals_per_run: u64,
    /// Largest Wait count/timeout.
    pub wait_cycles: u64,
    /// Fixed committed bank bytes.
    pub workflow_state_bytes: u64,
    /// Fixed staging bank bytes.
    pub workflow_staging_bytes: u64,
    /// Fixed watch descriptor count.
    pub watch_handles: u64,
    /// Worst-case staged Trace records per release.
    pub trace_events_per_release: u64,
    /// Actual preallocated Trace ring slots.
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
    let claims = prepare_claims(resource_inputs, &drafts, &workflow_map)?;
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
    let static_plan = StaticWorkflowPlan {
        schema_version: WorkflowArtifactVersion::static_plan(),
        semantic_digest: semantic_digest.clone(),
        instances,
        steps,
        edges: expanded_edges,
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
    if !is_dag(&workflow.nodes, &forward) {
        diagnostics.push(document_diagnostic(
            workflow,
            sources,
            WorkflowDiagnosticCode::UnmarkedCycleEdge,
            workflow.span,
            Some(workflow.workflow_id),
        )?);
    }
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
    Ok(())
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

fn is_dag(nodes: &[Node], edges: &[&Edge]) -> bool {
    let mut indegree = nodes
        .iter()
        .map(|node| (node.node_id, 0_usize))
        .collect::<BTreeMap<_, _>>();
    for edge in edges {
        if let Some(value) = indegree.get_mut(&edge.target_node_id) {
            *value = value.saturating_add(1);
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(id, count)| (*count == 0).then_some(*id))
        .collect::<BTreeSet<_>>();
    let mut visited = 0_usize;
    while let Some(id) = ready.pop_first() {
        visited = visited.saturating_add(1);
        for edge in edges.iter().filter(|edge| edge.source_node_id == id) {
            if let Some(value) = indegree.get_mut(&edge.target_node_id) {
                *value = value.saturating_sub(1);
                if *value == 0 {
                    ready.insert(edge.target_node_id);
                }
            }
        }
    }
    visited == nodes.len()
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
            expanded.push((instance_handles[&draft.key], edge_handles[&edge.edge_id]));
        }
    }
    expanded.sort();
    let edges = expanded
        .into_iter()
        .enumerate()
        .map(|(index, (instance, edge))| {
            Ok(PlannedWorkflowEdge {
                handle: ExpandedEdgeHandle(dense(index)?),
                instance,
                edge,
            })
        })
        .collect::<Result<Vec<_>, WorkflowPlanInputError>>()?;
    Ok((instances, steps, edges, step_by_key))
}

fn prepare_claims<'a>(
    inputs: &'a [ExpandedNodeResourceInput],
    drafts: &[InstanceDraft],
    workflows: &BTreeMap<StableId, &WorkflowDocument>,
) -> Result<BTreeMap<(InstanceKey, StableId), &'a ExpandedNodeResourceInput>, WorkflowPlanInputError>
{
    let mut expected = BTreeSet::new();
    for draft in drafts {
        let workflow = workflows[&draft.workflow_id];
        for node in &workflow.nodes {
            if matches!(node.kind, NodeKind::Action | NodeKind::Subworkflow { .. }) {
                expected.insert((draft.key.clone(), node.node_id));
            }
        }
    }
    let mut actual = BTreeMap::new();
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
        if actual.insert(key, input).is_some() {
            return Err(WorkflowPlanInputError::InvalidResourceClaim);
        }
        for region in &input.writes {
            if region.size_bytes == 0
                || region.offset_bytes.checked_add(region.size_bytes).is_none()
            {
                return Err(WorkflowPlanInputError::InvalidResourceClaim);
            }
        }
    }
    let actual_keys = actual.keys().cloned().collect::<BTreeSet<_>>();
    if actual_keys != expected {
        return Err(WorkflowPlanInputError::InvalidResourceClaim);
    }
    Ok(actual)
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
        let (_, region, _, _) = &regions[index];
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
                    }
                    NodeKind::Join {
                        mode,
                        fork_id: Some(fork_id),
                        loser_policy,
                    } if mode != JoinMode::Merge => {
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
                    }
                    NodeKind::Wait(WaitMode::Cycles { wait_cycles }) => {
                        wait_count = checked_add(wait_count, 1)?;
                        max_wait = max_wait.max(wait_cycles);
                    }
                    NodeKind::Wait(WaitMode::Condition { timeout_cycles, .. }) => {
                        wait_count = checked_add(wait_count, 1)?;
                        max_wait = max_wait.max(timeout_cycles.unwrap_or(0));
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
        let mut watch_fragments = 0_u64;
        for watch in &task.watches {
            let fragments = watch
                .encoded_bytes
                .checked_add(31)
                .ok_or(WorkflowPlanInputError::ArithmeticOverflow)?
                / 32;
            if fragments == 0 || fragments > u64::from(u16::MAX) {
                diagnostics.push(resource_diagnostic_for_task(task, workflows, sources)?);
            }
            watch_fragments = checked_add(watch_fragments, fragments)?;
        }
        let claim_trace = task_claims.iter().try_fold(0_u64, |total, claim| {
            checked_add(total, claim.trace_events_per_release)
        })?;
        let write_events = task_claims.iter().try_fold(0_u64, |total, claim| {
            checked_add(total, usize_u64(claim.writes.len())?)
        })?;
        let trace_events = checked_add(
            4,
            checked_add(
                executable,
                checked_add(
                    edges,
                    checked_add(watch_fragments, checked_add(claim_trace, write_events)?)?,
                )?,
            )?,
        )?;
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
        let over = source_over
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
            || proof.watch_handles > limit.max_watch_handles_per_task
            || proof.trace_events_per_release > limit.max_trace_events_per_release
            || proof.trace_ring_capacity > limit.workflow_trace_ring_capacity
            || proof.trace_ring_capacity == 0;
        if over {
            diagnostics.push(resource_diagnostic_for_task(task, workflows, sources)?);
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
    let expected_edges = drafts
        .iter()
        .flat_map(|draft| {
            workflows[&draft.workflow_id]
                .edges
                .iter()
                .map(|edge| (instance_handles[&draft.key], edge_handles[&edge.edge_id]))
        })
        .collect::<BTreeSet<_>>();
    let actual_edges = edges
        .iter()
        .map(|edge| (edge.instance, edge.edge))
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
    let root = task
        .root_workflow_ids
        .iter()
        .min_by_key(|id| id.network_bytes())
        .and_then(|id| workflows.get(id).copied())
        .ok_or(WorkflowPlanInputError::InvalidValidatedInput)?;
    document_diagnostic(
        root,
        sources,
        WorkflowDiagnosticCode::ResourceBudgetExceeded,
        root.span,
        Some(root.workflow_id),
    )
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
