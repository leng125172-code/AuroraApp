//! Static loop and per-task work proofs required before Canonical IR generation.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::ast::{AstNode, AstNodeKind};
use crate::diagnostic::{make_diagnostic, sort_diagnostics};
use crate::{
    AddressSemanticModel, AnalysisInputError, Diagnostic, DiagnosticCode, FixedDataLimits,
    IntegerArithmeticMode, IntegerOperation, IntegerType, ProgramTaskBinding, SemanticSource,
    SemanticSymbol, SemanticSymbolKind, SemanticType, SourceSpan, SymbolId, TaskHandle,
    analyze_faults, evaluate_integer_operation,
};

/// Explicit capacities for a host-only cyclic work proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CyclicWorkLimits {
    loops: usize,
    tasks: usize,
    iterations_per_loop: u64,
    operations_per_task: u64,
}

impl CyclicWorkLimits {
    /// Validates every mandatory finite capacity.
    ///
    /// # Errors
    ///
    /// Returns [`CyclicWorkLimitError`] when any capacity is zero.
    pub const fn new(
        max_loops: usize,
        max_tasks: usize,
        max_iterations_per_loop: u64,
        max_source_operations_per_task: u64,
    ) -> Result<Self, CyclicWorkLimitError> {
        if max_loops == 0 {
            return Err(CyclicWorkLimitError::ZeroLoops);
        }
        if max_tasks == 0 {
            return Err(CyclicWorkLimitError::ZeroTasks);
        }
        if max_iterations_per_loop == 0 {
            return Err(CyclicWorkLimitError::ZeroLoopIterations);
        }
        if max_source_operations_per_task == 0 {
            return Err(CyclicWorkLimitError::ZeroTaskOperations);
        }
        Ok(Self {
            loops: max_loops,
            tasks: max_tasks,
            iterations_per_loop: max_iterations_per_loop,
            operations_per_task: max_source_operations_per_task,
        })
    }

    /// Maximum accepted `FOR` nodes in one project.
    #[must_use]
    pub const fn max_loops(self) -> usize {
        self.loops
    }

    /// Maximum accepted Program-to-Task bindings.
    #[must_use]
    pub const fn max_tasks(self) -> usize {
        self.tasks
    }

    /// Maximum exact iterations for one `FOR`.
    #[must_use]
    pub const fn max_iterations_per_loop(self) -> u64 {
        self.iterations_per_loop
    }

    /// Maximum worst-case source semantic operations for one task invocation.
    #[must_use]
    pub const fn max_source_operations_per_task(self) -> u64 {
        self.operations_per_task
    }
}

/// Invalid zero-valued work-proof capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CyclicWorkLimitError {
    /// Project loop capacity is zero.
    #[error("max_loops must be non-zero")]
    ZeroLoops,
    /// Program task capacity is zero.
    #[error("max_tasks must be non-zero")]
    ZeroTasks,
    /// Per-loop iteration capacity is zero.
    #[error("max_iterations_per_loop must be non-zero")]
    ZeroLoopIterations,
    /// Per-task operation capacity is zero.
    #[error("max_source_operations_per_task must be non-zero")]
    ZeroTaskOperations,
}

/// Caller/model mismatch that cannot be represented as an ST source diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CyclicWorkInputError {
    /// Upstream reanalysis rejected a purportedly accepted source set.
    #[error(transparent)]
    UpstreamAnalysis(#[from] AnalysisInputError),
    /// Sources do not reproduce the exact R1-04 model embedded in the R1-05 model.
    #[error("sources do not match the accepted R1-05 semantic model")]
    UpstreamModelMismatch,
    /// The exact source used by the accepted upstream model is absent or repeated.
    #[error("missing or duplicate source `{0}` for cyclic work analysis")]
    InvalidSourceSet(String),
    /// A Task handle is bound more than once.
    #[error("duplicate Program-to-Task binding for task handle {0}")]
    DuplicateTask(u32),
    /// A binding does not reference a Program declaration.
    #[error("task handle {task} references non-Program symbol {program}")]
    InvalidProgramTask {
        /// Referenced symbol.
        program: u32,
        /// Task handle containing the reference.
        task: u32,
    },
    /// The accepted AST/model pair has an impossible shape.
    #[error("invalid accepted AST shape in `{source_path}` at bytes {start}..{end}")]
    InvalidAstShape {
        /// Source path.
        source_path: String,
        /// Inclusive offset.
        start: u32,
        /// Exclusive offset.
        end: u32,
    },
}

