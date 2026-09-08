use crate::diagnostic::make_diagnostic;
use crate::{Diagnostic, DiagnosticCode, ParserLimits, SourceSpan};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keyword {
    AuroraSt,
    Version,
    Type,
    EndType,
    Struct,
    EndStruct,
    Array,
    Of,
    VarGlobal,
    VarInput,
    VarOutput,
    Var,
    VarTemp,
    EndVar,
    Function,
    EndFunction,
    FunctionBlock,
    EndFunctionBlock,
    Program,
    EndProgram,
    If,
    Then,
    Elsif,
    Else,
    EndIf,
    For,
    To,
    By,
    Do,
    EndFor,
    Return,
    OrElse,
    Xor,
    Or,
    AndThen,
    And,
    Mod,
    Not,
    At,
    True,
    False,
    Bool,
    Sint,
    Int,
    Dint,
    Lint,
    Usint,
    Uint,
    Udint,
    Ulint,
    Real,
    Lreal,
    String,
    Wstring,
    Unsupported,
}

impl Keyword {
    pub(crate) const fn is_elementary_type(self) -> bool {
        matches!(
            self,
            Self::Bool
                | Self::Sint
                | Self::Int
                | Self::Dint
                | Self::Lint
                | Self::Usint
                | Self::Uint
                | Self::Udint
                | Self::Ulint
                | Self::Real
                | Self::Lreal
        )
    }

    pub(crate) const fn is_literal_qualifier(self) -> bool {
        self.is_elementary_type() || matches!(self, Self::String | Self::Wstring)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Identifier,
    Integer,
    Real,
    String,
    Wstring,
    DirectAddress,
    Keyword(Keyword),
    Assign,
    BindOutput,
    LessEqual,
    GreaterEqual,
    NotEqual,
    Range,
    Colon,
    Semicolon,
    Comma,
    Dot,
    Hash,
    LeftParen,
    RightParen,
    LeftBracket,
    RightBracket,
    Plus,
    Minus,
    Star,
    Slash,
    Equal,
    Less,
    Greater,
    Invalid,
    Eof,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub(crate) kind: TokenKind,
    pub(crate) span: SourceSpan,
    pub(crate) text: String,
}

pub(crate) struct LexOutput {
    pub(crate) tokens: Vec<Token>,
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) leading_comment: bool,
    pub(crate) limit_exceeded: bool,
}

pub(crate) fn lex(source_path: &str, source: &str, limits: ParserLimits) -> LexOutput {
    Lexer {
        source_path,
        source,
        bytes: source.as_bytes(),
        limits,
        index: 0,
        tokens: Vec::new(),
        diagnostics: Vec::new(),
        leading_comment: false,
        emitted_non_trivia: false,
        limit_exceeded: false,
    }
    .run()
}

struct Lexer<'a> {
    source_path: &'a str,
    source: &'a str,
    bytes: &'a [u8],
    limits: ParserLimits,
    index: usize,
    tokens: Vec<Token>,
    diagnostics: Vec<Diagnostic>,
    leading_comment: bool,
    emitted_non_trivia: bool,
    limit_exceeded: bool,
}

