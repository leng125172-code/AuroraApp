use std::str;

use thiserror::Error;

use crate::ast::{AstNode, AstNodeKind, AstVersion, VersionedAst};
use crate::diagnostic::make_diagnostic;
use crate::lexer::{Keyword, LexOutput, Token, TokenKind, lex};
use crate::{Diagnostic, DiagnosticCode, SourceSpan};

/// Validated resource limits for one source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserLimits {
    source_bytes: usize,
    tokens: usize,
    ast_nodes: usize,
    nesting_depth: usize,
}

impl ParserLimits {
    /// Validates all mandatory limits before any source allocation or parsing occurs.
    ///
    /// # Errors
    ///
    /// Returns [`LimitConfigurationError`] if any limit is zero or source byte offsets would not
    /// fit the frozen `u32` span representation.
    pub const fn new(
        max_source_bytes: usize,
        max_tokens: usize,
        max_ast_nodes: usize,
        max_nesting_depth: usize,
    ) -> Result<Self, LimitConfigurationError> {
        if max_source_bytes == 0 {
            return Err(LimitConfigurationError::ZeroSourceBytes);
        }
        if max_source_bytes > u32::MAX as usize {
            return Err(LimitConfigurationError::SourceSpanOverflow);
        }
        if max_tokens == 0 {
            return Err(LimitConfigurationError::ZeroTokens);
        }
        if max_ast_nodes == 0 {
            return Err(LimitConfigurationError::ZeroAstNodes);
        }
        if max_nesting_depth == 0 {
            return Err(LimitConfigurationError::ZeroNestingDepth);
        }
        Ok(Self {
            source_bytes: max_source_bytes,
            tokens: max_tokens,
            ast_nodes: max_ast_nodes,
            nesting_depth: max_nesting_depth,
        })
    }

    /// Maximum accepted UTF-8 bytes in one file.
    #[must_use]
    pub const fn max_source_bytes(self) -> usize {
        self.source_bytes
    }

    /// Maximum non-trivia tokens in one file, excluding EOF.
    #[must_use]
    pub const fn max_tokens(self) -> usize {
        self.tokens
    }

    /// Maximum AST nodes in one successful file.
    #[must_use]
    pub const fn max_ast_nodes(self) -> usize {
        self.ast_nodes
    }

    /// Maximum recursive grammar nesting depth.
    #[must_use]
    pub const fn max_nesting_depth(self) -> usize {
        self.nesting_depth
    }
}

/// Invalid or unrepresentable parser-limit configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LimitConfigurationError {
    /// Source byte limit is zero.
    #[error("Aurora ST max_source_bytes must be non-zero")]
    ZeroSourceBytes,
    /// Token limit is zero.
    #[error("Aurora ST max_tokens must be non-zero")]
    ZeroTokens,
    /// AST node limit is zero.
    #[error("Aurora ST max_ast_nodes must be non-zero")]
    ZeroAstNodes,
    /// Nesting limit is zero.
    #[error("Aurora ST max_nesting_depth must be non-zero")]
    ZeroNestingDepth,
    /// Source offsets would not fit the AST span representation.
    #[error("Aurora ST max_source_bytes must fit a u32 source span")]
    SourceSpanOverflow,
}

/// Atomic result of parsing one source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseOutput {
    /// AST is present only when there are no diagnostics.
    pub ast: Option<VersionedAst>,
    /// Deterministically sorted diagnostics. Any entry prevents AST publication.
    pub diagnostics: Vec<Diagnostic>,
}

/// Lexes and parses exactly Aurora ST Preview 1.0.
///
/// `source_path` is retained verbatim and is expected to have already passed the project path
/// normalization boundary. Parsing is host-only and bounded by `limits`. Syntax recovery may
/// discover independent later errors, but a failed call never returns a partial AST.
#[must_use]
pub fn parse(source_path: &str, source_bytes: &[u8], limits: ParserLimits) -> ParseOutput {
    if source_bytes.len() > limits.max_source_bytes() {
        let span_end = source_bytes.len().min(u32::MAX as usize);
        return failure(
            source_path,
            "",
            DiagnosticCode::SourceLimitExceeded,
            SourceSpan::new(limits.max_source_bytes(), span_end),
        );
    }
    let source = match str::from_utf8(source_bytes) {
        Ok(value) if !value.starts_with('\u{feff}') => value,
        Ok(value) => {
            return failure(
                source_path,
                value,
                DiagnosticCode::InvalidEncoding,
                SourceSpan::new(0, 3.min(value.len())),
            );
        }
        Err(error) => {
            let start = error.valid_up_to();
            let end = error
                .error_len()
                .map_or(source_bytes.len(), |len| start + len);
            return failure(
                source_path,
                "",
                DiagnosticCode::InvalidEncoding,
                SourceSpan::new(start, end),
            );
        }
    };

    let lexical = lex(source_path, source, limits);
    if lexical.limit_exceeded {
        return ParseOutput {
            ast: None,
            diagnostics: sorted(lexical.diagnostics),
        };
    }
    Parser::new(source_path, source, limits, lexical).run()
}

fn failure(source_path: &str, source: &str, code: DiagnosticCode, span: SourceSpan) -> ParseOutput {
    ParseOutput {
        ast: None,
        diagnostics: vec![make_diagnostic(source_path, source, code, span)],
    }
}

