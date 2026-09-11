//! R1-06/07 Canonical ST artifacts, AOT and reference differential boundaries.

use aurora_st_ir::{
    AddressBindingInputs, AddressBindingLimits, AddressSemanticModel, AotLimits, AotTarget,
    AstNode, AstNodeKind, CanonicalArtifactLimits, CanonicalIrInputError, CanonicalIrLimits,
    CanonicalIrSerializationError, CanonicalNode, CanonicalSourceMapLimits,
    CanonicalSourceMapSerializationError, CheckpointPlanLimits, CheckpointPlanSerializationError,
    CheckpointSiteKind, CyclicWorkLimits, CyclicWorkModel, DiagnosticCode, ExternalField,
    FixedDataLimits, InitializationLimits, ParserLimits, ProgramTaskBinding, ReferenceCycleRequest,
    ReferenceExecutionError, ReferenceExecutor, ReferenceInput, ReferenceLimitError,
    ReferenceLimits, SemanticSource, SourceSpan, TagCatalogEntry, TaskHandle, VersionedAst,
    analyze_addresses, analyze_cyclic_work, canonical_ir_to_json, canonical_source_map_to_json,
    checkpoint_plan_to_json, lower_canonical_ir, parse,
};
#[cfg(target_os = "linux")]
use aurora_st_ir::{
    DifferentialCycle, DifferentialDiagnostic, DifferentialFault, DifferentialStatus,
    DifferentialTrace, DifferentialValue, RuntimeFaultCode, compare_differential_traces,
};
use object::{Object as _, ObjectSection as _, ObjectSymbol as _, RelocationTarget};

const TAG_ID: &str = "01890f3e-4c7b-7cc2-98c4-dc0c0c073901";
const EXTERNAL_SOURCE: &str = "x";
const LINUX_X64_AOT_GOLDEN_SHA256: &str =
    "bb4ef29014ff99c6f991c3f7edfcbad355cfa308fcad09404586e79073f7ed7d";
#[cfg(target_os = "linux")]
const RUNTIME_SHIM: &str = r"
#include <stdint.h>

static uint32_t write_count;
static uint32_t checkpoint_count;
static uint32_t fault_count;
static uint32_t invalid_write_width;

uint64_t aurora_st_read_bits_v1(
    uint64_t context,
    uint32_t task,
    uint32_t activation,
    uint32_t symbol,
    uint64_t offset_bytes,
    uint32_t width_bytes
) {
    (void)context;
    (void)task;
    (void)activation;
    (void)symbol;
    (void)offset_bytes;
    (void)width_bytes;
    return 0;
}

void aurora_st_write_bits_v1(
    uint64_t context,
    uint32_t task,
    uint32_t activation,
    uint32_t symbol,
    uint64_t offset_bytes,
    uint32_t width_bytes,
    uint64_t value_bits
) {
    (void)context;
    (void)task;
    (void)activation;
    (void)symbol;
    (void)offset_bytes;
    if (width_bytes != 4) {
        invalid_write_width = 1;
    }
    (void)value_bits;
    write_count += 1;
}

uint32_t aurora_st_checkpoint_v1(uint64_t context, uint32_t checkpoint_site) {
    (void)context;
    (void)checkpoint_site;
    checkpoint_count += 1;
    return 0;
}

void aurora_st_report_fault_v1(
    uint64_t context,
    uint32_t canonical_fault_site,
    uint32_t fault_code
) {
    (void)context;
    (void)canonical_fault_site;
    (void)fault_code;
    fault_count += 1;
}

void aurora_st_reset_frame_v1(
    uint64_t context,
    uint32_t task,
    uint32_t activation,
    uint32_t pou
) {
    (void)context;
    (void)task;
    (void)activation;
    (void)pou;
}

uint32_t aurora_st_concat_string_v1(
    uint64_t destination,
    uint64_t left,
    uint64_t right,
    uint32_t capacity_units,
    uint32_t unit_width_bytes
) {
    (void)destination;
    (void)left;
    (void)right;
    (void)capacity_units;
    (void)unit_width_bytes;
    return 0;
}

extern uint32_t __TASK_SYMBOL__(uint64_t context);

int main(void) {
    uint32_t status = __TASK_SYMBOL__(1);
    if (status != 0) {
        return 10;
    }
    if (write_count == 0 || checkpoint_count == 0) {
        return 11;
    }
    if (fault_count != 0) {
        return 12;
    }
    if (invalid_write_width != 0) {
        return 13;
    }
    return 0;
}
";

#[cfg(target_os = "linux")]
const DIFFERENTIAL_RUNTIME_SHIM: &str = r#"
#include <stdint.h>
#include <stdio.h>

#define CYCLE_COUNT __CYCLE_COUNT__
#define NO_SITE UINT32_MAX

static const uint32_t inputs[CYCLE_COUNT] = { __INPUTS__ };
static const uint32_t stop_sites[CYCLE_COUNT] = { __STOP_SITES__ };
static uint32_t state_committed = 0;
static uint32_t state_staging = 0;
static uint32_t global_committed = 0;
static uint32_t global_staging = 0;
static uint32_t frame_input = 0;
static uint32_t frame_return = 0;
static uint32_t current_cycle = 0;
static uint32_t fault_site = NO_SITE;
static uint32_t fault_code = 0;
static uint32_t diagnostic_count = 0;
static uint32_t checkpoint_count = 0;
static uint32_t checkpoints[32];
static uint32_t bad_access = 0;

static uint32_t *resolve(uint32_t activation, uint32_t symbol) {
    if (symbol == __COUNTER_SYMBOL__ && activation == UINT32_MAX) {
        return &global_staging;
    }
    if (symbol == __STATE_SYMBOL__ && activation == UINT32_MAX) {
        return &state_staging;
    }
    if (symbol == __INPUT_SYMBOL__ && activation == __FUNCTION_SYMBOL__) {
        return &frame_input;
    }
    if (symbol == __FUNCTION_SYMBOL__ && activation == __FUNCTION_SYMBOL__) {
        return &frame_return;
    }
    bad_access = 1;
    return &bad_access;
}

uint64_t aurora_st_read_bits_v1(
    uint64_t context, uint32_t task, uint32_t activation, uint32_t symbol,
    uint64_t offset_bytes, uint32_t width_bytes
) {
    if (context != 1 || task != __TASK_HANDLE__ || offset_bytes != 0 || width_bytes != 4) {
        bad_access = 2;
        return 0;
    }
    return *resolve(activation, symbol);
}

void aurora_st_write_bits_v1(
    uint64_t context, uint32_t task, uint32_t activation, uint32_t symbol,
    uint64_t offset_bytes, uint32_t width_bytes, uint64_t value_bits
) {
    if (context != 1 || task != __TASK_HANDLE__ || offset_bytes != 0 || width_bytes != 4) {
        bad_access = 3;
        return;
    }
    *resolve(activation, symbol) = (uint32_t)value_bits;
}

uint32_t aurora_st_checkpoint_v1(uint64_t context, uint32_t checkpoint_site) {
    if (context != 1 || checkpoint_count >= 32) {
        bad_access = 4;
        return 1;
    }
    checkpoints[checkpoint_count++] = checkpoint_site;
    return stop_sites[current_cycle] == checkpoint_site ? 1 : 0;
}

void aurora_st_report_fault_v1(
    uint64_t context, uint32_t canonical_fault_site, uint32_t code
) {
    if (context != 1 || diagnostic_count != 0) {
        bad_access = 5;
        return;
    }
    fault_site = canonical_fault_site;
    fault_code = code;
    diagnostic_count = 1;
}

void aurora_st_reset_frame_v1(
    uint64_t context, uint32_t task, uint32_t activation, uint32_t pou
) {
    if (context != 1 || task != __TASK_HANDLE__ ||
        activation != __FUNCTION_SYMBOL__ || pou != __FUNCTION_SYMBOL__) {
        bad_access = 6;
        return;
    }
    frame_input = 0;
    frame_return = 0;
}

uint32_t aurora_st_concat_string_v1(
    uint64_t destination, uint64_t left, uint64_t right,
    uint32_t capacity_units, uint32_t unit_width_bytes
) {
    (void)destination;
    (void)left;
    (void)right;
    (void)capacity_units;
    (void)unit_width_bytes;
    bad_access = 7;
    return 1;
}

extern uint32_t __TASK_SYMBOL__(uint64_t context);

int main(void) {
    for (current_cycle = 0; current_cycle < CYCLE_COUNT; current_cycle += 1) {
        global_committed = inputs[current_cycle];
        global_staging = global_committed;
        state_staging = state_committed;
        fault_site = NO_SITE;
        fault_code = 0;
        diagnostic_count = 0;
        checkpoint_count = 0;
        uint32_t status = __TASK_SYMBOL__(1);
        if (status == 0) {
            global_committed = global_staging;
            state_committed = state_staging;
        }
        if (bad_access != 0) {
            return (int)(20 + bad_access);
        }
        printf("%u %u %u %u %u %u %u %u",
            current_cycle, status, state_committed, global_committed,
            fault_site, fault_code, diagnostic_count, checkpoint_count);
        for (uint32_t index = 0; index < checkpoint_count; index += 1) {
            printf(" %u", checkpoints[index]);
        }
        printf("\n");
    }
    return 0;
}
"#;

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

