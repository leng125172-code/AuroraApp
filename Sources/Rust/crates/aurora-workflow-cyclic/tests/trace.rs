//! R2-06 单 release Trace recorder 与真实结构化 transaction 路径验收。

use std::cell::Cell;
use std::error::Error;

use aurora_control_contracts::{
    CommitSequence, ExecutionBudgetNanos, ExecutionContractVersion, FaultReason, HardLimitNanos,
    MissPolicy, MissWindow, RelativeDeadlineNanos, TaskPeriodNanos, TaskPhaseNanos, TaskPriority,
    TaskSpec, TaskTiming, TraceCapacity, WorkflowTraceEventKind, WorkflowTraceFileHeader,
    WorkflowTraceFileView, WorkflowTraceRecord, WorkflowTraceRecordBytes,
};
use aurora_control_engine::{
    CycleStart, MonotonicClock, ScheduleAction, ScheduleControl, SpscPopError, StaticTaskPlan,
    StaticTaskPlanBuilder, TaskTransaction, TransactionError, WorkSetCapacity, WorkSetIndex,
    WorkSetLimits, WorkflowTraceObserveError, bounded_workflow_trace_channel,
};
use aurora_types::{BootEpochId, LocalHandle, MonotonicTimestamp};
use aurora_workflow_cyclic::*;

type TestResult = Result<(), Box<dyn Error>>;

struct Clock(Cell<MonotonicTimestamp>);
impl MonotonicClock for Clock {
    fn now(&self) -> MonotonicTimestamp {
        self.0.get()
    }
}

fn epoch() -> Result<BootEpochId, Box<dyn Error>> {
    Ok(BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x44,
    ])?)
}

fn node(
    handle: u32,
    edge_start: u32,
    kind: StructuredNodeKind,
    instance: u32,
) -> Result<StructuredNodeDefinition, WorkflowPlanError> {
    Ok(StructuredNodeDefinition {
        handle: WorkflowNodeHandle::new(handle)?,
        instance: StructuredInstanceHandle(instance),
        kind,
        outgoing: WorkflowEdgeRange {
            start: edge_start,
            count: 1,
        },
        cancellation_boundary: false,
    })
}

fn complete_edge(handle: u32, source: u32) -> Result<StructuredEdgeDefinition, WorkflowPlanError> {
    Ok(StructuredEdgeDefinition {
        handle: WorkflowEdgeHandle::new(handle)?,
        source: WorkflowNodeHandle::new(source)?,
        target: StructuredEdgeTarget::Complete,
        branch: None,
        maximum_traversals_per_run: None,
    })
}

fn definition<'a>(
    nodes: &'a [StructuredNodeDefinition],
    edges: &'a [StructuredEdgeDefinition],
    initial: &'a [WorkflowNodeHandle],
    instances: &'a [StructuredInstanceDefinition],
    calls: &'a [StructuredSubworkflowDefinition],
) -> StructuredWorkflowDefinition<'a> {
    definition_for_task(LocalHandle::ZERO, nodes, edges, initial, instances, calls)
}

fn definition_for_task<'a>(
    task_handle: LocalHandle,
    nodes: &'a [StructuredNodeDefinition],
    edges: &'a [StructuredEdgeDefinition],
    initial: &'a [WorkflowNodeHandle],
    instances: &'a [StructuredInstanceDefinition],
    calls: &'a [StructuredSubworkflowDefinition],
) -> StructuredWorkflowDefinition<'a> {
    StructuredWorkflowDefinition {
        task_handle,
        nodes,
        edges,
        initial_active: initial,
        forks: &[],
        branches: &[],
        memberships: &[],
        instances,
        calls,
        call_initial_nodes: &[],
        state_copies: &[],
        maximum_active_nodes: 8,
        maximum_node_executions: 8,
        maximum_pending_cancellations: 8,
        application_state_bytes: 1,
        output_bytes: 1,
    }
}

fn setup(
    runtime: &StructuredWorkflowRuntime,
) -> Result<(TaskTransaction, StaticTaskPlan, Clock), Box<dyn Error>> {
    setup_with_output(runtime, &[0])
}

fn setup_with_output(
    runtime: &StructuredWorkflowRuntime,
    initial_output: &[u8],
) -> Result<(TaskTransaction, StaticTaskPlan, Clock), Box<dyn Error>> {
    setup_for_task(runtime, LocalHandle::ZERO, initial_output)
}

fn setup_for_task(
    runtime: &StructuredWorkflowRuntime,
    task_handle: LocalHandle,
    initial_output: &[u8],
) -> Result<(TaskTransaction, StaticTaskPlan, Clock), Box<dyn Error>> {
    let epoch = epoch()?;
    let spec = TaskSpec::new(
        ExecutionContractVersion::V1_0,
        task_handle,
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
        MonotonicTimestamp::new(epoch, 0),
        WorkSetCapacity::new(1)?,
        limits,
    )?;
    builder.add_task(spec)?;
    let mut state = runtime.initial_control_state().to_vec();
    state.push(0);
    Ok((
        TaskTransaction::new(spec, epoch, &state, initial_output, limits)?,
        builder.seal()?,
        Clock(Cell::new(MonotonicTimestamp::new(epoch, 3))),
    ))
}

fn begin<'a, 'plan>(
    task: &'a mut TaskTransaction,
    plan: &'plan mut StaticTaskPlan,
    clock: &Clock,
) -> Result<aurora_control_engine::CycleTransaction<'a, 'plan>, Box<dyn Error>> {
    let ScheduleAction::Release(release) = plan.observe(clock, ScheduleControl::Continue)? else {
        return Err("release expected".into());
    };
    let CycleStart::Execute(cycle) = task.begin(release, clock, ScheduleControl::Continue)? else {
        return Err("cycle expected".into());
    };
    Ok(cycle)
}

