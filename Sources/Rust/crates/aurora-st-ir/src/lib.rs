//! Versioned Aurora ST frontend contracts.
//!
//! The parser is host-only compiler work: it may allocate within caller-supplied limits and is
//! never used on the cyclic execution path. A failed parse publishes diagnostics only; a partial
//! syntax tree cannot be mistaken for a deployable compiler artifact.

mod ast;
mod diagnostic;
mod lexer;
mod parser;

pub use ast::{
    AST_SCHEMA_MAJOR, AST_SCHEMA_MINOR, AstNode, AstNodeKind, AstSerializationError, AstVersion,
    VersionedAst, to_canonical_json,
};
pub use diagnostic::{Diagnostic, DiagnosticCode, SourcePosition, SourceSpan};
pub use parser::{LimitConfigurationError, ParseOutput, ParserLimits, parse};
