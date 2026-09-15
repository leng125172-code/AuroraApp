//! Versioned Aurora Cyclic Workflow authoring contracts.
//!
//! YAML parsing and Graph/Layout validation are host-only compiler work. Every allocation is
//! bounded by caller-supplied limits, diagnostics are locale-neutral, and an invalid semantic
//! graph never publishes a partial model. This crate does not participate in the cyclic runtime.

mod diagnostic;
mod limits;
mod model;
mod validator;
mod yaml;

pub use diagnostic::{
    SourcePosition, SourceSpan, WorkflowDiagnostic, WorkflowDiagnosticCode,
    WorkflowDiagnosticSerializationError, diagnostics_to_canonical_json,
};
pub use limits::{WorkflowLimitError, WorkflowValidationLimits, YamlSourceLimits};
pub use model::{
    Backedge, Edge, JoinMode, JoinPolicy, LayoutEdge, LayoutGroup, LayoutNode, LayoutPoint, Node,
    NodeKind, StableId, WaitMode, WorkflowDocument, WorkflowLayoutDocument,
};
pub use validator::{
    LayoutSource, WorkflowProjectInput, WorkflowSource, WorkflowValidationOutput, validate_project,
};

/// Supported Workflow Graph Schema major version.
pub const WORKFLOW_SCHEMA_MAJOR: u32 = 1;
/// Supported Workflow Graph Schema minor version.
pub const WORKFLOW_SCHEMA_MINOR: u32 = 0;
