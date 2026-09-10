//! Deterministic source-to-Canonical-IR mapping for R1-06.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::{
    CanonicalFaultSiteId, CanonicalNodeId, FaultOperationKind, FaultSite, SemanticSource,
    SemanticSymbol, SourceSpan, SymbolId,
};

/// Major version of the compiler-internal source-to-IR map.
pub const CANONICAL_SOURCE_MAP_MAJOR: u16 = 1;
/// Minor version of the compiler-internal source-to-IR map.
pub const CANONICAL_SOURCE_MAP_MINOR: u16 = 0;

/// Version carried by every serialized source-to-IR map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CanonicalSourceMapVersion {
    /// Reader-incompatible version.
    pub major: u16,
    /// Backward-compatible additive version.
    pub minor: u16,
}

impl CanonicalSourceMapVersion {
    /// Returns the exact version emitted by this crate.
    #[must_use]
    pub const fn preview_v1_0() -> Self {
        Self {
            major: CANONICAL_SOURCE_MAP_MAJOR,
            minor: CANONICAL_SOURCE_MAP_MINOR,
        }
    }
}

/// Mandatory finite capacities for source-map construction and publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalSourceMapLimits {
    sources: usize,
    symbols: usize,
    nodes: usize,
    fault_sites: usize,
    encoded_bytes: usize,
}

impl CanonicalSourceMapLimits {
    /// Validates every source-map capacity.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalSourceMapLimitError`] when any capacity is zero.
    pub const fn new(
        max_sources: usize,
        max_symbols: usize,
        max_nodes: usize,
        max_fault_sites: usize,
        max_encoded_bytes: usize,
    ) -> Result<Self, CanonicalSourceMapLimitError> {
        if max_sources == 0 {
            return Err(CanonicalSourceMapLimitError::ZeroSources);
        }
        if max_symbols == 0 {
            return Err(CanonicalSourceMapLimitError::ZeroSymbols);
        }
        if max_nodes == 0 {
            return Err(CanonicalSourceMapLimitError::ZeroNodes);
        }
        if max_fault_sites == 0 {
            return Err(CanonicalSourceMapLimitError::ZeroFaultSites);
        }
        if max_encoded_bytes == 0 {
            return Err(CanonicalSourceMapLimitError::ZeroEncodedBytes);
        }
        Ok(Self {
            sources: max_sources,
            symbols: max_symbols,
            nodes: max_nodes,
            fault_sites: max_fault_sites,
            encoded_bytes: max_encoded_bytes,
        })
    }

    /// Maximum source files represented by one map.
    #[must_use]
    pub const fn max_sources(self) -> usize {
        self.sources
    }

    /// Maximum declaration symbols represented by one map.
    #[must_use]
    pub const fn max_symbols(self) -> usize {
        self.symbols
    }

    /// Maximum executable Canonical IR nodes represented by one map.
    #[must_use]
    pub const fn max_nodes(self) -> usize {
        self.nodes
    }

    /// Maximum runtime Fault sites represented by one map.
    #[must_use]
    pub const fn max_fault_sites(self) -> usize {
        self.fault_sites
    }

    /// Maximum RFC 8785 JSON bytes published for one source map.
    #[must_use]
    pub const fn max_encoded_bytes(self) -> usize {
        self.encoded_bytes
    }
}

/// Invalid zero-valued source-map capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CanonicalSourceMapLimitError {
    /// Source-file capacity is zero.
    #[error("max_sources must be non-zero")]
    ZeroSources,
    /// Symbol capacity is zero.
    #[error("max_symbols must be non-zero")]
    ZeroSymbols,
    /// Node capacity is zero.
    #[error("max_nodes must be non-zero")]
    ZeroNodes,
    /// Fault-site capacity is zero.
    #[error("max_fault_sites must be non-zero")]
    ZeroFaultSites,
    /// Encoded-byte capacity is zero.
    #[error("max_encoded_bytes must be non-zero")]
    ZeroEncodedBytes,
}

/// Stable dense source-file identity. `u32::MAX` is never assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SourceFileId(pub u32);

/// One input source file retained even when it contains no executable POU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceFileEntry {
    /// Dense identity assigned by normalized path order.
    pub id: SourceFileId,
    /// Normalized project-relative path.
    pub path: String,
    /// Exact UTF-8 source length in bytes.
    pub byte_length: u32,
}

/// Source declaration associated with one Canonical IR symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SymbolSourceEntry {
    /// Canonical declaration identity.
    pub symbol: SymbolId,
    /// Owning source file.
    pub source: SourceFileId,
    /// Half-open UTF-8 declaration-name span.
    pub span: SourceSpan,
}

