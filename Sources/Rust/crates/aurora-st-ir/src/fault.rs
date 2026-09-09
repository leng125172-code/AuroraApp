use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::ast::{AstNode, AstNodeKind};
use crate::diagnostic::{make_diagnostic, sort_diagnostics};
use crate::fixed::{FixedFieldLayout, FixedTypeKind};
use crate::{
    AnalysisInputError, Diagnostic, DiagnosticCode, FixedDataLimits, FixedSemanticModel,
    FixedTypeId, SemanticSource, SemanticSymbolKind, SemanticType, SourceSpan, SymbolId,
    analyze_fixed,
};

/// Stable Preview 1.0 runtime Fault outcome emitted by one ST operation site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum RuntimeFaultCode {
    /// Checked/ABS/division integer overflow.
    #[serde(rename = "STF0001")]
    IntegerOverflow,
    /// Integer division or modulo by zero.
    #[serde(rename = "STF0002")]
    IntegerDivisionByZero,
    /// Non-finite float result or invalid float domain.
    #[serde(rename = "STF0003")]
    NonFiniteFloat,
    /// Runtime numeric conversion or LIMIT range is invalid.
    #[serde(rename = "STF0004")]
    InvalidRuntimeRange,
    /// Dynamic ARRAY index is outside its declared inclusive bounds.
    #[serde(rename = "STF0005")]
    ArrayIndexOutOfBounds,
    /// STRING/WSTRING result exceeds its fixed destination capacity.
    #[serde(rename = "STF0006")]
    StringCapacityExceeded,
}

impl RuntimeFaultCode {
    /// Returns the frozen textual site code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IntegerOverflow => "STF0001",
            Self::IntegerDivisionByZero => "STF0002",
            Self::NonFiniteFloat => "STF0003",
            Self::InvalidRuntimeRange => "STF0004",
            Self::ArrayIndexOutOfBounds => "STF0005",
            Self::StringCapacityExceeded => "STF0006",
        }
    }
}

/// Operation category retained for later Canonical IR lowering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultOperationKind {
    /// `CHECKED_*` integer arithmetic.
    CheckedInteger,
    /// Integer division, including its overflow and zero-divisor outcomes.
    IntegerDivision,
    /// Integer modulo.
    IntegerModulo,
    /// Signed integer `ABS`.
    IntegerAbsolute,
    /// Float arithmetic or a float standard function.
    FloatingPoint,
    /// A numeric `TO_*` conversion that can fail for a runtime value.
    NumericConversion,
    /// A `LIMIT` whose runtime lower/upper relationship is not proven.
    LimitRange,
    /// One dynamic ARRAY index operation.
    ArrayIndex,
    /// One fixed-capacity CONCAT operation.
    StringCapacity,
}

/// Stable, collision-free identity of one faultable source operation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct FaultSiteId {
    /// Normalized project-relative source path.
    pub source_path: String,
    /// Exact source operation span.
    pub span: SourceSpan,
    /// Operation category that distinguishes otherwise equal source locations.
    pub operation: FaultOperationKind,
}

/// One source operation and its complete bounded set of possible runtime Fault outcomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FaultSite {
    /// Identity used for deterministic ordering and later Source Map lookup.
    pub id: FaultSiteId,
    /// Sorted, duplicate-free outcomes for this one site; never empty or unbounded.
    pub possible_faults: Vec<RuntimeFaultCode>,
}

/// Complete R1-04 model layered over the successful fixed-data model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FaultSemanticModel {
    /// R1-03 semantic and fixed-layout model.
    pub fixed: FixedSemanticModel,
    /// Fault sites in deterministic identity order.
    pub fault_sites: Vec<FaultSite>,
}

/// Atomic R1-04 analysis result; diagnostics and a model are mutually exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultAnalysisOutput {
    /// Complete model, present only when every compile-time boundary is valid.
    pub model: Option<FaultSemanticModel>,
    /// Stable diagnostics sorted by path/byte/code.
    pub diagnostics: Vec<Diagnostic>,
}

/// Fixed-width integer type used by the shared constant/runtime arithmetic policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegerType {
    /// Signed 8-bit integer.
    Sint,
    /// Signed 16-bit integer.
    Int,
    /// Signed 32-bit integer.
    Dint,
    /// Signed 64-bit integer.
    Lint,
    /// Unsigned 8-bit integer.
    Usint,
    /// Unsigned 16-bit integer.
    Uint,
    /// Unsigned 32-bit integer.
    Udint,
    /// Unsigned 64-bit integer.
    Ulint,
}

impl IntegerType {
    const fn bits(self) -> u8 {
        match self {
            Self::Sint | Self::Usint => 8,
            Self::Int | Self::Uint => 16,
            Self::Dint | Self::Udint => 32,
            Self::Lint | Self::Ulint => 64,
        }
    }

    const fn signed(self) -> bool {
        matches!(self, Self::Sint | Self::Int | Self::Dint | Self::Lint)
    }

    const fn bounds(self) -> (i128, i128) {
        let bits = self.bits();
        if self.signed() {
            let magnitude = 1_i128 << (bits - 1);
            (-magnitude, magnitude - 1)
        } else {
            (0, (1_i128 << bits) - 1)
        }
    }

    const fn contains(self, value: i128) -> bool {
        let (minimum, maximum) = self.bounds();
        value >= minimum && value <= maximum
    }
}

/// Integer operation with semantics frozen by Preview 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegerOperation {
    /// Addition.
    Add,
    /// Subtraction.
    Subtract,
    /// Multiplication.
    Multiply,
    /// Unary negation.
    Negate,
    /// Division truncated toward zero.
    Divide,
    /// Remainder with the dividend's sign.
    Modulo,
    /// Signed absolute value.
    Absolute,
    /// Bitwise AND.
    BitwiseAnd,
    /// Bitwise OR.
    BitwiseOr,
    /// Bitwise XOR.
    BitwiseXor,
    /// Fixed-width bitwise complement.
    BitwiseNot,
}

/// Overflow behavior selected for `+`, `-`, `*`, and unary negation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegerArithmeticMode {
    /// Overflow becomes `STF0001`.
    Checked,
    /// Overflow clamps to the target minimum or maximum.
    Saturating,
    /// The low N bits are retained and signed values use two's-complement interpretation.
    Wrapping,
}

/// Invalid generated-operation input or a defined runtime Fault outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum IntegerArithmeticError {
    /// An operand cannot be represented by the declared fixed-width type.
    #[error("integer operand is outside its declared type")]
    OperandOutOfRange,
    /// A binary operation did not receive its right operand.
    #[error("binary integer operation requires a right operand")]
    MissingRightOperand,
    /// A unary operation received an unexpected right operand.
    #[error("unary integer operation does not accept a right operand")]
    UnexpectedRightOperand,
    /// An arithmetic operation did not select its required overflow mode.
    #[error("integer arithmetic operation requires an overflow mode")]
    ModeRequired,
    /// An operation with fixed behavior incorrectly supplied an overflow mode.
    #[error("integer division, modulo, ABS, and bitwise operations do not accept an overflow mode")]
    ModeNotAllowed,
    /// A valid operation produced its specified runtime Fault.
    #[error("integer operation produced runtime Fault {0:?}")]
    RuntimeFault(RuntimeFaultCode),
}

