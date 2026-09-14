//! Host-only command surface for the Aurora ST Preview 1.0 compiler.
//!
//! The library keeps command orchestration testable while the binary remains a process-lifecycle
//! adapter. All compiler passes use one fixed, documented R1 gate profile and publish no partial
//! AST, semantic model, IR, source map, checkpoint plan, or AOT output after a rejection.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use aurora_st_ir::{
    AddressBindingInputs, AddressBindingLimits, AotArtifact, AotLimits, AotTarget,
    CanonicalArtifactLimits, CanonicalIrLimits, CanonicalSourceMap, CanonicalSourceMapLimits,
    CheckpointPlan, CheckpointPlanLimits, CyclicWorkLimits, DeviceBindingEntry, Diagnostic,
    ExternalField, FixedDataLimits, InitializationLimits, MappingDirection, ParserLimits,
    ProgramTaskBinding, SemanticSource, SemanticSymbolKind, SourceSpan, StableId, TagCatalogEntry,
    TaskHandle, VersionedAst, analyze_addresses, analyze_cyclic_work, analyze_faults,
    canonical_ir_to_json, canonical_source_map_to_json, checkpoint_plan_to_json,
    compile_linux_x64_aot, diagnostics_to_canonical_json, lower_canonical_ir, parse,
    to_canonical_json,
};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

const MAX_SOURCE_FILES: usize = 64;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_MAPPING_BYTES: usize = 4 * 1024 * 1024;
const MAX_TASKS: usize = 65_536;
const MAX_ARTIFACT_JSON_BYTES: usize = 64 * 1024 * 1024;
const OUTPUT_NAMES: [&str; 5] = [
    "canonical-ir.json",
    "checkpoint-plan.json",
    "native-source-map.json",
    "program.o",
    "source-map.json",
];
const MAPPING_SCHEMA: &str = include_str!(
    "../../../../Contracts/schema/aurora/st-address-mapping/v1/st-address-mapping.schema.json"
);

/// Arguments accepted by `aurora-cli`.
#[derive(Debug, Parser)]
#[command(name = "aurora-cli", version, about)]
pub struct Arguments {
    /// Compiler operation to perform.
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Parse one source and print its RFC 8785 canonical AST JSON.
    Parse(ParseArguments),
    /// Run every source, semantic, address, and bounded-work check without generating files.
    Check(ProjectArguments),
    /// Build exactly five deterministic R1 artifacts into an absent or empty directory.
    Build(BuildArguments),
    /// Inspect generated IR or query its source map without writing files.
    Inspect(InspectArguments),
}

#[derive(Debug, Args)]
struct ParseArguments {
    /// Project root used to derive the reproducible source path.
    #[arg(long, default_value = ".")]
    project_root: PathBuf,
    /// One Aurora ST Preview 1.0 source file below the project root.
    source: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct ProjectArguments {
    /// Project root used to normalize every input path.
    #[arg(long, default_value = ".")]
    project_root: PathBuf,
    /// Aurora ST source; repeat the option once per project file.
    #[arg(long = "source", required = true)]
    sources: Vec<PathBuf>,
    /// Existing `aurora.st-address-mapping` Preview 1.0 document.
    #[arg(long)]
    mapping: Option<PathBuf>,
    /// Program-to-task binding in `ProgramName=TaskHandle` form; repeat per task.
    #[arg(long = "task", required = true)]
    tasks: Vec<TaskArgument>,
}

#[derive(Debug, Args)]
struct BuildArguments {
    #[command(flatten)]
    project: ProjectArguments,
    /// Absent or empty output directory. A successful build creates exactly five files.
    #[arg(long)]
    output: PathBuf,
}

#[derive(Debug, Args)]
struct InspectArguments {
    /// Inspection operation.
    #[command(subcommand)]
    command: InspectCommand,
}

#[derive(Debug, Subcommand)]
enum InspectCommand {
    /// Print the complete RFC 8785 Canonical ST IR JSON.
    Ir(ProjectArguments),
    /// Return every symbol, node, and Fault whose half-open span contains one byte offset.
    SourceMap(SourceMapArguments),
}

#[derive(Debug, Args)]
struct SourceMapArguments {
    #[command(flatten)]
    project: ProjectArguments,
    /// Normalized project-relative source path stored in the map.
    #[arg(long)]
    source_path: String,
    /// Zero-based UTF-8 byte offset to query.
    #[arg(long)]
    byte_offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskArgument {
    program: String,
    handle: u32,
}

impl FromStr for TaskArgument {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (program, handle) = value
            .split_once('=')
            .ok_or_else(|| "task must use ProgramName=TaskHandle".to_owned())?;
        if program.is_empty()
            || !program.bytes().enumerate().all(|(index, byte)| {
                byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
            })
        {
            return Err("task ProgramName must be an ST identifier".to_owned());
        }
        let handle = handle
            .parse::<u32>()
            .map_err(|_| "task handle must be a decimal u32".to_owned())?;
        if handle == u32::MAX {
            return Err("task handle u32::MAX is reserved".to_owned());
        }
        Ok(Self {
            program: program.to_ascii_lowercase(),
            handle,
        })
    }
}

/// A command failure that never represents partial compiler success.
#[derive(Debug, Error)]
pub enum CliError {
    /// An input or output filesystem operation failed.
    #[error("{operation} `{path}`: {source}")]
    Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Exact affected path.
        path: PathBuf,
        /// Operating-system failure.
        source: std::io::Error,
    },
    /// A JSON document could not be decoded.
    #[error("parse JSON `{path}`: {source}")]
    Json {
        /// Exact affected path.
        path: PathBuf,
        /// JSON decoder failure.
        source: serde_json::Error,
    },
    /// A caller-owned project or command value is invalid.
    #[error("{0}")]
    InvalidInput(String),
    /// The compiler rejected source or mapping input with stable diagnostics.
    #[error("{0}")]
    Diagnostics(String),
    /// Mutually inconsistent compiler inputs or a backend failure were detected.
    #[error("{0}")]
    Compiler(String),
}

