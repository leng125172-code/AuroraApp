//! Arithmetic, conversion, capacity, and ARRAY-index boundaries for R1-04.

use aurora_st_ir::{
    DiagnosticCode, FaultAnalysisOutput, FaultOperationKind, FaultSemanticModel, FixedDataLimits,
    IntegerArithmeticError, IntegerArithmeticMode, IntegerOperation, IntegerType, ParserLimits,
    RuntimeFaultCode, SemanticSource, VersionedAst, analyze_faults, evaluate_integer_operation,
    parse, validate_array_index,
};

fn parser_limits() -> ParserLimits {
    ParserLimits::new(64 * 1024, 8 * 1024, 8 * 1024, 256)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn fixed_limits() -> FixedDataLimits {
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

fn analyze_one(source: &str) -> FaultAnalysisOutput {
    let ast = parsed("program/main.st", source);
    analyze_faults(&[SemanticSource::new(&ast, source)], fixed_limits())
        .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"))
}

fn model(output: FaultAnalysisOutput) -> FaultSemanticModel {
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    output
        .model
        .unwrap_or_else(|| unreachable!("diagnostic-free analysis publishes a model"))
}

fn codes(output: &FaultAnalysisOutput) -> Vec<DiagnosticCode> {
    output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn integer_policy_covers_exact_minimum_maximum_and_fault_edges() {
    let checked = Some(IntegerArithmeticMode::Checked);
    let saturating = Some(IntegerArithmeticMode::Saturating);
    let wrapping = Some(IntegerArithmeticMode::Wrapping);
    assert_eq!(
        evaluate_integer_operation(
            IntegerType::Dint,
            IntegerOperation::Add,
            checked,
            i128::from(i32::MAX),
            Some(1),
        ),
        Err(IntegerArithmeticError::RuntimeFault(
            RuntimeFaultCode::IntegerOverflow
        ))
    );
    assert_eq!(
        evaluate_integer_operation(
            IntegerType::Dint,
            IntegerOperation::Add,
            saturating,
            i128::from(i32::MAX),
            Some(1),
        ),
        Ok(i128::from(i32::MAX))
    );
    assert_eq!(
        evaluate_integer_operation(
            IntegerType::Dint,
            IntegerOperation::Add,
            wrapping,
            i128::from(i32::MAX),
            Some(1),
        ),
        Ok(i128::from(i32::MIN))
    );
    assert_eq!(
        evaluate_integer_operation(
            IntegerType::Ulint,
            IntegerOperation::Multiply,
            wrapping,
            i128::from(u64::MAX),
            Some(i128::from(u64::MAX)),
        ),
        Ok(1)
    );
    assert_eq!(
        evaluate_integer_operation(
            IntegerType::Lint,
            IntegerOperation::Divide,
            None,
            i128::from(i64::MIN),
            Some(-1),
        ),
        Err(IntegerArithmeticError::RuntimeFault(
            RuntimeFaultCode::IntegerOverflow
        ))
    );
    assert_eq!(
        evaluate_integer_operation(
            IntegerType::Lint,
            IntegerOperation::Modulo,
            None,
            i128::from(i64::MIN),
            Some(-1),
        ),
        Ok(0)
    );
    assert_eq!(
        evaluate_integer_operation(
            IntegerType::Uint,
            IntegerOperation::Divide,
            None,
            1,
            Some(0),
        ),
        Err(IntegerArithmeticError::RuntimeFault(
            RuntimeFaultCode::IntegerDivisionByZero
        ))
    );
    assert_eq!(
        evaluate_integer_operation(
            IntegerType::Sint,
            IntegerOperation::BitwiseNot,
            None,
            0,
            None,
        ),
        Ok(-1)
    );
    assert_eq!(
        evaluate_integer_operation(
            IntegerType::Usint,
            IntegerOperation::BitwiseXor,
            None,
            0b1010,
            Some(0b1100),
        ),
        Ok(0b0110)
    );
}

#[test]
fn array_index_policy_accepts_both_inclusive_edges_only() {
    assert_eq!(validate_array_index(-2, -2, 3), Ok(()));
    assert_eq!(validate_array_index(3, -2, 3), Ok(()));
    assert_eq!(
        validate_array_index(-3, -2, 3),
        Err(RuntimeFaultCode::ArrayIndexOutOfBounds)
    );
    assert_eq!(
        validate_array_index(4, -2, 3),
        Err(RuntimeFaultCode::ArrayIndexOutOfBounds)
    );
}

#[test]
fn constant_failures_emit_one_diagnostic_and_no_model() {
    let overflow = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT := DINT#2147483647 + DINT#1;\nEND_VAR\nEND_PROGRAM\n",
    );
    assert_eq!(codes(&overflow), vec![DiagnosticCode::ConstantOverflow]);
    assert!(overflow.model.is_none());

    let division = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT := DINT#1 / DINT#0;\nEND_VAR\nEND_PROGRAM\n",
    );
    assert_eq!(
        codes(&division),
        vec![DiagnosticCode::ConstantDivisionByZero]
    );
    assert!(division.model.is_none());

    let float = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : REAL := REAL#1.0 / REAL#0.0;\nEND_VAR\nEND_PROGRAM\n",
    );
    assert_eq!(codes(&float), vec![DiagnosticCode::NonFiniteConstant]);
    assert!(float.model.is_none());
}