fn collect(
    observer: &mut aurora_control_engine::WorkflowTraceObserver,
) -> Result<Vec<WorkflowTraceRecord>, Box<dyn Error>> {
    let mut records = Vec::new();
    loop {
        match observer.try_observe() {
            Ok(observation) => records.push(observation.record()),
            Err(WorkflowTraceObserveError::Spsc(SpscPopError::Empty)) => return Ok(records),
            Err(error) => return Err(error.into()),
        }
    }
}

fn assert_strict_file_roundtrip(records: &[WorkflowTraceRecord]) -> TestResult {
    let mut file =
        WorkflowTraceFileHeader::new(epoch()?, [0x5a; 32], u64::try_from(records.len())?, 0)
            .encode()
            .to_vec();
    for record in records {
        file.extend_from_slice(WorkflowTraceRecordBytes::encode(*record).as_bytes());
    }
    let parsed = WorkflowTraceFileView::parse(&file)?;
    assert_eq!(parsed.records().count(), records.len());
    Ok(())
}

fn one_action_runtime() -> Result<StructuredWorkflowRuntime, Box<dyn Error>> {
    one_action_runtime_for_task(LocalHandle::ZERO)
}

fn one_action_runtime_for_task(
    task_handle: LocalHandle,
) -> Result<StructuredWorkflowRuntime, Box<dyn Error>> {
    let nodes = [node(0, 0, StructuredNodeKind::Action, 0)?];
    let edges = [complete_edge(0, 0)?];
    let initial = [WorkflowNodeHandle::new(0)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    Ok(StructuredWorkflowRuntime::new(definition_for_task(
        task_handle,
        &nodes,
        &edges,
        &initial,
        &instances,
        &[],
    ))?)
}

struct TracedOutputExecutor;

impl StructuredNodeExecutor for TracedOutputExecutor {
    fn execute(
        &mut self,
        _node: WorkflowNodeHandle,
        _context: &mut WorkflowNodeContext<'_, '_, '_>,
    ) -> Result<StructuredNodeOutcome, FaultReason> {
        Err(FaultReason::TaskExecutionFault)
    }

    fn execute_traced(
        &mut self,
        _node: WorkflowNodeHandle,
        context: &mut WorkflowNodeContext<'_, '_, '_>,
        trace: &mut dyn StructuredOutputTrace,
    ) -> Result<StructuredNodeOutcome, StructuredNodeExecutionError> {
        let before = [context
            .read_output(WorkSetIndex::new(0))
            .map_err(|_| StructuredNodeExecutionError::Fault(FaultReason::CapacityExceeded))?];
        context
            .write_output(WorkSetIndex::new(0), 0x5a)
            .map_err(|_| StructuredNodeExecutionError::Fault(FaultReason::CapacityExceeded))?;
        trace
            .stage_output(4, 2, 8, &before, &[0x5a])
            .map_err(StructuredNodeExecutionError::Trace)?;
        Ok(StructuredNodeOutcome::Take(
            WorkflowEdgeHandle::new(0).map_err(|_| {
                StructuredNodeExecutionError::Fault(FaultReason::TaskExecutionFault)
            })?,
        ))
    }
}

struct OutputBindingBackend;

impl RuntimeActionBackend for OutputBindingBackend {
    fn invoke_st_pou(
        invocation: RuntimeActionHandle,
        target_handle: u32,
        context: &mut RuntimeBindingContext<'_, '_, '_, '_>,
    ) -> Result<(), FaultReason> {
        if target_handle == 99 {
            context.write(0, 0, 0xee)?;
            return Err(FaultReason::TaskExecutionFault);
        }
        context.write(0, 0, if invocation.0 == 0 { 0x5a } else { 0x6b })?;
        if context.port_count() > 1 {
            context.write(1, 0, 0)?;
        }
        Ok(())
    }

    fn invoke_io_image(
        _invocation: RuntimeActionHandle,
        _target_handle: u32,
        _context: &mut RuntimeBindingContext<'_, '_, '_, '_>,
    ) -> Result<(), FaultReason> {
        Err(FaultReason::TaskExecutionFault)
    }

    fn stage_typed_command(
        _invocation: RuntimeActionHandle,
        _target_handle: u32,
        _context: &mut RuntimeBindingContext<'_, '_, '_, '_>,
    ) -> Result<(), FaultReason> {
        Err(FaultReason::TaskExecutionFault)
    }
}

fn output_port(port: u32, offset: usize, value_handle: u32) -> RuntimeActionPort {
    RuntimeActionPort {
        port,
        direction: RuntimePortDirection::Output,
        slot: RuntimeValueSlot {
            area: RuntimeValueArea::Output,
            offset_bytes: offset,
            value_type: RuntimeValueType::Bool,
        },
        output_trace: Some(RuntimeOutputTraceDescriptor {
            value_handle,
            type_handle: 0,
        }),
    }
}

fn output_binding_executor(
    nodes: &[StructuredNodeDefinition],
    edges: &[StructuredEdgeDefinition],
    targets: &[u32],
    ports_per_action: u32,
    ports: &[RuntimeActionPort],
    output_bytes: usize,
) -> Result<RuntimeBindingExecutor<OutputBindingBackend>, RuntimeBindingPlanError> {
    let mut actions = Vec::with_capacity(targets.len());
    for (index, target) in targets.iter().enumerate() {
        let handle = u32::try_from(index).map_err(|_| RuntimeBindingPlanError::InvalidCapacity)?;
        let start = handle
            .checked_mul(ports_per_action)
            .ok_or(RuntimeBindingPlanError::InvalidCapacity)?;
        actions.push(RuntimeActionDefinition {
            handle: RuntimeActionHandle(handle),
            version: RuntimeBindingVersion::V1_0,
            kind: RuntimeActionKind::StPou,
            target_handle: *target,
            invocation_state: RuntimeByteRange {
                start: 0,
                length: 0,
            },
            ports: BindingRange {
                start,
                count: ports_per_action,
            },
        });
    }
    let mut bindings = Vec::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        let handle = u32::try_from(index).map_err(|_| RuntimeBindingPlanError::InvalidCapacity)?;
        bindings.push(RuntimeNodeBindingDefinition {
            node: node.handle,
            kind: RuntimeNodeBindingKind::Action {
                action: RuntimeActionHandle(handle),
                guard: None,
                success_edge: edges[index].handle,
            },
        });
    }
    RuntimeBindingExecutor::from_untrusted_tables(
        nodes,
        edges,
        &bindings,
        &actions,
        ports,
        &[],
        &[],
        1,
        output_bytes,
        RuntimeBindingLimits {
            maximum_actions: 8,
            maximum_conditions: 8,
            maximum_ports_per_action: 8,
            maximum_guards_per_decision: 8,
        },
    )
}

