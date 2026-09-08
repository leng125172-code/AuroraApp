use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::ast::{AST_SCHEMA_MAJOR, AST_SCHEMA_MINOR, AstNode, AstNodeKind, VersionedAst};
use crate::diagnostic::make_diagnostic;
use crate::{Diagnostic, DiagnosticCode, SourceSpan};

/// One parsed source supplied to project-wide semantic analysis.
#[derive(Debug, Clone, Copy)]
pub struct SemanticSource<'a> {
    /// Parser-produced AST.
    pub ast: &'a VersionedAst,
    /// Exact UTF-8 source used to produce `ast`.
    pub source: &'a str,
}

impl<'a> SemanticSource<'a> {
    /// Couples a parser-produced AST with its exact source text.
    #[must_use]
    pub const fn new(ast: &'a VersionedAst, source: &'a str) -> Self {
        Self { ast, source }
    }
}

/// Stable project-local symbol identifier assigned by deterministic declaration passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SymbolId(
    /// Zero-based identifier value.
    pub u32,
);

/// Symbol categories produced by R1-02 name resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticSymbolKind {
    /// Named type declaration.
    Type,
    /// Address-backed global variable.
    GlobalVariable,
    /// Pure function.
    Function,
    /// Stateful function-block type.
    FunctionBlock,
    /// Program type.
    Program,
    /// Named member of an enumeration declaration.
    EnumerationMember,
    /// POU input.
    InputVariable,
    /// POU output.
    OutputVariable,
    /// POU local/state variable.
    LocalVariable,
    /// Per-invocation temporary variable.
    TemporaryVariable,
}

/// Type attached to a resolved symbol or expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SemanticType {
    /// Boolean value.
    Bool,
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
    /// IEEE 754 binary32 value.
    Real,
    /// IEEE 754 binary64 value.
    Lreal,
    /// Fixed-capacity UTF-8 string; capacity validation belongs to R1-03.
    String {
        /// Decimal capacity spelling from the declaration.
        capacity: String,
    },
    /// Fixed-capacity UTF-16 string; capacity validation belongs to R1-03.
    Wstring {
        /// Decimal capacity spelling from the declaration.
        capacity: String,
    },
    /// Nominal enumeration declaration.
    Enumeration {
        /// Owning named type when one exists.
        declaration: Option<SymbolId>,
    },
    /// Array or structure whose capacity/layout validation belongs to R1-03.
    Composite {
        /// Owning named type when one exists.
        declaration: Option<SymbolId>,
    },
    /// Named alias; semantic checks resolve through it without losing declaration identity.
    Named {
        /// Referenced type declaration.
        declaration: SymbolId,
    },
    /// Static function-block instance type.
    FunctionBlock {
        /// Referenced function-block declaration.
        declaration: SymbolId,
    },
}

/// One declaration retained in the successful semantic model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticSymbol {
    /// Deterministic project-local identifier.
    pub id: SymbolId,
    /// Original declaration spelling.
    pub name: String,
    /// ASCII-lowercase lookup key.
    pub canonical_name: String,
    /// Declaration category.
    pub kind: SemanticSymbolKind,
    /// Normalized project-relative source path.
    pub source_path: String,
    /// Identifier declaration span.
    pub span: SourceSpan,
    /// Owning POU for local symbols.
    pub owner: Option<SymbolId>,
    /// Declared value/return/type definition when applicable.
    pub declared_type: Option<SemanticType>,
}

/// One identifier reference bound to a declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedReference {
    /// Referencing source path.
    pub source_path: String,
    /// Identifier span.
    pub span: SourceSpan,
    /// Resolved declaration.
    pub symbol: SymbolId,
}

/// Type inferred for one expression node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TypedExpression {
    /// Expression source path.
    pub source_path: String,
    /// Expression span.
    pub span: SourceSpan,
    /// Resolved expression type.
    pub value_type: SemanticType,
}

/// Successful project-wide name and scalar-type model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SemanticModel {
    /// Valid declarations in deterministic symbol-id order.
    pub symbols: Vec<SemanticSymbol>,
    /// Bound references in path/span order.
    pub references: Vec<ResolvedReference>,
    /// Typed expressions in path/span order.
    pub expressions: Vec<TypedExpression>,
}

/// Atomic semantic-analysis result. Diagnostics and a model are mutually exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisOutput {
    /// Semantic model, present only when no diagnostics were produced.
    pub model: Option<SemanticModel>,
    /// Stable diagnostics sorted by path/byte/code.
    pub diagnostics: Vec<Diagnostic>,
}

/// Caller or AST corruption detected before semantic analysis.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AnalysisInputError {
    /// AST schema is not the exact version understood by this analyzer.
    #[error("unsupported Aurora ST AST schema {major}.{minor}")]
    UnsupportedAstVersion {
        /// Unsupported major.
        major: u16,
        /// Unsupported minor.
        minor: u16,
    },
    /// Two source entries use the same project path.
    #[error("duplicate Aurora ST source path `{0}`")]
    DuplicateSourcePath(String),
    /// AST root or child shape is not parser-produced Preview 1.0.
    #[error("invalid Aurora ST AST shape in `{source_path}` at bytes {span_start}..{span_end}")]
    InvalidAstShape {
        /// Source containing the invalid node.
        source_path: String,
        /// Invalid node start.
        span_start: u32,
        /// Invalid node end.
        span_end: u32,
    },
    /// Source spans do not fit or align with the supplied source text.
    #[error("invalid Aurora ST source span in `{source_path}` at bytes {span_start}..{span_end}")]
    InvalidSourceSpan {
        /// Source containing the invalid span.
        source_path: String,
        /// Invalid start.
        span_start: u32,
        /// Invalid end.
        span_end: u32,
    },
    /// Symbol identifiers would exceed the frozen `u32` representation.
    #[error("Aurora ST project contains too many symbols")]
    TooManySymbols,
}

/// Resolves names and checks Preview 1.0 scalar types across a set of parsed files.
///
/// Sources are normalized into path-byte order before symbol IDs or diagnostics are assigned.
/// Composite capacity/layout and recursive instance validation are intentionally deferred to
/// R1-03; arithmetic Fault lowering and address binding remain outside this pass.
///
/// # Errors
///
/// Returns [`AnalysisInputError`] when inputs are not exact parser-produced AST/source pairs.
pub fn analyze(sources: &[SemanticSource<'_>]) -> Result<AnalysisOutput, AnalysisInputError> {
    Analyzer::new(sources)?.run()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TypeValue {
    Bool,
    Signed(u8),
    Unsigned(u8),
    Float(u8),
    String { wide: bool, capacity: String },
    Enumeration(Option<SymbolId>),
    Composite(Option<SymbolId>, u32, u32),
    Named(SymbolId),
    FunctionBlock(SymbolId),
    Unknown,
}

impl TypeValue {
    fn public(&self) -> Option<SemanticType> {
        Some(match self {
            Self::Bool => SemanticType::Bool,
            Self::Signed(8) => SemanticType::Sint,
            Self::Signed(16) => SemanticType::Int,
            Self::Signed(32) => SemanticType::Dint,
            Self::Signed(64) => SemanticType::Lint,
            Self::Unsigned(8) => SemanticType::Usint,
            Self::Unsigned(16) => SemanticType::Uint,
            Self::Unsigned(32) => SemanticType::Udint,
            Self::Unsigned(64) => SemanticType::Ulint,
            Self::Float(32) => SemanticType::Real,
            Self::Float(64) => SemanticType::Lreal,
            Self::String {
                wide: false,
                capacity,
            } => SemanticType::String {
                capacity: capacity.clone(),
            },
            Self::String {
                wide: true,
                capacity,
            } => SemanticType::Wstring {
                capacity: capacity.clone(),
            },
            Self::Enumeration(declaration) => SemanticType::Enumeration {
                declaration: *declaration,
            },
            Self::Composite(declaration, _, _) => SemanticType::Composite {
                declaration: *declaration,
            },
            Self::Named(declaration) => SemanticType::Named {
                declaration: *declaration,
            },
            Self::FunctionBlock(declaration) => SemanticType::FunctionBlock {
                declaration: *declaration,
            },
            Self::Unknown | Self::Signed(_) | Self::Unsigned(_) | Self::Float(_) => return None,
        })
    }

    const fn is_integer(&self) -> bool {
        matches!(self, Self::Signed(_) | Self::Unsigned(_))
    }

    const fn is_numeric(&self) -> bool {
        self.is_integer() || matches!(self, Self::Float(_))
    }

    const fn is_scalar(&self) -> bool {
        matches!(self, Self::Bool) || self.is_numeric()
    }
}

#[derive(Debug, Clone)]
struct SymbolRecord<'a> {
    symbol: SemanticSymbol,
    source_index: usize,
    node: &'a AstNode,
    type_node: Option<&'a AstNode>,
    value_type: TypeValue,
}

#[derive(Debug, Clone)]
struct PouRecord<'a> {
    symbol_index: usize,
    source_index: usize,
    body: &'a AstNode,
    locals: BTreeMap<String, usize>,
    invalid_locals: BTreeSet<String>,
    inputs: Vec<usize>,
    outputs: Vec<usize>,
}

