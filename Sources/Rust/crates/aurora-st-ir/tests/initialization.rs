//! R1-06 step 3: canonical initialization images and final task-instance budgets.

use aurora_st_ir::{
    AddressBindingInputs, AddressBindingLimits, AddressSemanticModel, CyclicWorkLimits,
    CyclicWorkModel, DiagnosticCode, ExternalField, FixedDataLimits, FixedFieldLayout,
    InitializationLimits, InitializationModel, ParserLimits, ProgramTaskBinding, SemanticSource,
    SourceSpan, TagCatalogEntry, TaskHandle, VersionedAst, analyze_addresses, analyze_cyclic_work,
    build_initialization_images, parse,
};

const TAG_ID: &str = "01890f3e-4c7b-7cc2-98c4-dc0c0c073901";
const EXTERNAL_SOURCE: &str = "x";

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

fn work_limits() -> CyclicWorkLimits {
    CyclicWorkLimits::new(16, 16, 64, 4096)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn generous_initialization_limits() -> InitializationLimits {
    InitializationLimits::new(
        64 * 1024,
        1024 * 1024,
        1024 * 1024,
        1024 * 1024,
        4 * 1024 * 1024,
    )
    .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn source() -> &'static str {
    r#"AURORA_ST VERSION 1.0;
TYPE
  Mode : (Idle := DINT#1, Run := DINT#3);
  Pair : STRUCT
    Flag : BOOL := NOT FALSE;
    Value : DINT := CHECKED_ADD(DINT#1, DINT#2);
  END_STRUCT;
  Pairs : ARRAY[0..1] OF Pair;
END_TYPE
VAR_GLOBAL
  Seed AT %MD0 : DINT := DINT#11;
END_VAR
PROGRAM Main
VAR
  Values : Pairs;
  Text : STRING[5] := CONCAT('A', '\u{00e9}');
  Wide : WSTRING[3] := "\u{1d11e}";
  Selected : Mode := Mode#Run;
  Ratio : REAL := SQRT(REAL#4.0);
  DefaultNumber : UINT;
  DefaultText : STRING[2];
  DefaultMode : Mode;
END_VAR
VAR_TEMP
  Scratch : LINT := LINT#-2;
END_VAR
RETURN;
END_PROGRAM
"#
}

fn parsed(path: &str, source: &str) -> VersionedAst {
    let output = parse(path, source.as_bytes(), parser_limits());
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    output
        .ast
        .unwrap_or_else(|| unreachable!("diagnostic-free parse publishes an AST"))
}

fn accepted_models(
    ast: &VersionedAst,
    source_text: &str,
    task_handles: &[u32],
) -> (AddressSemanticModel, CyclicWorkModel) {
    let semantic_source = SemanticSource::new(ast, source_text);
    let catalog = [TagCatalogEntry {
        source_path: "project/tags.json",
        source: EXTERNAL_SOURCE,
        span: SourceSpan { start: 0, end: 1 },
        symbol: ExternalField {
            value: "seed",
            span: SourceSpan { start: 0, end: 1 },
        },
        tag_id: ExternalField {
            value: TAG_ID,
            span: SourceSpan { start: 0, end: 1 },
        },
    }];
    let fault_output = aurora_st_ir::analyze_faults(&[semantic_source], fixed_limits())
        .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"));
    let fault_model = fault_output.model.unwrap_or_else(|| {
        unreachable!("test source passes R1-04: {:?}", fault_output.diagnostics)
    });
    let program = fault_model
        .fixed
        .semantics
        .symbols
        .iter()
        .find(|symbol| symbol.name == "Main")
        .map_or_else(|| unreachable!("test declares Main"), |symbol| symbol.id);
    let tasks = task_handles
        .iter()
        .map(|task| ProgramTaskBinding {
            program,
            task_handle: TaskHandle(*task),
        })
        .collect::<Vec<_>>();
    let address_output = analyze_addresses(
        &[semantic_source],
        fixed_limits(),
        address_limits(),
        AddressBindingInputs {
            tag_catalog: &catalog,
            device_bindings: &[],
            device_packages: &[],
            program_tasks: &tasks,
        },
    )
    .unwrap_or_else(|error| unreachable!("accepted inputs have valid shape: {error}"));
    let address_model = address_output.model.unwrap_or_else(|| {
        unreachable!("test source passes R1-05: {:?}", address_output.diagnostics)
    });
    let work_output = analyze_cyclic_work(
        &[semantic_source],
        &address_model,
        fixed_limits(),
        work_limits(),
    )
    .unwrap_or_else(|error| unreachable!("accepted inputs have valid shape: {error}"));
    let work_model = work_output.model.unwrap_or_else(|| {
        unreachable!(
            "test source passes work proof: {:?}",
            work_output.diagnostics
        )
    });
    (address_model, work_model)
}

fn build(
    ast: &VersionedAst,
    address: &AddressSemanticModel,
    work: &CyclicWorkModel,
    limits: InitializationLimits,
) -> aurora_st_ir::InitializationOutput {
    build_initialization_images(
        &[SemanticSource::new(ast, source())],
        address,
        work,
        fixed_limits(),
        work_limits(),
        limits,
    )
    .unwrap_or_else(|error| unreachable!("accepted models initialize successfully: {error}"))
}

fn field<'a>(fields: &'a [FixedFieldLayout], name: &str) -> &'a FixedFieldLayout {
    fields
        .iter()
        .find(|field| field.name == name)
        .unwrap_or_else(|| unreachable!("test field exists: {name}"))
}

fn field_bytes<'a>(
    image: &'a [u8],
    fields: &[FixedFieldLayout],
    name: &str,
    size: usize,
) -> &'a [u8] {
    let offset = usize::try_from(field(fields, name).offset_bytes).unwrap_or(usize::MAX);
    image
        .get(offset..offset.saturating_add(size))
        .unwrap_or_else(|| unreachable!("field bytes fit the accepted layout"))
}