fn ir_limits(max_nodes: usize, max_pous: usize, max_bytes: usize) -> CanonicalIrLimits {
    CanonicalIrLimits::new(max_nodes, max_pous, max_bytes)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn initialization_limits() -> InitializationLimits {
    InitializationLimits::new(
        64 * 1024,
        1024 * 1024,
        1024 * 1024,
        1024 * 1024,
        4 * 1024 * 1024,
    )
    .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn source_map_limits(
    max_sources: usize,
    max_symbols: usize,
    max_nodes: usize,
    max_fault_sites: usize,
    max_bytes: usize,
) -> CanonicalSourceMapLimits {
    CanonicalSourceMapLimits::new(
        max_sources,
        max_symbols,
        max_nodes,
        max_fault_sites,
        max_bytes,
    )
    .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn generous_source_map_limits() -> CanonicalSourceMapLimits {
    source_map_limits(16, 4096, 4096, 4096, 1024 * 1024)
}

fn checkpoint_limits(
    max_sites_per_pou: usize,
    max_total_sites: usize,
    max_bytes: usize,
) -> CheckpointPlanLimits {
    CheckpointPlanLimits::new(max_sites_per_pou, max_total_sites, max_bytes)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn generous_checkpoint_limits() -> CheckpointPlanLimits {
    checkpoint_limits(4096, 4096, 1024 * 1024)
}

fn source() -> &'static str {
    r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
FUNCTION Increment : DINT
VAR_INPUT
  Value : DINT;
END_VAR
RETURN CHECKED_ADD(CHECKED_ADD(Value, DINT#1), DINT#1);
END_FUNCTION
PROGRAM Main
VAR
  Index : DINT;
END_VAR
FOR Index := DINT#0 TO DINT#2 BY DINT#1 DO
  Counter := Increment(Counter);
END_FOR;
END_PROGRAM
"
}

fn accepted_models() -> (VersionedAst, AddressSemanticModel, CyclicWorkModel) {
    let parsed = parse("program/main.st", source().as_bytes(), parser_limits());
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let ast = parsed
        .ast
        .unwrap_or_else(|| unreachable!("diagnostic-free parse publishes an AST"));
    let (address_model, work_model) = accepted_models_for(&[SemanticSource::new(&ast, source())]);
    (ast, address_model, work_model)
}

fn accepted_models_for(sources: &[SemanticSource<'_>]) -> (AddressSemanticModel, CyclicWorkModel) {
    accepted_models_for_bindings(sources, &[("Main", 7)])
}

fn accepted_models_for_bindings(
    sources: &[SemanticSource<'_>],
    task_bindings: &[(&str, u32)],
) -> (AddressSemanticModel, CyclicWorkModel) {
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
    let semantic = aurora_st_ir::analyze_faults(sources, fixed_limits())
        .unwrap_or_else(|error| unreachable!("parser-produced source is valid: {error}"));
    let fault_model = semantic
        .model
        .unwrap_or_else(|| unreachable!("test source passes R1-04: {:?}", semantic.diagnostics));
    let tasks = task_bindings
        .iter()
        .map(|(program_name, handle)| ProgramTaskBinding {
            program: fault_model
                .fixed
                .semantics
                .symbols
                .iter()
                .find(|symbol| symbol.name == *program_name)
                .map_or_else(
                    || unreachable!("test declares {program_name}"),
                    |symbol| symbol.id,
                ),
            task_handle: TaskHandle(*handle),
        })
        .collect::<Vec<_>>();
    let addresses = analyze_addresses(
        sources,
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
    let address_model = addresses
        .model
        .unwrap_or_else(|| unreachable!("test source passes R1-05: {:?}", addresses.diagnostics));
    let work = analyze_cyclic_work(sources, &address_model, fixed_limits(), work_limits())
        .unwrap_or_else(|error| unreachable!("accepted inputs have valid shape: {error}"));
    let work_model = work
        .model
        .unwrap_or_else(|| unreachable!("test source passes work proof: {:?}", work.diagnostics));
    (address_model, work_model)
}

fn lower(
    ast: &VersionedAst,
    address_model: &AddressSemanticModel,
    work_model: &CyclicWorkModel,
    limits: CanonicalIrLimits,
) -> aurora_st_ir::CanonicalIrOutput {
    lower_with_source_map(
        ast,
        address_model,
        work_model,
        generous_source_map_limits(),
        limits,
    )
}

fn lower_with_source_map(
    ast: &VersionedAst,
    address_model: &AddressSemanticModel,
    work_model: &CyclicWorkModel,
    map_limits: CanonicalSourceMapLimits,
    ir_limits: CanonicalIrLimits,
) -> aurora_st_ir::CanonicalIrOutput {
    lower_with_artifact_limits(
        ast,
        address_model,
        work_model,
        map_limits,
        generous_checkpoint_limits(),
        ir_limits,
    )
}

fn lower_with_artifact_limits(
    ast: &VersionedAst,
    address_model: &AddressSemanticModel,
    work_model: &CyclicWorkModel,
    map_limits: CanonicalSourceMapLimits,
    checkpoint_limits: CheckpointPlanLimits,
    ir_limits: CanonicalIrLimits,
) -> aurora_st_ir::CanonicalIrOutput {
    lower_canonical_ir(
        &[SemanticSource::new(ast, source())],
        address_model,
        work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(ir_limits, map_limits, checkpoint_limits),
    )
    .unwrap_or_else(|error| unreachable!("accepted models lower successfully: {error}"))
}

fn ir_node_count(node: &CanonicalNode) -> usize {
    1 + node.children.iter().map(ir_node_count).sum::<usize>()
}

fn compile_source_aot(source_text: &str) -> aurora_st_ir::AotArtifact {
    compile_source_aot_with_stack_limit(source_text, 1024 * 1024)
        .unwrap_or_else(|error| unreachable!("accepted project compiles: {error}"))
}

fn compile_source_aot_with_stack_limit(
    source_text: &str,
    stack_limit: usize,
) -> Result<aurora_st_ir::AotArtifact, aurora_st_ir::AotBuildError> {
    let parsed = parse("program/main.st", source_text.as_bytes(), parser_limits());
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let ast = parsed
        .ast
        .unwrap_or_else(|| unreachable!("diagnostic-free source publishes an AST"));
    let sources = [SemanticSource::new(&ast, source_text)];
    let (address_model, work_model) = accepted_models_for(&sources);
    let output = lower_canonical_ir(
        &sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 64, 4 * 1024 * 1024),
            generous_source_map_limits(),
            generous_checkpoint_limits(),
        ),
    )
    .unwrap_or_else(|error| unreachable!("accepted project lowers: {error}"));
    let ir = output
        .ir
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted IR is published"));
    let source_map = output
        .source_map
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted Source Map is published"));
    let checkpoints = output
        .checkpoint_plan
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted checkpoint plan is published"));
    assert_eq!(
        ir.pous
            .iter()
            .map(|pou| ir_node_count(&pou.body))
            .sum::<usize>(),
        source_map.nodes.len()
    );
    assert_eq!(ir.tasks.len(), checkpoints.tasks.len());
    let mut node_ids = ir
        .pous
        .iter()
        .flat_map(|pou| {
            let mut nodes = vec![&pou.body];
            let mut result = Vec::new();
            while let Some(node) = nodes.pop() {
                result.push(node.id);
                nodes.extend(node.children.iter());
            }
            result
        })
        .collect::<Vec<_>>();
    node_ids.sort_unstable();
    node_ids.dedup();
    assert_eq!(node_ids.len(), source_map.nodes.len());
    assert!(
        source_map
            .nodes
            .iter()
            .all(|entry| node_ids.binary_search(&entry.node).is_ok())
    );
    assert!(
        checkpoints
            .sites
            .iter()
            .all(|site| node_ids.binary_search(&site.node).is_ok())
    );
    assert!(checkpoints.tasks.iter().all(|planned| {
        ir.tasks
            .iter()
            .any(|task| task.task == planned.task && task.program == planned.program)
    }));
    let mut task_handles = ir.tasks.iter().map(|task| task.task).collect::<Vec<_>>();
    task_handles.sort_unstable();
    task_handles.dedup();
    assert_eq!(task_handles.len(), ir.tasks.len());
    aurora_st_ir::compile_linux_x64_aot(
        ir,
        source_map,
        checkpoints,
        AotTarget::linux_x64_v1(),
        AotLimits::new(
            128,
            2 * 1024 * 1024,
            8 * 1024 * 1024,
            8192,
            16 * 1024,
            stack_limit,
        )
        .unwrap_or_else(|error| unreachable!("test limits are non-zero: {error}")),
    )
}

#[test]
fn accepted_function_loop_project_emits_stable_linux_x64_aot() {
    let (ast, address_model, work_model) = accepted_models();
    let lowered = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 128, 4 * 1024 * 1024),
    );
    let ir = lowered
        .ir
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes IR"));
    let source_map = lowered
        .source_map
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes Source Map"));
    let checkpoints = lowered
        .checkpoint_plan
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes checkpoint plan"));
    let limits = AotLimits::new(128, 1024 * 1024, 8 * 1024 * 1024, 4096, 8192, 1024 * 1024)
        .unwrap_or_else(|error| unreachable!("test limits are non-zero: {error}"));
    let first = aurora_st_ir::compile_linux_x64_aot(
        ir,
        source_map,
        checkpoints,
        AotTarget::linux_x64_v1(),
        limits,
    )
    .unwrap_or_else(|error| unreachable!("accepted project compiles: {error}"));
    let second = aurora_st_ir::compile_linux_x64_aot(
        ir,
        source_map,
        checkpoints,
        AotTarget::linux_x64_v1(),
        limits,
    )
    .unwrap_or_else(|error| unreachable!("same project compiles: {error}"));

    assert_eq!(first.object, second.object);
    assert_eq!(first.object_sha256, second.object_sha256);
    assert_eq!(first.object_sha256, LINUX_X64_AOT_GOLDEN_SHA256);
    assert_eq!(first.task_exports.len(), 1);
    assert!(
        first
            .runtime_imports
            .iter()
            .any(|entry| entry.symbol == "aurora_st_checkpoint_v1")
    );
    assert!(
        first
            .native_source_map
            .ranges
            .windows(2)
            .all(|pair| pair[0].function_symbol < pair[1].function_symbol
                || (pair[0].function_symbol == pair[1].function_symbol
                    && pair[0].start <= pair[1].start))
    );
    let object = object::File::parse(&*first.object)
        .unwrap_or_else(|error| unreachable!("validated object parses: {error}"));
    let checkpoint_symbol = object
        .symbols()
        .find(|symbol| symbol.name().ok() == Some("aurora_st_checkpoint_v1"))
        .unwrap_or_else(|| unreachable!("checkpoint import is present"))
        .index();
    let checkpoint_calls = object
        .sections()
        .flat_map(|section| section.relocations())
        .filter(|(_, relocation)| {
            relocation.target() == RelocationTarget::Symbol(checkpoint_symbol)
        })
        .count();
    assert_eq!(
        checkpoint_calls, 3,
        "one static loop back edge and one before/after user call; Task return is not duplicated"
    );
}

#[test]
fn reference_executor_commits_and_discards_exact_cycle_state() {
    let (ast, address_model, work_model) = accepted_models();
    let lowered = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 128, 4 * 1024 * 1024),
    );
    let ir = lowered
        .ir
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes IR"));
    let source_map = lowered
        .source_map
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes Source Map"));
    let checkpoints = lowered
        .checkpoint_plan
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes checkpoint plan"));
    let counter = ir
        .symbols
        .iter()
        .find(|symbol| symbol.name == "Counter")
        .map_or_else(
            || unreachable!("source declares Counter"),
            |symbol| symbol.id,
        );
    let index = ir
        .symbols
        .iter()
        .find(|symbol| symbol.name == "Index")
        .map_or_else(|| unreachable!("source declares Index"), |symbol| symbol.id);
    let first_call_checkpoint = checkpoints
        .sites
        .iter()
        .find(|site| matches!(site.site, CheckpointSiteKind::BeforePouCall { .. }))
        .map_or_else(
            || unreachable!("source has a user call checkpoint"),
            |site| site.id,
        );
    let limits = ReferenceLimits::new(128, 64 * 1024, 3, 4, 32)
        .unwrap_or_else(|error| unreachable!("reference limits are non-zero: {error}"));
    let mut executor = ReferenceExecutor::new(ir, source_map, checkpoints, limits)
        .unwrap_or_else(|error| unreachable!("accepted artifacts create an executor: {error}"));

    let completed = executor
        .run_cycle(ReferenceCycleRequest {
            task: TaskHandle(7),
            inputs: &[ReferenceInput {
                symbol: counter,
                value_bits: 4,
            }],
            stop_at_checkpoint: None,
        })
        .unwrap_or_else(|error| unreachable!("first cycle executes: {error}"));
    assert_eq!(
        completed.status,
        aurora_st_ir::DifferentialStatus::Completed
    );
    assert_eq!(snapshot_u32(&completed.outputs, counter), 10);
    assert_eq!(snapshot_u32(&completed.state, index), 3);
    assert_eq!(completed.checkpoints.len(), 9);
    assert!(completed.fault.is_none());
    assert!(completed.diagnostics.is_empty());

    let stopped = executor
        .run_cycle(ReferenceCycleRequest {
            task: TaskHandle(7),
            inputs: &[ReferenceInput {
                symbol: counter,
                value_bits: 5,
            }],
            stop_at_checkpoint: Some(first_call_checkpoint),
        })
        .unwrap_or_else(|error| unreachable!("checkpoint cycle executes: {error}"));
    assert_eq!(
        stopped.status,
        aurora_st_ir::DifferentialStatus::CheckpointStop
    );
    assert_eq!(snapshot_u32(&stopped.outputs, counter), 5);
    assert_eq!(snapshot_u32(&stopped.state, index), 3);
    assert_eq!(stopped.checkpoints, [first_call_checkpoint]);

    let faulted = executor
        .run_cycle(ReferenceCycleRequest {
            task: TaskHandle(7),
            inputs: &[ReferenceInput {
                symbol: counter,
                value_bits: u64::from(i32::MAX.cast_unsigned()),
            }],
            stop_at_checkpoint: None,
        })
        .unwrap_or_else(|error| unreachable!("Fault cycle executes: {error}"));
    assert_eq!(faulted.status, aurora_st_ir::DifferentialStatus::Faulted);
    assert_eq!(
        snapshot_u32(&faulted.outputs, counter),
        i32::MAX.cast_unsigned()
    );
    assert_eq!(snapshot_u32(&faulted.state, index), 3);
    assert!(faulted.fault.is_some());
    assert_eq!(faulted.diagnostics.len(), 1);
}

#[test]
fn every_reference_zero_limit_is_rejected() {
    assert_eq!(
        ReferenceLimits::new(0, 1, 1, 1, 1),
        Err(ReferenceLimitError::ZeroStorageEntries)
    );
    assert_eq!(
        ReferenceLimits::new(1, 0, 1, 1, 1),
        Err(ReferenceLimitError::ZeroStorageBytes)
    );
    assert_eq!(
        ReferenceLimits::new(1, 1, 0, 1, 1),
        Err(ReferenceLimitError::ZeroCycles)
    );
    assert_eq!(
        ReferenceLimits::new(1, 1, 1, 0, 1),
        Err(ReferenceLimitError::ZeroInputs)
    );
    assert_eq!(
        ReferenceLimits::new(1, 1, 1, 1, 0),
        Err(ReferenceLimitError::ZeroCheckpoints)
    );
}

#[test]
fn reference_rejects_incomplete_artifact_and_duplicate_input_atomically() {
    let (ast, address_model, work_model) = accepted_models();
    let lowered = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 128, 4 * 1024 * 1024),
    );
    let ir = lowered
        .ir
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes IR"));
    let source_map = lowered
        .source_map
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes Source Map"));
    let checkpoints = lowered
        .checkpoint_plan
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes checkpoint plan"));
    let counter = symbol_named(ir, "Counter");
    let input = ReferenceInput {
        symbol: counter,
        value_bits: 4,
    };

    let mut incomplete = checkpoints.clone();
    incomplete.sites.pop();
    assert!(matches!(
        ReferenceExecutor::new(
            ir,
            source_map,
            &incomplete,
            ReferenceLimits::new(4, 16, 1, 1, 9)
                .unwrap_or_else(|error| unreachable!("limits are non-zero: {error}")),
        ),
        Err(ReferenceExecutionError::InconsistentArtifact)
    ));

    let duplicate = [input, input];
    let mut exact = ReferenceExecutor::new(
        ir,
        source_map,
        checkpoints,
        ReferenceLimits::new(4, 16, 1, 2, 9)
            .unwrap_or_else(|error| unreachable!("limits are non-zero: {error}")),
    )
    .unwrap_or_else(|error| unreachable!("exact storage limits construct: {error}"));
    assert_eq!(
        exact.run_cycle(ReferenceCycleRequest {
            task: TaskHandle(7),
            inputs: &duplicate,
            stop_at_checkpoint: None,
        }),
        Err(ReferenceExecutionError::DuplicateInput(counter.0))
    );
    let completed = exact
        .run_cycle(ReferenceCycleRequest {
            task: TaskHandle(7),
            inputs: &[input],
            stop_at_checkpoint: None,
        })
        .unwrap_or_else(|error| unreachable!("exact capacities execute: {error}"));
    assert_eq!(
        completed.cycle, 0,
        "rejected input must not consume a cycle"
    );
    assert_eq!(completed.checkpoints.len(), 9);
    assert_eq!(
        exact.run_cycle(ReferenceCycleRequest {
            task: TaskHandle(7),
            inputs: &[input],
            stop_at_checkpoint: None,
        }),
        Err(ReferenceExecutionError::CycleSequenceExhausted)
    );
}

