use serde::Serialize;
use thiserror::Error;

use crate::model::StableId;

/// Half-open UTF-8 byte range used by Workflow diagnostics and authoring entities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SourceSpan {
    /// Inclusive UTF-8 byte offset.
    pub start: u32,
    /// Exclusive UTF-8 byte offset.
    pub end: u32,
}

impl SourceSpan {
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) fn from_usize(start: usize, end: usize) -> Self {
        // YAML 输入在构造 span 前受 max_source_bytes <= u32::MAX 的硬限制。
        Self {
            start: start as u32,
            end: end as u32,
        }
    }

    pub(crate) const fn empty() -> Self {
        Self { start: 0, end: 0 }
    }
}

/// One-based Unicode-scalar source position plus its stable byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SourcePosition {
    /// Zero-based UTF-8 byte offset used for deterministic ordering.
    pub byte_offset: u32,
    /// One-based source line.
    pub line: u32,
    /// One-based Unicode-scalar column.
    pub column: u32,
}

/// Stable locale-neutral diagnostic codes emitted by Workflow validation and planning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum WorkflowDiagnosticCode {
    /// `WF0001`: input is not BOM-free UTF-8.
    #[serde(rename = "WF0001")]
    InvalidEncoding,
    /// `WF0002`: Graph/Layout version is not exact Preview 1.0.
    #[serde(rename = "WF0002")]
    UnsupportedSchemaVersion,
    /// `WF0003`: YAML syntax, Core scalar, or document count is invalid.
    #[serde(rename = "WF0003")]
    InvalidYaml,
    /// `WF0004`: a mapping key is repeated.
    #[serde(rename = "WF0004")]
    DuplicateMappingKey,
    /// `WF0005`: a custom, non-Core, or merge tag/key is present.
    #[serde(rename = "WF0005")]
    UnsupportedYamlTag,
    /// `WF0006`: an alias reaches its own open anchor.
    #[serde(rename = "WF0006")]
    AliasCycle,
    /// `WF0007`: a YAML byte, depth, alias, expansion, or scalar limit is exceeded.
    #[serde(rename = "WF0007")]
    SourceLimitExceeded,
    /// `WF0008`: a field is not defined by Preview 1.0.
    #[serde(rename = "WF0008")]
    UnknownField,
    /// `WF0009`: a required field, type, range, or field combination is invalid.
    #[serde(rename = "WF0009")]
    InvalidField,
    /// `WF0010`: an identity is not a canonical lowercase `UUIDv7`.
    #[serde(rename = "WF0010")]
    InvalidStableIdentity,
    /// `WF0011`: an identity is repeated in the project closure.
    #[serde(rename = "WF0011")]
    DuplicateStableIdentity,
    /// `WF0012`: a canonical name is invalid or repeated in its scope.
    #[serde(rename = "WF0012")]
    InvalidCanonicalName,
    /// `WF1001`: no Entry node exists.
    #[serde(rename = "WF1001")]
    MissingEntry,
    /// `WF1002`: a second or later Entry node exists.
    #[serde(rename = "WF1002")]
    MultipleEntries,
    /// `WF1003`: Entry control degree is invalid.
    #[serde(rename = "WF1003")]
    InvalidEntryEdge,
    /// `WF1004`: End control degree is invalid.
    #[serde(rename = "WF1004")]
    InvalidEndEdge,
    /// `WF1005`: an edge source or target does not exist.
    #[serde(rename = "WF1005")]
    DanglingEdge,
    /// `WF1006`: a node has an invalid control degree.
    #[serde(rename = "WF1006")]
    InvalidControlDegree,
    /// `WF1007`: a non-Join node has multiple control inputs.
    #[serde(rename = "WF1007")]
    ImplicitMerge,
    /// `WF1008`: execution order is missing, misplaced, repeated, or non-dense.
    #[serde(rename = "WF1008")]
    InvalidExecutionOrder,
    /// `WF1009`: a forward dependency contradicts execution order.
    #[serde(rename = "WF1009")]
    InvalidForwardDependency,
    /// `WF1010`: removing declared backedges does not produce a DAG, or a backedge is spurious.
    #[serde(rename = "WF1010")]
    UnmarkedCycleEdge,
    /// `WF1011`: a backedge marker and traversal bound disagree.
    #[serde(rename = "WF1011")]
    UnboundedBackedge,
    /// `WF1012`: a node is not reachable from Entry.
    #[serde(rename = "WF1012")]
    UnreachableNode,
    /// `WF1013`: a finite Workflow has no reachable End.
    #[serde(rename = "WF1013")]
    MissingCompletionPath,
    /// `WF1014`: a later edge repeats the same source, target, and kind.
    #[serde(rename = "WF1014")]
    DuplicateEdge,
    /// `WF2002`: Decision priority is missing, repeated, or non-dense.
    #[serde(rename = "WF2002")]
    InvalidDecisionPriority,
    /// `WF2003`: Fork branch order is missing, repeated, or non-dense.
    #[serde(rename = "WF2003")]
    InvalidBranchOrder,
    /// `WF2004`: a Join mode has an invalid Fork pairing.
    #[serde(rename = "WF2004")]
    InvalidForkJoinPair,
    /// `WF2005`: parallel or mutually exclusive regions cross, escape, or admit foreign tokens.
    #[serde(rename = "WF2005")]
    CrossRegionJoin,
    /// `WF2006`: Join mode or loser policy is invalid.
    #[serde(rename = "WF2006")]
    InvalidJoinMode,
    /// `WF2009`: Wait mode fields are inconsistent.
    #[serde(rename = "WF2009")]
    InvalidWaitPolicy,
    /// `WF2010`: a Wait cycle value is zero or not representable.
    #[serde(rename = "WF2010")]
    InvalidWaitRange,
    /// `WF2011`: the compile-time subworkflow call graph is recursive.
    #[serde(rename = "WF2011")]
    RecursiveSubworkflow,
    /// `WF2012`: a referenced subworkflow is absent from the project closure.
    #[serde(rename = "WF2012")]
    MissingSubworkflow,
    /// `WF3001`: two expanded nodes statically write overlapping storage.
    #[serde(rename = "WF3001")]
    WriteConflict,
    /// `WF3005`: a graph/project capacity exceeds a caller-supplied limit.
    #[serde(rename = "WF3005")]
    ResourceBudgetExceeded,
    /// `WF3007`: a Trace event, fragment, or ring capacity exceeds its bound.
    #[serde(rename = "WF3007")]
    TraceBudgetExceeded,
    /// `WF3010`: a Layout references a semantic entity outside its Workflow.
    #[serde(rename = "WF3010")]
    InvalidLayoutReference,
    /// `WF3011`: a Layout collection exceeds a host-only limit.
    #[serde(rename = "WF3011")]
    LayoutLimitExceeded,
}

