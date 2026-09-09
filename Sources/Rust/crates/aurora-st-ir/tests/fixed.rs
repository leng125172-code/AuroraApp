//! Fixed-capacity data, canonical layout, and static FB-instance boundaries for R1-03.

use aurora_st_ir::{
    DiagnosticCode, FixedAnalysisOutput, FixedDataLimitError, FixedDataLimits, FixedInitializer,
    FixedSemanticModel, FixedTypeKind, FixedTypeLayout, ParserLimits, SemanticSource, VersionedAst,
    analyze_fixed, parse,
};

fn parser_limits() -> ParserLimits {
    ParserLimits::new(64 * 1024, 8 * 1024, 8 * 1024, 256)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn limits() -> FixedDataLimits {
    FixedDataLimits::new(1024, 1024, 1024, 64 * 1024, 1024, 64 * 1024, 64 * 1024)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn parsed(path: &str, source: &str) -> VersionedAst {
    let output = parse(path, source.as_bytes(), parser_limits());
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    output
        .ast
        .unwrap_or_else(|| unreachable!("diagnostic-free parsing publishes an AST"))
}

fn analyze_one(source: &str, fixed_limits: FixedDataLimits) -> FixedAnalysisOutput {
    let ast = parsed("program/main.st", source);
    analyze_fixed(&[SemanticSource::new(&ast, source)], fixed_limits)
        .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"))
}

fn model(output: FixedAnalysisOutput) -> FixedSemanticModel {
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    output
        .model
        .unwrap_or_else(|| unreachable!("diagnostic-free analysis publishes a model"))
}

fn named_type<'a>(model: &'a FixedSemanticModel, name: &str) -> &'a FixedTypeLayout {
    let symbol = model
        .semantics
        .symbols
        .iter()
        .find(|symbol| symbol.name == name)
        .unwrap_or_else(|| unreachable!("named type symbol exists"));
    model
        .types
        .iter()
        .find(|layout| layout.declaration == Some(symbol.id))
        .unwrap_or_else(|| unreachable!("named type layout exists"))
}