#[test]
fn reference_capacities_accept_exact_and_reject_one_less() {
    let (ast, address_model, work_model) = accepted_models();
    let lowered = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 128, 4 * 1024 * 1024),
    );
    let ir = lowered
        .ir
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes IR"));
    let source_map = lowered
        .source_map
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes Source Map"));
    let checkpoints = lowered
        .checkpoint_plan
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes checkpoint plan"));
    let input = ReferenceInput {
        symbol: symbol_named(ir, "Counter"),
        value_bits: 4,
    };

    for (limits, resource, actual, limit) in [
        (
            ReferenceLimits::new(3, 16, 1, 1, 9)
                .unwrap_or_else(|error| unreachable!("limits are non-zero: {error}")),
            "storage entries",
            4,
            3,
        ),
        (
            ReferenceLimits::new(4, 15, 1, 1, 9)
                .unwrap_or_else(|error| unreachable!("limits are non-zero: {error}")),
            "storage bytes",
            16,
            15,
        ),
        (
            ReferenceLimits::new(4, 16, 1, 1, 8)
                .unwrap_or_else(|error| unreachable!("limits are non-zero: {error}")),
            "checkpoints per cycle",
            9,
            8,
        ),
    ] {
        let mut executor = ReferenceExecutor::new(ir, source_map, checkpoints, limits)
            .unwrap_or_else(|error| unreachable!("initial storage stays within limit: {error}"));
        assert_eq!(
            executor.run_cycle(ReferenceCycleRequest {
                task: TaskHandle(7),
                inputs: &[input],
                stop_at_checkpoint: None,
            }),
            Err(ReferenceExecutionError::CapacityExceeded {
                resource,
                actual,
                limit,
            })
        );
    }
}

fn snapshot_u32(values: &[aurora_st_ir::DifferentialValue], symbol: aurora_st_ir::SymbolId) -> u32 {
    let bytes = values
        .iter()
        .find(|entry| entry.symbol == symbol)
        .map_or_else(
            || unreachable!("snapshot contains requested symbol"),
            |entry| entry.bytes.as_slice(),
        );
    let encoded = bytes
        .get(..4)
        .and_then(|value| value.try_into().ok())
        .unwrap_or_else(|| unreachable!("DINT snapshot contains four bytes"));
    u32::from_le_bytes(encoded)
}

