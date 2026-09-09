//! Versioned Aurora ST frontend contracts.
//!
//! The parser is host-only compiler work: it may allocate within caller-supplied limits and is
//! never used on the cyclic execution path. A failed parse publishes diagnostics only; a partial
//! syntax tree cannot be mistaken for a deployable compiler artifact.

mod ast;
mod diagnostic;
mod fault;
mod fixed;
mod lexer;
mod parser;
mod semantic;

pub use ast::{
    AST_SCHEMA_MAJOR, AST_SCHEMA_MINOR, AstNode, AstNodeKind, AstSerializationError, AstVersion,
    VersionedAst, to_canonical_json,
};
pub use diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSerializationError, SourcePosition, SourceSpan,
    diagnostics_to_canonical_json,
};
pub use fault::{
    FaultAnalysisOutput, FaultOperationKind, FaultSemanticModel, FaultSite, FaultSiteId,
    IntegerArithmeticError, IntegerArithmeticMode, IntegerOperation, IntegerType, RuntimeFaultCode,
    analyze_faults, evaluate_integer_operation, validate_array_index,
};
pub use fixed::{
    FixedAnalysisOutput, FixedDataLimitError, FixedDataLimits, FixedEnumerationMember,
    FixedFieldLayout, FixedFieldStorage, FixedGlobalLayout, FixedInitializer, FixedSemanticModel,
    FixedTypeId, FixedTypeKind, FixedTypeLayout, InvocationFrameLayout,
    StaticFunctionBlockInstance, StaticProgramLayout, analyze_fixed,
};
pub use parser::{LimitConfigurationError, ParseOutput, ParserLimits, parse};
pub use semantic::{
    AnalysisInputError, AnalysisOutput, ResolvedReference, SemanticModel, SemanticSource,
    SemanticSymbol, SemanticSymbolKind, SemanticType, SymbolId, TypedExpression, analyze,
};
