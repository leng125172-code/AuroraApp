//! Deterministic AOT checkpoint insertion plan for R1-06.

use std::collections::BTreeMap;

use serde::Serialize;
use thiserror::Error;

use crate::{
    AstNodeKind, CanonicalNode, CanonicalNodeId, CanonicalPouKind, CanonicalSourceMap,
    CanonicalStIr, NodeSourceEntry, SemanticSymbol, SemanticSymbolKind, SemanticType, SourceFileId,
    SourceSpan, SymbolId, TaskHandle,
};

/// Major version of the compiler-internal checkpoint plan.
pub const CHECKPOINT_PLAN_MAJOR: u16 = 1;
/// Minor version of the compiler-internal checkpoint plan.
pub const CHECKPOINT_PLAN_MINOR: u16 = 0;

/// Version carried by every serialized checkpoint plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CheckpointPlanVersion {
    /// Reader-incompatible version.
    pub major: u16,
    /// Backward-compatible additive version.
    pub minor: u16,
}

impl CheckpointPlanVersion {
    /// Returns the exact version emitted by this crate.
    #[must_use]
    pub const fn preview_v1_0() -> Self {
        Self {
            major: CHECKPOINT_PLAN_MAJOR,
            minor: CHECKPOINT_PLAN_MINOR,
        }
    }
}

/// Mandatory finite capacities for checkpoint-plan construction and publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointPlanLimits {
    per_pou: usize,
    total: usize,
    encoded_bytes: usize,
}

impl CheckpointPlanLimits {
    /// Validates every checkpoint-plan capacity.
    ///
    /// # Errors
    ///
    /// Returns [`CheckpointPlanLimitError`] when any capacity is zero.
    pub const fn new(
        max_sites_per_pou: usize,
        max_total_sites: usize,
        max_encoded_bytes: usize,
    ) -> Result<Self, CheckpointPlanLimitError> {
        if max_sites_per_pou == 0 {
            return Err(CheckpointPlanLimitError::ZeroSitesPerPou);
        }
        if max_total_sites == 0 {
            return Err(CheckpointPlanLimitError::ZeroTotalSites);
        }
        if max_encoded_bytes == 0 {
            return Err(CheckpointPlanLimitError::ZeroEncodedBytes);
        }
        Ok(Self {
            per_pou: max_sites_per_pou,
            total: max_total_sites,
            encoded_bytes: max_encoded_bytes,
        })
    }

    /// Maximum static checkpoint insertion sites in one POU body.
    #[must_use]
    pub const fn max_sites_per_pou(self) -> usize {
        self.per_pou
    }

    /// Maximum static checkpoint sites across all POU bodies and Task returns.
    #[must_use]
    pub const fn max_total_sites(self) -> usize {
        self.total
    }

    /// Maximum RFC 8785 JSON bytes published for one checkpoint plan.
    #[must_use]
    pub const fn max_encoded_bytes(self) -> usize {
        self.encoded_bytes
    }
}

/// Invalid zero-valued checkpoint-plan capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CheckpointPlanLimitError {
    /// Per-POU site capacity is zero.
    #[error("max_sites_per_pou must be non-zero")]
    ZeroSitesPerPou,
    /// Total site capacity is zero.
    #[error("max_total_sites must be non-zero")]
    ZeroTotalSites,
    /// Encoded-byte capacity is zero.
    #[error("max_encoded_bytes must be non-zero")]
    ZeroEncodedBytes,
}

/// Stable dense checkpoint-site identity. `u32::MAX` is never assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CheckpointSiteId(pub u32);

/// Exact static reason and binding for one mandatory execution checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum CheckpointSiteKind {
    /// Runtime must check once after an actual Task Program invocation returns.
    TaskReturn {
        /// Scheduler Task instance that owns this return boundary.
        task: TaskHandle,
    },
    /// Check immediately before invoking one user Function or Function Block.
    BeforePouCall {
        /// Statically resolved callee declaration.
        callee: SymbolId,
    },
    /// Check immediately after one user Function or Function Block returns.
    AfterPouCall {
        /// Statically resolved callee declaration.
        callee: SymbolId,
    },
    /// Check whenever control takes one structured `FOR` back edge.
    LoopBackEdge,
}