#[cfg(target_os = "linux")]
#[test]
fn generated_object_statically_links_and_runs_with_the_runtime_abi() {
    let artifact = compile_source_aot(source());
    let task_symbol = artifact.task_exports.first().map_or_else(
        || unreachable!("accepted project exports one task"),
        |entry| entry.symbol.as_str(),
    );
    let directory = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("aot-static-link-{}", std::process::id()));
    if let Err(error) = std::fs::remove_dir_all(&directory)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        unreachable!("stale test directory can be removed: {error}");
    }
    std::fs::create_dir_all(&directory)
        .unwrap_or_else(|error| unreachable!("test directory can be created: {error}"));
    let object_path = directory.join("program.o");
    let shim_path = directory.join("runtime-shim.c");
    let executable_path = directory.join("runtime-shim");
    std::fs::write(&object_path, &artifact.object)
        .unwrap_or_else(|error| unreachable!("AOT object can be written: {error}"));
    std::fs::write(
        &shim_path,
        RUNTIME_SHIM.replace("__TASK_SYMBOL__", task_symbol),
    )
    .unwrap_or_else(|error| unreachable!("Runtime shim can be written: {error}"));

    let linked = std::process::Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-no-pie",
            "-Wl,--no-undefined",
            "-Wl,-z,noexecstack",
        ])
        .arg(&object_path)
        .arg(&shim_path)
        .arg("-o")
        .arg(&executable_path)
        .output()
        .unwrap_or_else(|error| unreachable!("Linux C linker is available: {error}"));
    assert!(
        linked.status.success(),
        "static AOT link failed:\n{}",
        String::from_utf8_lossy(&linked.stderr)
    );

    let executed = std::process::Command::new(&executable_path)
        .output()
        .unwrap_or_else(|error| unreachable!("linked Runtime shim can execute: {error}"));
    assert!(
        executed.status.success(),
        "linked Runtime shim exited with {:?}:\n{}",
        executed.status.code(),
        String::from_utf8_lossy(&executed.stderr)
    );
    std::fs::remove_dir_all(&directory)
        .unwrap_or_else(|error| unreachable!("test directory can be removed: {error}"));
}

#[cfg(target_os = "linux")]
#[test]
fn fixed_seed_reference_and_linked_aot_match_every_cycle() {
    const SEED: u64 = 0x6a09_e667_f3bc_c909;
    let (ast, address_model, work_model) = accepted_models();
    let lowered = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 128, 4 * 1024 * 1024),
    );
    let ir = lowered
        .ir
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes IR"));
    let source_map = lowered
        .source_map
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes Source Map"));
    let checkpoints = lowered
        .checkpoint_plan
        .as_ref()
        .unwrap_or_else(|| unreachable!("accepted lowering publishes checkpoint plan"));
    let artifact = aurora_st_ir::compile_linux_x64_aot(
        ir,
        source_map,
        checkpoints,
        AotTarget::linux_x64_v1(),
        AotLimits::new(128, 1024 * 1024, 8 * 1024 * 1024, 4096, 8192, 1024 * 1024)
            .unwrap_or_else(|error| unreachable!("AOT limits are non-zero: {error}")),
    )
    .unwrap_or_else(|error| unreachable!("accepted project compiles: {error}"));
    let counter = symbol_named(ir, "Counter");
    let index = symbol_named(ir, "Index");
    let increment = symbol_named(ir, "Increment");
    let input = ir
        .symbols
        .iter()
        .find(|symbol| symbol.owner == Some(increment) && symbol.name == "Value")
        .map_or_else(
            || unreachable!("Increment declares Value"),
            |symbol| symbol.id,
        );
    let task = ir.tasks[0].task;
    let stop_site = checkpoints
        .sites
        .iter()
        .find(|site| matches!(site.site, CheckpointSiteKind::BeforePouCall { .. }))
        .map_or_else(
            || unreachable!("test source contains a call checkpoint"),
            |site| site.id,
        );
    let inputs = fixed_seed_inputs(SEED);
    let mut reference = ReferenceExecutor::new(
        ir,
        source_map,
        checkpoints,
        ReferenceLimits::new(
            128,
            64 * 1024,
            u64::try_from(inputs.len())
                .unwrap_or_else(|error| unreachable!("cycle count fits u64: {error}")),
            1,
            32,
        )
        .unwrap_or_else(|error| unreachable!("reference limits are non-zero: {error}")),
    )
    .unwrap_or_else(|error| unreachable!("accepted artifacts create an executor: {error}"));
    let mut expected_cycles = Vec::with_capacity(inputs.len());
    for (cycle, value) in inputs.iter().copied().enumerate() {
        expected_cycles.push(
            reference
                .run_cycle(ReferenceCycleRequest {
                    task,
                    inputs: &[ReferenceInput {
                        symbol: counter,
                        value_bits: u64::from(value),
                    }],
                    stop_at_checkpoint: (cycle == 2).then_some(stop_site),
                })
                .unwrap_or_else(|error| unreachable!("reference cycle executes: {error}")),
        );
    }
    let expected = DifferentialTrace {
        seed: SEED,
        cycles: expected_cycles,
    };
    let actual = run_linked_aot_trace(&LinkedTraceInput {
        artifact: &artifact,
        seed: SEED,
        task,
        counter,
        state: index,
        function: increment,
        input,
        inputs: &inputs,
        stop_site,
    });
    assert_eq!(compare_differential_traces(&expected, &actual), Ok(()));
    assert_reproducible_minimal_mismatch(&expected, &actual);
}

#[cfg(target_os = "linux")]
fn assert_reproducible_minimal_mismatch(expected: &DifferentialTrace, actual: &DifferentialTrace) {
    let mut changed = actual.clone();
    changed.cycles[3].state[0].bytes[0] ^= 1;
    let first = compare_differential_traces(expected, &changed);
    let second = compare_differential_traces(expected, &changed);
    assert_eq!(first, second);
    let mismatch = first
        .err()
        .unwrap_or_else(|| unreachable!("changed trace has a mismatch"));
    assert_eq!(mismatch.cycle, 3);
    assert_eq!(mismatch.minimal_cycle_count, 4);
}

#[cfg(target_os = "linux")]
struct LinkedTraceInput<'a> {
    artifact: &'a aurora_st_ir::AotArtifact,
    seed: u64,
    task: TaskHandle,
    counter: aurora_st_ir::SymbolId,
    state: aurora_st_ir::SymbolId,
    function: aurora_st_ir::SymbolId,
    input: aurora_st_ir::SymbolId,
    inputs: &'a [u32],
    stop_site: aurora_st_ir::CheckpointSiteId,
}

#[cfg(target_os = "linux")]
fn run_linked_aot_trace(fixture: &LinkedTraceInput<'_>) -> DifferentialTrace {
    let task_symbol = fixture.artifact.task_exports.first().map_or_else(
        || unreachable!("accepted project exports one task"),
        |entry| entry.symbol.as_str(),
    );
    let input_values = fixture
        .inputs
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let stop_values = fixture
        .inputs
        .iter()
        .enumerate()
        .map(|(cycle, _)| {
            if cycle == 2 {
                fixture.stop_site.0.to_string()
            } else {
                "UINT32_MAX".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let shim = DIFFERENTIAL_RUNTIME_SHIM
        .replace("__CYCLE_COUNT__", &fixture.inputs.len().to_string())
        .replace("__INPUTS__", &input_values)
        .replace("__STOP_SITES__", &stop_values)
        .replace("__COUNTER_SYMBOL__", &fixture.counter.0.to_string())
        .replace("__STATE_SYMBOL__", &fixture.state.0.to_string())
        .replace("__INPUT_SYMBOL__", &fixture.input.0.to_string())
        .replace("__FUNCTION_SYMBOL__", &fixture.function.0.to_string())
        .replace("__TASK_HANDLE__", &fixture.task.0.to_string())
        .replace("__TASK_SYMBOL__", task_symbol);
    let directory = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("aot-differential-{}", std::process::id()));
    if let Err(error) = std::fs::remove_dir_all(&directory)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        unreachable!("stale differential directory can be removed: {error}");
    }
    std::fs::create_dir_all(&directory)
        .unwrap_or_else(|error| unreachable!("differential directory can be created: {error}"));
    let object_path = directory.join("program.o");
    let shim_path = directory.join("runtime-shim.c");
    let executable_path = directory.join("runtime-shim");
    std::fs::write(&object_path, &fixture.artifact.object)
        .unwrap_or_else(|error| unreachable!("AOT object can be written: {error}"));
    std::fs::write(&shim_path, shim)
        .unwrap_or_else(|error| unreachable!("differential shim can be written: {error}"));
    let linked = std::process::Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-no-pie",
            "-Wl,--no-undefined",
            "-Wl,-z,noexecstack",
        ])
        .arg(&object_path)
        .arg(&shim_path)
        .arg("-o")
        .arg(&executable_path)
        .output()
        .unwrap_or_else(|error| unreachable!("Linux C linker is available: {error}"));
    assert!(
        linked.status.success(),
        "differential AOT link failed:\n{}",
        String::from_utf8_lossy(&linked.stderr)
    );
    let executed = std::process::Command::new(&executable_path)
        .output()
        .unwrap_or_else(|error| unreachable!("linked differential shim executes: {error}"));
    assert!(
        executed.status.success(),
        "differential shim exited with {:?}:\n{}",
        executed.status.code(),
        String::from_utf8_lossy(&executed.stderr)
    );
    let stdout = String::from_utf8(executed.stdout)
        .unwrap_or_else(|error| unreachable!("shim output is UTF-8: {error}"));
    let cycles = stdout
        .lines()
        .map(|line| parse_aot_cycle(line, fixture.task, fixture.state, fixture.counter))
        .collect::<Vec<_>>();
    assert_eq!(cycles.len(), fixture.inputs.len());
    std::fs::remove_dir_all(&directory)
        .unwrap_or_else(|error| unreachable!("differential directory can be removed: {error}"));
    DifferentialTrace {
        seed: fixture.seed,
        cycles,
    }
}