fn sorted(mut diagnostics: Vec<Diagnostic>) -> Vec<Diagnostic> {
    diagnostics.sort_by(|left, right| {
        left.source_path
            .as_bytes()
            .cmp(right.source_path.as_bytes())
            .then(left.span.cmp(&right.span))
            .then(left.code.cmp(&right.code))
    });
    diagnostics
}

struct Parser<'a> {
    source_path: &'a str,
    source: &'a str,
    limits: ParserLimits,
    tokens: Vec<Token>,
    index: usize,
    diagnostics: Vec<Diagnostic>,
    leading_comment: bool,
    node_count: usize,
    hard_stop: bool,
    last_primary_token: Option<usize>,
}

impl<'a> Parser<'a> {
    fn new(
        source_path: &'a str,
        source: &'a str,
        limits: ParserLimits,
        lexical: LexOutput,
    ) -> Self {
        Self {
            source_path,
            source,
            limits,
            tokens: lexical.tokens,
            index: 0,
            diagnostics: lexical.diagnostics,
            leading_comment: lexical.leading_comment,
            node_count: 0,
            hard_stop: false,
            last_primary_token: None,
        }
    }

    fn run(mut self) -> ParseOutput {
        let start = 0;
        let mut children = Vec::new();
        if let Some(version) = self.parse_version() {
            children.push(version);
        }
        while !self.at(TokenKind::Eof) && !self.hard_stop {
            let before = self.index;
            if let Some(declaration) = self.parse_top_level(1) {
                children.push(declaration);
            } else {
                self.recover_top_level();
            }
            if self.index == before {
                self.bump();
            }
        }
        let end = self.current().span.end;
        let root = self.node(
            AstNodeKind::CompilationUnit,
            SourceSpan { start, end },
            None,
            children,
        );
        let diagnostics = sorted(self.diagnostics);
        let ast = if diagnostics.is_empty() {
            root.map(|root| VersionedAst {
                schema_version: AstVersion::preview_v1_0(),
                source_path: self.source_path.to_owned(),
                root,
            })
        } else {
            None
        };
        ParseOutput { ast, diagnostics }
    }

    fn parse_version(&mut self) -> Option<AstNode> {
        let start = self.current().span.start;
        let starts_directive = self.at_keyword(Keyword::AuroraSt);
        if self.leading_comment || !starts_directive {
            self.report(
                DiagnosticCode::UnsupportedLanguageVersion,
                self.current().span,
            );
            if !starts_directive {
                return None;
            }
        }
        self.bump();
        let valid_version_keyword = self.take_keyword(Keyword::Version).is_some();
        let valid_number = self.at(TokenKind::Real) && self.current().text == "1.0";
        if valid_number {
            self.bump();
        }
        let valid_terminator = self.take(TokenKind::Semicolon).is_some();
        if !valid_version_keyword || !valid_number || !valid_terminator {
            self.report(
                DiagnosticCode::UnsupportedLanguageVersion,
                SourceSpan {
                    start,
                    end: self.previous_end(),
                },
            );
            self.recover_to_semicolon();
        }
        self.node(
            AstNodeKind::VersionDirective,
            SourceSpan {
                start,
                end: self.previous_end(),
            },
            Some("1.0".to_owned()),
            Vec::new(),
        )
    }

    fn parse_top_level(&mut self, depth: usize) -> Option<AstNode> {
        if !self.check_depth(depth) {
            return None;
        }
        if self.at_keyword(Keyword::Unsupported) {
            let span = self.bump().span;
            self.report(DiagnosticCode::UnsupportedConstruct, span);
            return None;
        }
        if self.at_keyword(Keyword::AuroraSt) {
            let span = self.bump().span;
            self.report(DiagnosticCode::UnsupportedLanguageVersion, span);
            self.recover_to_semicolon();
            return None;
        }
        match self.keyword() {
            Some(Keyword::Type) => self.parse_type_block(depth + 1),
            Some(Keyword::VarGlobal) => self.parse_global_block(depth + 1),
            Some(Keyword::Function) => self.parse_function(depth + 1),
            Some(Keyword::FunctionBlock) => self.parse_pou(
                depth + 1,
                Keyword::FunctionBlock,
                Keyword::EndFunctionBlock,
                AstNodeKind::FunctionBlockDeclaration,
            ),
            Some(Keyword::Program) => self.parse_pou(
                depth + 1,
                Keyword::Program,
                Keyword::EndProgram,
                AstNodeKind::ProgramDeclaration,
            ),
            _ => {
                let span = self.current().span;
                self.report(DiagnosticCode::UnexpectedToken, span);
                None
            }
        }
    }

    fn parse_type_block(&mut self, depth: usize) -> Option<AstNode> {
        let start = self.bump().span.start;
        let mut declarations = Vec::new();
        let had_declaration_syntax = !self.at_keyword(Keyword::EndType);
        while !self.at_keyword(Keyword::EndType)
            && !self.at(TokenKind::Eof)
            && !self.is_declaration_recovery_boundary(Keyword::EndType)
            && !self.hard_stop
        {
            let before = self.index;
            if let Some(declaration) = self.parse_type_declaration(depth) {
                declarations.push(declaration);
            } else {
                self.recover_declaration(Keyword::EndType);
            }
            if before == self.index {
                self.bump();
            }
        }
        self.require_nonempty(had_declaration_syntax);
        self.expect_end(Keyword::EndType);
        self.node_from_start(AstNodeKind::TypeBlock, start, None, declarations)
    }