/// One static checkpoint insertion site anchored to one Canonical IR node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CheckpointSite {
    /// Dense plan-local identity.
    pub id: CheckpointSiteId,
    /// POU containing the operation or Task Program return.
    pub pou: SymbolId,
    /// Canonical IR node used for source and later native-range lookup.
    pub node: CanonicalNodeId,
    /// Required runtime placement and binding.
    #[serde(flatten)]
    pub site: CheckpointSiteKind,
}

/// Checkpoint identities inserted into one POU body in structured execution order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PouCheckpointPlan {
    /// POU declaration.
    pub pou: SymbolId,
    /// Child evaluation, call boundary, then loop-back-edge execution order.
    pub checkpoints: Vec<CheckpointSiteId>,
}

/// Required final checkpoint for one actual Task Program instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TaskCheckpointPlan {
    /// Scheduler Task identity.
    pub task: TaskHandle,
    /// Program declaration instantiated by the Task.
    pub program: SymbolId,
    /// Site invoked once after the Program returns and before commit/discard resolution.
    pub return_checkpoint: CheckpointSiteId,
}

/// Complete target-independent checkpoint insertion plan.
///
/// The plan contains one static loop site regardless of iteration count and does not expand call
/// graphs. A native backend/runtime wrapper must realize every listed site exactly once while each
/// runtime loop back edge can execute its one static site repeatedly. A `TaskReturn` site is
/// fulfilled by the R0 transaction `finish` final checkpoint; it is not an additional immediately
/// adjacent checkpoint call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckpointPlan {
    /// Exact writer version.
    pub schema_version: CheckpointPlanVersion,
    /// POU-local insertion sequences in POU symbol order.
    pub pous: Vec<PouCheckpointPlan>,
    /// Task return boundaries in Task-handle order.
    pub tasks: Vec<TaskCheckpointPlan>,
    /// Dense site table in allocation order.
    pub sites: Vec<CheckpointSite>,
}

/// Inconsistent Canonical IR/source-map inputs detected by checkpoint planning.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CheckpointPlanInputError {
    /// Source Map does not contain the referenced dense source-file identity.
    #[error("source map does not contain dense source file {0}")]
    InvalidSource(u32),
    /// Source Map does not contain exactly one entry for a dense Canonical node.
    #[error("source map does not contain dense Canonical node {0}")]
    InvalidNode(u32),
    /// Canonical IR contains a missing, duplicate, or invalid POU declaration.
    #[error("invalid checkpoint POU {0}")]
    InvalidPou(u32),
    /// A user POU/FB call does not resolve to the required static callee kind.
    #[error("invalid static call target at Canonical node {0}")]
    InvalidCallTarget(u32),
    /// Checkpoint identity cannot be represented without using the reserved value.
    #[error("checkpoint plan contains too many sites for its u32 identity")]
    TooManySiteIds,
}

/// Failure to serialize a complete checkpoint plan as bounded RFC 8785 canonical JSON.
#[derive(Debug, Error)]
pub enum CheckpointPlanSerializationError {
    /// The plan was not produced by this exact writer version.
    #[error("unsupported checkpoint-plan version {major}.{minor}")]
    UnsupportedVersion {
        /// Unsupported major.
        major: u16,
        /// Unsupported minor.
        minor: u16,
    },
    /// Serialized output exceeds the explicit caller limit.
    #[error("checkpoint plan requires {actual} bytes, exceeding limit {limit}")]
    EncodedSizeExceeded {
        /// Required bytes.
        actual: usize,
        /// Allowed bytes.
        limit: usize,
    },
    /// A future value cannot be represented as canonical JSON.
    #[error("failed to serialize checkpoint plan as canonical JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

/// Serializes a complete checkpoint plan using RFC 8785 JCS within an explicit byte limit.
///
/// # Errors
///
/// Returns [`CheckpointPlanSerializationError::UnsupportedVersion`] for a mismatched version,
/// [`CheckpointPlanSerializationError::EncodedSizeExceeded`] when output crosses the caller limit,
/// or [`CheckpointPlanSerializationError::InvalidJson`] for an unrepresentable future value.
pub fn checkpoint_plan_to_json(
    plan: &CheckpointPlan,
    limits: CheckpointPlanLimits,
) -> Result<Vec<u8>, CheckpointPlanSerializationError> {
    if plan.schema_version != CheckpointPlanVersion::preview_v1_0() {
        return Err(CheckpointPlanSerializationError::UnsupportedVersion {
            major: plan.schema_version.major,
            minor: plan.schema_version.minor,
        });
    }
    let bytes = serde_jcs::to_vec(plan)?;
    if bytes.len() > limits.max_encoded_bytes() {
        return Err(CheckpointPlanSerializationError::EncodedSizeExceeded {
            actual: bytes.len(),
            limit: limits.max_encoded_bytes(),
        });
    }
    Ok(bytes)
}

#[derive(Debug)]
pub(crate) enum CheckpointBuildError {
    Capacity {
        source: SourceFileId,
        span: SourceSpan,
    },
    Input(CheckpointPlanInputError),
}

impl From<CheckpointPlanInputError> for CheckpointBuildError {
    fn from(value: CheckpointPlanInputError) -> Self {
        Self::Input(value)
    }
}

pub(crate) struct CheckpointPlanner<'a> {
    ir: &'a CanonicalStIr,
    limits: CheckpointPlanLimits,
    node_locations: &'a [NodeSourceEntry],
    symbols: BTreeMap<SymbolId, &'a SemanticSymbol>,
    sites: Vec<CheckpointSite>,
}