#[cfg(target_os = "linux")]
fn parse_aot_cycle(
    line: &str,
    task: TaskHandle,
    state: aurora_st_ir::SymbolId,
    output: aurora_st_ir::SymbolId,
) -> DifferentialCycle {
    let values = line
        .split_ascii_whitespace()
        .map(|value| {
            value
                .parse::<u32>()
                .unwrap_or_else(|error| unreachable!("shim field is u32: {error}"))
        })
        .collect::<Vec<_>>();
    assert!(values.len() >= 8);
    let checkpoint_count = usize::try_from(values[7])
        .unwrap_or_else(|error| unreachable!("checkpoint count fits usize: {error}"));
    assert_eq!(values.len(), 8 + checkpoint_count);
    let status = match values[1] {
        0 => DifferentialStatus::Completed,
        1 => DifferentialStatus::CheckpointStop,
        2 => DifferentialStatus::Faulted,
        value => unreachable!("unknown AOT status {value}"),
    };
    let fault = (values[4] != u32::MAX).then(|| DifferentialFault {
        site: aurora_st_ir::CanonicalFaultSiteId(values[4]),
        code: runtime_fault(values[5]),
    });
    let diagnostics = fault
        .map(|fault| DifferentialDiagnostic {
            cycle: u64::from(values[0]),
            site: fault.site,
            code: fault.code,
        })
        .into_iter()
        .collect::<Vec<_>>();
    assert_eq!(
        diagnostics.len(),
        usize::try_from(values[6])
            .unwrap_or_else(|error| unreachable!("diagnostic count fits usize: {error}"))
    );
    DifferentialCycle {
        cycle: u64::from(values[0]),
        task,
        status,
        state: vec![DifferentialValue {
            task,
            activation: u32::MAX,
            symbol: state,
            bytes: values[2].to_le_bytes().to_vec(),
        }],
        outputs: vec![DifferentialValue {
            task,
            activation: u32::MAX,
            symbol: output,
            bytes: values[3].to_le_bytes().to_vec(),
        }],
        fault,
        diagnostics,
        checkpoints: values[8..]
            .iter()
            .copied()
            .map(aurora_st_ir::CheckpointSiteId)
            .collect(),
    }
}

#[cfg(target_os = "linux")]
fn runtime_fault(value: u32) -> RuntimeFaultCode {
    match value {
        1 => RuntimeFaultCode::IntegerOverflow,
        2 => RuntimeFaultCode::IntegerDivisionByZero,
        3 => RuntimeFaultCode::NonFiniteFloat,
        4 => RuntimeFaultCode::InvalidRuntimeRange,
        5 => RuntimeFaultCode::ArrayIndexOutOfBounds,
        6 => RuntimeFaultCode::StringCapacityExceeded,
        _ => unreachable!("unknown Runtime Fault code"),
    }
}

#[cfg(target_os = "linux")]
fn fixed_seed_inputs(seed: u64) -> [u32; 5] {
    let mut state = seed;
    let mut result = [0_u32; 5];
    for value in result.iter_mut().take(4) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *value = u32::try_from(state % 97)
            .unwrap_or_else(|error| unreachable!("modulo result fits u32: {error}"))
            + 1;
    }
    result[4] = i32::MAX.cast_unsigned();
    result
}

fn symbol_named(ir: &aurora_st_ir::CanonicalStIr, name: &str) -> aurora_st_ir::SymbolId {
    ir.symbols
        .iter()
        .find(|symbol| symbol.name == name)
        .map_or_else(
            || unreachable!("test source declares {name}"),
            |symbol| symbol.id,
        )
}

fn ast_body_node_count(root: &AstNode) -> usize {
    root.children
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                AstNodeKind::FunctionDeclaration
                    | AstNodeKind::FunctionBlockDeclaration
                    | AstNodeKind::ProgramDeclaration
            )
        })
        .map(|pou| {
            let body = pou
                .children
                .iter()
                .find(|child| child.kind == AstNodeKind::StatementList)
                .unwrap_or_else(|| unreachable!("parser-produced POU has one body"));
            ast_node_count(body)
        })
        .sum()
}

fn ast_node_count(node: &AstNode) -> usize {
    1 + node.children.iter().map(ast_node_count).sum::<usize>()
}

fn nodes<'a>(node: &'a CanonicalNode, output: &mut Vec<&'a CanonicalNode>) {
    output.push(node);
    for child in &node.children {
        nodes(child, output);
    }
}

#[test]
fn every_executable_ast_node_has_one_dense_ir_node_without_loop_unrolling() {
    let (ast, address_model, work_model) = accepted_models();
    let output = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    );
    assert!(output.diagnostics.is_empty());
    let ir = output
        .ir
        .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    assert_eq!(ir.pous.len(), 2);
    let actual: usize = ir.pous.iter().map(|pou| ir_node_count(&pou.body)).sum();
    assert_eq!(actual, ast_body_node_count(&ast.root));
    let mut flat = Vec::new();
    for pou in &ir.pous {
        nodes(&pou.body, &mut flat);
    }
    assert_eq!(
        flat.iter().map(|node| node.id.0).collect::<Vec<_>>(),
        (0..u32::try_from(actual).unwrap_or(0)).collect::<Vec<_>>()
    );
    let loops = flat
        .iter()
        .filter(|node| node.kind == AstNodeKind::ForStatement)
        .collect::<Vec<_>>();
    assert_eq!(loops.len(), 1);
    assert_eq!(loops[0].loop_iterations, Some(3));
}

#[test]
fn source_map_has_exactly_one_entry_per_source_symbol_node_and_fault() {
    let (ast, address_model, work_model) = accepted_models();
    let output = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    );
    assert!(output.diagnostics.is_empty());
    let ir = output
        .ir
        .as_ref()
        .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    let source_map = output
        .source_map
        .as_ref()
        .unwrap_or_else(|| unreachable!("successful lowering publishes its source map"));

    assert_eq!(source_map.sources.len(), 1);
    assert_eq!(source_map.sources[0].id.0, 0);
    assert_eq!(source_map.sources[0].path, "program/main.st");
    assert_eq!(
        source_map.sources[0].byte_length,
        u32::try_from(source().len()).unwrap_or(0)
    );
    assert_eq!(source_map.symbols.len(), ir.symbols.len());
    for (entry, symbol) in source_map.symbols.iter().zip(&ir.symbols) {
        assert_eq!(entry.symbol, symbol.id);
        assert_eq!(entry.source.0, 0);
        assert_eq!(entry.span, symbol.span);
    }

    let mut flat = Vec::new();
    let mut owners = Vec::new();
    for pou in &ir.pous {
        let mut pou_nodes = Vec::new();
        nodes(&pou.body, &mut pou_nodes);
        owners.extend(pou_nodes.iter().map(|node| (node.id, pou.symbol)));
        flat.extend(pou_nodes);
    }
    assert_eq!(source_map.nodes.len(), flat.len());
    for ((entry, node), (node_id, pou)) in source_map.nodes.iter().zip(&flat).zip(&owners) {
        assert_eq!(entry.node, node.id);
        assert_eq!(entry.node, *node_id);
        assert_eq!(entry.pou, *pou);
        assert_eq!(entry.source.0, 0);
        assert!(entry.span.start <= entry.span.end);
        assert!(entry.span.end <= source_map.sources[0].byte_length);
    }

    assert_eq!(source_map.fault_sites.len(), ir.fault_sites.len());
    for entry in &source_map.fault_sites {
        let node = flat
            .iter()
            .find(|node| node.id == entry.node)
            .unwrap_or_else(|| unreachable!("every mapped node exists"));
        assert_eq!(node.fault_site, Some(entry.fault_site));
        assert_eq!(entry.source.0, 0);
    }
}

#[test]
fn fault_and_task_tables_are_neither_duplicated_nor_omitted() {
    let (ast, address_model, work_model) = accepted_models();
    let output = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    );
    let ir = output
        .ir
        .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    assert_eq!(ir.tasks.len(), address_model.program_tasks.len());
    assert_eq!(ir.tasks[0].task, TaskHandle(7));
    assert_eq!(ir.fault_sites.len(), address_model.faults.fault_sites.len());
    let mut flat = Vec::new();
    for pou in &ir.pous {
        nodes(&pou.body, &mut flat);
    }
    let references = flat
        .iter()
        .filter_map(|node| node.fault_site)
        .collect::<Vec<_>>();
    assert_eq!(references.len(), ir.fault_sites.len());
    assert_eq!(
        references
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        references.len()
    );
}

#[test]
fn checkpoint_plan_has_exact_required_sites_without_standard_call_or_loop_expansion() {
    let (ast, address_model, work_model) = accepted_models();
    let output = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    );
    let ir = output
        .ir
        .as_ref()
        .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    let source_map = output
        .source_map
        .as_ref()
        .unwrap_or_else(|| unreachable!("successful lowering publishes its source map"));
    let plan = output
        .checkpoint_plan
        .as_ref()
        .unwrap_or_else(|| unreachable!("successful lowering publishes its checkpoint plan"));
    let increment = ir
        .symbols
        .iter()
        .find(|symbol| symbol.name == "Increment")
        .map_or_else(
            || unreachable!("test declares Increment"),
            |symbol| symbol.id,
        );

    assert_eq!(plan.pous.len(), ir.pous.len());
    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(plan.sites.len(), 4);
    assert_eq!(
        plan.sites.iter().map(|site| site.id.0).collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert!(matches!(
        plan.sites[0].site,
        CheckpointSiteKind::BeforePouCall { callee } if callee == increment
    ));
    assert!(matches!(
        plan.sites[1].site,
        CheckpointSiteKind::AfterPouCall { callee } if callee == increment
    ));
    assert_eq!(plan.sites[0].node, plan.sites[1].node);
    assert_eq!(plan.sites[2].site, CheckpointSiteKind::LoopBackEdge);
    assert!(matches!(
        plan.sites[3].site,
        CheckpointSiteKind::TaskReturn {
            task: TaskHandle(7)
        }
    ));
    assert_eq!(plan.tasks[0].return_checkpoint, plan.sites[3].id);

    let pou_site_ids = plan
        .pous
        .iter()
        .flat_map(|pou| pou.checkpoints.iter().map(|site| site.0))
        .collect::<Vec<_>>();
    assert_eq!(pou_site_ids, vec![0, 1, 2]);
    for site in &plan.sites {
        let mapped = source_map
            .nodes
            .get(usize::try_from(site.node.0).unwrap_or(usize::MAX))
            .unwrap_or_else(|| unreachable!("every checkpoint node is mapped"));
        assert_eq!(mapped.node, site.node);
        assert_eq!(mapped.pou, site.pou);
    }
}

