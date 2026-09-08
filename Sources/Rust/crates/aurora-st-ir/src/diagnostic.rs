use serde::Serialize;

/// Half-open UTF-8 byte range used by AST nodes and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SourceSpan {
    /// Inclusive start byte offset.
    pub start: u32,
    /// Exclusive end byte offset.
    pub end: u32,
}

impl SourceSpan {
    // `parse` rejects source limits above u32::MAX before lexing, and every caller derives these
    // offsets from that validated source slice.
    #[allow(clippy::cast_possible_truncation)]
    pub(crate) const fn new(start: usize, end: usize) -> Self {
        Self {
            start: start as u32,
            end: end as u32,
        }
    }
}

/// Human-facing one-based source position plus its stable byte offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SourcePosition {
    /// Zero-based UTF-8 byte offset used for deterministic ordering.
    pub byte_offset: u32,
    /// One-based Unicode-scalar line.
    pub line: u32,
    /// One-based Unicode-scalar column; a tab counts as one scalar here.
    pub column: u32,
}

/// Stable diagnostic codes emitted by the R1-01 frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum DiagnosticCode {
    /// `ST0001`: source is not BOM-free UTF-8.
    #[serde(rename = "ST0001")]
    InvalidEncoding,
    /// `ST0002`: bytes cannot form a legal token.
    #[serde(rename = "ST0002")]
    InvalidToken,
    /// `ST0003`: block comment has no closing delimiter.
    #[serde(rename = "ST0003")]
    UnterminatedComment,
    /// `ST0004`: string has no closing quote.
    #[serde(rename = "ST0004")]
    UnterminatedString,
    /// `ST0005`: string escape is not in the Preview 1.0 set.
    #[serde(rename = "ST0005")]
    InvalidEscape,
    /// `ST0006`: the exact leading language directive is absent or unsupported.
    #[serde(rename = "ST0006")]
    UnsupportedLanguageVersion,
    /// `ST0007`: a configured source/token/node/nesting limit is exceeded.
    #[serde(rename = "ST0007")]
    SourceLimitExceeded,
    /// `ST0101`: token is not valid at the grammar position.
    #[serde(rename = "ST0101")]
    UnexpectedToken,
    /// `ST0102`: a uniquely determined terminator is missing.
    #[serde(rename = "ST0102")]
    MissingTerminator,
    /// `ST0103`: a reserved traditional/future construct is outside Preview 1.0.
    #[serde(rename = "ST0103")]
    UnsupportedConstruct,
    /// `ST5001`: text beginning with `%` is not a canonical direct address.
    #[serde(rename = "ST5001")]
    InvalidDirectAddress,
    /// `ST5016`: a vendor-specific address appears where a logical address is required.
    #[serde(rename = "ST5016")]
    VendorAddressInSource,
}

impl DiagnosticCode {
    /// Returns the frozen textual code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEncoding => "ST0001",
            Self::InvalidToken => "ST0002",
            Self::UnterminatedComment => "ST0003",
            Self::UnterminatedString => "ST0004",
            Self::InvalidEscape => "ST0005",
            Self::UnsupportedLanguageVersion => "ST0006",
            Self::SourceLimitExceeded => "ST0007",
            Self::UnexpectedToken => "ST0101",
            Self::MissingTerminator => "ST0102",
            Self::UnsupportedConstruct => "ST0103",
            Self::InvalidDirectAddress => "ST5001",
            Self::VendorAddressInSource => "ST5016",
        }
    }
}

/// One deterministic compiler diagnostic for a single source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    /// Normalized project-relative path supplied by the caller.
    pub source_path: String,
    /// Stable diagnostic code.
    pub code: DiagnosticCode,
    /// Stable half-open byte range.
    pub span: SourceSpan,
    /// Human-facing start position.
    pub start: SourcePosition,
    /// Human-facing exclusive end position.
    pub end: SourcePosition,
}

pub(crate) fn make_diagnostic(
    source_path: &str,
    source: &str,
    code: DiagnosticCode,
    span: SourceSpan,
) -> Diagnostic {
    Diagnostic {
        source_path: source_path.to_owned(),
        code,
        span,
        start: locate(source, span.start),
        end: locate(source, span.end),
    }
}

fn locate(source: &str, byte_offset: u32) -> SourcePosition {
    let boundary = usize::try_from(byte_offset)
        .ok()
        .map_or(source.len(), |value| value.min(source.len()));
    let prefix = source.get(..boundary).unwrap_or(source);
    let mut line = 1_u32;
    let mut column = 1_u32;
    for value in prefix.chars() {
        if value == '\n' {
            line = line.saturating_add(1);
            column = 1;
        } else {
            column = column.saturating_add(1);
        }
    }
    SourcePosition {
        byte_offset,
        line,
        column,
    }
}