fn codes(output: &FixedAnalysisOutput) -> Vec<DiagnosticCode> {
    output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn all_target_profile_limits_are_mandatory_and_nonzero() {
    let cases = [
        (
            0,
            1,
            1,
            1,
            1,
            1,
            1,
            FixedDataLimitError::ZeroStringPayloadBytes,
        ),
        (
            1,
            0,
            1,
            1,
            1,
            1,
            1,
            FixedDataLimitError::ZeroWstringCodeUnits,
        ),
        (1, 1, 0, 1, 1, 1, 1, FixedDataLimitError::ZeroArrayElements),
        (1, 1, 1, 0, 1, 1, 1, FixedDataLimitError::ZeroTypeSizeBytes),
        (
            1,
            1,
            1,
            1,
            0,
            1,
            1,
            FixedDataLimitError::ZeroStaticFunctionBlockInstances,
        ),
        (
            1,
            1,
            1,
            1,
            1,
            0,
            1,
            FixedDataLimitError::ZeroProgramStaticBytes,
        ),
        (
            1,
            1,
            1,
            1,
            1,
            1,
            0,
            FixedDataLimitError::ZeroInvocationFrameBytes,
        ),
    ];
    for (string, wstring, array, ty, instances, program, frame, expected) in cases {
        assert_eq!(
            FixedDataLimits::new(string, wstring, array, ty, instances, program, frame),
            Err(expected)
        );
    }
}

#[test]
fn canonical_layout_has_exact_padding_defaults_and_no_duplicate_types() {
    let source = r"AURORA_ST VERSION 1.0;
TYPE
  Label : STRING[8];
  Wide : WSTRING[3];
  Samples : ARRAY[-1..1] OF UINT;
  Packet : STRUCT
    Enabled : BOOL;
    Count : DINT;
    Code : UINT;
  END_STRUCT;
  Mode : (Idle, Run := DINT#2, Stop);
END_TYPE
PROGRAM Main
VAR
  P : Packet;
  S : Samples;
  L : Label;
  M : Mode;
END_VAR
RETURN;
END_PROGRAM
";
    let model = model(analyze_one(source, limits()));

    assert_eq!(
        model.types.len(),
        8,
        "five named and three interned scalars"
    );
    let label = named_type(&model, "Label");
    assert_eq!((label.size_bytes, label.alignment_bytes), (12, 4));
    assert_eq!(label.default_initializer, FixedInitializer::EmptyString);
    let wide = named_type(&model, "Wide");
    assert_eq!((wide.size_bytes, wide.alignment_bytes), (12, 4));

    let samples = named_type(&model, "Samples");
    let FixedTypeKind::Array {
        lower,
        upper,
        element_count,
        element_stride_bytes,
        ..
    } = samples.kind
    else {
        unreachable!("Samples is an array")
    };
    assert_eq!(
        (lower, upper, element_count, element_stride_bytes),
        (-1, 1, 3, 2)
    );
    assert_eq!((samples.size_bytes, samples.alignment_bytes), (6, 2));

    let packet = named_type(&model, "Packet");
    let FixedTypeKind::Structure { fields } = &packet.kind else {
        unreachable!("Packet is a structure")
    };
    assert_eq!(
        fields
            .iter()
            .map(|field| (field.name.as_str(), field.offset_bytes))
            .collect::<Vec<_>>(),
        [("Enabled", 0), ("Count", 4), ("Code", 8)]
    );
    assert_eq!((packet.size_bytes, packet.alignment_bytes), (12, 4));

    let mode = named_type(&model, "Mode");
    let FixedTypeKind::Enumeration { members } = &mode.kind else {
        unreachable!("Mode is an enumeration")
    };
    assert_eq!(
        members
            .iter()
            .map(|member| (member.name.as_str(), member.value))
            .collect::<Vec<_>>(),
        [("Idle", 0), ("Run", 2), ("Stop", 3)]
    );
    assert_eq!(
        mode.default_initializer,
        FixedInitializer::FirstEnumerationMember {
            name: "Idle".to_owned(),
            value: 0,
        }
    );

    let program = &model.programs[0];
    assert_eq!(program.size_bytes, 36);
    assert_eq!(
        program
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field.offset_bytes))
            .collect::<Vec<_>>(),
        [("P", 0), ("S", 12), ("L", 20), ("M", 32)]
    );
}

#[test]
fn exact_capacity_boundaries_pass_and_one_less_rejects_once() {
    let source = r"AURORA_ST VERSION 1.0;
TYPE
  Label : STRING[8];
  Wide : WSTRING[3];
  Samples : ARRAY[0..2] OF UINT;
END_TYPE
PROGRAM Main
RETURN;
END_PROGRAM
";
    let exact = FixedDataLimits::new(8, 3, 3, 12, 1, 1, 1)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    assert!(analyze_one(source, exact).diagnostics.is_empty());

    let string_short = FixedDataLimits::new(7, 3, 3, 12, 1, 1, 1)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    assert_eq!(
        codes(&analyze_one(source, string_short)),
        [DiagnosticCode::InvalidTypeCapacity]
    );
    let wide_short = FixedDataLimits::new(8, 2, 3, 12, 1, 1, 1)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    assert_eq!(
        codes(&analyze_one(source, wide_short)),
        [DiagnosticCode::InvalidTypeCapacity]
    );
    let array_short = FixedDataLimits::new(8, 3, 2, 12, 1, 1, 1)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    assert_eq!(
        codes(&analyze_one(source, array_short)),
        [DiagnosticCode::InvalidTypeCapacity]
    );
    let type_short = FixedDataLimits::new(8, 3, 3, 11, 1, 1, 1)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    assert_eq!(
        codes(&analyze_one(source, type_short)),
        [
            DiagnosticCode::InvalidTypeCapacity,
            DiagnosticCode::InvalidTypeCapacity
        ]
    );
}