/// Execute one parsed command and return bytes for standard output.
///
/// Parse and inspect return canonical JSON. Check and build return a canonical summary. A source
/// rejection returns canonical diagnostics through [`CliError::Diagnostics`], suitable for a
/// deterministic stderr golden sample. The function performs host filesystem I/O and must never
/// be called from a cyclic execution path.
///
/// # Errors
///
/// Returns [`CliError`] for invalid paths or arguments, source diagnostics, failed compiler
/// invariants, output collisions, serialization failures, or filesystem failures.
pub fn execute(arguments: Arguments) -> Result<Vec<u8>, CliError> {
    match arguments.command {
        Command::Parse(arguments) => execute_parse(&arguments),
        Command::Check(arguments) => execute_check(&arguments),
        Command::Build(arguments) => execute_build(&arguments),
        Command::Inspect(arguments) => match arguments.command {
            InspectCommand::Ir(project) => execute_inspect_ir(&project),
            InspectCommand::SourceMap(arguments) => execute_source_map_query(&arguments),
        },
    }
}

fn execute_parse(arguments: &ParseArguments) -> Result<Vec<u8>, CliError> {
    let root = canonical_directory(&arguments.project_root)?;
    let (path, bytes) = read_source_bytes(&root, &arguments.source)?;
    let output = parse(&path, &bytes, parser_limits()?);
    reject_diagnostics(&output.diagnostics)?;
    let ast = output.ast.ok_or_else(|| {
        CliError::Compiler("parser returned neither AST nor diagnostic".to_owned())
    })?;
    to_canonical_json(&ast).map_err(compiler_error)
}

fn execute_check(arguments: &ProjectArguments) -> Result<Vec<u8>, CliError> {
    let checked = check_project(arguments)?;
    canonical_json(&CheckSummary {
        source_files: checked.source_count,
        tasks: checked.task_count,
        tags: checked.tag_count,
        device_bindings: checked.device_binding_count,
        bounded_loops: checked.loop_count,
        status: "checked",
    })
}

fn execute_build(arguments: &BuildArguments) -> Result<Vec<u8>, CliError> {
    let lowered = lower_project(&arguments.project)?;
    let artifact = compile_linux_x64_aot(
        &lowered.ir,
        &lowered.source_map,
        &lowered.checkpoints,
        AotTarget::linux_x64_v1(),
        aot_limits()?,
    )
    .map_err(compiler_error)?;
    let files = serialize_build_outputs(&lowered, &artifact)?;
    let output = if arguments.output.is_absolute() {
        arguments.output.clone()
    } else {
        resolve_relative_output(&arguments.project.project_root, &arguments.output)?
    };
    publish_outputs(&output, &files)?;
    canonical_json(&BuildSummary {
        object_sha256: &artifact.object_sha256,
        outputs: OUTPUT_NAMES,
        status: "built",
    })
}

fn execute_inspect_ir(arguments: &ProjectArguments) -> Result<Vec<u8>, CliError> {
    let lowered = lower_project(arguments)?;
    canonical_ir_to_json(&lowered.ir, ir_limits()?).map_err(compiler_error)
}

fn execute_source_map_query(arguments: &SourceMapArguments) -> Result<Vec<u8>, CliError> {
    let lowered = lower_project(&arguments.project)?;
    let source = lowered
        .source_map
        .sources
        .iter()
        .find(|entry| entry.path == arguments.source_path)
        .ok_or_else(|| {
            CliError::InvalidInput(format!(
                "source map has no normalized path `{}`",
                arguments.source_path
            ))
        })?;
    if arguments.byte_offset >= source.byte_length {
        return Err(CliError::InvalidInput(format!(
            "byte offset {} is outside `{}` length {}",
            arguments.byte_offset, source.path, source.byte_length
        )));
    }
    let contains =
        |span: SourceSpan| span.start <= arguments.byte_offset && arguments.byte_offset < span.end;
    canonical_json(&SourceMapQuery {
        source,
        byte_offset: arguments.byte_offset,
        symbols: lowered
            .source_map
            .symbols
            .iter()
            .filter(|entry| entry.source == source.id && contains(entry.span))
            .collect(),
        nodes: lowered
            .source_map
            .nodes
            .iter()
            .filter(|entry| entry.source == source.id && contains(entry.span))
            .collect(),
        fault_sites: lowered
            .source_map
            .fault_sites
            .iter()
            .filter(|entry| entry.source == source.id && contains(entry.span))
            .collect(),
    })
}

struct LoadedSource {
    path: String,
    text: String,
    ast: VersionedAst,
}

struct CheckedProject {
    sources: Vec<LoadedSource>,
    address_model: aurora_st_ir::AddressSemanticModel,
    work_model: aurora_st_ir::CyclicWorkModel,
    source_count: usize,
    task_count: usize,
    tag_count: usize,
    device_binding_count: usize,
    loop_count: usize,
}

struct LoweredProject {
    ir: aurora_st_ir::CanonicalStIr,
    source_map: CanonicalSourceMap,
    checkpoints: CheckpointPlan,
}

fn check_project(arguments: &ProjectArguments) -> Result<CheckedProject, CliError> {
    let root = canonical_directory(&arguments.project_root)?;
    let sources = read_sources(&root, &arguments.sources)?;
    let semantic_sources = semantic_sources(&sources);
    let fixed_limits = fixed_limits()?;
    let fault_output = analyze_faults(&semantic_sources, fixed_limits).map_err(compiler_error)?;
    reject_diagnostics(&fault_output.diagnostics)?;
    let fault_model = fault_output.model.ok_or_else(|| {
        CliError::Compiler("fault analysis returned neither model nor diagnostic".to_owned())
    })?;
    let tasks = resolve_tasks(&arguments.tasks, &fault_model.fixed.semantics.symbols)?;
    let mapping = load_mapping(&root, arguments.mapping.as_deref())?;
    let external = mapping.external_entries()?;
    let addresses = analyze_addresses(
        &semantic_sources,
        fixed_limits,
        address_limits()?,
        AddressBindingInputs {
            tag_catalog: &external.tags,
            device_bindings: &external.bindings,
            device_packages: &[],
            program_tasks: &tasks,
        },
    )
    .map_err(compiler_error)?;
    reject_diagnostics(&addresses.diagnostics)?;
    let address_model = addresses.model.ok_or_else(|| {
        CliError::Compiler("address analysis returned neither model nor diagnostic".to_owned())
    })?;
    let work = analyze_cyclic_work(
        &semantic_sources,
        &address_model,
        fixed_limits,
        work_limits()?,
    )
    .map_err(compiler_error)?;
    reject_diagnostics(&work.diagnostics)?;
    let work_model = work.model.ok_or_else(|| {
        CliError::Compiler("work analysis returned neither model nor diagnostic".to_owned())
    })?;
    Ok(CheckedProject {
        source_count: sources.len(),
        task_count: tasks.len(),
        tag_count: address_model.tags.len(),
        device_binding_count: address_model.device_bindings.len(),
        loop_count: work_model.loops.len(),
        sources,
        address_model,
        work_model,
    })
}