    fn parse_type_declaration(&mut self, depth: usize) -> Option<AstNode> {
        let start = self.current().span.start;
        let name = self.parse_identifier()?;
        self.expect(TokenKind::Colon);
        let mut children = vec![name, self.parse_type_specification(depth + 1)?];
        if self.take(TokenKind::Assign).is_some() {
            children.push(self.parse_expression(depth + 1)?);
        }
        self.expect_terminator();
        self.node_from_start(AstNodeKind::TypeDeclaration, start, None, children)
    }

    fn parse_type_specification(&mut self, depth: usize) -> Option<AstNode> {
        if !self.check_depth(depth) {
            return None;
        }
        let start = self.current().span.start;
        match self.keyword() {
            Some(keyword) if keyword.is_elementary_type() => {
                let token = self.bump().clone();
                self.node(
                    AstNodeKind::ElementaryType,
                    token.span,
                    Some(token.text),
                    Vec::new(),
                )
            }
            Some(Keyword::String | Keyword::Wstring) => {
                let keyword = self.bump().clone();
                self.expect(TokenKind::LeftBracket);
                let capacity = self.parse_positive_decimal()?;
                self.expect(TokenKind::RightBracket);
                self.node_from_start(
                    AstNodeKind::StringType,
                    start,
                    Some(keyword.text),
                    vec![capacity],
                )
            }
            Some(Keyword::Array) => {
                self.bump();
                self.expect(TokenKind::LeftBracket);
                let lower = self.parse_expression(depth + 1)?;
                self.expect(TokenKind::Range);
                let upper = self.parse_expression(depth + 1)?;
                self.expect(TokenKind::RightBracket);
                self.expect_keyword(Keyword::Of);
                let element = self.parse_type_specification(depth + 1)?;
                self.node_from_start(
                    AstNodeKind::ArrayType,
                    start,
                    None,
                    vec![lower, upper, element],
                )
            }
            Some(Keyword::Struct) => self.parse_structure_type(start, depth),
            _ if self.at(TokenKind::LeftParen) => {
                self.bump();
                let mut items = Vec::new();
                loop {
                    let item_start = self.current().span.start;
                    let name = self.parse_identifier()?;
                    let mut children = vec![name];
                    if self.take(TokenKind::Assign).is_some() {
                        children.push(self.parse_expression(depth + 1)?);
                    }
                    items.push(self.node_from_start(
                        AstNodeKind::EnumerationItem,
                        item_start,
                        None,
                        children,
                    )?);
                    if self.take(TokenKind::Comma).is_none() {
                        break;
                    }
                }
                self.expect(TokenKind::RightParen);
                self.node_from_start(AstNodeKind::EnumerationType, start, None, items)
            }
            _ if self.at(TokenKind::Identifier) => {
                let token = self.bump().clone();
                self.node(
                    AstNodeKind::NamedType,
                    token.span,
                    Some(token.text),
                    Vec::new(),
                )
            }
            _ => {
                self.unexpected();
                None
            }
        }
    }

    fn parse_structure_type(&mut self, start: u32, depth: usize) -> Option<AstNode> {
        self.bump();
        let mut fields = Vec::new();
        let had_field_syntax = !self.at_keyword(Keyword::EndStruct);
        while !self.at_keyword(Keyword::EndStruct)
            && !self.at(TokenKind::Eof)
            && !self.is_declaration_recovery_boundary(Keyword::EndStruct)
            && !self.hard_stop
        {
            let field_start = self.current().span.start;
            let name = self.parse_identifier()?;
            self.expect(TokenKind::Colon);
            let mut children = vec![name, self.parse_type_specification(depth + 1)?];
            if self.take(TokenKind::Assign).is_some() {
                children.push(self.parse_expression(depth + 1)?);
            }
            self.expect_terminator();
            fields.push(self.node_from_start(
                AstNodeKind::StructureField,
                field_start,
                None,
                children,
            )?);
        }
        self.require_nonempty(had_field_syntax);
        self.expect_end(Keyword::EndStruct);
        self.node_from_start(AstNodeKind::StructureType, start, None, fields)
    }

    fn parse_global_block(&mut self, depth: usize) -> Option<AstNode> {
        let start = self.bump().span.start;
        let mut declarations = Vec::new();
        let had_declaration_syntax = !self.at_keyword(Keyword::EndVar);
        while !self.at_keyword(Keyword::EndVar)
            && !self.at(TokenKind::Eof)
            && !self.is_declaration_recovery_boundary(Keyword::EndVar)
            && !self.hard_stop
        {
            let before = self.index;
            if let Some(declaration) = self.parse_global_declaration(depth) {
                declarations.push(declaration);
            } else {
                self.recover_declaration(Keyword::EndVar);
            }
            if before == self.index {
                self.bump();
            }
        }
        self.require_nonempty(had_declaration_syntax);
        self.expect_end(Keyword::EndVar);
        self.node_from_start(AstNodeKind::GlobalVariableBlock, start, None, declarations)
    }