impl WorkflowDiagnosticCode {
    /// Returns the frozen code used for ordering and external catalogs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEncoding => "WF0001",
            Self::UnsupportedSchemaVersion => "WF0002",
            Self::InvalidYaml => "WF0003",
            Self::DuplicateMappingKey => "WF0004",
            Self::UnsupportedYamlTag => "WF0005",
            Self::AliasCycle => "WF0006",
            Self::SourceLimitExceeded => "WF0007",
            Self::UnknownField => "WF0008",
            Self::InvalidField => "WF0009",
            Self::InvalidStableIdentity => "WF0010",
            Self::DuplicateStableIdentity => "WF0011",
            Self::InvalidCanonicalName => "WF0012",
            Self::MissingEntry => "WF1001",
            Self::MultipleEntries => "WF1002",
            Self::InvalidEntryEdge => "WF1003",
            Self::InvalidEndEdge => "WF1004",
            Self::DanglingEdge => "WF1005",
            Self::InvalidControlDegree => "WF1006",
            Self::ImplicitMerge => "WF1007",
            Self::InvalidExecutionOrder => "WF1008",
            Self::InvalidForwardDependency => "WF1009",
            Self::UnmarkedCycleEdge => "WF1010",
            Self::UnboundedBackedge => "WF1011",
            Self::UnreachableNode => "WF1012",
            Self::MissingCompletionPath => "WF1013",
            Self::DuplicateEdge => "WF1014",
            Self::InvalidDecisionPriority => "WF2002",
            Self::InvalidBranchOrder => "WF2003",
            Self::InvalidForkJoinPair => "WF2004",
            Self::CrossRegionJoin => "WF2005",
            Self::InvalidJoinMode => "WF2006",
            Self::InvalidWaitPolicy => "WF2009",
            Self::InvalidWaitRange => "WF2010",
            Self::RecursiveSubworkflow => "WF2011",
            Self::MissingSubworkflow => "WF2012",
            Self::WriteConflict => "WF3001",
            Self::ResourceBudgetExceeded => "WF3005",
            Self::TraceBudgetExceeded => "WF3007",
            Self::InvalidLayoutReference => "WF3010",
            Self::LayoutLimitExceeded => "WF3011",
        }
    }
}