#[test]
fn binding_emits_exact_changed_and_unchanged_writable_ports() -> TestResult {
    let nodes = [node(0, 0, StructuredNodeKind::Action, 0)?];
    let edges = [complete_edge(0, 0)?];
    let initial = [WorkflowNodeHandle::new(0)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let mut runtime_definition = definition(&nodes, &edges, &initial, &instances, &[]);
    runtime_definition.output_bytes = 2;
    let mut runtime = StructuredWorkflowRuntime::new(runtime_definition)?;
    let mut executor = output_binding_executor(
        &nodes,
        &edges,
        &[11],
        2,
        &[output_port(0, 0, 0), output_port(1, 1, 1)],
        2,
    )?;
    let (mut task, mut plan, clock) = setup_with_output(&runtime, &[0, 0])?;
    let mut recorder = WorkflowTraceRecorder::new(9, runtime.control_state_bytes(), 1, 2, &[])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    runtime.stage_scan_traced(&mut cycle, &clock, &mut executor, &mut recorder)?;
    let commit = cycle.finish(&clock)?;
    recorder.finalize_committed(commit)?;
    let (mut publisher, mut observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(9, 9)?)?;
    recorder.flush(&mut publisher)?;
    let records = collect(&mut observer)?;
    let outputs = records
        .iter()
        .filter(|record| record.kind() == WorkflowTraceEventKind::OutputStaged)
        .copied()
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].value_handle(), Some(0));
    assert_eq!(outputs[0].detail(), 1);
    assert!(outputs[0].fragment().digest.is_some());
    assert_eq!(outputs[0].fragment().storage[0], 0x5a);
    assert_eq!(outputs[1].value_handle(), Some(1));
    assert_eq!(outputs[1].detail(), 2);
    assert_eq!(
        outputs[1].fragment(),
        aurora_control_contracts::WorkflowTraceValueFragment::ABSENT
    );
    let node_position = records
        .iter()
        .position(|record| record.kind() == WorkflowTraceEventKind::NodeExecuted)
        .ok_or("node missing")?;
    let transition_position = records
        .iter()
        .position(|record| record.kind() == WorkflowTraceEventKind::TransitionTaken)
        .ok_or("transition missing")?;
    assert!(
        records[node_position + 1..transition_position]
            .iter()
            .all(|record| record.kind() == WorkflowTraceEventKind::OutputStaged)
    );
    assert_strict_file_roundtrip(&records)
}

