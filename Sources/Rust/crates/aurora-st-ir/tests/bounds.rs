//! R1-06 step 1: static loop and per-task work boundary tests.

use aurora_st_ir::{
    AddressBindingInputs, AddressBindingLimits, AddressSemanticModel, CyclicWorkAnalysisOutput,
    CyclicWorkInputError, CyclicWorkLimits, DiagnosticCode, ExternalField, FixedDataLimits,
    ParserLimits, ProgramTaskBinding, SemanticSource, SourceSpan, TagCatalogEntry, TaskHandle,
    VersionedAst, analyze_addresses, analyze_cyclic_work, analyze_faults, parse,
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

fn work_limits(max_loops: usize, max_iterations: u64, max_operations: u64) -> CyclicWorkLimits {
    CyclicWorkLimits::new(max_loops, 16, max_iterations, max_operations)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn parsed(source: &str) -> VersionedAst {
    let output = parse("program/main.st", source.as_bytes(), parser_limits());
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    output
        .ast
        .unwrap_or_else(|| unreachable!("diagnostic-free parse publishes an AST"))
}

fn analyze(source: &str, limits: CyclicWorkLimits) -> CyclicWorkAnalysisOutput {
    let (ast, address_model) = accepted_model(source, &[7]);
    analyze_cyclic_work(
        &[SemanticSource::new(&ast, source)],
        &address_model,
        fixed_limits(),
        limits,
    )
    .unwrap_or_else(|error| unreachable!("accepted upstream model has valid shape: {error}"))
}

fn accepted_model(source: &str, task_handles: &[u32]) -> (VersionedAst, AddressSemanticModel) {
    let ast = parsed(source);
    let semantic_source = SemanticSource::new(&ast, source);
    let catalog = [TagCatalogEntry {
        source_path: "project/tags.json",
        source: EXTERNAL_SOURCE,
        span: SourceSpan { start: 0, end: 1 },
        symbol: ExternalField {
            value: "counter",
            span: SourceSpan { start: 0, end: 1 },
        },
        tag_id: ExternalField {
            value: TAG_ID,
            span: SourceSpan { start: 0, end: 1 },
        },
    }];
    let fault_output = analyze_faults(&[semantic_source], fixed_limits())
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
    .unwrap_or_else(|error| unreachable!("parser-produced input is valid: {error}"));
    let address_model = address_output.model.unwrap_or_else(|| {
        unreachable!("test source passes R1-05: {:?}", address_output.diagnostics)
    });
    (ast, address_model)
}

fn valid_loop_source() -> &'static str {
    r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
PROGRAM Main
VAR
  Index : DINT;
END_VAR
FOR Index := DINT#0 TO DINT#2 BY DINT#1 DO
  Counter := CHECKED_ADD(Counter, DINT#1);
END_FOR;
END_PROGRAM
"
}

fn codes(output: &CyclicWorkAnalysisOutput) -> Vec<DiagnosticCode> {
    output
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn one_static_for_produces_exactly_one_proof_at_both_inclusive_edges() {
    let output = analyze(valid_loop_source(), work_limits(1, 3, 128));
    assert!(output.diagnostics.is_empty());
    let model = output
        .model
        .unwrap_or_else(|| unreachable!("successful analysis publishes a proof"));
    assert_eq!(model.loops.len(), 1);
    assert_eq!(model.loops[0].initial, 0);
    assert_eq!(model.loops[0].end, 2);
    assert_eq!(model.loops[0].step, 1);
    assert_eq!(model.loops[0].iterations, 3);
    assert_eq!(model.tasks.len(), 1);
    assert_eq!(model.tasks[0].task, TaskHandle(7));
}

#[test]
fn loop_limit_accepts_exact_capacity_and_rejects_one_over_without_partial_model() {
    assert!(
        analyze(valid_loop_source(), work_limits(1, 3, 128))
            .diagnostics
            .is_empty()
    );
    let rejected = analyze(valid_loop_source(), work_limits(1, 2, 128));
    assert_eq!(codes(&rejected), [DiagnosticCode::LoopLimitExceeded]);
    assert!(rejected.model.is_none());
}

#[test]
fn descending_and_zero_iteration_loops_use_signed_inclusive_rules() {
    let descending =
        valid_loop_source().replace("DINT#0 TO DINT#2 BY DINT#1", "DINT#2 TO DINT#0 BY DINT#-1");
    let output = analyze(&descending, work_limits(1, 3, 128));
    assert!(output.diagnostics.is_empty());
    assert_eq!(
        output
            .model
            .as_ref()
            .and_then(|model| model.loops.first())
            .map(|loop_proof| loop_proof.iterations),
        Some(3)
    );

    let empty =
        valid_loop_source().replace("DINT#0 TO DINT#2 BY DINT#1", "DINT#0 TO DINT#2 BY DINT#-1");
    let output = analyze(&empty, work_limits(1, 1, 128));
    assert!(output.diagnostics.is_empty());
    assert_eq!(
        output
            .model
            .as_ref()
            .and_then(|model| model.loops.first())
            .map(|loop_proof| loop_proof.iterations),
        Some(0)
    );
}

#[test]
fn dynamic_bound_and_zero_step_each_emit_one_primary_diagnostic() {
    let dynamic = valid_loop_source().replace("DINT#2 BY", "Counter BY");
    let output = analyze(&dynamic, work_limits(1, 8, 128));
    assert_eq!(codes(&output), [DiagnosticCode::UnboundedLoop]);
    assert!(output.model.is_none());

    let zero = valid_loop_source().replace("BY DINT#1", "BY DINT#0");
    let output = analyze(&zero, work_limits(1, 8, 128));
    assert_eq!(codes(&output), [DiagnosticCode::InvalidForStep]);
    assert!(output.model.is_none());
}

#[test]
fn fixed_width_wrapping_constant_uses_declared_type_before_counting() {
    let source = valid_loop_source().replace(
        "DINT#0 TO DINT#2 BY DINT#1",
        "WRAPPING_ADD(DINT#2147483647, DINT#1) TO DINT#-2147483648 BY DINT#1",
    );
    let output = analyze(&source, work_limits(1, 1, 128));
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let loop_proof = output
        .model
        .as_ref()
        .and_then(|model| model.loops.first())
        .unwrap_or_else(|| unreachable!("valid loop has one proof"));
    assert_eq!(loop_proof.initial, i128::from(i32::MIN));
    assert_eq!(loop_proof.end, i128::from(i32::MIN));
    assert_eq!(loop_proof.iterations, 1);
}

#[test]
fn loop_record_capacity_does_not_drop_or_truncate_nested_loops() {
    let nested = valid_loop_source().replace(
        "  Index : DINT;",
        "  Index : DINT;\n  Inner : DINT;",
    ).replace(
        "  Counter := CHECKED_ADD(Counter, DINT#1);",
        "  FOR Inner := DINT#0 TO DINT#0 DO\n    Counter := CHECKED_ADD(Counter, DINT#1);\n  END_FOR;",
    );
    let accepted = analyze(&nested, work_limits(2, 3, 256));
    assert!(accepted.diagnostics.is_empty());
    assert_eq!(accepted.model.map(|model| model.loops.len()), Some(2));

    let rejected = analyze(&nested, work_limits(1, 3, 256));
    assert_eq!(codes(&rejected), [DiagnosticCode::ResourceBudgetExceeded]);
    assert!(rejected.model.is_none());
}

#[test]
fn nested_for_cannot_implicitly_write_an_active_control_variable() {
    let nested = valid_loop_source().replace(
        "  Counter := CHECKED_ADD(Counter, DINT#1);",
        "  FOR Index := DINT#0 TO DINT#0 DO\n    Counter := CHECKED_ADD(Counter, DINT#1);\n  END_FOR;",
    );
    let output = analyze(&nested, work_limits(2, 3, 256));
    assert_eq!(codes(&output), [DiagnosticCode::InvalidAssignmentTarget]);
    assert!(output.model.is_none());
}

#[test]
fn task_budget_accepts_exact_computed_bound_and_rejects_one_less() {
    let baseline = analyze(valid_loop_source(), work_limits(1, 3, u64::MAX));
    let operations = baseline
        .model
        .as_ref()
        .and_then(|model| model.tasks.first())
        .map_or_else(
            || unreachable!("successful analysis publishes one task"),
            |task| task.source_operations,
        );
    assert!(
        analyze(valid_loop_source(), work_limits(1, 3, operations))
            .diagnostics
            .is_empty()
    );
    let rejected = analyze(
        valid_loop_source(),
        work_limits(1, 3, operations.saturating_sub(1)),
    );
    assert_eq!(codes(&rejected), [DiagnosticCode::ResourceBudgetExceeded]);
    assert!(rejected.model.is_none());
}

#[test]
fn every_zero_limit_is_rejected_before_analysis() {
    assert!(CyclicWorkLimits::new(0, 1, 1, 1).is_err());
    assert!(CyclicWorkLimits::new(1, 0, 1, 1).is_err());
    assert!(CyclicWorkLimits::new(1, 1, 0, 1).is_err());
    assert!(CyclicWorkLimits::new(1, 1, 1, 0).is_err());
}

#[test]
fn duplicate_task_handle_is_rejected_instead_of_generating_two_task_proofs() {
    let (ast, model) = accepted_model(valid_loop_source(), &[7, 7]);
    let result = analyze_cyclic_work(
        &[SemanticSource::new(&ast, valid_loop_source())],
        &model,
        fixed_limits(),
        work_limits(1, 3, 128),
    );
    assert_eq!(result.err(), Some(CyclicWorkInputError::DuplicateTask(7)));
}

#[test]
fn a_span_incompatible_source_cannot_reuse_an_accepted_address_model() {
    let (_ast, model) = accepted_model(valid_loop_source(), &[7]);
    let changed = valid_loop_source().replace("DINT#2 BY", "DINT#-2 BY");
    let changed_ast = parsed(&changed);
    let result = analyze_cyclic_work(
        &[SemanticSource::new(&changed_ast, &changed)],
        &model,
        fixed_limits(),
        work_limits(1, 3, 128),
    );
    assert_eq!(
        result.err(),
        Some(CyclicWorkInputError::UpstreamModelMismatch)
    );
}