#[test]
fn function_block_call_has_exactly_one_before_and_after_checkpoint() {
    const FB_SOURCE: &str = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
FUNCTION_BLOCK Latch
VAR_INPUT
  Value : DINT;
END_VAR
VAR_OUTPUT
  State : DINT;
END_VAR
State := Value;
END_FUNCTION_BLOCK
PROGRAM Main
VAR
  Instance : Latch;
  Result : DINT;
END_VAR
Instance(Value := Counter, State => Result);
END_PROGRAM
";
    let ast = parse("program/main.st", FB_SOURCE.as_bytes(), parser_limits())
        .ast
        .unwrap_or_else(|| unreachable!("function-block source parses"));
    let sources = [SemanticSource::new(&ast, FB_SOURCE)];
    let (address_model, work_model) = accepted_models_for(&sources);
    let output = lower_canonical_ir(
        &sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            generous_source_map_limits(),
            generous_checkpoint_limits(),
        ),
    )
    .unwrap_or_else(|error| unreachable!("accepted project lowers: {error}"));
    let ir = output
        .ir
        .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    let source_map = output
        .source_map
        .unwrap_or_else(|| unreachable!("successful lowering publishes complete Source Map"));
    let plan = output
        .checkpoint_plan
        .unwrap_or_else(|| unreachable!("successful lowering publishes its checkpoint plan"));
    let latch = ir
        .symbols
        .iter()
        .find(|symbol| symbol.name == "Latch")
        .map_or_else(|| unreachable!("test declares Latch"), |symbol| symbol.id);

    assert_eq!(plan.sites.len(), 3);
    assert!(matches!(
        plan.sites[0].site,
        CheckpointSiteKind::BeforePouCall { callee } if callee == latch
    ));
    assert!(matches!(
        plan.sites[1].site,
        CheckpointSiteKind::AfterPouCall { callee } if callee == latch
    ));
    assert!(matches!(
        plan.sites[2].site,
        CheckpointSiteKind::TaskReturn {
            task: TaskHandle(7)
        }
    ));
    assert_reference_function_block_state(&ir, &source_map, &plan);
    aurora_st_ir::compile_linux_x64_aot(
        &ir,
        &source_map,
        &plan,
        AotTarget::linux_x64_v1(),
        AotLimits::new(64, 1024 * 1024, 8 * 1024 * 1024, 4096, 8192, 1024 * 1024)
            .unwrap_or_else(|error| unreachable!("test limits are non-zero: {error}")),
    )
    .unwrap_or_else(|error| unreachable!("function-block project compiles: {error}"));
}

fn assert_reference_function_block_state(
    ir: &aurora_st_ir::CanonicalStIr,
    source_map: &aurora_st_ir::CanonicalSourceMap,
    plan: &aurora_st_ir::CheckpointPlan,
) {
    let instance = symbol_named(ir, "Instance");
    let result = symbol_named(ir, "Result");
    let counter = symbol_named(ir, "Counter");
    let mut reference = ReferenceExecutor::new(
        ir,
        source_map,
        plan,
        ReferenceLimits::new(5, 24, 2, 1, 2)
            .unwrap_or_else(|error| unreachable!("limits are non-zero: {error}")),
    )
    .unwrap_or_else(|error| unreachable!("function-block artifacts execute: {error}"));
    for (cycle, value) in [4_u32, 7].into_iter().enumerate() {
        let observed = reference
            .run_cycle(ReferenceCycleRequest {
                task: TaskHandle(7),
                inputs: &[ReferenceInput {
                    symbol: counter,
                    value_bits: u64::from(value),
                }],
                stop_at_checkpoint: None,
            })
            .unwrap_or_else(|error| unreachable!("function-block cycle executes: {error}"));
        assert_eq!(
            observed.cycle,
            u64::try_from(cycle)
                .unwrap_or_else(|error| unreachable!("cycle index fits u64: {error}"))
        );
        assert_eq!(observed.state.len(), 2, "FB aliases are not extra state");
        assert_eq!(snapshot_u32(&observed.state, result), value);
        let instance_bytes = observed
            .state
            .iter()
            .find(|entry| entry.symbol == instance)
            .map_or_else(
                || unreachable!("snapshot contains Instance"),
                |entry| entry.bytes.as_slice(),
            );
        assert_eq!(
            instance_bytes,
            [value.to_le_bytes(), value.to_le_bytes()].concat()
        );
    }
}

#[test]
fn finite_float_operations_emit_fault_aware_aot() {
    const FLOAT_SOURCE: &str = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
PROGRAM Main
VAR
  Left : REAL;
  Right : REAL;
END_VAR
Left := Left / Right;
END_PROGRAM
";
    let ast = parse("program/main.st", FLOAT_SOURCE.as_bytes(), parser_limits())
        .ast
        .unwrap_or_else(|| unreachable!("float source parses"));
    let sources = [SemanticSource::new(&ast, FLOAT_SOURCE)];
    let (address_model, work_model) = accepted_models_for(&sources);
    let output = lower_canonical_ir(
        &sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            generous_source_map_limits(),
            generous_checkpoint_limits(),
        ),
    )
    .unwrap_or_else(|error| unreachable!("accepted float project lowers: {error}"));
    let artifact = aurora_st_ir::compile_linux_x64_aot(
        output
            .ir
            .as_ref()
            .unwrap_or_else(|| unreachable!("float IR is published")),
        output
            .source_map
            .as_ref()
            .unwrap_or_else(|| unreachable!("float Source Map is published")),
        output
            .checkpoint_plan
            .as_ref()
            .unwrap_or_else(|| unreachable!("float checkpoint plan is published")),
        AotTarget::linux_x64_v1(),
        AotLimits::new(64, 1024 * 1024, 8 * 1024 * 1024, 4096, 8192, 1024 * 1024)
            .unwrap_or_else(|error| unreachable!("test limits are non-zero: {error}")),
    )
    .unwrap_or_else(|error| unreachable!("float project compiles: {error}"));
    assert!(
        artifact
            .runtime_imports
            .iter()
            .any(|entry| entry.symbol == "aurora_st_report_fault_v1")
    );
}