fn lower_project(arguments: &ProjectArguments) -> Result<LoweredProject, CliError> {
    let checked = check_project(arguments)?;
    let semantic_sources = semantic_sources(&checked.sources);
    let fixed_limits = fixed_limits()?;
    let work_limits = work_limits()?;
    let output = lower_canonical_ir(
        &semantic_sources,
        &checked.address_model,
        &checked.work_model,
        fixed_limits,
        work_limits,
        initialization_limits()?,
        CanonicalArtifactLimits::new(ir_limits()?, source_map_limits()?, checkpoint_limits()?),
    )
    .map_err(compiler_error)?;
    reject_diagnostics(&output.diagnostics)?;
    let ir = output.ir.ok_or_else(|| {
        CliError::Compiler("lowering returned neither IR nor diagnostic".to_owned())
    })?;
    let source_map = output
        .source_map
        .ok_or_else(|| CliError::Compiler("lowering omitted the atomic source map".to_owned()))?;
    let checkpoints = output.checkpoint_plan.ok_or_else(|| {
        CliError::Compiler("lowering omitted the atomic checkpoint plan".to_owned())
    })?;
    Ok(LoweredProject {
        ir,
        source_map,
        checkpoints,
    })
}

fn read_sources(root: &Path, paths: &[PathBuf]) -> Result<Vec<LoadedSource>, CliError> {
    if paths.len() > MAX_SOURCE_FILES {
        return Err(CliError::InvalidInput(format!(
            "project has {} sources, exceeding R1 gate limit {MAX_SOURCE_FILES}",
            paths.len()
        )));
    }
    let inputs = paths
        .iter()
        .map(|path| read_source_bytes(root, path))
        .collect::<Result<Vec<_>, _>>()?;
    let limits = parser_limits()?;
    let mut diagnostics = Vec::new();
    let mut loaded = Vec::with_capacity(inputs.len());
    for (path, bytes) in inputs {
        let output = parse(&path, &bytes, limits);
        diagnostics.extend(output.diagnostics);
        if let Some(ast) = output.ast {
            let text = String::from_utf8(bytes)
                .map_err(|_| CliError::Compiler("validated source was not UTF-8".to_owned()))?;
            loaded.push(LoadedSource { path, text, ast });
        }
    }
    reject_diagnostics(&diagnostics)?;
    if loaded.len() != paths.len() {
        return Err(CliError::Compiler(
            "parser omitted an AST without publishing a diagnostic".to_owned(),
        ));
    }
    loaded.sort_by(|left, right| left.path.as_bytes().cmp(right.path.as_bytes()));
    for pair in loaded.windows(2) {
        if pair[0].path == pair[1].path {
            return Err(CliError::InvalidInput(format!(
                "duplicate source path `{}`",
                pair[0].path
            )));
        }
    }
    Ok(loaded)
}

fn read_source_bytes(root: &Path, input: &Path) -> Result<(String, Vec<u8>), CliError> {
    let absolute = canonical_file(root, input)?;
    let path = normalized_relative(root, &absolute)?;
    let bytes = read_bounded(&absolute, MAX_SOURCE_BYTES, "source")?;
    Ok((path, bytes))
}

fn semantic_sources(sources: &[LoadedSource]) -> Vec<SemanticSource<'_>> {
    sources
        .iter()
        .map(|source| SemanticSource::new(&source.ast, &source.text))
        .collect()
}