/// Exact proof for one accepted static `FOR`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundedForLoop {
    /// Owning POU.
    pub owner: SymbolId,
    /// Normalized source path.
    pub source_path: String,
    /// Complete `FOR` span.
    pub span: SourceSpan,
    /// Local control variable.
    pub control: SymbolId,
    /// Compile-time initial value.
    pub initial: i128,
    /// Compile-time inclusive end value.
    pub end: i128,
    /// Compile-time nonzero step.
    pub step: i128,
    /// Exact iteration count, including legal zero-iteration loops.
    pub iterations: u64,
}

/// Worst-case source-operation proof for one Program task instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TaskWorkBound {
    /// Scheduler Task identity.
    pub task: TaskHandle,
    /// Program declaration instantiated by the task.
    pub program: SymbolId,
    /// Worst-case number of source semantic operations in one invocation.
    pub source_operations: u64,
}

/// Complete boundary proof consumed by later Canonical IR lowering.
///
/// `source_operations` counts each semantic AST operation at most once per execution, expands
/// calls and exact loop counts, and takes the worst `IF` branch. It is not a native instruction or
/// checkpoint estimate; Canonical IR/AOT must separately validate every generated operation and
/// inserted checkpoint against the Target Profile before publishing artifacts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CyclicWorkModel {
    /// Loops in path/span order, exactly one entry per accepted `FOR`.
    pub loops: Vec<BoundedForLoop>,
    /// Task bounds in Task-handle order, exactly one entry per binding.
    pub tasks: Vec<TaskWorkBound>,
}

/// Atomic result. Any diagnostic suppresses the complete proof model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CyclicWorkAnalysisOutput {
    /// Complete proof only when every loop and task satisfies its bound.
    pub model: Option<CyclicWorkModel>,
    /// Stable diagnostics sorted by path/span/code.
    pub diagnostics: Vec<Diagnostic>,
}

/// Proves static `FOR` iteration counts and worst-case per-task work.
///
/// This pass is host-only and performs no I/O or code generation. It consumes an accepted R1-05
/// model, checks the missing Preview 1.0 `ST3001`/`ST3002`/`ST3003` boundaries, and produces one
/// loop/task record only after all limits pass. A diagnostic publishes no partial proof.
///
/// # Errors
///
/// Returns [`CyclicWorkInputError`] for caller-owned source/task mismatches or an impossible
/// upstream AST/model shape.
pub fn analyze_cyclic_work(
    sources: &[SemanticSource<'_>],
    model: &AddressSemanticModel,
    fixed_limits: FixedDataLimits,
    limits: CyclicWorkLimits,
) -> Result<CyclicWorkAnalysisOutput, CyclicWorkInputError> {
    WorkAnalyzer::new(sources, model, fixed_limits, limits)?.run()
}

struct WorkAnalyzer<'a> {
    sources: Vec<SemanticSource<'a>>,
    limits: CyclicWorkLimits,
    symbols: BTreeMap<SymbolId, &'a SemanticSymbol>,
    references: BTreeMap<(String, SourceSpan), SymbolId>,
    expression_types: BTreeMap<(String, SourceSpan), SemanticType>,
    pous: BTreeMap<SymbolId, (SemanticSource<'a>, &'a AstNode)>,
    tasks: Vec<ProgramTaskBinding>,
    task_limit_exceeded: bool,
    loop_count: usize,
    loops: Vec<BoundedForLoop>,
    loop_iterations: BTreeMap<(String, SourceSpan), u64>,
    diagnostics: Vec<Diagnostic>,
}

