//! Deterministic structured Canonical ST IR lowering.
//!
//! The representation keeps loops structured rather than unrolling them. This gives every
//! accepted executable AST node exactly one IR node and leaves native instruction/checkpoint
//! expansion to the later AOT step.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::ast::{AstNode, AstNodeKind};
use crate::{
    AddressSemanticModel, BoundTag, BoundedForLoop, CyclicWorkInputError, CyclicWorkLimits,
    CyclicWorkModel, FaultSite, FixedDataLimits, FixedGlobalLayout, FixedTypeLayout,
    InvocationFrameLayout, ProgramTaskBinding, RuntimeFaultCode, SemanticSource, SemanticSymbol,
    SemanticSymbolKind, SemanticType, SnapshotDependency, SourceSpan, StaticFunctionBlockInstance,
    StaticProgramLayout, SymbolId, TaskWorkBound, analyze_cyclic_work,
};

/// Major version of the compiler-internal structured Canonical ST IR.
pub const CANONICAL_ST_IR_MAJOR: u16 = 1;
/// Minor version of the compiler-internal structured Canonical ST IR.
pub const CANONICAL_ST_IR_MINOR: u16 = 0;

/// Version carried by every serialized structured Canonical ST IR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CanonicalIrVersion {
    /// Reader-incompatible version.
    pub major: u16,
    /// Backward-compatible additive version.
    pub minor: u16,
}

impl CanonicalIrVersion {
    /// Returns the exact version emitted by this crate.
    #[must_use]
    pub const fn preview_v1_0() -> Self {
        Self {
            major: CANONICAL_ST_IR_MAJOR,
            minor: CANONICAL_ST_IR_MINOR,
        }
    }
}

/// Mandatory finite capacities for Canonical IR construction and publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalIrLimits {
    nodes: usize,
    pous: usize,
    encoded_bytes: usize,
}

impl CanonicalIrLimits {
    /// Validates all host-side construction bounds.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalIrLimitError`] when any mandatory capacity is zero.
    pub const fn new(
        max_nodes: usize,
        max_pous: usize,
        max_encoded_bytes: usize,
    ) -> Result<Self, CanonicalIrLimitError> {
        if max_nodes == 0 {
            return Err(CanonicalIrLimitError::ZeroNodes);
        }
        if max_pous == 0 {
            return Err(CanonicalIrLimitError::ZeroPous);
        }
        if max_encoded_bytes == 0 {
            return Err(CanonicalIrLimitError::ZeroEncodedBytes);
        }
        Ok(Self {
            nodes: max_nodes,
            pous: max_pous,
            encoded_bytes: max_encoded_bytes,
        })
    }

    /// Maximum total nodes across all POU bodies.
    #[must_use]
    pub const fn max_nodes(self) -> usize {
        self.nodes
    }

    /// Maximum Function, Function Block, and Program units.
    #[must_use]
    pub const fn max_pous(self) -> usize {
        self.pous
    }

    /// Maximum RFC 8785 JSON bytes published for one IR document.
    #[must_use]
    pub const fn max_encoded_bytes(self) -> usize {
        self.encoded_bytes
    }
}

/// Invalid zero-valued Canonical IR capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CanonicalIrLimitError {
    /// IR node capacity is zero.
    #[error("max_nodes must be non-zero")]
    ZeroNodes,
    /// POU capacity is zero.
    #[error("max_pous must be non-zero")]
    ZeroPous,
    /// Serialized byte capacity is zero.
    #[error("max_encoded_bytes must be non-zero")]
    ZeroEncodedBytes,
}

/// Stable dense node identity. `u32::MAX` is never assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CanonicalNodeId(pub u32);

/// Stable dense Fault-site identity. `u32::MAX` is never assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CanonicalFaultSiteId(pub u32);

/// Canonical executable POU category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CanonicalPouKind {
    /// Pure Function.
    Function,
    /// Stateful Function Block type.
    FunctionBlock,
    /// Task-instantiated Program type.
    Program,
}

/// One deterministic runtime Fault table entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalFaultSite {
    /// Dense identity referenced by exactly one executable node.
    pub id: CanonicalFaultSiteId,
    /// Sorted, unique, non-empty runtime outcomes.
    pub possible_faults: Vec<RuntimeFaultCode>,
}