fn resolve_tasks(
    requested: &[TaskArgument],
    symbols: &[aurora_st_ir::SemanticSymbol],
) -> Result<Vec<ProgramTaskBinding>, CliError> {
    if requested.len() > MAX_TASKS {
        return Err(CliError::InvalidInput(format!(
            "project has {} tasks, exceeding R1 gate limit {MAX_TASKS}",
            requested.len()
        )));
    }
    let mut handles = BTreeSet::new();
    let mut bindings = Vec::with_capacity(requested.len());
    for task in requested {
        if !handles.insert(task.handle) {
            return Err(CliError::InvalidInput(format!(
                "task handle {} is bound more than once",
                task.handle
            )));
        }
        let matches = symbols
            .iter()
            .filter(|symbol| {
                symbol.kind == SemanticSymbolKind::Program && symbol.canonical_name == task.program
            })
            .collect::<Vec<_>>();
        let [program] = matches.as_slice() else {
            return Err(CliError::InvalidInput(format!(
                "task handle {} must name exactly one Program `{}`",
                task.handle, task.program
            )));
        };
        bindings.push(ProgramTaskBinding {
            program: program.id,
            task_handle: TaskHandle(task.handle),
        });
    }
    bindings.sort_by_key(|binding| binding.task_handle);
    Ok(bindings)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MappingDocument {
    kind: String,
    schema_version: ContractVersion,
    document_id: String,
    #[serde(default)]
    tag_catalog: Vec<TagIdentity>,
    #[serde(default)]
    device_bindings: Vec<DeviceBinding>,
    #[serde(default)]
    extensions: Option<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractVersion {
    major: u16,
    minor: u16,
    lifecycle: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TagIdentity {
    tag_id: String,
    symbol: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DeviceBinding {
    mapping_version: String,
    binding_id: String,
    tag_id: String,
    device_id: String,
    direction: Direction,
    vendor_endpoint: String,
    width_bits: u8,
    byte_order: ByteOrderValue,
    bit_order: BitOrderValue,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ByteOrderValue {
    Little,
    Big,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BitOrderValue {
    Lsb0,
    Msb0,
}

struct LoadedMapping {
    path: String,
    text: String,
    document: MappingDocument,
}

struct ExternalEntries<'a> {
    tags: Vec<TagCatalogEntry<'a>>,
    bindings: Vec<DeviceBindingEntry<'a>>,
}

struct MappingSpans {
    catalog_tags: Vec<SourceSpan>,
    symbols: Vec<SourceSpan>,
    catalog_entries: Vec<SourceSpan>,
    binding_ids: Vec<SourceSpan>,
    binding_tags: Vec<SourceSpan>,
    device_ids: Vec<SourceSpan>,
    endpoints: Vec<SourceSpan>,
    binding_entries: Vec<SourceSpan>,
}

impl LoadedMapping {
    fn empty() -> Self {
        Self {
            path: "<empty-mapping>".to_owned(),
            text: String::new(),
            document: MappingDocument {
                kind: "aurora.st-address-mapping".to_owned(),
                schema_version: ContractVersion {
                    major: 1,
                    minor: 0,
                    lifecycle: "preview".to_owned(),
                },
                document_id: "01890f3e-4c7b-7cc2-98c4-dc0c0c073900".to_owned(),
                tag_catalog: Vec::new(),
                device_bindings: Vec::new(),
                extensions: None,
            },
        }
    }

    fn external_entries(&self) -> Result<ExternalEntries<'_>, CliError> {
        if self.text.is_empty() {
            return Ok(ExternalEntries {
                tags: Vec::new(),
                bindings: Vec::new(),
            });
        }
        let spans = self.locate_spans()?;
        Ok(ExternalEntries {
            tags: self.catalog_entries(&spans),
            bindings: self.binding_entries(&spans),
        })
    }

    fn locate_spans(&self) -> Result<MappingSpans, CliError> {
        let catalog_range = array_field_range(&self.text, "tagCatalog")?;
        let binding_range = array_field_range(&self.text, "deviceBindings")?;
        let catalog_offset = u32::try_from(catalog_range.start)
            .map_err(|_| CliError::InvalidInput("mapping source span exceeds u32".to_owned()))?;
        let binding_offset = u32::try_from(binding_range.start)
            .map_err(|_| CliError::InvalidInput("mapping source span exceeds u32".to_owned()))?;
        let catalog_source = &self.text[catalog_range];
        let binding_source = &self.text[binding_range];
        let catalog_entries = object_element_spans(catalog_source, catalog_offset)?;
        let binding_entries = object_element_spans(binding_source, binding_offset)?;
        let catalog_tags =
            shifted_spans(string_field_spans(catalog_source, "tagId")?, catalog_offset)?;
        let symbols = shifted_spans(
            string_field_spans(catalog_source, "symbol")?,
            catalog_offset,
        )?;
        let binding_ids = shifted_spans(
            string_field_spans(binding_source, "bindingId")?,
            binding_offset,
        )?;
        let binding_tags =
            shifted_spans(string_field_spans(binding_source, "tagId")?, binding_offset)?;
        let device_ids = shifted_spans(
            string_field_spans(binding_source, "deviceId")?,
            binding_offset,
        )?;
        let endpoints = shifted_spans(
            string_field_spans(binding_source, "vendorEndpoint")?,
            binding_offset,
        )?;
        ensure_span_count(
            "tag catalog tagId",
            self.document.tag_catalog.len(),
            catalog_tags.len(),
        )?;
        ensure_span_count(
            "tag catalog symbol",
            self.document.tag_catalog.len(),
            symbols.len(),
        )?;
        ensure_span_count(
            "tag catalog entries",
            self.document.tag_catalog.len(),
            catalog_entries.len(),
        )?;
        for (field, actual) in [
            ("bindingId", binding_ids.len()),
            ("binding tagId", binding_tags.len()),
            ("deviceId", device_ids.len()),
            ("vendorEndpoint", endpoints.len()),
        ] {
            ensure_span_count(field, self.document.device_bindings.len(), actual)?;
        }
        ensure_span_count(
            "device binding entries",
            self.document.device_bindings.len(),
            binding_entries.len(),
        )?;
        Ok(MappingSpans {
            catalog_tags,
            symbols,
            catalog_entries,
            binding_ids,
            binding_tags,
            device_ids,
            endpoints,
            binding_entries,
        })
    }

    fn catalog_entries<'a>(&'a self, spans: &MappingSpans) -> Vec<TagCatalogEntry<'a>> {
        self.document
            .tag_catalog
            .iter()
            .enumerate()
            .map(|(index, tag)| TagCatalogEntry {
                source_path: &self.path,
                source: &self.text,
                span: spans.catalog_entries[index],
                symbol: ExternalField {
                    value: &tag.symbol,
                    span: spans.symbols[index],
                },
                tag_id: ExternalField {
                    value: &tag.tag_id,
                    span: spans.catalog_tags[index],
                },
            })
            .collect()
    }

    fn binding_entries<'a>(&'a self, spans: &MappingSpans) -> Vec<DeviceBindingEntry<'a>> {
        self.document
            .device_bindings
            .iter()
            .enumerate()
            .map(|(index, binding)| DeviceBindingEntry {
                source_path: &self.path,
                source: &self.text,
                span: spans.binding_entries[index],
                binding_id: ExternalField {
                    value: &binding.binding_id,
                    span: spans.binding_ids[index],
                },
                tag_id: ExternalField {
                    value: &binding.tag_id,
                    span: spans.binding_tags[index],
                },
                device_id: ExternalField {
                    value: &binding.device_id,
                    span: spans.device_ids[index],
                },
                direction: match binding.direction {
                    Direction::Input => MappingDirection::Input,
                    Direction::Output => MappingDirection::Output,
                },
                vendor_endpoint: ExternalField {
                    value: &binding.vendor_endpoint,
                    span: spans.endpoints[index],
                },
                width_bits: binding.width_bits,
                byte_order: Some(match binding.byte_order {
                    ByteOrderValue::Little => aurora_st_ir::ByteOrder::Little,
                    ByteOrderValue::Big => aurora_st_ir::ByteOrder::Big,
                }),
                bit_order: Some(match binding.bit_order {
                    BitOrderValue::Lsb0 => aurora_st_ir::BitOrder::Lsb0,
                    BitOrderValue::Msb0 => aurora_st_ir::BitOrder::Msb0,
                }),
            })
            .collect()
    }
}