#[test]
fn images_cover_each_owner_once_and_preserve_exact_values_and_zero_padding() {
    let ast = parsed("program/main.st", source());
    let (address, work) = accepted_models(&ast, source(), &[9, 7]);
    let output = build(&ast, &address, &work, generous_initialization_limits());
    assert!(output.diagnostics.is_empty());
    let model = output
        .model
        .unwrap_or_else(|| unreachable!("successful build publishes all images"));

    assert_eq!(model.globals.len(), 1);
    assert_eq!(model.globals[0].bytes, 11_i32.to_le_bytes());
    assert_eq!(
        model.tasks.iter().map(|task| task.task).collect::<Vec<_>>(),
        [TaskHandle(7), TaskHandle(9)]
    );
    assert_eq!(model.tasks[0].bytes, model.tasks[1].bytes);

    let program = &address.faults.fixed.programs[0];
    let image = &model.tasks[0].bytes;
    let values = field_bytes(image, &program.fields, "Values", 16);
    assert_eq!(&values[0..8], &[1, 0, 0, 0, 3, 0, 0, 0]);
    assert_eq!(&values[8..16], &[1, 0, 0, 0, 3, 0, 0, 0]);

    let text = field_bytes(image, &program.fields, "Text", 12);
    assert_eq!(&text[..7], &[3, 0, 0, 0, b'A', 0xc3, 0xa9]);
    assert!(text[7..].iter().all(|byte| *byte == 0));

    let wide = field_bytes(image, &program.fields, "Wide", 12);
    assert_eq!(&wide[..8], &[2, 0, 0, 0, 0x34, 0xd8, 0x1e, 0xdd]);
    assert!(wide[8..].iter().all(|byte| *byte == 0));
    assert_eq!(
        field_bytes(image, &program.fields, "Selected", 4),
        3_i32.to_le_bytes()
    );
    assert_eq!(
        field_bytes(image, &program.fields, "Ratio", 4),
        2.0_f32.to_bits().to_le_bytes()
    );
    assert_eq!(
        field_bytes(image, &program.fields, "DefaultNumber", 2),
        [0, 0]
    );
    assert!(
        field_bytes(image, &program.fields, "DefaultText", 8)
            .iter()
            .all(|byte| *byte == 0)
    );
    assert_eq!(
        field_bytes(image, &program.fields, "DefaultMode", 4),
        1_i32.to_le_bytes()
    );

    let frame = model
        .frames
        .iter()
        .find(|frame| frame.pou == program.program)
        .unwrap_or_else(|| unreachable!("Program frame exists"));
    assert_eq!(frame.bytes, (-2_i64).to_le_bytes());
    assert_eq!(model.total_task_state_bytes, program.size_bytes * 2);
    assert_eq!(
        model.total_initialization_bytes,
        model.total_task_state_bytes + model.total_global_bytes + model.total_frame_bytes
    );
}

