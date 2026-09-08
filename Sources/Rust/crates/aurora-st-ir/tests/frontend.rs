//! End-to-end boundaries for the Aurora ST Preview 1.0 frontend.

use aurora_st_ir::{
    AstNode, AstSerializationError, DiagnosticCode, LimitConfigurationError, ParserLimits, parse,
    to_canonical_json,
};

const COMPLETE_SOURCE: &str = r"AURORA_ST VERSION 1.0;

TYPE
  Mode : (Idle := 0, Running := 1);
  Sample : STRUCT
    Value : DINT := DINT#-1;
    Window : ARRAY[0..3] OF UINT;
  END_STRUCT;
  Label : STRING[8];
END_TYPE

VAR_GLOBAL
  StartButton AT %IX0.0 : BOOL;
  SpeedCommand AT %QW2 : UINT := UINT#0;
END_VAR

FUNCTION Increment : DINT
VAR_INPUT
  Value : DINT;
END_VAR
RETURN CHECKED_ADD(Value, DINT#1);
END_FUNCTION

FUNCTION_BLOCK Latch
VAR_INPUT
  Set : BOOL;
END_VAR
VAR_OUTPUT
  State : BOOL;
END_VAR
IF Set THEN
  State := TRUE;
END_IF;
END_FUNCTION_BLOCK

PROGRAM Main
VAR
  Index : UINT := UINT#0;
  Instance : Latch;
  Result : BOOL;
END_VAR
FOR Index := UINT#0 TO UINT#3 BY UINT#1 DO
  Instance(Set := StartButton, State => Result);
END_FOR;
RETURN;
END_PROGRAM
";

fn generous_limits() -> ParserLimits {
    ParserLimits::new(64 * 1024, 8 * 1024, 8 * 1024, 256)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn diagnostic_codes(source: &[u8]) -> Vec<DiagnosticCode> {
    parse("program/main.st", source, generous_limits())
        .diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

fn count_nodes(node: &AstNode) -> usize {
    1 + node.children.iter().map(count_nodes).sum::<usize>()
}

#[test]
fn complete_preview_grammar_builds_one_source_positioned_ast() {
    let output = parse(
        "program/main.st",
        COMPLETE_SOURCE.as_bytes(),
        generous_limits(),
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let Some(ast) = output.ast else {
        unreachable!("a diagnostic-free parse must publish its AST")
    };
    assert_eq!(ast.root.span.start, 0);
    assert_eq!(ast.root.span.end as usize, COMPLETE_SOURCE.len());
    assert_eq!(ast.root.children.len(), 6);
}

#[test]
fn lexical_boundaries_accept_exact_addresses_and_typed_literals_only() {
    let valid = b"AURORA_ST VERSION 1.0; VAR_GLOBAL X AT %ix0.0 : DINT := DINT#-1; END_VAR";
    assert!(diagnostic_codes(valid).is_empty());

    let invalid_address = b"AURORA_ST VERSION 1.0; VAR_GLOBAL X AT %IX00.0 : BOOL; END_VAR";
    assert_eq!(
        diagnostic_codes(invalid_address),
        vec![DiagnosticCode::InvalidDirectAddress]
    );

    let vendor_address = b"AURORA_ST VERSION 1.0; VAR_GLOBAL X AT DB1.DBW0 : UINT; END_VAR";
    assert_eq!(
        diagnostic_codes(vendor_address),
        vec![DiagnosticCode::VendorAddressInSource]
    );
}

#[test]
fn recovery_reports_independent_roots_without_publishing_partial_ast() {
    let source = b"AURORA_ST VERSION 1.0;
PROGRAM Broken
VAR
  A : DINT
END_VAR
A := ;
RETURN;
END_PROGRAM
PROGRAM Valid
RETURN;
END_PROGRAM
";
    let output = parse("broken.st", source, generous_limits());
    assert!(output.ast.is_none());
    assert_eq!(
        output
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>(),
        vec![
            DiagnosticCode::MissingTerminator,
            DiagnosticCode::UnexpectedToken
        ]
    );
    assert!(output.diagnostics[0].span.start < output.diagnostics[1].span.start);
}

#[test]
fn recovery_keeps_outer_block_terminators_and_later_declarations() {
    let missing_nested_end = b"AURORA_ST VERSION 1.0;
PROGRAM First
IF TRUE THEN
  RETURN;
END_PROGRAM
PROGRAM Second
RETURN;
END_PROGRAM
";
    assert_eq!(
        diagnostic_codes(missing_nested_end),
        vec![DiagnosticCode::MissingTerminator]
    );

    let missing_variable_end = b"AURORA_ST VERSION 1.0;
PROGRAM P
VAR
  A : DINT;
A := DINT#1;
RETURN;
END_PROGRAM
";
    assert_eq!(
        diagnostic_codes(missing_variable_end),
        vec![DiagnosticCode::MissingTerminator]
    );
}

#[test]
fn only_unambiguous_missing_semicolons_use_missing_terminator() {
    let missing = b"AURORA_ST VERSION 1.0; PROGRAM P RETURN DINT#1 END_PROGRAM";
    assert_eq!(
        diagnostic_codes(missing),
        vec![DiagnosticCode::MissingTerminator]
    );

    let comparison_chain =
        b"AURORA_ST VERSION 1.0; PROGRAM P RETURN DINT#1 < DINT#2 < DINT#3; END_PROGRAM";
    assert_eq!(
        diagnostic_codes(comparison_chain),
        vec![DiagnosticCode::UnexpectedToken]
    );

    let extra_literal = b"AURORA_ST VERSION 1.0; PROGRAM P RETURN DINT#1 DINT#2; END_PROGRAM";
    assert_eq!(
        diagnostic_codes(extra_literal),
        vec![DiagnosticCode::UnexpectedToken]
    );
}

#[test]
fn unsupported_construct_has_one_primary_diagnostic() {
    let source = b"AURORA_ST VERSION 1.0; PROGRAM P WHILE TRUE DO END_WHILE; END_PROGRAM";
    assert_eq!(
        diagnostic_codes(source),
        vec![DiagnosticCode::UnsupportedConstruct]
    );
}

#[test]
fn empty_required_blocks_and_non_vendor_address_typos_are_not_accepted() {
    let empty_type = b"AURORA_ST VERSION 1.0; TYPE END_TYPE";
    assert_eq!(
        diagnostic_codes(empty_type),
        vec![DiagnosticCode::UnexpectedToken]
    );

    let empty_variables = b"AURORA_ST VERSION 1.0; PROGRAM P VAR END_VAR RETURN; END_PROGRAM";
    assert_eq!(
        diagnostic_codes(empty_variables),
        vec![DiagnosticCode::UnexpectedToken]
    );

    let missing_logical_syntax =
        b"AURORA_ST VERSION 1.0; VAR_GLOBAL X AT LogicalName : UINT; END_VAR";
    assert_eq!(
        diagnostic_codes(missing_logical_syntax),
        vec![DiagnosticCode::UnexpectedToken]
    );
}

#[test]
fn grammar_rejects_non_decimal_capacities_and_signs_on_non_numeric_qualified_values() {
    let zero_capacity = b"AURORA_ST VERSION 1.0; TYPE T : STRING[0]; END_TYPE";
    assert_eq!(
        diagnostic_codes(zero_capacity),
        vec![DiagnosticCode::UnexpectedToken]
    );
    let hexadecimal_capacity = b"AURORA_ST VERSION 1.0; TYPE T : STRING[16#10]; END_TYPE";
    assert_eq!(
        diagnostic_codes(hexadecimal_capacity),
        vec![DiagnosticCode::UnexpectedToken]
    );
    let signed_boolean = b"AURORA_ST VERSION 1.0; PROGRAM P RETURN BOOL#-TRUE; END_PROGRAM";
    assert_eq!(
        diagnostic_codes(signed_boolean),
        vec![DiagnosticCode::UnexpectedToken]
    );
    let signed_enum = b"AURORA_ST VERSION 1.0; PROGRAM P RETURN State#-Running; END_PROGRAM";
    assert_eq!(
        diagnostic_codes(signed_enum),
        vec![DiagnosticCode::UnexpectedToken]
    );
}

#[test]
fn utf8_bom_and_leading_comment_are_rejected_at_the_version_boundary() {
    assert_eq!(
        diagnostic_codes(b"\xef\xbb\xbfAURORA_ST VERSION 1.0;"),
        vec![DiagnosticCode::InvalidEncoding]
    );
    assert_eq!(
        diagnostic_codes(b"// hidden version\nAURORA_ST VERSION 1.0;"),
        vec![DiagnosticCode::UnsupportedLanguageVersion]
    );
    assert_eq!(
        diagnostic_codes(&[0xff, 0xfe]),
        vec![DiagnosticCode::InvalidEncoding]
    );
}

#[test]
fn lexical_root_errors_do_not_cascade_and_positions_use_byte_and_scalar_units() {
    let unicode = "AURORA_ST VERSION 1.0;\nPROGRAM P\n变量 := 1;\nEND_PROGRAM\n";
    let output = parse("unicode.st", unicode.as_bytes(), generous_limits());
    assert_eq!(output.diagnostics.len(), 1);
    let diagnostic = &output.diagnostics[0];
    assert_eq!(diagnostic.code, DiagnosticCode::InvalidToken);
    assert_eq!(diagnostic.span.end - diagnostic.span.start, 6);
    assert_eq!((diagnostic.start.line, diagnostic.start.column), (3, 1));

    let bad_escape = b"AURORA_ST VERSION 1.0; PROGRAM P RETURN '\\q'; END_PROGRAM";
    assert_eq!(
        diagnostic_codes(bad_escape),
        vec![DiagnosticCode::InvalidEscape]
    );
    let two_bad_escapes = b"AURORA_ST VERSION 1.0; PROGRAM P RETURN '\\q\\z'; END_PROGRAM";
    assert_eq!(
        diagnostic_codes(two_bad_escapes),
        vec![DiagnosticCode::InvalidEscape, DiagnosticCode::InvalidEscape]
    );
    let bad_number = b"AURORA_ST VERSION 1.0; PROGRAM P RETURN 01; END_PROGRAM";
    assert_eq!(
        diagnostic_codes(bad_number),
        vec![DiagnosticCode::InvalidToken]
    );
    let unterminated_comment = b"AURORA_ST VERSION 1.0; (* never closed";
    assert_eq!(
        diagnostic_codes(unterminated_comment),
        vec![DiagnosticCode::UnterminatedComment]
    );
    let nested_comment = b"AURORA_ST VERSION 1.0; (* outer (* nested *) outer *)";
    assert_eq!(
        diagnostic_codes(nested_comment),
        vec![DiagnosticCode::InvalidToken]
    );
}

#[test]
fn zero_and_unrepresentable_limit_configurations_are_rejected_before_parsing() {
    assert_eq!(
        ParserLimits::new(0, 1, 1, 1),
        Err(LimitConfigurationError::ZeroSourceBytes)
    );
    assert_eq!(
        ParserLimits::new(1, 0, 1, 1),
        Err(LimitConfigurationError::ZeroTokens)
    );
    assert_eq!(
        ParserLimits::new(1, 1, 0, 1),
        Err(LimitConfigurationError::ZeroAstNodes)
    );
    assert_eq!(
        ParserLimits::new(1, 1, 1, 0),
        Err(LimitConfigurationError::ZeroNestingDepth)
    );
    if usize::BITS > u32::BITS {
        assert_eq!(
            ParserLimits::new(u32::MAX as usize + 1, 1, 1, 1),
            Err(LimitConfigurationError::SourceSpanOverflow)
        );
    }
}

#[test]
fn every_resource_limit_accepts_the_exact_boundary_and_rejects_one_less() {
    let source = b"AURORA_ST VERSION 1.0; PROGRAM P RETURN; END_PROGRAM";
    let baseline = parse("limits.st", source, generous_limits());
    let Some(ast) = baseline.ast else {
        unreachable!("baseline source is valid")
    };
    let nodes = count_nodes(&ast.root);

    let exact_source = ParserLimits::new(source.len(), 128, 128, 128)
        .unwrap_or_else(|error| unreachable!("valid limits: {error}"));
    assert!(parse("limits.st", source, exact_source).ast.is_some());
    let short_source = ParserLimits::new(source.len() - 1, 128, 128, 128)
        .unwrap_or_else(|error| unreachable!("valid limits: {error}"));
    assert_eq!(
        parse("limits.st", source, short_source).diagnostics[0].code,
        DiagnosticCode::SourceLimitExceeded
    );

    let exact_nodes = ParserLimits::new(source.len(), 128, nodes, 128)
        .unwrap_or_else(|error| unreachable!("valid limits: {error}"));
    assert!(parse("limits.st", source, exact_nodes).ast.is_some());
    let short_nodes = ParserLimits::new(source.len(), 128, nodes - 1, 128)
        .unwrap_or_else(|error| unreachable!("valid limits: {error}"));
    assert_eq!(
        parse("limits.st", source, short_nodes).diagnostics[0].code,
        DiagnosticCode::SourceLimitExceeded
    );

    let minimum_tokens = (1..128)
        .find(|tokens| {
            let limits = ParserLimits::new(source.len(), *tokens, 128, 128)
                .unwrap_or_else(|error| unreachable!("valid limits: {error}"));
            parse("limits.st", source, limits).ast.is_some()
        })
        .unwrap_or_else(|| unreachable!("token boundary must be found"));
    let exact_tokens = ParserLimits::new(source.len(), minimum_tokens, 128, 128)
        .unwrap_or_else(|error| unreachable!("valid limits: {error}"));
    assert!(parse("limits.st", source, exact_tokens).ast.is_some());
    let short_tokens = ParserLimits::new(source.len(), minimum_tokens - 1, 128, 128)
        .unwrap_or_else(|error| unreachable!("valid limits: {error}"));
    assert_eq!(
        parse("limits.st", source, short_tokens).diagnostics[0].code,
        DiagnosticCode::SourceLimitExceeded
    );

    let minimum_depth = (1..128)
        .find(|depth| {
            let limits = ParserLimits::new(source.len(), 128, 128, *depth)
                .unwrap_or_else(|error| unreachable!("valid limits: {error}"));
            parse("limits.st", source, limits).ast.is_some()
        })
        .unwrap_or_else(|| unreachable!("depth boundary must be found"));
    let short_depth = ParserLimits::new(source.len(), 128, 128, minimum_depth - 1)
        .unwrap_or_else(|error| unreachable!("valid limits: {error}"));
    assert_eq!(
        parse("limits.st", source, short_depth).diagnostics[0].code,
        DiagnosticCode::SourceLimitExceeded
    );
}

#[test]
fn canonical_ast_has_a_stable_golden_and_writer_rejects_unknown_versions() {
    let source = b"AURORA_ST VERSION 1.0;";
    let output = parse("main.st", source, generous_limits());
    let Some(ast) = output.ast else {
        unreachable!("golden source is valid")
    };
    let bytes = to_canonical_json(&ast)
        .unwrap_or_else(|error| unreachable!("golden AST serializes: {error}"));
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        r#"{"root":{"children":[{"children":[],"kind":"version_directive","span":{"end":22,"start":0},"text":"1.0"}],"kind":"compilation_unit","span":{"end":22,"start":0}},"schema_version":{"major":1,"minor":0},"source_path":"main.st"}"#
    );
    let mut unknown = ast;
    unknown.schema_version.minor = 1;
    assert!(matches!(
        to_canonical_json(&unknown),
        Err(AstSerializationError::UnsupportedVersion { major: 1, minor: 1 })
    ));
}
