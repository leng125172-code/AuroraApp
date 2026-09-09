//! Compile-time logical address, Device Mapping, writer, and local-handle binding for R1-05.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Serialize, Serializer};
use thiserror::Error;

use crate::ast::{AstNode, AstNodeKind};
use crate::diagnostic::{make_diagnostic, sort_diagnostics};
use crate::fixed::{FixedDataLimits, FixedTypeKind};
use crate::{
    AnalysisInputError, Diagnostic, DiagnosticCode, FaultSemanticModel, SemanticSource,
    SemanticSymbol, SemanticSymbolKind, SemanticType, SourceSpan, SymbolId, analyze_faults,
};

/// Canonical RFC 9562 `UUIDv7` stored in network-byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableId([u8; 16]);

impl StableId {
    /// Parses a lowercase canonical `UUIDv7`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        if value.len() != 36
            || value.as_bytes().get(8) != Some(&b'-')
            || value.as_bytes().get(13) != Some(&b'-')
            || value.as_bytes().get(18) != Some(&b'-')
            || value.as_bytes().get(23) != Some(&b'-')
            || value.as_bytes().get(14) != Some(&b'7')
            || !matches!(value.as_bytes().get(19), Some(b'8' | b'9' | b'a' | b'b'))
        {
            return None;
        }
        let mut bytes = [0_u8; 16];
        let mut output = 0_usize;
        let mut high = None;
        for byte in value.bytes() {
            if byte == b'-' {
                continue;
            }
            let digit = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => return None,
            };
            if let Some(value) = high.take() {
                let slot = bytes.get_mut(output)?;
                *slot = (value << 4) | digit;
                output += 1;
            } else {
                high = Some(digit);
            }
        }
        (output == bytes.len() && high.is_none()).then_some(Self(bytes))
    }

    /// Returns the RFC 9562 network bytes used for deterministic ordering.
    #[must_use]
    pub const fn network_bytes(self) -> [u8; 16] {
        self.0
    }
}

impl fmt::Display for StableId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if matches!(index, 4 | 6 | 8 | 10) {
                formatter.write_str("-")?;
            }
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for StableId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

/// One external field and its exact diagnostic anchor.
#[derive(Debug, Clone, Copy)]
pub struct ExternalField<'a> {
    /// Field text after JSON decoding.
    pub value: &'a str,
    /// Field value span in the owning source document.
    pub span: SourceSpan,
}

/// One Tag catalog entry in canonical `(path, byte offset)` input order.
#[derive(Debug, Clone, Copy)]
pub struct TagCatalogEntry<'a> {
    /// Normalized project-relative catalog path.
    pub source_path: &'a str,
    /// Exact UTF-8 catalog source.
    pub source: &'a str,
    /// Complete entry span.
    pub span: SourceSpan,
    /// Canonical fully-qualified global symbol.
    pub symbol: ExternalField<'a>,
    /// Stable Tag `UUIDv7`.
    pub tag_id: ExternalField<'a>,
}

/// Mapping direction at the Guardian boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingDirection {
    /// Device data enters `%I`.
    Input,
    /// `%Q` commands leave Aurora.
    Output,
}

/// Explicit byte order selected by a Device Mapping binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ByteOrder {
    /// Least-significant byte first.
    Little,
    /// Most-significant byte first.
    Big,
}

/// Explicit bit order selected by a Device Mapping binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BitOrder {
    /// Bit zero is least significant.
    Lsb0,
    /// Bit zero is most significant.
    Msb0,
}

/// One explicit, package-supported byte/bit transform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappingTransform {
    /// Byte ordering.
    pub byte_order: ByteOrder,
    /// Bit ordering.
    pub bit_order: BitOrder,
}

/// One Device Mapping entry after JSON Schema validation.
#[derive(Debug, Clone, Copy)]
pub struct DeviceBindingEntry<'a> {
    /// Normalized project-relative mapping path.
    pub source_path: &'a str,
    /// Exact UTF-8 mapping source.
    pub source: &'a str,
    /// Complete binding span.
    pub span: SourceSpan,
    /// Binding `UUIDv7`.
    pub binding_id: ExternalField<'a>,
    /// Referenced Tag `UUIDv7`.
    pub tag_id: ExternalField<'a>,
    /// Referenced device `UUIDv7`.
    pub device_id: ExternalField<'a>,
    /// Explicit logical direction.
    pub direction: MappingDirection,
    /// Opaque package-defined endpoint.
    pub vendor_endpoint: ExternalField<'a>,
    /// Physical width in bits.
    pub width_bits: u8,
    /// Explicit byte order; `None` represents a missing/unknown decoded value.
    pub byte_order: Option<ByteOrder>,
    /// Explicit bit order; `None` represents a missing/unknown decoded value.
    pub bit_order: Option<BitOrder>,
}

/// One endpoint resolved only from locked Device Package metadata.
#[derive(Debug, Clone)]
pub struct DeviceEndpoint<'a> {
    /// Opaque endpoint identity matched exactly.
    pub vendor_endpoint: &'a str,
    /// Half-open physical bit interval start.
    pub start_bit: u64,
    /// Half-open physical bit interval end.
    pub end_bit: u64,
    /// Supported access direction.
    pub direction: MappingDirection,
    /// Supported physical width.
    pub width_bits: u8,
    /// Finite set of supported transforms.
    pub transforms: &'a [MappingTransform],
}

/// Locked build-time Device Package metadata. This pass performs no device or network I/O.
#[derive(Debug, Clone)]
pub struct LockedDevicePackage<'a> {
    /// Device identity supplied by the resolved project.
    pub device_id: StableId,
    /// Package identity used to cardinalize `ST5025`.
    pub package_id: StableId,
    /// Whether the exact locked version is available to the build.
    pub available: bool,
    /// Diagnostic source path for an unavailable locked package.
    pub source_path: &'a str,
    /// Exact UTF-8 package-lock source.
    pub source: &'a str,
    /// Package identity span.
    pub span: SourceSpan,
    /// Bounded endpoint metadata decoded by the Device Package host tooling.
    pub endpoints: &'a [DeviceEndpoint<'a>],
}

/// Stable R0 task association consumed by writer ownership analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProgramTaskBinding {
    /// Program declaration symbol.
    pub program: SymbolId,
    /// Stable task handle from the build plan.
    pub task_handle: TaskHandle,
}