type SymbolMap<'a> = BTreeMap<SymbolId, &'a SemanticSymbol>;
type ReferenceMap = BTreeMap<(String, SourceSpan), SymbolId>;
type ExpressionTypeMap = BTreeMap<(String, SourceSpan), SemanticType>;
type PouMap<'a> = BTreeMap<SymbolId, (SemanticSource<'a>, &'a AstNode)>;

impl<'a> WorkAnalyzer<'a> {
    fn new(
        sources: &[SemanticSource<'a>],
        model: &'a AddressSemanticModel,
        fixed_limits: FixedDataLimits,
        limits: CyclicWorkLimits,
    ) -> Result<Self, CyclicWorkInputError> {
        let reanalysis = analyze_faults(sources, fixed_limits)?;
        if reanalysis.model.as_ref() != Some(&model.faults) {
            return Err(CyclicWorkInputError::UpstreamModelMismatch);
        }
        let ordered = prepare_sources(sources, &model.faults.fixed.semantics.symbols)?;
        let (symbols, references, expression_types) = semantic_maps(model);
        let (tasks, task_limit_exceeded) = prepare_tasks(&model.program_tasks, limits, &symbols)?;
        let pous = collect_pous(&ordered, &symbols)?;
        Ok(Self {
            sources: ordered,
            limits,
            symbols,
            references,
            expression_types,
            pous,
            tasks,
            task_limit_exceeded,
            loop_count: 0,
            loops: Vec::new(),
            loop_iterations: BTreeMap::new(),
            diagnostics: Vec::new(),
        })
    }

    fn run(mut self) -> Result<CyclicWorkAnalysisOutput, CyclicWorkInputError> {
        let pous = self.pous.clone();
        for (owner, (source, node)) in pous {
            self.collect_loops(owner, source, node, &mut BTreeSet::new())?;
        }
        if self.loop_count > self.limits.max_loops()
            && let Some(source) = self.sources.first().copied()
        {
            self.emit(
                source,
                DiagnosticCode::ResourceBudgetExceeded,
                source.ast.root.span,
            );
        }
        if self.task_limit_exceeded
            && let Some(source) = self.sources.first().copied()
        {
            self.emit(
                source,
                DiagnosticCode::ResourceBudgetExceeded,
                source.ast.root.span,
            );
        }
        let mut task_bounds = Vec::with_capacity(self.tasks.len());
        for binding in self.tasks.clone() {
            let source_operations = self.pou_work(binding.program, &mut BTreeSet::new())?;
            if source_operations > self.limits.max_source_operations_per_task()
                && let Some(symbol) = self.symbols.get(&binding.program).copied()
                && let Some(source) = self.source(&symbol.source_path)
            {
                self.emit(source, DiagnosticCode::ResourceBudgetExceeded, symbol.span);
            }
            task_bounds.push(TaskWorkBound {
                task: binding.task_handle,
                program: binding.program,
                source_operations,
            });
        }
        sort_diagnostics(&mut self.diagnostics);
        self.diagnostics.dedup_by(|left, right| {
            left.source_path == right.source_path
                && left.span == right.span
                && left.code == right.code
        });
        if self.diagnostics.is_empty() {
            Ok(CyclicWorkAnalysisOutput {
                model: Some(CyclicWorkModel {
                    loops: self.loops,
                    tasks: task_bounds,
                }),
                diagnostics: Vec::new(),
            })
        } else {
            Ok(CyclicWorkAnalysisOutput {
                model: None,
                diagnostics: self.diagnostics,
            })
        }
    }

    fn collect_loops(
        &mut self,
        owner: SymbolId,
        source: SemanticSource<'a>,
        node: &'a AstNode,
        active_controls: &mut BTreeSet<SymbolId>,
    ) -> Result<(), CyclicWorkInputError> {
        let mut inserted_control = None;
        if node.kind == AstNodeKind::ForStatement {
            let control_node = node
                .children
                .first()
                .ok_or_else(|| invalid_shape(source, node.span))?;
            let control = self
                .references
                .get(&(source.ast.source_path.clone(), control_node.span))
                .copied()
                .ok_or_else(|| invalid_shape(source, control_node.span))?;
            if active_controls.contains(&control) {
                self.emit(
                    source,
                    DiagnosticCode::InvalidAssignmentTarget,
                    control_node.span,
                );
            } else {
                active_controls.insert(control);
                inserted_control = Some(control);
            }
            self.collect_loop(owner, source, node)?;
        }
        for child in &node.children {
            self.collect_loops(owner, source, child, active_controls)?;
        }
        if let Some(control) = inserted_control {
            active_controls.remove(&control);
        }
        Ok(())
    }

    fn collect_loop(
        &mut self,
        owner: SymbolId,
        source: SemanticSource<'a>,
        node: &'a AstNode,
    ) -> Result<(), CyclicWorkInputError> {
        self.loop_count = self.loop_count.saturating_add(1);
        let control_node = node
            .children
            .first()
            .ok_or_else(|| invalid_shape(source, node.span))?;
        let control = self
            .references
            .get(&(source.ast.source_path.clone(), control_node.span))
            .copied()
            .ok_or_else(|| invalid_shape(source, control_node.span))?;
        if !self.symbols.get(&control).is_some_and(|symbol| {
            symbol.kind == SemanticSymbolKind::LocalVariable && symbol.owner == Some(owner)
        }) {
            self.emit(
                source,
                DiagnosticCode::InvalidAssignmentTarget,
                control_node.span,
            );
        }
        let bounds_end = node.children.len().saturating_sub(1);
        let bounds = node
            .children
            .get(1..bounds_end)
            .ok_or_else(|| invalid_shape(source, node.span))?;
        let initial_node = bounds
            .first()
            .ok_or_else(|| invalid_shape(source, node.span))?;
        let end_node = bounds
            .get(1)
            .ok_or_else(|| invalid_shape(source, node.span))?;
        let step_node = bounds.get(2);
        let initial = self.constant_integer(source, initial_node);
        let end = self.constant_integer(source, end_node);
        let step = step_node.map_or(Some(1), |value| self.constant_integer(source, value));
        let (Some(initial), Some(end), Some(step)) = (initial, end, step) else {
            self.emit(source, DiagnosticCode::UnboundedLoop, node.span);
            return Ok(());
        };
        if step == 0 {
            self.emit(
                source,
                DiagnosticCode::InvalidForStep,
                step_node.map_or(node.span, |value| value.span),
            );
            return Ok(());
        }
        let iterations = exact_iterations(initial, end, step);
        let Some(iterations) = iterations else {
            self.emit(source, DiagnosticCode::LoopLimitExceeded, node.span);
            return Ok(());
        };
        if iterations > self.limits.max_iterations_per_loop() {
            self.emit(source, DiagnosticCode::LoopLimitExceeded, node.span);
            return Ok(());
        }
        if self.loop_count <= self.limits.max_loops() {
            self.loop_iterations
                .insert((source.ast.source_path.clone(), node.span), iterations);
            self.loops.push(BoundedForLoop {
                owner,
                source_path: source.ast.source_path.clone(),
                span: node.span,
                control,
                initial,
                end,
                step,
                iterations,
            });
        }
        Ok(())
    }

    fn pou_work(
        &self,
        symbol: SymbolId,
        visiting: &mut BTreeSet<SymbolId>,
    ) -> Result<u64, CyclicWorkInputError> {
        if !visiting.insert(symbol) {
            return Err(CyclicWorkInputError::InvalidAstShape {
                source_path: self
                    .symbols
                    .get(&symbol)
                    .map_or_else(String::new, |value| value.source_path.clone()),
                start: 0,
                end: 0,
            });
        }
        let Some((source, pou)) = self.pous.get(&symbol).copied() else {
            visiting.remove(&symbol);
            let source_path = self
                .symbols
                .get(&symbol)
                .map_or_else(String::new, |value| value.source_path.clone());
            let span = self
                .symbols
                .get(&symbol)
                .map_or(SourceSpan { start: 0, end: 0 }, |value| value.span);
            return Err(CyclicWorkInputError::InvalidAstShape {
                source_path,
                start: span.start,
                end: span.end,
            });
        };
        let work = pou
            .children
            .iter()
            .find(|child| child.kind == AstNodeKind::StatementList)
            .map_or(Ok(0), |body| self.node_work(source, body, visiting))?;
        visiting.remove(&symbol);
        Ok(work)
    }

    fn node_work(
        &self,
        source: SemanticSource<'a>,
        node: &'a AstNode,
        visiting: &mut BTreeSet<SymbolId>,
    ) -> Result<u64, CyclicWorkInputError> {
        match node.kind {
            AstNodeKind::CompilationUnit
            | AstNodeKind::StatementList
            | AstNodeKind::ParenthesizedExpression
            | AstNodeKind::QualifiedIdentifier
            | AstNodeKind::Identifier
            | AstNodeKind::DirectAddress => self.sum_children(source, &node.children, visiting),
            AstNodeKind::QualifiedLiteral => Ok(1),
            AstNodeKind::IfStatement => self.if_work(source, node, visiting),
            AstNodeKind::ElsifClause | AstNodeKind::ElseClause => {
                self.sum_children(source, &node.children, visiting)
            }
            AstNodeKind::ForStatement => self.for_work(source, node, visiting),
            AstNodeKind::CallExpression | AstNodeKind::FunctionBlockCallStatement => {
                let children = self.sum_children(source, &node.children, visiting)?;
                let called = self
                    .first_reference(&source.ast.source_path, node)
                    .and_then(|symbol| self.call_target(symbol))
                    .map_or(Ok(0), |target| self.pou_work(target, visiting))?;
                Ok(bounded_sum(
                    [1, children, called],
                    self.limits.max_source_operations_per_task(),
                ))
            }
            kind if is_semantic_operation(kind) => {
                let children = self.sum_children(source, &node.children, visiting)?;
                Ok(bounded_sum(
                    [1, children],
                    self.limits.max_source_operations_per_task(),
                ))
            }
            _ => self.sum_children(source, &node.children, visiting),
        }
    }

    fn if_work(
        &self,
        source: SemanticSource<'a>,
        node: &'a AstNode,
        visiting: &mut BTreeSet<SymbolId>,
    ) -> Result<u64, CyclicWorkInputError> {
        let condition = node
            .children
            .first()
            .ok_or_else(|| invalid_shape(source, node.span))?;
        let then_body = node
            .children
            .get(1)
            .ok_or_else(|| invalid_shape(source, node.span))?;
        let condition_work = self.node_work(source, condition, visiting)?;
        let mut branch_max = self.node_work(source, then_body, visiting)?;
        for clause in node.children.iter().skip(2) {
            branch_max = branch_max.max(self.node_work(source, clause, visiting)?);
        }
        Ok(bounded_sum(
            [1, condition_work, branch_max],
            self.limits.max_source_operations_per_task(),
        ))
    }

    fn for_work(
        &self,
        source: SemanticSource<'a>,
        node: &'a AstNode,
        visiting: &mut BTreeSet<SymbolId>,
    ) -> Result<u64, CyclicWorkInputError> {
        let body = node
            .children
            .last()
            .ok_or_else(|| invalid_shape(source, node.span))?;
        let prelude = self.sum_children(
            source,
            node.children
                .get(1..node.children.len().saturating_sub(1))
                .ok_or_else(|| invalid_shape(source, node.span))?,
            visiting,
        )?;
        let body_work = self.node_work(source, body, visiting)?;
        let iterations = self
            .loop_iterations
            .get(&(source.ast.source_path.clone(), node.span))
            .copied()
            .unwrap_or(0);
        let ceiling = self
            .limits
            .max_source_operations_per_task()
            .saturating_add(1);
        let repeated = body_work.saturating_mul(iterations).min(ceiling);
        Ok(bounded_sum(
            [1, prelude, repeated],
            self.limits.max_source_operations_per_task(),
        ))
    }

    fn sum_children(
        &self,
        source: SemanticSource<'a>,
        children: &'a [AstNode],
        visiting: &mut BTreeSet<SymbolId>,
    ) -> Result<u64, CyclicWorkInputError> {
        let mut total = 0_u64;
        for child in children {
            total = total
                .saturating_add(self.node_work(source, child, visiting)?)
                .min(
                    self.limits
                        .max_source_operations_per_task()
                        .saturating_add(1),
                );
        }
        Ok(total)
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

    fn first_reference(&self, source_path: &str, node: &AstNode) -> Option<SymbolId> {
        self.references
            .get(&(source_path.to_owned(), node.span))
            .copied()
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| self.first_reference(source_path, child))
            })
    }

    fn constant_integer(&self, source: SemanticSource<'_>, node: &AstNode) -> Option<i128> {
        match node.kind {
            AstNodeKind::Literal => parse_integer(node.text.as_deref()?),
            AstNodeKind::QualifiedLiteral => self.constant_integer(source, node.children.get(1)?),
            AstNodeKind::ParenthesizedExpression => {
                self.constant_integer(source, node.children.first()?)
            }
            AstNodeKind::UnaryExpression => {
                let value = self.constant_integer(source, node.children.first()?)?;
                match node.text.as_deref()?.to_ascii_uppercase().as_str() {
                    "+" => Some(value),
                    "-" => value.checked_neg(),
                    "NOT" => evaluate_integer_operation(
                        self.integer_type(source, node)?,
                        IntegerOperation::BitwiseNot,
                        None,
                        value,
                        None,
                    )
                    .ok(),
                    _ => None,
                }
            }
            AstNodeKind::BinaryExpression => {
                let left = self.constant_integer(source, node.children.first()?)?;
                let right = self.constant_integer(source, node.children.get(1)?)?;
                match node.text.as_deref()?.to_ascii_uppercase().as_str() {
                    "+" => left.checked_add(right),
                    "-" => left.checked_sub(right),
                    "*" => left.checked_mul(right),
                    "/" => left.checked_div(right),
                    "MOD" => left.checked_rem(right),
                    "AND" => {
                        self.fixed_bitwise(source, node, IntegerOperation::BitwiseAnd, left, right)
                    }
                    "OR" => {
                        self.fixed_bitwise(source, node, IntegerOperation::BitwiseOr, left, right)
                    }
                    "XOR" => {
                        self.fixed_bitwise(source, node, IntegerOperation::BitwiseXor, left, right)
                    }
                    _ => None,
                }
            }
            AstNodeKind::CallExpression => self.constant_integer_call(source, node),
            _ => None,
        }
    }

    fn fixed_bitwise(
        &self,
        source: SemanticSource<'_>,
        node: &AstNode,
        operation: IntegerOperation,
        left: i128,
        right: i128,
    ) -> Option<i128> {
        evaluate_integer_operation(
            self.integer_type(source, node)?,
            operation,
            None,
            left,
            Some(right),
        )
        .ok()
    }

    fn constant_integer_call(&self, source: SemanticSource<'_>, node: &AstNode) -> Option<i128> {
        let name = node
            .children
            .first()?
            .children
            .first()?
            .text
            .as_deref()?
            .to_ascii_uppercase();
        let arguments = node
            .children
            .iter()
            .skip(1)
            .map(|argument| self.constant_integer(source, argument))
            .collect::<Option<Vec<_>>>()?;
        match (name.as_str(), arguments.as_slice()) {
            ("MIN", [left, right]) => Some((*left).min(*right)),
            ("MAX", [left, right]) => Some((*left).max(*right)),
            ("LIMIT", [value, low, high]) if low <= high => Some((*value).clamp(*low, *high)),
            (name, [value]) if name.starts_with("TO_") => Some(*value),
            _ => {
                let (operation, mode) = integer_standard_operation(&name)?;
                evaluate_integer_operation(
                    self.integer_type(source, node)?,
                    operation,
                    mode,
                    *arguments.first()?,
                    arguments.get(1).copied(),
                )
                .ok()
            }
        }
    }

    fn integer_type(&self, source: SemanticSource<'_>, node: &AstNode) -> Option<IntegerType> {
        integer_type(
            self.expression_types
                .get(&(source.ast.source_path.clone(), node.span))?,
        )
    }

    fn source(&self, path: &str) -> Option<SemanticSource<'a>> {
        self.sources
            .iter()
            .copied()
            .find(|source| source.ast.source_path == path)
    }

    fn emit(&mut self, source: SemanticSource<'_>, code: DiagnosticCode, span: SourceSpan) {
        self.diagnostics.push(make_diagnostic(
            &source.ast.source_path,
            source.source,
            code,
            span,
        ));
    }
}