fn array_field_range(source: &str, field: &str) -> Result<std::ops::Range<usize>, CliError> {
    let bytes = source.as_bytes();
    let mut index = top_level_field_value_start(source, field)?;
    if bytes.get(index) != Some(&b'[') {
        return Err(CliError::Compiler(format!(
            "schema-validated mapping field `{field}` is not an array"
        )));
    }
    let start = index;
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;
    while let Some(byte) = bytes.get(index).copied() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'\"' {
                in_string = false;
            }
        } else if byte == b'\"' {
            in_string = true;
        } else if byte == b'[' {
            depth = depth.saturating_add(1);
        } else if byte == b']' {
            depth = depth.checked_sub(1).ok_or_else(|| {
                CliError::Compiler(format!(
                    "schema-validated mapping field `{field}` has an invalid array boundary"
                ))
            })?;
            if depth == 0 {
                return Ok(start..index + 1);
            }
        }
        index += 1;
    }
    Err(CliError::Compiler(format!(
        "schema-validated mapping field `{field}` has no closing array boundary"
    )))
}

fn top_level_field_value_start(source: &str, field: &str) -> Result<usize, CliError> {
    let bytes = source.as_bytes();
    let mut index = 0_usize;
    let mut object_depth = 0_usize;
    let mut array_depth = 0_usize;
    while let Some(byte) = bytes.get(index).copied() {
        match byte {
            b'{' => object_depth = object_depth.saturating_add(1),
            b'}' => {
                object_depth = object_depth.checked_sub(1).ok_or_else(|| {
                    CliError::Compiler(
                        "schema-validated mapping has invalid object nesting".to_owned(),
                    )
                })?;
            }
            b'[' => array_depth = array_depth.saturating_add(1),
            b']' => {
                array_depth = array_depth.checked_sub(1).ok_or_else(|| {
                    CliError::Compiler(
                        "schema-validated mapping has invalid array nesting".to_owned(),
                    )
                })?;
            }
            b'\"' => {
                let (end, escaped) = json_string_end(bytes, index)?;
                let matches_field = if escaped {
                    serde_json::from_str::<String>(source.get(index..=end).ok_or_else(|| {
                        CliError::Compiler("mapping key span is invalid".to_owned())
                    })?)
                    .map_err(compiler_error)?
                        == field
                } else {
                    source.get(index + 1..end) == Some(field)
                };
                if object_depth == 1 && array_depth == 0 && matches_field {
                    let mut value = end + 1;
                    while bytes.get(value).is_some_and(u8::is_ascii_whitespace) {
                        value += 1;
                    }
                    if bytes.get(value) == Some(&b':') {
                        value += 1;
                        while bytes.get(value).is_some_and(u8::is_ascii_whitespace) {
                            value += 1;
                        }
                        return Ok(value);
                    }
                }
                index = end;
            }
            _ => {}
        }
        index += 1;
    }
    Err(CliError::Compiler(format!(
        "schema-validated mapping omitted top-level `{field}`"
    )))
}

fn json_string_end(bytes: &[u8], start: usize) -> Result<(usize, bool), CliError> {
    let mut index = start + 1;
    let mut escaped = false;
    let mut contained_escape = false;
    while let Some(byte) = bytes.get(index).copied() {
        if escaped {
            escaped = false;
            contained_escape = true;
        } else if byte == b'\\' {
            escaped = true;
        } else if byte == b'\"' {
            return Ok((index, contained_escape));
        }
        index += 1;
    }
    Err(CliError::Compiler(
        "schema-validated mapping contains an unterminated string".to_owned(),
    ))
}

fn object_element_spans(source: &str, offset: u32) -> Result<Vec<SourceSpan>, CliError> {
    let bytes = source.as_bytes();
    let mut index = 0_usize;
    let mut depth = 0_usize;
    let mut start = None;
    let mut spans = Vec::new();
    while let Some(byte) = bytes.get(index).copied() {
        if byte == b'\"' {
            let (end, _) = json_string_end(bytes, index)?;
            index = end;
        } else if byte == b'{' {
            if depth == 0 {
                start = Some(index);
            }
            depth = depth.saturating_add(1);
        } else if byte == b'}' {
            depth = depth.checked_sub(1).ok_or_else(|| {
                CliError::Compiler("schema-validated mapping has invalid entry nesting".to_owned())
            })?;
            if depth == 0 {
                let entry_start = start.take().ok_or_else(|| {
                    CliError::Compiler("mapping entry start is missing".to_owned())
                })?;
                let span = span_from_usize(entry_start, index + 1)?;
                spans.push(SourceSpan {
                    start: span.start.checked_add(offset).ok_or_else(|| {
                        CliError::InvalidInput("mapping source span overflow".to_owned())
                    })?,
                    end: span.end.checked_add(offset).ok_or_else(|| {
                        CliError::InvalidInput("mapping source span overflow".to_owned())
                    })?,
                });
            }
        }
        index += 1;
    }
    if depth != 0 {
        return Err(CliError::Compiler(
            "schema-validated mapping entry has no closing object boundary".to_owned(),
        ));
    }
    Ok(spans)
}

fn load_mapping(root: &Path, input: Option<&Path>) -> Result<LoadedMapping, CliError> {
    let Some(input) = input else {
        return Ok(LoadedMapping::empty());
    };
    let absolute = canonical_file(root, input)?;
    let path = normalized_relative(root, &absolute)?;
    let bytes = read_bounded(&absolute, MAX_MAPPING_BYTES, "mapping")?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|source| CliError::Json {
        path: absolute.clone(),
        source,
    })?;
    let schema: Value = serde_json::from_str(MAPPING_SCHEMA).map_err(|source| CliError::Json {
        path: PathBuf::from("<embedded-st-address-mapping-schema>"),
        source,
    })?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|error| CliError::Compiler(format!("compile embedded mapping schema: {error}")))?;
    let mut errors = validator
        .iter_errors(&value)
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    errors.sort();
    if !errors.is_empty() {
        return Err(CliError::InvalidInput(format!(
            "mapping `{path}` does not match Preview 1.0: {}",
            errors.join("; ")
        )));
    }
    let document: MappingDocument =
        serde_json::from_value(value).map_err(|source| CliError::Json {
            path: absolute,
            source,
        })?;
    validate_decoded_mapping(&document)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| CliError::Compiler("schema-validated mapping was not UTF-8".to_owned()))?;
    Ok(LoadedMapping {
        path,
        text,
        document,
    })
}

