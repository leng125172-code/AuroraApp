//! R2-07 黄金 Graph、输入 Trace、单线程并行、Fault 原子性与 CLI replay 验收。

use std::cell::Cell;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, ThreadId};

use aurora_control_contracts::{
    ExecutionBudgetNanos, ExecutionContractVersion, FaultReason, HardLimitNanos, MissPolicy,
    MissWindow, RelativeDeadlineNanos, TaskPeriodNanos, TaskPhaseNanos, TaskPriority, TaskSpec,
    TaskTiming, TraceCapacity, WorkflowTraceEventKind, WorkflowTraceFileHeader,
    WorkflowTraceRecord, WorkflowTraceRecordBytes,
};
use aurora_control_engine::{
    CycleStart, MonotonicClock, ScheduleAction, ScheduleControl, SpscPopError, StaticTaskPlan,
    StaticTaskPlanBuilder, TaskTransaction, WorkSetCapacity, WorkSetIndex, WorkSetLimits,
    WorkflowTraceObserveError, bounded_workflow_trace_channel,
};
use aurora_types::{BootEpochId, LocalHandle, MonotonicTimestamp};
use aurora_workflow_cyclic::{
    StructuredBranchDefinition, StructuredBranchHandle, StructuredBranchMembership,
    StructuredBranchRange, StructuredEdgeDefinition, StructuredEdgeTarget,
    StructuredForkDefinition, StructuredForkHandle, StructuredInstanceDefinition,
    StructuredInstanceHandle, StructuredJoinMode, StructuredJoinPolicy, StructuredNodeDefinition,
    StructuredNodeExecutionError, StructuredNodeExecutor, StructuredNodeKind,
    StructuredNodeOutcome, StructuredOutputTrace, StructuredScanError,
    StructuredWorkflowDefinition, StructuredWorkflowRuntime,
    WorkflowEdgeHandle as RuntimeEdgeHandle, WorkflowEdgeRange, WorkflowNodeContext,
    WorkflowNodeHandle as RuntimeNodeHandle, WorkflowTraceRecorder, stage_simulated_release,
};
use aurora_workflow_graph::{
    ExpandedActionBindingInput, ExpandedNodeResourceInput, StableId, TaskBindingImageInput,
    TaskWorkflowPlanningInput, WorkflowActionKind, WorkflowActionPortBinding,
    WorkflowArtifactLimits, WorkflowBindingVersion, WorkflowPlanArtifacts, WorkflowPlanInputError,
    WorkflowPortDirection, WorkflowSource, WorkflowTargetLimitValues, WorkflowTargetLimits,
    WorkflowValidationLimits, WorkflowValueArea, WorkflowValueSlot, WorkflowValueType,
    WorkflowWriteRegion, YamlSourceLimits, compile_traced_workflow_plan,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const GOLDEN_GRAPH: &[u8] = include_bytes!(
    "../../../../Contracts/workflow/v1/examples/join-any.valid.aurora-workflow.yaml"
);
const INPUT_TRACE: &[u8] = include_bytes!("fixtures/r2-gate/input-trace.json");
const EXPECTED_EVIDENCE: &[u8] = include_bytes!("fixtures/r2-gate/expected-evidence.json");
const TASK_HANDLE: u32 = 0;
const OUTPUT_BYTES: usize = 8;
const APPLICATION_STATE_BYTES: usize = 2;
const TRACE_CHANNEL_CAPACITY: u32 = 256;
const GOLDEN_FIXTURES: [&str; 2] = ["expected-evidence.json", "input-trace.json"];

static TEMP_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputTrace {
    schema_version: InputTraceVersion,
    scenarios: Vec<InputScenario>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputTraceVersion {
    major: u32,
    minor: u32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InputScenario {
    name: String,
    releases: Vec<ReleaseInput>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseInput {
    left_output: i32,
    right_output: i32,
    right_retain: bool,
    right_fault: bool,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct GateEvidence {
    artifact_counts: ArtifactCounts,
    plan_digest: String,
    scenarios: Vec<ScenarioEvidence>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct ArtifactCounts {
    instances: usize,
    steps: usize,
    edges: usize,
    node_resources: usize,
    conditions: usize,
    trace_values: usize,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct ScenarioEvidence {
    name: String,
    trace_sha256: String,
    record_count: usize,
    callbacks: Vec<u32>,
    releases: Vec<ReleaseEvidence>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct ReleaseEvidence {
    release: u64,
    terminal: String,
    committed_output: Vec<u8>,
    events: Vec<EventEvidence>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
struct EventEvidence {
    kind: &'static str,
    detail: u16,
    node: Option<u32>,
    edge: Option<u32>,
    source: Option<u32>,
    value: Option<u32>,
    branch: Option<u32>,
    fault: Option<String>,
    bytes: Vec<u8>,
}

struct SimulationOutput {
    evidence: ScenarioEvidence,
    trace_file: Vec<u8>,
}

struct Clock(Cell<MonotonicTimestamp>);

impl MonotonicClock for Clock {
    fn now(&self) -> MonotonicTimestamp {
        self.0.get()
    }
}

struct GoldenExecutor<'input> {
    input: &'input ReleaseInput,
    owner: ThreadId,
    wrong_thread: bool,
    callbacks: &'input mut Vec<u32>,
}

impl StructuredNodeExecutor for GoldenExecutor<'_> {
    fn execute(
        &mut self,
        _node: RuntimeNodeHandle,
        _context: &mut WorkflowNodeContext<'_, '_, '_>,
    ) -> Result<StructuredNodeOutcome, FaultReason> {
        Err(FaultReason::TaskExecutionFault)
    }

    fn execute_traced(
        &mut self,
        node: RuntimeNodeHandle,
        context: &mut WorkflowNodeContext<'_, '_, '_>,
        trace: &mut dyn StructuredOutputTrace,
    ) -> Result<StructuredNodeOutcome, StructuredNodeExecutionError> {
        self.wrong_thread |= thread::current().id() != self.owner;
        self.callbacks.push(node.get());
        match node.get() {
            1 => {
                stage_i32_output(context, trace, 0, 0, 0, self.input.left_output)?;
                Ok(StructuredNodeOutcome::Take(
                    RuntimeEdgeHandle::new(2).map_err(|_| {
                        StructuredNodeExecutionError::Fault(FaultReason::TaskExecutionFault)
                    })?,
                ))
            }
            2 if self.input.right_fault => Err(StructuredNodeExecutionError::Fault(
                FaultReason::TaskExecutionFault,
            )),
            2 => {
                stage_i32_output(context, trace, 1, 1, 4, self.input.right_output)?;
                if self.input.right_retain {
                    Ok(StructuredNodeOutcome::Retain)
                } else {
                    Ok(StructuredNodeOutcome::Take(
                        RuntimeEdgeHandle::new(3).map_err(|_| {
                            StructuredNodeExecutionError::Fault(FaultReason::TaskExecutionFault)
                        })?,
                    ))
                }
            }
            _ => Err(StructuredNodeExecutionError::Fault(
                FaultReason::TaskExecutionFault,
            )),
        }
    }
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn create() -> TestResult<Self> {
        let sequence = TEMP_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("aurora-r2-gate-{}-{sequence}", std::process::id()));
        fs::create_dir(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _cleanup_result = fs::remove_dir_all(&self.0);
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "R2 Gate 将黄金 Graph、两种输入场景、逐周期证据与真实 CLI replay 保持在一个原子验收中"
)]
fn golden_graph_and_input_trace_are_exact_deterministic_and_replayable() -> TestResult {
    assert_fixture_inventory()?;
    let input: InputTrace = serde_json::from_slice(INPUT_TRACE)?;
    assert_eq!(
        (input.schema_version.major, input.schema_version.minor),
        (1, 0)
    );
    assert_eq!(
        input
            .scenarios
            .iter()
            .map(|scenario| scenario.name.as_str())
            .collect::<Vec<_>>(),
        ["parallel_complete", "later_branch_fault"]
    );

    let artifacts = compile_golden(target_limits(target_values())?)?;
    let counts = ArtifactCounts {
        instances: artifacts.static_plan.instances.len(),
        steps: artifacts.static_plan.steps.len(),
        edges: artifacts.static_plan.edges.len(),
        node_resources: artifacts.static_plan.node_resources.len(),
        conditions: artifacts.static_plan.condition_bindings.len(),
        trace_values: artifacts.static_plan.trace_values.len(),
    };
    assert_eq!(
        counts,
        ArtifactCounts {
            instances: 1,
            steps: 4,
            edges: 6,
            node_resources: 2,
            conditions: 0,
            trace_values: 2,
        }
    );

    let mut scenarios = Vec::with_capacity(input.scenarios.len());
    let mut complete_files = None;
    for scenario in &input.scenarios {
        let first = simulate(scenario, &artifacts)?;
        let second = simulate(scenario, &artifacts)?;
        assert_eq!(first.evidence, second.evidence);
        assert_eq!(first.trace_file, second.trace_file);
        assert_eq!(first.evidence.releases.len(), scenario.releases.len());
        if scenario.name == "parallel_complete" {
            complete_files = Some((first.trace_file.clone(), artifacts.static_plan_json.clone()));
        }
        scenarios.push(first.evidence);
    }

    let actual = GateEvidence {
        artifact_counts: counts,
        plan_digest: artifacts.plan_digest.clone(),
        scenarios,
    };
    let expected: serde_json::Value = serde_json::from_slice(EXPECTED_EVIDENCE)?;
    assert_eq!(serde_json::to_value(actual)?, expected);

    let (trace_file, plan_file) = complete_files.ok_or("complete scenario was not executed")?;
    assert_cli_replay(&trace_file, &plan_file)?;
    Ok(())
}

#[test]
fn golden_graph_resource_limits_and_resource_closure_are_atomic() -> TestResult {
    let baseline = compile_golden(target_limits(target_values())?)?;
    let proof = baseline
        .static_plan
        .resources
        .tasks
        .first()
        .ok_or("golden task resource proof is missing")?;
    let mut exact = target_values();
    exact.max_workflows_per_task = proof.root_workflows;
    exact.max_source_nodes_per_workflow = 6;
    exact.max_source_edges_per_workflow = 6;
    exact.max_expanded_workflow_instances = proof.expanded_workflow_instances;
    exact.max_expanded_nodes_per_task = proof.expanded_nodes;
    exact.max_expanded_edges_per_task = proof.expanded_edges;
    exact.max_active_nodes_per_task = proof.active_nodes;
    exact.max_node_executions_per_release = proof.node_executions_per_release;
    exact.max_fork_nesting_depth = proof.fork_nesting_depth.max(1);
    exact.max_branches_per_fork = proof.branches_per_fork.max(1);
    exact.max_pending_cancellations = proof.pending_cancellations.max(1);
    exact.max_subworkflow_expansion_depth = proof.subworkflow_expansion_depth.max(1);
    exact.max_backedge_traversals_per_run = proof.backedge_traversals_per_run.max(1);
    exact.max_wait_cycles = proof.wait_cycles.max(1);
    exact.max_workflow_state_bytes_per_task = proof.workflow_state_bytes.max(1);
    exact.max_workflow_staging_bytes_per_task = proof.workflow_staging_bytes.max(1);
    exact.max_watch_handles_per_task = proof.watch_handles.max(1);
    exact.max_action_ports_per_node = 1;
    exact.max_condition_bindings_per_task = 1;
    exact.max_trace_events_per_release = proof.trace_events_per_release;
    exact.workflow_trace_ring_capacity = proof.trace_ring_capacity;
    assert!(compile_golden(target_limits(exact)?).is_ok());

    let mut one_less_node = exact;
    one_less_node.max_expanded_nodes_per_task = proof
        .expanded_nodes
        .checked_sub(1)
        .ok_or("expanded node proof unexpectedly zero")?;
    assert_no_artifacts(target_limits(one_less_node)?)?;

    let mut one_less_trace = exact;
    one_less_trace.max_trace_events_per_release = proof
        .trace_events_per_release
        .checked_sub(1)
        .ok_or("trace event proof unexpectedly zero")?;
    assert_no_artifacts(target_limits(one_less_trace)?)?;

    let claims = action_claims()?;
    for (invalid, expected_error) in [
        (
            vec![claims[0].clone()],
            WorkflowPlanInputError::InvalidResourceClaim,
        ),
        (
            vec![claims[0].clone(), claims[0].clone()],
            WorkflowPlanInputError::InvalidActionBinding,
        ),
    ] {
        let result = compile_with_inputs(&invalid, target_limits(target_values())?);
        assert_eq!(result, Err(expected_error));
    }
    Ok(())
}

fn assert_fixture_inventory() -> TestResult {
    let fixture_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/r2-gate");
    let mut actual = fs::read_dir(fixture_dir)?
        .map(|entry| {
            let entry = entry?;
            entry
                .file_name()
                .into_string()
                .map_err(|_| std::io::Error::other("R2 Gate fixture name is not UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    actual.sort();
    assert_eq!(actual, GOLDEN_FIXTURES);
    Ok(())
}

fn id(value: &str) -> TestResult<StableId> {
    StableId::parse(value).ok_or_else(|| "test UUID is not canonical v7".into())
}

fn validation_limits() -> TestResult<WorkflowValidationLimits> {
    let yaml = YamlSourceLimits::new(128 * 1024, 32, 32, 512, 128 * 1024)?;
    Ok(WorkflowValidationLimits::new(
        yaml, yaml, 16, 128, 256, 16, 128, 256, 32, 512,
    )?)
}

fn target_values() -> WorkflowTargetLimitValues {
    WorkflowTargetLimitValues {
        max_workflows_per_task: 8,
        max_source_nodes_per_workflow: 128,
        max_source_edges_per_workflow: 256,
        max_expanded_workflow_instances: 32,
        max_expanded_nodes_per_task: 1024,
        max_expanded_edges_per_task: 2048,
        max_active_nodes_per_task: 512,
        max_node_executions_per_release: 512,
        max_fork_nesting_depth: 64,
        max_branches_per_fork: 64,
        max_pending_cancellations: 64,
        max_subworkflow_expansion_depth: 16,
        max_backedge_traversals_per_run: 1024,
        max_wait_cycles: 1024,
        max_workflow_state_bytes_per_task: 1024 * 1024,
        max_workflow_staging_bytes_per_task: 1024 * 1024,
        max_watch_handles_per_task: 128,
        max_action_ports_per_node: 1,
        max_condition_bindings_per_task: 1,
        max_trace_events_per_release: 4096,
        workflow_trace_ring_capacity: 4096,
    }
}

fn target_limits(values: WorkflowTargetLimitValues) -> TestResult<WorkflowTargetLimits> {
    Ok(WorkflowTargetLimits::new(values)?)
}

fn task_input() -> TestResult<TaskWorkflowPlanningInput> {
    Ok(TaskWorkflowPlanningInput {
        task_handle: TASK_HANDLE,
        root_workflow_ids: vec![id("018f0000-0000-7000-8000-000000000121")?],
        watches: Vec::new(),
        trace_ring_capacity: 64,
    })
}

fn action_claims() -> TestResult<Vec<ExpandedNodeResourceInput>> {
    let root = id("018f0000-0000-7000-8000-000000000121")?;
    Ok(vec![
        action_claim(
            root,
            id("018f0000-0000-7000-8000-000000000124")?,
            id("018f0000-0000-7000-8000-000000000481")?,
            id("018f0000-0000-7000-8000-000000000483")?,
            0,
            0,
            11,
        ),
        action_claim(
            root,
            id("018f0000-0000-7000-8000-000000000125")?,
            id("018f0000-0000-7000-8000-000000000482")?,
            id("018f0000-0000-7000-8000-000000000484")?,
            1,
            4,
            12,
        ),
    ])
}

#[allow(clippy::too_many_arguments)]
fn action_claim(
    root: StableId,
    node_id: StableId,
    target_id: StableId,
    binding_id: StableId,
    state_offset: u64,
    output_offset: u64,
    target_handle: u32,
) -> ExpandedNodeResourceInput {
    let slot = WorkflowValueSlot {
        target_id,
        area: WorkflowValueArea::Output,
        offset_bytes: 0,
        image_offset_bytes: output_offset,
        value_type: WorkflowValueType::Dint,
    };
    ExpandedNodeResourceInput {
        task_handle: TASK_HANDLE,
        instance_path: vec![root],
        node_id,
        committed_state_bytes: 1,
        staging_state_bytes: 1,
        trace_events_per_release: 0,
        writes: vec![WorkflowWriteRegion {
            target_id,
            offset_bytes: 0,
            size_bytes: 4,
        }],
        action_binding: Some(ExpandedActionBindingInput {
            version: WorkflowBindingVersion::V1_0,
            binding_id,
            kind: WorkflowActionKind::StPou,
            target_handle,
            invocation_state_offset_bytes: state_offset,
            ports: vec![WorkflowActionPortBinding {
                port: 0,
                direction: WorkflowPortDirection::Output,
                slot,
            }],
            committed_state_bytes: 1,
            staging_state_bytes: 1,
            trace_events_per_release: 0,
        }),
        subworkflow_binding: None,
    }
}

fn compile_golden(limits: WorkflowTargetLimits) -> TestResult<WorkflowPlanArtifacts> {
    let output = compile_with_inputs(&action_claims()?, limits)?;
    if !output.diagnostics.is_empty() {
        return Err(format!("golden Graph diagnostics: {:?}", output.diagnostics).into());
    }
    output
        .artifacts
        .ok_or_else(|| "golden Graph did not publish artifacts".into())
}

fn compile_with_inputs(
    claims: &[ExpandedNodeResourceInput],
    limits: WorkflowTargetLimits,
) -> Result<aurora_workflow_graph::WorkflowPlanOutput, WorkflowPlanInputError> {
    let tasks = [task_input().map_err(|_| WorkflowPlanInputError::EmptyTaskSet)?];
    let images = [TaskBindingImageInput {
        task_handle: TASK_HANDLE,
        application_state_bytes: APPLICATION_STATE_BYTES as u64,
        output_bytes: OUTPUT_BYTES as u64,
    }];
    compile_traced_workflow_plan(
        &[WorkflowSource {
            source_path: "join-any.valid.aurora-workflow.yaml",
            source_bytes: GOLDEN_GRAPH,
        }],
        validation_limits().map_err(|_| WorkflowPlanInputError::EmptyTaskSet)?,
        &tasks,
        claims,
        &[],
        &images,
        &[],
        limits,
        WorkflowArtifactLimits::new(1024 * 1024, 1024 * 1024)
            .map_err(|_| WorkflowPlanInputError::EmptyTaskSet)?,
    )
}

fn assert_no_artifacts(limits: WorkflowTargetLimits) -> TestResult {
    let output = compile_with_inputs(&action_claims()?, limits)?;
    assert!(output.artifacts.is_none());
    assert_eq!(output.diagnostics.len(), 1);
    Ok(())
}

fn structured_runtime() -> TestResult<StructuredWorkflowRuntime> {
    let nodes = [
        runtime_node(
            0,
            0,
            2,
            StructuredNodeKind::Fork(StructuredForkHandle(0)),
            false,
        )?,
        runtime_node(1, 2, 1, StructuredNodeKind::Action, true)?,
        runtime_node(2, 3, 1, StructuredNodeKind::Action, true)?,
        runtime_node(
            3,
            4,
            1,
            StructuredNodeKind::Join {
                fork: Some(StructuredForkHandle(0)),
                mode: StructuredJoinMode::Any(StructuredJoinPolicy::WaitAtBoundary),
            },
            false,
        )?,
    ];
    let edges = [
        runtime_control_edge(0, 0, Some(1), Some(0))?,
        runtime_control_edge(1, 0, Some(2), Some(1))?,
        runtime_control_edge(2, 1, Some(3), Some(0))?,
        runtime_control_edge(3, 2, Some(3), Some(1))?,
        runtime_control_edge(4, 3, None, None)?,
    ];
    let initial = [runtime_node_handle(0)?];
    let forks = [StructuredForkDefinition {
        handle: StructuredForkHandle(0),
        node: runtime_node_handle(0)?,
        branches: StructuredBranchRange { start: 0, count: 2 },
    }];
    let branches = [
        StructuredBranchDefinition {
            handle: StructuredBranchHandle(0),
            fork: StructuredForkHandle(0),
            branch_order: 0,
            activation_edge: runtime_edge(0)?,
        },
        StructuredBranchDefinition {
            handle: StructuredBranchHandle(1),
            fork: StructuredForkHandle(0),
            branch_order: 1,
            activation_edge: runtime_edge(1)?,
        },
    ];
    let memberships = [
        StructuredBranchMembership {
            node: runtime_node_handle(1)?,
            branch: StructuredBranchHandle(0),
        },
        StructuredBranchMembership {
            node: runtime_node_handle(2)?,
            branch: StructuredBranchHandle(1),
        },
    ];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    Ok(StructuredWorkflowRuntime::new(
        StructuredWorkflowDefinition {
            task_handle: LocalHandle::ZERO,
            nodes: &nodes,
            edges: &edges,
            initial_active: &initial,
            forks: &forks,
            branches: &branches,
            memberships: &memberships,
            instances: &instances,
            calls: &[],
            call_initial_nodes: &[],
            state_copies: &[],
            maximum_active_nodes: 4,
            maximum_node_executions: 4,
            maximum_pending_cancellations: 1,
            application_state_bytes: APPLICATION_STATE_BYTES,
            output_bytes: OUTPUT_BYTES,
        },
    )?)
}

fn runtime_node(
    handle: u32,
    edge_start: u32,
    edge_count: u32,
    kind: StructuredNodeKind,
    cancellation_boundary: bool,
) -> TestResult<StructuredNodeDefinition> {
    Ok(StructuredNodeDefinition {
        handle: runtime_node_handle(handle)?,
        instance: StructuredInstanceHandle(0),
        kind,
        outgoing: WorkflowEdgeRange {
            start: edge_start,
            count: edge_count,
        },
        cancellation_boundary,
    })
}

fn runtime_control_edge(
    handle: u32,
    source: u32,
    target: Option<u32>,
    branch: Option<u32>,
) -> TestResult<StructuredEdgeDefinition> {
    Ok(StructuredEdgeDefinition {
        handle: runtime_edge(handle)?,
        source: runtime_node_handle(source)?,
        target: match target {
            Some(target) => StructuredEdgeTarget::Node(runtime_node_handle(target)?),
            None => StructuredEdgeTarget::Complete,
        },
        branch: branch.map(StructuredBranchHandle),
        maximum_traversals_per_run: None,
    })
}

fn runtime_node_handle(value: u32) -> TestResult<RuntimeNodeHandle> {
    Ok(RuntimeNodeHandle::new(value)?)
}

fn runtime_edge(value: u32) -> TestResult<RuntimeEdgeHandle> {
    Ok(RuntimeEdgeHandle::new(value)?)
}

fn epoch() -> TestResult<BootEpochId> {
    Ok(BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x44,
    ])?)
}

fn setup(
    runtime: &StructuredWorkflowRuntime,
) -> TestResult<(TaskTransaction, StaticTaskPlan, Clock)> {
    let engine_epoch = epoch()?;
    let spec = TaskSpec::new(
        ExecutionContractVersion::V1_0,
        LocalHandle::ZERO,
        TaskPriority::new(0),
        TaskTiming::new(
            TaskPeriodNanos::new(10)?,
            TaskPhaseNanos::new(3),
            RelativeDeadlineNanos::new(8)?,
            ExecutionBudgetNanos::new(2)?,
            HardLimitNanos::new(5)?,
        )?,
        MissPolicy::new(MissWindow::new(4, 4)?, 2, 3)?,
    );
    let limits = WorkSetLimits::new(WorkSetCapacity::new(128)?, 32_768);
    let mut builder = StaticTaskPlanBuilder::new(
        MonotonicTimestamp::new(engine_epoch, 0),
        WorkSetCapacity::new(1)?,
        limits,
    )?;
    builder.add_task(spec)?;
    let mut state = runtime.initial_control_state().to_vec();
    state.resize(runtime.control_state_bytes() + APPLICATION_STATE_BYTES, 0);
    Ok((
        TaskTransaction::new(spec, engine_epoch, &state, &[0; OUTPUT_BYTES], limits)?,
        builder.seal()?,
        Clock(Cell::new(MonotonicTimestamp::new(engine_epoch, 3))),
    ))
}

#[allow(
    clippy::too_many_lines,
    reason = "黄金场景必须把每个 input 到唯一 release、terminal、commit 输出和 Trace 证据保持在同一验收路径"
)]
fn simulate(
    scenario: &InputScenario,
    artifacts: &WorkflowPlanArtifacts,
) -> TestResult<SimulationOutput> {
    if scenario.releases.is_empty() {
        return Err("input scenario must contain a release".into());
    }
    let mut runtime = structured_runtime()?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let maximum_events = u32::try_from(
        artifacts
            .static_plan
            .resources
            .tasks
            .first()
            .ok_or("task proof missing")?
            .trace_events_per_release,
    )?;
    let mut recorder = WorkflowTraceRecorder::new(
        maximum_events,
        runtime.control_state_bytes(),
        APPLICATION_STATE_BYTES,
        OUTPUT_BYTES,
        &[],
    )?;
    let (mut publisher, mut observer) = bounded_workflow_trace_channel(
        epoch()?,
        TraceCapacity::new(TRACE_CHANNEL_CAPACITY, TRACE_CHANNEL_CAPACITY)?,
    )?;
    let owner = thread::current().id();
    let mut callbacks = Vec::new();
    let mut all_records = Vec::new();
    let mut releases = Vec::with_capacity(scenario.releases.len());

    for (index, input) in scenario.releases.iter().enumerate() {
        let release = u64::try_from(index)?;
        clock
            .0
            .set(MonotonicTimestamp::new(epoch()?, 3 + release * 10));
        let ScheduleAction::Release(selected) = plan.observe(&clock, ScheduleControl::Continue)?
        else {
            return Err("release expected for every golden input".into());
        };
        let CycleStart::Execute(mut cycle) =
            task.begin(selected, &clock, ScheduleControl::Continue)?
        else {
            return Err("golden release was not executable".into());
        };
        let mut executor = GoldenExecutor {
            input,
            owner,
            wrong_thread: false,
            callbacks: &mut callbacks,
        };
        let scan = stage_simulated_release(
            &mut runtime,
            &mut cycle,
            &clock,
            &mut executor,
            &mut recorder,
        );
        if executor.wrong_thread {
            return Err("logical parallel branch executed on another thread".into());
        }
        let terminal = match scan {
            Ok(_) if input.right_fault => {
                return Err("fault input unexpectedly committed".into());
            }
            Ok(_) => {
                let commit = cycle.finish(&clock)?;
                recorder.finalize_committed(commit)?;
                "committed"
            }
            Err(StructuredScanError::NodeFault { node, reason })
                if input.right_fault
                    && node == runtime_node_handle(2)?
                    && reason == FaultReason::TaskExecutionFault =>
            {
                let discard = cycle.discard_observed(reason);
                recorder.finalize_discarded(discard)?;
                "discarded"
            }
            Err(error) => return Err(error.into()),
        };
        recorder.flush(&mut publisher)?;
        let records = collect_records(&mut observer)?;
        assert_one_terminal(&records)?;
        let committed_output = (0..OUTPUT_BYTES)
            .map(|offset| task.diagnostic().values().output(WorkSetIndex::new(offset)))
            .collect::<Result<Vec<_>, _>>()?;
        releases.push(ReleaseEvidence {
            release,
            terminal: terminal.to_owned(),
            committed_output,
            events: records.iter().copied().map(event_evidence).collect(),
        });
        all_records.extend(records);
    }
    if releases.len() != scenario.releases.len() {
        return Err("input trace generated too many or too few releases".into());
    }

    let trace_file = encode_trace_file(&all_records, &artifacts.static_plan_json)?;
    Ok(SimulationOutput {
        evidence: ScenarioEvidence {
            name: scenario.name.clone(),
            trace_sha256: sha256(&trace_file),
            record_count: all_records.len(),
            callbacks,
            releases,
        },
        trace_file,
    })
}

fn stage_i32_output(
    context: &mut WorkflowNodeContext<'_, '_, '_>,
    trace: &mut dyn StructuredOutputTrace,
    source_handle: u32,
    value_handle: u32,
    offset: usize,
    value: i32,
) -> Result<(), StructuredNodeExecutionError> {
    let mut before = [0_u8; 4];
    for (index, byte) in before.iter_mut().enumerate() {
        *byte = context
            .read_output(WorkSetIndex::new(offset + index))
            .map_err(|_| StructuredNodeExecutionError::Fault(FaultReason::CapacityExceeded))?;
    }
    let after = value.to_le_bytes();
    for (index, byte) in after.iter().copied().enumerate() {
        context
            .write_output(WorkSetIndex::new(offset + index), byte)
            .map_err(|_| StructuredNodeExecutionError::Fault(FaultReason::CapacityExceeded))?;
    }
    trace
        .stage_output(source_handle, value_handle, 4, &before, &after)
        .map_err(StructuredNodeExecutionError::Trace)
}

fn collect_records(
    observer: &mut aurora_control_engine::WorkflowTraceObserver,
) -> TestResult<Vec<WorkflowTraceRecord>> {
    let mut records = Vec::new();
    loop {
        match observer.try_observe() {
            Ok(observation) => records.push(observation.record()),
            Err(WorkflowTraceObserveError::Spsc(SpscPopError::Empty)) => return Ok(records),
            Err(error) => return Err(error.into()),
        }
    }
}

fn assert_one_terminal(records: &[WorkflowTraceRecord]) -> TestResult {
    let terminals = records
        .iter()
        .filter(|record| {
            matches!(
                record.kind(),
                WorkflowTraceEventKind::ScanCommitted | WorkflowTraceEventKind::ScanDiscarded
            )
        })
        .count();
    if terminals != 1
        || !records.last().is_some_and(|record| {
            matches!(
                record.kind(),
                WorkflowTraceEventKind::ScanCommitted | WorkflowTraceEventKind::ScanDiscarded
            )
        })
    {
        return Err("release did not produce exactly one final terminal event".into());
    }
    Ok(())
}

fn event_evidence(record: WorkflowTraceRecord) -> EventEvidence {
    let fragment = record.fragment();
    EventEvidence {
        kind: event_kind_name(record.kind()),
        detail: record.detail(),
        node: record.node_handle(),
        edge: record.edge_handle(),
        source: record.source_handle(),
        value: record.value_handle(),
        branch: record.branch_order(),
        fault: record.fault().map(|fault| format!("{fault:?}")),
        bytes: fragment.storage[..usize::from(fragment.bytes)].to_vec(),
    }
}

const fn event_kind_name(kind: WorkflowTraceEventKind) -> &'static str {
    match kind {
        WorkflowTraceEventKind::WorkflowInitialized => "WorkflowInitialized",
        WorkflowTraceEventKind::NodeExecuted => "NodeExecuted",
        WorkflowTraceEventKind::TransitionTaken => "TransitionTaken",
        WorkflowTraceEventKind::ForkActivated => "ForkActivated",
        WorkflowTraceEventKind::JoinSatisfied => "JoinSatisfied",
        WorkflowTraceEventKind::WaitObserved => "WaitObserved",
        WorkflowTraceEventKind::CancelRequested => "CancelRequested",
        WorkflowTraceEventKind::CancelApplied => "CancelApplied",
        WorkflowTraceEventKind::SubworkflowActivated => "SubworkflowActivated",
        WorkflowTraceEventKind::SubworkflowCompleted => "SubworkflowCompleted",
        WorkflowTraceEventKind::OutputStaged => "OutputStaged",
        WorkflowTraceEventKind::WatchedValue => "WatchedValue",
        WorkflowTraceEventKind::CompletionRequested => "CompletionRequested",
        WorkflowTraceEventKind::WorkflowCompleted => "WorkflowCompleted",
        WorkflowTraceEventKind::WorkflowFaulted => "WorkflowFaulted",
        WorkflowTraceEventKind::ForceObserved => "ForceObserved",
        WorkflowTraceEventKind::FallbackObserved => "FallbackObserved",
        WorkflowTraceEventKind::DeadlineObserved => "DeadlineObserved",
        WorkflowTraceEventKind::ScanCommitted => "ScanCommitted",
        WorkflowTraceEventKind::ScanDiscarded => "ScanDiscarded",
    }
}

fn encode_trace_file(records: &[WorkflowTraceRecord], plan: &[u8]) -> TestResult<Vec<u8>> {
    let digest: [u8; 32] = Sha256::digest(plan).into();
    let mut file = WorkflowTraceFileHeader::new(epoch()?, digest, u64::try_from(records.len())?, 0)
        .encode()
        .to_vec();
    for record in records {
        file.extend_from_slice(WorkflowTraceRecordBytes::encode(*record).as_bytes());
    }
    Ok(file)
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _write_result = write!(output, "{byte:02x}");
    }
    output
}

fn assert_cli_replay(trace: &[u8], plan: &[u8]) -> TestResult {
    let directory = TestDirectory::create()?;
    let trace_path = directory.path().join("golden.workflow-trace");
    let plan_path = directory.path().join("static-plan.json");
    let wrong_plan_path = directory.path().join("wrong-plan.json");
    let truncated_path = directory.path().join("truncated.workflow-trace");
    fs::write(&trace_path, trace)?;
    fs::write(&plan_path, plan)?;
    let mut wrong_plan = plan.to_vec();
    wrong_plan.push(b'\n');
    fs::write(&wrong_plan_path, wrong_plan)?;
    fs::write(
        &truncated_path,
        trace
            .get(..trace.len().saturating_sub(1))
            .ok_or("trace empty")?,
    )?;

    let first = replay_command(&trace_path, &plan_path, "C")?;
    let second = replay_command(&trace_path, &plan_path, "tr_TR.UTF-8")?;
    assert!(first.status.success());
    assert!(second.status.success());
    assert_eq!(first.stdout, second.stdout);
    let stdout = String::from_utf8(first.stdout)?;
    assert!(stdout.starts_with("status=complete traceability=traceable"));
    assert!(stdout.contains("kind=ScanCommitted"));

    let wrong_plan = replay_command(&trace_path, &wrong_plan_path, "C")?;
    assert!(!wrong_plan.status.success());
    assert!(String::from_utf8(wrong_plan.stderr)?.contains("PlanDigest"));

    let truncated = replay_command(&truncated_path, &plan_path, "C")?;
    assert!(!truncated.status.success());
    assert!(String::from_utf8(truncated.stderr)?.contains("Workflow Trace"));
    Ok(())
}

fn replay_command(
    trace: &Path,
    plan: &Path,
    locale: &str,
) -> Result<std::process::Output, std::io::Error> {
    Command::new(env!("CARGO_BIN_EXE_aurora-build"))
        .args(["workflow-trace-replay"])
        .arg(trace)
        .args(["--plan"])
        .arg(plan)
        .env("LANG", locale)
        .env("LC_ALL", locale)
        .output()
}