#[derive(Debug, Clone, Copy)]
struct CallEdge {
    from: SymbolId,
    to: SymbolId,
    source_index: usize,
    span: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum InferredType {
    Known(TypeValue),
    UntypedInteger,
    UntypedReal,
    UntypedString(bool),
    Unknown,
}

#[derive(Debug, Clone)]
struct ExpressionInfo {
    inferred: InferredType,
    integer: Option<IntegerConstant>,
}

#[derive(Debug, Clone, Copy)]
struct IntegerConstant {
    negative: bool,
    magnitude: Option<u128>,
}

#[derive(Clone, Copy)]
enum Lookup {
    Found(usize),
    Suppressed,
    Missing,
}

struct Analyzer<'a> {
    sources: Vec<SemanticSource<'a>>,
    diagnostics: Vec<Diagnostic>,
    diagnostic_keys: BTreeSet<(usize, u32, u32, &'static str)>,
    symbols: Vec<SymbolRecord<'a>>,
    top: BTreeMap<String, usize>,
    invalid_top: BTreeSet<String>,
    enum_members: BTreeMap<(SymbolId, String), usize>,
    invalid_enum_members: BTreeSet<(SymbolId, String)>,
    pous: Vec<PouRecord<'a>>,
    pou_by_symbol: BTreeMap<SymbolId, usize>,
    references: Vec<ResolvedReference>,
    expressions: Vec<TypedExpression>,
    call_edges: Vec<CallEdge>,
}

impl<'a> Analyzer<'a> {
    fn new(sources: &[SemanticSource<'a>]) -> Result<Self, AnalysisInputError> {
        let mut ordered = sources.to_vec();
        ordered.sort_by(|left, right| {
            left.ast
                .source_path
                .as_bytes()
                .cmp(right.ast.source_path.as_bytes())
        });
        for pair in ordered.windows(2) {
            if pair[0].ast.source_path == pair[1].ast.source_path {
                return Err(AnalysisInputError::DuplicateSourcePath(
                    pair[0].ast.source_path.clone(),
                ));
            }
        }
        for source in &ordered {
            validate_source(source)?;
        }
        Ok(Self {
            sources: ordered,
            diagnostics: Vec::new(),
            diagnostic_keys: BTreeSet::new(),
            symbols: Vec::new(),
            top: BTreeMap::new(),
            invalid_top: BTreeSet::new(),
            enum_members: BTreeMap::new(),
            invalid_enum_members: BTreeSet::new(),
            pous: Vec::new(),
            pou_by_symbol: BTreeMap::new(),
            references: Vec::new(),
            expressions: Vec::new(),
            call_edges: Vec::new(),
        })
    }

    fn run(mut self) -> Result<AnalysisOutput, AnalysisInputError> {
        self.collect_top_level()?;
        self.resolve_top_types()?;
        self.collect_pous()?;
        self.analyze_initializers()?;
        self.analyze_pous()?;
        self.report_recursive_calls();
        sort_diagnostics(&mut self.diagnostics);
        if self.diagnostics.is_empty() {
            self.references.sort_by(reference_order);
            self.references.dedup();
            self.expressions.sort_by(expression_order);
            self.expressions.dedup();
            let symbols = self
                .symbols
                .into_iter()
                .map(|record| record.symbol)
                .collect();
            Ok(AnalysisOutput {
                model: Some(SemanticModel {
                    symbols,
                    references: self.references,
                    expressions: self.expressions,
                }),
                diagnostics: Vec::new(),
            })
        } else {
            Ok(AnalysisOutput {
                model: None,
                diagnostics: self.diagnostics,
            })
        }
    }

    fn collect_top_level(&mut self) -> Result<(), AnalysisInputError> {
        for source_index in 0..self.sources.len() {
            let ast = self.sources[source_index].ast;
            for child in &ast.root.children {
                match child.kind {
                    AstNodeKind::VersionDirective => {}
                    AstNodeKind::TypeBlock => {
                        for declaration in &child.children {
                            self.declare_top(source_index, declaration, SemanticSymbolKind::Type)?;
                        }
                    }
                    AstNodeKind::GlobalVariableBlock => {
                        for declaration in &child.children {
                            self.declare_top(
                                source_index,
                                declaration,
                                SemanticSymbolKind::GlobalVariable,
                            )?;
                        }
                    }
                    AstNodeKind::FunctionDeclaration => {
                        self.declare_top(source_index, child, SemanticSymbolKind::Function)?;
                    }
                    AstNodeKind::FunctionBlockDeclaration => {
                        self.declare_top(source_index, child, SemanticSymbolKind::FunctionBlock)?;
                    }
                    AstNodeKind::ProgramDeclaration => {
                        self.declare_top(source_index, child, SemanticSymbolKind::Program)?;
                    }
                    _ => return Err(self.invalid_shape(source_index, child.span)),
                }
            }
        }
        Ok(())
    }

    fn declare_top(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        kind: SemanticSymbolKind,
    ) -> Result<(), AnalysisInputError> {
        let name = required_child(self.sources[source_index].ast, node, 0)?;
        if name.kind != AstNodeKind::Identifier {
            return Err(self.invalid_shape(source_index, name.span));
        }
        let spelling = required_text(self.sources[source_index].ast, name)?;
        let canonical = spelling.to_ascii_lowercase();
        if is_reserved(&canonical) {
            self.emit(source_index, DiagnosticCode::ReservedIdentifier, name.span);
            self.invalid_top.insert(canonical);
            return Ok(());
        }
        if self.top.contains_key(&canonical) {
            self.emit(source_index, DiagnosticCode::DuplicateSymbol, name.span);
            return Ok(());
        }
        let id = self.next_symbol_id()?;
        let type_node = match kind {
            SemanticSymbolKind::Type
            | SemanticSymbolKind::GlobalVariable
            | SemanticSymbolKind::Function => Some(required_child(
                self.sources[source_index].ast,
                node,
                if kind == SemanticSymbolKind::GlobalVariable {
                    2
                } else {
                    1
                },
            )?),
            _ => None,
        };
        let symbol = SemanticSymbol {
            id,
            name: spelling.to_owned(),
            canonical_name: canonical.clone(),
            kind,
            source_path: self.sources[source_index].ast.source_path.clone(),
            span: name.span,
            owner: None,
            declared_type: None,
        };
        let index = self.symbols.len();
        self.symbols.push(SymbolRecord {
            symbol,
            source_index,
            node,
            type_node,
            value_type: TypeValue::Unknown,
        });
        self.top.insert(canonical, index);
        Ok(())
    }

    fn resolve_top_types(&mut self) -> Result<(), AnalysisInputError> {
        let top_count = self.symbols.len();
        for index in 0..top_count {
            let source_index = self.symbols[index].source_index;
            let type_node = self.symbols[index].type_node;
            let owner = (self.symbols[index].symbol.kind == SemanticSymbolKind::Type)
                .then_some(self.symbols[index].symbol.id);
            let value_type = match type_node {
                Some(node) => self.resolve_type(source_index, node, owner)?,
                None if self.symbols[index].symbol.kind == SemanticSymbolKind::FunctionBlock => {
                    TypeValue::FunctionBlock(self.symbols[index].symbol.id)
                }
                None => TypeValue::Unknown,
            };
            self.symbols[index].value_type = value_type.clone();
            self.symbols[index].symbol.declared_type = value_type.public();
            if self.symbols[index].symbol.kind == SemanticSymbolKind::Type {
                self.validate_type_members(
                    source_index,
                    self.symbols[index].node,
                    self.symbols[index].symbol.id,
                )?;
            }
        }
        Ok(())
    }

    fn resolve_type(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        owner: Option<SymbolId>,
    ) -> Result<TypeValue, AnalysisInputError> {
        match node.kind {
            AstNodeKind::ElementaryType => {
                let text = required_text(self.sources[source_index].ast, node)?;
                Ok(elementary_type(text).unwrap_or(TypeValue::Unknown))
            }
            AstNodeKind::NamedType => {
                let text = required_text(self.sources[source_index].ast, node)?;
                match self.lookup_top(text) {
                    Lookup::Found(index)
                        if self.symbols[index].symbol.kind == SemanticSymbolKind::Type =>
                    {
                        self.reference(source_index, node.span, index);
                        Ok(TypeValue::Named(self.symbols[index].symbol.id))
                    }
                    Lookup::Found(index)
                        if self.symbols[index].symbol.kind == SemanticSymbolKind::FunctionBlock =>
                    {
                        self.reference(source_index, node.span, index);
                        Ok(TypeValue::FunctionBlock(self.symbols[index].symbol.id))
                    }
                    Lookup::Suppressed => Ok(TypeValue::Unknown),
                    Lookup::Found(_) | Lookup::Missing => {
                        self.emit(source_index, DiagnosticCode::UndefinedSymbol, node.span);
                        Ok(TypeValue::Unknown)
                    }
                }
            }
            AstNodeKind::StringType => {
                let wide = required_text(self.sources[source_index].ast, node)?
                    .eq_ignore_ascii_case("WSTRING");
                let capacity = required_child(self.sources[source_index].ast, node, 0)?
                    .text
                    .clone()
                    .ok_or_else(|| self.invalid_shape(source_index, node.span))?;
                Ok(TypeValue::String { wide, capacity })
            }
            AstNodeKind::EnumerationType => Ok(TypeValue::Enumeration(owner)),
            AstNodeKind::ArrayType => {
                let element = required_child(self.sources[source_index].ast, node, 2)?;
                self.resolve_type(source_index, element, None)?;
                Ok(TypeValue::Composite(
                    owner,
                    u32::try_from(source_index).map_err(|_| AnalysisInputError::TooManySymbols)?,
                    node.span.start,
                ))
            }
            AstNodeKind::StructureType => {
                for field in &node.children {
                    let field_type = required_child(self.sources[source_index].ast, field, 1)?;
                    self.resolve_type(source_index, field_type, None)?;
                }
                Ok(TypeValue::Composite(
                    owner,
                    u32::try_from(source_index).map_err(|_| AnalysisInputError::TooManySymbols)?,
                    node.span.start,
                ))
            }
            _ => Err(self.invalid_shape(source_index, node.span)),
        }
    }

    fn validate_type_members(
        &mut self,
        source_index: usize,
        declaration: &'a AstNode,
        owner: SymbolId,
    ) -> Result<(), AnalysisInputError> {
        let type_node = required_child(self.sources[source_index].ast, declaration, 1)?;
        if !matches!(
            type_node.kind,
            AstNodeKind::EnumerationType | AstNodeKind::StructureType
        ) {
            return Ok(());
        }
        let mut names = BTreeSet::new();
        for member in &type_node.children {
            let name = required_child(self.sources[source_index].ast, member, 0)?;
            let spelling = required_text(self.sources[source_index].ast, name)?;
            let canonical = spelling.to_ascii_lowercase();
            if is_reserved(&canonical) {
                self.emit(source_index, DiagnosticCode::ReservedIdentifier, name.span);
                if type_node.kind == AstNodeKind::EnumerationType {
                    self.invalid_enum_members.insert((owner, canonical));
                }
            } else if !names.insert(canonical.clone()) {
                self.emit(source_index, DiagnosticCode::DuplicateSymbol, name.span);
            } else if type_node.kind == AstNodeKind::EnumerationType {
                let id = self.next_symbol_id()?;
                let index = self.symbols.len();
                self.symbols.push(SymbolRecord {
                    symbol: SemanticSymbol {
                        id,
                        name: spelling.to_owned(),
                        canonical_name: canonical.clone(),
                        kind: SemanticSymbolKind::EnumerationMember,
                        source_path: self.sources[source_index].ast.source_path.clone(),
                        span: name.span,
                        owner: Some(owner),
                        declared_type: Some(SemanticType::Enumeration {
                            declaration: Some(owner),
                        }),
                    },
                    source_index,
                    node: member,
                    type_node: None,
                    value_type: TypeValue::Enumeration(Some(owner)),
                });
                self.enum_members.insert((owner, canonical), index);
            }
        }
        Ok(())
    }

    fn collect_pous(&mut self) -> Result<(), AnalysisInputError> {
        let top_indices = self
            .symbols
            .iter()
            .enumerate()
            .filter_map(|(index, record)| {
                matches!(
                    record.symbol.kind,
                    SemanticSymbolKind::Function
                        | SemanticSymbolKind::FunctionBlock
                        | SemanticSymbolKind::Program
                )
                .then_some(index)
            })
            .collect::<Vec<_>>();
        for symbol_index in top_indices {
            let source_index = self.symbols[symbol_index].source_index;
            let node = self.symbols[symbol_index].node;
            let body = node
                .children
                .last()
                .filter(|child| child.kind == AstNodeKind::StatementList)
                .ok_or_else(|| self.invalid_shape(source_index, node.span))?;
            let mut pou = PouRecord {
                symbol_index,
                source_index,
                body,
                locals: BTreeMap::new(),
                invalid_locals: BTreeSet::new(),
                inputs: Vec::new(),
                outputs: Vec::new(),
            };
            for block in &node.children {
                let local_kind = match block.kind {
                    AstNodeKind::InputVariableBlock => SemanticSymbolKind::InputVariable,
                    AstNodeKind::OutputVariableBlock => SemanticSymbolKind::OutputVariable,
                    AstNodeKind::LocalVariableBlock => SemanticSymbolKind::LocalVariable,
                    AstNodeKind::TemporaryVariableBlock => SemanticSymbolKind::TemporaryVariable,
                    _ => continue,
                };
                for declaration in &block.children {
                    self.declare_locals(&mut pou, declaration, local_kind)?;
                }
            }
            let pou_index = self.pous.len();
            self.pou_by_symbol
                .insert(self.symbols[symbol_index].symbol.id, pou_index);
            self.pous.push(pou);
        }
        Ok(())
    }

    fn declare_locals(
        &mut self,
        pou: &mut PouRecord<'a>,
        declaration: &'a AstNode,
        kind: SemanticSymbolKind,
    ) -> Result<(), AnalysisInputError> {
        let names = required_child(self.sources[pou.source_index].ast, declaration, 0)?;
        let type_node = required_child(self.sources[pou.source_index].ast, declaration, 1)?;
        let value_type = self.resolve_type(pou.source_index, type_node, None)?;
        for name in &names.children {
            let spelling = required_text(self.sources[pou.source_index].ast, name)?;
            let canonical = spelling.to_ascii_lowercase();
            if is_reserved(&canonical) {
                self.emit(
                    pou.source_index,
                    DiagnosticCode::ReservedIdentifier,
                    name.span,
                );
                pou.invalid_locals.insert(canonical);
                continue;
            }
            if pou.locals.contains_key(&canonical) || self.top.contains_key(&canonical) {
                self.emit(pou.source_index, DiagnosticCode::DuplicateSymbol, name.span);
                pou.invalid_locals.insert(canonical);
                continue;
            }
            let id = self.next_symbol_id()?;
            let symbol = SemanticSymbol {
                id,
                name: spelling.to_owned(),
                canonical_name: canonical.clone(),
                kind,
                source_path: self.sources[pou.source_index].ast.source_path.clone(),
                span: name.span,
                owner: Some(self.symbols[pou.symbol_index].symbol.id),
                declared_type: value_type.public(),
            };
            let index = self.symbols.len();
            self.symbols.push(SymbolRecord {
                symbol,
                source_index: pou.source_index,
                node: declaration,
                type_node: Some(type_node),
                value_type: value_type.clone(),
            });
            pou.locals.insert(canonical, index);
            if kind == SemanticSymbolKind::InputVariable {
                pou.inputs.push(index);
            } else if kind == SemanticSymbolKind::OutputVariable {
                pou.outputs.push(index);
            }
        }
        Ok(())
    }

    fn analyze_initializers(&mut self) -> Result<(), AnalysisInputError> {
        let top_count = self
            .symbols
            .iter()
            .take_while(|record| record.symbol.owner.is_none())
            .count();
        for index in 0..top_count {
            let record = self.symbols[index].clone();
            if record.symbol.kind == SemanticSymbolKind::Type
                && let Some(type_node) = record.type_node
            {
                self.analyze_embedded_initializers(record.source_index, type_node)?;
            }
            let initializer_index = match record.symbol.kind {
                SemanticSymbolKind::Type | SemanticSymbolKind::Function => 2,
                SemanticSymbolKind::GlobalVariable => 3,
                _ => continue,
            };
            if record.symbol.kind == SemanticSymbolKind::Function {
                continue;
            }
            if let Some(initializer) = record.node.children.get(initializer_index) {
                self.expression(
                    record.source_index,
                    initializer,
                    None,
                    Some(record.value_type.clone()),
                    &BTreeSet::new(),
                )?;
            }
        }
        let mut checked_declarations = BTreeSet::new();
        for pou_index in 0..self.pous.len() {
            let pou = self.pous[pou_index].clone();
            for symbol_index in pou.locals.values().copied() {
                let record = self.symbols[symbol_index].clone();
                let declaration_key = (
                    record.source_index,
                    record.node.span.start,
                    record.node.span.end,
                );
                if !checked_declarations.insert(declaration_key) {
                    continue;
                }
                if let Some(initializer) = record.node.children.get(2) {
                    self.expression(
                        record.source_index,
                        initializer,
                        Some(&pou),
                        Some(record.value_type),
                        &BTreeSet::new(),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn analyze_embedded_initializers(
        &mut self,
        source_index: usize,
        type_node: &'a AstNode,
    ) -> Result<(), AnalysisInputError> {
        match type_node.kind {
            AstNodeKind::ArrayType => {
                let element = required_child(self.sources[source_index].ast, type_node, 2)?;
                self.analyze_embedded_initializers(source_index, element)?;
            }
            AstNodeKind::StructureType => {
                for field in &type_node.children {
                    let field_type = required_child(self.sources[source_index].ast, field, 1)?;
                    let value_type = self.resolve_type(source_index, field_type, None)?;
                    if let Some(initializer) = field.children.get(2) {
                        self.expression(
                            source_index,
                            initializer,
                            None,
                            Some(value_type),
                            &BTreeSet::new(),
                        )?;
                    }
                    self.analyze_embedded_initializers(source_index, field_type)?;
                }
            }
            AstNodeKind::EnumerationType => {
                for member in &type_node.children {
                    if let Some(initializer) = member.children.get(1) {
                        self.expression(
                            source_index,
                            initializer,
                            None,
                            Some(TypeValue::Signed(32)),
                            &BTreeSet::new(),
                        )?;
                    }
                }
            }
            AstNodeKind::ElementaryType | AstNodeKind::NamedType | AstNodeKind::StringType => {}
            _ => return Err(self.invalid_shape(source_index, type_node.span)),
        }
        Ok(())
    }

    fn analyze_pous(&mut self) -> Result<(), AnalysisInputError> {
        for pou_index in 0..self.pous.len() {
            let pou = self.pous[pou_index].clone();
            let mut invalid_return = false;
            self.statement_list(&pou, pou.body, &BTreeSet::new(), &mut invalid_return)?;
            if self.symbols[pou.symbol_index].symbol.kind == SemanticSymbolKind::Function
                && !invalid_return
                && !guarantees_return(pou.body)
            {
                self.emit(
                    pou.source_index,
                    DiagnosticCode::InvalidPouAccess,
                    self.symbols[pou.symbol_index].symbol.span,
                );
            }
        }
        Ok(())
    }

    fn statement_list(
        &mut self,
        pou: &PouRecord<'a>,
        list: &'a AstNode,
        loop_controls: &BTreeSet<String>,
        invalid_return: &mut bool,
    ) -> Result<(), AnalysisInputError> {
        for statement in &list.children {
            match statement.kind {
                AstNodeKind::AssignmentStatement => {
                    self.assignment(pou, statement, loop_controls)?;
                }
                AstNodeKind::FunctionBlockCallStatement => {
                    self.function_block_call(pou, statement, loop_controls)?;
                }
                AstNodeKind::IfStatement => {
                    let condition =
                        required_child(self.sources[pou.source_index].ast, statement, 0)?;
                    self.expression(
                        pou.source_index,
                        condition,
                        Some(pou),
                        Some(TypeValue::Bool),
                        loop_controls,
                    )?;
                    for branch in statement.children.iter().skip(1) {
                        match branch.kind {
                            AstNodeKind::StatementList => {
                                self.statement_list(pou, branch, loop_controls, invalid_return)?;
                            }
                            AstNodeKind::ElsifClause => {
                                let condition =
                                    required_child(self.sources[pou.source_index].ast, branch, 0)?;
                                let body =
                                    required_child(self.sources[pou.source_index].ast, branch, 1)?;
                                self.expression(
                                    pou.source_index,
                                    condition,
                                    Some(pou),
                                    Some(TypeValue::Bool),
                                    loop_controls,
                                )?;
                                self.statement_list(pou, body, loop_controls, invalid_return)?;
                            }
                            AstNodeKind::ElseClause => {
                                let body =
                                    required_child(self.sources[pou.source_index].ast, branch, 0)?;
                                self.statement_list(pou, body, loop_controls, invalid_return)?;
                            }
                            _ => return Err(self.invalid_shape(pou.source_index, branch.span)),
                        }
                    }
                }
                AstNodeKind::ForStatement => {
                    self.for_statement(pou, statement, loop_controls, invalid_return)?;
                }
                AstNodeKind::ReturnStatement => {
                    let function =
                        self.symbols[pou.symbol_index].symbol.kind == SemanticSymbolKind::Function;
                    match (function, statement.children.first()) {
                        (true, Some(value)) => {
                            self.expression(
                                pou.source_index,
                                value,
                                Some(pou),
                                Some(self.symbols[pou.symbol_index].value_type.clone()),
                                loop_controls,
                            )?;
                        }
                        (false, None) => {}
                        _ => {
                            *invalid_return = true;
                            self.emit(
                                pou.source_index,
                                DiagnosticCode::InvalidPouAccess,
                                statement.span,
                            );
                        }
                    }
                }
                _ => return Err(self.invalid_shape(pou.source_index, statement.span)),
            }
        }
        Ok(())
    }

    fn assignment(
        &mut self,
        pou: &PouRecord<'a>,
        statement: &'a AstNode,
        loop_controls: &BTreeSet<String>,
    ) -> Result<(), AnalysisInputError> {
        let target = required_child(self.sources[pou.source_index].ast, statement, 0)?;
        let value = required_child(self.sources[pou.source_index].ast, statement, 1)?;
        let (target_type, target_name) =
            self.assignable(pou.source_index, target, Some(pou), true)?;
        if target_name
            .as_ref()
            .is_some_and(|name| loop_controls.contains(name))
        {
            self.emit(
                pou.source_index,
                DiagnosticCode::InvalidAssignmentTarget,
                target.span,
            );
        }
        self.expression(
            pou.source_index,
            value,
            Some(pou),
            target_type,
            loop_controls,
        )?;
        Ok(())
    }

    fn for_statement(
        &mut self,
        pou: &PouRecord<'a>,
        statement: &'a AstNode,
        loop_controls: &BTreeSet<String>,
        invalid_return: &mut bool,
    ) -> Result<(), AnalysisInputError> {
        let control = required_child(self.sources[pou.source_index].ast, statement, 0)?;
        let spelling = required_text(self.sources[pou.source_index].ast, control)?;
        let canonical = spelling.to_ascii_lowercase();
        let lookup = self.lookup(Some(pou), spelling);
        let control_type =
            self.resolve_lookup(pou.source_index, control.span, lookup, Some(pou), true);
        if control_type
            .as_ref()
            .is_some_and(|value| !self.normalize(value).is_integer())
        {
            self.emit(pou.source_index, DiagnosticCode::TypeMismatch, control.span);
        }
        for bound in statement
            .children
            .iter()
            .skip(1)
            .take(statement.children.len().saturating_sub(2))
        {
            self.expression(
                pou.source_index,
                bound,
                Some(pou),
                control_type.clone(),
                loop_controls,
            )?;
        }
        let mut nested = loop_controls.clone();
        nested.insert(canonical);
        let body = statement
            .children
            .last()
            .ok_or_else(|| self.invalid_shape(pou.source_index, statement.span))?;
        self.statement_list(pou, body, &nested, invalid_return)
    }

    fn expression(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        pou: Option<&PouRecord<'a>>,
        expected: Option<TypeValue>,
        loop_controls: &BTreeSet<String>,
    ) -> Result<ExpressionInfo, AnalysisInputError> {
        let mut info = match node.kind {
            AstNodeKind::Literal => self.literal(source_index, node)?,
            AstNodeKind::QualifiedLiteral => self.qualified_literal(source_index, node)?,
            AstNodeKind::Assignable => {
                let (value_type, _) = self.assignable(source_index, node, pou, false)?;
                ExpressionInfo {
                    inferred: value_type.map_or(InferredType::Unknown, InferredType::Known),
                    integer: None,
                }
            }
            AstNodeKind::ParenthesizedExpression => {
                let child = required_child(self.sources[source_index].ast, node, 0)?;
                self.expression(source_index, child, pou, expected.clone(), loop_controls)?
            }
            AstNodeKind::UnaryExpression => {
                self.unary(source_index, node, pou, expected.as_ref(), loop_controls)?
            }
            AstNodeKind::BinaryExpression => {
                self.binary(source_index, node, pou, expected.clone(), loop_controls)?
            }
            AstNodeKind::CallExpression => {
                self.call(source_index, node, pou, expected.clone(), loop_controls)?
            }
            _ => return Err(self.invalid_shape(source_index, node.span)),
        };
        if !matches!(
            node.kind,
            AstNodeKind::BinaryExpression | AstNodeKind::CallExpression
        ) {
            self.apply_expected(source_index, node.span, &mut info, expected);
        }
        if let InferredType::Known(value_type) = &info.inferred
            && let Some(public) = self.normalize(value_type).public()
        {
            self.expressions.push(TypedExpression {
                source_path: self.sources[source_index].ast.source_path.clone(),
                span: node.span,
                value_type: public,
            });
        }
        Ok(info)
    }

    fn literal(
        &self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<ExpressionInfo, AnalysisInputError> {
        let text = required_text(self.sources[source_index].ast, node)?;
        let upper = text.to_ascii_uppercase();
        if matches!(upper.as_str(), "TRUE" | "FALSE") {
            return Ok(ExpressionInfo {
                inferred: InferredType::Known(TypeValue::Bool),
                integer: None,
            });
        }
        if text.starts_with('\'') || text.starts_with('"') {
            return Ok(ExpressionInfo {
                inferred: InferredType::UntypedString(text.starts_with('"')),
                integer: None,
            });
        }
        if text.contains('.') {
            return Ok(ExpressionInfo {
                inferred: InferredType::UntypedReal,
                integer: None,
            });
        }
        Ok(ExpressionInfo {
            inferred: InferredType::UntypedInteger,
            integer: Some(IntegerConstant {
                negative: false,
                magnitude: parse_magnitude(text),
            }),
        })
    }

    fn qualified_literal(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
    ) -> Result<ExpressionInfo, AnalysisInputError> {
        let qualifier = required_child(self.sources[source_index].ast, node, 0)?;
        let spelling = required_text(self.sources[source_index].ast, qualifier)?;
        let target = elementary_type(spelling).or_else(|| match self.lookup_top(spelling) {
            Lookup::Found(index) if self.symbols[index].symbol.kind == SemanticSymbolKind::Type => {
                self.reference(source_index, qualifier.span, index);
                Some(TypeValue::Named(self.symbols[index].symbol.id))
            }
            Lookup::Suppressed => Some(TypeValue::Unknown),
            Lookup::Found(_) | Lookup::Missing => {
                self.emit(
                    source_index,
                    DiagnosticCode::UndefinedSymbol,
                    qualifier.span,
                );
                Some(TypeValue::Unknown)
            }
        });
        let target = target.unwrap_or(TypeValue::Unknown);
        let value = required_child(self.sources[source_index].ast, node, 1)?;
        let (value_node, negative) = if value.kind == AstNodeKind::UnaryExpression {
            (
                required_child(self.sources[source_index].ast, value, 0)?,
                value.text.as_deref() == Some("-"),
            )
        } else {
            (value, false)
        };
        let raw = required_text(self.sources[source_index].ast, value_node)?;
        let normalized = self.normalize(&target);
        let mut unresolved_member = false;
        let mut valid = match &normalized {
            TypeValue::Bool => matches!(raw.to_ascii_uppercase().as_str(), "TRUE" | "FALSE"),
            TypeValue::Signed(_) | TypeValue::Unsigned(_) => !raw.contains('.'),
            TypeValue::Float(_) => raw.bytes().next().is_some_and(|byte| byte.is_ascii_digit()),
            TypeValue::String { wide, .. } => {
                (*wide && raw.starts_with('"')) || (!*wide && raw.starts_with('\''))
            }
            TypeValue::Enumeration(_) => value_node.kind == AstNodeKind::Identifier,
            TypeValue::Unknown => true,
            _ => false,
        };
        if let TypeValue::Enumeration(Some(owner)) = &normalized
            && value_node.kind == AstNodeKind::Identifier
        {
            let member = raw.to_ascii_lowercase();
            if let Some(index) = self.enum_members.get(&(*owner, member.clone())).copied() {
                self.reference(source_index, value_node.span, index);
            } else if !self.invalid_enum_members.contains(&(*owner, member)) {
                self.emit(
                    source_index,
                    DiagnosticCode::UndefinedSymbol,
                    value_node.span,
                );
                unresolved_member = true;
                valid = false;
            }
        }
        let integer = normalized.is_integer().then(|| IntegerConstant {
            negative,
            magnitude: parse_magnitude(raw),
        });
        if integer.is_some_and(|constant| !integer_fits(constant, &normalized)) {
            valid = false;
        }
        if !valid && !unresolved_member {
            self.emit(
                source_index,
                DiagnosticCode::InvalidExplicitConversion,
                node.span,
            );
        }
        Ok(ExpressionInfo {
            inferred: if target == TypeValue::Unknown || unresolved_member {
                InferredType::Unknown
            } else {
                InferredType::Known(target)
            },
            integer,
        })
    }

    fn unary(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        pou: Option<&PouRecord<'a>>,
        expected: Option<&TypeValue>,
        loop_controls: &BTreeSet<String>,
    ) -> Result<ExpressionInfo, AnalysisInputError> {
        let child = required_child(self.sources[source_index].ast, node, 0)?;
        let operator = required_text(self.sources[source_index].ast, node)?;
        let child_expected = if operator.eq_ignore_ascii_case("NOT") {
            expected.cloned().filter(|value| {
                let normalized = self.normalize(value);
                normalized == TypeValue::Bool || normalized.is_integer()
            })
        } else {
            None
        };
        let mut info = self.expression(source_index, child, pou, child_expected, loop_controls)?;
        if operator == "-"
            && let Some(integer) = &mut info.integer
        {
            integer.negative = !integer.negative;
        }
        if let InferredType::Known(value) = &info.inferred {
            let normalized = self.normalize(value);
            let valid = if operator.eq_ignore_ascii_case("NOT") {
                normalized == TypeValue::Bool || normalized.is_integer()
            } else if operator == "-" {
                matches!(normalized, TypeValue::Signed(_) | TypeValue::Float(_))
            } else {
                normalized.is_numeric()
            };
            if !valid && normalized != TypeValue::Unknown {
                self.emit(source_index, DiagnosticCode::TypeMismatch, node.span);
            }
        }
        Ok(info)
    }

    fn binary(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        pou: Option<&PouRecord<'a>>,
        expected: Option<TypeValue>,
        loop_controls: &BTreeSet<String>,
    ) -> Result<ExpressionInfo, AnalysisInputError> {
        let left_node = required_child(self.sources[source_index].ast, node, 0)?;
        let right_node = required_child(self.sources[source_index].ast, node, 1)?;
        let operator = required_text(self.sources[source_index].ast, node)?.to_ascii_uppercase();
        let comparison = matches!(operator.as_str(), "=" | "<>" | "<" | "<=" | ">" | ">=");
        let operand_expected = (!comparison).then_some(expected.clone()).flatten();
        let mut left = self.expression(source_index, left_node, pou, None, loop_controls)?;
        let mut right = self.expression(source_index, right_node, pou, None, loop_controls)?;
        if known_type(&left.inferred).is_none()
            && let Some(value) = known_type(&right.inferred).or_else(|| operand_expected.clone())
        {
            self.apply_expected(source_index, left_node.span, &mut left, Some(value));
        }
        if known_type(&right.inferred).is_none()
            && let Some(value) = known_type(&left.inferred).or(operand_expected)
        {
            self.apply_expected(source_index, right_node.span, &mut right, Some(value));
        }
        let left_type = known_type(&left.inferred);
        let right_type = known_type(&right.inferred);
        let result = if comparison {
            if left_type.is_none()
                && right_type.is_none()
                && !matches!(left.inferred, InferredType::Unknown)
                && !matches!(right.inferred, InferredType::Unknown)
            {
                self.emit(source_index, DiagnosticCode::AmbiguousLiteral, node.span);
            } else if let (Some(left), Some(right)) = (&left_type, &right_type) {
                let left = self.normalize(left);
                let right = self.normalize(right);
                if !Self::comparable(&left, &right, &operator) {
                    let code = if left.is_numeric() && right.is_numeric() {
                        DiagnosticCode::LossyImplicitConversion
                    } else {
                        DiagnosticCode::TypeMismatch
                    };
                    self.emit(source_index, code, node.span);
                }
            }
            TypeValue::Bool
        } else {
            self.binary_value_type(source_index, node.span, &operator, left_type, right_type)
        };
        let mut info = ExpressionInfo {
            inferred: if result == TypeValue::Unknown {
                InferredType::Unknown
            } else {
                InferredType::Known(result)
            },
            integer: None,
        };
        if !comparison {
            self.apply_expected(source_index, node.span, &mut info, expected);
        } else if expected.is_some_and(|value| self.normalize(&value) != TypeValue::Bool) {
            self.emit(source_index, DiagnosticCode::TypeMismatch, node.span);
        }
        Ok(info)
    }

    fn binary_value_type(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        operator: &str,
        left: Option<TypeValue>,
        right: Option<TypeValue>,
    ) -> TypeValue {
        if left.is_none() || right.is_none() {
            if left.is_none() && right.is_none() {
                self.emit(source_index, DiagnosticCode::AmbiguousLiteral, span);
            }
            return TypeValue::Unknown;
        }
        let left = left.unwrap_or(TypeValue::Unknown);
        let right = right.unwrap_or(TypeValue::Unknown);
        let left = self.normalize(&left);
        let right = self.normalize(&right);
        let common = Self::common_type(&left, &right);
        let operands_are_valid = match operator {
            "AND" | "OR" | "XOR" => {
                (left == TypeValue::Bool && right == TypeValue::Bool)
                    || (left.is_integer() && right.is_integer() && common.is_some())
            }
            "AND_THEN" | "OR_ELSE" => left == TypeValue::Bool && right == TypeValue::Bool,
            "+" | "-" | "*" | "/" => left.is_numeric() && right.is_numeric() && common.is_some(),
            "MOD" => left.is_integer() && right.is_integer() && common.is_some(),
            _ => false,
        };
        if operands_are_valid {
            common.unwrap_or(TypeValue::Bool)
        } else {
            let code = if left.is_numeric() && right.is_numeric() && common.is_none() {
                DiagnosticCode::LossyImplicitConversion
            } else {
                DiagnosticCode::TypeMismatch
            };
            self.emit(source_index, code, span);
            TypeValue::Unknown
        }
    }

    fn call(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        pou: Option<&PouRecord<'a>>,
        expected: Option<TypeValue>,
        loop_controls: &BTreeSet<String>,
    ) -> Result<ExpressionInfo, AnalysisInputError> {
        let name_node = required_child(self.sources[source_index].ast, node, 0)?;
        let first = required_child(self.sources[source_index].ast, name_node, 0)?;
        let spelling = required_text(self.sources[source_index].ast, first)?;
        let args = &node.children[1..];
        if name_node.children.len() > 1 {
            self.emit(source_index, DiagnosticCode::InvalidCall, node.span);
            return Ok(ExpressionInfo {
                inferred: InferredType::Unknown,
                integer: None,
            });
        }
        if is_standard_function(spelling) {
            return self.standard_call(
                source_index,
                node,
                spelling,
                args,
                pou,
                expected,
                loop_controls,
            );
        }
        match self.lookup_top(spelling) {
            Lookup::Found(index)
                if self.symbols[index].symbol.kind == SemanticSymbolKind::Function =>
            {
                self.reference(source_index, first.span, index);
                let target_pou = self
                    .pou_by_symbol
                    .get(&self.symbols[index].symbol.id)
                    .and_then(|pou_index| self.pous.get(*pou_index))
                    .cloned()
                    .ok_or_else(|| self.invalid_shape(source_index, node.span))?;
                if args.len() != target_pou.inputs.len() {
                    self.emit(source_index, DiagnosticCode::InvalidCall, node.span);
                    return Ok(ExpressionInfo {
                        inferred: InferredType::Unknown,
                        integer: None,
                    });
                }
                let diagnostic_count = self.diagnostics.len();
                for (argument, parameter) in args.iter().zip(&target_pou.inputs) {
                    let argument_expected = Some(self.symbols[*parameter].value_type.clone());
                    self.expression(
                        source_index,
                        argument,
                        pou,
                        argument_expected,
                        loop_controls,
                    )?;
                }
                let result = self.symbols[index].value_type.clone();
                let mut info = ExpressionInfo {
                    inferred: InferredType::Known(result),
                    integer: None,
                };
                self.apply_expected(source_index, node.span, &mut info, expected);
                if self.diagnostics.len() == diagnostic_count
                    && let Some(current) = pou
                {
                    self.call_edges.push(CallEdge {
                        from: self.symbols[current.symbol_index].symbol.id,
                        to: self.symbols[index].symbol.id,
                        source_index,
                        span: node.span,
                    });
                }
                Ok(info)
            }
            Lookup::Suppressed => Ok(ExpressionInfo {
                inferred: InferredType::Unknown,
                integer: None,
            }),
            Lookup::Found(_) => {
                self.emit(source_index, DiagnosticCode::InvalidCall, node.span);
                Ok(ExpressionInfo {
                    inferred: InferredType::Unknown,
                    integer: None,
                })
            }
            Lookup::Missing => {
                self.emit(source_index, DiagnosticCode::UndefinedSymbol, first.span);
                Ok(ExpressionInfo {
                    inferred: InferredType::Unknown,
                    integer: None,
                })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn standard_call(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        spelling: &str,
        args: &'a [AstNode],
        pou: Option<&PouRecord<'a>>,
        expected: Option<TypeValue>,
        loop_controls: &BTreeSet<String>,
    ) -> Result<ExpressionInfo, AnalysisInputError> {
        let upper = spelling.to_ascii_uppercase();
        if let Some(target) = conversion_target(&upper) {
            if args.len() != 1 {
                self.emit(source_index, DiagnosticCode::InvalidCall, node.span);
                return Ok(ExpressionInfo {
                    inferred: InferredType::Unknown,
                    integer: None,
                });
            }
            let argument_node = required_child(self.sources[source_index].ast, node, 1)?;
            return self.conversion_call(
                source_index,
                node,
                argument_node,
                target,
                pou,
                expected,
                loop_controls,
            );
        }

        let unary = matches!(
            upper.as_str(),
            "CHECKED_NEG" | "SATURATING_NEG" | "WRAPPING_NEG" | "ABS" | "SQRT"
        );
        let arity = if unary {
            1
        } else if upper == "LIMIT" {
            3
        } else {
            2
        };
        if args.len() != arity {
            self.emit(source_index, DiagnosticCode::InvalidCall, node.span);
            return Ok(ExpressionInfo {
                inferred: InferredType::Unknown,
                integer: None,
            });
        }
        let mut inferred_args = Vec::with_capacity(args.len());
        for argument in args {
            inferred_args.push(self.expression(
                source_index,
                argument,
                pou,
                None,
                loop_controls,
            )?);
        }
        if inferred_args
            .iter()
            .any(|info| matches!(info.inferred, InferredType::Unknown))
        {
            return Ok(ExpressionInfo {
                inferred: InferredType::Unknown,
                integer: None,
            });
        }

        let mut result = None;
        let mut incompatible = false;
        for value in inferred_args
            .iter()
            .filter_map(|info| known_type(&info.inferred))
            .map(|value| self.normalize(&value))
        {
            result = match result {
                None => Some(value),
                Some(current) => Self::common_type(&current, &value).or_else(|| {
                    incompatible = true;
                    None
                }),
            };
        }
        let result = result.or(expected.clone());
        let valid = !incompatible
            && result
                .as_ref()
                .is_some_and(|value| standard_type_supported(&upper, &self.normalize(value)));
        if !valid {
            self.emit(source_index, DiagnosticCode::InvalidCall, node.span);
            return Ok(ExpressionInfo {
                inferred: InferredType::Unknown,
                integer: None,
            });
        }
        let result = result.unwrap_or(TypeValue::Unknown);
        for (argument, info) in args.iter().zip(&mut inferred_args) {
            if !matches!(info.inferred, InferredType::Known(_)) {
                self.apply_expected(source_index, argument.span, info, Some(result.clone()));
            }
        }
        let mut info = ExpressionInfo {
            inferred: InferredType::Known(result),
            integer: None,
        };
        self.apply_expected(source_index, node.span, &mut info, expected);
        Ok(info)
    }

    #[allow(clippy::too_many_arguments)]
    fn conversion_call(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        argument_node: &'a AstNode,
        target: TypeValue,
        pou: Option<&PouRecord<'a>>,
        expected: Option<TypeValue>,
        loop_controls: &BTreeSet<String>,
    ) -> Result<ExpressionInfo, AnalysisInputError> {
        let argument = self.expression(source_index, argument_node, pou, None, loop_controls)?;
        let source_type = known_type(&argument.inferred).map(|value| self.normalize(&value));
        let invalid = match source_type {
            Some(value) => !value.is_numeric(),
            None => matches!(argument.inferred, InferredType::UntypedString(_)),
        } || argument
            .integer
            .is_some_and(|integer| !integer_fits(integer, &target));
        if invalid {
            self.emit(
                source_index,
                DiagnosticCode::InvalidExplicitConversion,
                node.span,
            );
        }
        let mut info = ExpressionInfo {
            inferred: InferredType::Known(target),
            integer: argument.integer,
        };
        self.apply_expected(source_index, node.span, &mut info, expected);
        Ok(info)
    }

    fn function_block_call(
        &mut self,
        pou: &PouRecord<'a>,
        node: &'a AstNode,
        loop_controls: &BTreeSet<String>,
    ) -> Result<(), AnalysisInputError> {
        let invalid_pou_access =
            self.symbols[pou.symbol_index].symbol.kind == SemanticSymbolKind::Function;
        if invalid_pou_access {
            self.emit(
                pou.source_index,
                DiagnosticCode::InvalidPouAccess,
                node.span,
            );
        }
        let name_node = required_child(self.sources[pou.source_index].ast, node, 0)?;
        let first = required_child(self.sources[pou.source_index].ast, name_node, 0)?;
        let spelling = required_text(self.sources[pou.source_index].ast, first)?;
        let lookup = self.lookup(Some(pou), spelling);
        let instance_type =
            self.resolve_lookup(pou.source_index, first.span, lookup, Some(pou), false);
        if name_node.children.len() > 1 {
            if instance_type.as_ref().is_some_and(|value| {
                !matches!(
                    self.normalize(value),
                    TypeValue::Composite(_, _, _) | TypeValue::FunctionBlock(_)
                )
            }) {
                self.emit(pou.source_index, DiagnosticCode::InvalidCall, node.span);
            }
            return Ok(());
        }
        let Some(TypeValue::FunctionBlock(target)) =
            instance_type.as_ref().map(|value| self.normalize(value))
        else {
            if instance_type.is_some() {
                self.emit(pou.source_index, DiagnosticCode::InvalidCall, node.span);
            }
            return Ok(());
        };
        let signature = self
            .pou_by_symbol
            .get(&target)
            .and_then(|index| self.pous.get(*index))
            .cloned()
            .ok_or_else(|| self.invalid_shape(pou.source_index, node.span))?;
        let diagnostic_count = self.diagnostics.len();
        if self.check_function_block_arguments(pou, node, &signature, loop_controls)?
            && !invalid_pou_access
            && self.diagnostics.len() == diagnostic_count
        {
            self.call_edges.push(CallEdge {
                from: self.symbols[pou.symbol_index].symbol.id,
                to: target,
                source_index: pou.source_index,
                span: node.span,
            });
        }
        Ok(())
    }

    fn check_function_block_arguments(
        &mut self,
        pou: &PouRecord<'a>,
        node: &'a AstNode,
        signature: &PouRecord<'a>,
        loop_controls: &BTreeSet<String>,
    ) -> Result<bool, AnalysisInputError> {
        let input_names = signature
            .inputs
            .iter()
            .map(|index| (self.symbols[*index].symbol.canonical_name.clone(), *index))
            .collect::<BTreeMap<_, _>>();
        let output_names = signature
            .outputs
            .iter()
            .map(|index| (self.symbols[*index].symbol.canonical_name.clone(), *index))
            .collect::<BTreeMap<_, _>>();
        let mut supplied_inputs = BTreeSet::new();
        let mut supplied_outputs = BTreeSet::new();
        let mut output_targets = BTreeSet::new();
        let mut invalid_call = false;
        for argument in node.children.iter().skip(1) {
            let name = required_child(self.sources[pou.source_index].ast, argument, 0)?;
            let value = required_child(self.sources[pou.source_index].ast, argument, 1)?;
            let canonical =
                required_text(self.sources[pou.source_index].ast, name)?.to_ascii_lowercase();
            match argument.kind {
                AstNodeKind::InputArgument => {
                    let Some(parameter) = input_names.get(&canonical).copied() else {
                        invalid_call = true;
                        continue;
                    };
                    if !supplied_inputs.insert(canonical) {
                        invalid_call = true;
                        continue;
                    }
                    self.expression(
                        pou.source_index,
                        value,
                        Some(pou),
                        Some(self.symbols[parameter].value_type.clone()),
                        loop_controls,
                    )?;
                }
                AstNodeKind::OutputArgument => {
                    let Some(parameter) = output_names.get(&canonical).copied() else {
                        invalid_call = true;
                        continue;
                    };
                    if !supplied_outputs.insert(canonical) {
                        invalid_call = true;
                        continue;
                    }
                    let target_key = self.sources[pou.source_index].source
                        [value.span.start as usize..value.span.end as usize]
                        .chars()
                        .filter(|character| !character.is_ascii_whitespace())
                        .collect::<String>()
                        .to_ascii_lowercase();
                    if !output_targets.insert(target_key) {
                        invalid_call = true;
                        continue;
                    }
                    let (target_type, _) =
                        self.assignable(pou.source_index, value, Some(pou), true)?;
                    if let Some(target_type) = target_type {
                        let parameter_type = self.symbols[parameter].value_type.clone();
                        self.check_conversion(
                            pou.source_index,
                            value.span,
                            &parameter_type,
                            &target_type,
                        );
                    }
                }
                _ => return Err(self.invalid_shape(pou.source_index, argument.span)),
            }
        }
        for required in input_names.keys() {
            if !supplied_inputs.contains(required) {
                invalid_call = true;
                break;
            }
        }
        if invalid_call {
            self.emit(pou.source_index, DiagnosticCode::InvalidCall, node.span);
        }
        Ok(!invalid_call)
    }

    fn assignable(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        pou: Option<&PouRecord<'a>>,
        write: bool,
    ) -> Result<(Option<TypeValue>, Option<String>), AnalysisInputError> {
        let name_node = if node.kind == AstNodeKind::Assignable {
            required_child(self.sources[source_index].ast, node, 0)?
        } else {
            node
        };
        let first = required_child(self.sources[source_index].ast, name_node, 0)?;
        let spelling = required_text(self.sources[source_index].ast, first)?;
        let canonical = spelling.to_ascii_lowercase();
        let lookup = self.lookup(pou, spelling);
        let result = self.resolve_lookup(source_index, first.span, lookup, pou, write);
        if let Some(value) = &result {
            let normalized = self.normalize(value);
            let has_suffix = node.kind == AstNodeKind::Assignable
                && (node.children.len() > 1 || name_node.children.len() > 1);
            if has_suffix {
                if !matches!(normalized, TypeValue::Composite(_, _, _)) {
                    self.emit(source_index, DiagnosticCode::TypeMismatch, node.span);
                    return Ok((None, Some(canonical)));
                }
                // R1-03 resolves fixed array/member shapes without changing the root binding.
                return Ok((None, Some(canonical)));
            }
            if write && matches!(normalized, TypeValue::FunctionBlock(_)) {
                self.emit(
                    source_index,
                    DiagnosticCode::InvalidAssignmentTarget,
                    node.span,
                );
                return Ok((None, Some(canonical)));
            }
        }
        Ok((result, Some(canonical)))
    }

    fn resolve_lookup(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        lookup: Lookup,
        pou: Option<&PouRecord<'a>>,
        write: bool,
    ) -> Option<TypeValue> {
        match lookup {
            Lookup::Found(index) => {
                let kind = self.symbols[index].symbol.kind;
                let is_value = matches!(
                    kind,
                    SemanticSymbolKind::GlobalVariable
                        | SemanticSymbolKind::InputVariable
                        | SemanticSymbolKind::OutputVariable
                        | SemanticSymbolKind::LocalVariable
                        | SemanticSymbolKind::TemporaryVariable
                );
                if !is_value {
                    self.emit(
                        source_index,
                        if write {
                            DiagnosticCode::InvalidAssignmentTarget
                        } else {
                            DiagnosticCode::TypeMismatch
                        },
                        span,
                    );
                    return None;
                }
                if pou.is_some_and(|current| {
                    self.symbols[current.symbol_index].symbol.kind == SemanticSymbolKind::Function
                        && kind == SemanticSymbolKind::GlobalVariable
                }) {
                    self.emit(source_index, DiagnosticCode::InvalidPouAccess, span);
                }
                self.reference(source_index, span, index);
                Some(self.symbols[index].value_type.clone())
            }
            Lookup::Suppressed => None,
            Lookup::Missing => {
                self.emit(source_index, DiagnosticCode::UndefinedSymbol, span);
                None
            }
        }
    }

    fn apply_expected(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        info: &mut ExpressionInfo,
        expected: Option<TypeValue>,
    ) {
        let Some(expected) = expected else {
            return;
        };
        match &info.inferred {
            InferredType::Known(actual) => {
                self.check_conversion(source_index, span, actual, &expected);
            }
            InferredType::UntypedInteger => {
                let normalized = self.normalize(&expected);
                if !normalized.is_numeric() {
                    self.emit(source_index, DiagnosticCode::TypeMismatch, span);
                } else if info
                    .integer
                    .is_some_and(|constant| !integer_fits(constant, &normalized))
                {
                    self.emit(
                        source_index,
                        DiagnosticCode::InvalidExplicitConversion,
                        span,
                    );
                } else {
                    info.inferred = InferredType::Known(expected);
                }
            }
            InferredType::UntypedReal => {
                if matches!(self.normalize(&expected), TypeValue::Float(_)) {
                    info.inferred = InferredType::Known(expected);
                } else {
                    self.emit(source_index, DiagnosticCode::TypeMismatch, span);
                }
            }
            InferredType::UntypedString(wide) => {
                if matches!(
                    self.normalize(&expected),
                    TypeValue::String {
                        wide: expected_wide,
                        ..
                    } if expected_wide == *wide
                ) {
                    info.inferred = InferredType::Known(expected);
                } else {
                    self.emit(source_index, DiagnosticCode::TypeMismatch, span);
                }
            }
            InferredType::Unknown => {}
        }
    }

    fn check_conversion(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        actual: &TypeValue,
        expected: &TypeValue,
    ) {
        let actual = self.normalize(actual);
        let expected = self.normalize(expected);
        if actual == TypeValue::Unknown || expected == TypeValue::Unknown || actual == expected {
            return;
        }
        if implicit_conversion(&actual, &expected) {
            return;
        }
        self.emit(
            source_index,
            if actual.is_numeric() && expected.is_numeric() {
                DiagnosticCode::LossyImplicitConversion
            } else {
                DiagnosticCode::TypeMismatch
            },
            span,
        );
    }

    fn comparable(left: &TypeValue, right: &TypeValue, operator: &str) -> bool {
        if left == right {
            return matches!(operator, "=" | "<>") || left.is_numeric();
        }
        left.is_numeric() && right.is_numeric() && Self::common_type(left, right).is_some()
    }

    fn common_type(left: &TypeValue, right: &TypeValue) -> Option<TypeValue> {
        if left == right {
            return Some(left.clone());
        }
        match (left, right) {
            (TypeValue::Signed(left), TypeValue::Signed(right)) => {
                Some(TypeValue::Signed((*left).max(*right)))
            }
            (TypeValue::Unsigned(left), TypeValue::Unsigned(right)) => {
                Some(TypeValue::Unsigned((*left).max(*right)))
            }
            (TypeValue::Float(left), TypeValue::Float(right)) => {
                Some(TypeValue::Float((*left).max(*right)))
            }
            (TypeValue::Signed(signed), TypeValue::Unsigned(unsigned))
            | (TypeValue::Unsigned(unsigned), TypeValue::Signed(signed)) => [8_u8, 16, 32, 64]
                .into_iter()
                .find(|candidate| candidate >= signed && candidate > unsigned)
                .map(TypeValue::Signed),
            _ => None,
        }
    }

    fn normalize(&self, value: &TypeValue) -> TypeValue {
        let mut current = value.clone();
        let mut visited = BTreeSet::new();
        while let TypeValue::Named(id) = current {
            if !visited.insert(id) {
                return TypeValue::Named(id);
            }
            let Some(record) = self.symbols.get(id.0 as usize) else {
                return TypeValue::Unknown;
            };
            current = record.value_type.clone();
        }
        current
    }

    fn lookup(&self, pou: Option<&PouRecord<'a>>, spelling: &str) -> Lookup {
        let canonical = spelling.to_ascii_lowercase();
        if let Some(pou) = pou {
            if let Some(index) = pou.locals.get(&canonical) {
                return Lookup::Found(*index);
            }
            if pou.invalid_locals.contains(&canonical) {
                return Lookup::Suppressed;
            }
        }
        self.lookup_top(spelling)
    }

    fn lookup_top(&self, spelling: &str) -> Lookup {
        let canonical = spelling.to_ascii_lowercase();
        self.top.get(&canonical).map_or_else(
            || {
                if self.invalid_top.contains(&canonical) {
                    Lookup::Suppressed
                } else {
                    Lookup::Missing
                }
            },
            |index| Lookup::Found(*index),
        )
    }

    fn reference(&mut self, source_index: usize, span: SourceSpan, symbol_index: usize) {
        self.references.push(ResolvedReference {
            source_path: self.sources[source_index].ast.source_path.clone(),
            span,
            symbol: self.symbols[symbol_index].symbol.id,
        });
    }

    fn report_recursive_calls(&mut self) {
        let edges = self.call_edges.clone();
        for edge in edges {
            let mut visited = BTreeSet::new();
            if reachable(edge.to, edge.from, &self.call_edges, &mut visited) {
                self.emit(edge.source_index, DiagnosticCode::RecursiveCall, edge.span);
            }
        }
    }

    fn emit(&mut self, source_index: usize, code: DiagnosticCode, span: SourceSpan) {
        let key = (source_index, span.start, span.end, code.as_str());
        if !self.diagnostic_keys.insert(key) {
            return;
        }
        let source = self.sources[source_index];
        self.diagnostics.push(make_diagnostic(
            &source.ast.source_path,
            source.source,
            code,
            span,
        ));
    }

    fn next_symbol_id(&self) -> Result<SymbolId, AnalysisInputError> {
        u32::try_from(self.symbols.len())
            .map(SymbolId)
            .map_err(|_| AnalysisInputError::TooManySymbols)
    }

    fn invalid_shape(&self, source_index: usize, span: SourceSpan) -> AnalysisInputError {
        AnalysisInputError::InvalidAstShape {
            source_path: self.sources[source_index].ast.source_path.clone(),
            span_start: span.start,
            span_end: span.end,
        }
    }
}

fn validate_source(source: &SemanticSource<'_>) -> Result<(), AnalysisInputError> {
    let version = source.ast.schema_version;
    if version.major != AST_SCHEMA_MAJOR || version.minor != AST_SCHEMA_MINOR {
        return Err(AnalysisInputError::UnsupportedAstVersion {
            major: version.major,
            minor: version.minor,
        });
    }
    if source.ast.root.kind != AstNodeKind::CompilationUnit {
        return Err(invalid_shape_for(source, source.ast.root.span));
    }
    if source.ast.root.span.start != 0
        || usize::try_from(source.ast.root.span.end).ok() != Some(source.source.len())
    {
        return Err(invalid_span_for(source, source.ast.root.span));
    }
    validate_node(source, &source.ast.root, None)
}

fn validate_node(
    source: &SemanticSource<'_>,
    node: &AstNode,
    parent: Option<SourceSpan>,
) -> Result<(), AnalysisInputError> {
    let start =
        usize::try_from(node.span.start).map_err(|_| invalid_span_for(source, node.span))?;
    let end = usize::try_from(node.span.end).map_err(|_| invalid_span_for(source, node.span))?;
    let contained =
        parent.is_none_or(|parent| parent.start <= node.span.start && node.span.end <= parent.end);
    if start > end
        || end > source.source.len()
        || !source.source.is_char_boundary(start)
        || !source.source.is_char_boundary(end)
        || !contained
    {
        return Err(invalid_span_for(source, node.span));
    }
    for child in &node.children {
        validate_node(source, child, Some(node.span))?;
    }
    Ok(())
}

fn required_child<'a>(
    ast: &VersionedAst,
    node: &'a AstNode,
    index: usize,
) -> Result<&'a AstNode, AnalysisInputError> {
    node.children
        .get(index)
        .ok_or_else(|| AnalysisInputError::InvalidAstShape {
            source_path: ast.source_path.clone(),
            span_start: node.span.start,
            span_end: node.span.end,
        })
}

fn required_text<'a>(ast: &VersionedAst, node: &'a AstNode) -> Result<&'a str, AnalysisInputError> {
    node.text
        .as_deref()
        .ok_or_else(|| AnalysisInputError::InvalidAstShape {
            source_path: ast.source_path.clone(),
            span_start: node.span.start,
            span_end: node.span.end,
        })
}

fn invalid_shape_for(source: &SemanticSource<'_>, span: SourceSpan) -> AnalysisInputError {
    AnalysisInputError::InvalidAstShape {
        source_path: source.ast.source_path.clone(),
        span_start: span.start,
        span_end: span.end,
    }
}

fn invalid_span_for(source: &SemanticSource<'_>, span: SourceSpan) -> AnalysisInputError {
    AnalysisInputError::InvalidSourceSpan {
        source_path: source.ast.source_path.clone(),
        span_start: span.start,
        span_end: span.end,
    }
}

fn elementary_type(value: &str) -> Option<TypeValue> {
    Some(match value.to_ascii_uppercase().as_str() {
        "BOOL" => TypeValue::Bool,
        "SINT" => TypeValue::Signed(8),
        "INT" => TypeValue::Signed(16),
        "DINT" => TypeValue::Signed(32),
        "LINT" => TypeValue::Signed(64),
        "USINT" => TypeValue::Unsigned(8),
        "UINT" => TypeValue::Unsigned(16),
        "UDINT" => TypeValue::Unsigned(32),
        "ULINT" => TypeValue::Unsigned(64),
        "REAL" => TypeValue::Float(32),
        "LREAL" => TypeValue::Float(64),
        "STRING" => TypeValue::String {
            wide: false,
            capacity: String::new(),
        },
        "WSTRING" => TypeValue::String {
            wide: true,
            capacity: String::new(),
        },
        _ => return None,
    })
}

fn implicit_conversion(actual: &TypeValue, expected: &TypeValue) -> bool {
    match (actual, expected) {
        (TypeValue::Signed(left) | TypeValue::Unsigned(left), TypeValue::Signed(right))
        | (TypeValue::Unsigned(left), TypeValue::Unsigned(right))
        | (TypeValue::Float(left), TypeValue::Float(right)) => left < right,
        _ => false,
    }
}

fn known_type(inferred: &InferredType) -> Option<TypeValue> {
    match inferred {
        InferredType::Known(value) => Some(value.clone()),
        _ => None,
    }
}

fn parse_magnitude(value: &str) -> Option<u128> {
    if let Some(hex) = value.strip_prefix("16#") {
        u128::from_str_radix(hex, 16).ok()
    } else if let Some(binary) = value.strip_prefix("2#") {
        u128::from_str_radix(binary, 2).ok()
    } else {
        value.parse::<u128>().ok()
    }
}

fn integer_fits(value: IntegerConstant, target: &TypeValue) -> bool {
    let Some(magnitude) = value.magnitude else {
        return false;
    };
    match target {
        TypeValue::Signed(bits) => {
            let sign_limit = 1_u128 << (u32::from(*bits) - 1);
            if value.negative {
                magnitude <= sign_limit
            } else {
                magnitude < sign_limit
            }
        }
        TypeValue::Unsigned(bits) => {
            !value.negative
                && if *bits == 64 {
                    magnitude <= u128::from(u64::MAX)
                } else {
                    magnitude < (1_u128 << u32::from(*bits))
                }
        }
        TypeValue::Float(_) => true,
        _ => false,
    }
}

fn conversion_target(name: &str) -> Option<TypeValue> {
    let suffix = name.strip_prefix("TO_")?;
    elementary_type(suffix).filter(TypeValue::is_numeric)
}

fn standard_type_supported(name: &str, value_type: &TypeValue) -> bool {
    match name {
        "CHECKED_ADD" | "CHECKED_SUB" | "CHECKED_MUL" | "CHECKED_NEG" | "SATURATING_ADD"
        | "SATURATING_SUB" | "SATURATING_MUL" | "SATURATING_NEG" | "WRAPPING_ADD"
        | "WRAPPING_SUB" | "WRAPPING_MUL" | "WRAPPING_NEG" => value_type.is_integer(),
        "ABS" => matches!(value_type, TypeValue::Signed(_) | TypeValue::Float(_)),
        "SQRT" => matches!(value_type, TypeValue::Float(_)),
        "MIN" | "MAX" | "LIMIT" => value_type.is_scalar() && value_type != &TypeValue::Bool,
        "CONCAT" => matches!(value_type, TypeValue::String { .. }),
        _ => false,
    }
}

fn is_standard_function(value: &str) -> bool {
    let upper = value.to_ascii_uppercase();
    conversion_target(&upper).is_some()
        || matches!(
            upper.as_str(),
            "CHECKED_ADD"
                | "CHECKED_SUB"
                | "CHECKED_MUL"
                | "CHECKED_NEG"
                | "SATURATING_ADD"
                | "SATURATING_SUB"
                | "SATURATING_MUL"
                | "SATURATING_NEG"
                | "WRAPPING_ADD"
                | "WRAPPING_SUB"
                | "WRAPPING_MUL"
                | "WRAPPING_NEG"
                | "MIN"
                | "MAX"
                | "LIMIT"
                | "ABS"
                | "SQRT"
                | "CONCAT"
        )
}

fn is_reserved(value: &str) -> bool {
    value.starts_with("__aurora_") || is_standard_function(value)
}

fn guarantees_return(list: &AstNode) -> bool {
    list.children.iter().any(statement_guarantees_return)
}

fn statement_guarantees_return(statement: &AstNode) -> bool {
    if statement.kind == AstNodeKind::ReturnStatement && !statement.children.is_empty() {
        return true;
    }
    if statement.kind != AstNodeKind::IfStatement {
        return false;
    }
    let mut has_else = false;
    for branch in statement.children.iter().skip(1) {
        let body = match branch.kind {
            AstNodeKind::StatementList => branch,
            AstNodeKind::ElsifClause => match branch.children.get(1) {
                Some(value) => value,
                None => return false,
            },
            AstNodeKind::ElseClause => {
                has_else = true;
                match branch.children.first() {
                    Some(value) => value,
                    None => return false,
                }
            }
            _ => return false,
        };
        if !guarantees_return(body) {
            return false;
        }
    }
    has_else
}

fn reachable(
    current: SymbolId,
    target: SymbolId,
    edges: &[CallEdge],
    visited: &mut BTreeSet<SymbolId>,
) -> bool {
    if current == target {
        return true;
    }
    if !visited.insert(current) {
        return false;
    }
    edges
        .iter()
        .filter(|edge| edge.from == current)
        .any(|edge| reachable(edge.to, target, edges, visited))
}

fn sort_diagnostics(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by(|left, right| {
        left.source_path
            .as_bytes()
            .cmp(right.source_path.as_bytes())
            .then(left.span.cmp(&right.span))
            .then(left.code.as_str().cmp(right.code.as_str()))
    });
}

fn reference_order(left: &ResolvedReference, right: &ResolvedReference) -> std::cmp::Ordering {
    left.source_path
        .as_bytes()
        .cmp(right.source_path.as_bytes())
        .then(left.span.cmp(&right.span))
        .then(left.symbol.cmp(&right.symbol))
}

fn expression_order(left: &TypedExpression, right: &TypedExpression) -> std::cmp::Ordering {
    left.source_path
        .as_bytes()
        .cmp(right.source_path.as_bytes())
        .then(left.span.cmp(&right.span))
}