/// Stable task identity supplied by the resolved R0 build plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct TaskHandle(pub u32);

/// All non-ST inputs to the R1-05 binding pass.
#[derive(Debug, Clone, Copy)]
pub struct AddressBindingInputs<'a> {
    /// Stable Tag catalog entries.
    pub tag_catalog: &'a [TagCatalogEntry<'a>],
    /// Logical-to-device bindings.
    pub device_bindings: &'a [DeviceBindingEntry<'a>],
    /// Locked Device Package metadata.
    pub device_packages: &'a [LockedDevicePackage<'a>],
    /// Program instances assigned to cyclic tasks.
    pub program_tasks: &'a [ProgramTaskBinding],
}

/// Mandatory image capacities and host-side collection bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressBindingLimits {
    input_image_bytes: u64,
    output_image_bytes: u64,
    memory_image_bytes: u64,
    catalog_entries: usize,
    device_bindings: usize,
    device_packages: usize,
    endpoints_per_package: usize,
    program_tasks: usize,
}

impl AddressBindingLimits {
    /// Creates explicit R1-05 bounds. Image capacities may be zero; collection bounds may not.
    ///
    /// # Errors
    ///
    /// Returns [`AddressLimitError`] when a host-side collection bound is zero.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        input_image_bytes: u64,
        output_image_bytes: u64,
        memory_image_bytes: u64,
        max_catalog_entries: usize,
        max_device_bindings: usize,
        max_device_packages: usize,
        max_endpoints_per_package: usize,
        max_program_tasks: usize,
    ) -> Result<Self, AddressLimitError> {
        if max_catalog_entries == 0 {
            return Err(AddressLimitError::ZeroCatalogEntries);
        }
        if max_device_bindings == 0 {
            return Err(AddressLimitError::ZeroDeviceBindings);
        }
        if max_device_packages == 0 {
            return Err(AddressLimitError::ZeroDevicePackages);
        }
        if max_endpoints_per_package == 0 {
            return Err(AddressLimitError::ZeroEndpointsPerPackage);
        }
        if max_program_tasks == 0 {
            return Err(AddressLimitError::ZeroProgramTasks);
        }
        Ok(Self {
            input_image_bytes,
            output_image_bytes,
            memory_image_bytes,
            catalog_entries: max_catalog_entries,
            device_bindings: max_device_bindings,
            device_packages: max_device_packages,
            endpoints_per_package: max_endpoints_per_package,
            program_tasks: max_program_tasks,
        })
    }
}

/// Invalid mandatory host-side R1-05 resource bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AddressLimitError {
    /// Tag catalog entry bound is zero.
    #[error("max_catalog_entries must be non-zero")]
    ZeroCatalogEntries,
    /// Device binding entry bound is zero.
    #[error("max_device_bindings must be non-zero")]
    ZeroDeviceBindings,
    /// Device Package bound is zero.
    #[error("max_device_packages must be non-zero")]
    ZeroDevicePackages,
    /// Per-package endpoint bound is zero.
    #[error("max_endpoints_per_package must be non-zero")]
    ZeroEndpointsPerPackage,
    /// Program-to-task association bound is zero.
    #[error("max_program_tasks must be non-zero")]
    ZeroProgramTasks,
}

/// Logical image area. Each area has an independent capacity and overlap domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogicalArea {
    /// Guardian-owned input image.
    I,
    /// Normal-control output image.
    Q,
    /// Runtime-internal memory image.
    M,
}

/// One exact half-open logical bit interval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LogicalAddress {
    /// Independent image area.
    pub area: LogicalArea,
    /// Inclusive bit offset.
    pub start_bit: u64,
    /// Width in bits.
    pub width_bits: u8,
}

/// Payload-local handle. `u32::MAX` is never assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct LocalHandle(pub u32);

/// One successfully bound observable Tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BoundTag {
    /// Stable engineering identity.
    pub tag_id: StableId,
    /// Canonical global symbol.
    pub symbol: String,
    /// Compiler declaration identity.
    pub global: SymbolId,
    /// Exact fixed scalar type used by typed image loads/stores in later IR lowering.
    pub value_type: crate::FixedTypeId,
    /// Logical image interval.
    pub address: LogicalAddress,
    /// Payload-local dense handle.
    pub handle: LocalHandle,
    /// Sorted normal writer tasks; empty is valid only for `%I` and read-only `%M`.
    pub writer_tasks: Vec<TaskHandle>,
}

/// One resolved physical binding retained for later Guardian payload generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedDeviceBinding {
    /// Mapping identity used for deterministic diagnostics and ordering.
    pub binding_id: StableId,
    /// Bound Tag identity.
    pub tag_id: StableId,
    /// Device identity.
    pub device_id: StableId,
    /// Direction at the Guardian boundary.
    pub direction: MappingDirection,
    /// Opaque endpoint, never interpreted by ST or the cyclic engine.
    pub vendor_endpoint: String,
    /// Package-resolved physical interval start.
    pub physical_start_bit: u64,
    /// Package-resolved physical interval width.
    pub width_bits: u8,
    /// Explicit transform selected by the mapping.
    pub byte_order: ByteOrder,
    /// Explicit transform selected by the mapping.
    pub bit_order: BitOrder,
}

/// One R0 snapshot edge required for a cross-task `%Q/%M` read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SnapshotDependency {
    /// Unique writer/owner task.
    pub source_task: TaskHandle,
    /// Reading task.
    pub target_task: TaskHandle,
    /// Dense handle shared with the Tag table.
    pub tag_handle: LocalHandle,
}

/// Complete R1-05 model layered over successful R1-04 analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddressSemanticModel {
    /// R1-04 fixed data and deterministic Fault sites.
    pub faults: FaultSemanticModel,
    /// Exactly one entry per valid catalog-backed global, sorted by `TagId`.
    pub tags: Vec<BoundTag>,
    /// Exactly one binding per valid `%I/%Q` Tag.
    pub device_bindings: Vec<ResolvedDeviceBinding>,
    /// Sorted, duplicate-free cross-task snapshot requirements.
    pub snapshot_dependencies: Vec<SnapshotDependency>,
}