/// Evaluates the fixed integer policy shared by compile-time folding and later executors.
///
/// `right` is required only for binary operations. `mode` is required only for add, subtract,
/// multiply, and negate. Inputs are validated before arithmetic, so an invalid generated operation
/// cannot be mistaken for a program Fault.
///
/// # Errors
///
/// Returns [`IntegerArithmeticError`] for an invalid operation shape/input or for the exact runtime
/// Fault defined by Preview 1.0.
pub fn evaluate_integer_operation(
    value_type: IntegerType,
    operation: IntegerOperation,
    mode: Option<IntegerArithmeticMode>,
    left: i128,
    right: Option<i128>,
) -> Result<i128, IntegerArithmeticError> {
    if !value_type.contains(left) || right.is_some_and(|value| !value_type.contains(value)) {
        return Err(IntegerArithmeticError::OperandOutOfRange);
    }
    let binary = matches!(
        operation,
        IntegerOperation::Add
            | IntegerOperation::Subtract
            | IntegerOperation::Multiply
            | IntegerOperation::Divide
            | IntegerOperation::Modulo
            | IntegerOperation::BitwiseAnd
            | IntegerOperation::BitwiseOr
            | IntegerOperation::BitwiseXor
    );
    let right = match (binary, right) {
        (true, Some(value)) => Some(value),
        (true, None) => return Err(IntegerArithmeticError::MissingRightOperand),
        (false, Some(_)) => return Err(IntegerArithmeticError::UnexpectedRightOperand),
        (false, None) => None,
    };
    let uses_mode = matches!(
        operation,
        IntegerOperation::Add
            | IntegerOperation::Subtract
            | IntegerOperation::Multiply
            | IntegerOperation::Negate
    );
    if uses_mode && mode.is_none() {
        return Err(IntegerArithmeticError::ModeRequired);
    }
    if !uses_mode && mode.is_some() {
        return Err(IntegerArithmeticError::ModeNotAllowed);
    }

    if operation == IntegerOperation::Divide || operation == IntegerOperation::Modulo {
        let divisor = right.ok_or(IntegerArithmeticError::MissingRightOperand)?;
        if divisor == 0 {
            return Err(IntegerArithmeticError::RuntimeFault(
                RuntimeFaultCode::IntegerDivisionByZero,
            ));
        }
        let (minimum, _) = value_type.bounds();
        if value_type.signed() && left == minimum && divisor == -1 {
            return if operation == IntegerOperation::Modulo {
                Ok(0)
            } else {
                Err(IntegerArithmeticError::RuntimeFault(
                    RuntimeFaultCode::IntegerOverflow,
                ))
            };
        }
        return if operation == IntegerOperation::Divide {
            Ok(left / divisor)
        } else {
            Ok(left % divisor)
        };
    }

    if operation == IntegerOperation::Absolute {
        if !value_type.signed() {
            return Err(IntegerArithmeticError::OperandOutOfRange);
        }
        let (minimum, _) = value_type.bounds();
        return if left == minimum {
            Err(IntegerArithmeticError::RuntimeFault(
                RuntimeFaultCode::IntegerOverflow,
            ))
        } else {
            Ok(left.abs())
        };
    }

    if matches!(
        operation,
        IntegerOperation::BitwiseAnd
            | IntegerOperation::BitwiseOr
            | IntegerOperation::BitwiseXor
            | IntegerOperation::BitwiseNot
    ) {
        return Ok(bitwise_integer(
            value_type,
            operation,
            left,
            right.unwrap_or(0),
        ));
    }

    let right = right.unwrap_or(0);
    match mode.ok_or(IntegerArithmeticError::ModeRequired)? {
        IntegerArithmeticMode::Checked => checked_integer(value_type, operation, left, right),
        IntegerArithmeticMode::Saturating => {
            Ok(saturating_integer(value_type, operation, left, right))
        }
        IntegerArithmeticMode::Wrapping => Ok(wrapping_integer(value_type, operation, left, right)),
    }
}

/// Checks one inclusive ARRAY bound using mathematical integer comparison.
///
/// # Errors
///
/// Returns `STF0005` when `index` lies outside `lower..=upper`.
pub const fn validate_array_index(
    index: i128,
    lower: i128,
    upper: i128,
) -> Result<(), RuntimeFaultCode> {
    if index < lower || index > upper {
        Err(RuntimeFaultCode::ArrayIndexOutOfBounds)
    } else {
        Ok(())
    }
}

/// Runs fixed-data validation followed by arithmetic, conversion, capacity, and ARRAY-index Fault
/// analysis. The pass is host-only and creates neither Canonical IR nor executable code.
///
/// Each dynamic source operation contributes at most one site. A site can list two bounded outcome
/// codes when one operation has two specified failures (for example signed integer division).
/// Compile-time failures produce one diagnostic and no site.
///
/// # Errors
///
/// Returns [`AnalysisInputError`] for corrupt or mismatched AST/source inputs.
pub fn analyze_faults(
    sources: &[SemanticSource<'_>],
    limits: FixedDataLimits,
) -> Result<FaultAnalysisOutput, AnalysisInputError> {
    let fixed_output = analyze_fixed(sources, limits)?;
    let Some(fixed) = fixed_output.model else {
        return Ok(FaultAnalysisOutput {
            model: None,
            diagnostics: fixed_output.diagnostics,
        });
    };
    FaultAnalyzer::new(sources, fixed)?.run()
}

fn checked_operation_can_fault(
    operation: IntegerOperation,
    left: Option<i128>,
    right: ConstantRightOperand,
) -> bool {
    match operation {
        IntegerOperation::Add => left != Some(0) && right != ConstantRightOperand::Value(0),
        IntegerOperation::Subtract => right != ConstantRightOperand::Value(0),
        IntegerOperation::Multiply => {
            !matches!(left, Some(0 | 1)) && !matches!(right, ConstantRightOperand::Value(0 | 1))
        }
        IntegerOperation::Negate => true,
        IntegerOperation::Divide
        | IntegerOperation::Modulo
        | IntegerOperation::Absolute
        | IntegerOperation::BitwiseAnd
        | IntegerOperation::BitwiseOr
        | IntegerOperation::BitwiseXor
        | IntegerOperation::BitwiseNot => false,
    }
}

fn checked_integer(
    value_type: IntegerType,
    operation: IntegerOperation,
    left: i128,
    right: i128,
) -> Result<i128, IntegerArithmeticError> {
    let value = match operation {
        IntegerOperation::Add => left.checked_add(right),
        IntegerOperation::Subtract => left.checked_sub(right),
        IntegerOperation::Multiply => left.checked_mul(right),
        IntegerOperation::Negate => left.checked_neg(),
        IntegerOperation::Divide
        | IntegerOperation::Modulo
        | IntegerOperation::Absolute
        | IntegerOperation::BitwiseAnd
        | IntegerOperation::BitwiseOr
        | IntegerOperation::BitwiseXor
        | IntegerOperation::BitwiseNot => None,
    };
    value
        .filter(|value| value_type.contains(*value))
        .ok_or(IntegerArithmeticError::RuntimeFault(
            RuntimeFaultCode::IntegerOverflow,
        ))
}

fn saturating_integer(
    value_type: IntegerType,
    operation: IntegerOperation,
    left: i128,
    right: i128,
) -> i128 {
    let (minimum, maximum) = value_type.bounds();
    let value = match operation {
        IntegerOperation::Add => left.saturating_add(right),
        IntegerOperation::Subtract => left.saturating_sub(right),
        IntegerOperation::Multiply => left.saturating_mul(right),
        IntegerOperation::Negate => left.saturating_neg(),
        IntegerOperation::Divide
        | IntegerOperation::Modulo
        | IntegerOperation::Absolute
        | IntegerOperation::BitwiseAnd
        | IntegerOperation::BitwiseOr
        | IntegerOperation::BitwiseXor
        | IntegerOperation::BitwiseNot => left,
    };
    value.clamp(minimum, maximum)
}

fn wrapping_integer(
    value_type: IntegerType,
    operation: IntegerOperation,
    left: i128,
    right: i128,
) -> i128 {
    let modulus = 1_u128 << value_type.bits();
    let mask = modulus - 1;
    let signed_modulus = 1_i128 << value_type.bits();
    let left = nonnegative_i128_to_u128(left.rem_euclid(signed_modulus));
    let right = nonnegative_i128_to_u128(right.rem_euclid(signed_modulus));
    let bits = match operation {
        IntegerOperation::Add => left.wrapping_add(right) & mask,
        IntegerOperation::Subtract => left.wrapping_sub(right) & mask,
        IntegerOperation::Multiply => left.wrapping_mul(right) & mask,
        IntegerOperation::Negate => 0_u128.wrapping_sub(left) & mask,
        IntegerOperation::Divide
        | IntegerOperation::Modulo
        | IntegerOperation::Absolute
        | IntegerOperation::BitwiseAnd
        | IntegerOperation::BitwiseOr
        | IntegerOperation::BitwiseXor
        | IntegerOperation::BitwiseNot => left,
    };
    let sign_bit = modulus / 2;
    let negative = value_type.signed() && bits >= sign_bit;
    let bits = low_u64_bits_to_i128(bits);
    if negative {
        bits - signed_modulus
    } else {
        bits
    }
}

fn bitwise_integer(
    value_type: IntegerType,
    operation: IntegerOperation,
    left: i128,
    right: i128,
) -> i128 {
    let modulus = 1_u128 << value_type.bits();
    let mask = modulus - 1;
    let signed_modulus = 1_i128 << value_type.bits();
    let left = nonnegative_i128_to_u128(left.rem_euclid(signed_modulus));
    let right = nonnegative_i128_to_u128(right.rem_euclid(signed_modulus));
    let bits = match operation {
        IntegerOperation::BitwiseAnd => left & right,
        IntegerOperation::BitwiseOr => left | right,
        IntegerOperation::BitwiseXor => left ^ right,
        IntegerOperation::BitwiseNot => !left & mask,
        IntegerOperation::Add
        | IntegerOperation::Subtract
        | IntegerOperation::Multiply
        | IntegerOperation::Negate
        | IntegerOperation::Divide
        | IntegerOperation::Modulo
        | IntegerOperation::Absolute => left,
    };
    let sign_bit = modulus / 2;
    let negative = value_type.signed() && bits >= sign_bit;
    let bits = low_u64_bits_to_i128(bits);
    if negative {
        bits - signed_modulus
    } else {
        bits
    }
}

// `rem_euclid(2^N)` is non-negative and N is at most 64 by `IntegerType` construction.
#[allow(clippy::cast_sign_loss)]
const fn nonnegative_i128_to_u128(value: i128) -> u128 {
    value as u128
}

// The mask applied by `wrapping_integer` limits this value to at most `u64::MAX`.
#[allow(clippy::cast_possible_wrap)]
const fn low_u64_bits_to_i128(value: u128) -> i128 {
    value as i128
}

#[derive(Debug, Clone, PartialEq)]
enum ConstantValue {
    Integer(i128),
    UntypedReal(u64),
    Real(u32),
    Lreal(u64),
    String { wide: bool, units: u64 },
    Other,
}

#[derive(Debug, Clone, PartialEq)]
enum Evaluation {
    Constant(ConstantValue),
    Dynamic,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConstantRightOperand {
    Absent,
    Value(i128),
    Dynamic,
}

struct FaultAnalyzer<'a> {
    sources: &'a [SemanticSource<'a>],
    fixed: FixedSemanticModel,
    expression_types: BTreeMap<(usize, SourceSpan), SemanticType>,
    references: BTreeMap<(usize, SourceSpan), SymbolId>,
    symbol_types: BTreeMap<SymbolId, FixedTypeId>,
    diagnostics: Vec<Diagnostic>,
    sites: BTreeMap<FaultSiteId, BTreeSet<RuntimeFaultCode>>,
}