fn validate_decoded_mapping(document: &MappingDocument) -> Result<(), CliError> {
    if document.kind != "aurora.st-address-mapping"
        || document.schema_version.major != 1
        || document.schema_version.minor != 0
        || document.schema_version.lifecycle != "preview"
        || StableId::parse(&document.document_id).is_none()
    {
        return Err(CliError::InvalidInput(
            "mapping header is not Aurora ST Address Mapping Preview 1.0".to_owned(),
        ));
    }
    let _ = &document.extensions;
    if document
        .device_bindings
        .iter()
        .any(|binding| binding.mapping_version != "1.0")
    {
        return Err(CliError::InvalidInput(
            "device binding mappingVersion must be 1.0".to_owned(),
        ));
    }
    Ok(())
}

fn string_field_spans(source: &str, field: &str) -> Result<Vec<SourceSpan>, CliError> {
    let bytes = source.as_bytes();
    let mut cursor = 0_usize;
    let mut spans = Vec::new();
    while let Some(relative) = source[cursor..].find('\"') {
        let key_start = cursor + relative;
        let (key_end, escaped) = json_string_end(bytes, key_start)?;
        let matches_field = if escaped {
            serde_json::from_str::<String>(source.get(key_start..=key_end).ok_or_else(|| {
                CliError::Compiler("mapping field key span is invalid".to_owned())
            })?)
            .map_err(compiler_error)?
                == field
        } else {
            source.get(key_start + 1..key_end) == Some(field)
        };
        let mut index = key_end + 1;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if !matches_field || bytes.get(index) != Some(&b':') {
            cursor = key_end + 1;
            continue;
        }
        index += 1;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        if bytes.get(index) != Some(&b'\"') {
            return Err(CliError::InvalidInput(format!(
                "mapping field `{field}` is not a JSON string"
            )));
        }
        let start = index + 1;
        let (end, _) = json_string_end(bytes, index)?;
        spans.push(span_from_usize(start, end)?);
        cursor = end + 1;
    }
    Ok(spans)
}

fn shifted_spans(spans: Vec<SourceSpan>, offset: u32) -> Result<Vec<SourceSpan>, CliError> {
    spans
        .into_iter()
        .map(|span| {
            Ok(SourceSpan {
                start: span.start.checked_add(offset).ok_or_else(|| {
                    CliError::InvalidInput("mapping source span overflow".to_owned())
                })?,
                end: span.end.checked_add(offset).ok_or_else(|| {
                    CliError::InvalidInput("mapping source span overflow".to_owned())
                })?,
            })
        })
        .collect()
}

fn span_from_usize(start: usize, end: usize) -> Result<SourceSpan, CliError> {
    Ok(SourceSpan {
        start: u32::try_from(start)
            .map_err(|_| CliError::InvalidInput("mapping source span exceeds u32".to_owned()))?,
        end: u32::try_from(end)
            .map_err(|_| CliError::InvalidInput("mapping source span exceeds u32".to_owned()))?,
    })
}

fn ensure_span_count(field: &str, expected: usize, actual: usize) -> Result<(), CliError> {
    if expected == actual {
        Ok(())
    } else {
        Err(CliError::Compiler(format!(
            "mapping field `{field}` decoded {expected} values but located {actual} source spans"
        )))
    }
}

fn canonical_directory(path: &Path) -> Result<PathBuf, CliError> {
    let canonical = fs::canonicalize(path).map_err(|source| CliError::Io {
        operation: "resolve project root",
        path: path.to_path_buf(),
        source,
    })?;
    if !canonical.is_dir() {
        return Err(CliError::InvalidInput(format!(
            "project root `{}` is not a directory",
            canonical.display()
        )));
    }
    Ok(canonical)
}

fn read_bounded(path: &Path, limit: usize, kind: &'static str) -> Result<Vec<u8>, CliError> {
    let file = fs::File::open(path).map_err(|source| CliError::Io {
        operation: "open input",
        path: path.to_path_buf(),
        source,
    })?;
    let read_limit = u64::try_from(limit)
        .ok()
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| CliError::Compiler("input read limit is not representable".to_owned()))?;
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut bounded = file.take(read_limit);
    bounded
        .read_to_end(&mut bytes)
        .map_err(|source| CliError::Io {
            operation: "read input",
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > limit {
        return Err(CliError::InvalidInput(format!(
            "{kind} `{}` exceeds byte limit {limit}",
            path.display()
        )));
    }
    Ok(bytes)
}

fn canonical_file(root: &Path, input: &Path) -> Result<PathBuf, CliError> {
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    };
    let canonical = fs::canonicalize(&candidate).map_err(|source| CliError::Io {
        operation: "resolve input",
        path: candidate,
        source,
    })?;
    if !canonical.is_file() {
        return Err(CliError::InvalidInput(format!(
            "input `{}` is not a file",
            canonical.display()
        )));
    }
    canonical.strip_prefix(root).map_err(|_| {
        CliError::InvalidInput(format!(
            "input `{}` resolves outside project root `{}`",
            canonical.display(),
            root.display()
        ))
    })?;
    Ok(canonical)
}

fn normalized_relative(root: &Path, path: &Path) -> Result<String, CliError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        CliError::InvalidInput(format!(
            "input `{}` is outside project root `{}`",
            path.display(),
            root.display()
        ))
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_str().ok_or_else(|| {
                CliError::InvalidInput(format!("input path `{}` is not UTF-8", path.display()))
            })?),
            _ => {
                return Err(CliError::InvalidInput(format!(
                    "input path `{}` is not normalized",
                    path.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(CliError::InvalidInput("input path is empty".to_owned()));
    }
    Ok(parts.join("/"))
}

fn validate_relative_output(path: &Path) -> Result<(), CliError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CliError::InvalidInput(format!(
            "relative output `{}` must be a normalized child of the project root",
            path.display()
        )));
    }
    Ok(())
}

fn resolve_relative_output(project_root: &Path, output: &Path) -> Result<PathBuf, CliError> {
    validate_relative_output(output)?;
    let root = canonical_directory(project_root)?;
    let parent = output.parent().unwrap_or_else(|| Path::new(""));
    let unresolved_parent = root.join(parent);
    let resolved_parent = fs::canonicalize(&unresolved_parent).map_err(|source| CliError::Io {
        operation: "resolve output parent",
        path: unresolved_parent,
        source,
    })?;
    if !resolved_parent.is_dir() || resolved_parent.strip_prefix(&root).is_err() {
        return Err(CliError::InvalidInput(format!(
            "relative output `{}` resolves outside project root `{}`",
            output.display(),
            root.display()
        )));
    }
    let name = output.file_name().ok_or_else(|| {
        CliError::InvalidInput(format!(
            "relative output `{}` has no directory name",
            output.display()
        ))
    })?;
    let candidate = resolved_parent.join(name);
    if !candidate.exists() {
        return Ok(candidate);
    }
    let resolved = fs::canonicalize(&candidate).map_err(|source| CliError::Io {
        operation: "resolve output directory",
        path: candidate,
        source,
    })?;
    if resolved.strip_prefix(&root).is_err() {
        return Err(CliError::InvalidInput(format!(
            "relative output `{}` resolves outside project root `{}`",
            output.display(),
            root.display()
        )));
    }
    Ok(resolved)
}