    fn parse_global_declaration(&mut self, depth: usize) -> Option<AstNode> {
        let start = self.current().span.start;
        let name = self.parse_identifier()?;
        self.expect_keyword(Keyword::At);
        let address = if self.at(TokenKind::DirectAddress) || self.at(TokenKind::Invalid) {
            let token = self.bump().clone();
            self.node(
                AstNodeKind::DirectAddress,
                token.span,
                Some(token.text),
                Vec::new(),
            )?
        } else if let Some(vendor_end) = self.vendor_address_end() {
            let vendor_start = self.current().span.start;
            while self.index < vendor_end {
                self.bump();
            }
            let span = SourceSpan {
                start: vendor_start,
                end: self.previous_end(),
            };
            self.report(DiagnosticCode::VendorAddressInSource, span);
            self.node(
                AstNodeKind::DirectAddress,
                span,
                Some(self.source[span.start as usize..span.end as usize].to_owned()),
                Vec::new(),
            )?
        } else {
            self.unexpected();
            return None;
        };
        self.expect(TokenKind::Colon);
        let mut children = vec![name, address, self.parse_type_specification(depth + 1)?];
        if self.take(TokenKind::Assign).is_some() {
            children.push(self.parse_expression(depth + 1)?);
        }
        self.expect_terminator();
        self.node_from_start(
            AstNodeKind::GlobalVariableDeclaration,
            start,
            None,
            children,
        )
    }

    fn parse_function(&mut self, depth: usize) -> Option<AstNode> {
        let start = self.bump().span.start;
        let name = self.parse_identifier()?;
        self.expect(TokenKind::Colon);
        let mut children = vec![name, self.parse_type_specification(depth + 1)?];
        while self.is_function_variable_block_start() {
            children.push(self.parse_variable_block(depth + 1)?);
        }
        children.push(self.parse_statement_list(depth + 1, &[Keyword::EndFunction])?);
        self.expect_end(Keyword::EndFunction);
        self.node_from_start(AstNodeKind::FunctionDeclaration, start, None, children)
    }

    fn parse_pou(
        &mut self,
        depth: usize,
        start_keyword: Keyword,
        end_keyword: Keyword,
        kind: AstNodeKind,
    ) -> Option<AstNode> {
        debug_assert!(self.at_keyword(start_keyword));
        let start = self.bump().span.start;
        let name = self.parse_identifier()?;
        let mut children = vec![name];
        while self.is_stateful_variable_block_start() {
            children.push(self.parse_variable_block(depth + 1)?);
        }
        children.push(self.parse_statement_list(depth + 1, &[end_keyword])?);
        self.expect_end(end_keyword);
        self.node_from_start(kind, start, None, children)
    }

    fn parse_variable_block(&mut self, depth: usize) -> Option<AstNode> {
        let start = self.current().span.start;
        let kind = match self.keyword()? {
            Keyword::VarInput => AstNodeKind::InputVariableBlock,
            Keyword::VarOutput => AstNodeKind::OutputVariableBlock,
            Keyword::Var => AstNodeKind::LocalVariableBlock,
            Keyword::VarTemp => AstNodeKind::TemporaryVariableBlock,
            _ => return None,
        };
        self.bump();
        let mut declarations = Vec::new();
        let had_declaration_syntax = !self.at_keyword(Keyword::EndVar);
        while !self.at_keyword(Keyword::EndVar)
            && !self.at(TokenKind::Eof)
            && !self.is_declaration_recovery_boundary(Keyword::EndVar)
            && !self.hard_stop
        {
            let before = self.index;
            if let Some(declaration) = self.parse_variable_declaration(depth) {
                declarations.push(declaration);
            } else {
                self.recover_declaration(Keyword::EndVar);
            }
            if before == self.index {
                self.bump();
            }
        }
        self.require_nonempty(had_declaration_syntax);
        self.expect_end(Keyword::EndVar);
        self.node_from_start(kind, start, None, declarations)
    }

    fn parse_variable_declaration(&mut self, depth: usize) -> Option<AstNode> {
        let start = self.current().span.start;
        let mut names = vec![self.parse_identifier()?];
        while self.take(TokenKind::Comma).is_some() {
            names.push(self.parse_identifier()?);
        }
        let name_list = self.node_from_children(AstNodeKind::IdentifierList, None, names)?;
        self.expect(TokenKind::Colon);
        let mut children = vec![name_list, self.parse_type_specification(depth + 1)?];
        if self.take(TokenKind::Assign).is_some() {
            children.push(self.parse_expression(depth + 1)?);
        }
        self.expect_terminator();
        self.node_from_start(AstNodeKind::VariableDeclaration, start, None, children)
    }

    fn parse_statement_list(&mut self, depth: usize, stops: &[Keyword]) -> Option<AstNode> {
        if !self.check_depth(depth) {
            return None;
        }
        let start = self.current().span.start;
        let mut statements = Vec::new();
        while !self.at(TokenKind::Eof)
            && !stops.iter().any(|keyword| self.at_keyword(*keyword))
            && !self.is_statement_recovery_boundary()
            && !self.hard_stop
        {
            let before = self.index;
            if let Some(statement) = self.parse_statement(depth + 1) {
                statements.push(statement);
            } else {
                self.recover_statement(stops);
            }
            if before == self.index {
                self.bump();
            }
        }
        let end = statements
            .last()
            .map_or(start, |statement| statement.span.end);
        self.node(
            AstNodeKind::StatementList,
            SourceSpan { start, end },
            None,
            statements,
        )
    }

    fn parse_statement(&mut self, depth: usize) -> Option<AstNode> {
        if self.at_keyword(Keyword::Unsupported) {
            let span = self.bump().span;
            self.report(DiagnosticCode::UnsupportedConstruct, span);
            return None;
        }
        match self.keyword() {
            Some(Keyword::If) => self.parse_if(depth + 1),
            Some(Keyword::For) => self.parse_for(depth + 1),
            Some(Keyword::Return) => self.parse_return(depth + 1),
            _ if self.at(TokenKind::Identifier) => self.parse_named_statement(depth + 1),
            _ => {
                self.unexpected();
                None
            }
        }
    }