impl<'a> FaultAnalyzer<'a> {
    fn new(
        sources: &'a [SemanticSource<'a>],
        fixed: FixedSemanticModel,
    ) -> Result<Self, AnalysisInputError> {
        let source_by_path = sources
            .iter()
            .enumerate()
            .map(|(index, source)| (source.ast.source_path.clone(), index))
            .collect::<BTreeMap<_, _>>();
        let mut expression_types = BTreeMap::new();
        for expression in &fixed.semantics.expressions {
            let Some(source_index) = source_by_path.get(&expression.source_path).copied() else {
                return Err(AnalysisInputError::InvalidAstShape {
                    source_path: expression.source_path.clone(),
                    span_start: expression.span.start,
                    span_end: expression.span.end,
                });
            };
            expression_types.insert(
                (source_index, expression.span),
                expression.value_type.clone(),
            );
        }
        let mut references = BTreeMap::new();
        for reference in &fixed.semantics.references {
            let Some(source_index) = source_by_path.get(&reference.source_path).copied() else {
                return Err(AnalysisInputError::InvalidAstShape {
                    source_path: reference.source_path.clone(),
                    span_start: reference.span.start,
                    span_end: reference.span.end,
                });
            };
            references.insert((source_index, reference.span), reference.symbol);
        }
        let symbol_types = collect_symbol_types(&fixed, &source_by_path);
        Ok(Self {
            sources,
            fixed,
            expression_types,
            references,
            symbol_types,
            diagnostics: Vec::new(),
            sites: BTreeMap::new(),
        })
    }

    fn run(mut self) -> Result<FaultAnalysisOutput, AnalysisInputError> {
        for source_index in 0..self.sources.len() {
            self.visit_tree(source_index, &self.sources[source_index].ast.root)?;
        }
        sort_diagnostics(&mut self.diagnostics);
        if !self.diagnostics.is_empty() {
            return Ok(FaultAnalysisOutput {
                model: None,
                diagnostics: self.diagnostics,
            });
        }

        let mut fault_sites = Vec::with_capacity(self.sites.len());
        for (id, possible_faults) in self.sites {
            fault_sites.push(FaultSite {
                id,
                possible_faults: possible_faults.into_iter().collect(),
            });
        }
        Ok(FaultAnalysisOutput {
            model: Some(FaultSemanticModel {
                fixed: self.fixed,
                fault_sites,
            }),
            diagnostics: Vec::new(),
        })
    }

    fn visit_tree(
        &mut self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<(), AnalysisInputError> {
        if is_expression(node.kind) {
            let _ = self.expression(source_index, node)?;
            return Ok(());
        }
        for child in &node.children {
            self.visit_tree(source_index, child)?;
        }
        Ok(())
    }

    fn expression(
        &mut self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<Evaluation, AnalysisInputError> {
        match node.kind {
            AstNodeKind::Literal | AstNodeKind::QualifiedLiteral => {
                self.literal(source_index, node)
            }
            AstNodeKind::Assignable => self.assignable(source_index, node),
            AstNodeKind::ParenthesizedExpression => {
                let child = self.child(source_index, node, 0)?;
                self.expression(source_index, child)
            }
            AstNodeKind::UnaryExpression => self.unary(source_index, node),
            AstNodeKind::BinaryExpression => self.binary(source_index, node),
            AstNodeKind::CallExpression => self.call(source_index, node),
            _ => Err(self.invalid_shape(source_index, node.span)),
        }
    }

    fn literal(
        &mut self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<Evaluation, AnalysisInputError> {
        let value_node = if node.kind == AstNodeKind::QualifiedLiteral {
            self.child(source_index, node, 1)?
        } else {
            node
        };
        let (value_node, negative) = if value_node.kind == AstNodeKind::UnaryExpression {
            (
                self.child(source_index, value_node, 0)?,
                value_node.text.as_deref() == Some("-"),
            )
        } else {
            (value_node, false)
        };
        let text = self.text(source_index, value_node)?;
        let value_type = self.expression_type(source_index, node).cloned();
        if let Some(integer_type) = value_type.as_ref().and_then(integer_type) {
            let Some(mut value) = parse_integer(text) else {
                return Ok(Evaluation::Invalid);
            };
            if negative {
                let Some(negated) = value.checked_neg() else {
                    self.emit(source_index, DiagnosticCode::ConstantOverflow, node.span);
                    return Ok(Evaluation::Invalid);
                };
                value = negated;
            }
            if !integer_type.contains(value) {
                self.emit(
                    source_index,
                    DiagnosticCode::InvalidExplicitConversion,
                    node.span,
                );
                return Ok(Evaluation::Invalid);
            }
            return Ok(Evaluation::Constant(ConstantValue::Integer(value)));
        }
        if matches!(
            value_type.as_ref(),
            Some(SemanticType::Real | SemanticType::Lreal)
        ) {
            let spelling = if negative {
                format!("-{text}")
            } else {
                text.to_owned()
            };
            let result = match value_type.as_ref() {
                Some(SemanticType::Real) => spelling
                    .parse::<f32>()
                    .ok()
                    .filter(|value| value.is_finite())
                    .map(|value| ConstantValue::Real(value.to_bits())),
                Some(SemanticType::Lreal) => spelling
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())
                    .map(|value| ConstantValue::Lreal(value.to_bits())),
                _ => None,
            };
            return Ok(result.map_or_else(
                || {
                    self.emit(source_index, DiagnosticCode::NonFiniteConstant, node.span);
                    Evaluation::Invalid
                },
                Evaluation::Constant,
            ));
        }
        if let Some((wide, capacity)) = value_type.as_ref().and_then(string_type) {
            let Some(units) = string_units(text, wide) else {
                return Ok(Evaluation::Invalid);
            };
            if units > capacity {
                self.emit(
                    source_index,
                    DiagnosticCode::InvalidExplicitConversion,
                    node.span,
                );
                return Ok(Evaluation::Invalid);
            }
            return Ok(Evaluation::Constant(ConstantValue::String { wide, units }));
        }
        Ok(self.untyped_literal(source_index, node.span, value_node.kind, text, negative))
    }

    fn untyped_literal(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        kind: AstNodeKind,
        text: &str,
        negative: bool,
    ) -> Evaluation {
        if text.starts_with('\'') || text.starts_with('"') {
            let wide = text.starts_with('"');
            let Some(units) = string_units(text, wide) else {
                return Evaluation::Invalid;
            };
            return Evaluation::Constant(ConstantValue::String { wide, units });
        }
        if text.contains('.') {
            let spelling = if negative {
                format!("-{text}")
            } else {
                text.to_owned()
            };
            let Some(value) = spelling
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
            else {
                self.emit(source_index, DiagnosticCode::NonFiniteConstant, span);
                return Evaluation::Invalid;
            };
            return Evaluation::Constant(ConstantValue::UntypedReal(value.to_bits()));
        }
        if kind == AstNodeKind::Literal
            && let Some(mut value) = parse_integer(text)
        {
            if negative {
                let Some(negated) = value.checked_neg() else {
                    return Evaluation::Invalid;
                };
                value = negated;
            }
            return Evaluation::Constant(ConstantValue::Integer(value));
        }
        Evaluation::Constant(ConstantValue::Other)
    }

    fn unary(
        &mut self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<Evaluation, AnalysisInputError> {
        let operand_node = self.child(source_index, node, 0)?;
        let operand = self.expression(source_index, operand_node)?;
        let operator = self.text(source_index, node)?.to_ascii_uppercase();
        let value_type = self.expression_type(source_index, node);
        if operator == "-"
            && let Some(integer_type) = value_type.and_then(integer_type)
        {
            return self.constant_or_dynamic_integer(
                source_index,
                node.span,
                integer_type,
                IntegerOperation::Negate,
                IntegerArithmeticMode::Checked,
                &operand,
                None,
                true,
            );
        }
        if operator == "NOT"
            && let Some(integer_type) = value_type.and_then(integer_type)
        {
            return self.constant_or_dynamic_integer(
                source_index,
                node.span,
                integer_type,
                IntegerOperation::BitwiseNot,
                IntegerArithmeticMode::Checked,
                &operand,
                None,
                false,
            );
        }
        if operator == "-" && is_float(value_type) {
            return match operand {
                Evaluation::Constant(ConstantValue::Real(bits)) => Ok(Evaluation::Constant(
                    ConstantValue::Real(bits ^ (1_u32 << 31)),
                )),
                Evaluation::Constant(ConstantValue::Lreal(bits)) => Ok(Evaluation::Constant(
                    ConstantValue::Lreal(bits ^ (1_u64 << 63)),
                )),
                Evaluation::Constant(ConstantValue::UntypedReal(bits)) => {
                    let value = f64::from_bits(bits);
                    match value_type {
                        Some(SemanticType::Real) => {
                            let value = lreal_to_real(value);
                            Ok(self.finite_constant(
                                source_index,
                                node.span,
                                value
                                    .is_finite()
                                    .then(|| ConstantValue::Real(value.to_bits() ^ (1_u32 << 31))),
                            ))
                        }
                        Some(SemanticType::Lreal) => Ok(Evaluation::Constant(
                            ConstantValue::Lreal(bits ^ (1_u64 << 63)),
                        )),
                        _ => Ok(Evaluation::Invalid),
                    }
                }
                Evaluation::Constant(_) | Evaluation::Invalid => Ok(Evaluation::Invalid),
                Evaluation::Dynamic => {
                    self.add_site(
                        source_index,
                        node.span,
                        FaultOperationKind::FloatingPoint,
                        [RuntimeFaultCode::NonFiniteFloat],
                    );
                    Ok(Evaluation::Dynamic)
                }
            };
        }
        Ok(operand)
    }

    fn binary(
        &mut self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<Evaluation, AnalysisInputError> {
        let left_node = self.child(source_index, node, 0)?;
        let right_node = self.child(source_index, node, 1)?;
        let left = self.expression(source_index, left_node)?;
        let right = self.expression(source_index, right_node)?;
        if left == Evaluation::Invalid || right == Evaluation::Invalid {
            return Ok(Evaluation::Invalid);
        }
        let operator = self.text(source_index, node)?.to_ascii_uppercase();
        let value_type = self.expression_type(source_index, node).cloned();
        if let Some(integer_type) = value_type.as_ref().and_then(integer_type) {
            let operation = match operator.as_str() {
                "+" => Some(IntegerOperation::Add),
                "-" => Some(IntegerOperation::Subtract),
                "*" => Some(IntegerOperation::Multiply),
                "/" => Some(IntegerOperation::Divide),
                "MOD" => Some(IntegerOperation::Modulo),
                "AND" => Some(IntegerOperation::BitwiseAnd),
                "OR" => Some(IntegerOperation::BitwiseOr),
                "XOR" => Some(IntegerOperation::BitwiseXor),
                _ => None,
            };
            if let Some(operation) = operation {
                let ordinary = matches!(
                    operation,
                    IntegerOperation::Add | IntegerOperation::Subtract | IntegerOperation::Multiply
                );
                return self.constant_or_dynamic_integer(
                    source_index,
                    node.span,
                    integer_type,
                    operation,
                    IntegerArithmeticMode::Checked,
                    &left,
                    Some(&right),
                    ordinary,
                );
            }
        }
        if is_float(value_type.as_ref()) && matches!(operator.as_str(), "+" | "-" | "*" | "/") {
            return Ok(self.float_binary(
                source_index,
                node.span,
                &operator,
                left,
                right,
                value_type.as_ref(),
            ));
        }
        if matches!(left, Evaluation::Constant(_)) && matches!(right, Evaluation::Constant(_)) {
            Ok(Evaluation::Constant(ConstantValue::Other))
        } else {
            Ok(Evaluation::Dynamic)
        }
    }

    fn call(
        &mut self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<Evaluation, AnalysisInputError> {
        let name = self.child(source_index, node, 0)?;
        let identifier = self.child(source_index, name, 0)?;
        let upper = self.text(source_index, identifier)?.to_ascii_uppercase();
        let mut arguments = Vec::with_capacity(node.children.len().saturating_sub(1));
        for argument in node.children.iter().skip(1) {
            arguments.push(self.expression(source_index, argument)?);
        }
        if arguments.contains(&Evaluation::Invalid) {
            return Ok(Evaluation::Invalid);
        }
        let value_type = self.expression_type(source_index, node).cloned();
        if let Some(target) = conversion_target(&upper) {
            let source_type = node
                .children
                .get(1)
                .and_then(|argument| self.expression_type(source_index, argument))
                .cloned();
            return self.conversion(
                source_index,
                node.span,
                &target,
                source_type.as_ref(),
                &arguments,
            );
        }
        if let Some((operation, mode)) = integer_standard_operation(&upper)
            && let Some(integer_type) = value_type.as_ref().and_then(integer_type)
        {
            let Some(left) = arguments.first() else {
                return Err(self.invalid_shape(source_index, node.span));
            };
            let right = arguments.get(1);
            return self.constant_or_dynamic_integer(
                source_index,
                node.span,
                integer_type,
                operation,
                mode.unwrap_or(IntegerArithmeticMode::Checked),
                left,
                right,
                false,
            );
        }
        match upper.as_str() {
            "MIN" | "MAX" => self.min_max(
                source_index,
                node.span,
                &upper,
                value_type.as_ref(),
                &arguments,
            ),
            "LIMIT" => Ok(self.limit(source_index, node.span, value_type.as_ref(), &arguments)),
            "ABS" => Ok(self.float_abs(source_index, node.span, value_type.as_ref(), &arguments)),
            "SQRT" => Ok(self.sqrt(source_index, node.span, value_type.as_ref(), &arguments)),
            "CONCAT" => Ok(self.concat(source_index, node.span, value_type.as_ref(), &arguments)),
            _ => Ok(Evaluation::Dynamic),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn constant_or_dynamic_integer(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        value_type: IntegerType,
        operation: IntegerOperation,
        mode: IntegerArithmeticMode,
        left: &Evaluation,
        right: Option<&Evaluation>,
        ordinary_requires_constant: bool,
    ) -> Result<Evaluation, AnalysisInputError> {
        let constant_left = match left {
            Evaluation::Constant(ConstantValue::Integer(value)) => Some(*value),
            Evaluation::Constant(_) | Evaluation::Invalid => return Ok(Evaluation::Invalid),
            Evaluation::Dynamic => None,
        };
        let constant_right = match right {
            Some(Evaluation::Constant(ConstantValue::Integer(value))) => {
                ConstantRightOperand::Value(*value)
            }
            Some(Evaluation::Constant(_) | Evaluation::Invalid) => {
                return Ok(Evaluation::Invalid);
            }
            Some(Evaluation::Dynamic) => ConstantRightOperand::Dynamic,
            None => ConstantRightOperand::Absent,
        };
        if matches!(
            operation,
            IntegerOperation::Divide | IntegerOperation::Modulo
        ) && constant_right == ConstantRightOperand::Value(0)
        {
            self.emit(source_index, DiagnosticCode::ConstantDivisionByZero, span);
            return Ok(Evaluation::Invalid);
        }
        let constant_right_value = match constant_right {
            ConstantRightOperand::Absent => Some(None),
            ConstantRightOperand::Value(value) => Some(Some(value)),
            ConstantRightOperand::Dynamic => None,
        };
        if let (Some(left), Some(right)) = (constant_left, constant_right_value) {
            let selected_mode = matches!(
                operation,
                IntegerOperation::Add
                    | IntegerOperation::Subtract
                    | IntegerOperation::Multiply
                    | IntegerOperation::Negate
            )
            .then_some(mode);
            return match evaluate_integer_operation(
                value_type,
                operation,
                selected_mode,
                left,
                right,
            ) {
                Ok(value) => Ok(Evaluation::Constant(ConstantValue::Integer(value))),
                Err(IntegerArithmeticError::RuntimeFault(code)) => {
                    self.emit(
                        source_index,
                        if code == RuntimeFaultCode::IntegerDivisionByZero {
                            DiagnosticCode::ConstantDivisionByZero
                        } else {
                            DiagnosticCode::ConstantOverflow
                        },
                        span,
                    );
                    Ok(Evaluation::Invalid)
                }
                Err(_) => Err(self.invalid_shape(source_index, span)),
            };
        }

        if ordinary_requires_constant {
            self.emit(source_index, DiagnosticCode::ArithmeticModeRequired, span);
            return Ok(Evaluation::Invalid);
        }
        self.record_dynamic_integer_site(
            source_index,
            span,
            value_type,
            operation,
            mode,
            constant_left,
            constant_right,
        );
        Ok(Evaluation::Dynamic)
    }

    #[allow(clippy::too_many_arguments)]
    fn record_dynamic_integer_site(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        value_type: IntegerType,
        operation: IntegerOperation,
        mode: IntegerArithmeticMode,
        constant_left: Option<i128>,
        constant_right: ConstantRightOperand,
    ) {
        match operation {
            IntegerOperation::Add
            | IntegerOperation::Subtract
            | IntegerOperation::Multiply
            | IntegerOperation::Negate
                if mode == IntegerArithmeticMode::Checked =>
            {
                if checked_operation_can_fault(operation, constant_left, constant_right) {
                    self.add_site(
                        source_index,
                        span,
                        FaultOperationKind::CheckedInteger,
                        [RuntimeFaultCode::IntegerOverflow],
                    );
                }
            }
            IntegerOperation::Divide => {
                let mut faults = BTreeSet::new();
                if constant_right == ConstantRightOperand::Dynamic {
                    faults.insert(RuntimeFaultCode::IntegerDivisionByZero);
                }
                let (minimum, _) = value_type.bounds();
                let left_can_be_minimum = constant_left.is_none_or(|value| value == minimum);
                let right_can_be_negative_one = matches!(
                    constant_right,
                    ConstantRightOperand::Dynamic | ConstantRightOperand::Value(-1)
                );
                if value_type.signed() && left_can_be_minimum && right_can_be_negative_one {
                    faults.insert(RuntimeFaultCode::IntegerOverflow);
                }
                if !faults.is_empty() {
                    self.add_site(
                        source_index,
                        span,
                        FaultOperationKind::IntegerDivision,
                        faults,
                    );
                }
            }
            IntegerOperation::Modulo => {
                if constant_right == ConstantRightOperand::Dynamic {
                    self.add_site(
                        source_index,
                        span,
                        FaultOperationKind::IntegerModulo,
                        [RuntimeFaultCode::IntegerDivisionByZero],
                    );
                }
            }
            IntegerOperation::Absolute => self.add_site(
                source_index,
                span,
                FaultOperationKind::IntegerAbsolute,
                [RuntimeFaultCode::IntegerOverflow],
            ),
            IntegerOperation::Add
            | IntegerOperation::Subtract
            | IntegerOperation::Multiply
            | IntegerOperation::Negate
            | IntegerOperation::BitwiseAnd
            | IntegerOperation::BitwiseOr
            | IntegerOperation::BitwiseXor
            | IntegerOperation::BitwiseNot => {}
        }
    }

    fn float_binary(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        operator: &str,
        left: Evaluation,
        right: Evaluation,
        value_type: Option<&SemanticType>,
    ) -> Evaluation {
        match (left, right, value_type) {
            (
                Evaluation::Constant(ConstantValue::Real(left)),
                Evaluation::Constant(ConstantValue::Real(right)),
                Some(SemanticType::Real),
            ) => {
                let value = float_operation_f32(operator, left, right);
                self.finite_constant(source_index, span, value.map(ConstantValue::Real))
            }
            (
                Evaluation::Constant(ConstantValue::Lreal(left)),
                Evaluation::Constant(ConstantValue::Lreal(right)),
                Some(SemanticType::Lreal),
            ) => {
                let value = float_operation_f64(operator, left, right);
                self.finite_constant(source_index, span, value.map(ConstantValue::Lreal))
            }
            (Evaluation::Constant(_), Evaluation::Constant(_), _) => Evaluation::Invalid,
            _ => {
                self.add_site(
                    source_index,
                    span,
                    FaultOperationKind::FloatingPoint,
                    [RuntimeFaultCode::NonFiniteFloat],
                );
                Evaluation::Dynamic
            }
        }
    }

    fn conversion(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        target: &SemanticType,
        source_type: Option<&SemanticType>,
        arguments: &[Evaluation],
    ) -> Result<Evaluation, AnalysisInputError> {
        let Some(argument) = arguments.first() else {
            return Err(self.invalid_shape(source_index, span));
        };
        if let Evaluation::Constant(value) = argument {
            return match convert_constant(value, target) {
                Ok(value) => Ok(Evaluation::Constant(value)),
                Err(code) => {
                    self.emit(source_index, code, span);
                    Ok(Evaluation::Invalid)
                }
            };
        }
        if conversion_can_fault(source_type, target) {
            self.add_site(
                source_index,
                span,
                FaultOperationKind::NumericConversion,
                [if is_float(Some(target)) {
                    RuntimeFaultCode::NonFiniteFloat
                } else {
                    RuntimeFaultCode::InvalidRuntimeRange
                }],
            );
        }
        Ok(Evaluation::Dynamic)
    }

    fn min_max(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        operation: &str,
        value_type: Option<&SemanticType>,
        arguments: &[Evaluation],
    ) -> Result<Evaluation, AnalysisInputError> {
        if let [Evaluation::Constant(left), Evaluation::Constant(right)] = arguments {
            return min_max_constant(operation, left, right).map_or_else(
                || Ok(Evaluation::Invalid),
                |value| Ok(self.finite_constant(source_index, span, Some(value))),
            );
        }
        if is_float(value_type) {
            self.add_site(
                source_index,
                span,
                FaultOperationKind::FloatingPoint,
                [RuntimeFaultCode::NonFiniteFloat],
            );
        }
        Ok(Evaluation::Dynamic)
    }

    fn limit(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        value_type: Option<&SemanticType>,
        arguments: &[Evaluation],
    ) -> Evaluation {
        let [value, low, high] = arguments else {
            return Evaluation::Invalid;
        };
        let constant_range = match (low, high) {
            (Evaluation::Constant(low), Evaluation::Constant(high)) => {
                ordered_less_equal(low, high)
            }
            _ => None,
        };
        if constant_range == Some(false) {
            self.emit(
                source_index,
                DiagnosticCode::InvalidExplicitConversion,
                span,
            );
            return Evaluation::Invalid;
        }
        if let (
            Evaluation::Constant(value),
            Evaluation::Constant(low),
            Evaluation::Constant(high),
        ) = (value, low, high)
        {
            let Some(lowered) = min_max_constant("MAX", value, low)
                .and_then(|value| min_max_constant("MIN", &value, high))
            else {
                return Evaluation::Invalid;
            };
            return self.finite_constant(source_index, span, Some(lowered));
        }
        if constant_range == Some(true) {
            if is_float(value_type) {
                self.add_site(
                    source_index,
                    span,
                    FaultOperationKind::FloatingPoint,
                    [RuntimeFaultCode::NonFiniteFloat],
                );
            }
            return Evaluation::Dynamic;
        }
        let mut faults = BTreeSet::from([RuntimeFaultCode::InvalidRuntimeRange]);
        if is_float(value_type) {
            faults.insert(RuntimeFaultCode::NonFiniteFloat);
        }
        self.add_site(source_index, span, FaultOperationKind::LimitRange, faults);
        Evaluation::Dynamic
    }

    fn sqrt(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        value_type: Option<&SemanticType>,
        arguments: &[Evaluation],
    ) -> Evaluation {
        match (arguments.first(), value_type) {
            (Some(Evaluation::Constant(ConstantValue::Real(bits))), Some(SemanticType::Real)) => {
                let result = f32::from_bits(*bits).sqrt();
                self.finite_constant(
                    source_index,
                    span,
                    result
                        .is_finite()
                        .then(|| ConstantValue::Real(result.to_bits())),
                )
            }
            (Some(Evaluation::Constant(ConstantValue::Lreal(bits))), Some(SemanticType::Lreal)) => {
                let result = f64::from_bits(*bits).sqrt();
                self.finite_constant(
                    source_index,
                    span,
                    result
                        .is_finite()
                        .then(|| ConstantValue::Lreal(result.to_bits())),
                )
            }
            (
                Some(Evaluation::Constant(ConstantValue::UntypedReal(bits))),
                Some(SemanticType::Real),
            ) => {
                let value = lreal_to_real(f64::from_bits(*bits));
                let result = value.sqrt();
                self.finite_constant(
                    source_index,
                    span,
                    result
                        .is_finite()
                        .then(|| ConstantValue::Real(result.to_bits())),
                )
            }
            (
                Some(Evaluation::Constant(ConstantValue::UntypedReal(bits))),
                Some(SemanticType::Lreal),
            ) => {
                let result = f64::from_bits(*bits).sqrt();
                self.finite_constant(
                    source_index,
                    span,
                    result
                        .is_finite()
                        .then(|| ConstantValue::Lreal(result.to_bits())),
                )
            }
            (Some(Evaluation::Constant(_)), _) => Evaluation::Invalid,
            _ => {
                self.add_site(
                    source_index,
                    span,
                    FaultOperationKind::FloatingPoint,
                    [RuntimeFaultCode::NonFiniteFloat],
                );
                Evaluation::Dynamic
            }
        }
    }

    fn float_abs(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        value_type: Option<&SemanticType>,
        arguments: &[Evaluation],
    ) -> Evaluation {
        match (arguments.first(), value_type) {
            (Some(Evaluation::Constant(ConstantValue::Real(bits))), Some(SemanticType::Real)) => {
                Evaluation::Constant(ConstantValue::Real(*bits & !(1_u32 << 31)))
            }
            (Some(Evaluation::Constant(ConstantValue::Lreal(bits))), Some(SemanticType::Lreal)) => {
                Evaluation::Constant(ConstantValue::Lreal(*bits & !(1_u64 << 63)))
            }
            (Some(Evaluation::Constant(_)), _) => Evaluation::Invalid,
            _ => {
                self.add_site(
                    source_index,
                    span,
                    FaultOperationKind::FloatingPoint,
                    [RuntimeFaultCode::NonFiniteFloat],
                );
                Evaluation::Dynamic
            }
        }
    }

    fn concat(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        value_type: Option<&SemanticType>,
        arguments: &[Evaluation],
    ) -> Evaluation {
        let Some((wide, capacity)) = value_type.and_then(string_type) else {
            return Evaluation::Invalid;
        };
        if let [
            Evaluation::Constant(ConstantValue::String {
                wide: left_wide,
                units: left,
            }),
            Evaluation::Constant(ConstantValue::String {
                wide: right_wide,
                units: right,
            }),
        ] = arguments
        {
            let units = left.checked_add(*right);
            if *left_wide != wide || *right_wide != wide || units.is_none_or(|sum| sum > capacity) {
                self.emit(
                    source_index,
                    DiagnosticCode::InvalidExplicitConversion,
                    span,
                );
                return Evaluation::Invalid;
            }
            return Evaluation::Constant(ConstantValue::String {
                wide,
                units: units.unwrap_or(0),
            });
        }
        if arguments.iter().any(|argument| {
            matches!(
                argument,
                Evaluation::Constant(ConstantValue::String {
                    wide: argument_wide,
                    units: 0,
                }) if *argument_wide == wide
            )
        }) {
            return Evaluation::Dynamic;
        }
        self.add_site(
            source_index,
            span,
            FaultOperationKind::StringCapacity,
            [RuntimeFaultCode::StringCapacityExceeded],
        );
        Evaluation::Dynamic
    }

    fn assignable(
        &mut self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<Evaluation, AnalysisInputError> {
        let qualified = self.child(source_index, node, 0)?;
        let root = self.child(source_index, qualified, 0)?;
        let symbol = self.references.get(&(source_index, root.span)).copied();
        let mut current = symbol.and_then(|symbol| self.symbol_types.get(&symbol).copied());
        for field in qualified.children.iter().skip(1) {
            current = current.and_then(|value_type| {
                self.field_type(value_type, field.text.as_deref().unwrap_or_default())
            });
        }
        for suffix in node.children.iter().skip(1) {
            match suffix.kind {
                AstNodeKind::FieldSuffix => {
                    let field = self.child(source_index, suffix, 0)?;
                    current = current.and_then(|value_type| {
                        self.field_type(value_type, field.text.as_deref().unwrap_or_default())
                    });
                }
                AstNodeKind::IndexSuffix => {
                    let index = self.child(source_index, suffix, 0)?;
                    let evaluation = self.expression(source_index, index)?;
                    let Some(value_type) = current else {
                        continue;
                    };
                    let Some((lower, upper, element)) = self.array_type(value_type) else {
                        continue;
                    };
                    match evaluation {
                        Evaluation::Constant(ConstantValue::Integer(value)) => {
                            if validate_array_index(value, lower, upper).is_err() {
                                self.emit(
                                    source_index,
                                    DiagnosticCode::InvalidExplicitConversion,
                                    suffix.span,
                                );
                                return Ok(Evaluation::Invalid);
                            }
                        }
                        Evaluation::Constant(_) | Evaluation::Invalid => {
                            return Ok(Evaluation::Invalid);
                        }
                        Evaluation::Dynamic => self.add_site(
                            source_index,
                            suffix.span,
                            FaultOperationKind::ArrayIndex,
                            [RuntimeFaultCode::ArrayIndexOutOfBounds],
                        ),
                    }
                    current = Some(element);
                }
                _ => return Err(self.invalid_shape(source_index, suffix.span)),
            }
        }
        Ok(Evaluation::Dynamic)
    }

    fn finite_constant(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        value: Option<ConstantValue>,
    ) -> Evaluation {
        if let Some(value) = value {
            Evaluation::Constant(value)
        } else {
            self.emit(source_index, DiagnosticCode::NonFiniteConstant, span);
            Evaluation::Invalid
        }
    }

    fn expression_type(&self, source_index: usize, node: &AstNode) -> Option<&SemanticType> {
        self.expression_types.get(&(source_index, node.span))
    }

    fn resolved_layout(&self, mut id: FixedTypeId) -> Option<&crate::FixedTypeLayout> {
        for _ in 0..=self.fixed.types.len() {
            let layout = self.fixed.types.get(usize::try_from(id.0).ok()?)?;
            if let FixedTypeKind::Alias { target } = layout.kind {
                id = target;
            } else {
                return Some(layout);
            }
        }
        None
    }

    fn field_type(&self, id: FixedTypeId, name: &str) -> Option<FixedTypeId> {
        let layout = self.resolved_layout(id)?;
        let (FixedTypeKind::Structure { fields } | FixedTypeKind::FunctionBlock { fields, .. }) =
            &layout.kind
        else {
            return None;
        };
        fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(name))
            .map(|field| field.value_type)
    }

    fn array_type(&self, id: FixedTypeId) -> Option<(i128, i128, FixedTypeId)> {
        let layout = self.resolved_layout(id)?;
        match layout.kind {
            FixedTypeKind::Array {
                lower,
                upper,
                element_type,
                ..
            } => Some((lower, upper, element_type)),
            _ => None,
        }
    }

    fn add_site(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        operation: FaultOperationKind,
        faults: impl IntoIterator<Item = RuntimeFaultCode>,
    ) {
        let entry = self
            .sites
            .entry(FaultSiteId {
                source_path: self.sources[source_index].ast.source_path.clone(),
                span,
                operation,
            })
            .or_default();
        entry.extend(faults);
    }

    fn emit(&mut self, source_index: usize, code: DiagnosticCode, span: SourceSpan) {
        self.diagnostics.push(make_diagnostic(
            &self.sources[source_index].ast.source_path,
            self.sources[source_index].source,
            code,
            span,
        ));
    }

    fn child<'n>(
        &self,
        source_index: usize,
        node: &'n AstNode,
        index: usize,
    ) -> Result<&'n AstNode, AnalysisInputError> {
        node.children
            .get(index)
            .ok_or_else(|| self.invalid_shape(source_index, node.span))
    }

    fn text<'n>(
        &self,
        source_index: usize,
        node: &'n AstNode,
    ) -> Result<&'n str, AnalysisInputError> {
        node.text
            .as_deref()
            .ok_or_else(|| self.invalid_shape(source_index, node.span))
    }

    fn invalid_shape(&self, source_index: usize, span: SourceSpan) -> AnalysisInputError {
        AnalysisInputError::InvalidAstShape {
            source_path: self.sources[source_index].ast.source_path.clone(),
            span_start: span.start,
            span_end: span.end,
        }
    }
}

fn collect_symbol_types(
    fixed: &FixedSemanticModel,
    source_by_path: &BTreeMap<String, usize>,
) -> BTreeMap<SymbolId, FixedTypeId> {
    let declarations = fixed
        .semantics
        .symbols
        .iter()
        .filter(|symbol| {
            matches!(
                symbol.kind,
                SemanticSymbolKind::InputVariable
                    | SemanticSymbolKind::OutputVariable
                    | SemanticSymbolKind::LocalVariable
                    | SemanticSymbolKind::TemporaryVariable
            )
        })
        .filter_map(|symbol| {
            source_by_path
                .get(&symbol.source_path)
                .map(|source| ((*source, symbol.span), symbol.id))
        })
        .collect::<BTreeMap<_, _>>();
    let mut result = fixed
        .globals
        .iter()
        .map(|global| (global.global, global.value_type))
        .collect::<BTreeMap<_, _>>();
    let mut register = |source_path: &str, fields: &[FixedFieldLayout]| {
        let Some(source_index) = source_by_path.get(source_path).copied() else {
            return;
        };
        for field in fields {
            if let Some(symbol) = declarations.get(&(source_index, field.span)) {
                result.insert(*symbol, field.value_type);
            }
        }
    };
    for layout in &fixed.types {
        match &layout.kind {
            FixedTypeKind::Structure { fields } => register(&layout.source_path, fields),
            FixedTypeKind::FunctionBlock {
                fields,
                temporary_fields,
                ..
            } => {
                register(&layout.source_path, fields);
                register(&layout.source_path, temporary_fields);
            }
            _ => {}
        }
    }
    for program in &fixed.programs {
        register(&program.source_path, &program.fields);
        register(&program.source_path, &program.temporary_fields);
    }
    for frame in &fixed.invocation_frames {
        register(&frame.source_path, &frame.fields);
    }
    result
}

const fn is_expression(kind: AstNodeKind) -> bool {
    matches!(
        kind,
        AstNodeKind::Literal
            | AstNodeKind::QualifiedLiteral
            | AstNodeKind::Assignable
            | AstNodeKind::ParenthesizedExpression
            | AstNodeKind::UnaryExpression
            | AstNodeKind::BinaryExpression
            | AstNodeKind::CallExpression
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

const fn is_float(value_type: Option<&SemanticType>) -> bool {
    matches!(value_type, Some(SemanticType::Real | SemanticType::Lreal))
}

fn string_type(value_type: &SemanticType) -> Option<(bool, u64)> {
    match value_type {
        SemanticType::String { capacity } => capacity.parse().ok().map(|value| (false, value)),
        SemanticType::Wstring { capacity } => capacity.parse().ok().map(|value| (true, value)),
        _ => None,
    }
}

fn conversion_target(name: &str) -> Option<SemanticType> {
    Some(match name {
        "TO_SINT" => SemanticType::Sint,
        "TO_INT" => SemanticType::Int,
        "TO_DINT" => SemanticType::Dint,
        "TO_LINT" => SemanticType::Lint,
        "TO_USINT" => SemanticType::Usint,
        "TO_UINT" => SemanticType::Uint,
        "TO_UDINT" => SemanticType::Udint,
        "TO_ULINT" => SemanticType::Ulint,
        "TO_REAL" => SemanticType::Real,
        "TO_LREAL" => SemanticType::Lreal,
        _ => return None,
    })
}

fn integer_standard_operation(
    name: &str,
) -> Option<(IntegerOperation, Option<IntegerArithmeticMode>)> {
    if name == "ABS" {
        return Some((IntegerOperation::Absolute, None));
    }
    let (prefix, operation) = name.rsplit_once('_')?;
    let operation = match operation {
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

fn float_operation_f32(operator: &str, left: u32, right: u32) -> Option<u32> {
    let left = f32::from_bits(left);
    let right = f32::from_bits(right);
    let value = match operator {
        "+" => left + right,
        "-" => left - right,
        "*" => left * right,
        "/" => left / right,
        _ => return None,
    };
    value.is_finite().then(|| value.to_bits())
}

fn float_operation_f64(operator: &str, left: u64, right: u64) -> Option<u64> {
    let left = f64::from_bits(left);
    let right = f64::from_bits(right);
    let value = match operator {
        "+" => left + right,
        "-" => left - right,
        "*" => left * right,
        "/" => left / right,
        _ => return None,
    };
    value.is_finite().then(|| value.to_bits())
}

fn min_max_constant(
    operation: &str,
    left: &ConstantValue,
    right: &ConstantValue,
) -> Option<ConstantValue> {
    match (left, right) {
        (ConstantValue::Integer(left), ConstantValue::Integer(right)) => {
            Some(ConstantValue::Integer(
                if (operation == "MIN" && left <= right) || (operation == "MAX" && left >= right) {
                    *left
                } else {
                    *right
                },
            ))
        }
        (ConstantValue::Real(left), ConstantValue::Real(right)) => {
            let left_value = f32::from_bits(*left);
            let right_value = f32::from_bits(*right);
            if !left_value.is_finite() || !right_value.is_finite() {
                return None;
            }
            Some(ConstantValue::Real(
                if (operation == "MIN" && left_value <= right_value)
                    || (operation == "MAX" && left_value >= right_value)
                {
                    *left
                } else {
                    *right
                },
            ))
        }
        (ConstantValue::Lreal(left), ConstantValue::Lreal(right)) => {
            let left_value = f64::from_bits(*left);
            let right_value = f64::from_bits(*right);
            if !left_value.is_finite() || !right_value.is_finite() {
                return None;
            }
            Some(ConstantValue::Lreal(
                if (operation == "MIN" && left_value <= right_value)
                    || (operation == "MAX" && left_value >= right_value)
                {
                    *left
                } else {
                    *right
                },
            ))
        }
        _ => None,
    }
}

fn ordered_less_equal(left: &ConstantValue, right: &ConstantValue) -> Option<bool> {
    match (left, right) {
        (ConstantValue::Integer(left), ConstantValue::Integer(right)) => Some(left <= right),
        (ConstantValue::Real(left), ConstantValue::Real(right)) => {
            Some(f32::from_bits(*left) <= f32::from_bits(*right))
        }
        (ConstantValue::Lreal(left), ConstantValue::Lreal(right)) => {
            Some(f64::from_bits(*left) <= f64::from_bits(*right))
        }
        _ => None,
    }
}

fn convert_constant(
    value: &ConstantValue,
    target: &SemanticType,
) -> Result<ConstantValue, DiagnosticCode> {
    if let Some(target_type) = integer_type(target) {
        let converted = match value {
            ConstantValue::Integer(value) => Some(*value),
            ConstantValue::UntypedReal(bits) => {
                float_to_integer(f64::from_bits(*bits), target_type)
            }
            ConstantValue::Real(bits) => {
                float_to_integer(f64::from(f32::from_bits(*bits)), target_type)
            }
            ConstantValue::Lreal(bits) => float_to_integer(f64::from_bits(*bits), target_type),
            _ => None,
        };
        return converted
            .filter(|value| target_type.contains(*value))
            .map(ConstantValue::Integer)
            .ok_or(DiagnosticCode::InvalidExplicitConversion);
    }
    match target {
        SemanticType::Real => {
            let value = match value {
                ConstantValue::Integer(value) => integer_to_f32(*value),
                ConstantValue::UntypedReal(bits) | ConstantValue::Lreal(bits) => {
                    lreal_to_real(f64::from_bits(*bits))
                }
                ConstantValue::Real(bits) => f32::from_bits(*bits),
                _ => return Err(DiagnosticCode::InvalidExplicitConversion),
            };
            value
                .is_finite()
                .then(|| ConstantValue::Real(value.to_bits()))
                .ok_or(DiagnosticCode::NonFiniteConstant)
        }
        SemanticType::Lreal => {
            let value = match value {
                ConstantValue::Integer(value) => integer_to_f64(*value),
                ConstantValue::UntypedReal(bits) | ConstantValue::Lreal(bits) => {
                    f64::from_bits(*bits)
                }
                ConstantValue::Real(bits) => f64::from(f32::from_bits(*bits)),
                _ => return Err(DiagnosticCode::InvalidExplicitConversion),
            };
            value
                .is_finite()
                .then(|| ConstantValue::Lreal(value.to_bits()))
                .ok_or(DiagnosticCode::NonFiniteConstant)
        }
        _ => Err(DiagnosticCode::InvalidExplicitConversion),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn float_to_integer(value: f64, target: IntegerType) -> Option<i128> {
    if !value.is_finite() || (!target.signed() && value < 0.0) {
        return None;
    }
    let truncated = value.trunc();
    let bits = target.bits();
    let (minimum, upper_exclusive) = if target.signed() {
        let upper = 2_f64.powi(i32::from(bits - 1));
        (-upper, upper)
    } else {
        (0.0, 2_f64.powi(i32::from(bits)))
    };
    (truncated >= minimum && truncated < upper_exclusive).then_some(truncated as i128)
}

#[allow(clippy::cast_precision_loss)]
fn integer_to_f32(value: i128) -> f32 {
    value as f32
}

#[allow(clippy::cast_precision_loss)]
fn integer_to_f64(value: i128) -> f64 {
    value as f64
}

#[allow(clippy::cast_possible_truncation)]
fn lreal_to_real(value: f64) -> f32 {
    value as f32
}

fn conversion_can_fault(source: Option<&SemanticType>, target: &SemanticType) -> bool {
    match (source, target) {
        (Some(source), target)
            if integer_type(source).is_some() && integer_type(target).is_some() =>
        {
            let source = integer_type(source).unwrap_or(IntegerType::Lint);
            let target = integer_type(target).unwrap_or(IntegerType::Lint);
            let (source_minimum, source_maximum) = source.bounds();
            let (target_minimum, target_maximum) = target.bounds();
            source_minimum < target_minimum || source_maximum > target_maximum
        }
        (Some(SemanticType::Real | SemanticType::Lreal), SemanticType::Lreal)
        | (Some(SemanticType::Real), SemanticType::Real) => false,
        (Some(SemanticType::Lreal), SemanticType::Real) => true,
        (Some(source), SemanticType::Real | SemanticType::Lreal)
            if integer_type(source).is_some() =>
        {
            false
        }
        (Some(SemanticType::Real | SemanticType::Lreal), target)
            if integer_type(target).is_some() =>
        {
            true
        }
        _ => true,
    }
}

fn string_units(text: &str, wide: bool) -> Option<u64> {
    let quote = if wide { '"' } else { '\'' };
    let content = text.strip_prefix(quote)?.strip_suffix(quote)?;
    let mut characters = content.chars();
    let mut units = 0_u64;
    while let Some(character) = characters.next() {
        let decoded = if character == '\\' {
            match characters.next()? {
                '\\' => '\\',
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                value if value == quote => quote,
                'u' => {
                    if characters.next()? != '{' {
                        return None;
                    }
                    let mut digits = String::new();
                    loop {
                        let next = characters.next()?;
                        if next == '}' {
                            break;
                        }
                        digits.push(next);
                    }
                    char::from_u32(u32::from_str_radix(&digits, 16).ok()?)?
                }
                _ => return None,
            }
        } else {
            character
        };
        let increment = if wide {
            u64::try_from(decoded.len_utf16()).ok()?
        } else {
            u64::try_from(decoded.len_utf8()).ok()?
        };
        units = units.checked_add(increment)?;
    }
    Some(units)
}