#[test]
fn every_budget_accepts_equality_and_rejects_one_byte_less_without_partial_images() {
    let ast = parsed("program/main.st", source());
    let (address, work) = accepted_models(&ast, source(), &[7, 9]);
    let baseline = build(&ast, &address, &work, generous_initialization_limits())
        .model
        .unwrap_or_else(|| unreachable!("generous limits publish images"));
    let per_task = u64::try_from(baseline.tasks[0].bytes.len()).unwrap_or(u64::MAX);
    let exact = InitializationLimits::new(
        per_task,
        baseline.total_task_state_bytes,
        baseline.total_global_bytes,
        baseline.total_frame_bytes,
        baseline.total_initialization_bytes,
    )
    .unwrap_or_else(|error| unreachable!("nonzero exact limits are valid: {error}"));
    assert!(build(&ast, &address, &work, exact).diagnostics.is_empty());

    let candidates = [
        InitializationLimits::new(
            per_task - 1,
            baseline.total_task_state_bytes,
            baseline.total_global_bytes,
            baseline.total_frame_bytes,
            baseline.total_initialization_bytes,
        ),
        InitializationLimits::new(
            per_task,
            baseline.total_task_state_bytes - 1,
            baseline.total_global_bytes,
            baseline.total_frame_bytes,
            baseline.total_initialization_bytes,
        ),
        InitializationLimits::new(
            per_task,
            baseline.total_task_state_bytes,
            baseline.total_global_bytes - 1,
            baseline.total_frame_bytes,
            baseline.total_initialization_bytes,
        ),
        InitializationLimits::new(
            per_task,
            baseline.total_task_state_bytes,
            baseline.total_global_bytes,
            baseline.total_frame_bytes - 1,
            baseline.total_initialization_bytes,
        ),
        InitializationLimits::new(
            per_task,
            baseline.total_task_state_bytes,
            baseline.total_global_bytes,
            baseline.total_frame_bytes,
            baseline.total_initialization_bytes - 1,
        ),
    ];
    for limits in candidates {
        let limits =
            limits.unwrap_or_else(|error| unreachable!("one-less limit stays nonzero: {error}"));
        let rejected = build(&ast, &address, &work, limits);
        assert!(rejected.model.is_none());
        assert_eq!(rejected.diagnostics.len(), 1);
        assert_eq!(
            rejected.diagnostics[0].code,
            DiagnosticCode::ResourceBudgetExceeded
        );
    }
}