/// Atomic R1-05 result; any diagnostic suppresses the complete model and handle table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressAnalysisOutput {
    /// Complete model, present only after every ordered R1-05 validation pass succeeds.
    pub model: Option<AddressSemanticModel>,
    /// Stable diagnostics sorted by path/byte/code.
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone)]
struct CatalogTag {
    tag_id: StableId,
    global: SymbolId,
    symbol: String,
    source_path: String,
    declaration_span: SourceSpan,
    address_span: SourceSpan,
    address_text: String,
    value_type: crate::FixedTypeId,
}

#[derive(Debug, Clone)]
struct LogicalTag {
    catalog: CatalogTag,
    address: LogicalAddress,
}

#[derive(Debug, Clone, Default)]
struct PouAccess {
    reads: BTreeSet<SymbolId>,
    writes: Vec<(SymbolId, String, SourceSpan)>,
    calls: BTreeSet<SymbolId>,
}

#[derive(Debug, Clone)]
struct WriterState {
    tag: LogicalTag,
    writers: BTreeSet<TaskHandle>,
    readers: BTreeSet<TaskHandle>,
}

#[derive(Debug, Clone)]
struct ValidBinding<'a> {
    binding_id: StableId,
    tag_id: StableId,
    device_id: StableId,
    direction: MappingDirection,
    endpoint: &'a str,
    width_bits: u8,
    byte_order: ByteOrder,
    bit_order: BitOrder,
    physical_start: u64,
    physical_end: u64,
    source_path: &'a str,
    source: &'a str,
    span: SourceSpan,
}

#[derive(Debug, Clone)]
struct CandidateBinding<'a> {
    state: WriterState,
    binding_id: StableId,
    device_id: StableId,
    entry: DeviceBindingEntry<'a>,
}

type Ownership = BTreeMap<StableId, (BTreeSet<TaskHandle>, BTreeSet<TaskHandle>)>;

/// Runs R1-04 followed by catalog, logical interval, writer, mapping, and handle validation.
///
/// The pass is host-only. It neither opens devices nor emits Canonical IR/AOT code, and all loops
/// are bounded by parsed source limits or the explicit collection limits supplied here.
///
/// # Errors
///
/// Returns [`AnalysisInputError`] only for corrupt parser/source pairs. Project input failures are
/// deterministic diagnostics and never publish a partial model.
pub fn analyze_addresses(
    sources: &[SemanticSource<'_>],
    fixed_limits: FixedDataLimits,
    limits: AddressBindingLimits,
    inputs: AddressBindingInputs<'_>,
) -> Result<AddressAnalysisOutput, AnalysisInputError> {
    let fault_output = analyze_faults(sources, fixed_limits)?;
    let Some(faults) = fault_output.model else {
        return Ok(AddressAnalysisOutput {
            model: None,
            diagnostics: fault_output.diagnostics,
        });
    };
    let mut analyzer = AddressAnalyzer {
        sources,
        faults,
        limits,
        inputs,
        diagnostics: Vec::new(),
    };
    analyzer.run()
}

struct AddressAnalyzer<'a, 'input> {
    sources: &'a [SemanticSource<'a>],
    faults: FaultSemanticModel,
    limits: AddressBindingLimits,
    inputs: AddressBindingInputs<'input>,
    diagnostics: Vec<Diagnostic>,
}