fn prepare_sources<'a>(
    sources: &[SemanticSource<'a>],
    symbols: &[SemanticSymbol],
) -> Result<Vec<SemanticSource<'a>>, CyclicWorkInputError> {
    let mut ordered = sources.to_vec();
    ordered.sort_by(|left, right| {
        left.ast
            .source_path
            .as_bytes()
            .cmp(right.ast.source_path.as_bytes())
    });
    for pair in ordered.windows(2) {
        if pair[0].ast.source_path == pair[1].ast.source_path {
            return Err(CyclicWorkInputError::InvalidSourceSet(
                pair[0].ast.source_path.clone(),
            ));
        }
    }
    let paths = ordered
        .iter()
        .map(|source| source.ast.source_path.as_str())
        .collect::<BTreeSet<_>>();
    if let Some(symbol) = symbols
        .iter()
        .find(|symbol| !paths.contains(symbol.source_path.as_str()))
    {
        return Err(CyclicWorkInputError::InvalidSourceSet(
            symbol.source_path.clone(),
        ));
    }
    Ok(ordered)
}

fn semantic_maps(model: &AddressSemanticModel) -> (SymbolMap<'_>, ReferenceMap, ExpressionTypeMap) {
    let semantics = &model.faults.fixed.semantics;
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
    let expression_types = semantics
        .expressions
        .iter()
        .map(|expression| {
            (
                (expression.source_path.clone(), expression.span),
                expression.value_type.clone(),
            )
        })
        .collect();
    (symbols, references, expression_types)
}