#[test]
fn dynamic_plain_integer_arithmetic_emits_exactly_one_mode_diagnostic() {
    let output = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT;\nEND_VAR\nValue := Value + DINT#1;\nEND_PROGRAM\n",
    );
    assert_eq!(codes(&output), vec![DiagnosticCode::ArithmeticModeRequired]);
    assert!(output.model.is_none());
}

#[test]
fn only_faultable_dynamic_integer_operations_create_sites() {
    let output = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT;\n    Divisor : DINT;\nEND_VAR\nValue := CHECKED_ADD(Value, DINT#1);\nValue := SATURATING_ADD(Value, DINT#1);\nValue := WRAPPING_ADD(Value, DINT#1);\nValue := ABS(Value);\nValue := Value / Divisor;\nValue := Value MOD Divisor;\nEND_PROGRAM\n",
    );
    let model = model(output);
    assert_eq!(model.fault_sites.len(), 4);
    assert_eq!(
        model
            .fault_sites
            .iter()
            .map(|site| site.id.operation)
            .collect::<Vec<_>>(),
        vec![
            FaultOperationKind::CheckedInteger,
            FaultOperationKind::IntegerAbsolute,
            FaultOperationKind::IntegerDivision,
            FaultOperationKind::IntegerModulo,
        ]
    );
    assert_eq!(
        model.fault_sites[2].possible_faults,
        vec![
            RuntimeFaultCode::IntegerOverflow,
            RuntimeFaultCode::IntegerDivisionByZero,
        ]
    );
    assert!(
        model
            .fault_sites
            .windows(2)
            .all(|sites| sites[0].id < sites[1].id)
    );
}

#[test]
fn known_integer_operands_neither_overgenerate_nor_hide_faults() {
    let safe = model(analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT;\nEND_VAR\nValue := CHECKED_ADD(Value, DINT#0);\nValue := CHECKED_SUB(Value, DINT#0);\nValue := CHECKED_MUL(Value, DINT#0);\nValue := CHECKED_MUL(Value, DINT#1);\nValue := Value / DINT#2;\nValue := Value MOD DINT#2;\nEND_PROGRAM\n",
    ));
    assert!(safe.fault_sites.is_empty(), "{:?}", safe.fault_sites);

    let overflow_only = model(analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT;\nEND_VAR\nValue := Value / DINT#-1;\nEND_PROGRAM\n",
    ));
    assert_eq!(overflow_only.fault_sites.len(), 1);
    assert_eq!(
        overflow_only.fault_sites[0].possible_faults,
        [RuntimeFaultCode::IntegerOverflow]
    );

    let zero_only = model(analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT;\n    Divisor : DINT;\nEND_VAR\nValue := DINT#1 / Divisor;\nEND_PROGRAM\n",
    ));
    assert_eq!(zero_only.fault_sites.len(), 1);
    assert_eq!(
        zero_only.fault_sites[0].possible_faults,
        [RuntimeFaultCode::IntegerDivisionByZero]
    );

    for operator in ["/", "MOD"] {
        let output = analyze_one(&format!(
            "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT;\nEND_VAR\nValue := Value {operator} DINT#0;\nEND_PROGRAM\n"
        ));
        assert_eq!(codes(&output), [DiagnosticCode::ConstantDivisionByZero]);
        assert!(output.model.is_none());
    }

    let folded_zero = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT;\nEND_VAR\nValue := Value / (DINT#1 XOR DINT#1);\nEND_PROGRAM\n",
    );
    assert_eq!(
        codes(&folded_zero),
        [DiagnosticCode::ConstantDivisionByZero]
    );
    assert!(folded_zero.model.is_none());
}