    fn parse_named_statement(&mut self, depth: usize) -> Option<AstNode> {
        let start = self.current().span.start;
        let name = self.parse_qualified_identifier()?;
        if self.take(TokenKind::LeftParen).is_some() {
            let mut children = vec![name];
            if !self.at(TokenKind::RightParen) {
                loop {
                    let argument_start = self.current().span.start;
                    let argument_name = self.parse_identifier()?;
                    let kind = if self.take(TokenKind::Assign).is_some() {
                        AstNodeKind::InputArgument
                    } else if self.take(TokenKind::BindOutput).is_some() {
                        AstNodeKind::OutputArgument
                    } else {
                        self.unexpected();
                        return None;
                    };
                    let value = if kind == AstNodeKind::InputArgument {
                        self.parse_expression(depth + 1)?
                    } else {
                        self.parse_assignable(depth + 1)?
                    };
                    children.push(self.node_from_start(
                        kind,
                        argument_start,
                        None,
                        vec![argument_name, value],
                    )?);
                    if self.take(TokenKind::Comma).is_none() {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RightParen);
            self.expect_terminator();
            return self.node_from_start(
                AstNodeKind::FunctionBlockCallStatement,
                start,
                None,
                children,
            );
        }
        let target = self.parse_assignable_suffixes(depth + 1, name)?;
        self.expect(TokenKind::Assign);
        let value = self.parse_expression(depth + 1)?;
        self.expect_terminator();
        self.node_from_start(
            AstNodeKind::AssignmentStatement,
            start,
            None,
            vec![target, value],
        )
    }

    fn parse_if(&mut self, depth: usize) -> Option<AstNode> {
        let start = self.bump().span.start;
        let condition = self.parse_expression(depth + 1)?;
        self.expect_keyword(Keyword::Then);
        let mut children = vec![
            condition,
            self.parse_statement_list(depth + 1, &[Keyword::Elsif, Keyword::Else, Keyword::EndIf])?,
        ];
        while self.take_keyword(Keyword::Elsif).is_some() {
            let clause_start = self.previous_start();
            let condition = self.parse_expression(depth + 1)?;
            self.expect_keyword(Keyword::Then);
            let body = self.parse_statement_list(
                depth + 1,
                &[Keyword::Elsif, Keyword::Else, Keyword::EndIf],
            )?;
            children.push(self.node_from_start(
                AstNodeKind::ElsifClause,
                clause_start,
                None,
                vec![condition, body],
            )?);
        }
        if self.take_keyword(Keyword::Else).is_some() {
            let clause_start = self.previous_start();
            let body = self.parse_statement_list(depth + 1, &[Keyword::EndIf])?;
            children.push(self.node_from_start(
                AstNodeKind::ElseClause,
                clause_start,
                None,
                vec![body],
            )?);
        }
        self.expect_end(Keyword::EndIf);
        self.expect_terminator();
        self.node_from_start(AstNodeKind::IfStatement, start, None, children)
    }

    fn parse_for(&mut self, depth: usize) -> Option<AstNode> {
        let start = self.bump().span.start;
        let control = self.parse_identifier()?;
        self.expect(TokenKind::Assign);
        let initial = self.parse_expression(depth + 1)?;
        self.expect_keyword(Keyword::To);
        let end = self.parse_expression(depth + 1)?;
        let mut children = vec![control, initial, end];
        if self.take_keyword(Keyword::By).is_some() {
            children.push(self.parse_expression(depth + 1)?);
        }
        self.expect_keyword(Keyword::Do);
        children.push(self.parse_statement_list(depth + 1, &[Keyword::EndFor])?);
        self.expect_end(Keyword::EndFor);
        self.expect_terminator();
        self.node_from_start(AstNodeKind::ForStatement, start, None, children)
    }

    fn parse_return(&mut self, depth: usize) -> Option<AstNode> {
        let start = self.bump().span.start;
        let mut children = Vec::new();
        if !self.at(TokenKind::Semicolon) {
            children.push(self.parse_expression(depth + 1)?);
        }
        self.expect_terminator();
        self.node_from_start(AstNodeKind::ReturnStatement, start, None, children)
    }

    fn parse_expression(&mut self, depth: usize) -> Option<AstNode> {
        self.parse_binary(depth, 1)
    }

    fn parse_binary(&mut self, depth: usize, minimum_precedence: u8) -> Option<AstNode> {
        if !self.check_depth(depth) {
            return None;
        }
        let mut left = self.parse_unary(depth + 1)?;
        while let Some((precedence, comparison)) = binary_precedence(self.current().kind) {
            if precedence < minimum_precedence {
                break;
            }
            let operator = self.bump().clone();
            let right = self.parse_binary(depth + 1, precedence + 1)?;
            let span = SourceSpan {
                start: left.span.start,
                end: right.span.end,
            };
            left = self.node(
                AstNodeKind::BinaryExpression,
                span,
                Some(operator.text),
                vec![left, right],
            )?;
            if comparison {
                break;
            }
        }
        Some(left)
    }

    fn parse_unary(&mut self, depth: usize) -> Option<AstNode> {
        if matches!(
            self.current().kind,
            TokenKind::Plus | TokenKind::Minus | TokenKind::Keyword(Keyword::Not)
        ) {
            let operator = self.bump().clone();
            let expression = self.parse_primary(depth + 1)?;
            let span = SourceSpan {
                start: operator.span.start,
                end: expression.span.end,
            };
            return self.node(
                AstNodeKind::UnaryExpression,
                span,
                Some(operator.text),
                vec![expression],
            );
        }
        self.parse_primary(depth + 1)
    }

    fn parse_primary(&mut self, depth: usize) -> Option<AstNode> {
        if !self.check_depth(depth) {
            return None;
        }
        if self.is_literal() {
            return self.parse_literal();
        }
        if self.is_literal_qualifier() && self.peek_at(1, TokenKind::Hash) {
            return self.parse_qualified_literal();
        }
        if self.at(TokenKind::Identifier) {
            let name = self.parse_qualified_identifier()?;
            if self.take(TokenKind::LeftParen).is_some() {
                let start = name.span.start;
                let mut children = vec![name];
                if !self.at(TokenKind::RightParen) {
                    loop {
                        children.push(self.parse_expression(depth + 1)?);
                        if self.take(TokenKind::Comma).is_none() {
                            break;
                        }
                    }
                }
                self.expect(TokenKind::RightParen);
                return self.node_from_start(AstNodeKind::CallExpression, start, None, children);
            }
            return self.parse_assignable_suffixes(depth + 1, name);
        }
        if self.take(TokenKind::LeftParen).is_some() {
            let start = self.previous_start();
            let expression = self.parse_expression(depth + 1)?;
            self.expect(TokenKind::RightParen);
            return self.node_from_start(
                AstNodeKind::ParenthesizedExpression,
                start,
                None,
                vec![expression],
            );
        }
        if self.at(TokenKind::Invalid) {
            self.bump();
            return None;
        }
        self.unexpected();
        None
    }

    fn parse_qualified_literal(&mut self) -> Option<AstNode> {
        let start = self.current().span.start;
        let qualifier_token = self.bump().clone();
        let qualifier = self.node(
            AstNodeKind::Identifier,
            qualifier_token.span,
            Some(qualifier_token.text),
            Vec::new(),
        )?;
        self.expect(TokenKind::Hash);
        let mut children = vec![qualifier];
        if matches!(self.current().kind, TokenKind::Plus | TokenKind::Minus) {
            let sign = self.bump().clone();
            let literal = self.parse_numeric_literal()?;
            let span = SourceSpan {
                start: sign.span.start,
                end: literal.span.end,
            };
            children.push(self.node(
                AstNodeKind::UnaryExpression,
                span,
                Some(sign.text),
                vec![literal],
            )?);
        } else {
            children.push(self.parse_literal_or_identifier()?);
        }
        self.node_from_start(AstNodeKind::QualifiedLiteral, start, None, children)
    }

    fn parse_literal_or_identifier(&mut self) -> Option<AstNode> {
        if self.is_literal() {
            self.parse_literal()
        } else {
            self.parse_identifier()
        }
    }

    fn parse_literal(&mut self) -> Option<AstNode> {
        let token = self.bump().clone();
        self.node(
            AstNodeKind::Literal,
            token.span,
            Some(token.text),
            Vec::new(),
        )
    }

    fn parse_positive_decimal(&mut self) -> Option<AstNode> {
        let text = &self.current().text;
        if !self.at(TokenKind::Integer)
            || text.is_empty()
            || !matches!(text.as_bytes()[0], b'1'..=b'9')
            || !text.bytes().all(|byte| byte.is_ascii_digit())
        {
            self.unexpected();
            return None;
        }
        self.parse_literal()
    }

    fn parse_numeric_literal(&mut self) -> Option<AstNode> {
        if !matches!(self.current().kind, TokenKind::Integer | TokenKind::Real) {
            self.unexpected();
            return None;
        }
        self.parse_literal()
    }

    fn parse_assignable(&mut self, depth: usize) -> Option<AstNode> {
        let name = self.parse_qualified_identifier()?;
        self.parse_assignable_suffixes(depth, name)
    }

    fn parse_assignable_suffixes(&mut self, depth: usize, name: AstNode) -> Option<AstNode> {
        let start = name.span.start;
        let mut children = vec![name];
        loop {
            if self.take(TokenKind::LeftBracket).is_some() {
                let suffix_start = self.previous_start();
                let index = self.parse_expression(depth + 1)?;
                self.expect(TokenKind::RightBracket);
                children.push(self.node_from_start(
                    AstNodeKind::IndexSuffix,
                    suffix_start,
                    None,
                    vec![index],
                )?);
            } else if self.take(TokenKind::Dot).is_some() {
                let suffix_start = self.previous_start();
                let field = self.parse_identifier()?;
                children.push(self.node_from_start(
                    AstNodeKind::FieldSuffix,
                    suffix_start,
                    None,
                    vec![field],
                )?);
            } else {
                break;
            }
        }
        self.node_from_start(AstNodeKind::Assignable, start, None, children)
    }

    fn parse_qualified_identifier(&mut self) -> Option<AstNode> {
        let start = self.current().span.start;
        let mut children = vec![self.parse_identifier()?];
        while self.at(TokenKind::Dot) && self.peek_at(1, TokenKind::Identifier) {
            self.bump();
            children.push(self.parse_identifier()?);
        }
        self.node_from_start(AstNodeKind::QualifiedIdentifier, start, None, children)
    }

    fn parse_identifier(&mut self) -> Option<AstNode> {
        if !self.at(TokenKind::Identifier) {
            self.unexpected();
            return None;
        }
        let token = self.bump().clone();
        self.node(
            AstNodeKind::Identifier,
            token.span,
            Some(token.text),
            Vec::new(),
        )
    }

    fn is_literal(&self) -> bool {
        matches!(
            self.current().kind,
            TokenKind::Integer
                | TokenKind::Real
                | TokenKind::String
                | TokenKind::Wstring
                | TokenKind::Keyword(Keyword::True | Keyword::False)
        )
    }

    fn is_literal_qualifier(&self) -> bool {
        self.at(TokenKind::Identifier) || self.keyword().is_some_and(Keyword::is_literal_qualifier)
    }

    fn is_function_variable_block_start(&self) -> bool {
        matches!(
            self.keyword(),
            Some(Keyword::VarInput | Keyword::Var | Keyword::VarTemp)
        )
    }

    fn is_stateful_variable_block_start(&self) -> bool {
        matches!(
            self.keyword(),
            Some(Keyword::VarInput | Keyword::VarOutput | Keyword::Var | Keyword::VarTemp)
        )
    }

    fn is_declaration_recovery_boundary(&self, expected_end: Keyword) -> bool {
        if expected_end == Keyword::EndVar && self.starts_named_statement() {
            return true;
        }
        self.keyword().is_some_and(|keyword| {
            keyword != expected_end
                && matches!(
                    keyword,
                    Keyword::Type
                        | Keyword::VarGlobal
                        | Keyword::Function
                        | Keyword::FunctionBlock
                        | Keyword::Program
                        | Keyword::EndType
                        | Keyword::EndStruct
                        | Keyword::EndVar
                        | Keyword::EndFunction
                        | Keyword::EndFunctionBlock
                        | Keyword::EndProgram
                        | Keyword::If
                        | Keyword::For
                        | Keyword::Return
                )
        })
    }

    fn starts_named_statement(&self) -> bool {
        if !self.at(TokenKind::Identifier) {
            return false;
        }
        let mut cursor = self.index + 1;
        while self
            .tokens
            .get(cursor)
            .is_some_and(|token| token.kind == TokenKind::Dot)
            && self
                .tokens
                .get(cursor + 1)
                .is_some_and(|token| token.kind == TokenKind::Identifier)
        {
            cursor += 2;
        }
        self.tokens.get(cursor).is_some_and(|token| {
            matches!(
                token.kind,
                TokenKind::Assign | TokenKind::LeftParen | TokenKind::LeftBracket
            )
        })
    }

    fn is_statement_recovery_boundary(&self) -> bool {
        matches!(
            self.keyword(),
            Some(
                Keyword::Elsif
                    | Keyword::Else
                    | Keyword::EndIf
                    | Keyword::EndFor
                    | Keyword::EndFunction
                    | Keyword::EndFunctionBlock
                    | Keyword::EndProgram
                    | Keyword::EndVar
                    | Keyword::EndType
                    | Keyword::EndStruct
            )
        )
    }

    fn vendor_address_end(&self) -> Option<usize> {
        if self.at(TokenKind::Integer) {
            return Some(self.index + 1);
        }
        if !self.at(TokenKind::Identifier) {
            return None;
        }
        let upper = self.current().text.to_ascii_uppercase();
        if upper.strip_prefix('D').is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        }) {
            return Some(self.index + 1);
        }
        let siemens_head = upper.strip_prefix("DB").is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        });
        if siemens_head && self.peek_at(1, TokenKind::Dot) && self.peek_at(2, TokenKind::Identifier)
        {
            return Some(self.index + 3);
        }
        None
    }

    fn require_nonempty(&mut self, had_entry_syntax: bool) {
        if !had_entry_syntax {
            self.report(DiagnosticCode::UnexpectedToken, self.current().span);
        }
    }

    fn expect(&mut self, kind: TokenKind) {
        if self.take(kind).is_none() {
            self.unexpected();
        }
    }

    fn expect_keyword(&mut self, keyword: Keyword) {
        if self.take_keyword(keyword).is_none() {
            self.unexpected();
        }
    }

    fn expect_end(&mut self, keyword: Keyword) {
        if self.take_keyword(keyword).is_none() {
            self.report(DiagnosticCode::MissingTerminator, self.current().span);
        }
    }

    fn expect_terminator(&mut self) {
        if self.take(TokenKind::Semicolon).is_none() {
            self.report(DiagnosticCode::MissingTerminator, self.current().span);
        }
    }

    fn recover_to_semicolon(&mut self) {
        while !self.at(TokenKind::Semicolon) && !self.at(TokenKind::Eof) {
            self.bump();
        }
        self.take(TokenKind::Semicolon);
    }

    fn recover_top_level(&mut self) {
        while !self.at(TokenKind::Eof) {
            if matches!(
                self.keyword(),
                Some(
                    Keyword::Type
                        | Keyword::VarGlobal
                        | Keyword::Function
                        | Keyword::FunctionBlock
                        | Keyword::Program
                        | Keyword::AuroraSt
                )
            ) {
                return;
            }
            self.bump();
        }
    }

    fn recover_declaration(&mut self, end: Keyword) {
        while !self.at(TokenKind::Eof)
            && !self.at_keyword(end)
            && !self.is_declaration_recovery_boundary(end)
        {
            if self.take(TokenKind::Semicolon).is_some() {
                return;
            }
            self.bump();
        }
    }

    fn recover_statement(&mut self, stops: &[Keyword]) {
        while !self.at(TokenKind::Eof)
            && !stops.iter().any(|keyword| self.at_keyword(*keyword))
            && !self.is_statement_recovery_boundary()
        {
            if self.take(TokenKind::Semicolon).is_some() {
                return;
            }
            self.bump();
        }
    }

    fn unexpected(&mut self) {
        if self.at_keyword(Keyword::Unsupported) {
            let span = self.current().span;
            self.report(DiagnosticCode::UnsupportedConstruct, span);
        } else if !self.at(TokenKind::Invalid) {
            let span = self.current().span;
            self.report(DiagnosticCode::UnexpectedToken, span);
        }
    }

    fn report(&mut self, code: DiagnosticCode, span: SourceSpan) {
        if matches!(
            code,
            DiagnosticCode::UnexpectedToken | DiagnosticCode::MissingTerminator
        ) && self.last_primary_token == Some(self.index)
        {
            return;
        }
        if matches!(
            code,
            DiagnosticCode::UnexpectedToken | DiagnosticCode::MissingTerminator
        ) {
            self.last_primary_token = Some(self.index);
        }
        self.diagnostics
            .push(make_diagnostic(self.source_path, self.source, code, span));
    }

    fn check_depth(&mut self, depth: usize) -> bool {
        if depth <= self.limits.max_nesting_depth() {
            return true;
        }
        if !self.hard_stop {
            self.report(DiagnosticCode::SourceLimitExceeded, self.current().span);
            self.hard_stop = true;
        }
        false
    }

    fn node(
        &mut self,
        kind: AstNodeKind,
        span: SourceSpan,
        text: Option<String>,
        children: Vec<AstNode>,
    ) -> Option<AstNode> {
        if self.node_count >= self.limits.max_ast_nodes() {
            if !self.hard_stop {
                self.report(DiagnosticCode::SourceLimitExceeded, span);
                self.hard_stop = true;
            }
            return None;
        }
        self.node_count += 1;
        Some(AstNode {
            kind,
            span,
            text,
            children,
        })
    }

    fn node_from_start(
        &mut self,
        kind: AstNodeKind,
        start: u32,
        text: Option<String>,
        children: Vec<AstNode>,
    ) -> Option<AstNode> {
        self.node(
            kind,
            SourceSpan {
                start,
                end: self.previous_end(),
            },
            text,
            children,
        )
    }

    fn node_from_children(
        &mut self,
        kind: AstNodeKind,
        text: Option<String>,
        children: Vec<AstNode>,
    ) -> Option<AstNode> {
        let first = children.first()?;
        let last = children.last()?;
        self.node(
            kind,
            SourceSpan {
                start: first.span.start,
                end: last.span.end,
            },
            text,
            children,
        )
    }

    fn take(&mut self, kind: TokenKind) -> Option<&Token> {
        if self.at(kind) {
            Some(self.bump())
        } else {
            None
        }
    }

    fn take_keyword(&mut self, keyword: Keyword) -> Option<&Token> {
        if self.at_keyword(keyword) {
            Some(self.bump())
        } else {
            None
        }
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.current().kind == kind
    }

    fn at_keyword(&self, keyword: Keyword) -> bool {
        self.current().kind == TokenKind::Keyword(keyword)
    }

    fn peek_at(&self, offset: usize, kind: TokenKind) -> bool {
        self.tokens
            .get(self.index + offset)
            .is_some_and(|token| token.kind == kind)
    }

    fn keyword(&self) -> Option<Keyword> {
        match self.current().kind {
            TokenKind::Keyword(keyword) => Some(keyword),
            _ => None,
        }
    }

    fn current(&self) -> &Token {
        &self.tokens[self.index.min(self.tokens.len() - 1)]
    }

    fn bump(&mut self) -> &Token {
        let current = self.index;
        if !self.at(TokenKind::Eof) {
            self.index += 1;
        }
        &self.tokens[current]
    }

    fn previous_start(&self) -> u32 {
        self.tokens
            .get(self.index.saturating_sub(1))
            .map_or(self.current().span.start, |token| token.span.start)
    }

    fn previous_end(&self) -> u32 {
        self.tokens
            .get(self.index.saturating_sub(1))
            .map_or(self.current().span.start, |token| token.span.end)
    }
}

const fn binary_precedence(kind: TokenKind) -> Option<(u8, bool)> {
    match kind {
        TokenKind::Keyword(Keyword::OrElse) => Some((1, false)),
        TokenKind::Keyword(Keyword::Xor) => Some((2, false)),
        TokenKind::Keyword(Keyword::Or) => Some((3, false)),
        TokenKind::Keyword(Keyword::AndThen) => Some((4, false)),
        TokenKind::Keyword(Keyword::And) => Some((5, false)),
        TokenKind::Equal
        | TokenKind::NotEqual
        | TokenKind::Less
        | TokenKind::LessEqual
        | TokenKind::Greater
        | TokenKind::GreaterEqual => Some((6, true)),
        TokenKind::Plus | TokenKind::Minus => Some((7, false)),
        TokenKind::Star | TokenKind::Slash | TokenKind::Keyword(Keyword::Mod) => Some((8, false)),
        _ => None,
    }
}
