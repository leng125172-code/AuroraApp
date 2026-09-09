//! R1-05 logical address, writer, mapping, and handle boundary tests.

use aurora_st_ir::{
    AddressAnalysisOutput, AddressBindingInputs, AddressBindingLimits, BitOrder, ByteOrder,
    DeviceBindingEntry, DeviceEndpoint, DiagnosticCode, ExternalField, FixedDataLimits,
    LockedDevicePackage, MappingDirection, MappingTransform, ParserLimits, ProgramTaskBinding,
    SemanticSource, SourceSpan, StableId, TagCatalogEntry, TaskHandle, VersionedAst,
    analyze_addresses, analyze_faults, parse,
};

const CATALOG_SOURCE: &str = "tag catalog entries used by the compiler test";
const MAPPING_SOURCE: &str = "device mapping entries used by the compiler test";
const LOCK_SOURCE: &str = "locked device package used by the compiler test";
const TAG_A: &str = "01890f3e-4c7b-7cc2-98c4-dc0c0c073901";
const TAG_B: &str = "01890f3e-4c7b-7cc2-98c4-dc0c0c073902";
const TAG_C: &str = "01890f3e-4c7b-7cc2-98c4-dc0c0c073903";
const TAG_D: &str = "01890f3e-4c7b-7cc2-98c4-dc0c0c073904";
const DEVICE: &str = "01890f3e-4c7b-7cc2-98c4-dc0c0c073910";
const PACKAGE: &str = "01890f3e-4c7b-7cc2-98c4-dc0c0c073911";

fn parser_limits() -> ParserLimits {
    ParserLimits::new(64 * 1024, 8 * 1024, 8 * 1024, 256)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn fixed_limits() -> FixedDataLimits {
    FixedDataLimits::new(1024, 1024, 1024, 64 * 1024, 1024, 64 * 1024, 64 * 1024)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn address_limits() -> AddressBindingLimits {
    AddressBindingLimits::new(128, 128, 128, 64, 64, 16, 64, 64)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn parsed(source: &str) -> VersionedAst {
    let output = parse("program/main.st", source.as_bytes(), parser_limits());
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    output
        .ast
        .unwrap_or_else(|| unreachable!("diagnostic-free parse publishes an AST"))
}

fn span(start: u32) -> SourceSpan {
    SourceSpan {
        start,
        end: start + 1,
    }
}

fn field(value: &str, start: u32) -> ExternalField<'_> {
    ExternalField {
        value,
        span: span(start),
    }
}

fn catalog<'a>(symbol: &'a str, tag_id: &'a str, start: u32) -> TagCatalogEntry<'a> {
    TagCatalogEntry {
        source_path: "project/tags.json",
        source: CATALOG_SOURCE,
        span: span(start),
        symbol: field(symbol, start),
        tag_id: field(tag_id, start),
    }
}

#[allow(clippy::too_many_arguments)]
fn binding<'a>(
    tag_id: &'a str,
    binding_id: &'a str,
    start: u32,
    direction: MappingDirection,
    endpoint: &'a str,
    width_bits: u8,
) -> DeviceBindingEntry<'a> {
    DeviceBindingEntry {
        source_path: "project/mapping.json",
        source: MAPPING_SOURCE,
        span: span(start),
        binding_id: field(binding_id, start),
        tag_id: field(tag_id, start),
        device_id: field(DEVICE, start),
        direction,
        vendor_endpoint: field(endpoint, start),
        width_bits,
        byte_order: Some(ByteOrder::Little),
        bit_order: Some(BitOrder::Lsb0),
    }
}

fn stable_id(value: &str) -> StableId {
    StableId::parse(value).unwrap_or_else(|| unreachable!("test UUID is a canonical UUIDv7"))
}