impl Lexer<'_> {
    fn run(mut self) -> LexOutput {
        while self.index < self.bytes.len() && !self.limit_exceeded {
            if self.skip_trivia() {
                continue;
            }
            let start = self.index;
            let byte = self.bytes[start];
            if byte.is_ascii_alphabetic() || byte == b'_' {
                self.lex_word(start);
            } else if byte.is_ascii_digit() {
                self.lex_number(start);
            } else {
                match byte {
                    b'\'' | b'"' => self.lex_string(start, byte),
                    b'%' => self.lex_address(start),
                    b':' => self.simple_or_pair(start, b'=', TokenKind::Assign, TokenKind::Colon),
                    b'=' => {
                        self.simple_or_pair(start, b'>', TokenKind::BindOutput, TokenKind::Equal);
                    }
                    b'<' => self.lex_less(start),
                    b'>' => self.simple_or_pair(
                        start,
                        b'=',
                        TokenKind::GreaterEqual,
                        TokenKind::Greater,
                    ),
                    b'.' => self.simple_or_pair(start, b'.', TokenKind::Range, TokenKind::Dot),
                    b';' => self.simple(start, TokenKind::Semicolon),
                    b',' => self.simple(start, TokenKind::Comma),
                    b'#' => self.simple(start, TokenKind::Hash),
                    b'(' => self.simple(start, TokenKind::LeftParen),
                    b')' => self.simple(start, TokenKind::RightParen),
                    b'[' => self.simple(start, TokenKind::LeftBracket),
                    b']' => self.simple(start, TokenKind::RightBracket),
                    b'+' => self.simple(start, TokenKind::Plus),
                    b'-' => self.simple(start, TokenKind::Minus),
                    b'*' => self.simple(start, TokenKind::Star),
                    b'/' => self.simple(start, TokenKind::Slash),
                    _ => {
                        self.consume_invalid_scalar_region();
                        self.emit(TokenKind::Invalid, start, self.index);
                        self.diagnostic(
                            DiagnosticCode::InvalidToken,
                            SourceSpan::new(start, self.index),
                        );
                    }
                }
            }
        }
        let eof = self.bytes.len();
        self.tokens.push(Token {
            kind: TokenKind::Eof,
            span: SourceSpan::new(eof, eof),
            text: String::new(),
        });
        LexOutput {
            tokens: self.tokens,
            diagnostics: self.diagnostics,
            leading_comment: self.leading_comment,
            limit_exceeded: self.limit_exceeded,
        }
    }

    fn skip_trivia(&mut self) -> bool {
        if self.bytes[self.index].is_ascii_whitespace() {
            self.index += 1;
            return true;
        }
        if self.bytes[self.index..].starts_with(b"//") {
            if !self.emitted_non_trivia {
                self.leading_comment = true;
            }
            self.index += 2;
            while self.index < self.bytes.len() && self.bytes[self.index] != b'\n' {
                self.index += 1;
            }
            return true;
        }
        if self.bytes[self.index..].starts_with(b"(*") {
            let start = self.index;
            if !self.emitted_non_trivia {
                self.leading_comment = true;
            }
            self.index += 2;
            let mut recovery_depth = 1_usize;
            let mut nested_reported = false;
            while self.index + 1 < self.bytes.len() {
                if self.bytes[self.index..].starts_with(b"(*") {
                    if !nested_reported {
                        self.diagnostic(
                            DiagnosticCode::InvalidToken,
                            SourceSpan::new(self.index, self.index + 2),
                        );
                        nested_reported = true;
                    }
                    recovery_depth += 1;
                    self.index += 2;
                } else if self.bytes[self.index..].starts_with(b"*)") {
                    recovery_depth -= 1;
                    self.index += 2;
                    if recovery_depth == 0 {
                        return true;
                    }
                } else {
                    self.index += 1;
                }
            }
            self.index = self.bytes.len();
            self.diagnostic(
                DiagnosticCode::UnterminatedComment,
                SourceSpan::new(start, (start + 2).min(self.bytes.len())),
            );
            return true;
        }
        false
    }

    fn lex_word(&mut self, start: usize) {
        self.index += 1;
        while self
            .bytes
            .get(self.index)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        {
            self.index += 1;
        }
        let text = &self.source[start..self.index];
        let kind = if text.len() > 128 {
            self.diagnostic(
                DiagnosticCode::InvalidToken,
                SourceSpan::new(start, self.index),
            );
            TokenKind::Invalid
        } else if let Some(keyword) = keyword(text) {
            TokenKind::Keyword(keyword)
        } else {
            TokenKind::Identifier
        };
        self.emit(kind, start, self.index);
    }

    fn consume_invalid_scalar_region(&mut self) {
        let Some(first) = self.source[self.index..].chars().next() else {
            return;
        };
        self.index += first.len_utf8();
        if first.is_ascii() {
            return;
        }
        while let Some(value) = self.source[self.index..].chars().next() {
            if value.is_ascii() {
                break;
            }
            self.index += value.len_utf8();
        }
    }

    fn lex_number(&mut self, start: usize) {
        while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
            self.index += 1;
        }
        let digit_end = self.index;
        if self.bytes.get(self.index) == Some(&b'#')
            && (&self.source[start..digit_end] == "2" || &self.source[start..digit_end] == "16")
        {
            self.index += 1;
            let value_start = self.index;
            let radix = &self.source[start..digit_end];
            while self.bytes.get(self.index).is_some_and(|byte| {
                if radix == "2" {
                    matches!(byte, b'0' | b'1')
                } else {
                    byte.is_ascii_hexdigit()
                }
            }) {
                self.index += 1;
            }
            let valid = self.index > value_start
                && !self
                    .bytes
                    .get(self.index)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
            if valid {
                self.emit(TokenKind::Integer, start, self.index);
            } else {
                self.consume_word_tail();
                self.emit_invalid_number(start);
            }
            return;
        }

        let mut kind = TokenKind::Integer;
        if self.bytes.get(self.index) == Some(&b'.')
            && self.bytes.get(self.index + 1) != Some(&b'.')
        {
            self.index += 1;
            let fraction_start = self.index;
            while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
                self.index += 1;
            }
            if self.index == fraction_start {
                self.emit_invalid_number(start);
                return;
            }
            kind = TokenKind::Real;
            if matches!(self.bytes.get(self.index), Some(b'e' | b'E')) {
                self.index += 1;
                if matches!(self.bytes.get(self.index), Some(b'+' | b'-')) {
                    self.index += 1;
                }
                let exponent_start = self.index;
                while self.bytes.get(self.index).is_some_and(u8::is_ascii_digit) {
                    self.index += 1;
                }
                if self.index == exponent_start {
                    self.emit_invalid_number(start);
                    return;
                }
            }
        }
        let leading_zero = digit_end - start > 1 && self.bytes[start] == b'0';
        let invalid_tail = self
            .bytes
            .get(self.index)
            .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_' || *byte == b'#');
        if leading_zero || invalid_tail {
            self.consume_word_tail();
            self.emit_invalid_number(start);
        } else {
            self.emit(kind, start, self.index);
        }
    }

    fn consume_word_tail(&mut self) {
        while self
            .bytes
            .get(self.index)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'#' | b'.'))
        {
            self.index += 1;
        }
    }

    fn emit_invalid_number(&mut self, start: usize) {
        self.emit(TokenKind::Invalid, start, self.index);
        self.diagnostic(
            DiagnosticCode::InvalidToken,
            SourceSpan::new(start, self.index),
        );
    }

    fn lex_string(&mut self, start: usize, quote: u8) {
        self.index += 1;
        while self.index < self.bytes.len() {
            if self.bytes[self.index] == quote {
                self.index += 1;
                let kind = if quote == b'\'' {
                    TokenKind::String
                } else {
                    TokenKind::Wstring
                };
                self.emit(kind, start, self.index);
                return;
            }
            if matches!(self.bytes[self.index], b'\r' | b'\n') {
                break;
            }
            if self.bytes[self.index] == b'\\' {
                let escape_start = self.index;
                if !self.consume_escape(quote) {
                    self.diagnostic(
                        DiagnosticCode::InvalidEscape,
                        SourceSpan::new(escape_start, self.index.min(self.bytes.len())),
                    );
                }
                continue;
            }
            self.index += self.source[self.index..]
                .chars()
                .next()
                .map_or(1, char::len_utf8);
        }
        self.emit(TokenKind::Invalid, start, self.index);
        self.diagnostic(
            DiagnosticCode::UnterminatedString,
            SourceSpan::new(start, (start + 1).min(self.bytes.len())),
        );
    }

    fn consume_escape(&mut self, quote: u8) -> bool {
        self.index += 1;
        let Some(&escaped) = self.bytes.get(self.index) else {
            return false;
        };
        if matches!(escaped, b'\\' | b'n' | b'r' | b't') || escaped == quote {
            self.index += 1;
            return true;
        }
        if escaped != b'u' || self.bytes.get(self.index + 1) != Some(&b'{') {
            self.index += 1;
            return false;
        }
        self.index += 2;
        let digits_start = self.index;
        while self
            .bytes
            .get(self.index)
            .is_some_and(u8::is_ascii_hexdigit)
            && self.index - digits_start < 6
        {
            self.index += 1;
        }
        let Some(hex) = self.source.get(digits_start..self.index) else {
            return false;
        };
        let closed = self.bytes.get(self.index) == Some(&b'}');
        if closed {
            self.index += 1;
        }
        let scalar = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
        !hex.is_empty() && closed && scalar.is_some()
    }

    fn lex_address(&mut self, start: usize) {
        self.index += 1;
        while self
            .bytes
            .get(self.index)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'.')
        {
            self.index += 1;
        }
        let valid = is_direct_address(&self.source[start..self.index]);
        self.emit(
            if valid {
                TokenKind::DirectAddress
            } else {
                TokenKind::Invalid
            },
            start,
            self.index,
        );
        if !valid {
            self.diagnostic(
                DiagnosticCode::InvalidDirectAddress,
                SourceSpan::new(start, self.index),
            );
        }
    }

    fn lex_less(&mut self, start: usize) {
        self.index += 1;
        let kind = match self.bytes.get(self.index) {
            Some(b'=') => {
                self.index += 1;
                TokenKind::LessEqual
            }
            Some(b'>') => {
                self.index += 1;
                TokenKind::NotEqual
            }
            _ => TokenKind::Less,
        };
        self.emit(kind, start, self.index);
    }

    fn simple(&mut self, start: usize, kind: TokenKind) {
        self.index += 1;
        self.emit(kind, start, self.index);
    }

    fn simple_or_pair(&mut self, start: usize, second: u8, pair: TokenKind, single: TokenKind) {
        self.index += 1;
        let kind = if self.bytes.get(self.index) == Some(&second) {
            self.index += 1;
            pair
        } else {
            single
        };
        self.emit(kind, start, self.index);
    }

    fn emit(&mut self, kind: TokenKind, start: usize, end: usize) {
        if self.tokens.len() >= self.limits.max_tokens() {
            self.limit_exceeded = true;
            self.diagnostic(
                DiagnosticCode::SourceLimitExceeded,
                SourceSpan::new(start, end),
            );
            return;
        }
        self.emitted_non_trivia = true;
        self.tokens.push(Token {
            kind,
            span: SourceSpan::new(start, end),
            text: self.source[start..end].to_owned(),
        });
    }

    fn diagnostic(&mut self, code: DiagnosticCode, span: SourceSpan) {
        self.diagnostics
            .push(make_diagnostic(self.source_path, self.source, code, span));
    }
}