/// One structured IR node.
///
/// `children` preserve the parser-defined evaluation order. `symbol`, `value_type`, `fault_site`,
/// and `loop_iterations` are semantic annotations and are present only where the accepted upstream
/// models define them. Source locations intentionally remain outside this representation for the
/// subsequent Source Map step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalNode {
    /// Globally dense preorder identity.
    pub id: CanonicalNodeId,
    /// Frozen syntax/semantic operation category.
    pub kind: AstNodeKind,
    /// Canonical spelling for identifier/operator/literal leaves when needed by later lowering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Resolved declaration identity for a referenced identifier.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<SymbolId>,
    /// Accepted semantic result type for expression nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_type: Option<SemanticType>,
    /// Runtime Fault entry owned by this source operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault_site: Option<CanonicalFaultSiteId>,
    /// Exact static iteration count for one `FOR`; the body is never copied or unrolled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub loop_iterations: Option<u64>,
    /// Children in deterministic source/evaluation order.
    pub children: Vec<Self>,
}

/// One executable Function, Function Block, or Program body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalPou {
    /// Resolved declaration identity.
    pub symbol: SymbolId,
    /// Executable category.
    pub kind: CanonicalPouKind,
    /// Exactly one statement-list root.
    pub body: CanonicalNode,
}

/// Complete structured Canonical ST IR layered over accepted R1-02 through R1-06 models.
///
/// Layout and address collections retain their already-canonical upstream order. `pous` are in
/// symbol order, `tasks` in Task-handle order, and node IDs form one global dense preorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalStIr {
    /// Exact writer version.
    pub schema_version: CanonicalIrVersion,
    /// Declarations in deterministic symbol order.
    pub symbols: Vec<SemanticSymbol>,
    /// Canonical fixed layouts in fixed type-ID order.
    pub types: Vec<FixedTypeLayout>,
    /// Program storage templates in Program symbol order.
    pub programs: Vec<StaticProgramLayout>,
    /// Address-backed global layouts in Global symbol order.
    pub globals: Vec<FixedGlobalLayout>,
    /// Invocation frames in POU symbol order.
    pub invocation_frames: Vec<InvocationFrameLayout>,
    /// Expanded static FB instances in Program/path order.
    pub function_block_instances: Vec<StaticFunctionBlockInstance>,
    /// Bound logical tags in stable Tag-ID order.
    pub tags: Vec<BoundTag>,
    /// Cross-task snapshot edges in canonical tuple order.
    pub snapshot_dependencies: Vec<SnapshotDependency>,
    /// Dense runtime Fault table.
    pub fault_sites: Vec<CanonicalFaultSite>,
    /// Task work entries in Task-handle order, exactly one per accepted task binding.
    pub tasks: Vec<TaskWorkBound>,
    /// Executable units in declaration symbol order.
    pub pous: Vec<CanonicalPou>,
}

/// Atomic lowering result. A capacity diagnostic suppresses the complete IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalIrOutput {
    /// Complete IR only when every boundary is satisfied.
    pub ir: Option<CanonicalStIr>,
    /// At most one deterministic resource diagnostic for this lowering step.
    pub diagnostics: Vec<crate::Diagnostic>,
}