fn analyze_one(
    source: &str,
    catalog_entries: &[TagCatalogEntry<'_>],
    bindings: &[DeviceBindingEntry<'_>],
    packages: &[LockedDevicePackage<'_>],
    task_handles: &[u32],
) -> AddressAnalysisOutput {
    let ast = parsed(source);
    let semantic_source = SemanticSource::new(&ast, source);
    let fault_output = analyze_faults(&[semantic_source], fixed_limits())
        .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"));
    let model = fault_output.model.unwrap_or_else(|| {
        unreachable!("test source passes R1-04: {:?}", fault_output.diagnostics)
    });
    let program = model
        .fixed
        .semantics
        .symbols
        .iter()
        .find(|symbol| symbol.name == "Main")
        .map(|symbol| symbol.id);
    let tasks: Vec<ProgramTaskBinding> = program
        .into_iter()
        .flat_map(|program| {
            task_handles
                .iter()
                .map(move |task_handle| ProgramTaskBinding {
                    program,
                    task_handle: TaskHandle(*task_handle),
                })
        })
        .collect();
    analyze_addresses(
        &[semantic_source],
        fixed_limits(),
        address_limits(),
        AddressBindingInputs {
            tag_catalog: catalog_entries,
            device_bindings: bindings,
            device_packages: packages,
            program_tasks: &tasks,
        },
    )
    .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"))
}

fn codes(output: &AddressAnalysisOutput) -> Vec<DiagnosticCode> {
    output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn valid_project_generates_exactly_one_handle_per_tag_and_one_binding_per_io_tag() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  StartButton AT %IX0.0 : BOOL;
  SpeedCommand AT %QW2 : UINT := UINT#0;
  WorkCounter AT %MD100 : DINT := DINT#0;
END_VAR
PROGRAM Main
  SpeedCommand := UINT#1;
  WorkCounter := DINT#1;
END_PROGRAM
";
    let catalog_entries = [
        catalog("workcounter", TAG_C, 30),
        catalog("startbutton", TAG_A, 10),
        catalog("speedcommand", TAG_B, 20),
    ];
    let bindings = [
        binding(
            TAG_B,
            "01890f3e-4c7b-7cc2-98c4-dc0c0c073922",
            20,
            MappingDirection::Output,
            "output-word",
            16,
        ),
        binding(
            TAG_A,
            "01890f3e-4c7b-7cc2-98c4-dc0c0c073921",
            10,
            MappingDirection::Input,
            "input-bit",
            1,
        ),
    ];
    let transforms = [MappingTransform {
        byte_order: ByteOrder::Little,
        bit_order: BitOrder::Lsb0,
    }];
    let endpoints = [
        DeviceEndpoint {
            vendor_endpoint: "input-bit",
            start_bit: 0,
            end_bit: 1,
            direction: MappingDirection::Input,
            width_bits: 1,
            transforms: &transforms,
        },
        DeviceEndpoint {
            vendor_endpoint: "output-word",
            start_bit: 16,
            end_bit: 32,
            direction: MappingDirection::Output,
            width_bits: 16,
            transforms: &transforms,
        },
    ];
    let packages = [LockedDevicePackage {
        device_id: stable_id(DEVICE),
        package_id: stable_id(PACKAGE),
        available: true,
        source_path: "project/packages.lock.json",
        source: LOCK_SOURCE,
        span: span(0),
        endpoints: &endpoints,
    }];

    let output = analyze_one(source, &catalog_entries, &bindings, &packages, &[7]);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let model = output
        .model
        .unwrap_or_else(|| unreachable!("valid binding publishes a model"));
    assert_eq!(model.tags.len(), 3);
    assert_eq!(
        model
            .tags
            .iter()
            .map(|tag| tag.handle.0)
            .collect::<Vec<_>>(),
        vec![0, 1, 2]
    );
    assert_eq!(model.device_bindings.len(), 2);
    assert!(model.snapshot_dependencies.is_empty());
}

#[test]
fn image_end_boundary_is_inclusive_only_for_the_last_representable_bit() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  LastBit AT %MX0.7 : BOOL;
  TooFar AT %MX1.0 : BOOL;
END_VAR
";
    let entries = [catalog("lastbit", TAG_A, 1), catalog("toofar", TAG_B, 2)];
    let limits = AddressBindingLimits::new(0, 0, 1, 8, 8, 1, 1, 1)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    let ast = parsed(source);
    let output = analyze_addresses(
        &[SemanticSource::new(&ast, source)],
        fixed_limits(),
        limits,
        AddressBindingInputs {
            tag_catalog: &entries,
            device_bindings: &[],
            device_packages: &[],
            program_tasks: &[],
        },
    )
    .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"));
    assert_eq!(codes(&output), vec![DiagnosticCode::AddressOutOfRange]);
    assert!(output.model.is_none());
}

#[test]
fn lowercase_address_spelling_is_bound_and_unrepresentable_offsets_are_range_errors() {
    let lowercase_source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  State AT %md0 : DINT;
END_VAR
";
    let entries = [catalog("state", TAG_A, 1)];
    let lowercase = analyze_one(lowercase_source, &entries, &[], &[], &[]);
    let model = lowercase
        .model
        .unwrap_or_else(|| unreachable!("case-insensitive address is valid"));
    assert_eq!(model.tags.len(), 1);
    assert_eq!(model.tags[0].address.start_bit, 0);

    let overflow_source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  State AT %MD18446744073709551616 : DINT;
END_VAR
";
    let overflow = analyze_one(overflow_source, &entries, &[], &[], &[]);
    assert_eq!(codes(&overflow), vec![DiagnosticCode::AddressOutOfRange]);
}

#[test]
fn type_alignment_and_overlap_each_emit_once_without_pair_explosion() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Misaligned AT %MW1 : UINT;
  WrongWidth AT %MW2 : UDINT;
  First AT %MB4 : USINT;
  Cover AT %MW4 : UINT;
END_VAR
";
    let entries = [
        catalog("misaligned", TAG_A, 1),
        catalog("wrongwidth", TAG_B, 2),
        catalog("first", TAG_C, 3),
        catalog("cover", TAG_D, 4),
    ];
    let output = analyze_one(source, &entries, &[], &[], &[]);
    assert_eq!(
        codes(&output),
        vec![
            DiagnosticCode::AddressMisaligned,
            DiagnosticCode::AddressTypeMismatch,
            DiagnosticCode::AddressOverlap,
        ]
    );
}

#[test]
fn every_input_assignment_target_is_rejected_exactly_once() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  InputBit AT %IX0.0 : BOOL;
END_VAR
PROGRAM Main
  InputBit := TRUE;
  InputBit := FALSE;
END_PROGRAM
";
    let entries = [catalog("inputbit", TAG_A, 1)];
    let bindings = [binding(
        TAG_A,
        "01890f3e-4c7b-7cc2-98c4-dc0c0c073921",
        1,
        MappingDirection::Input,
        "input-bit",
        1,
    )];
    let transforms = [MappingTransform {
        byte_order: ByteOrder::Little,
        bit_order: BitOrder::Lsb0,
    }];
    let endpoints = [DeviceEndpoint {
        vendor_endpoint: "input-bit",
        start_bit: 0,
        end_bit: 1,
        direction: MappingDirection::Input,
        width_bits: 1,
        transforms: &transforms,
    }];
    let packages = [LockedDevicePackage {
        device_id: stable_id(DEVICE),
        package_id: stable_id(PACKAGE),
        available: true,
        source_path: "project/packages.lock.json",
        source: LOCK_SOURCE,
        span: span(0),
        endpoints: &endpoints,
    }];
    let output = analyze_one(source, &entries, &bindings, &packages, &[1]);
    assert_eq!(
        codes(&output),
        vec![
            DiagnosticCode::InputWriteForbidden,
            DiagnosticCode::InputWriteForbidden,
        ]
    );
}