#[test]
fn explicit_default_from_another_source_uses_its_own_source_identity() {
    let type_source = r"AURORA_ST VERSION 1.0;
TYPE
  Count : DINT := DINT#7;
END_TYPE
";
    let program_source = r"AURORA_ST VERSION 1.0;
PROGRAM Main
VAR
  Value : Count;
END_VAR
RETURN;
END_PROGRAM
";
    let type_ast = parsed("types/count.st", type_source);
    let program_ast = parsed("program/main.st", program_source);
    let sources = [
        SemanticSource::new(&program_ast, program_source),
        SemanticSource::new(&type_ast, type_source),
    ];
    let fault = aurora_st_ir::analyze_faults(&sources, fixed_limits())
        .unwrap_or_else(|error| unreachable!("parser-produced sources are valid: {error}"));
    let fault_model = fault
        .model
        .unwrap_or_else(|| unreachable!("sources pass R1-04: {:?}", fault.diagnostics));
    let program = fault_model
        .fixed
        .semantics
        .symbols
        .iter()
        .find(|symbol| symbol.name == "Main")
        .map_or_else(|| unreachable!("test declares Main"), |symbol| symbol.id);
    let tasks = [ProgramTaskBinding {
        program,
        task_handle: TaskHandle(4),
    }];
    let addresses = analyze_addresses(
        &sources,
        fixed_limits(),
        address_limits(),
        AddressBindingInputs {
            tag_catalog: &[],
            device_bindings: &[],
            device_packages: &[],
            program_tasks: &tasks,
        },
    )
    .unwrap_or_else(|error| unreachable!("accepted inputs have valid shape: {error}"));
    let address = addresses
        .model
        .unwrap_or_else(|| unreachable!("sources pass R1-05: {:?}", addresses.diagnostics));
    let work = analyze_cyclic_work(&sources, &address, fixed_limits(), work_limits())
        .unwrap_or_else(|error| unreachable!("accepted inputs have valid shape: {error}"));
    let work = work
        .model
        .unwrap_or_else(|| unreachable!("sources pass work proof: {:?}", work.diagnostics));
    let initialized = build_initialization_images(
        &sources,
        &address,
        &work,
        fixed_limits(),
        work_limits(),
        generous_initialization_limits(),
    )
    .unwrap_or_else(|error| unreachable!("cross-source default initializes: {error}"));
    assert_eq!(
        initialized
            .model
            .and_then(|model| model.tasks.into_iter().next())
            .map(|task| task.bytes),
        Some(7_i32.to_le_bytes().to_vec())
    );
}

#[test]
fn every_zero_budget_is_rejected_before_generation() {
    assert!(InitializationLimits::new(0, 1, 1, 1, 1).is_err());
    assert!(InitializationLimits::new(1, 0, 1, 1, 1).is_err());
    assert!(InitializationLimits::new(1, 1, 0, 1, 1).is_err());
    assert!(InitializationLimits::new(1, 1, 1, 0, 1).is_err());
    assert!(InitializationLimits::new(1, 1, 1, 1, 0).is_err());
}

#[test]
fn canonical_ir_embeds_the_same_complete_initialization_model() {
    let ast = parsed("program/main.st", source());
    let (address, work) = accepted_models(&ast, source(), &[7]);
    let standalone = build(&ast, &address, &work, generous_initialization_limits())
        .model
        .unwrap_or_else(|| unreachable!("standalone initialization succeeds"));
    let canonical = aurora_st_ir::lower_canonical_ir(
        &[SemanticSource::new(&ast, source())],
        &address,
        &work,
        fixed_limits(),
        work_limits(),
        generous_initialization_limits(),
        aurora_st_ir::CanonicalArtifactLimits::new(
            aurora_st_ir::CanonicalIrLimits::new(4096, 16, 4 * 1024 * 1024)
                .unwrap_or_else(|error| unreachable!("test limits are valid: {error}")),
            aurora_st_ir::CanonicalSourceMapLimits::new(16, 4096, 4096, 4096, 4 * 1024 * 1024)
                .unwrap_or_else(|error| unreachable!("test limits are valid: {error}")),
        ),
    )
    .unwrap_or_else(|error| unreachable!("accepted project lowers: {error}"));
    assert_eq!(
        canonical.ir.map(|ir| ir.initialization),
        Some::<InitializationModel>(standalone)
    );
}