/// Source operation associated with one executable Canonical IR node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct NodeSourceEntry {
    /// Dense Canonical IR node identity.
    pub node: CanonicalNodeId,
    /// POU containing the node.
    pub pou: SymbolId,
    /// Owning source file.
    pub source: SourceFileId,
    /// Half-open UTF-8 operation span.
    pub span: SourceSpan,
}

/// Source and IR identities associated with one runtime Fault site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FaultSourceEntry {
    /// Dense runtime Fault identity.
    pub fault_site: CanonicalFaultSiteId,
    /// Unique Canonical IR node that can raise the Fault.
    pub node: CanonicalNodeId,
    /// Owning source file.
    pub source: SourceFileId,
    /// Half-open UTF-8 source-operation span.
    pub span: SourceSpan,
    /// Operation category used by the stable source-site identity.
    pub operation: FaultOperationKind,
}

/// Complete deterministic source-to-Canonical-IR map.
///
/// Native instruction ranges are deliberately absent until AOT has real code offsets to publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanonicalSourceMap {
    /// Exact writer version.
    pub schema_version: CanonicalSourceMapVersion,
    /// Input files in normalized path order.
    pub sources: Vec<SourceFileEntry>,
    /// Declarations in Symbol-ID order, exactly one per IR symbol.
    pub symbols: Vec<SymbolSourceEntry>,
    /// Executable operations in Node-ID order, exactly one per IR node.
    pub nodes: Vec<NodeSourceEntry>,
    /// Runtime Fault locations in Fault-site-ID order.
    pub fault_sites: Vec<FaultSourceEntry>,
}

/// Inconsistent accepted inputs detected while constructing a source map.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CanonicalSourceMapInputError {
    /// More than one source has the same normalized project-relative path.
    #[error("duplicate source path `{0}`")]
    DuplicateSource(String),
    /// A source is too large for the frozen `u32` byte-offset representation.
    #[error("source `{0}` exceeds the u32 source-map byte range")]
    SourceTooLarge(String),
    /// A symbol, node, or Fault refers to an absent source.
    #[error("source map entry refers to absent source `{0}`")]
    MissingSource(String),
    /// A source span is reversed or crosses the owning source length.
    #[error("invalid span {start}..{end} in source `{source_path}`")]
    InvalidSpan {
        /// Owning source path.
        source_path: String,
        /// Inclusive start byte.
        start: u32,
        /// Exclusive end byte.
        end: u32,
    },
    /// A supposedly dense identity is missing, repeated, or out of order.
    #[error("non-dense source-map {kind} identity at index {index}")]
    NonDenseIdentity {
        /// Identity category.
        kind: &'static str,
        /// Expected zero-based index.
        index: usize,
    },
    /// Two Fault sites occupy one source node or a site does not bind exactly once.
    #[error("runtime Fault site {0} is not mapped to exactly one IR node")]
    InvalidFaultBinding(u32),
}