#[test]
fn constant_array_edges_create_no_site_and_dynamic_index_creates_one() {
    let output = analyze_one(
        "AURORA_ST VERSION 1.0;\nTYPE\n    Values : ARRAY[DINT#-1..DINT#1] OF DINT;\nEND_TYPE\nPROGRAM P\nVAR\n    Data : Values;\n    Index : DINT;\n    Value : DINT;\nEND_VAR\nValue := Data[DINT#-1];\nValue := Data[DINT#1];\nValue := Data[Index];\nEND_PROGRAM\n",
    );
    let model = model(output);
    assert_eq!(model.fault_sites.len(), 1);
    assert_eq!(
        model.fault_sites[0].id.operation,
        FaultOperationKind::ArrayIndex
    );
    assert_eq!(
        model.fault_sites[0].possible_faults,
        vec![RuntimeFaultCode::ArrayIndexOutOfBounds]
    );

    let outside = analyze_one(
        "AURORA_ST VERSION 1.0;\nTYPE\n    Values : ARRAY[DINT#-1..DINT#1] OF DINT;\nEND_TYPE\nPROGRAM P\nVAR\n    Data : Values;\n    Value : DINT;\nEND_VAR\nValue := Data[DINT#2];\nEND_PROGRAM\n",
    );
    assert_eq!(
        codes(&outside),
        vec![DiagnosticCode::InvalidExplicitConversion]
    );
    assert!(outside.model.is_none());
}

#[test]
fn provably_safe_widening_conversion_does_not_create_a_site() {
    let output = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT;\n    Small : USINT;\n    Wide : LINT;\nEND_VAR\nSmall := TO_USINT(Value);\nWide := TO_LINT(Value);\nEND_PROGRAM\n",
    );
    let model = model(output);
    assert_eq!(model.fault_sites.len(), 1);
    assert_eq!(
        model.fault_sites[0].id.operation,
        FaultOperationKind::NumericConversion
    );
    assert_eq!(
        model.fault_sites[0].possible_faults,
        vec![RuntimeFaultCode::InvalidRuntimeRange]
    );
}

#[test]
fn nested_array_indices_each_contribute_one_independent_site() {
    let output = analyze_one(
        "AURORA_ST VERSION 1.0;\nTYPE\n    Row : ARRAY[DINT#0..DINT#1] OF DINT;\n    Matrix : ARRAY[DINT#0..DINT#1] OF Row;\nEND_TYPE\nPROGRAM P\nVAR\n    Data : Matrix;\n    RowIndex : DINT;\n    ColumnIndex : DINT;\n    Value : DINT;\nEND_VAR\nValue := Data[RowIndex][ColumnIndex];\nEND_PROGRAM\n",
    );
    let model = model(output);
    assert_eq!(model.fault_sites.len(), 2);
    assert!(
        model
            .fault_sites
            .iter()
            .all(|site| site.id.operation == FaultOperationKind::ArrayIndex)
    );
    assert_ne!(model.fault_sites[0].id.span, model.fault_sites[1].id.span);
}

#[test]
fn structure_member_array_chain_is_resolved_before_index_generation() {
    let output = analyze_one(
        "AURORA_ST VERSION 1.0;\nTYPE\n    Row : STRUCT\n        Values : ARRAY[DINT#0..DINT#1] OF DINT;\n    END_STRUCT;\nEND_TYPE\nPROGRAM P\nVAR\n    Data : Row;\n    Index : DINT;\n    Value : DINT;\nEND_VAR\nValue := Data.Values[Index];\nEND_PROGRAM\n",
    );
    let model = model(output);
    assert_eq!(model.fault_sites.len(), 1);
    assert_eq!(
        model.fault_sites[0].id.operation,
        FaultOperationKind::ArrayIndex
    );

    let missing = analyze_one(
        "AURORA_ST VERSION 1.0;\nTYPE\n    Row : STRUCT\n        Value : DINT;\n    END_STRUCT;\nEND_TYPE\nPROGRAM P\nVAR\n    Data : Row;\n    Value : DINT;\nEND_VAR\nValue := Data.Missing;\nEND_PROGRAM\n",
    );
    assert_eq!(codes(&missing), vec![DiagnosticCode::TypeMismatch]);
    assert!(missing.model.is_none());
}