#[test]
fn same_target_expanded_invocations_keep_distinct_output_sources() -> TestResult {
    let nodes = [
        node(0, 0, StructuredNodeKind::Action, 0)?,
        node(1, 1, StructuredNodeKind::Action, 0)?,
    ];
    let edges = [complete_edge(0, 0)?, complete_edge(1, 1)?];
    let initial = [WorkflowNodeHandle::new(0)?, WorkflowNodeHandle::new(1)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let mut runtime_definition = definition(&nodes, &edges, &initial, &instances, &[]);
    runtime_definition.output_bytes = 2;
    let mut runtime = StructuredWorkflowRuntime::new(runtime_definition)?;
    let mut executor = output_binding_executor(
        &nodes,
        &edges,
        &[11, 11],
        1,
        &[output_port(0, 0, 0), output_port(0, 1, 1)],
        2,
    )?;
    let (mut task, mut plan, clock) = setup_with_output(&runtime, &[0, 0])?;
    let mut recorder = WorkflowTraceRecorder::new(12, runtime.control_state_bytes(), 1, 2, &[])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    runtime.stage_scan_traced(&mut cycle, &clock, &mut executor, &mut recorder)?;
    let commit = cycle.finish(&clock)?;
    recorder.finalize_committed(commit)?;
    let (mut publisher, mut observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(12, 12)?)?;
    recorder.flush(&mut publisher)?;
    let records = collect(&mut observer)?;
    let outputs = records
        .iter()
        .filter(|record| record.kind() == WorkflowTraceEventKind::OutputStaged)
        .map(|record| {
            (
                record.source_handle(),
                record.value_handle(),
                record.fragment().storage[0],
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        outputs,
        vec![(Some(0), Some(0), 0x5a), (Some(1), Some(1), 0x6b)]
    );
    assert_strict_file_roundtrip(&records)
}

#[test]
fn later_binding_fault_discards_and_faulting_action_emits_no_output() -> TestResult {
    let nodes = [
        node(0, 0, StructuredNodeKind::Action, 0)?,
        node(1, 1, StructuredNodeKind::Action, 0)?,
    ];
    let edges = [complete_edge(0, 0)?, complete_edge(1, 1)?];
    let initial = [WorkflowNodeHandle::new(0)?, WorkflowNodeHandle::new(1)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let mut runtime_definition = definition(&nodes, &edges, &initial, &instances, &[]);
    runtime_definition.output_bytes = 2;
    let mut runtime = StructuredWorkflowRuntime::new(runtime_definition)?;
    let mut executor = output_binding_executor(
        &nodes,
        &edges,
        &[11, 99],
        1,
        &[output_port(0, 0, 0), output_port(0, 1, 1)],
        2,
    )?;
    let (mut task, mut plan, clock) = setup_with_output(&runtime, &[0, 0])?;
    let mut recorder = WorkflowTraceRecorder::new(8, runtime.control_state_bytes(), 1, 2, &[])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    let error = runtime.stage_scan_traced(&mut cycle, &clock, &mut executor, &mut recorder);
    assert!(matches!(
        error,
        Err(StructuredScanError::NodeFault { node, .. }) if node.get() == 1
    ));
    let discard = cycle.discard_observed(FaultReason::TaskExecutionFault);
    recorder.finalize_discarded(discard)?;
    let (mut publisher, mut observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(8, 8)?)?;
    recorder.flush(&mut publisher)?;
    let records = collect(&mut observer)?;
    let outputs = records
        .iter()
        .filter(|record| record.kind() == WorkflowTraceEventKind::OutputStaged)
        .collect::<Vec<_>>();
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].source_handle(), Some(0));
    assert_eq!(
        records[records.len() - 2].kind(),
        WorkflowTraceEventKind::WorkflowFaulted
    );
    assert_eq!(
        records.last().map(|record| record.kind()),
        Some(WorkflowTraceEventKind::ScanDiscarded)
    );
    assert_strict_file_roundtrip(&records)
}

#[test]
fn traced_executor_stages_changed_output_between_node_and_transition() -> TestResult {
    let mut runtime = one_action_runtime()?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut recorder = WorkflowTraceRecorder::new(8, runtime.control_state_bytes(), 1, 1, &[])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    runtime.stage_scan_traced(&mut cycle, &clock, &mut TracedOutputExecutor, &mut recorder)?;
    let commit = cycle.finish(&clock)?;
    recorder.finalize_committed(commit)?;
    let (mut publisher, mut observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(8, 8)?)?;
    recorder.flush(&mut publisher)?;
    let records = collect(&mut observer)?;
    let kinds = records
        .iter()
        .map(|record| record.kind())
        .collect::<Vec<_>>();
    let node_index = kinds
        .iter()
        .position(|kind| *kind == WorkflowTraceEventKind::NodeExecuted)
        .ok_or("node event missing")?;
    let output_index = kinds
        .iter()
        .position(|kind| *kind == WorkflowTraceEventKind::OutputStaged)
        .ok_or("output event missing")?;
    let transition_index = kinds
        .iter()
        .position(|kind| *kind == WorkflowTraceEventKind::TransitionTaken)
        .ok_or("transition event missing")?;
    assert!(node_index < output_index && output_index < transition_index);
    let output = records[output_index];
    assert_eq!(output.detail(), 1);
    assert_eq!(output.source_handle(), Some(4));
    assert_eq!(output.value_handle(), Some(2));
    assert_eq!(output.type_handle(), Some(8));
    assert_eq!(output.fragment().storage[0], 0x5a);
    assert!(output.fragment().digest.is_some());
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "fixture keeps the Fork, KeepRunning Join, slow loser, and five release trace path reviewable"
)]
fn keep_running_loser_records_resolved_join_consumption_before_root_completion() -> TestResult {
    let nodes = [
        StructuredNodeDefinition {
            handle: WorkflowNodeHandle::new(0)?,
            instance: StructuredInstanceHandle(0),
            kind: StructuredNodeKind::Fork(StructuredForkHandle(0)),
            outgoing: WorkflowEdgeRange { start: 0, count: 2 },
            cancellation_boundary: false,
        },
        node(1, 2, StructuredNodeKind::Action, 0)?,
        node(2, 3, StructuredNodeKind::Action, 0)?,
        node(
            3,
            4,
            StructuredNodeKind::Join {
                fork: Some(StructuredForkHandle(0)),
                mode: StructuredJoinMode::Any(StructuredJoinPolicy::KeepRunning),
            },
            0,
        )?,
        node(4, 5, StructuredNodeKind::Action, 0)?,
    ];
    let edges = [
        StructuredEdgeDefinition {
            handle: WorkflowEdgeHandle::new(0)?,
            source: WorkflowNodeHandle::new(0)?,
            target: StructuredEdgeTarget::Node(WorkflowNodeHandle::new(1)?),
            branch: Some(StructuredBranchHandle(0)),
            maximum_traversals_per_run: None,
        },
        StructuredEdgeDefinition {
            handle: WorkflowEdgeHandle::new(1)?,
            source: WorkflowNodeHandle::new(0)?,
            target: StructuredEdgeTarget::Node(WorkflowNodeHandle::new(2)?),
            branch: Some(StructuredBranchHandle(1)),
            maximum_traversals_per_run: None,
        },
        StructuredEdgeDefinition {
            handle: WorkflowEdgeHandle::new(2)?,
            source: WorkflowNodeHandle::new(1)?,
            target: StructuredEdgeTarget::Node(WorkflowNodeHandle::new(3)?),
            branch: Some(StructuredBranchHandle(0)),
            maximum_traversals_per_run: None,
        },
        StructuredEdgeDefinition {
            handle: WorkflowEdgeHandle::new(3)?,
            source: WorkflowNodeHandle::new(2)?,
            target: StructuredEdgeTarget::Node(WorkflowNodeHandle::new(3)?),
            branch: Some(StructuredBranchHandle(1)),
            maximum_traversals_per_run: None,
        },
        StructuredEdgeDefinition {
            handle: WorkflowEdgeHandle::new(4)?,
            source: WorkflowNodeHandle::new(3)?,
            target: StructuredEdgeTarget::Node(WorkflowNodeHandle::new(4)?),
            branch: None,
            maximum_traversals_per_run: None,
        },
        complete_edge(5, 4)?,
    ];
    let forks = [StructuredForkDefinition {
        handle: StructuredForkHandle(0),
        node: WorkflowNodeHandle::new(0)?,
        branches: StructuredBranchRange { start: 0, count: 2 },
    }];
    let branches = [
        StructuredBranchDefinition {
            handle: StructuredBranchHandle(0),
            fork: StructuredForkHandle(0),
            branch_order: 0,
            activation_edge: WorkflowEdgeHandle::new(0)?,
        },
        StructuredBranchDefinition {
            handle: StructuredBranchHandle(1),
            fork: StructuredForkHandle(0),
            branch_order: 1,
            activation_edge: WorkflowEdgeHandle::new(1)?,
        },
    ];
    let memberships = [
        StructuredBranchMembership {
            node: WorkflowNodeHandle::new(1)?,
            branch: StructuredBranchHandle(0),
        },
        StructuredBranchMembership {
            node: WorkflowNodeHandle::new(2)?,
            branch: StructuredBranchHandle(1),
        },
    ];
    let initial = [WorkflowNodeHandle::new(0)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let mut runtime_definition = definition(&nodes, &edges, &initial, &instances, &[]);
    runtime_definition.forks = &forks;
    runtime_definition.branches = &branches;
    runtime_definition.memberships = &memberships;
    let mut runtime = StructuredWorkflowRuntime::new(runtime_definition)?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut recorder = WorkflowTraceRecorder::new(32, runtime.control_state_bytes(), 1, 1, &[])?;
    let (mut publisher, mut observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(128, 128)?)?;
    let mut slow_visits = 0_u8;
    for release in 0_u64..5 {
        clock
            .0
            .set(MonotonicTimestamp::new(epoch()?, 3 + release * 10));
        let mut cycle = begin(&mut task, &mut plan, &clock)?;
        runtime.stage_scan_traced(
            &mut cycle,
            &clock,
            &mut |node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
                let edge = match node.get() {
                    1 => 2,
                    2 => {
                        slow_visits += 1;
                        if slow_visits < 4 {
                            return Ok(StructuredNodeOutcome::Retain);
                        }
                        3
                    }
                    4 => 5,
                    _ => return Err(FaultReason::TaskExecutionFault),
                };
                Ok(StructuredNodeOutcome::Take(
                    WorkflowEdgeHandle::new(edge).map_err(|_| FaultReason::TaskExecutionFault)?,
                ))
            },
            &mut recorder,
        )?;
        recorder.finalize_committed(cycle.finish(&clock)?)?;
        recorder.flush(&mut publisher)?;
    }
    let records = collect(&mut observer)?;
    let consumed = records
        .iter()
        .find(|record| {
            record.kind() == WorkflowTraceEventKind::TransitionTaken
                && record.edge_handle() == Some(3)
        })
        .ok_or("resolved Join consumption missing")?;
    assert_eq!(consumed.detail(), 1);
    assert!(records.iter().any(|record| {
        record.kind() == WorkflowTraceEventKind::WorkflowCompleted
            && record.release_sequence() == consumed.release_sequence()
    }));
    assert_strict_file_roundtrip(&records)
}

#[test]
fn commit_receipt_from_another_task_cannot_finalize_the_recorder() -> TestResult {
    let owner_handle = LocalHandle::ZERO;
    let foreign_handle = LocalHandle::new(1)?;
    let mut runtime_a = one_action_runtime_for_task(owner_handle)?;
    let mut runtime_b = one_action_runtime_for_task(foreign_handle)?;
    let (mut task_a, mut plan_a, clock_a) = setup_for_task(&runtime_a, owner_handle, &[0])?;
    let (mut task_b, mut plan_b, clock_b) = setup_for_task(&runtime_b, foreign_handle, &[0])?;
    let mut recorder_a = WorkflowTraceRecorder::new(8, runtime_a.control_state_bytes(), 1, 1, &[])?;
    let mut recorder_b = WorkflowTraceRecorder::new(8, runtime_b.control_state_bytes(), 1, 1, &[])?;

    let mut cycle_a = begin(&mut task_a, &mut plan_a, &clock_a)?;
    runtime_a.stage_scan_traced(
        &mut cycle_a,
        &clock_a,
        &mut TracedOutputExecutor,
        &mut recorder_a,
    )?;
    let _discard = cycle_a.discard_observed(FaultReason::TaskExecutionFault);

    let mut cycle_b = begin(&mut task_b, &mut plan_b, &clock_b)?;
    runtime_b.stage_scan_traced(
        &mut cycle_b,
        &clock_b,
        &mut TracedOutputExecutor,
        &mut recorder_b,
    )?;
    let commit_b = cycle_b.finish(&clock_b)?;
    assert_eq!(commit_b.identity().task_handle, foreign_handle);
    assert_eq!(commit_b.release_sequence().get(), 0);
    assert!(matches!(
        recorder_a.finalize_committed(commit_b),
        Err(WorkflowTraceError::InvalidCommitTransition)
    ));
    recorder_b.finalize_committed(commit_b)?;
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "容量相等与首个超限必须在同一测试中共享事件预算证据"
)]
fn exact_event_capacity_commits_and_first_excess_faults_without_truncation() -> TestResult {
    let watches = [WorkflowTraceWatchBinding {
        workflow_instance: StructuredInstanceHandle(0),
        value_handle: 0,
        type_handle: 7,
        area: WorkflowTraceWatchArea::Output,
        offset: 0,
        byte_count: 1,
    }];
    let mut runtime = one_action_runtime()?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut recorder =
        WorkflowTraceRecorder::new(8, runtime.control_state_bytes(), 1, 1, &watches)?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    let report = runtime.stage_scan_traced(
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, context: &mut WorkflowNodeContext<'_, '_, '_>| {
            context
                .write_output(WorkSetIndex::new(0), 0xa5)
                .map_err(|_| FaultReason::CapacityExceeded)?;
            Ok(StructuredNodeOutcome::Take(
                WorkflowEdgeHandle::new(0).map_err(|_| FaultReason::TaskExecutionFault)?,
            ))
        },
        &mut recorder,
    )?;
    assert!(report.completed);
    let commit = cycle.finish(&clock)?;
    recorder.finalize_committed(commit)?;
    assert_eq!(recorder.staged_event_count(), 8);
    let (mut publisher, mut observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(8, 8)?)?;
    let flushed = recorder.flush(&mut publisher)?;
    assert_eq!(flushed.published, 8);
    let records = collect(&mut observer)?;
    assert!(records.iter().all(|record| !matches!(
        record.kind(),
        WorkflowTraceEventKind::ForceObserved | WorkflowTraceEventKind::FallbackObserved
    )));
    assert_eq!(
        records.last().map(|record| record.kind()),
        Some(WorkflowTraceEventKind::ScanCommitted)
    );
    let deadline = records
        .iter()
        .find(|record| record.kind() == WorkflowTraceEventKind::DeadlineObserved)
        .ok_or("deadline record missing")?;
    assert_eq!(deadline.detail(), 1);
    assert_eq!(
        records.last().map(|record| record.commit_before()),
        Some(CommitSequence::ZERO)
    );
    assert_eq!(
        records.last().map(|record| record.commit_after().get()),
        Some(1)
    );
    let watch = records
        .iter()
        .find(|record| record.kind() == WorkflowTraceEventKind::WatchedValue)
        .ok_or("watch record missing")?;
    assert_eq!(watch.fragment().storage[0], 0xa5);

    let mut runtime = one_action_runtime()?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut too_small =
        WorkflowTraceRecorder::new(6, runtime.control_state_bytes(), 1, 1, &watches)?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    let error = runtime.stage_scan_traced(
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
            Ok(StructuredNodeOutcome::Take(
                WorkflowEdgeHandle::new(0).map_err(|_| FaultReason::TaskExecutionFault)?,
            ))
        },
        &mut too_small,
    );
    assert_eq!(
        error,
        Err(StructuredScanError::Trace(
            WorkflowTraceError::StageCapacityExceeded
        ))
    );
    let discard = cycle.discard_observed(FaultReason::CapacityExceeded);
    assert_eq!(discard.fault().reason, FaultReason::CapacityExceeded);
    assert_eq!(
        too_small.finalize_discarded(discard),
        Err(WorkflowTraceError::InvalidLifecycle)
    );
    Ok(())
}