#[test]
fn string_length_prefix_accepts_u32_max_and_rejects_one_more() {
    let exact_source = r"AURORA_ST VERSION 1.0;
TYPE
  Largest : STRING[4294967295];
END_TYPE
PROGRAM Main
RETURN;
END_PROGRAM
";
    let exact = FixedDataLimits::new(
        u64::from(u32::MAX) + 1,
        1,
        1,
        u64::from(u32::MAX) + 5,
        1,
        1,
        1,
    )
    .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    let model = model(analyze_one(exact_source, exact));
    assert_eq!(named_type(&model, "Largest").size_bytes, 4_294_967_300);

    let too_large_source = exact_source.replace("4294967295", "4294967296");
    assert_eq!(
        codes(&analyze_one(&too_large_source, exact)),
        [DiagnosticCode::InvalidTypeCapacity]
    );
}

#[test]
fn dynamic_mixed_and_reversed_array_bounds_are_rejected_without_a_model() {
    let dynamic = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Count AT %MD0 : DINT;
END_VAR
TYPE
  Values : ARRAY[0..Count] OF DINT;
END_TYPE
PROGRAM Main
RETURN;
END_PROGRAM
";
    let output = analyze_one(dynamic, limits());
    assert_eq!(codes(&output), [DiagnosticCode::DynamicCyclicStorage]);
    assert!(output.model.is_none());

    let contextual = r"AURORA_ST VERSION 1.0;
TYPE
  Values : ARRAY[DINT#0..1] OF DINT;
END_TYPE
PROGRAM Main
RETURN;
END_PROGRAM
";
    assert!(analyze_one(contextual, limits()).diagnostics.is_empty());

    let mixed = r"AURORA_ST VERSION 1.0;
TYPE
  Values : ARRAY[DINT#0..INT#1] OF DINT;
END_TYPE
PROGRAM Main
RETURN;
END_PROGRAM
";
    assert_eq!(
        codes(&analyze_one(mixed, limits())),
        [DiagnosticCode::InvalidTypeCapacity]
    );
    let reversed = mixed.replace("DINT#0..INT#1", "DINT#2..DINT#1");
    assert_eq!(
        codes(&analyze_one(&reversed, limits())),
        [DiagnosticCode::InvalidTypeCapacity]
    );
}

#[test]
fn capacity_constants_use_lossless_common_types_and_exact_arithmetic_diagnostics() {
    let widened = r"AURORA_ST VERSION 1.0;
TYPE
  Values : ARRAY[0..(INT#1 + DINT#2)] OF DINT;
  Mode : (Ready := INT#1 + DINT#2);
END_TYPE
PROGRAM Main
RETURN;
END_PROGRAM
";
    let model = model(analyze_one(widened, limits()));
    let values = named_type(&model, "Values");
    let FixedTypeKind::Array { element_count, .. } = values.kind else {
        unreachable!("Values is an array")
    };
    assert_eq!(element_count, 4);

    let division_by_zero = widened
        .replace("(INT#1 + DINT#2)", "(DINT#1 / DINT#0)")
        .replace("INT#1 + DINT#2", "DINT#1 / DINT#0");
    assert_eq!(
        codes(&analyze_one(&division_by_zero, limits())),
        [
            DiagnosticCode::ConstantDivisionByZero,
            DiagnosticCode::ConstantDivisionByZero,
        ]
    );

    let overflow = widened
        .replace("(INT#1 + DINT#2)", "(DINT#2147483647 + DINT#1)")
        .replace("INT#1 + DINT#2", "DINT#2147483647 + DINT#1");
    assert_eq!(
        codes(&analyze_one(&overflow, limits())),
        [
            DiagnosticCode::ConstantOverflow,
            DiagnosticCode::ConstantOverflow,
        ]
    );
}

#[test]
fn intermediate_fixed_width_overflow_cannot_be_hidden_by_a_later_operation() {
    let source = r"AURORA_ST VERSION 1.0;
TYPE
  Values : ARRAY[0..((-LINT#-9223372036854775808) - LINT#1)] OF BOOL;
END_TYPE
PROGRAM Main
RETURN;
END_PROGRAM
";
    assert_eq!(
        codes(&analyze_one(source, limits())),
        [DiagnosticCode::ConstantOverflow]
    );
}

#[test]
fn aliases_do_not_repeat_the_target_capacity_diagnostic() {
    let source = r"AURORA_ST VERSION 1.0;
TYPE
  Count : DINT;
  OtherCount : Count;
END_TYPE
PROGRAM Main
RETURN;
END_PROGRAM
";
    let too_small = FixedDataLimits::new(1, 1, 1, 3, 1, 1, 1)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    assert_eq!(
        codes(&analyze_one(source, too_small)),
        [DiagnosticCode::InvalidTypeCapacity]
    );
}

#[test]
fn enum_representation_rejects_duplicate_and_out_of_range_values() {
    let duplicate = r"AURORA_ST VERSION 1.0;
TYPE
  Mode : (Idle := 0, Run := 0);
END_TYPE
PROGRAM Main
RETURN;
END_PROGRAM
";
    assert_eq!(
        codes(&analyze_one(duplicate, limits())),
        [DiagnosticCode::InvalidInitializer]
    );
    let out_of_range = duplicate.replace("Run := 0", "Run := 2147483648");
    assert_eq!(
        codes(&analyze_one(&out_of_range, limits())),
        [DiagnosticCode::InvalidExplicitConversion]
    );
}

#[test]
fn recursive_type_and_function_block_graphs_are_rejected_once() {
    let recursive_type = r"AURORA_ST VERSION 1.0;
TYPE
  Node : STRUCT
    Next : Node;
  END_STRUCT;
END_TYPE
PROGRAM Main
RETURN;
END_PROGRAM
";
    assert_eq!(
        codes(&analyze_one(recursive_type, limits())),
        [DiagnosticCode::RecursiveInstance]
    );

    let recursive_fb = r"AURORA_ST VERSION 1.0;
FUNCTION_BLOCK Loop
VAR
  Next : Loop;
END_VAR
RETURN;
END_FUNCTION_BLOCK
PROGRAM Main
RETURN;
END_PROGRAM
";
    assert_eq!(
        codes(&analyze_one(recursive_fb, limits())),
        [DiagnosticCode::RecursiveInstance]
    );
}

#[test]
fn static_fb_instances_are_expanded_exactly_once_with_isolated_offsets() {
    let source = r"AURORA_ST VERSION 1.0;
FUNCTION_BLOCK Inner
VAR
  State : DINT;
END_VAR
RETURN;
END_FUNCTION_BLOCK
FUNCTION_BLOCK Outer
VAR
  A, B : Inner;
END_VAR
VAR_TEMP
  Scratch : LINT;
END_VAR
RETURN;
END_FUNCTION_BLOCK
PROGRAM Main
VAR
  First, Second : Outer;
END_VAR
RETURN;
END_PROGRAM
";
    let model = model(analyze_one(source, limits()));
    assert_eq!(model.programs[0].size_bytes, 16);
    assert_eq!(model.invocation_frames.len(), 3);
    assert_eq!(
        model
            .function_block_instances
            .iter()
            .map(|instance| {
                (
                    instance.instance_id,
                    instance.path.as_str(),
                    instance.offset_bytes,
                    instance.size_bytes,
                )
            })
            .collect::<Vec<_>>(),
        [
            (0, "First", 0, 8),
            (1, "First.A", 0, 4),
            (2, "First.B", 4, 4),
            (3, "Second", 8, 8),
            (4, "Second.A", 8, 4),
            (5, "Second.B", 12, 4),
        ]
    );
    let outer = named_type(&model, "Outer");
    let FixedTypeKind::FunctionBlock {
        temporary_size_bytes,
        ..
    } = outer.kind
    else {
        unreachable!("Outer is a function block")
    };
    assert_eq!(temporary_size_bytes, 8);
}

#[test]
fn array_fb_instances_use_declared_indices_and_exact_element_count() {
    let source = r"AURORA_ST VERSION 1.0;
FUNCTION_BLOCK Unit
VAR
  State : DINT;
END_VAR
RETURN;
END_FUNCTION_BLOCK
PROGRAM Main
VAR
  Units : ARRAY[2..3] OF Unit;
END_VAR
RETURN;
END_PROGRAM
";
    let model = model(analyze_one(source, limits()));
    assert_eq!(model.programs[0].size_bytes, 8);
    assert_eq!(
        model
            .function_block_instances
            .iter()
            .map(|instance| {
                (
                    instance.instance_id,
                    instance.path.as_str(),
                    instance.offset_bytes,
                )
            })
            .collect::<Vec<_>>(),
        [(0, "Units[2]", 0), (1, "Units[3]", 4)]
    );
}

#[test]
fn fixed_capacities_in_globals_and_function_returns_are_not_skipped() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Label AT %MB0 : STRING[9];
END_VAR
FUNCTION MakeLabel : STRING[9]
RETURN 'x';
END_FUNCTION
PROGRAM Main
RETURN;
END_PROGRAM
";
    let short = FixedDataLimits::new(8, 1, 1, 64, 1, 1, 16)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    assert_eq!(
        codes(&analyze_one(source, short)),
        [
            DiagnosticCode::InvalidTypeCapacity,
            DiagnosticCode::InvalidTypeCapacity,
        ]
    );
}

#[test]
fn resource_budgets_accept_exact_values_and_reject_one_less_once() {
    let source = r"AURORA_ST VERSION 1.0;
FUNCTION_BLOCK Inner
VAR
  State : DINT;
END_VAR
RETURN;
END_FUNCTION_BLOCK
FUNCTION_BLOCK Outer
VAR
  A, B : Inner;
END_VAR
VAR_TEMP
  Scratch : LINT;
END_VAR
RETURN;
END_FUNCTION_BLOCK
PROGRAM Main
VAR
  First, Second : Outer;
END_VAR
RETURN;
END_PROGRAM
";
    let exact = FixedDataLimits::new(1, 1, 1, 8, 6, 16, 8)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    assert!(analyze_one(source, exact).diagnostics.is_empty());
    let instance_short = FixedDataLimits::new(1, 1, 1, 8, 5, 16, 8)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    assert_eq!(
        codes(&analyze_one(source, instance_short)),
        [DiagnosticCode::ResourceBudgetExceeded]
    );
    let program_short = FixedDataLimits::new(1, 1, 1, 8, 6, 15, 8)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    assert_eq!(
        codes(&analyze_one(source, program_short)),
        [DiagnosticCode::ResourceBudgetExceeded]
    );
    let frame_short = FixedDataLimits::new(1, 1, 1, 8, 6, 16, 7)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"));
    assert_eq!(
        codes(&analyze_one(source, frame_short)),
        [DiagnosticCode::ResourceBudgetExceeded]
    );
}

#[test]
fn named_and_field_initializers_are_preserved_without_sharing_instance_identity() {
    let source = r"AURORA_ST VERSION 1.0;
TYPE
  Count : DINT := DINT#7;
END_TYPE
PROGRAM Main
VAR
  FromType : Count;
  Explicit : Count := Count#9;
END_VAR
RETURN;
END_PROGRAM
";
    let model = model(analyze_one(source, limits()));
    let count = named_type(&model, "Count");
    assert!(matches!(
        count.default_initializer,
        FixedInitializer::ExplicitExpression { .. }
    ));
    let fields = &model.programs[0].fields;
    assert_eq!(fields.len(), 2);
    assert!(matches!(
        fields[0].initializer,
        FixedInitializer::ExplicitExpression { .. }
    ));
    assert!(matches!(
        fields[1].initializer,
        FixedInitializer::ExplicitExpression { .. }
    ));
    assert_ne!(fields[0].offset_bytes, fields[1].offset_bytes);
}

#[test]
fn global_initializers_are_retained_and_runtime_dependencies_are_rejected() {
    let valid = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Seed AT %MD0 : DINT := DINT#7;
END_VAR
PROGRAM Main
VAR
  StaticValue : DINT := CHECKED_ADD(DINT#1, DINT#2);
END_VAR
RETURN;
END_PROGRAM
";
    let model = model(analyze_one(valid, limits()));
    assert_eq!(model.globals.len(), 1);
    assert!(matches!(
        model.globals[0].initializer,
        FixedInitializer::ExplicitExpression { .. }
    ));
    assert!(matches!(
        model.programs[0].fields[0].initializer,
        FixedInitializer::ExplicitExpression { .. }
    ));

    let dynamic = valid.replace(
        "StaticValue : DINT := CHECKED_ADD(DINT#1, DINT#2)",
        "StaticValue : DINT := Seed",
    );
    let output = analyze_one(&dynamic, limits());
    assert_eq!(codes(&output), [DiagnosticCode::InvalidInitializer]);
    assert!(output.model.is_none());
}

#[test]
fn user_function_calls_cannot_hide_runtime_initializer_dependencies() {
    let source = r"AURORA_ST VERSION 1.0;
FUNCTION Increment : DINT
VAR_INPUT
  Value : DINT;
END_VAR
RETURN CHECKED_ADD(Value, DINT#1);
END_FUNCTION
PROGRAM Main
VAR
  DynamicValue : DINT := Increment(DINT#1);
END_VAR
RETURN;
END_PROGRAM
";
    assert_eq!(
        codes(&analyze_one(source, limits())),
        [DiagnosticCode::InvalidInitializer]
    );
}

#[test]
fn source_order_does_not_change_layout_or_instance_generation() {
    let declarations = r"AURORA_ST VERSION 1.0;
FUNCTION_BLOCK State
VAR
  Value : DINT;
END_VAR
RETURN;
END_FUNCTION_BLOCK
";
    let program = r"AURORA_ST VERSION 1.0;
PROGRAM Main
VAR
  First, Second : State;
END_VAR
RETURN;
END_PROGRAM
";
    let declaration_ast = parsed("a/declarations.st", declarations);
    let program_ast = parsed("z/program.st", program);
    let forward = analyze_fixed(
        &[
            SemanticSource::new(&declaration_ast, declarations),
            SemanticSource::new(&program_ast, program),
        ],
        limits(),
    )
    .unwrap_or_else(|error| unreachable!("parser-produced inputs are valid: {error}"));
    let reverse = analyze_fixed(
        &[
            SemanticSource::new(&program_ast, program),
            SemanticSource::new(&declaration_ast, declarations),
        ],
        limits(),
    )
    .unwrap_or_else(|error| unreachable!("parser-produced inputs are valid: {error}"));
    assert_eq!(forward, reverse);
}

#[test]
fn cross_file_recursive_back_edge_is_anchored_to_the_referencing_source() {
    let first = r"AURORA_ST VERSION 1.0;
TYPE
  A : STRUCT
    Next : B;
  END_STRUCT;
END_TYPE
";
    let second = r"AURORA_ST VERSION 1.0;
TYPE
  B : STRUCT
    Next : A;
  END_STRUCT;
END_TYPE
PROGRAM Main
RETURN;
END_PROGRAM
";
    let first_ast = parsed("a/types.st", first);
    let second_ast = parsed("z/program.st", second);
    let output = analyze_fixed(
        &[
            SemanticSource::new(&second_ast, second),
            SemanticSource::new(&first_ast, first),
        ],
        limits(),
    )
    .unwrap_or_else(|error| unreachable!("parser-produced inputs are valid: {error}"));
    assert_eq!(codes(&output), [DiagnosticCode::RecursiveInstance]);
    let diagnostic = &output.diagnostics[0];
    assert_eq!(diagnostic.source_path, "z/program.st");
    let start = diagnostic.span.start as usize;
    let end = diagnostic.span.end as usize;
    assert_eq!(&second[start..end], "A");
}