#[test]
fn selector_semantics_reject_undefined_and_non_integer_indices_without_cascades() {
    let undefined = analyze_one(
        "AURORA_ST VERSION 1.0;\nTYPE\n    Values : ARRAY[DINT#0..DINT#1] OF DINT;\nEND_TYPE\nPROGRAM P\nVAR\n    Data : Values;\n    Value : DINT;\nEND_VAR\nValue := Data[Missing];\nEND_PROGRAM\n",
    );
    assert_eq!(codes(&undefined), vec![DiagnosticCode::UndefinedSymbol]);
    assert!(undefined.model.is_none());

    let wrong_type = analyze_one(
        "AURORA_ST VERSION 1.0;\nTYPE\n    Values : ARRAY[DINT#0..DINT#1] OF DINT;\nEND_TYPE\nPROGRAM P\nVAR\n    Data : Values;\n    Value : DINT;\nEND_VAR\nValue := Data[BOOL#TRUE];\nEND_PROGRAM\n",
    );
    assert_eq!(codes(&wrong_type), vec![DiagnosticCode::TypeMismatch]);
    assert!(wrong_type.model.is_none());

    let untyped_string = analyze_one(
        "AURORA_ST VERSION 1.0;\nTYPE\n    Values : ARRAY[DINT#0..DINT#1] OF DINT;\nEND_TYPE\nPROGRAM P\nVAR\n    Data : Values;\n    Value : DINT;\nEND_VAR\nValue := Data['0'];\nEND_PROGRAM\n",
    );
    assert_eq!(codes(&untyped_string), vec![DiagnosticCode::TypeMismatch]);
    assert!(untyped_string.model.is_none());
}

#[test]
fn limit_site_is_omitted_when_constant_bounds_prove_the_range_valid() {
    let output = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT;\n    Low : DINT;\n    High : DINT;\nEND_VAR\nValue := LIMIT(Value, DINT#0, DINT#10);\nValue := LIMIT(Value, Low, High);\nEND_PROGRAM\n",
    );
    let model = model(output);
    assert_eq!(model.fault_sites.len(), 1);
    assert_eq!(
        model.fault_sites[0].id.operation,
        FaultOperationKind::LimitRange
    );
    assert_eq!(
        model.fault_sites[0].possible_faults,
        vec![RuntimeFaultCode::InvalidRuntimeRange]
    );

    let invalid = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT;\nEND_VAR\nValue := LIMIT(Value, DINT#10, DINT#0);\nEND_PROGRAM\n",
    );
    assert_eq!(
        codes(&invalid),
        vec![DiagnosticCode::InvalidExplicitConversion]
    );
    assert!(invalid.model.is_none());
}

#[test]
fn fault_site_ids_follow_path_bytes_independent_of_input_order() {
    let source_a = "AURORA_ST VERSION 1.0;\nPROGRAM Alpha\nVAR\n    Value : DINT;\nEND_VAR\nValue := CHECKED_ADD(Value, DINT#1);\nEND_PROGRAM\n";
    let source_b = "AURORA_ST VERSION 1.0;\nPROGRAM Beta\nVAR\n    Value : DINT;\nEND_VAR\nValue := CHECKED_ADD(Value, DINT#1);\nEND_PROGRAM\n";
    let ast_a = parsed("a/alpha.st", source_a);
    let ast_b = parsed("b/beta.st", source_b);
    let output = analyze_faults(
        &[
            SemanticSource::new(&ast_b, source_b),
            SemanticSource::new(&ast_a, source_a),
        ],
        fixed_limits(),
    )
    .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"));
    let model = model(output);
    assert_eq!(model.fault_sites.len(), 2);
    assert_eq!(model.fault_sites[0].id.source_path, "a/alpha.st");
    assert_eq!(model.fault_sites[1].id.source_path, "b/beta.st");
}

#[test]
fn float_and_string_operations_generate_only_their_dynamic_sites() {
    let output = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Left : REAL;\n    Right : REAL;\n    Text : STRING[4];\nEND_VAR\nLeft := Left / Right;\nText := CONCAT(Text, 'x');\nEND_PROGRAM\n",
    );
    let dynamic_model = model(output);
    assert_eq!(dynamic_model.fault_sites.len(), 2);
    assert_eq!(
        dynamic_model
            .fault_sites
            .iter()
            .map(|site| site.id.operation)
            .collect::<Vec<_>>(),
        vec![
            FaultOperationKind::FloatingPoint,
            FaultOperationKind::StringCapacity,
        ]
    );
    assert_eq!(
        dynamic_model.fault_sites[0].possible_faults,
        vec![RuntimeFaultCode::NonFiniteFloat]
    );
    assert_eq!(
        dynamic_model.fault_sites[1].possible_faults,
        vec![RuntimeFaultCode::StringCapacityExceeded]
    );

    let constant = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Text : STRING[4] := CONCAT('ab', 'cd');\nEND_VAR\nEND_PROGRAM\n",
    );
    let constant_model = model(constant);
    assert!(
        constant_model.fault_sites.is_empty(),
        "{:?}",
        constant_model.fault_sites
    );

    let too_long = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Text : STRING[4] := CONCAT('abc', 'de');\nEND_VAR\nEND_PROGRAM\n",
    );
    assert_eq!(
        codes(&too_long),
        vec![DiagnosticCode::InvalidExplicitConversion]
    );
    assert!(too_long.model.is_none());
}