#[test]
fn finish_after_deadline_receipt_discards_at_exact_capacity_and_roundtrips() -> TestResult {
    let watches = [WorkflowTraceWatchBinding {
        workflow_instance: StructuredInstanceHandle(0),
        value_handle: 0,
        type_handle: 7,
        area: WorkflowTraceWatchArea::Output,
        offset: 0,
        byte_count: 1,
    }];
    let mut runtime = one_action_runtime()?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    clock
        .0
        .set(MonotonicTimestamp::new(clock.now().boot_epoch(), 9));
    let mut recorder =
        WorkflowTraceRecorder::new(8, runtime.control_state_bytes(), 1, 1, &watches)?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    runtime.stage_scan_traced(
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
            Ok(StructuredNodeOutcome::Take(
                WorkflowEdgeHandle::new(0).map_err(|_| FaultReason::TaskExecutionFault)?,
            ))
        },
        &mut recorder,
    )?;
    assert_eq!(recorder.staged_event_count(), 6);
    clock
        .0
        .set(MonotonicTimestamp::new(clock.now().boot_epoch(), 12));
    let Err(failure) = cycle.finish_observed(&clock) else {
        return Err("deadline miss unexpectedly committed".into());
    };
    assert_eq!(failure.error(), TransactionError::DeadlineMissed);
    recorder.finalize_deadline_discarded(failure)?;
    assert_eq!(recorder.staged_event_count(), 6);

    let (mut publisher, mut observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(8, 8)?)?;
    recorder.flush(&mut publisher)?;
    let records = collect(&mut observer)?;
    let deadline = records
        .iter()
        .find(|record| record.kind() == WorkflowTraceEventKind::DeadlineObserved)
        .ok_or("deadline record missing")?;
    assert_eq!(deadline.detail(), 4);
    assert_eq!(deadline.commit_before(), deadline.commit_after());
    assert!(!records.iter().any(|record| matches!(
        record.kind(),
        WorkflowTraceEventKind::WorkflowFaulted
            | WorkflowTraceEventKind::WorkflowCompleted
            | WorkflowTraceEventKind::CancelApplied
            | WorkflowTraceEventKind::WatchedValue
    )));
    assert_eq!(
        records.last().map(|record| record.kind()),
        Some(WorkflowTraceEventKind::ScanDiscarded)
    );
    assert_strict_file_roundtrip(&records)
}