fn reject_diagnostics(diagnostics: &[Diagnostic]) -> Result<(), CliError> {
    if diagnostics.is_empty() {
        return Ok(());
    }
    let bytes = diagnostics_to_canonical_json(diagnostics).map_err(compiler_error)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| CliError::Compiler("diagnostic JSON was not UTF-8".to_owned()))?;
    Err(CliError::Diagnostics(text))
}

fn serialize_build_outputs(
    lowered: &LoweredProject,
    artifact: &AotArtifact,
) -> Result<Vec<(&'static str, Vec<u8>)>, CliError> {
    let native = canonical_json(&artifact.native_source_map)?;
    if native.len() > MAX_ARTIFACT_JSON_BYTES {
        return Err(CliError::Compiler(format!(
            "native source map requires {} bytes, exceeding limit {MAX_ARTIFACT_JSON_BYTES}",
            native.len()
        )));
    }
    Ok(vec![
        (
            "canonical-ir.json",
            canonical_ir_to_json(&lowered.ir, ir_limits()?).map_err(compiler_error)?,
        ),
        (
            "checkpoint-plan.json",
            checkpoint_plan_to_json(&lowered.checkpoints, checkpoint_limits()?)
                .map_err(compiler_error)?,
        ),
        ("native-source-map.json", native),
        ("program.o", artifact.object.clone()),
        (
            "source-map.json",
            canonical_source_map_to_json(&lowered.source_map, source_map_limits()?)
                .map_err(compiler_error)?,
        ),
    ])
}

fn publish_outputs(output: &Path, files: &[(&str, Vec<u8>)]) -> Result<(), CliError> {
    if files.len() != OUTPUT_NAMES.len()
        || !files
            .iter()
            .zip(OUTPUT_NAMES)
            .all(|((actual, _), expected)| *actual == expected)
    {
        return Err(CliError::Compiler(
            "build output cardinality does not match the frozen R1 set".to_owned(),
        ));
    }
    let created_directory = if output.exists() {
        if !output.is_dir() {
            return Err(CliError::InvalidInput(format!(
                "output `{}` is not a directory",
                output.display()
            )));
        }
        let mut entries = fs::read_dir(output).map_err(|source| CliError::Io {
            operation: "list output directory",
            path: output.to_path_buf(),
            source,
        })?;
        if entries
            .next()
            .transpose()
            .map_err(|source| CliError::Io {
                operation: "read output directory entry",
                path: output.to_path_buf(),
                source,
            })?
            .is_some()
        {
            return Err(CliError::InvalidInput(format!(
                "output directory `{}` must be empty",
                output.display()
            )));
        }
        false
    } else {
        fs::create_dir(output).map_err(|source| CliError::Io {
            operation: "create output directory",
            path: output.to_path_buf(),
            source,
        })?;
        true
    };
    let result = stage_and_commit_outputs(output, files);
    if result.is_err() && created_directory {
        let _ = fs::remove_dir(output);
    }
    result
}

fn stage_and_commit_outputs(output: &Path, files: &[(&str, Vec<u8>)]) -> Result<(), CliError> {
    let staged = files
        .iter()
        .map(|(name, _)| output.join(format!(".{name}.aurora-tmp")))
        .collect::<Vec<_>>();
    let final_paths = files
        .iter()
        .map(|(name, _)| output.join(name))
        .collect::<Vec<_>>();
    let mut created_staged = Vec::with_capacity(files.len());
    let mut published = Vec::with_capacity(files.len());
    let result = (|| {
        for ((_, bytes), path) in files.iter().zip(&staged) {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|source| CliError::Io {
                    operation: "create staged artifact",
                    path: path.clone(),
                    source,
                })?;
            created_staged.push(path.clone());
            file.write_all(bytes).map_err(|source| CliError::Io {
                operation: "write staged artifact",
                path: path.clone(),
                source,
            })?;
            file.sync_all().map_err(|source| CliError::Io {
                operation: "sync staged artifact",
                path: path.clone(),
                source,
            })?;
        }
        for (staged_path, final_path) in staged.iter().zip(&final_paths) {
            fs::hard_link(staged_path, final_path).map_err(|source| CliError::Io {
                operation: "publish artifact",
                path: final_path.clone(),
                source,
            })?;
            published.push(final_path.clone());
        }
        while let Some(staged_path) = created_staged.last().cloned() {
            fs::remove_file(&staged_path).map_err(|source| CliError::Io {
                operation: "remove staged artifact",
                path: staged_path,
                source,
            })?;
            let _removed = created_staged.pop();
        }
        ensure_exact_output_set(output, files)?;
        Ok(())
    })();
    if result.is_err() {
        for path in created_staged.iter().chain(published.iter()) {
            let _ = fs::remove_file(path);
        }
    }
    result
}