#[test]
fn concatenating_an_empty_constant_does_not_create_a_capacity_site() {
    let output = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Text : STRING[4];\nEND_VAR\nText := CONCAT(Text, '');\nText := CONCAT('', Text);\nEND_PROGRAM\n",
    );
    let model = model(output);
    assert!(model.fault_sites.is_empty(), "{:?}", model.fault_sites);
}

#[test]
fn direct_string_constants_respect_the_typed_destination_capacity() {
    let exact = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Text : STRING[4];\n    Wide : WSTRING[2];\nEND_VAR\nText := 'test';\nWide := \"😀\";\nEND_PROGRAM\n",
    );
    assert!(model(exact).fault_sites.is_empty());

    let narrow = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Text : STRING[4];\nEND_VAR\nText := 'tests';\nEND_PROGRAM\n",
    );
    assert_eq!(codes(&narrow), [DiagnosticCode::InvalidExplicitConversion]);
    assert!(narrow.model.is_none());

    let wide = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Text : WSTRING[1];\nEND_VAR\nText := \"😀\";\nEND_PROGRAM\n",
    );
    assert_eq!(codes(&wide), [DiagnosticCode::InvalidExplicitConversion]);
    assert!(wide.model.is_none());
}

#[test]
fn constant_standard_operation_and_conversion_fail_once_without_sites() {
    let checked = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : DINT := CHECKED_ADD(DINT#2147483647, DINT#1);\nEND_VAR\nEND_PROGRAM\n",
    );
    assert_eq!(codes(&checked), vec![DiagnosticCode::ConstantOverflow]);
    assert!(checked.model.is_none());

    let conversion = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : LINT := TO_LINT(LREAL#9223372036854775808.0);\nEND_VAR\nEND_PROGRAM\n",
    );
    assert_eq!(
        codes(&conversion),
        vec![DiagnosticCode::InvalidExplicitConversion]
    );
    assert!(conversion.model.is_none());

    let negative_fraction = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : UINT := TO_UINT(LREAL#-0.5);\nEND_VAR\nEND_PROGRAM\n",
    );
    assert_eq!(
        codes(&negative_fraction),
        vec![DiagnosticCode::InvalidExplicitConversion]
    );
    assert!(negative_fraction.model.is_none());

    let finite = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : REAL := SQRT(4.0);\nEND_VAR\nEND_PROGRAM\n",
    );
    assert!(model(finite).fault_sites.is_empty());

    let domain = analyze_one(
        "AURORA_ST VERSION 1.0;\nPROGRAM P\nVAR\n    Value : REAL := SQRT(-1.0);\nEND_VAR\nEND_PROGRAM\n",
    );
    assert_eq!(codes(&domain), vec![DiagnosticCode::NonFiniteConstant]);
    assert!(domain.model.is_none());
}

#[test]
fn fixed_bounds_consume_the_same_constant_integer_policy() {
    let valid = analyze_one(
        "AURORA_ST VERSION 1.0;\nTYPE\n    Values : ARRAY[CHECKED_ADD(DINT#0, DINT#0)..CHECKED_ADD(DINT#0, DINT#1)] OF DINT;\nEND_TYPE\nPROGRAM P\nVAR\n    Data : Values;\nEND_VAR\nEND_PROGRAM\n",
    );
    assert!(model(valid).fault_sites.is_empty());

    let overflow = analyze_one(
        "AURORA_ST VERSION 1.0;\nTYPE\n    Values : ARRAY[DINT#0..CHECKED_ADD(DINT#2147483647, DINT#1)] OF DINT;\nEND_TYPE\n",
    );
    assert_eq!(codes(&overflow), vec![DiagnosticCode::ConstantOverflow]);
    assert!(overflow.model.is_none());
}