#[test]
fn transaction_fault_locked_is_attributed_to_current_node_once() -> TestResult {
    let mut runtime = one_action_runtime()?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut recorder = WorkflowTraceRecorder::new(6, runtime.control_state_bytes(), 1, 1, &[])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    let error = runtime.stage_scan_traced(
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
            clock
                .0
                .set(MonotonicTimestamp::new(clock.now().boot_epoch(), 9));
            Ok(StructuredNodeOutcome::Take(
                WorkflowEdgeHandle::new(0).map_err(|_| FaultReason::TaskExecutionFault)?,
            ))
        },
        &mut recorder,
    );
    let fault = match error {
        Err(StructuredScanError::Transaction(TransactionError::FaultLocked(fault))) => fault,
        other => return Err(format!("unexpected scan result: {other:?}").into()),
    };
    assert_eq!(fault.reason, FaultReason::HardLimitExceeded);
    let discard = cycle.discard_observed(fault.reason);
    recorder.finalize_discarded(discard)?;

    let (mut publisher, mut observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(6, 6)?)?;
    recorder.flush(&mut publisher)?;
    let records = collect(&mut observer)?;
    let faults = records
        .iter()
        .filter(|record| record.kind() == WorkflowTraceEventKind::WorkflowFaulted)
        .collect::<Vec<_>>();
    assert_eq!(faults.len(), 1);
    assert_eq!(faults[0].node_handle(), Some(0));
    assert_eq!(faults[0].source_handle(), Some(0));
    assert_eq!(faults[0].fault(), Some(FaultReason::HardLimitExceeded));
    assert_eq!(
        records.last().map(|record| record.kind()),
        Some(WorkflowTraceEventKind::ScanDiscarded)
    );
    assert_strict_file_roundtrip(&records)
}