#[test]
fn duplicate_catalog_and_mapping_entries_report_only_the_later_entries() {
    let memory_source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  State AT %MD0 : DINT;
END_VAR
";
    let duplicate_catalog = [
        catalog("state", TAG_A, 1),
        catalog("state", TAG_B, 2),
        catalog("state", TAG_C, 3),
    ];
    let catalog_output = analyze_one(memory_source, &duplicate_catalog, &[], &[], &[]);
    assert_eq!(
        codes(&catalog_output),
        vec![
            DiagnosticCode::DuplicateTagCatalogEntry,
            DiagnosticCode::DuplicateTagCatalogEntry,
        ]
    );

    let input_source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  InputBit AT %IX0.0 : BOOL;
END_VAR
";
    let entries = [catalog("inputbit", TAG_A, 1)];
    let bindings = [
        binding(
            TAG_A,
            "01890f3e-4c7b-7cc2-98c4-dc0c0c073921",
            1,
            MappingDirection::Input,
            "input-bit",
            1,
        ),
        binding(
            TAG_A,
            "01890f3e-4c7b-7cc2-98c4-dc0c0c073922",
            2,
            MappingDirection::Input,
            "input-bit-2",
            1,
        ),
        binding(
            TAG_A,
            "01890f3e-4c7b-7cc2-98c4-dc0c0c073923",
            3,
            MappingDirection::Input,
            "input-bit-3",
            1,
        ),
    ];
    let mapping_output = analyze_one(input_source, &entries, &bindings, &[], &[]);
    assert_eq!(
        codes(&mapping_output),
        vec![
            DiagnosticCode::DevicePackageUnavailable,
            DiagnosticCode::MappingDuplicate,
            DiagnosticCode::MappingDuplicate,
        ]
    );
}