fn prepare_tasks(
    program_tasks: &[ProgramTaskBinding],
    limits: CyclicWorkLimits,
    symbols: &SymbolMap<'_>,
) -> Result<(Vec<ProgramTaskBinding>, bool), CyclicWorkInputError> {
    let limit_exceeded = program_tasks.len() > limits.max_tasks();
    let mut tasks = if limit_exceeded {
        Vec::new()
    } else {
        program_tasks.to_vec()
    };
    tasks.sort_by_key(|binding| binding.task_handle);
    if let Some(pair) = tasks
        .windows(2)
        .find(|pair| pair[0].task_handle == pair[1].task_handle)
    {
        return Err(CyclicWorkInputError::DuplicateTask(pair[0].task_handle.0));
    }
    if let Some(binding) = tasks.iter().find(|binding| {
        !symbols
            .get(&binding.program)
            .is_some_and(|symbol| symbol.kind == SemanticSymbolKind::Program)
    }) {
        return Err(CyclicWorkInputError::InvalidProgramTask {
            program: binding.program.0,
            task: binding.task_handle.0,
        });
    }
    Ok((tasks, limit_exceeded))
}

fn collect_pous<'a>(
    sources: &[SemanticSource<'a>],
    symbols: &SymbolMap<'_>,
) -> Result<PouMap<'a>, CyclicWorkInputError> {
    let mut pous = BTreeMap::new();
    for source in sources {
        for node in &source.ast.root.children {
            let Some(kind) = pou_kind(node.kind) else {
                continue;
            };
            let identifier = node
                .children
                .iter()
                .find(|child| child.kind == AstNodeKind::Identifier)
                .ok_or_else(|| invalid_shape(*source, node.span))?;
            let symbol = symbols
                .values()
                .find(|symbol| {
                    symbol.source_path == source.ast.source_path
                        && symbol.span == identifier.span
                        && symbol.kind == kind
                })
                .map(|symbol| symbol.id)
                .ok_or_else(|| invalid_shape(*source, identifier.span))?;
            pous.insert(symbol, (*source, node));
        }
    }
    Ok(pous)
}