#[test]
fn numeric_boundaries_and_short_circuit_emit_aot_without_identity_conversions() {
    const NUMERIC_SOURCE: &str = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
PROGRAM Main
VAR
  Value : DINT;
  Divisor : DINT;
  Small : USINT;
  Wide : LINT;
  Ratio : REAL;
  Low : REAL;
  High : REAL;
  Flag : BOOL;
END_VAR
Small := TO_USINT(Value);
Small := SATURATING_NEG(Small);
Small := CHECKED_NEG(Small);
Small := MIN(Small, USINT#1);
Small := MAX(Small, USINT#2);
Small := LIMIT(Small, USINT#0, USINT#10);
Wide := TO_LINT(Value);
Ratio := TO_REAL(Value);
Value := TO_DINT(Ratio);
Ratio := MIN(Ratio, Low);
Ratio := MAX(Ratio, High);
Ratio := LIMIT(Ratio, Low, High);
Ratio := ABS(Ratio);
Value := Value MOD Divisor;
Flag := Flag AND_THEN (Value / Divisor = DINT#0);
END_PROGRAM
";
    let artifact = compile_source_aot(NUMERIC_SOURCE);
    assert!(
        artifact
            .runtime_imports
            .iter()
            .any(|entry| entry.symbol == "aurora_st_report_fault_v1")
    );
}

#[test]
fn maximum_ulint_literal_emits_aot() {
    const ULINT_SOURCE: &str = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
PROGRAM Main
VAR
  Bits : ULINT;
END_VAR
Bits := ULINT#18446744073709551615;
END_PROGRAM
";

    let _artifact = compile_source_aot(ULINT_SOURCE);
}

#[test]
fn radix_and_enumeration_literals_emit_aot() {
    const LITERAL_SOURCE: &str = r"AURORA_ST VERSION 1.0;
TYPE
  Mode : (Idle := DINT#0, Run := DINT#1);
END_TYPE
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
PROGRAM Main
VAR
  Bits : ULINT;
  Current : Mode;
END_VAR
Bits := ULINT#16#FFFFFFFFFFFFFFFF;
Bits := Bits XOR ULINT#2#1;
Current := Mode#Run;
END_PROGRAM
";

    let _artifact = compile_source_aot(LITERAL_SOURCE);
}

#[test]
fn string_concat_and_composite_copy_emit_bounded_aot() {
    const AGGREGATE_SOURCE: &str = r"AURORA_ST VERSION 1.0;
TYPE
  Pair : STRUCT
    First : DINT;
    Second : UINT;
  END_STRUCT;
END_TYPE
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
PROGRAM Main
VAR
  Text : STRING[4];
  Copy : STRING[4];
  Left : Pair;
  Right : Pair;
END_VAR
Text := CONCAT(Text, 'x');
Copy := Text;
Left := Right;
END_PROGRAM
";
    let artifact = compile_source_aot(AGGREGATE_SOURCE);
    assert!(
        artifact
            .runtime_imports
            .iter()
            .any(|entry| entry.symbol == "aurora_st_concat_string_v1")
    );
}

#[test]
fn string_function_and_function_block_bindings_emit_complete_aot() {
    const STRING_CALL_SOURCE: &str = r#"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
FUNCTION Echo : STRING[4]
VAR_INPUT
  Value : STRING[4];
END_VAR
RETURN Value;
END_FUNCTION
FUNCTION_BLOCK Latch
VAR_INPUT
  Value : STRING[4];
END_VAR
VAR_OUTPUT
  State : STRING[4];
END_VAR
State := Value;
END_FUNCTION_BLOCK
PROGRAM Main
VAR
  Instance : Latch;
  Text : STRING[4];
  Wide : WSTRING[2];
END_VAR
Text := Echo('x');
Instance(Value := Text, State => Text);
Wide := CONCAT(Wide, "x");
END_PROGRAM
"#;
    let artifact = compile_source_aot(STRING_CALL_SOURCE);
    assert!(
        artifact
            .runtime_imports
            .iter()
            .any(|entry| entry.symbol == "aurora_st_concat_string_v1")
    );
}

#[test]
fn aggregate_stack_budget_accepts_exact_size_and_rejects_one_less() {
    const STRING_LITERAL_SOURCE: &str = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
PROGRAM Main
VAR
  Text : STRING[4];
END_VAR
Text := 'x';
END_PROGRAM
";
    compile_source_aot_with_stack_limit(STRING_LITERAL_SOURCE, 8)
        .unwrap_or_else(|error| unreachable!("eight-byte STRING temporary fits exactly: {error}"));
    assert!(matches!(
        compile_source_aot_with_stack_limit(STRING_LITERAL_SOURCE, 7),
        Err(aurora_st_ir::AotBuildError::CapacityExceeded {
            resource: "transient stack bytes",
            actual: 8,
            limit: 7,
        })
    ));
}

#[test]
fn dynamic_array_access_emits_bounds_fault_before_scalar_read() {
    const ARRAY_SOURCE: &str = r"AURORA_ST VERSION 1.0;
TYPE
  Values : ARRAY[DINT#-1..DINT#1] OF DINT;
END_TYPE
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
PROGRAM Main
VAR
  Data : Values;
  Index : DINT;
END_VAR
Counter := Data[Index];
END_PROGRAM
";
    let ast = parse("program/main.st", ARRAY_SOURCE.as_bytes(), parser_limits())
        .ast
        .unwrap_or_else(|| unreachable!("array source parses"));
    let sources = [SemanticSource::new(&ast, ARRAY_SOURCE)];
    let (address_model, work_model) = accepted_models_for(&sources);
    let output = lower_canonical_ir(
        &sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            generous_source_map_limits(),
            generous_checkpoint_limits(),
        ),
    )
    .unwrap_or_else(|error| unreachable!("accepted array project lowers: {error}"));
    let artifact = aurora_st_ir::compile_linux_x64_aot(
        output
            .ir
            .as_ref()
            .unwrap_or_else(|| unreachable!("array IR is published")),
        output
            .source_map
            .as_ref()
            .unwrap_or_else(|| unreachable!("array Source Map is published")),
        output
            .checkpoint_plan
            .as_ref()
            .unwrap_or_else(|| unreachable!("array checkpoint plan is published")),
        AotTarget::linux_x64_v1(),
        AotLimits::new(64, 1024 * 1024, 8 * 1024 * 1024, 4096, 8192, 1024 * 1024)
            .unwrap_or_else(|error| unreachable!("test limits are non-zero: {error}")),
    )
    .unwrap_or_else(|error| unreachable!("array project compiles: {error}"));
    assert!(
        artifact
            .runtime_imports
            .iter()
            .any(|entry| entry.symbol == "aurora_st_report_fault_v1")
    );
}

#[test]
fn nested_pou_calls_follow_argument_evaluation_order_without_missing_sites() {
    const NESTED_SOURCE: &str = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
FUNCTION Increment : DINT
VAR_INPUT
  Value : DINT;
END_VAR
RETURN CHECKED_ADD(Value, DINT#1);
END_FUNCTION
PROGRAM Main
Counter := Increment(Increment(Counter));
END_PROGRAM
";
    let ast = parse("program/main.st", NESTED_SOURCE.as_bytes(), parser_limits())
        .ast
        .unwrap_or_else(|| unreachable!("nested-call source parses"));
    let sources = [SemanticSource::new(&ast, NESTED_SOURCE)];
    let (address_model, work_model) = accepted_models_for(&sources);
    let output = lower_canonical_ir(
        &sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            generous_source_map_limits(),
            generous_checkpoint_limits(),
        ),
    )
    .unwrap_or_else(|error| unreachable!("accepted project lowers: {error}"));
    let plan = output
        .checkpoint_plan
        .unwrap_or_else(|| unreachable!("successful lowering publishes its checkpoint plan"));

    assert_eq!(plan.sites.len(), 5);
    assert!(matches!(
        plan.sites[0].site,
        CheckpointSiteKind::BeforePouCall { .. }
    ));
    assert!(matches!(
        plan.sites[1].site,
        CheckpointSiteKind::AfterPouCall { .. }
    ));
    assert!(matches!(
        plan.sites[2].site,
        CheckpointSiteKind::BeforePouCall { .. }
    ));
    assert!(matches!(
        plan.sites[3].site,
        CheckpointSiteKind::AfterPouCall { .. }
    ));
    assert_eq!(plan.sites[0].node, plan.sites[1].node);
    assert_eq!(plan.sites[2].node, plan.sites[3].node);
    assert_ne!(plan.sites[0].node, plan.sites[2].node);
    assert!(plan.sites[0].node.0 > plan.sites[2].node.0);
    assert!(matches!(
        plan.sites[4].site,
        CheckpointSiteKind::TaskReturn {
            task: TaskHandle(7)
        }
    ));
}

#[test]
fn multiple_tasks_add_one_return_each_without_duplicating_pou_sites() {
    const MULTI_TASK_SOURCE: &str = r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
FUNCTION Increment : DINT
VAR_INPUT
  Value : DINT;
END_VAR
RETURN CHECKED_ADD(CHECKED_ADD(Value, DINT#1), DINT#1);
END_FUNCTION
PROGRAM Main
VAR
  Index : DINT;
END_VAR
FOR Index := DINT#0 TO DINT#2 BY DINT#1 DO
  Counter := Increment(Counter);
END_FOR;
END_PROGRAM
PROGRAM Backup
END_PROGRAM
";
    let ast = parse(
        "program/main.st",
        MULTI_TASK_SOURCE.as_bytes(),
        parser_limits(),
    )
    .ast
    .unwrap_or_else(|| unreachable!("main source parses"));
    let sources = [SemanticSource::new(&ast, MULTI_TASK_SOURCE)];
    let (address_model, work_model) =
        accepted_models_for_bindings(&sources, &[("Main", 7), ("Backup", 9)]);
    let output = lower_canonical_ir(
        &sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            generous_source_map_limits(),
            generous_checkpoint_limits(),
        ),
    )
    .unwrap_or_else(|error| unreachable!("accepted project lowers: {error}"));
    let plan = output
        .checkpoint_plan
        .unwrap_or_else(|| unreachable!("successful lowering publishes its checkpoint plan"));

    assert_eq!(plan.sites.len(), 5);
    assert_eq!(
        plan.pous
            .iter()
            .map(|pou| pou.checkpoints.len())
            .sum::<usize>(),
        3
    );
    assert_eq!(
        plan.tasks.iter().map(|task| task.task).collect::<Vec<_>>(),
        vec![TaskHandle(7), TaskHandle(9)]
    );
    assert_eq!(
        plan.sites
            .iter()
            .filter(|site| matches!(site.site, CheckpointSiteKind::TaskReturn { .. }))
            .count(),
        2
    );
}

#[test]
fn checkpoint_capacities_accept_exact_counts_and_reject_one_less_atomically() {
    let (ast, address_model, work_model) = accepted_models();
    let exact = lower_with_artifact_limits(
        &ast,
        &address_model,
        &work_model,
        generous_source_map_limits(),
        checkpoint_limits(3, 4, usize::MAX),
        ir_limits(4096, 16, 1024 * 1024),
    );
    assert!(exact.ir.is_some());
    assert!(exact.source_map.is_some());
    assert!(exact.checkpoint_plan.is_some());
    assert!(exact.diagnostics.is_empty());

    for limits in [
        checkpoint_limits(2, 4, usize::MAX),
        checkpoint_limits(3, 3, usize::MAX),
    ] {
        let rejected = lower_with_artifact_limits(
            &ast,
            &address_model,
            &work_model,
            generous_source_map_limits(),
            limits,
            ir_limits(4096, 16, 1024 * 1024),
        );
        assert!(rejected.ir.is_none());
        assert!(rejected.source_map.is_none());
        assert!(rejected.checkpoint_plan.is_none());
        assert_eq!(rejected.diagnostics.len(), 1);
        assert_eq!(
            rejected.diagnostics[0].code,
            DiagnosticCode::ResourceBudgetExceeded
        );
        assert_eq!(rejected.diagnostics[0].source_path, "program/main.st");
    }
}

#[test]
fn node_capacity_accepts_exact_count_and_rejects_one_less_atomically() {
    let (ast, address_model, work_model) = accepted_models();
    let baseline = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    );
    let count = baseline.ir.as_ref().map_or(0, |ir| {
        ir.pous.iter().map(|pou| ir_node_count(&pou.body)).sum()
    });
    assert!(
        lower(
            &ast,
            &address_model,
            &work_model,
            ir_limits(count, 16, 1024 * 1024),
        )
        .diagnostics
        .is_empty()
    );
    let rejected = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(count.saturating_sub(1), 16, 1024 * 1024),
    );
    assert!(rejected.ir.is_none());
    assert!(rejected.source_map.is_none());
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(
        rejected.diagnostics[0].code,
        DiagnosticCode::ResourceBudgetExceeded
    );
    assert_eq!(rejected.diagnostics[0].source_path, "program/main.st");
}

#[test]
fn pou_capacity_accepts_exact_count_and_rejects_one_less_atomically() {
    let (ast, address_model, work_model) = accepted_models();
    assert!(
        lower(
            &ast,
            &address_model,
            &work_model,
            ir_limits(4096, 2, 1024 * 1024),
        )
        .diagnostics
        .is_empty()
    );
    let rejected = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 1, 1024 * 1024),
    );
    assert!(rejected.ir.is_none());
    assert!(rejected.source_map.is_none());
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(
        rejected.diagnostics[0].code,
        DiagnosticCode::ResourceBudgetExceeded
    );
}

#[test]
fn canonical_json_is_stable_and_has_an_exact_byte_boundary() {
    let (ast, address_model, work_model) = accepted_models();
    let ir = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, usize::MAX),
    )
    .ir
    .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    let unlimited = ir_limits(4096, 16, usize::MAX);
    let first = canonical_ir_to_json(&ir, unlimited)
        .unwrap_or_else(|error| unreachable!("valid IR serializes: {error}"));
    let second = canonical_ir_to_json(&ir, unlimited)
        .unwrap_or_else(|error| unreachable!("valid IR serializes: {error}"));
    assert_eq!(first, second);
    let exact = canonical_ir_to_json(&ir, ir_limits(4096, 16, first.len()))
        .unwrap_or_else(|error| unreachable!("exact byte boundary is accepted: {error}"));
    assert_eq!(exact, first);
    assert!(matches!(
        canonical_ir_to_json(&ir, ir_limits(4096, 16, first.len().saturating_sub(1))),
        Err(CanonicalIrSerializationError::EncodedSizeExceeded { .. })
    ));
}