#[test]
fn an_unmapped_io_tag_emits_one_missing_binding_diagnostic() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  InputBit AT %IX0.0 : BOOL;
END_VAR
";
    let entries = [catalog("inputbit", TAG_A, 1)];
    let output = analyze_one(source, &entries, &[], &[], &[]);
    assert_eq!(codes(&output), vec![DiagnosticCode::MappingMissing]);
}

#[test]
fn an_output_without_a_writer_emits_one_error_and_suppresses_mapping_cascades() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  OutputBit AT %QX0.0 : BOOL;
END_VAR
";
    let entries = [catalog("outputbit", TAG_A, 1)];
    let output = analyze_one(source, &entries, &[], &[], &[]);
    assert_eq!(codes(&output), vec![DiagnosticCode::OutputWriterMissing]);
}

#[test]
fn physical_overlap_reports_each_later_binding_once() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  FirstInput AT %IB0 : USINT;
  SecondInput AT %IB1 : USINT;
END_VAR
";
    let entries = [
        catalog("firstinput", TAG_A, 1),
        catalog("secondinput", TAG_B, 2),
    ];
    let bindings = [
        binding(
            TAG_A,
            "01890f3e-4c7b-7cc2-98c4-dc0c0c073921",
            1,
            MappingDirection::Input,
            "first-byte",
            8,
        ),
        binding(
            TAG_B,
            "01890f3e-4c7b-7cc2-98c4-dc0c0c073922",
            2,
            MappingDirection::Input,
            "same-byte",
            8,
        ),
    ];
    let transforms = [MappingTransform {
        byte_order: ByteOrder::Little,
        bit_order: BitOrder::Lsb0,
    }];
    let endpoints = [
        DeviceEndpoint {
            vendor_endpoint: "first-byte",
            start_bit: 0,
            end_bit: 8,
            direction: MappingDirection::Input,
            width_bits: 8,
            transforms: &transforms,
        },
        DeviceEndpoint {
            vendor_endpoint: "same-byte",
            start_bit: 0,
            end_bit: 8,
            direction: MappingDirection::Input,
            width_bits: 8,
            transforms: &transforms,
        },
    ];
    let packages = [LockedDevicePackage {
        device_id: stable_id(DEVICE),
        package_id: stable_id(PACKAGE),
        available: true,
        source_path: "project/packages.lock.json",
        source: LOCK_SOURCE,
        span: span(0),
        endpoints: &endpoints,
    }];
    let output = analyze_one(source, &entries, &bindings, &packages, &[]);
    assert_eq!(codes(&output), vec![DiagnosticCode::PhysicalAddressOverlap]);
}

#[test]
fn one_writer_and_one_foreign_reader_generate_one_snapshot_dependency() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  SharedState AT %MD0 : DINT;
END_VAR
PROGRAM Writer
  SharedState := DINT#1;
END_PROGRAM
PROGRAM Reader
VAR_TEMP
  Copy : DINT;
END_VAR
  Copy := SharedState;