fn exact_iterations(initial: i128, end: i128, step: i128) -> Option<u64> {
    let distance = if step > 0 {
        if initial > end {
            return Some(0);
        }
        end.checked_sub(initial)?
    } else {
        if initial < end {
            return Some(0);
        }
        initial.checked_sub(end)?
    };
    let magnitude = step.checked_abs()?;
    let iterations = distance.checked_div(magnitude)?.checked_add(1)?;
    u64::try_from(iterations).ok()
}

fn bounded_sum<const N: usize>(values: [u64; N], maximum: u64) -> u64 {
    let ceiling = maximum.saturating_add(1);
    values.into_iter().fold(0_u64, |total, value| {
        total.saturating_add(value).min(ceiling)
    })
}

const fn pou_kind(kind: AstNodeKind) -> Option<SemanticSymbolKind> {
    match kind {
        AstNodeKind::FunctionDeclaration => Some(SemanticSymbolKind::Function),
        AstNodeKind::FunctionBlockDeclaration => Some(SemanticSymbolKind::FunctionBlock),
        AstNodeKind::ProgramDeclaration => Some(SemanticSymbolKind::Program),
        _ => None,
    }
}

const fn is_semantic_operation(kind: AstNodeKind) -> bool {
    matches!(
        kind,
        AstNodeKind::AssignmentStatement
            | AstNodeKind::InputArgument
            | AstNodeKind::OutputArgument
            | AstNodeKind::ReturnStatement
            | AstNodeKind::UnaryExpression
            | AstNodeKind::BinaryExpression
            | AstNodeKind::Assignable
            | AstNodeKind::IndexSuffix
            | AstNodeKind::FieldSuffix
            | AstNodeKind::Literal
    )
}