impl<'input> AddressAnalyzer<'_, 'input> {
    fn run(&mut self) -> Result<AddressAnalysisOutput, AnalysisInputError> {
        if self.exceeds_resource_limits() {
            self.emit_project(DiagnosticCode::ResourceBudgetExceeded);
            return Ok(self.finish(None));
        }
        let catalog = self.bind_catalog()?;
        let known_tag_ids: BTreeSet<StableId> = catalog.iter().map(|tag| tag.tag_id).collect();
        let logical = self.bind_logical(catalog);
        let writer_states = self.analyze_writers(logical)?;
        let bindings = self.bind_devices(&writer_states, &known_tag_ids);

        if !self.diagnostics.is_empty() {
            return Ok(self.finish(None));
        }
        let Some((tags, handles)) = assign_handles(writer_states) else {
            self.emit_project(DiagnosticCode::LocalHandleExhausted);
            return Ok(self.finish(None));
        };
        let mut resolved_bindings: Vec<ResolvedDeviceBinding> = bindings
            .into_iter()
            .map(|binding| ResolvedDeviceBinding {
                binding_id: binding.binding_id,
                tag_id: binding.tag_id,
                device_id: binding.device_id,
                direction: binding.direction,
                vendor_endpoint: binding.endpoint.to_owned(),
                physical_start_bit: binding.physical_start,
                width_bits: binding.width_bits,
                byte_order: binding.byte_order,
                bit_order: binding.bit_order,
            })
            .collect();
        resolved_bindings.sort_by_key(|binding| binding.binding_id);
        let dependencies = snapshot_dependencies(&tags, &handles);
        let model = AddressSemanticModel {
            faults: self.faults.clone(),
            tags,
            device_bindings: resolved_bindings,
            snapshot_dependencies: dependencies,
        };
        Ok(self.finish(Some(model)))
    }

    fn exceeds_resource_limits(&self) -> bool {
        self.inputs.tag_catalog.len() > self.limits.catalog_entries
            || self.inputs.device_bindings.len() > self.limits.device_bindings
            || self.inputs.device_packages.len() > self.limits.device_packages
            || self.inputs.program_tasks.len() > self.limits.program_tasks
            || self
                .inputs
                .device_packages
                .iter()
                .any(|package| package.endpoints.len() > self.limits.endpoints_per_package)
    }

    fn bind_catalog(&mut self) -> Result<Vec<CatalogTag>, AnalysisInputError> {
        let globals = self.global_declarations()?;
        let by_name: BTreeMap<&str, &CatalogTag> = globals
            .iter()
            .map(|tag| (tag.symbol.as_str(), tag))
            .collect();
        let mut entries: Vec<&TagCatalogEntry<'_>> = self.inputs.tag_catalog.iter().collect();
        entries.sort_by(|left, right| {
            external_order(left.source_path, left.span, right.source_path, right.span)
        });
        let mut first_symbols = BTreeSet::new();
        let mut seen_tag_ids = BTreeSet::new();
        let mut matched_symbols = BTreeSet::new();
        let mut result = Vec::new();

        for entry in entries {
            if !first_symbols.insert(entry.symbol.value) {
                self.emit_external(
                    entry.source_path,
                    entry.source,
                    DiagnosticCode::DuplicateTagCatalogEntry,
                    entry.symbol.span,
                );
                continue;
            }
            let Some(global) = by_name.get(entry.symbol.value).copied() else {
                if StableId::parse(entry.tag_id.value).is_none() {
                    self.emit_external(
                        entry.source_path,
                        entry.source,
                        DiagnosticCode::InvalidStableIdentity,
                        entry.tag_id.span,
                    );
                }
                self.emit_external(
                    entry.source_path,
                    entry.source,
                    DiagnosticCode::OrphanTagIdentity,
                    entry.span,
                );
                continue;
            };
            matched_symbols.insert(global.symbol.clone());
            let Some(tag_id) = StableId::parse(entry.tag_id.value) else {
                self.emit_external(
                    entry.source_path,
                    entry.source,
                    DiagnosticCode::InvalidStableIdentity,
                    entry.tag_id.span,
                );
                continue;
            };
            if !seen_tag_ids.insert(tag_id) {
                self.emit_external(
                    entry.source_path,
                    entry.source,
                    DiagnosticCode::DuplicateTagIdentity,
                    entry.tag_id.span,
                );
                continue;
            }
            let mut bound = global.clone();
            bound.tag_id = tag_id;
            result.push(bound);
        }
        for global in globals {
            if !matched_symbols.contains(&global.symbol) {
                self.emit_st(
                    &global.source_path,
                    DiagnosticCode::MissingTagIdentity,
                    global.declaration_span,
                );
            }
        }
        Ok(result)
    }

    fn global_declarations(&self) -> Result<Vec<CatalogTag>, AnalysisInputError> {
        let fixed = &self.faults.fixed;
        let mut result = Vec::new();
        for source in self.sources {
            for block in &source.ast.root.children {
                if block.kind != AstNodeKind::GlobalVariableBlock {
                    continue;
                }
                for declaration in &block.children {
                    let name = child(declaration, 0, source)?;
                    let address = child(declaration, 1, source)?;
                    let symbol = fixed
                        .semantics
                        .symbols
                        .iter()
                        .find(|candidate| {
                            candidate.kind == SemanticSymbolKind::GlobalVariable
                                && candidate.source_path == source.ast.source_path
                                && candidate.span == name.span
                        })
                        .ok_or_else(|| invalid_shape(source, declaration.span))?;
                    let layout = fixed
                        .globals
                        .iter()
                        .find(|global| global.global == symbol.id)
                        .ok_or_else(|| invalid_shape(source, declaration.span))?;
                    let address_text = address
                        .text
                        .as_ref()
                        .ok_or_else(|| invalid_shape(source, address.span))?;
                    result.push(CatalogTag {
                        tag_id: StableId([0; 16]),
                        global: symbol.id,
                        symbol: symbol.canonical_name.clone(),
                        source_path: source.ast.source_path.clone(),
                        declaration_span: symbol.span,
                        address_span: address.span,
                        address_text: address_text.clone(),
                        value_type: layout.value_type,
                    });
                }
            }
        }
        Ok(result)
    }

    fn bind_logical(&mut self, catalog: Vec<CatalogTag>) -> Vec<LogicalTag> {
        let mut candidates = Vec::new();
        for tag in catalog {
            let (area, start, width) = match parse_logical_address(&tag.address_text) {
                LogicalAddressParse::Valid { area, start, width } => (area, start, width),
                LogicalAddressParse::OutOfRange => {
                    self.emit_st(
                        &tag.source_path,
                        DiagnosticCode::AddressOutOfRange,
                        tag.address_span,
                    );
                    continue;
                }
                LogicalAddressParse::Invalid => {
                    self.emit_st(
                        &tag.source_path,
                        DiagnosticCode::InvalidDirectAddress,
                        tag.address_span,
                    );
                    continue;
                }
            };
            let Some(expected_width) = self.scalar_width(tag.value_type) else {
                self.emit_st(
                    &tag.source_path,
                    DiagnosticCode::AddressTypeMismatch,
                    tag.address_span,
                );
                continue;
            };
            if expected_width != width {
                self.emit_st(
                    &tag.source_path,
                    DiagnosticCode::AddressTypeMismatch,
                    tag.address_span,
                );
                continue;
            }
            if width > 1 && start % u64::from(width) != 0 {
                self.emit_st(
                    &tag.source_path,
                    DiagnosticCode::AddressMisaligned,
                    tag.address_span,
                );
                continue;
            }
            let capacity_bytes = match area {
                LogicalArea::I => self.limits.input_image_bytes,
                LogicalArea::Q => self.limits.output_image_bytes,
                LogicalArea::M => self.limits.memory_image_bytes,
            };
            let end = start.checked_add(u64::from(width));
            let capacity = capacity_bytes.checked_mul(8);
            if end.is_none() || capacity.is_none() || end > capacity {
                self.emit_st(
                    &tag.source_path,
                    DiagnosticCode::AddressOutOfRange,
                    tag.address_span,
                );
                continue;
            }
            candidates.push(LogicalTag {
                catalog: tag,
                address: LogicalAddress {
                    area,
                    start_bit: start,
                    width_bits: width,
                },
            });
        }
        candidates.sort_by_key(|tag| {
            (
                tag.address.area,
                tag.address.start_bit,
                tag.address.start_bit + u64::from(tag.address.width_bits),
                tag.catalog.tag_id,
            )
        });
        let mut prior: Vec<LogicalTag> = Vec::new();
        let mut valid = Vec::new();
        for candidate in candidates {
            let overlaps = prior
                .iter()
                .any(|earlier| logical_overlap(earlier, &candidate));
            prior.push(candidate.clone());
            if overlaps {
                self.emit_st(
                    &candidate.catalog.source_path,
                    DiagnosticCode::AddressOverlap,
                    candidate.catalog.address_span,
                );
            } else {
                valid.push(candidate);
            }
        }
        valid
    }

    fn scalar_width(&self, type_id: crate::FixedTypeId) -> Option<u8> {
        let layout = self
            .faults
            .fixed
            .types
            .iter()
            .find(|layout| layout.id == type_id)?;
        let FixedTypeKind::Scalar { name } = &layout.kind else {
            return None;
        };
        match name.as_str() {
            "BOOL" => Some(1),
            "SINT" | "USINT" => Some(8),
            "INT" | "UINT" => Some(16),
            "DINT" | "UDINT" | "REAL" => Some(32),
            "LINT" | "ULINT" | "LREAL" => Some(64),
            _ => None,
        }
    }

    fn analyze_writers(
        &mut self,
        logical: Vec<LogicalTag>,
    ) -> Result<Vec<WriterState>, AnalysisInputError> {
        let summaries = self.pou_summaries()?;
        let by_global: BTreeMap<SymbolId, LogicalArea> = logical
            .iter()
            .map(|tag| (tag.catalog.global, tag.address.area))
            .collect();
        let mut input_write_spans = BTreeSet::new();
        for summary in summaries.values() {
            for (global, path, span) in &summary.writes {
                if by_global.get(global) == Some(&LogicalArea::I)
                    && input_write_spans.insert((path.clone(), *span))
                {
                    self.emit_st(path, DiagnosticCode::InputWriteForbidden, *span);
                }
            }
        }

        let mut states: BTreeMap<SymbolId, WriterState> = logical
            .into_iter()
            .map(|tag| {
                (
                    tag.catalog.global,
                    WriterState {
                        tag,
                        writers: BTreeSet::new(),
                        readers: BTreeSet::new(),
                    },
                )
            })
            .collect();
        for task in self.inputs.program_tasks {
            let closure = transitive_access(task.program, &summaries);
            for global in closure.reads {
                if let Some(state) = states.get_mut(&global) {
                    state.readers.insert(task.task_handle);
                }
            }
            for (global, _, _) in closure.writes {
                if let Some(state) = states.get_mut(&global) {
                    state.writers.insert(task.task_handle);
                }
            }
        }
        let mut valid = Vec::new();
        for (_, state) in states {
            let invalid = match state.tag.address.area {
                LogicalArea::Q if state.writers.is_empty() => {
                    self.emit_st(
                        &state.tag.catalog.source_path,
                        DiagnosticCode::OutputWriterMissing,
                        state.tag.catalog.declaration_span,
                    );
                    true
                }
                LogicalArea::Q | LogicalArea::M if state.writers.len() > 1 => {
                    self.emit_st(
                        &state.tag.catalog.source_path,
                        DiagnosticCode::MultipleWriters,
                        state.tag.catalog.declaration_span,
                    );
                    true
                }
                LogicalArea::M if state.writers.is_empty() && state.readers.len() > 1 => {
                    self.emit_st(
                        &state.tag.catalog.source_path,
                        DiagnosticCode::CrossTaskAccessUnresolved,
                        state.tag.catalog.declaration_span,
                    );
                    true
                }
                _ => false,
            };
            if !invalid {
                valid.push(state);
            }
        }
        Ok(valid)
    }

    fn pou_summaries(&self) -> Result<BTreeMap<SymbolId, PouAccess>, AnalysisInputError> {
        let mut summaries = BTreeMap::new();
        for source in self.sources {
            for node in &source.ast.root.children {
                if !matches!(
                    node.kind,
                    AstNodeKind::FunctionDeclaration
                        | AstNodeKind::FunctionBlockDeclaration
                        | AstNodeKind::ProgramDeclaration
                ) {
                    continue;
                }
                let name = child(node, 0, source)?;
                let symbol = self
                    .faults
                    .fixed
                    .semantics
                    .symbols
                    .iter()
                    .find(|symbol| {
                        symbol.source_path == source.ast.source_path && symbol.span == name.span
                    })
                    .ok_or_else(|| invalid_shape(source, node.span))?;
                let mut access = PouAccess::default();
                let body = node
                    .children
                    .last()
                    .filter(|child| child.kind == AstNodeKind::StatementList)
                    .ok_or_else(|| invalid_shape(source, node.span))?;
                self.walk_access(source, body, &mut access)?;
                summaries.insert(symbol.id, access);
            }
        }
        Ok(summaries)
    }

    fn walk_access(
        &self,
        source: &SemanticSource<'_>,
        node: &AstNode,
        access: &mut PouAccess,
    ) -> Result<(), AnalysisInputError> {
        match node.kind {
            AstNodeKind::AssignmentStatement => {
                let target = child(node, 0, source)?;
                self.record_target(source, target, access);
                self.walk_access(source, child(node, 1, source)?, access)?;
                return Ok(());
            }
            AstNodeKind::OutputArgument => {
                self.record_target(source, child(node, 1, source)?, access);
                return Ok(());
            }
            AstNodeKind::ForStatement => {
                self.record_target(source, child(node, 0, source)?, access);
                for bound_or_body in node.children.iter().skip(1) {
                    self.walk_access(source, bound_or_body, access)?;
                }
                return Ok(());
            }
            AstNodeKind::FunctionBlockCallStatement | AstNodeKind::CallExpression => {
                let callee = child(node, 0, source)?;
                if let Some(reference) = self.first_reference(&source.ast.source_path, callee.span)
                    && let Some(target) = self.call_target(reference.symbol)
                {
                    access.calls.insert(target);
                }
                for argument in node.children.iter().skip(1) {
                    self.walk_access(source, argument, access)?;
                }
                return Ok(());
            }
            AstNodeKind::QualifiedIdentifier => {
                for reference in self.references_in(&source.ast.source_path, node.span) {
                    if self.is_global(reference.symbol) {
                        access.reads.insert(reference.symbol);
                    }
                }
                return Ok(());
            }
            _ => {}
        }
        for child in &node.children {
            self.walk_access(source, child, access)?;
        }
        Ok(())
    }

    fn record_target(&self, source: &SemanticSource<'_>, target: &AstNode, access: &mut PouAccess) {
        let references = self.references_in(&source.ast.source_path, target.span);
        if let Some(root) = references.first().copied() {
            if self.is_global(root.symbol) {
                access
                    .writes
                    .push((root.symbol, source.ast.source_path.clone(), target.span));
            }
            for reference in references.iter().skip(1) {
                if self.is_global(reference.symbol) {
                    access.reads.insert(reference.symbol);
                }
            }
        }
    }

    fn references_in(&self, path: &str, span: SourceSpan) -> Vec<&crate::ResolvedReference> {
        self.faults
            .fixed
            .semantics
            .references
            .iter()
            .filter(|reference| {
                reference.source_path == path
                    && reference.span.start >= span.start
                    && reference.span.end <= span.end
            })
            .collect()
    }

    fn first_reference(&self, path: &str, span: SourceSpan) -> Option<&crate::ResolvedReference> {
        self.references_in(path, span).into_iter().next()
    }

    fn is_global(&self, symbol_id: SymbolId) -> bool {
        self.symbol(symbol_id)
            .is_some_and(|symbol| symbol.kind == SemanticSymbolKind::GlobalVariable)
    }

    fn call_target(&self, symbol_id: SymbolId) -> Option<SymbolId> {
        let symbol = self.symbol(symbol_id)?;
        if matches!(
            symbol.kind,
            SemanticSymbolKind::Function | SemanticSymbolKind::FunctionBlock
        ) {
            return Some(symbol.id);
        }
        match &symbol.declared_type {
            Some(SemanticType::FunctionBlock { declaration }) => Some(*declaration),
            _ => None,
        }
    }

    fn symbol(&self, symbol_id: SymbolId) -> Option<&SemanticSymbol> {
        self.faults
            .fixed
            .semantics
            .symbols
            .iter()
            .find(|symbol| symbol.id == symbol_id)
    }

    fn bind_devices(
        &mut self,
        states: &[WriterState],
        known_tag_ids: &BTreeSet<StableId>,
    ) -> Vec<ValidBinding<'input>> {
        let candidates = self.collect_binding_candidates(states, known_tag_ids);
        let valid = self.validate_binding_candidates(candidates);
        self.reject_physical_overlaps(valid)
    }

    fn collect_binding_candidates(
        &mut self,
        states: &[WriterState],
        known_tag_ids: &BTreeSet<StableId>,
    ) -> Vec<CandidateBinding<'input>> {
        let state_by_tag: BTreeMap<StableId, &WriterState> = states
            .iter()
            .map(|state| (state.tag.catalog.tag_id, state))
            .collect();
        let mut entries: Vec<&DeviceBindingEntry<'_>> =
            self.inputs.device_bindings.iter().collect();
        entries.sort_by(|left, right| {
            external_order(left.source_path, left.span, right.source_path, right.span)
        });
        let mut seen_binding_ids = BTreeSet::new();
        let mut grouped: BTreeMap<StableId, Vec<(StableId, StableId, &DeviceBindingEntry<'_>)>> =
            BTreeMap::new();
        for entry in entries {
            let binding_id = self.parse_external_id(entry, entry.binding_id);
            let tag_id = self.parse_external_id(entry, entry.tag_id);
            let device_id = self.parse_external_id(entry, entry.device_id);
            let (Some(binding_id), Some(tag_id), Some(device_id)) = (binding_id, tag_id, device_id)
            else {
                continue;
            };
            if !seen_binding_ids.insert(binding_id) {
                self.emit_external(
                    entry.source_path,
                    entry.source,
                    DiagnosticCode::DuplicateBindingIdentity,
                    entry.binding_id.span,
                );
                continue;
            }
            let Some(state) = state_by_tag.get(&tag_id) else {
                if !known_tag_ids.contains(&tag_id) {
                    self.emit_external(
                        entry.source_path,
                        entry.source,
                        DiagnosticCode::MappingUnexpected,
                        entry.span,
                    );
                }
                continue;
            };
            if state.tag.address.area == LogicalArea::M {
                self.emit_external(
                    entry.source_path,
                    entry.source,
                    DiagnosticCode::MappingUnexpected,
                    entry.span,
                );
                continue;
            }
            grouped
                .entry(tag_id)
                .or_default()
                .push((binding_id, device_id, entry));
        }

        let mut candidates = Vec::new();
        for state in states {
            if state.tag.address.area == LogicalArea::M {
                continue;
            }
            let Some(entries) = grouped.get(&state.tag.catalog.tag_id) else {
                self.emit_st(
                    &state.tag.catalog.source_path,
                    DiagnosticCode::MappingMissing,
                    state.tag.catalog.declaration_span,
                );
                continue;
            };
            for (_, _, duplicate) in entries.iter().skip(1) {
                self.emit_external(
                    duplicate.source_path,
                    duplicate.source,
                    DiagnosticCode::MappingDuplicate,
                    duplicate.span,
                );
            }
            if let Some((binding_id, device_id, entry)) = entries.first() {
                candidates.push(CandidateBinding {
                    state: state.clone(),
                    binding_id: *binding_id,
                    device_id: *device_id,
                    entry: **entry,
                });
            }
        }
        candidates
    }

    // The mapping-spec order stays linear here so one entry cannot accidentally be diagnosed or
    // resolved twice while sharing package-level cardinality state.
    #[allow(clippy::too_many_lines)]
    fn validate_binding_candidates(
        &mut self,
        candidates: Vec<CandidateBinding<'input>>,
    ) -> Vec<ValidBinding<'input>> {
        let packages: BTreeMap<StableId, &LockedDevicePackage<'_>> = self
            .inputs
            .device_packages
            .iter()
            .map(|package| (package.device_id, package))
            .collect();
        let mut unavailable_packages = BTreeSet::new();
        let mut valid = Vec::new();
        for candidate in candidates {
            let state = candidate.state;
            let entry = candidate.entry;
            let expected_direction = match state.tag.address.area {
                LogicalArea::I => MappingDirection::Input,
                LogicalArea::Q => MappingDirection::Output,
                LogicalArea::M => continue,
            };
            let mut entry_valid = true;
            if entry.direction != expected_direction {
                self.emit_external(
                    entry.source_path,
                    entry.source,
                    DiagnosticCode::MappingDirectionMismatch,
                    entry.span,
                );
                entry_valid = false;
            }
            if entry.width_bits != state.tag.address.width_bits {
                self.emit_external(
                    entry.source_path,
                    entry.source,
                    DiagnosticCode::MappingWidthMismatch,
                    entry.span,
                );
                entry_valid = false;
            }
            let transform = entry.byte_order.zip(entry.bit_order);
            if transform.is_none() {
                self.emit_external(
                    entry.source_path,
                    entry.source,
                    DiagnosticCode::InvalidMappingTransform,
                    entry.span,
                );
                entry_valid = false;
            }
            let Some(package) = packages
                .get(&candidate.device_id)
                .copied()
                .filter(|value| value.available)
            else {
                let package_identity = packages
                    .get(&candidate.device_id)
                    .map_or(candidate.device_id, |package| package.package_id);
                if unavailable_packages.insert(package_identity) {
                    if let Some(package) = packages.get(&candidate.device_id) {
                        self.emit_external(
                            package.source_path,
                            package.source,
                            DiagnosticCode::DevicePackageUnavailable,
                            package.span,
                        );
                    } else {
                        self.emit_external(
                            entry.source_path,
                            entry.source,
                            DiagnosticCode::DevicePackageUnavailable,
                            entry.device_id.span,
                        );
                    }
                }
                continue;
            };
            let endpoint = package
                .endpoints
                .iter()
                .find(|endpoint| endpoint.vendor_endpoint == entry.vendor_endpoint.value);
            let Some(endpoint) = endpoint.filter(|endpoint| {
                !entry.vendor_endpoint.value.is_empty()
                    && endpoint.start_bit < endpoint.end_bit
                    && endpoint.direction == entry.direction
                    && endpoint.width_bits == entry.width_bits
                    && endpoint.end_bit - endpoint.start_bit == u64::from(endpoint.width_bits)
                    && (endpoint.width_bits == 1
                        || endpoint.start_bit % u64::from(endpoint.width_bits) == 0)
            }) else {
                self.emit_external(
                    entry.source_path,
                    entry.source,
                    DiagnosticCode::InvalidVendorEndpoint,
                    entry.vendor_endpoint.span,
                );
                continue;
            };
            let Some((byte_order, bit_order)) = transform else {
                continue;
            };
            if !endpoint.transforms.contains(&MappingTransform {
                byte_order,
                bit_order,
            }) {
                self.emit_external(
                    entry.source_path,
                    entry.source,
                    DiagnosticCode::InvalidMappingTransform,
                    entry.span,
                );
                entry_valid = false;
            }
            if entry_valid {
                valid.push(ValidBinding {
                    binding_id: candidate.binding_id,
                    tag_id: state.tag.catalog.tag_id,
                    device_id: candidate.device_id,
                    direction: entry.direction,
                    endpoint: entry.vendor_endpoint.value,
                    width_bits: entry.width_bits,
                    byte_order,
                    bit_order,
                    physical_start: endpoint.start_bit,
                    physical_end: endpoint.end_bit,
                    source_path: entry.source_path,
                    source: entry.source,
                    span: entry.span,
                });
            }
        }
        valid
    }

    fn reject_physical_overlaps(
        &mut self,
        mut valid: Vec<ValidBinding<'input>>,
    ) -> Vec<ValidBinding<'input>> {
        valid.sort_by_key(|binding| {
            (
                binding.device_id,
                binding.physical_start,
                binding.physical_end,
                binding.binding_id,
            )
        });
        let mut prior: Vec<ValidBinding> = Vec::new();
        let mut non_overlapping = Vec::new();
        for binding in valid {
            let overlaps = prior.iter().any(|earlier| {
                earlier.device_id == binding.device_id
                    && earlier.physical_start < binding.physical_end
                    && binding.physical_start < earlier.physical_end
            });
            prior.push(binding.clone());
            if overlaps {
                self.emit_external(
                    binding.source_path,
                    binding.source,
                    DiagnosticCode::PhysicalAddressOverlap,
                    binding.span,
                );
            } else {
                non_overlapping.push(binding);
            }
        }
        non_overlapping
    }

    fn parse_external_id(
        &mut self,
        entry: &DeviceBindingEntry<'_>,
        field: ExternalField<'_>,
    ) -> Option<StableId> {
        let value = StableId::parse(field.value);
        if value.is_none() {
            self.emit_external(
                entry.source_path,
                entry.source,
                DiagnosticCode::InvalidStableIdentity,
                field.span,
            );
        }
        value
    }

    fn emit_st(&mut self, path: &str, code: DiagnosticCode, span: SourceSpan) {
        if let Some(source) = self
            .sources
            .iter()
            .find(|source| source.ast.source_path == path)
        {
            self.diagnostics
                .push(make_diagnostic(path, source.source, code, span));
        }
    }

    fn emit_external(&mut self, path: &str, source: &str, code: DiagnosticCode, span: SourceSpan) {
        self.diagnostics
            .push(make_diagnostic(path, source, code, span));
    }

    fn emit_project(&mut self, code: DiagnosticCode) {
        if let Some(source) = self.sources.first() {
            self.diagnostics.push(make_diagnostic(
                &source.ast.source_path,
                source.source,
                code,
                SourceSpan { start: 0, end: 0 },
            ));
        } else if let Some(entry) = self.inputs.tag_catalog.first() {
            self.emit_external(entry.source_path, entry.source, code, entry.span);
        } else if let Some(entry) = self.inputs.device_bindings.first() {
            self.emit_external(entry.source_path, entry.source, code, entry.span);
        } else if let Some(package) = self.inputs.device_packages.first() {
            self.emit_external(package.source_path, package.source, code, package.span);
        }
    }

    fn finish(&mut self, model: Option<AddressSemanticModel>) -> AddressAnalysisOutput {
        sort_diagnostics(&mut self.diagnostics);
        AddressAnalysisOutput {
            model,
            diagnostics: std::mem::take(&mut self.diagnostics),
        }
    }
}