/// One deterministic diagnostic without localized display text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkflowDiagnostic {
    /// Normalized project-relative path supplied by the caller.
    pub source_path: String,
    /// Stable diagnostic code.
    pub code: WorkflowDiagnosticCode,
    /// Stable half-open UTF-8 byte range.
    pub span: SourceSpan,
    /// Human-facing start position.
    pub start: SourcePosition,
    /// Human-facing exclusive end position.
    pub end: SourcePosition,
    /// Stable field location or empty string when no field exists.
    pub pointer: String,
    /// Related semantic identity when one is available.
    pub related_id: Option<StableId>,
}

/// Failure to encode Workflow diagnostics as RFC 8785 canonical JSON.
#[derive(Debug, Error)]
#[error("failed to serialize Workflow diagnostics as canonical JSON: {0}")]
pub struct WorkflowDiagnosticSerializationError(#[from] serde_json::Error);

/// Sorts diagnostics by the frozen order and serializes RFC 8785 canonical JSON.
///
/// # Errors
///
/// Returns [`WorkflowDiagnosticSerializationError`] if serialization fails.
pub fn diagnostics_to_canonical_json(
    diagnostics: &[WorkflowDiagnostic],
) -> Result<Vec<u8>, WorkflowDiagnosticSerializationError> {
    let mut ordered = diagnostics.to_vec();
    sort_diagnostics(&mut ordered);
    serde_jcs::to_vec(&ordered).map_err(WorkflowDiagnosticSerializationError::from)
}

pub(crate) fn make_diagnostic(
    source_path: &str,
    source: &str,
    code: WorkflowDiagnosticCode,
    span: SourceSpan,
    pointer: &str,
    related_id: Option<StableId>,
) -> WorkflowDiagnostic {
    WorkflowDiagnostic {
        source_path: source_path.to_owned(),
        code,
        span,
        start: locate(source, span.start),
        end: locate(source, span.end),
        pointer: pointer.to_owned(),
        related_id,
    }
}

pub(crate) fn sort_diagnostics(diagnostics: &mut [WorkflowDiagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.source_path
            .as_bytes()
            .cmp(right.source_path.as_bytes())
            .then(left.span.cmp(&right.span))
            .then(left.code.as_str().cmp(right.code.as_str()))
            .then_with(|| {
                left.related_id
                    .map(StableId::network_bytes)
                    .cmp(&right.related_id.map(StableId::network_bytes))
            })
            .then(left.pointer.as_bytes().cmp(right.pointer.as_bytes()))
    });
}

fn locate(source: &str, byte_offset: u32) -> SourcePosition {
    let boundary = usize::try_from(byte_offset)
        .ok()
        .map_or(source.len(), |value| value.min(source.len()));
    let prefix = source.get(..boundary).unwrap_or(source);
    let mut line = 1_u32;
    let mut column = 1_u32;
    for value in prefix.chars() {
        if value == '\n' {
            line = line.saturating_add(1);
            column = 1;
        } else {
            column = column.saturating_add(1);
        }
    }
    SourcePosition {
        byte_offset,
        line,
        column,
    }
}