const fn integer_type(value_type: &SemanticType) -> Option<IntegerType> {
    Some(match value_type {
        SemanticType::Sint => IntegerType::Sint,
        SemanticType::Int => IntegerType::Int,
        SemanticType::Dint => IntegerType::Dint,
        SemanticType::Lint => IntegerType::Lint,
        SemanticType::Usint => IntegerType::Usint,
        SemanticType::Uint => IntegerType::Uint,
        SemanticType::Udint => IntegerType::Udint,
        SemanticType::Ulint => IntegerType::Ulint,
        _ => return None,
    })
}

fn integer_standard_operation(
    name: &str,
) -> Option<(IntegerOperation, Option<IntegerArithmeticMode>)> {
    if name == "ABS" {
        return Some((IntegerOperation::Absolute, None));
    }
    let (prefix, suffix) = name.rsplit_once('_')?;
    let operation = match suffix {
        "ADD" => IntegerOperation::Add,
        "SUB" => IntegerOperation::Subtract,
        "MUL" => IntegerOperation::Multiply,
        "NEG" => IntegerOperation::Negate,
        _ => return None,
    };
    let mode = match prefix {
        "CHECKED" => IntegerArithmeticMode::Checked,
        "SATURATING" => IntegerArithmeticMode::Saturating,
        "WRAPPING" => IntegerArithmeticMode::Wrapping,
        _ => return None,
    };
    Some((operation, Some(mode)))
}

fn parse_integer(text: &str) -> Option<i128> {
    let (radix, digits) = if let Some(value) = text.strip_prefix("16#") {
        (16, value)
    } else if let Some(value) = text.strip_prefix("2#") {
        (2, value)
    } else {
        (10, text)
    };
    i128::from_str_radix(digits, radix).ok()
}

fn invalid_shape(source: SemanticSource<'_>, span: SourceSpan) -> CyclicWorkInputError {
    CyclicWorkInputError::InvalidAstShape {
        source_path: source.ast.source_path.clone(),
        start: span.start,
        end: span.end,
    }
}