/// Failure to serialize a complete source map as bounded RFC 8785 canonical JSON.
#[derive(Debug, Error)]
pub enum CanonicalSourceMapSerializationError {
    /// The map was not produced by this exact writer version.
    #[error("unsupported Canonical source-map version {major}.{minor}")]
    UnsupportedVersion {
        /// Unsupported major.
        major: u16,
        /// Unsupported minor.
        minor: u16,
    },
    /// Serialized output exceeds the explicit caller limit.
    #[error("Canonical source map requires {actual} bytes, exceeding limit {limit}")]
    EncodedSizeExceeded {
        /// Required bytes.
        actual: usize,
        /// Allowed bytes.
        limit: usize,
    },
    /// A future value cannot be represented as canonical JSON.
    #[error("failed to serialize Canonical source map as canonical JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

/// Serializes a complete source map using RFC 8785 JCS within an explicit byte limit.
///
/// # Errors
///
/// Returns [`CanonicalSourceMapSerializationError::UnsupportedVersion`] for a mismatched version,
/// [`CanonicalSourceMapSerializationError::EncodedSizeExceeded`] when output crosses the caller
/// limit, or [`CanonicalSourceMapSerializationError::InvalidJson`] for an unrepresentable value.
pub fn canonical_source_map_to_json(
    source_map: &CanonicalSourceMap,
    limits: CanonicalSourceMapLimits,
) -> Result<Vec<u8>, CanonicalSourceMapSerializationError> {
    if source_map.schema_version != CanonicalSourceMapVersion::preview_v1_0() {
        return Err(CanonicalSourceMapSerializationError::UnsupportedVersion {
            major: source_map.schema_version.major,
            minor: source_map.schema_version.minor,
        });
    }
    let bytes = serde_jcs::to_vec(source_map)?;
    if bytes.len() > limits.max_encoded_bytes() {
        return Err(CanonicalSourceMapSerializationError::EncodedSizeExceeded {
            actual: bytes.len(),
            limit: limits.max_encoded_bytes(),
        });
    }
    Ok(bytes)
}

type LocationKey = (String, SourceSpan);

#[derive(Debug)]
pub(crate) enum SourceMapBuildError {
    Capacity {
        source_path: String,
        span: SourceSpan,
    },
    Input(CanonicalSourceMapInputError),
}

impl From<CanonicalSourceMapInputError> for SourceMapBuildError {
    fn from(value: CanonicalSourceMapInputError) -> Self {
        Self::Input(value)
    }
}

pub(crate) struct SourceMapBuilder {
    limits: CanonicalSourceMapLimits,
    source_ids: BTreeMap<String, SourceFileId>,
    source_lengths: BTreeMap<SourceFileId, u32>,
    sources: Vec<SourceFileEntry>,
    symbols: Vec<SymbolSourceEntry>,
    nodes: Vec<NodeSourceEntry>,
    expected_faults: BTreeMap<LocationKey, (CanonicalFaultSiteId, FaultOperationKind)>,
    used_faults: BTreeSet<CanonicalFaultSiteId>,
    fault_sites: Vec<FaultSourceEntry>,
}

impl SourceMapBuilder {
    pub(crate) fn new(
        sources: &[SemanticSource<'_>],
        symbols: &[SemanticSymbol],
        fault_sites: &[FaultSite],
        limits: CanonicalSourceMapLimits,
    ) -> Result<Self, SourceMapBuildError> {
        if sources.len() > limits.max_sources() {
            let (source_path, span) = source_capacity_anchor(sources, limits.max_sources());
            return Err(SourceMapBuildError::Capacity { source_path, span });
        }
        preflight_count(
            symbols.len(),
            limits.max_symbols(),
            symbols
                .get(limits.max_symbols())
                .map_or(("", SourceSpan { start: 0, end: 0 }), |symbol| {
                    (symbol.source_path.as_str(), symbol.span)
                }),
        )?;
        preflight_count(
            fault_sites.len(),
            limits.max_fault_sites(),
            fault_sites
                .get(limits.max_fault_sites())
                .map_or(("", SourceSpan { start: 0, end: 0 }), |site| {
                    (site.id.source_path.as_str(), site.id.span)
                }),
        )?;

        let mut ordered_sources = BTreeMap::new();
        for source in sources {
            if ordered_sources
                .insert(source.ast.source_path.clone(), source.source)
                .is_some()
            {
                return Err(CanonicalSourceMapInputError::DuplicateSource(
                    source.ast.source_path.clone(),
                )
                .into());
            }
        }
        let mut source_ids = BTreeMap::new();
        let mut source_lengths = BTreeMap::new();
        let mut source_entries = Vec::with_capacity(ordered_sources.len());
        for (index, (path, source)) in ordered_sources.into_iter().enumerate() {
            let id = dense_id(index, "source").map(SourceFileId)?;
            let byte_length = u32::try_from(source.len())
                .map_err(|_| CanonicalSourceMapInputError::SourceTooLarge(path.clone()))?;
            source_ids.insert(path.clone(), id);
            source_lengths.insert(id, byte_length);
            source_entries.push(SourceFileEntry {
                id,
                path,
                byte_length,
            });
        }

        let mut symbol_entries = Vec::with_capacity(symbols.len());
        for (index, symbol) in symbols.iter().enumerate() {
            if symbol.id.0 != dense_id(index, "symbol")? {
                return Err(CanonicalSourceMapInputError::NonDenseIdentity {
                    kind: "symbol",
                    index,
                }
                .into());
            }
            let source = source_id(&source_ids, &symbol.source_path)?;
            validate_span(&source_lengths, source, &symbol.source_path, symbol.span)?;
            symbol_entries.push(SymbolSourceEntry {
                symbol: symbol.id,
                source,
                span: symbol.span,
            });
        }

        let mut expected_faults = BTreeMap::new();
        for (index, site) in fault_sites.iter().enumerate() {
            let id = CanonicalFaultSiteId(dense_id(index, "Fault site")?);
            let source = source_id(&source_ids, &site.id.source_path)?;
            validate_span(&source_lengths, source, &site.id.source_path, site.id.span)?;
            let key = (site.id.source_path.clone(), site.id.span);
            if expected_faults
                .insert(key, (id, site.id.operation))
                .is_some()
            {
                return Err(CanonicalSourceMapInputError::InvalidFaultBinding(id.0).into());
            }
        }

        Ok(Self {
            limits,
            source_ids,
            source_lengths,
            sources: source_entries,
            symbols: symbol_entries,
            nodes: Vec::new(),
            expected_faults,
            used_faults: BTreeSet::new(),
            fault_sites: Vec::new(),
        })
    }

    pub(crate) fn add_node(
        &mut self,
        node: CanonicalNodeId,
        pou: SymbolId,
        source_path: &str,
        span: SourceSpan,
        fault_site: Option<CanonicalFaultSiteId>,
    ) -> Result<(), SourceMapBuildError> {
        if self.nodes.len() >= self.limits.max_nodes() {
            return Err(SourceMapBuildError::Capacity {
                source_path: source_path.to_owned(),
                span,
            });
        }
        let expected = dense_id(self.nodes.len(), "node")?;
        if node.0 != expected {
            return Err(CanonicalSourceMapInputError::NonDenseIdentity {
                kind: "node",
                index: self.nodes.len(),
            }
            .into());
        }
        let source = source_id(&self.source_ids, source_path)?;
        validate_span(&self.source_lengths, source, source_path, span)?;
        self.nodes.push(NodeSourceEntry {
            node,
            pou,
            source,
            span,
        });

        match (
            self.expected_faults.get(&(source_path.to_owned(), span)),
            fault_site,
        ) {
            (Some((expected_site, operation)), Some(actual_site))
                if *expected_site == actual_site && self.used_faults.insert(actual_site) =>
            {
                self.fault_sites.push(FaultSourceEntry {
                    fault_site: actual_site,
                    node,
                    source,
                    span,
                    operation: *operation,
                });
            }
            (None, None) => {}
            (_, Some(actual_site)) => {
                return Err(
                    CanonicalSourceMapInputError::InvalidFaultBinding(actual_site.0).into(),
                );
            }
            (Some((expected_site, _)), None) => {
                return Err(
                    CanonicalSourceMapInputError::InvalidFaultBinding(expected_site.0).into(),
                );
            }
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<CanonicalSourceMap, CanonicalSourceMapInputError> {
        if self.used_faults.len() != self.expected_faults.len() {
            let missing = self
                .expected_faults
                .values()
                .map(|(id, _)| *id)
                .find(|id| !self.used_faults.contains(id))
                .map_or(0, |id| id.0);
            return Err(CanonicalSourceMapInputError::InvalidFaultBinding(missing));
        }
        self.fault_sites.sort_by_key(|entry| entry.fault_site);
        for (index, entry) in self.fault_sites.iter().enumerate() {
            if entry.fault_site.0 != dense_id(index, "Fault site")? {
                return Err(CanonicalSourceMapInputError::NonDenseIdentity {
                    kind: "Fault site",
                    index,
                });
            }
        }
        Ok(CanonicalSourceMap {
            schema_version: CanonicalSourceMapVersion::preview_v1_0(),
            sources: self.sources,
            symbols: self.symbols,
            nodes: self.nodes,
            fault_sites: self.fault_sites,
        })
    }
}

fn preflight_count(
    actual: usize,
    limit: usize,
    anchor: (&str, SourceSpan),
) -> Result<(), SourceMapBuildError> {
    if actual > limit {
        Err(SourceMapBuildError::Capacity {
            source_path: anchor.0.to_owned(),
            span: anchor.1,
        })
    } else {
        Ok(())
    }
}

fn source_capacity_anchor(sources: &[SemanticSource<'_>], limit: usize) -> (String, SourceSpan) {
    let mut first_paths = BTreeMap::new();
    for source in sources {
        first_paths.insert(source.ast.source_path.as_str(), source.ast.root.span);
        if first_paths.len() > limit.saturating_add(1) {
            let _removed = first_paths.pop_last();
        }
    }
    first_paths.iter().nth(limit).map_or_else(
        || (String::new(), SourceSpan { start: 0, end: 0 }),
        |(path, span)| ((*path).to_owned(), *span),
    )
}

fn dense_id(index: usize, kind: &'static str) -> Result<u32, CanonicalSourceMapInputError> {
    u32::try_from(index)
        .ok()
        .filter(|value| *value != u32::MAX)
        .ok_or(CanonicalSourceMapInputError::NonDenseIdentity { kind, index })
}

fn source_id(
    sources: &BTreeMap<String, SourceFileId>,
    path: &str,
) -> Result<SourceFileId, CanonicalSourceMapInputError> {
    sources
        .get(path)
        .copied()
        .ok_or_else(|| CanonicalSourceMapInputError::MissingSource(path.to_owned()))
}

fn validate_span(
    lengths: &BTreeMap<SourceFileId, u32>,
    source: SourceFileId,
    source_path: &str,
    span: SourceSpan,
) -> Result<(), CanonicalSourceMapInputError> {
    let valid = lengths
        .get(&source)
        .is_some_and(|length| span.start <= span.end && span.end <= *length);
    if valid {
        Ok(())
    } else {
        Err(CanonicalSourceMapInputError::InvalidSpan {
            source_path: source_path.to_owned(),
            start: span.start,
            end: span.end,
        })
    }
}