/// Caller/model mismatch that cannot be represented as an ST source diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CanonicalIrInputError {
    /// Revalidation of the accepted work model failed.
    #[error(transparent)]
    WorkAnalysis(#[from] CyclicWorkInputError),
    /// Sources or upstream models do not reproduce the accepted work proof.
    #[error("sources do not match the accepted R1-06 work model")]
    WorkModelMismatch,
    /// A POU declaration/body cannot be matched exactly once.
    #[error("missing or duplicate executable POU {0}")]
    InvalidPou(u32),
    /// A runtime Fault site has the same source identity as another entry.
    #[error("duplicate runtime Fault site at `{source_path}` bytes {start}..{end}")]
    DuplicateFaultSite {
        /// Source path.
        source_path: String,
        /// Inclusive byte offset.
        start: u32,
        /// Exclusive byte offset.
        end: u32,
    },
    /// An executable Fault site was omitted or would be generated more than once.
    #[error("runtime Fault site {0} is not bound exactly once")]
    InvalidFaultBinding(u32),
    /// A proved loop was omitted or would be generated more than once.
    #[error("bounded loop at `{source_path}` bytes {start}..{end} is not bound exactly once")]
    InvalidLoopBinding {
        /// Source path.
        source_path: String,
        /// Inclusive byte offset.
        start: u32,
        /// Exclusive byte offset.
        end: u32,
    },
    /// A task is absent, repeated, or does not match its Program binding.
    #[error("task handle {0} is not represented exactly once")]
    InvalidTask(u32),
    /// The accepted AST/model pair has an impossible executable shape.
    #[error("invalid executable AST shape in `{source_path}` at bytes {start}..{end}")]
    InvalidAstShape {
        /// Source path.
        source_path: String,
        /// Inclusive byte offset.
        start: u32,
        /// Exclusive byte offset.
        end: u32,
    },
    /// Dense node identity cannot be represented without using the reserved value.
    #[error("Canonical IR contains too many nodes for its u32 identity")]
    TooManyNodeIds,
}