fn child<'a>(
    node: &'a AstNode,
    index: usize,
    source: &SemanticSource<'_>,
) -> Result<&'a AstNode, AnalysisInputError> {
    node.children
        .get(index)
        .ok_or_else(|| invalid_shape(source, node.span))
}

fn invalid_shape(source: &SemanticSource<'_>, span: SourceSpan) -> AnalysisInputError {
    AnalysisInputError::InvalidAstShape {
        source_path: source.ast.source_path.clone(),
        span_start: span.start,
        span_end: span.end,
    }
}

fn external_order(
    left_path: &str,
    left_span: SourceSpan,
    right_path: &str,
    right_span: SourceSpan,
) -> std::cmp::Ordering {
    left_path
        .as_bytes()
        .cmp(right_path.as_bytes())
        .then(left_span.start.cmp(&right_span.start))
}

enum LogicalAddressParse {
    Valid {
        area: LogicalArea,
        start: u64,
        width: u8,
    },
    OutOfRange,
    Invalid,
}

fn parse_logical_address(text: &str) -> LogicalAddressParse {
    let bytes = text.as_bytes();
    if bytes.len() < 4 || bytes.first() != Some(&b'%') {
        return LogicalAddressParse::Invalid;
    }
    let area = match bytes.get(1).map(u8::to_ascii_uppercase) {
        Some(b'I') => LogicalArea::I,
        Some(b'Q') => LogicalArea::Q,
        Some(b'M') => LogicalArea::M,
        _ => return LogicalAddressParse::Invalid,
    };
    let Some(width_code) = bytes.get(2).map(u8::to_ascii_uppercase) else {
        return LogicalAddressParse::Invalid;
    };
    let width = match width_code {
        b'X' => 1,
        b'B' => 8,
        b'W' => 16,
        b'D' => 32,
        b'L' => 64,
        _ => return LogicalAddressParse::Invalid,
    };
    let Some(body) = text.get(3..) else {
        return LogicalAddressParse::Invalid;
    };
    if width_code == b'X' {
        let Some((byte_text, bit_text)) = body.split_once('.') else {
            return LogicalAddressParse::Invalid;
        };
        if !canonical_decimal(byte_text) || bit_text.len() != 1 {
            return LogicalAddressParse::Invalid;
        }
        let Ok(bit) = bit_text.parse::<u8>() else {
            return LogicalAddressParse::Invalid;
        };
        if bit > 7 {
            return LogicalAddressParse::Invalid;
        }
        let Ok(byte) = byte_text.parse::<u64>() else {
            return LogicalAddressParse::OutOfRange;
        };
        let Some(start) = byte
            .checked_mul(8)
            .and_then(|value| value.checked_add(u64::from(bit)))
        else {
            return LogicalAddressParse::OutOfRange;
        };
        LogicalAddressParse::Valid { area, start, width }
    } else {
        if !canonical_decimal(body) {
            return LogicalAddressParse::Invalid;
        }
        let Ok(byte) = body.parse::<u64>() else {
            return LogicalAddressParse::OutOfRange;
        };
        let Some(start) = byte.checked_mul(8) else {
            return LogicalAddressParse::OutOfRange;
        };
        LogicalAddressParse::Valid { area, start, width }
    }
}