fn keyword(value: &str) -> Option<Keyword> {
    let upper = value.to_ascii_uppercase();
    Some(match upper.as_str() {
        "AURORA_ST" => Keyword::AuroraSt,
        "VERSION" => Keyword::Version,
        "TYPE" => Keyword::Type,
        "END_TYPE" => Keyword::EndType,
        "STRUCT" => Keyword::Struct,
        "END_STRUCT" => Keyword::EndStruct,
        "ARRAY" => Keyword::Array,
        "OF" => Keyword::Of,
        "VAR_GLOBAL" => Keyword::VarGlobal,
        "VAR_INPUT" => Keyword::VarInput,
        "VAR_OUTPUT" => Keyword::VarOutput,
        "VAR" => Keyword::Var,
        "VAR_TEMP" => Keyword::VarTemp,
        "END_VAR" => Keyword::EndVar,
        "FUNCTION" => Keyword::Function,
        "END_FUNCTION" => Keyword::EndFunction,
        "FUNCTION_BLOCK" => Keyword::FunctionBlock,
        "END_FUNCTION_BLOCK" => Keyword::EndFunctionBlock,
        "PROGRAM" => Keyword::Program,
        "END_PROGRAM" => Keyword::EndProgram,
        "IF" => Keyword::If,
        "THEN" => Keyword::Then,
        "ELSIF" => Keyword::Elsif,
        "ELSE" => Keyword::Else,
        "END_IF" => Keyword::EndIf,
        "FOR" => Keyword::For,
        "TO" => Keyword::To,
        "BY" => Keyword::By,
        "DO" => Keyword::Do,
        "END_FOR" => Keyword::EndFor,
        "RETURN" => Keyword::Return,
        "OR_ELSE" => Keyword::OrElse,
        "XOR" => Keyword::Xor,
        "OR" => Keyword::Or,
        "AND_THEN" => Keyword::AndThen,
        "AND" => Keyword::And,
        "MOD" => Keyword::Mod,
        "NOT" => Keyword::Not,
        "AT" => Keyword::At,
        "TRUE" => Keyword::True,
        "FALSE" => Keyword::False,
        "BOOL" => Keyword::Bool,
        "SINT" => Keyword::Sint,
        "INT" => Keyword::Int,
        "DINT" => Keyword::Dint,
        "LINT" => Keyword::Lint,
        "USINT" => Keyword::Usint,
        "UINT" => Keyword::Uint,
        "UDINT" => Keyword::Udint,
        "ULINT" => Keyword::Ulint,
        "REAL" => Keyword::Real,
        "LREAL" => Keyword::Lreal,
        "STRING" => Keyword::String,
        "WSTRING" => Keyword::Wstring,
        "WHILE" | "END_WHILE" | "REPEAT" | "UNTIL" | "END_REPEAT" | "VAR_IN_OUT" | "RETAIN"
        | "PERSISTENT" | "METHOD" | "INTERFACE" | "EXTENDS" => Keyword::Unsupported,
        _ => return None,
    })
}

fn is_direct_address(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() < 4 || bytes[0] != b'%' {
        return false;
    }
    let area = bytes[1].to_ascii_uppercase();
    let width = bytes[2].to_ascii_uppercase();
    if !matches!(area, b'I' | b'Q' | b'M') || !matches!(width, b'X' | b'B' | b'W' | b'D' | b'L') {
        return false;
    }
    let remainder = &value[3..];
    if width == b'X' {
        let Some((offset, bit)) = remainder.split_once('.') else {
            return false;
        };
        canonical_decimal(offset) && bit.len() == 1 && matches!(bit.as_bytes()[0], b'0'..=b'7')
    } else {
        canonical_decimal(remainder)
    }
}

fn canonical_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}