END_PROGRAM
";
    let ast = parsed(source);
    let semantic_source = SemanticSource::new(&ast, source);
    let fault_output = analyze_faults(&[semantic_source], fixed_limits())
        .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"));
    let fault_model = fault_output
        .model
        .unwrap_or_else(|| unreachable!("test source passes R1-04"));
    let program = |name: &str| {
        fault_model
            .fixed
            .semantics
            .symbols
            .iter()
            .find(|symbol| symbol.name == name)
            .map_or_else(|| unreachable!("test Program exists"), |symbol| symbol.id)
    };
    let tasks = [
        ProgramTaskBinding {
            program: program("Writer"),
            task_handle: TaskHandle(3),
        },
        ProgramTaskBinding {
            program: program("Reader"),
            task_handle: TaskHandle(9),
        },
    ];
    let entries = [catalog("sharedstate", TAG_A, 1)];
    let output = analyze_addresses(
        &[semantic_source],
        fixed_limits(),
        address_limits(),
        AddressBindingInputs {
            tag_catalog: &entries,
            device_bindings: &[],
            device_packages: &[],
            program_tasks: &tasks,
        },
    )
    .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"));
    let model = output
        .model
        .unwrap_or_else(|| unreachable!("uniquely owned cross-task read is valid"));
    assert_eq!(model.snapshot_dependencies.len(), 1);
    let dependency = model.snapshot_dependencies[0];
    assert_eq!(dependency.source_task, TaskHandle(3));
    assert_eq!(dependency.target_task, TaskHandle(9));
    assert_eq!(dependency.tag_handle.0, 0);
}

#[test]
fn catalog_order_does_not_change_dense_tag_id_handle_assignment() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  LaterId AT %MD0 : DINT;
  EarlierId AT %MD4 : DINT;
END_VAR
";
    let forward = [catalog("laterid", TAG_B, 1), catalog("earlierid", TAG_A, 2)];
    let reverse = [catalog("earlierid", TAG_A, 2), catalog("laterid", TAG_B, 1)];
    let left = analyze_one(source, &forward, &[], &[], &[])
        .model
        .unwrap_or_else(|| unreachable!("valid project publishes a model"));
    let right = analyze_one(source, &reverse, &[], &[], &[])
        .model
        .unwrap_or_else(|| unreachable!("valid project publishes a model"));
    assert_eq!(left.tags, right.tags);
    assert_eq!(left.tags[0].symbol, "earlierid");
    assert_eq!(left.tags[0].handle.0, 0);
    assert_eq!(left.tags[1].handle.0, 1);
}

#[test]
fn multiple_task_writers_produce_one_tag_level_diagnostic() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  OutputBit AT %QX0.0 : BOOL;
END_VAR
PROGRAM First
  OutputBit := TRUE;
END_PROGRAM
PROGRAM Second
  OutputBit := FALSE;
END_PROGRAM
";
    let ast = parsed(source);
    let semantic_source = SemanticSource::new(&ast, source);
    let fault_output = analyze_faults(&[semantic_source], fixed_limits())
        .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"));
    let fault_model = fault_output
        .model
        .unwrap_or_else(|| unreachable!("test source passes R1-04"));
    let program = |name: &str| {
        fault_model
            .fixed
            .semantics
            .symbols
            .iter()
            .find(|symbol| symbol.name == name)
            .map_or_else(|| unreachable!("test Program exists"), |symbol| symbol.id)
    };
    let tasks = [
        ProgramTaskBinding {
            program: program("First"),
            task_handle: TaskHandle(2),
        },
        ProgramTaskBinding {
            program: program("Second"),
            task_handle: TaskHandle(5),
        },
    ];
    let entries = [catalog("outputbit", TAG_A, 1)];
    let output = analyze_addresses(
        &[semantic_source],
        fixed_limits(),
        address_limits(),
        AddressBindingInputs {
            tag_catalog: &entries,
            device_bindings: &[],
            device_packages: &[],
            program_tasks: &tasks,
        },
    )
    .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"));
    assert_eq!(codes(&output), vec![DiagnosticCode::MultipleWriters]);
}