#[test]
fn poisoned_invalid_transitions_emit_current_node_fault() -> TestResult {
    let nodes = [
        node(0, 0, StructuredNodeKind::Action, 0)?,
        node(1, 1, StructuredNodeKind::Action, 0)?,
    ];
    let edges = [complete_edge(0, 0)?, complete_edge(1, 1)?];
    let initial = [WorkflowNodeHandle::new(0)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];

    for outcome in [
        StructuredNodeOutcome::Condition(true),
        StructuredNodeOutcome::Take(WorkflowEdgeHandle::new(1)?),
    ] {
        let mut runtime =
            StructuredWorkflowRuntime::new(definition(&nodes, &edges, &initial, &instances, &[]))?;
        let (mut task, mut plan, clock) = setup(&runtime)?;
        let mut recorder = WorkflowTraceRecorder::new(4, runtime.control_state_bytes(), 1, 1, &[])?;
        let mut cycle = begin(&mut task, &mut plan, &clock)?;
        let error = runtime.stage_scan_traced(
            &mut cycle,
            &clock,
            &mut |_node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
                Ok(outcome)
            },
            &mut recorder,
        );
        assert_eq!(error, Err(StructuredScanError::InvalidTransition));
        let discard = cycle.discard_observed(FaultReason::TaskExecutionFault);
        recorder.finalize_discarded(discard)?;

        let (mut publisher, mut observer) =
            bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(4, 4)?)?;
        recorder.flush(&mut publisher)?;
        let records = collect(&mut observer)?;
        assert_eq!(
            records
                .iter()
                .map(|record| record.kind())
                .collect::<Vec<_>>(),
            vec![
                WorkflowTraceEventKind::WorkflowInitialized,
                WorkflowTraceEventKind::NodeExecuted,
                WorkflowTraceEventKind::WorkflowFaulted,
                WorkflowTraceEventKind::ScanDiscarded,
            ]
        );
        let fault = records.get(2).ok_or("fault record missing")?;
        assert_eq!(fault.node_handle(), Some(0));
        assert_eq!(fault.source_handle(), Some(0));
        assert_eq!(fault.execution_order(), Some(0));
        assert_eq!(fault.fault(), Some(FaultReason::TaskExecutionFault));
        assert_strict_file_roundtrip(&records)?;
    }

    Ok(())
}

#[test]
fn fault_is_followed_only_by_discard_and_later_nodes_never_execute() -> TestResult {
    let nodes = [
        node(0, 0, StructuredNodeKind::Action, 0)?,
        node(1, 1, StructuredNodeKind::Action, 0)?,
    ];
    let edges = [complete_edge(0, 0)?, complete_edge(1, 1)?];
    let initial = [WorkflowNodeHandle::new(0)?, WorkflowNodeHandle::new(1)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let mut runtime =
        StructuredWorkflowRuntime::new(definition(&nodes, &edges, &initial, &instances, &[]))?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut recorder = WorkflowTraceRecorder::new(4, runtime.control_state_bytes(), 1, 1, &[])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    let error = runtime.stage_scan_traced(
        &mut cycle,
        &clock,
        &mut |node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
            if node.get() == 0 {
                Err(FaultReason::TaskExecutionFault)
            } else {
                Ok(StructuredNodeOutcome::Retain)
            }
        },
        &mut recorder,
    );
    assert!(matches!(error, Err(StructuredScanError::NodeFault { node, .. }) if node.get() == 0));
    let discard = cycle.discard_observed(FaultReason::TaskExecutionFault);
    recorder.finalize_discarded(discard)?;
    let (mut publisher, mut observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(4, 4)?)?;
    recorder.flush(&mut publisher)?;
    let records = collect(&mut observer)?;
    assert_eq!(
        records
            .iter()
            .map(|record| record.kind())
            .collect::<Vec<_>>(),
        vec![
            WorkflowTraceEventKind::WorkflowInitialized,
            WorkflowTraceEventKind::NodeExecuted,
            WorkflowTraceEventKind::WorkflowFaulted,
            WorkflowTraceEventKind::ScanDiscarded,
        ]
    );
    assert_eq!(records[1].node_handle(), Some(0));
    assert_eq!(records[2].fault(), Some(FaultReason::TaskExecutionFault));
    assert_eq!(records[3].commit_before(), records[3].commit_after());
    let mut file =
        WorkflowTraceFileHeader::new(epoch()?, [0x5a; 32], u64::try_from(records.len())?, 0)
            .encode()
            .to_vec();
    for record in &records {
        file.extend_from_slice(WorkflowTraceRecordBytes::encode(*record).as_bytes());
    }
    let parsed = WorkflowTraceFileView::parse(&file)?;
    assert_eq!(parsed.records().count(), records.len());
    Ok(())
}

