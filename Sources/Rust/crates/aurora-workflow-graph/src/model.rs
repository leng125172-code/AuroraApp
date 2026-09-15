use std::fmt;

use serde::{Serialize, Serializer};

use crate::SourceSpan;

/// Canonical lowercase RFC 9562 `UUIDv7` stored in network-byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableId([u8; 16]);

impl StableId {
    /// Parses only the canonical lowercase `UUIDv7` spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if value.len() != 36
            || value.as_bytes().get(8) != Some(&b'-')
            || value.as_bytes().get(13) != Some(&b'-')
            || value.as_bytes().get(18) != Some(&b'-')
            || value.as_bytes().get(23) != Some(&b'-')
            || value.as_bytes().get(14) != Some(&b'7')
            || !matches!(value.as_bytes().get(19), Some(b'8' | b'9' | b'a' | b'b'))
        {
            return None;
        }
        let mut bytes = [0_u8; 16];
        let mut output = 0_usize;
        let mut high = None;
        for byte in value.bytes() {
            if byte == b'-' {
                continue;
            }
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => return None,
            };
            if let Some(value) = high.take() {
                let slot = bytes.get_mut(output)?;
                *slot = (value << 4) | digit;
                output += 1;
            } else {
                high = Some(digit);
            }
        }
        (output == bytes.len() && high.is_none()).then_some(Self(bytes))
    }

    /// Returns network-order bytes used for locale-independent ordering.
    #[must_use]
    pub const fn network_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Display for StableId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                formatter.write_str("-")?;
            }
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for StableId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

/// Join behavior selected by one explicit Join node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinMode {
    /// Non-parallel, statically exclusive control-flow merge.
    Merge,
    /// Wait for every branch of the paired Fork.
    JoinAll,
    /// Continue after the first branch of the paired Fork arrives.
    JoinAny,
}

/// Loser handling selected by a `JoinAny` node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinPolicy {
    /// Clear future loser activity at the successful commit boundary.
    CancelOthers,
    /// Allow loser branches to run to natural quiescence.
    KeepRunning,
    /// Stop losers only after their declared cancellation boundary.
    WaitAtBoundary,
}

/// Validated Wait behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitMode {
    /// Wait for the exact number of task releases.
    Cycles {
        /// Non-zero release count.
        wait_cycles: u64,
    },
    /// Evaluate one stable condition each active scan.
    Condition {
        /// Stable condition binding identity; its typed binding is frozen by R2-05.
        condition_id: StableId,
        /// Finite timeout, or `None` only for an explicitly permanent wait.
        timeout_cycles: Option<u64>,
        /// Whether the wait is explicitly permanent.
        permanent: bool,
    },
}

/// Kind-specific data for one validated source node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// Structural entry marker.
    Entry,
    /// One cyclic Action whose typed binding is supplied separately in R2-05.
    Action,
    /// Priority-ordered conditional branch.
    Decision,
    /// Ordered logical parallel split.
    Fork,
    /// Explicit merge or paired Fork join.
    Join {
        /// Merge behavior.
        mode: JoinMode,
        /// Required for JoinAll/JoinAny and absent for Merge.
        fork_id: Option<StableId>,
        /// Required only for `JoinAny`.
        loser_policy: Option<JoinPolicy>,
    },
    /// Release-counted or condition-counted wait.
    Wait(WaitMode),
    /// Compile-time-expanded subworkflow call.
    Subworkflow {
        /// Referenced Workflow identity.
        target_workflow_id: StableId,
    },
    /// Structural completion marker.
    End,
}

/// One validated Workflow node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Stable node identity.
    pub node_id: StableId,
    /// Scope-local ASCII canonical name.
    pub canonical_name: String,
    /// Dense order for executable nodes; absent for Entry/End.
    pub execution_order: Option<u32>,
    /// Whether this executable node is an explicit cancellation boundary.
    pub cancellation_boundary: bool,
    /// Kind-specific data.
    pub kind: NodeKind,
    /// Complete author-source span.
    pub span: SourceSpan,
}

/// Explicit traversal bound for a marked backedge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backedge {
    /// Maximum successful traversals in one expanded Workflow run.
    pub max_traversals_per_run: u64,
}

/// One validated control edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// Stable edge identity.
    pub edge_id: StableId,
    /// Source node identity.
    pub source_node_id: StableId,
    /// Target node identity.
    pub target_node_id: StableId,
    /// Stable condition binding for a guarded edge.
    pub condition_id: Option<StableId>,
    /// Dense Decision priority when the source is a Decision.
    pub priority: Option<u32>,
    /// Dense Fork branch order when the source is a Fork.
    pub branch_order: Option<u32>,
    /// Explicit traversal bound when this is a backedge.
    pub backedge: Option<Backedge>,
    /// Complete author-source span.
    pub span: SourceSpan,
}

/// One validated semantic Workflow author document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDocument {
    /// Normalized source path supplied by the caller.
    pub source_path: String,
    /// Stable document identity.
    pub document_id: StableId,
    /// Stable Workflow template identity.
    pub workflow_id: StableId,
    /// Project-scope canonical name.
    pub canonical_name: String,
    /// Whether no finite completion path is required.
    pub permanent: bool,
    /// Source-order nodes; execution uses explicit `execution_order`.
    pub nodes: Vec<Node>,
    /// Source-order control edges.
    pub edges: Vec<Edge>,
    /// Complete author-source span.
    pub span: SourceSpan,
}

/// One signed canvas position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutPoint {
    /// Horizontal signed canvas units.
    pub x_canvas_units: i32,
    /// Vertical signed canvas units.
    pub y_canvas_units: i32,
}

/// Layout data for one semantic node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutNode {
    /// Referenced semantic Node identity.
    pub node_id: StableId,
    /// Top-left position.
    pub position: LayoutPoint,
    /// Positive width in canvas units.
    pub width_canvas_units: u32,
    /// Positive height in canvas units.
    pub height_canvas_units: u32,
    /// Complete author-source span.
    pub span: SourceSpan,
}

/// Layout route for one semantic edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutEdge {
    /// Referenced semantic Edge identity.
    pub edge_id: StableId,
    /// Ordered routing points.
    pub points: Vec<LayoutPoint>,
    /// Complete author-source span.
    pub span: SourceSpan,
}

/// Host-only visual group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutGroup {
    /// Stable layout-only group identity.
    pub group_id: StableId,
    /// Layout-scope canonical name.
    pub canonical_name: String,
    /// Semantic nodes displayed in this group.
    pub node_ids: Vec<StableId>,
    /// Top-left position.
    pub position: LayoutPoint,
    /// Positive width in canvas units.
    pub width_canvas_units: u32,
    /// Positive height in canvas units.
    pub height_canvas_units: u32,
    /// Optional human annotation with no control meaning.
    pub annotation: Option<String>,
    /// Complete author-source span.
    pub span: SourceSpan,
}

/// One validated host-only Workflow Layout document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowLayoutDocument {
    /// Normalized source path supplied by the caller.
    pub source_path: String,
    /// Stable Layout document identity.
    pub document_id: StableId,
    /// Referenced semantic Workflow identity.
    pub workflow_id: StableId,
    /// Source-order node layout entries.
    pub nodes: Vec<LayoutNode>,
    /// Source-order edge route entries.
    pub edges: Vec<LayoutEdge>,
    /// Source-order visual groups.
    pub groups: Vec<LayoutGroup>,
    /// Complete author-source span.
    pub span: SourceSpan,
}