fn canonical_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

fn logical_overlap(left: &LogicalTag, right: &LogicalTag) -> bool {
    left.address.area == right.address.area
        && left.address.start_bit < right.address.start_bit + u64::from(right.address.width_bits)
        && right.address.start_bit < left.address.start_bit + u64::from(left.address.width_bits)
}

fn transitive_access(root: SymbolId, summaries: &BTreeMap<SymbolId, PouAccess>) -> PouAccess {
    let mut pending = vec![root];
    let mut visited = BTreeSet::new();
    let mut result = PouAccess::default();
    while let Some(symbol) = pending.pop() {
        if !visited.insert(symbol) {
            continue;
        }
        let Some(summary) = summaries.get(&symbol) else {
            continue;
        };
        result.reads.extend(summary.reads.iter().copied());
        result.writes.extend(summary.writes.iter().cloned());
        pending.extend(summary.calls.iter().copied());
    }
    result
}

fn assign_handles(mut states: Vec<WriterState>) -> Option<(Vec<BoundTag>, Ownership)> {
    states.sort_by_key(|state| state.tag.catalog.tag_id);
    let count = u64::try_from(states.len()).ok()?;
    if !handle_count_is_valid(count) {
        return None;
    }
    let mut ownership = BTreeMap::new();
    let mut tags = Vec::with_capacity(states.len());
    for (index, state) in states.into_iter().enumerate() {
        let handle = LocalHandle(u32::try_from(index).ok()?);
        ownership.insert(
            state.tag.catalog.tag_id,
            (state.writers.clone(), state.readers.clone()),
        );
        tags.push(BoundTag {
            tag_id: state.tag.catalog.tag_id,
            symbol: state.tag.catalog.symbol,
            global: state.tag.catalog.global,
            value_type: state.tag.catalog.value_type,
            address: state.tag.address,
            handle,
            writer_tasks: state.writers.into_iter().collect(),
        });
    }
    Some((tags, ownership))
}