#[test]
fn expanded_call_sites_keep_distinct_trace_identity() -> TestResult {
    let nodes = [
        node(
            0,
            0,
            StructuredNodeKind::Subworkflow(StructuredCallHandle(0)),
            0,
        )?,
        node(
            1,
            1,
            StructuredNodeKind::Subworkflow(StructuredCallHandle(1)),
            0,
        )?,
    ];
    let edges = [complete_edge(0, 0)?, complete_edge(1, 1)?];
    let initial = [WorkflowNodeHandle::new(0)?, WorkflowNodeHandle::new(1)?];
    let instances = [
        StructuredInstanceDefinition {
            handle: StructuredInstanceHandle(0),
            parent_call: None,
        },
        StructuredInstanceDefinition {
            handle: StructuredInstanceHandle(1),
            parent_call: Some(StructuredCallHandle(0)),
        },
        StructuredInstanceDefinition {
            handle: StructuredInstanceHandle(2),
            parent_call: Some(StructuredCallHandle(1)),
        },
    ];
    let calls = [
        StructuredSubworkflowDefinition {
            handle: StructuredCallHandle(0),
            node: WorkflowNodeHandle::new(0)?,
            child_instance: StructuredInstanceHandle(1),
            initial_nodes: StructuredBranchRange { start: 0, count: 0 },
            input_copies: StructuredBranchRange { start: 0, count: 0 },
            output_copies: StructuredBranchRange { start: 0, count: 0 },
        },
        StructuredSubworkflowDefinition {
            handle: StructuredCallHandle(1),
            node: WorkflowNodeHandle::new(1)?,
            child_instance: StructuredInstanceHandle(2),
            initial_nodes: StructuredBranchRange { start: 0, count: 0 },
            input_copies: StructuredBranchRange { start: 0, count: 0 },
            output_copies: StructuredBranchRange { start: 0, count: 0 },
        },
    ];
    let mut runtime =
        StructuredWorkflowRuntime::new(definition(&nodes, &edges, &initial, &instances, &calls))?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut recorder = WorkflowTraceRecorder::new(16, runtime.control_state_bytes(), 1, 1, &[])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    runtime.stage_scan_traced(
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
            Err(FaultReason::TaskExecutionFault)
        },
        &mut recorder,
    )?;
    let commit = cycle.finish(&clock)?;
    recorder.finalize_committed(commit)?;
    let (mut publisher, mut observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(16, 16)?)?;
    recorder.flush(&mut publisher)?;
    let records = collect(&mut observer)?;
    let activated = records
        .iter()
        .filter(|record| record.kind() == WorkflowTraceEventKind::SubworkflowActivated)
        .map(|record| (record.workflow_instance_handle(), record.source_handle()))
        .collect::<Vec<_>>();
    let completed = records
        .iter()
        .filter(|record| record.kind() == WorkflowTraceEventKind::SubworkflowCompleted)
        .map(|record| (record.workflow_instance_handle(), record.source_handle()))
        .collect::<Vec<_>>();
    assert_eq!(activated, vec![(1, Some(0)), (2, Some(1))]);
    assert_eq!(completed, activated);
    Ok(())
}

#[test]
fn offline_repeated_simulation_uses_same_runtime_and_is_byte_deterministic() -> TestResult {
    fn run() -> Result<Vec<[u8; 192]>, Box<dyn Error>> {
        let mut runtime = one_action_runtime()?;
        let (mut task, mut plan, clock) = setup(&runtime)?;
        let mut recorder = WorkflowTraceRecorder::new(7, runtime.control_state_bytes(), 1, 1, &[])?;
        let mut cycle = begin(&mut task, &mut plan, &clock)?;
        stage_simulated_release(
            &mut runtime,
            &mut cycle,
            &clock,
            &mut |_node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
                Ok(StructuredNodeOutcome::Take(
                    WorkflowEdgeHandle::new(0).map_err(|_| FaultReason::TaskExecutionFault)?,
                ))
            },
            &mut recorder,
        )?;
        let commit = cycle.finish(&clock)?;
        recorder.finalize_committed(commit)?;
        let (mut publisher, mut observer) =
            bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(7, 7)?)?;
        recorder.flush(&mut publisher)?;
        Ok(collect(&mut observer)?
            .into_iter()
            .map(|record| {
                *aurora_control_contracts::WorkflowTraceRecordBytes::encode(record).as_bytes()
            })
            .collect())
    }

    assert_eq!(run()?, run()?);
    Ok(())
}

#[test]
fn dropped_observer_does_not_poison_later_control_releases() -> TestResult {
    let mut runtime = one_action_runtime()?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut recorder = WorkflowTraceRecorder::new(7, runtime.control_state_bytes(), 1, 1, &[])?;
    let (mut publisher, observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(16, 16)?)?;
    drop(observer);

    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    stage_simulated_release(
        &mut runtime,
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
            Ok(StructuredNodeOutcome::Take(
                WorkflowEdgeHandle::new(0).map_err(|_| FaultReason::TaskExecutionFault)?,
            ))
        },
        &mut recorder,
    )?;
    recorder.finalize_committed(cycle.finish(&clock)?)?;
    let first = recorder.flush(&mut publisher)?;
    assert_eq!(first.published, 0);
    assert!(first.dropped_newest > 0);

    clock.0.set(MonotonicTimestamp::new(epoch()?, 13));
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    stage_simulated_release(
        &mut runtime,
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
            Err(FaultReason::TaskExecutionFault)
        },
        &mut recorder,
    )?;
    recorder.finalize_committed(cycle.finish(&clock)?)?;
    let second = recorder.flush(&mut publisher)?;
    assert_eq!(second.published, 0);
    assert!(second.dropped_newest > 0);
    Ok(())
}

#[test]
fn multiple_recorders_share_the_publishers_global_event_sequence() -> TestResult {
    let mut runtime = one_action_runtime()?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut first = WorkflowTraceRecorder::new(7, runtime.control_state_bytes(), 1, 1, &[])?;
    let mut second = WorkflowTraceRecorder::new(2, runtime.control_state_bytes(), 1, 1, &[])?;
    let (mut publisher, mut observer) =
        bounded_workflow_trace_channel(epoch()?, TraceCapacity::new(16, 16)?)?;

    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    stage_simulated_release(
        &mut runtime,
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
            Ok(StructuredNodeOutcome::Take(
                WorkflowEdgeHandle::new(0).map_err(|_| FaultReason::TaskExecutionFault)?,
            ))
        },
        &mut first,
    )?;
    first.finalize_committed(cycle.finish(&clock)?)?;
    first.flush(&mut publisher)?;

    clock.0.set(MonotonicTimestamp::new(epoch()?, 13));
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    stage_simulated_release(
        &mut runtime,
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, _context: &mut WorkflowNodeContext<'_, '_, '_>| {
            Err(FaultReason::TaskExecutionFault)
        },
        &mut second,
    )?;
    second.finalize_committed(cycle.finish(&clock)?)?;
    second.flush(&mut publisher)?;

    let records = collect(&mut observer)?;
    assert_eq!(records.len(), 9);
    assert!(
        records
            .iter()
            .enumerate()
            .all(|(index, record)| record.event_sequence().get() == index as u64)
    );
    Ok(())
}
