//! Project-wide name and scalar-type boundaries for Aurora ST Preview 1.0.

use std::collections::BTreeSet;

use aurora_st_ir::{
    AnalysisInputError, AnalysisOutput, DiagnosticCode, ParserLimits, SemanticSource,
    SemanticSymbolKind, SemanticType, VersionedAst, analyze, diagnostics_to_canonical_json, parse,
};

fn generous_limits() -> ParserLimits {
    ParserLimits::new(64 * 1024, 8 * 1024, 8 * 1024, 256)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn parsed(path: &str, source: &str) -> VersionedAst {
    let output = parse(path, source.as_bytes(), generous_limits());
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    output
        .ast
        .unwrap_or_else(|| unreachable!("diagnostic-free parsing publishes an AST"))
}

fn analyze_one(source: &str) -> AnalysisOutput {
    let ast = parsed("program/main.st", source);
    analyze(&[SemanticSource::new(&ast, source)])
        .unwrap_or_else(|error| unreachable!("parser-produced inputs are valid: {error}"))
}

fn codes(output: &AnalysisOutput) -> Vec<DiagnosticCode> {
    output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn successful_project_has_stable_scope_bindings_and_no_duplicate_entries() {
    let declarations = r"AURORA_ST VERSION 1.0;
TYPE
  Count : DINT;
  Mode : (Idle := 0, Running := 1);
  Label : STRING[8];
END_TYPE
FUNCTION Increment : Count
VAR_INPUT
  Value : Count;
END_VAR
RETURN CHECKED_ADD(Value, Count#1);
END_FUNCTION
FUNCTION_BLOCK Latch
VAR_INPUT
  Set : BOOL;
END_VAR
VAR_OUTPUT
  State : BOOL;
END_VAR
State := Set;
END_FUNCTION_BLOCK
";
    let program = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Command AT %MW0 : DINT := DINT#0;
END_VAR
PROGRAM Main
VAR
  Instance : Latch;
  Result, Copy : BOOL := FALSE;
  Next : DINT;
  CurrentMode : Mode := Mode#Idle;
END_VAR
Next := Increment(Command);
Instance(Set := TRUE, State => Result);
Copy := Result;
RETURN;
END_PROGRAM
";
    let declaration_ast = parsed("a/declarations.st", declarations);
    let program_ast = parsed("z/program.st", program);
    let output = analyze(&[
        SemanticSource::new(&program_ast, program),
        SemanticSource::new(&declaration_ast, declarations),
    ])
    .unwrap_or_else(|error| unreachable!("parser-produced inputs are valid: {error}"));
    let forward = analyze(&[
        SemanticSource::new(&declaration_ast, declarations),
        SemanticSource::new(&program_ast, program),
    ])
    .unwrap_or_else(|error| unreachable!("parser-produced inputs are valid: {error}"));

    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert_eq!(output, forward);
    let model = output
        .model
        .unwrap_or_else(|| unreachable!("diagnostic-free analysis publishes a model"));
    assert_eq!(model.symbols[0].name, "Count");
    assert_eq!(model.symbols[0].source_path, "a/declarations.st");
    assert!(model.symbols.iter().any(|symbol| {
        symbol.kind == SemanticSymbolKind::EnumerationMember && symbol.name == "Idle"
    }));
    assert!(model.symbols.iter().any(|symbol| {
        symbol.name == "Instance"
            && matches!(
                symbol.declared_type,
                Some(SemanticType::FunctionBlock { .. })
            )
    }));
    assert!(model.symbols.iter().any(|symbol| {
        symbol.name == "Label"
            && symbol.declared_type
                == Some(SemanticType::String {
                    capacity: "8".to_owned(),
                })
    }));
    let reference_keys = model
        .references
        .iter()
        .map(|reference| {
            (
                reference.source_path.as_str(),
                reference.span,
                reference.symbol,
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(reference_keys.len(), model.references.len());
    let expression_keys = model
        .expressions
        .iter()
        .map(|expression| (expression.source_path.as_str(), expression.span))
        .collect::<BTreeSet<_>>();
    assert_eq!(expression_keys.len(), model.expressions.len());
}

#[test]
fn duplicate_reserved_and_undefined_names_have_exact_cardinality() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Shared AT %MW0 : DINT;
  shared AT %MW4 : DINT;
END_VAR
PROGRAM __aurora_internal
RETURN;
END_PROGRAM
PROGRAM Main
VAR
  Shared : DINT;
END_VAR
Shared := Missing;
RETURN;
END_PROGRAM
";
    let output = analyze_one(source);
    assert!(output.model.is_none());
    assert_eq!(
        codes(&output),
        vec![
            DiagnosticCode::DuplicateSymbol,
            DiagnosticCode::ReservedIdentifier,
            DiagnosticCode::DuplicateSymbol,
            DiagnosticCode::UndefinedSymbol,
        ]
    );
}

#[test]
fn scalar_assignment_and_conversion_diagnostics_are_not_cascaded() {
    let source = r"AURORA_ST VERSION 1.0;
PROGRAM Main
VAR
  UnsignedValue : UINT;
  SignedValue : DINT;
  Flag : BOOL;
END_VAR
UnsignedValue := DINT#1;
SignedValue := UINT#1;
Flag := DINT#1;
UnsignedValue := -1;
UnsignedValue := TO_USINT(DINT#-1);
RETURN;
END_PROGRAM
";
    let output = analyze_one(source);
    assert_eq!(
        codes(&output),
        vec![
            DiagnosticCode::LossyImplicitConversion,
            DiagnosticCode::TypeMismatch,
            DiagnosticCode::InvalidExplicitConversion,
            DiagnosticCode::InvalidExplicitConversion,
        ]
    );
}

#[test]
fn numeric_common_type_uses_the_narrowest_lossless_width() {
    let accepted = r"AURORA_ST VERSION 1.0;
PROGRAM Main
VAR
  Result : DINT;
END_VAR
Result := SINT#1 + UINT#1;
RETURN;
END_PROGRAM
";
    assert!(analyze_one(accepted).diagnostics.is_empty());

    let rejected = r"AURORA_ST VERSION 1.0;
PROGRAM Main
VAR
  Result : LINT;
END_VAR
Result := DINT#1 + ULINT#1;
RETURN;
END_PROGRAM
";
    assert_eq!(
        codes(&analyze_one(rejected)),
        vec![DiagnosticCode::LossyImplicitConversion]
    );
}

#[test]
fn standard_overloads_use_common_types_and_reject_ambiguity_once() {
    let accepted = r"AURORA_ST VERSION 1.0;
PROGRAM Main
VAR
  Result : DINT;
END_VAR
Result := CHECKED_ADD(SINT#1, UINT#1);
RETURN;
END_PROGRAM
";
    assert!(analyze_one(accepted).diagnostics.is_empty());

    let rejected = r"AURORA_ST VERSION 1.0;
PROGRAM Main
VAR
  Result : DINT;
END_VAR
IF MIN(1, 2) = 1 THEN
  RETURN;
END_IF;
Result := MIN(DINT#1, ULINT#1);
Result := SQRT(DINT#1);
RETURN;
END_PROGRAM
";
    assert_eq!(
        codes(&analyze_one(rejected)),
        vec![DiagnosticCode::InvalidCall; 3]
    );
}

#[test]
fn pou_access_return_and_recursion_diagnostics_are_exact() {
    let source = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Shared AT %MW0 : DINT;
END_VAR
FUNCTION ReadGlobal : DINT
RETURN Shared;
END_FUNCTION
FUNCTION First : DINT
VAR_INPUT
  Value : DINT;
END_VAR
RETURN Second(Value);
END_FUNCTION
FUNCTION Second : DINT
VAR_INPUT
  Value : DINT;
END_VAR
RETURN First(Value);
END_FUNCTION
FUNCTION MissingReturn : DINT
VAR_INPUT
  Value : DINT;
END_VAR
IF TRUE THEN
  RETURN Value;
END_IF;
END_FUNCTION
PROGRAM Main
RETURN DINT#1;
END_PROGRAM
";
    let output = analyze_one(source);
    assert_eq!(
        codes(&output),
        vec![
            DiagnosticCode::InvalidPouAccess,
            DiagnosticCode::RecursiveCall,
            DiagnosticCode::RecursiveCall,
            DiagnosticCode::InvalidPouAccess,
            DiagnosticCode::InvalidPouAccess,
        ]
    );
}

#[test]
fn an_unconditional_early_return_satisfies_the_function_contract() {
    let source = r"AURORA_ST VERSION 1.0;
FUNCTION Early : DINT
RETURN DINT#1;
RETURN DINT#2;
END_FUNCTION
";
    assert!(analyze_one(source).diagnostics.is_empty());
}

#[test]
fn unknown_enum_member_reports_only_its_reference() {
    let source = r"AURORA_ST VERSION 1.0;
TYPE
  Mode : (Idle := 0, Running := 1);
END_TYPE
PROGRAM Main
VAR
  Current : Mode;
END_VAR
Current := Mode#Missing;
RETURN;
END_PROGRAM
";
    let output = analyze_one(source);
    assert_eq!(codes(&output), vec![DiagnosticCode::UndefinedSymbol]);
    assert_eq!(
        &source[output.diagnostics[0].span.start as usize..output.diagnostics[0].span.end as usize],
        "Missing"
    );
}

#[test]
fn invalid_function_and_function_block_calls_emit_once_per_call_site() {
    let source = r"AURORA_ST VERSION 1.0;
FUNCTION Identity : DINT
VAR_INPUT
  Value : DINT;
END_VAR
RETURN Value;
END_FUNCTION
FUNCTION_BLOCK Pair
VAR_INPUT
  Set : BOOL;
END_VAR
VAR_OUTPUT
  First, Second : BOOL;
END_VAR
First := Set;
Second := Set;
END_FUNCTION_BLOCK
PROGRAM Main
VAR
  Instance : Pair;
  Result : BOOL;
  Number : DINT;
END_VAR
Number := Identity(1, 2);
Instance(Unknown := TRUE);
Instance(Set := TRUE, First => Result, Second => Result);
RETURN;
END_PROGRAM
";
    let output = analyze_one(source);
    assert_eq!(
        codes(&output),
        vec![
            DiagnosticCode::InvalidCall,
            DiagnosticCode::InvalidCall,
            DiagnosticCode::InvalidCall,
        ]
    );
}

#[test]
fn an_invalid_recursive_call_is_not_also_added_to_the_call_graph() {
    let source = r"AURORA_ST VERSION 1.0;
FUNCTION Bad : DINT
VAR_INPUT
  Value : DINT;
END_VAR
RETURN Bad();
END_FUNCTION
";
    assert_eq!(
        codes(&analyze_one(source)),
        vec![DiagnosticCode::InvalidCall]
    );
}

#[test]
fn nested_type_references_and_non_writable_instances_keep_pass_boundaries() {
    let undefined_nested_type = r"AURORA_ST VERSION 1.0;
TYPE
  Container : STRUCT
    Value : MissingType;
  END_STRUCT;
END_TYPE
";
    assert_eq!(
        codes(&analyze_one(undefined_nested_type)),
        vec![DiagnosticCode::UndefinedSymbol]
    );

    let invalid_targets = r"AURORA_ST VERSION 1.0;
FUNCTION_BLOCK Latch
END_FUNCTION_BLOCK
PROGRAM Main
VAR
  Instance : Latch;
  Value : DINT;
END_VAR
Instance := Instance;
Value.Member := 1;
RETURN;
END_PROGRAM
";
    assert_eq!(
        codes(&analyze_one(invalid_targets)),
        vec![
            DiagnosticCode::InvalidAssignmentTarget,
            DiagnosticCode::TypeMismatch,
        ]
    );
}

#[test]
fn diagnostics_use_canonical_sorted_json() {
    let first_source = "AURORA_ST VERSION 1.0; PROGRAM B Missing := TRUE; END_PROGRAM";
    let second_source = "AURORA_ST VERSION 1.0; PROGRAM A Other := TRUE; END_PROGRAM";
    let first_ast = parsed("z.st", first_source);
    let second_ast = parsed("a.st", second_source);
    let output = analyze(&[
        SemanticSource::new(&first_ast, first_source),
        SemanticSource::new(&second_ast, second_source),
    ])
    .unwrap_or_else(|error| unreachable!("parser-produced inputs are valid: {error}"));
    let json = diagnostics_to_canonical_json(&output.diagnostics)
        .unwrap_or_else(|error| unreachable!("diagnostics are serializable: {error}"));
    let text = String::from_utf8(json)
        .unwrap_or_else(|error| unreachable!("canonical JSON is UTF-8: {error}"));
    assert_eq!(
        text,
        r#"[{"code":"ST1002","end":{"byte_offset":38,"column":39,"line":1},"source_path":"a.st","span":{"end":38,"start":33},"start":{"byte_offset":33,"column":34,"line":1}},{"code":"ST1002","end":{"byte_offset":40,"column":41,"line":1},"source_path":"z.st","span":{"end":40,"start":33},"start":{"byte_offset":33,"column":34,"line":1}}]"#
    );
    assert_eq!(codes(&output), vec![DiagnosticCode::UndefinedSymbol; 2]);
}

#[test]
fn corrupt_or_duplicate_inputs_are_rejected_before_analysis() {
    let source = "AURORA_ST VERSION 1.0; PROGRAM Main RETURN; END_PROGRAM";
    let ast = parsed("main.st", source);
    assert!(matches!(
        analyze(&[
            SemanticSource::new(&ast, source),
            SemanticSource::new(&ast, source),
        ]),
        Err(AnalysisInputError::DuplicateSourcePath(path)) if path == "main.st"
    ));

    let mut corrupt = ast.clone();
    corrupt.root.span.end = u32::MAX;
    assert!(matches!(
        analyze(&[SemanticSource::new(&corrupt, source)]),
        Err(AnalysisInputError::InvalidSourceSpan { .. })
    ));
    let extended_source = format!("{source} ");
    assert!(matches!(
        analyze(&[SemanticSource::new(&ast, &extended_source)]),
        Err(AnalysisInputError::InvalidSourceSpan { .. })
    ));
}