impl<'a> CheckpointPlanner<'a> {
    pub(crate) fn new(
        ir: &'a CanonicalStIr,
        source_map: &'a CanonicalSourceMap,
        limits: CheckpointPlanLimits,
    ) -> Result<Self, CheckpointPlanInputError> {
        for (index, entry) in source_map.sources.iter().enumerate() {
            if entry.id.0 != dense_raw_id(index)? {
                return Err(CheckpointPlanInputError::InvalidSource(entry.id.0));
            }
        }
        for (index, entry) in source_map.nodes.iter().enumerate() {
            if entry.node.0 != dense_raw_id(index)? {
                return Err(CheckpointPlanInputError::InvalidNode(entry.node.0));
            }
            let source_index = usize::try_from(entry.source.0)
                .map_err(|_| CheckpointPlanInputError::InvalidSource(entry.source.0))?;
            if source_map.sources.get(source_index).map(|source| source.id) != Some(entry.source) {
                return Err(CheckpointPlanInputError::InvalidSource(entry.source.0));
            }
        }
        let symbols = ir
            .symbols
            .iter()
            .map(|symbol| (symbol.id, symbol))
            .collect::<BTreeMap<_, _>>();
        if symbols.len() != ir.symbols.len() {
            return Err(CheckpointPlanInputError::InvalidPou(0));
        }
        Ok(Self {
            ir,
            limits,
            node_locations: &source_map.nodes,
            symbols,
            sites: Vec::new(),
        })
    }

    pub(crate) fn build(mut self) -> Result<CheckpointPlan, CheckpointBuildError> {
        let mut pous = Vec::with_capacity(self.ir.pous.len());
        for pou in &self.ir.pous {
            if self
                .symbols
                .get(&pou.symbol)
                .and_then(|symbol| pou_kind(symbol.kind))
                != Some(pou.kind)
            {
                return Err(CheckpointPlanInputError::InvalidPou(pou.symbol.0).into());
            }
            let mut checkpoints = Vec::new();
            self.walk_node(pou.symbol, &pou.body, &mut checkpoints)?;
            pous.push(PouCheckpointPlan {
                pou: pou.symbol,
                checkpoints,
            });
        }

        let mut tasks = Vec::with_capacity(self.ir.tasks.len());
        for task in &self.ir.tasks {
            let program = self
                .ir
                .pous
                .iter()
                .find(|pou| pou.symbol == task.program && pou.kind == CanonicalPouKind::Program)
                .ok_or(CheckpointPlanInputError::InvalidPou(task.program.0))?;
            let site = self.add_site(
                program.symbol,
                program.body.id,
                CheckpointSiteKind::TaskReturn { task: task.task },
                None,
            )?;
            tasks.push(TaskCheckpointPlan {
                task: task.task,
                program: task.program,
                return_checkpoint: site,
            });
        }

        Ok(CheckpointPlan {
            schema_version: CheckpointPlanVersion::preview_v1_0(),
            pous,
            tasks,
            sites: self.sites,
        })
    }