fn ensure_exact_output_set(output: &Path, files: &[(&str, Vec<u8>)]) -> Result<(), CliError> {
    let mut actual = fs::read_dir(output)
        .map_err(|source| CliError::Io {
            operation: "list published output directory",
            path: output.to_path_buf(),
            source,
        })?
        .map(|entry| {
            entry
                .map(|value| value.file_name())
                .map_err(|source| CliError::Io {
                    operation: "read published output directory entry",
                    path: output.to_path_buf(),
                    source,
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    actual.sort();
    let mut expected = files
        .iter()
        .map(|(name, _)| std::ffi::OsString::from(name))
        .collect::<Vec<_>>();
    expected.sort();
    if actual != expected {
        return Err(CliError::InvalidInput(format!(
            "output directory `{}` changed during publication",
            output.display()
        )));
    }
    Ok(())
}

fn canonical_json(value: &impl Serialize) -> Result<Vec<u8>, CliError> {
    serde_jcs::to_vec(value).map_err(compiler_error)
}

fn compiler_error(error: impl std::fmt::Display) -> CliError {
    CliError::Compiler(error.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CheckSummary {
    source_files: usize,
    tasks: usize,
    tags: usize,
    device_bindings: usize,
    bounded_loops: usize,
    status: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildSummary<'a> {
    object_sha256: &'a str,
    outputs: [&'static str; 5],
    status: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceMapQuery<'a> {
    source: &'a aurora_st_ir::SourceFileEntry,
    byte_offset: u32,
    symbols: Vec<&'a aurora_st_ir::SymbolSourceEntry>,
    nodes: Vec<&'a aurora_st_ir::NodeSourceEntry>,
    fault_sites: Vec<&'a aurora_st_ir::FaultSourceEntry>,
}

fn parser_limits() -> Result<ParserLimits, CliError> {
    configured(ParserLimits::new(MAX_SOURCE_BYTES, 131_072, 131_072, 256))
}

fn fixed_limits() -> Result<FixedDataLimits, CliError> {
    configured(FixedDataLimits::new(
        64 * 1024,
        64 * 1024,
        1_048_576,
        64 * 1024 * 1024,
        65_536,
        64 * 1024 * 1024,
        64 * 1024 * 1024,
    ))
}

fn address_limits() -> Result<AddressBindingLimits, CliError> {
    configured(AddressBindingLimits::new(
        1024 * 1024,
        1024 * 1024,
        1024 * 1024,
        65_536,
        65_536,
        1,
        1,
        MAX_TASKS,
    ))
}

fn work_limits() -> Result<CyclicWorkLimits, CliError> {
    configured(CyclicWorkLimits::new(
        65_536, MAX_TASKS, 1_048_576, 10_000_000,
    ))
}

fn initialization_limits() -> Result<InitializationLimits, CliError> {
    configured(InitializationLimits::new(
        64 * 1024 * 1024,
        1024 * 1024 * 1024,
        1024 * 1024 * 1024,
        1024 * 1024 * 1024,
        4 * 1024 * 1024 * 1024,
    ))
}

fn ir_limits() -> Result<CanonicalIrLimits, CliError> {
    configured(CanonicalIrLimits::new(
        1_048_576,
        65_536,
        MAX_ARTIFACT_JSON_BYTES,
    ))
}

fn source_map_limits() -> Result<CanonicalSourceMapLimits, CliError> {
    configured(CanonicalSourceMapLimits::new(
        MAX_SOURCE_FILES,
        1_048_576,
        1_048_576,
        1_048_576,
        MAX_ARTIFACT_JSON_BYTES,
    ))
}

fn checkpoint_limits() -> Result<CheckpointPlanLimits, CliError> {
    configured(CheckpointPlanLimits::new(
        1_048_576,
        1_048_576,
        MAX_ARTIFACT_JSON_BYTES,
    ))
}

fn aot_limits() -> Result<AotLimits, CliError> {
    configured(AotLimits::new(
        131_072,
        16 * 1024 * 1024,
        256 * 1024 * 1024,
        1_048_576,
        1_048_576,
        64 * 1024 * 1024,
    ))
}

fn configured<T, E: std::fmt::Display>(result: Result<T, E>) -> Result<T, CliError> {
    result.map_err(|error| CliError::Compiler(format!("invalid fixed R1 gate limits: {error}")))
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::resolve_relative_output;
    use super::stage_and_commit_outputs;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new() -> Self {
            let serial = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "aurora-cli-r1-08-unit-{}-{serial}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap_or_else(|error| {
                unreachable!("create isolated publication test directory: {error}")
            });
            Self { path }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).unwrap_or_else(|error| {
                unreachable!("remove isolated publication test directory: {error}")
            });
        }
    }

    #[test]
    fn staging_collision_does_not_delete_a_file_owned_by_another_writer() {
        let directory = TestDirectory::new();
        let collision = directory.path.join(".artifact.bin.aurora-tmp");
        fs::write(&collision, b"owned by another writer")
            .unwrap_or_else(|error| unreachable!("write colliding staged artifact: {error}"));

        let result = stage_and_commit_outputs(
            &directory.path,
            &[("artifact.bin", b"aurora artifact".to_vec())],
        );

        assert!(result.is_err());
        let preserved = fs::read(&collision)
            .unwrap_or_else(|error| unreachable!("read preserved colliding artifact: {error}"));
        assert_eq!(preserved, b"owned by another writer");
        assert!(!directory.path.join("artifact.bin").exists());
    }

    #[test]
    fn publication_rejects_extra_entries_without_deleting_them() {
        let directory = TestDirectory::new();
        let foreign = directory.path.join("foreign.bin");
        fs::write(&foreign, b"owned by another writer")
            .unwrap_or_else(|error| unreachable!("write foreign output entry: {error}"));

        let result = stage_and_commit_outputs(
            &directory.path,
            &[("artifact.bin", b"aurora artifact".to_vec())],
        );

        let Err(error) = result else {
            unreachable!("an extra output entry must reject publication");
        };
        assert!(error.to_string().contains("changed during publication"));
        let preserved = fs::read(&foreign)
            .unwrap_or_else(|error| unreachable!("read preserved foreign entry: {error}"));
        assert_eq!(preserved, b"owned by another writer");
        assert!(!directory.path.join("artifact.bin").exists());
    }

    #[cfg(unix)]
    #[test]
    fn relative_output_rejects_a_symlinked_parent_outside_the_project() {
        use std::os::unix::fs::symlink;

        let directory = TestDirectory::new();
        let project = directory.path.join("project");
        let outside = directory.path.join("outside");
        fs::create_dir(&project)
            .unwrap_or_else(|error| unreachable!("create project directory: {error}"));
        fs::create_dir(&outside)
            .unwrap_or_else(|error| unreachable!("create outside directory: {error}"));
        symlink(&outside, project.join("escape"))
            .unwrap_or_else(|error| unreachable!("create output parent symlink: {error}"));

        let result = resolve_relative_output(&project, PathBuf::from("escape/build").as_path());

        let Err(error) = result else {
            unreachable!("relative output through an escaping symlink must be rejected");
        };
        assert!(error.to_string().contains("resolves outside project root"));
        assert!(!outside.join("build").exists());
    }
}