#[test]
fn source_map_capacities_accept_exact_counts_and_reject_one_less_atomically() {
    let (ast, address_model, work_model) = accepted_models();
    let baseline = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    );
    let map = baseline
        .source_map
        .as_ref()
        .unwrap_or_else(|| unreachable!("baseline publishes a source map"));
    let counts = (
        map.sources.len(),
        map.symbols.len(),
        map.nodes.len(),
        map.fault_sites.len(),
    );
    assert!(counts.1 > 1 && counts.2 > 1 && counts.3 > 1);

    let exact = lower_with_source_map(
        &ast,
        &address_model,
        &work_model,
        source_map_limits(counts.0, counts.1, counts.2, counts.3, usize::MAX),
        ir_limits(4096, 16, 1024 * 1024),
    );
    assert!(exact.ir.is_some());
    assert!(exact.source_map.is_some());
    assert!(exact.diagnostics.is_empty());

    for limits in [
        source_map_limits(counts.0, counts.1 - 1, counts.2, counts.3, usize::MAX),
        source_map_limits(counts.0, counts.1, counts.2 - 1, counts.3, usize::MAX),
        source_map_limits(counts.0, counts.1, counts.2, counts.3 - 1, usize::MAX),
    ] {
        let rejected = lower_with_source_map(
            &ast,
            &address_model,
            &work_model,
            limits,
            ir_limits(4096, 16, 1024 * 1024),
        );
        assert!(rejected.ir.is_none());
        assert!(rejected.source_map.is_none());
        assert_eq!(rejected.diagnostics.len(), 1);
        assert_eq!(
            rejected.diagnostics[0].code,
            DiagnosticCode::ResourceBudgetExceeded
        );
    }
}

#[test]
fn source_file_capacity_counts_non_executable_inputs_without_omission() {
    const TYPES_SOURCE: &str = "AURORA_ST VERSION 1.0;\nTYPE Auxiliary : DINT; END_TYPE\n";
    let main_ast = parse("program/main.st", source().as_bytes(), parser_limits())
        .ast
        .unwrap_or_else(|| unreachable!("main source parses"));
    let types_ast = parse(
        "types/auxiliary.st",
        TYPES_SOURCE.as_bytes(),
        parser_limits(),
    )
    .ast
    .unwrap_or_else(|| unreachable!("type source parses"));
    let sources = [
        SemanticSource::new(&main_ast, source()),
        SemanticSource::new(&types_ast, TYPES_SOURCE),
    ];
    let (address_model, work_model) = accepted_models_for(&sources);

    let accepted = lower_canonical_ir(
        &sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            source_map_limits(2, 4096, 4096, 4096, 1024 * 1024),
            generous_checkpoint_limits(),
        ),
    )
    .unwrap_or_else(|error| unreachable!("accepted models lower: {error}"));
    let map = accepted
        .source_map
        .unwrap_or_else(|| unreachable!("accepted build publishes a source map"));
    assert_eq!(
        map.sources
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        ["program/main.st", "types/auxiliary.st"]
    );

    let reversed_sources = [sources[1], sources[0]];
    let reordered = lower_canonical_ir(
        &reversed_sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            source_map_limits(2, 4096, 4096, 4096, 1024 * 1024),
            generous_checkpoint_limits(),
        ),
    )
    .unwrap_or_else(|error| unreachable!("source order does not change lowering: {error}"));
    assert_eq!(reordered.ir, accepted.ir);
    assert_eq!(reordered.source_map.as_ref(), Some(&map));
    assert_eq!(reordered.checkpoint_plan, accepted.checkpoint_plan);

    let rejected = lower_canonical_ir(
        &sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            source_map_limits(1, 4096, 4096, 4096, 1024 * 1024),
            generous_checkpoint_limits(),
        ),
    )
    .unwrap_or_else(|error| unreachable!("capacity crossing is a diagnostic: {error}"));
    assert!(rejected.ir.is_none());
    assert!(rejected.source_map.is_none());
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(
        rejected.diagnostics[0].code,
        DiagnosticCode::ResourceBudgetExceeded
    );
    assert_eq!(rejected.diagnostics[0].source_path, "types/auxiliary.st");
}

#[test]
fn canonical_source_map_json_is_stable_versioned_and_exactly_bounded() {
    let (ast, address_model, work_model) = accepted_models();
    let mut map = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    )
    .source_map
    .unwrap_or_else(|| unreachable!("successful lowering publishes a source map"));
    let unlimited = source_map_limits(16, 4096, 4096, 4096, usize::MAX);
    let first = canonical_source_map_to_json(&map, unlimited)
        .unwrap_or_else(|error| unreachable!("valid source map serializes: {error}"));
    let second = canonical_source_map_to_json(&map, unlimited)
        .unwrap_or_else(|error| unreachable!("valid source map serializes: {error}"));
    assert_eq!(first, second);
    let exact =
        canonical_source_map_to_json(&map, source_map_limits(16, 4096, 4096, 4096, first.len()))
            .unwrap_or_else(|error| unreachable!("exact byte boundary is accepted: {error}"));
    assert_eq!(exact, first);
    assert!(matches!(
        canonical_source_map_to_json(
            &map,
            source_map_limits(16, 4096, 4096, 4096, first.len().saturating_sub(1)),
        ),
        Err(CanonicalSourceMapSerializationError::EncodedSizeExceeded { .. })
    ));

    map.schema_version.minor = map.schema_version.minor.saturating_add(1);
    assert!(matches!(
        canonical_source_map_to_json(&map, unlimited),
        Err(CanonicalSourceMapSerializationError::UnsupportedVersion { .. })
    ));
}

#[test]
fn checkpoint_plan_json_is_stable_versioned_and_exactly_bounded() {
    let (ast, address_model, work_model) = accepted_models();
    let mut plan = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    )
    .checkpoint_plan
    .unwrap_or_else(|| unreachable!("successful lowering publishes its checkpoint plan"));
    let unlimited = checkpoint_limits(4096, 4096, usize::MAX);
    let first = checkpoint_plan_to_json(&plan, unlimited)
        .unwrap_or_else(|error| unreachable!("valid checkpoint plan serializes: {error}"));
    let second = checkpoint_plan_to_json(&plan, unlimited)
        .unwrap_or_else(|error| unreachable!("valid checkpoint plan serializes: {error}"));
    assert_eq!(first, second);
    let exact = checkpoint_plan_to_json(&plan, checkpoint_limits(4096, 4096, first.len()))
        .unwrap_or_else(|error| unreachable!("exact byte boundary is accepted: {error}"));
    assert_eq!(exact, first);
    assert!(matches!(
        checkpoint_plan_to_json(
            &plan,
            checkpoint_limits(4096, 4096, first.len().saturating_sub(1)),
        ),
        Err(CheckpointPlanSerializationError::EncodedSizeExceeded { .. })
    ));

    plan.schema_version.minor = plan.schema_version.minor.saturating_add(1);
    assert!(matches!(
        checkpoint_plan_to_json(&plan, unlimited),
        Err(CheckpointPlanSerializationError::UnsupportedVersion { .. })
    ));
}

#[test]
fn serializer_rejects_an_unknown_writer_version_without_bytes() {
    let (ast, address_model, work_model) = accepted_models();
    let mut ir = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, usize::MAX),
    )
    .ir
    .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    ir.schema_version.minor = ir.schema_version.minor.saturating_add(1);
    assert!(matches!(
        canonical_ir_to_json(&ir, ir_limits(4096, 16, usize::MAX)),
        Err(CanonicalIrSerializationError::UnsupportedVersion { .. })
    ));
}

#[test]
fn mismatched_work_model_is_rejected_before_any_ir_is_published() {
    let (ast, address_model, mut work_model) = accepted_models();
    work_model.tasks.clear();
    let result = lower_canonical_ir(
        &[SemanticSource::new(&ast, source())],
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            generous_source_map_limits(),
            generous_checkpoint_limits(),
        ),
    );
    assert_eq!(result.err(), Some(CanonicalIrInputError::WorkModelMismatch));
}

#[test]
fn every_zero_limit_is_rejected() {
    assert!(CanonicalIrLimits::new(0, 1, 1).is_err());
    assert!(CanonicalIrLimits::new(1, 0, 1).is_err());
    assert!(CanonicalIrLimits::new(1, 1, 0).is_err());
    assert!(CanonicalSourceMapLimits::new(0, 1, 1, 1, 1).is_err());
    assert!(CanonicalSourceMapLimits::new(1, 0, 1, 1, 1).is_err());
    assert!(CanonicalSourceMapLimits::new(1, 1, 0, 1, 1).is_err());
    assert!(CanonicalSourceMapLimits::new(1, 1, 1, 0, 1).is_err());
    assert!(CanonicalSourceMapLimits::new(1, 1, 1, 1, 0).is_err());
    assert!(CheckpointPlanLimits::new(0, 1, 1).is_err());
    assert!(CheckpointPlanLimits::new(1, 0, 1).is_err());
    assert!(CheckpointPlanLimits::new(1, 1, 0).is_err());
}