/// Failure to serialize a complete IR as bounded RFC 8785 canonical JSON.
#[derive(Debug, Error)]
pub enum CanonicalIrSerializationError {
    /// The value was not produced by this exact writer version.
    #[error("unsupported Canonical ST IR version {major}.{minor}")]
    UnsupportedVersion {
        /// Unsupported major.
        major: u16,
        /// Unsupported minor.
        minor: u16,
    },
    /// Serialized output exceeds the explicit caller limit.
    #[error("Canonical ST IR requires {actual} bytes, exceeding limit {limit}")]
    EncodedSizeExceeded {
        /// Required bytes.
        actual: usize,
        /// Allowed bytes.
        limit: usize,
    },
    /// A future value cannot be represented as canonical JSON.
    #[error("failed to serialize Canonical ST IR as canonical JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

/// Lowers one fully accepted project into deterministic structured Canonical ST IR.
///
/// The function revalidates the R1-06 work proof against the supplied sources and R1-05 model.
/// It emits exactly one POU per executable declaration, one task entry per binding, one node per
/// executable AST node, one Fault reference per site, and one proof annotation per `FOR`. Loops are
/// never unrolled. Any mismatch returns an error; hitting a caller capacity returns one `ST3005`
/// diagnostic and no partial IR.
///
/// # Errors
///
/// Returns [`CanonicalIrInputError`] when accepted inputs are mutually inconsistent or corrupt.
pub fn lower_canonical_ir(
    sources: &[SemanticSource<'_>],
    address_model: &AddressSemanticModel,
    work_model: &CyclicWorkModel,
    fixed_limits: FixedDataLimits,
    work_limits: CyclicWorkLimits,
    ir_limits: CanonicalIrLimits,
) -> Result<CanonicalIrOutput, CanonicalIrInputError> {
    let revalidated = analyze_cyclic_work(sources, address_model, fixed_limits, work_limits)?;
    if revalidated.model.as_ref() != Some(work_model) || !revalidated.diagnostics.is_empty() {
        return Err(CanonicalIrInputError::WorkModelMismatch);
    }
    Lowerer::new(sources, address_model, work_model, ir_limits)?.run()
}

/// Serializes a complete structured IR using RFC 8785 JCS within an explicit byte limit.
///
/// # Errors
///
/// Returns [`CanonicalIrSerializationError::UnsupportedVersion`] for a mismatched version,
/// [`CanonicalIrSerializationError::EncodedSizeExceeded`] when output would cross the caller
/// limit, or [`CanonicalIrSerializationError::InvalidJson`] for an unrepresentable future value.
pub fn canonical_ir_to_json(
    ir: &CanonicalStIr,
    limits: CanonicalIrLimits,
) -> Result<Vec<u8>, CanonicalIrSerializationError> {
    if ir.schema_version != CanonicalIrVersion::preview_v1_0() {
        return Err(CanonicalIrSerializationError::UnsupportedVersion {
            major: ir.schema_version.major,
            minor: ir.schema_version.minor,
        });
    }
    let bytes = serde_jcs::to_vec(ir)?;
    if bytes.len() > limits.max_encoded_bytes() {
        return Err(CanonicalIrSerializationError::EncodedSizeExceeded {
            actual: bytes.len(),
            limit: limits.max_encoded_bytes(),
        });
    }
    Ok(bytes)
}

type LocationKey = (String, SourceSpan);

struct Lowerer<'a> {
    sources: BTreeMap<String, SemanticSource<'a>>,
    address_model: &'a AddressSemanticModel,
    work_model: &'a CyclicWorkModel,
    limits: CanonicalIrLimits,
    symbols: BTreeMap<SymbolId, &'a SemanticSymbol>,
    references: BTreeMap<LocationKey, SymbolId>,
    expressions: BTreeMap<LocationKey, SemanticType>,
    faults: BTreeMap<LocationKey, CanonicalFaultSiteId>,
    fault_entries: Vec<CanonicalFaultSite>,
    used_faults: BTreeSet<CanonicalFaultSiteId>,
    loops: BTreeMap<LocationKey, &'a BoundedForLoop>,
    used_loops: BTreeSet<LocationKey>,
    next_node: usize,
}

impl<'a> Lowerer<'a> {
    fn new(
        sources: &[SemanticSource<'a>],
        address_model: &'a AddressSemanticModel,
        work_model: &'a CyclicWorkModel,
        limits: CanonicalIrLimits,
    ) -> Result<Self, CanonicalIrInputError> {
        let sources = sources
            .iter()
            .map(|source| (source.ast.source_path.clone(), *source))
            .collect();
        let semantics = &address_model.faults.fixed.semantics;
        let symbols = semantics
            .symbols
            .iter()
            .map(|symbol| (symbol.id, symbol))
            .collect();
        let references = semantics
            .references
            .iter()
            .map(|reference| {
                (
                    (reference.source_path.clone(), reference.span),
                    reference.symbol,
                )
            })
            .collect();
        let expressions = semantics
            .expressions
            .iter()
            .map(|expression| {
                (
                    (expression.source_path.clone(), expression.span),
                    expression.value_type.clone(),
                )
            })
            .collect();
        let mut faults = BTreeMap::new();
        let mut fault_entries = Vec::with_capacity(address_model.faults.fault_sites.len());
        for (index, site) in address_model.faults.fault_sites.iter().enumerate() {
            let id = dense_fault_id(index)?;
            let key = (site.id.source_path.clone(), site.id.span);
            if faults.insert(key.clone(), id).is_some() {
                return Err(CanonicalIrInputError::DuplicateFaultSite {
                    source_path: key.0,
                    start: key.1.start,
                    end: key.1.end,
                });
            }
            fault_entries.push(canonical_fault(id, site));
        }
        let loops = work_model
            .loops
            .iter()
            .map(|proof| ((proof.source_path.clone(), proof.span), proof))
            .collect();
        Ok(Self {
            sources,
            address_model,
            work_model,
            limits,
            symbols,
            references,
            expressions,
            faults,
            fault_entries,
            used_faults: BTreeSet::new(),
            loops,
            used_loops: BTreeSet::new(),
            next_node: 0,
        })
    }

    fn run(mut self) -> Result<CanonicalIrOutput, CanonicalIrInputError> {
        self.validate_tasks()?;
        let executable = self
            .symbols
            .values()
            .filter(|symbol| canonical_pou_kind(symbol.kind).is_some())
            .copied()
            .collect::<Vec<_>>();
        if executable.len() > self.limits.max_pous() {
            let (source_path, span) = executable
                .get(self.limits.max_pous())
                .map_or(("", SourceSpan { start: 0, end: 0 }), |symbol| {
                    (symbol.source_path.as_str(), symbol.span)
                });
            return Ok(self.capacity_output(source_path, span));
        }
        let mut pous = Vec::with_capacity(executable.len());
        for symbol in executable {
            let source = self
                .sources
                .get(&symbol.source_path)
                .copied()
                .ok_or(CanonicalIrInputError::InvalidPou(symbol.id.0))?;
            let declaration = find_unique_pou(&source.ast.root, symbol)?;
            let bodies = declaration
                .children
                .iter()
                .filter(|child| child.kind == AstNodeKind::StatementList)
                .collect::<Vec<_>>();
            let [body] = bodies.as_slice() else {
                return Err(CanonicalIrInputError::InvalidPou(symbol.id.0));
            };
            let lowered = match self.lower_node(&symbol.source_path, body) {
                Ok(node) => node,
                Err(LowerNodeError::Capacity(span)) => {
                    return Ok(self.capacity_output(&symbol.source_path, span));
                }
                Err(LowerNodeError::Input(error)) => return Err(error),
            };
            pous.push(CanonicalPou {
                symbol: symbol.id,
                kind: canonical_pou_kind(symbol.kind)
                    .ok_or(CanonicalIrInputError::InvalidPou(symbol.id.0))?,
                body: lowered,
            });
        }
        self.validate_consumed_proofs()?;
        let fixed = &self.address_model.faults.fixed;
        Ok(CanonicalIrOutput {
            ir: Some(CanonicalStIr {
                schema_version: CanonicalIrVersion::preview_v1_0(),
                symbols: fixed.semantics.symbols.clone(),
                types: fixed.types.clone(),
                programs: fixed.programs.clone(),
                globals: fixed.globals.clone(),
                invocation_frames: fixed.invocation_frames.clone(),
                function_block_instances: fixed.function_block_instances.clone(),
                tags: self.address_model.tags.clone(),
                snapshot_dependencies: self.address_model.snapshot_dependencies.clone(),
                fault_sites: self.fault_entries,
                tasks: self.work_model.tasks.clone(),
                pous,
            }),
            diagnostics: Vec::new(),
        })
    }

    fn validate_tasks(&self) -> Result<(), CanonicalIrInputError> {
        let mut accepted = BTreeMap::new();
        for ProgramTaskBinding {
            program,
            task_handle,
        } in &self.address_model.program_tasks
        {
            if accepted.insert(*task_handle, *program).is_some() {
                return Err(CanonicalIrInputError::InvalidTask(task_handle.0));
            }
        }
        let mut proved = BTreeSet::new();
        for task in &self.work_model.tasks {
            if !proved.insert(task.task) || accepted.get(&task.task).copied() != Some(task.program)
            {
                return Err(CanonicalIrInputError::InvalidTask(task.task.0));
            }
        }
        if accepted.len() != proved.len() {
            let missing = accepted
                .keys()
                .find(|task| !proved.contains(task))
                .map_or(0, |task| task.0);
            return Err(CanonicalIrInputError::InvalidTask(missing));
        }
        Ok(())
    }

    fn lower_node(
        &mut self,
        source_path: &str,
        node: &AstNode,
    ) -> Result<CanonicalNode, LowerNodeError> {
        if self.next_node >= self.limits.max_nodes() {
            return Err(LowerNodeError::Capacity(node.span));
        }
        let id = u32::try_from(self.next_node)
            .ok()
            .filter(|value| *value != u32::MAX)
            .map(CanonicalNodeId)
            .ok_or(CanonicalIrInputError::TooManyNodeIds)?;
        self.next_node += 1;
        let key = (source_path.to_owned(), node.span);
        let fault_site = self.faults.get(&key).copied();
        if let Some(site) = fault_site
            && !self.used_faults.insert(site)
        {
            return Err(CanonicalIrInputError::InvalidFaultBinding(site.0).into());
        }
        let loop_iterations = if node.kind == AstNodeKind::ForStatement {
            let proof = self.loops.get(&key).copied().ok_or_else(|| {
                CanonicalIrInputError::InvalidLoopBinding {
                    source_path: source_path.to_owned(),
                    start: node.span.start,
                    end: node.span.end,
                }
            })?;
            if !self.used_loops.insert(key.clone()) {
                return Err(CanonicalIrInputError::InvalidLoopBinding {
                    source_path: source_path.to_owned(),
                    start: node.span.start,
                    end: node.span.end,
                }
                .into());
            }
            Some(proof.iterations)
        } else {
            None
        };
        let mut children = Vec::with_capacity(node.children.len());
        for child in &node.children {
            children.push(self.lower_node(source_path, child)?);
        }
        Ok(CanonicalNode {
            id,
            kind: node.kind,
            text: canonical_text(node),
            symbol: self.references.get(&key).copied(),
            value_type: self.expressions.get(&key).cloned(),
            fault_site,
            loop_iterations,
            children,
        })
    }

    fn validate_consumed_proofs(&self) -> Result<(), CanonicalIrInputError> {
        for site in &self.fault_entries {
            if !self.used_faults.contains(&site.id) {
                return Err(CanonicalIrInputError::InvalidFaultBinding(site.id.0));
            }
        }
        for key in self.loops.keys() {
            if !self.used_loops.contains(key) {
                return Err(CanonicalIrInputError::InvalidLoopBinding {
                    source_path: key.0.clone(),
                    start: key.1.start,
                    end: key.1.end,
                });
            }
        }
        Ok(())
    }

    fn capacity_output(&self, source_path: &str, span: SourceSpan) -> CanonicalIrOutput {
        let source = self
            .sources
            .get(source_path)
            .map_or("", |value| value.source);
        CanonicalIrOutput {
            ir: None,
            diagnostics: vec![crate::diagnostic::make_diagnostic(
                source,
                source_path,
                crate::DiagnosticCode::ResourceBudgetExceeded,
                span,
            )],
        }
    }
}

#[derive(Debug)]
enum LowerNodeError {
    Capacity(SourceSpan),
    Input(CanonicalIrInputError),
}

impl From<CanonicalIrInputError> for LowerNodeError {
    fn from(value: CanonicalIrInputError) -> Self {
        Self::Input(value)
    }
}

fn dense_fault_id(index: usize) -> Result<CanonicalFaultSiteId, CanonicalIrInputError> {
    u32::try_from(index)
        .ok()
        .filter(|value| *value != u32::MAX)
        .map(CanonicalFaultSiteId)
        .ok_or(CanonicalIrInputError::TooManyNodeIds)
}

fn canonical_fault(id: CanonicalFaultSiteId, site: &FaultSite) -> CanonicalFaultSite {
    CanonicalFaultSite {
        id,
        possible_faults: site.possible_faults.clone(),
    }
}

const fn canonical_pou_kind(kind: SemanticSymbolKind) -> Option<CanonicalPouKind> {
    match kind {
        SemanticSymbolKind::Function => Some(CanonicalPouKind::Function),
        SemanticSymbolKind::FunctionBlock => Some(CanonicalPouKind::FunctionBlock),
        SemanticSymbolKind::Program => Some(CanonicalPouKind::Program),
        _ => None,
    }
}

fn find_unique_pou<'a>(
    root: &'a AstNode,
    symbol: &SemanticSymbol,
) -> Result<&'a AstNode, CanonicalIrInputError> {
    let expected = match symbol.kind {
        SemanticSymbolKind::Function => AstNodeKind::FunctionDeclaration,
        SemanticSymbolKind::FunctionBlock => AstNodeKind::FunctionBlockDeclaration,
        SemanticSymbolKind::Program => AstNodeKind::ProgramDeclaration,
        _ => return Err(CanonicalIrInputError::InvalidPou(symbol.id.0)),
    };
    let matches = root
        .children
        .iter()
        .filter(|node| {
            node.kind == expected
                && node
                    .children
                    .first()
                    .is_some_and(|name| name.span == symbol.span)
        })
        .collect::<Vec<_>>();
    let [declaration] = matches.as_slice() else {
        return Err(CanonicalIrInputError::InvalidPou(symbol.id.0));
    };
    Ok(declaration)
}

fn canonical_text(node: &AstNode) -> Option<String> {
    let text = node.text.as_ref()?;
    Some(match node.kind {
        AstNodeKind::Identifier => text.to_ascii_lowercase(),
        AstNodeKind::UnaryExpression
        | AstNodeKind::BinaryExpression
        | AstNodeKind::DirectAddress => text.to_ascii_uppercase(),
        AstNodeKind::Literal if text.eq_ignore_ascii_case("true") => "TRUE".to_owned(),
        AstNodeKind::Literal if text.eq_ignore_ascii_case("false") => "FALSE".to_owned(),
        _ => text.clone(),
    })
}