    fn walk_node(
        &mut self,
        pou: SymbolId,
        node: &CanonicalNode,
        checkpoints: &mut Vec<CheckpointSiteId>,
    ) -> Result<(), CheckpointBuildError> {
        let callee = self.user_call_target(node)?;
        for child in &node.children {
            self.walk_node(pou, child, checkpoints)?;
        }
        if let Some(callee) = callee {
            let before = self.add_site(
                pou,
                node.id,
                CheckpointSiteKind::BeforePouCall { callee },
                Some(checkpoints.len()),
            )?;
            checkpoints.push(before);
            let site = self.add_site(
                pou,
                node.id,
                CheckpointSiteKind::AfterPouCall { callee },
                Some(checkpoints.len()),
            )?;
            checkpoints.push(site);
        }
        if node.kind == AstNodeKind::ForStatement {
            if node.loop_iterations.is_none() {
                return Err(CheckpointPlanInputError::InvalidNode(node.id.0).into());
            }
            let site = self.add_site(
                pou,
                node.id,
                CheckpointSiteKind::LoopBackEdge,
                Some(checkpoints.len()),
            )?;
            checkpoints.push(site);
        }
        Ok(())
    }

    fn user_call_target(
        &self,
        node: &CanonicalNode,
    ) -> Result<Option<SymbolId>, CheckpointPlanInputError> {
        let expected = match node.kind {
            AstNodeKind::CallExpression => Some(SemanticSymbolKind::Function),
            AstNodeKind::FunctionBlockCallStatement => Some(SemanticSymbolKind::FunctionBlock),
            _ => None,
        };
        let Some(expected) = expected else {
            return Ok(None);
        };
        let referenced = node.children.first().and_then(|child| child.symbol);
        let callee = referenced.and_then(|symbol| self.call_target(symbol));
        match callee.and_then(|symbol| {
            self.symbols
                .get(&symbol)
                .map(|target| (symbol, target.kind))
        }) {
            Some((callee, actual)) if actual == expected => Ok(Some(callee)),
            None if node.kind == AstNodeKind::CallExpression => Ok(None),
            _ => Err(CheckpointPlanInputError::InvalidCallTarget(node.id.0)),
        }
    }

    fn call_target(&self, symbol: SymbolId) -> Option<SymbolId> {
        let symbol = self.symbols.get(&symbol)?;
        match symbol.kind {
            SemanticSymbolKind::Function | SemanticSymbolKind::FunctionBlock => Some(symbol.id),
            _ => match symbol.declared_type.as_ref() {
                Some(SemanticType::FunctionBlock { declaration }) => Some(*declaration),
                _ => None,
            },
        }
    }

    fn add_site(
        &mut self,
        pou: SymbolId,
        node: CanonicalNodeId,
        site: CheckpointSiteKind,
        pou_site_index: Option<usize>,
    ) -> Result<CheckpointSiteId, CheckpointBuildError> {
        let location = self.location(pou, node)?;
        if pou_site_index.is_some_and(|count| count >= self.limits.max_sites_per_pou())
            || self.sites.len() >= self.limits.max_total_sites()
        {
            return Err(CheckpointBuildError::Capacity {
                source: location.source,
                span: location.span,
            });
        }
        let id = CheckpointSiteId(dense_raw_id(self.sites.len())?);
        self.sites.push(CheckpointSite {
            id,
            pou,
            node,
            site,
        });
        Ok(id)
    }

    fn location(
        &self,
        pou: SymbolId,
        node: CanonicalNodeId,
    ) -> Result<NodeSourceEntry, CheckpointPlanInputError> {
        self.node_locations
            .get(
                usize::try_from(node.0)
                    .map_err(|_| CheckpointPlanInputError::InvalidNode(node.0))?,
            )
            .copied()
            .filter(|entry| entry.node == node && entry.pou == pou)
            .ok_or(CheckpointPlanInputError::InvalidNode(node.0))
    }
}

fn dense_raw_id(index: usize) -> Result<u32, CheckpointPlanInputError> {
    u32::try_from(index)
        .ok()
        .filter(|value| *value != u32::MAX)
        .ok_or(CheckpointPlanInputError::TooManySiteIds)
}

const fn pou_kind(kind: SemanticSymbolKind) -> Option<CanonicalPouKind> {
    match kind {
        SemanticSymbolKind::Function => Some(CanonicalPouKind::Function),
        SemanticSymbolKind::FunctionBlock => Some(CanonicalPouKind::FunctionBlock),
        SemanticSymbolKind::Program => Some(CanonicalPouKind::Program),
        _ => None,
    }
}