fn handle_count_is_valid(count: u64) -> bool {
    u32::try_from(count).is_ok()
}

fn snapshot_dependencies(tags: &[BoundTag], ownership: &Ownership) -> Vec<SnapshotDependency> {
    let mut result = BTreeSet::new();
    for tag in tags {
        if tag.address.area == LogicalArea::I {
            continue;
        }
        let Some((writers, readers)) = ownership.get(&tag.tag_id) else {
            continue;
        };
        let source = writers.iter().next().copied().or_else(|| {
            (tag.address.area == LogicalArea::M && readers.len() == 1)
                .then(|| readers.iter().next().copied())
                .flatten()
        });
        if let Some(source_task) = source {
            for target_task in readers
                .iter()
                .copied()
                .filter(|reader| *reader != source_task)
            {
                result.insert(SnapshotDependency {
                    source_task,
                    target_task,
                    tag_handle: tag.handle,
                });
            }
        }
    }
    result.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::{StableId, handle_count_is_valid};

    #[test]
    fn stable_id_requires_canonical_uuid_v7() {
        assert!(StableId::parse("01890f3e-4c7b-7cc2-98c4-dc0c0c07398f").is_some());
        assert!(StableId::parse("01890F3E-4C7B-7CC2-98C4-DC0C0C07398F").is_none());
        assert!(StableId::parse("01890f3e-4c7b-6cc2-98c4-dc0c0c07398f").is_none());
        assert!(StableId::parse("01890f3e-4c7b-7cc2-78c4-dc0c0c07398f").is_none());
    }

    #[test]
    fn handle_count_boundary_rejects_only_values_above_reserved_range() {
        assert!(handle_count_is_valid(u64::from(u32::MAX)));
        assert!(!handle_count_is_valid(u64::from(u32::MAX) + 1));
    }
}
