//! Versioned Aurora Cyclic Workflow authoring contracts.
//!
//! YAML parsing and Graph/Layout validation are host-only compiler work. Every allocation is
//! bounded by caller-supplied limits, diagnostics are locale-neutral, and an invalid semantic
//! graph never publishes a partial model. This crate does not participate in the cyclic runtime.

mod binding;
mod diagnostic;
mod limits;
mod model;
mod planning;
mod validator;
mod yaml;

pub use binding::{
    ExpandedActionBindingInput, PlannedConditionBinding, WORKFLOW_BINDING_MAJOR,
    WORKFLOW_BINDING_MINOR, WorkflowActionKind, WorkflowActionPortBinding, WorkflowBindingVersion,
    WorkflowConditionBindingInput, WorkflowConditionHandle, WorkflowPortDirection,
    WorkflowValueArea, WorkflowValueSlot, WorkflowValueType,
};
pub use diagnostic::{
    SourcePosition, SourceSpan, WorkflowDiagnostic, WorkflowDiagnosticCode,
    WorkflowDiagnosticSerializationError, diagnostics_to_canonical_json,
};
pub use limits::{WorkflowLimitError, WorkflowValidationLimits, YamlSourceLimits};
pub use model::{
    Backedge, Edge, JoinMode, JoinPolicy, LayoutEdge, LayoutGroup, LayoutNode, LayoutPoint, Node,
    NodeKind, StableId, WaitMode, WorkflowDocument, WorkflowLayoutDocument,
};
pub use planning::{
    CANONICAL_WORKFLOW_IR_MAJOR, CANONICAL_WORKFLOW_IR_MINOR, CanonicalJoinMode,
    CanonicalJoinPolicy, CanonicalWorkflowEdge, CanonicalWorkflowIr, CanonicalWorkflowNode,
    CanonicalWorkflowNodeKind, CanonicalWorkflowTemplate, ExpandedEdgeHandle,
    ExpandedNodeResourceInput, PlannedNodeResources, PlannedWorkflowEdge, PlannedWorkflowInstance,
    PlannedWorkflowWatch, STATIC_WORKFLOW_PLAN_MAJOR, STATIC_WORKFLOW_PLAN_MINOR,
    StaticWorkflowPlan, TaskWorkflowPlanningInput, TaskWorkflowResourceProof,
    WorkflowArtifactLimits, WorkflowArtifactVersion, WorkflowEdgeHandle, WorkflowHandle,
    WorkflowInstanceHandle, WorkflowNodeHandle, WorkflowPlanArtifacts, WorkflowPlanInputError,
    WorkflowPlanOutput, WorkflowPlanStep, WorkflowPlanningLimitError, WorkflowResourceProof,
    WorkflowSourceDigest, WorkflowStepHandle, WorkflowTargetLimitValues, WorkflowTargetLimits,
    WorkflowWatchHandle, WorkflowWatchInput, WorkflowWriteRegion, compile_bound_workflow_plan,
    compile_static_workflow_plan,
};
pub use validator::{
    LayoutSource, WorkflowProjectInput, WorkflowSource, WorkflowValidationOutput, validate_project,
};

/// Supported Workflow Graph Schema major version.
pub const WORKFLOW_SCHEMA_MAJOR: u32 = 1;
/// Supported Workflow Graph Schema minor version.
pub const WORKFLOW_SCHEMA_MINOR: u32 = 0;
