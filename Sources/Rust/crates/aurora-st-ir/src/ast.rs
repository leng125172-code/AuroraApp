use serde::Serialize;
use thiserror::Error;

use crate::SourceSpan;

/// Major version of the serialized Aurora ST syntax tree.
pub const AST_SCHEMA_MAJOR: u16 = 1;
/// Minor version of the serialized Aurora ST syntax tree.
pub const AST_SCHEMA_MINOR: u16 = 0;

/// Version carried by every serialized syntax tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AstVersion {
    /// Schema major; readers must reject unknown values.
    pub major: u16,
    /// Schema minor; readers must explicitly declare support.
    pub minor: u16,
}

impl AstVersion {
    /// Returns the exact schema version emitted by this crate.
    #[must_use]
    pub const fn preview_v1_0() -> Self {
        Self {
            major: AST_SCHEMA_MAJOR,
            minor: AST_SCHEMA_MINOR,
        }
    }
}

/// Versioned, source-position-preserving AST for one valid compilation unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VersionedAst {
    /// Stable AST schema version, independent of the source language version.
    pub schema_version: AstVersion,
    /// Normalized project-relative source path supplied by the caller.
    pub source_path: String,
    /// Root compilation-unit node.
    pub root: AstNode,
}

/// One AST node. `text` contains source spelling only for identifiers, literals, addresses and
/// operators; structural nodes keep it absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AstNode {
    /// Stable syntax kind.
    pub kind: AstNodeKind,
    /// Half-open UTF-8 byte range in the source file.
    pub span: SourceSpan,
    /// Relevant source spelling for leaf/operator nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Ordered child nodes in source/semantic evaluation order.
    pub children: Vec<Self>,
}

/// Stable Preview 1.0 syntax-node kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AstNodeKind {
    /// Complete file.
    CompilationUnit,
    /// Required `AURORA_ST VERSION 1.0;` directive.
    VersionDirective,
    /// `TYPE` declaration block.
    TypeBlock,
    /// Named type declaration.
    TypeDeclaration,
    /// Elementary scalar type.
    ElementaryType,
    /// Fixed-capacity narrow or wide string type.
    StringType,
    /// Fixed-bound array type.
    ArrayType,
    /// Structure type.
    StructureType,
    /// Structure field.
    StructureField,
    /// Enumeration type.
    EnumerationType,
    /// Enumeration member.
    EnumerationItem,
    /// Named type reference.
    NamedType,
    /// `VAR_GLOBAL` declaration block.
    GlobalVariableBlock,
    /// Address-backed global declaration.
    GlobalVariableDeclaration,
    /// Function declaration.
    FunctionDeclaration,
    /// Function-block declaration.
    FunctionBlockDeclaration,
    /// Program declaration.
    ProgramDeclaration,
    /// `VAR_INPUT` block.
    InputVariableBlock,
    /// `VAR_OUTPUT` block.
    OutputVariableBlock,
    /// Stateful/local `VAR` block.
    LocalVariableBlock,
    /// Per-invocation `VAR_TEMP` block.
    TemporaryVariableBlock,
    /// Local/input/output variable declaration.
    VariableDeclaration,
    /// Ordered identifier list.
    IdentifierList,
    /// Ordered list of statements.
    StatementList,
    /// Assignment statement.
    AssignmentStatement,
    /// Named function-block invocation statement.
    FunctionBlockCallStatement,
    /// Named function-block input argument.
    InputArgument,
    /// Named function-block output argument.
    OutputArgument,
    /// `IF` statement.
    IfStatement,
    /// One `ELSIF` clause.
    ElsifClause,
    /// Optional `ELSE` clause.
    ElseClause,
    /// Statically bounded `FOR` statement.
    ForStatement,
    /// `RETURN` statement.
    ReturnStatement,
    /// Binary expression; `text` is the operator spelling.
    BinaryExpression,
    /// Unary expression; `text` is the operator spelling.
    UnaryExpression,
    /// Literal token.
    Literal,
    /// Type- or enum-qualified literal.
    QualifiedLiteral,
    /// Positional function call expression.
    CallExpression,
    /// Assignable name with optional index/member suffixes.
    Assignable,
    /// Dotted identifier path.
    QualifiedIdentifier,
    /// Array index suffix.
    IndexSuffix,
    /// Field-selection suffix that follows an indexed value.
    FieldSuffix,
    /// Explicitly parenthesized expression.
    ParenthesizedExpression,
    /// Identifier spelling.
    Identifier,
    /// Canonical direct-address token.
    DirectAddress,
}

/// Failure to serialize an otherwise valid AST as RFC 8785 canonical JSON.
#[derive(Debug, Error)]
pub enum AstSerializationError {
    /// AST version does not match this writer.
    #[error("unsupported Aurora ST AST schema version {major}.{minor}")]
    UnsupportedVersion {
        /// Unsupported major value.
        major: u16,
        /// Unsupported minor value.
        minor: u16,
    },
    /// AST could not be represented as canonical JSON.
    #[error("failed to serialize the Aurora ST AST as canonical JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

/// Serializes a valid AST using RFC 8785 canonical JSON.
///
/// # Errors
///
/// Returns [`AstSerializationError::UnsupportedVersion`] for a mismatched writer version, or
/// [`AstSerializationError::InvalidJson`] if a future AST value cannot be represented.
pub fn to_canonical_json(ast: &VersionedAst) -> Result<Vec<u8>, AstSerializationError> {
    if ast.schema_version != AstVersion::preview_v1_0() {
        return Err(AstSerializationError::UnsupportedVersion {
            major: ast.schema_version.major,
            minor: ast.schema_version.minor,
        });
    }
    serde_jcs::to_vec(ast).map_err(AstSerializationError::from)
}
