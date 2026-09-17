//! R2-03 next-cycle activation、静态边界与 R0 原子回滚验收测试。

use std::cell::Cell;
use std::error::Error;

use aurora_control_contracts::{
    ExecutionBudgetNanos, ExecutionContractVersion, FaultReason, HardLimitNanos, MissPolicy,
    MissWindow, RelativeDeadlineNanos, TaskPeriodNanos, TaskPhaseNanos, TaskPriority, TaskSpec,
    TaskTiming,
};
use aurora_control_engine::{
    CycleStart, CycleTransaction, MonotonicClock, ResetGuard, ResetGuardError, ResetRequest,
    ScheduleAction, ScheduleControl, StaticTaskPlan, StaticTaskPlanBuilder, TaskTransaction,
    TransactionError, WorkSetCapacity, WorkSetIndex, WorkSetLimits,
};
use aurora_types::{BootEpochId, LocalHandle, MonotonicTimestamp};
use aurora_workflow_cyclic::{
    CyclicWorkflowDefinition, CyclicWorkflowRuntime, WorkflowEdgeDefinition, WorkflowEdgeHandle,
    WorkflowEdgeRange, WorkflowEdgeTarget, WorkflowNodeContext, WorkflowNodeDefinition,
    WorkflowNodeHandle, WorkflowNodeOutcome, WorkflowPlanError, WorkflowScanError,
};

type TestResult = Result<(), Box<dyn Error>>;

struct TestClock(Cell<MonotonicTimestamp>);

impl TestClock {
    fn set(&self, nanos: u64) {
        self.0
            .set(MonotonicTimestamp::new(self.now().boot_epoch(), nanos));
    }
}

impl MonotonicClock for TestClock {
    fn now(&self) -> MonotonicTimestamp {
        self.0.get()
    }
}

struct AllowExactReset(ResetRequest);

impl ResetGuard for AllowExactReset {
    fn check(&mut self, request: ResetRequest) -> Result<(), ResetGuardError> {
        if request == self.0 {
            Ok(())
        } else {
            Err(ResetGuardError::Unauthorized)
        }
    }
}

fn epoch() -> Result<BootEpochId, aurora_types::IdentifierError> {
    BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x44,
    ])
}

fn task_spec() -> Result<TaskSpec, Box<dyn Error>> {
    Ok(TaskSpec::new(
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
    ))
}

fn limits() -> Result<WorkSetLimits, Box<dyn Error>> {
    Ok(WorkSetLimits::new(WorkSetCapacity::new(128)?, 32_768))
}

fn setup_task(
    runtime: &CyclicWorkflowRuntime,
    application_state: &[u8],
    output: &[u8],
) -> Result<(TaskTransaction, StaticTaskPlan, TestClock), Box<dyn Error>> {
    let mut initial_state = Vec::from(runtime.initial_control_state());
    initial_state.extend_from_slice(application_state);
    let engine_epoch = epoch()?;
    let spec = task_spec()?;
    let mut builder = StaticTaskPlanBuilder::new(
        MonotonicTimestamp::new(engine_epoch, 0),
        WorkSetCapacity::new(1)?,
        limits()?,
    )?;
    builder.add_task(spec)?;
    Ok((
        TaskTransaction::new(spec, engine_epoch, &initial_state, output, limits()?)?,
        builder.seal()?,
        TestClock(Cell::new(MonotonicTimestamp::new(engine_epoch, 3))),
    ))
}

fn begin<'task, 'plan>(
    task: &'task mut TaskTransaction,
    plan: &'plan mut StaticTaskPlan,
    clock: &TestClock,
) -> Result<CycleTransaction<'task, 'plan>, Box<dyn Error>> {
    let ScheduleAction::Release(release) = plan.observe(clock, ScheduleControl::Continue)? else {
        return Err("expected release".into());
    };
    match task.begin(release, clock, ScheduleControl::Continue)? {
        CycleStart::Execute(cycle) => Ok(cycle),
        _ => Err("expected executable cycle".into()),
    }
}

fn node(raw: u32, start: u32, count: u32) -> Result<WorkflowNodeDefinition, WorkflowPlanError> {
    Ok(WorkflowNodeDefinition {
        handle: WorkflowNodeHandle::new(raw)?,
        outgoing: WorkflowEdgeRange { start, count },
    })
}

fn edge(
    raw: u32,
    source: u32,
    target: WorkflowEdgeTarget,
    maximum_traversals_per_run: Option<u64>,
) -> Result<WorkflowEdgeDefinition, WorkflowPlanError> {
    Ok(WorkflowEdgeDefinition {
        handle: WorkflowEdgeHandle::new(raw)?,
        source: WorkflowNodeHandle::new(source)?,
        target,
        maximum_traversals_per_run,
    })
}

fn build_runtime(
    nodes: &[WorkflowNodeDefinition],
    edges: &[WorkflowEdgeDefinition],
    initial_active: &[WorkflowNodeHandle],
    application_state_bytes: usize,
    output_bytes: usize,
) -> Result<CyclicWorkflowRuntime, WorkflowPlanError> {
    CyclicWorkflowRuntime::new(CyclicWorkflowDefinition {
        task_handle: LocalHandle::ZERO,
        nodes,
        edges,
        initial_active,
        maximum_active_nodes: u32::try_from(nodes.len()).unwrap_or(u32::MAX - 1).max(1),
        maximum_node_executions: u32::try_from(nodes.len()).unwrap_or(u32::MAX - 1).max(1),
        application_state_bytes,
        output_bytes,
    })
}

#[test]
fn forward_and_backedge_targets_wait_until_the_next_release() -> TestResult {
    let nodes = [node(0, 0, 1)?, node(1, 1, 1)?];
    let edges = [
        edge(
            0,
            0,
            WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(1)?),
            None,
        )?,
        edge(
            1,
            1,
            WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(0)?),
            Some(4),
        )?,
    ];
    let mut runtime = build_runtime(&nodes, &edges, &[WorkflowNodeHandle::new(0)?], 0, 1)?;
    let (mut task, mut plan, clock) = setup_task(&runtime, &[], &[0])?;
    let mut calls = Vec::new();

    for (release, expected) in [(3, 0), (13, 1), (23, 0)] {
        clock.set(release);
        let mut cycle = begin(&mut task, &mut plan, &clock)?;
        let report =
            runtime.stage_scan(&mut cycle, &clock, &mut |node: WorkflowNodeHandle,
                                                          _: &mut WorkflowNodeContext<
                '_,
                '_,
                '_,
            >| {
                calls.push(node.get());
                let selected = WorkflowEdgeHandle::new(node.get())
                    .map_err(|_| FaultReason::TaskExecutionFault)?;
                Ok(WorkflowNodeOutcome::Take(selected))
            })?;
        assert_eq!(calls.last().copied(), Some(expected));
        assert_eq!(report.executed_nodes, 1);
        assert_eq!(report.next_active_nodes, 1);
        cycle.finish(&clock)?;
    }
    assert_eq!(calls, [0, 1, 0]);
    Ok(())
}

#[test]
fn backedge_limit_accepts_equality_then_faults_without_partial_commit() -> TestResult {
    let nodes = [node(0, 0, 1)?];
    let edges = [edge(
        0,
        0,
        WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(0)?),
        Some(1),
    )?];
    let mut runtime = build_runtime(&nodes, &edges, &[WorkflowNodeHandle::new(0)?], 0, 1)?;
    let (mut task, mut plan, clock) = setup_task(&runtime, &[], &[3])?;
    let selected = WorkflowEdgeHandle::new(0)?;

    let mut first = begin(&mut task, &mut plan, &clock)?;
    assert!(
        runtime
            .stage_scan(&mut first, &clock, &mut |_node: WorkflowNodeHandle,
                                                  _: &mut WorkflowNodeContext<
                '_,
                '_,
                '_,
            >| {
                Ok(WorkflowNodeOutcome::Take(selected))
            },)
            .is_ok()
    );
    first.finish(&clock)?;
    assert_eq!(task.diagnostic().values().state(WorkSetIndex::new(1))?, 1);

    clock.set(13);
    let mut second = begin(&mut task, &mut plan, &clock)?;
    assert_eq!(
        runtime.stage_scan(&mut second, &clock, &mut |_node: WorkflowNodeHandle,
                                                      _: &mut WorkflowNodeContext<
            '_,
            '_,
            '_,
        >| {
            Ok(WorkflowNodeOutcome::Take(selected))
        },),
        Err(WorkflowScanError::BackedgeTraversalExceeded)
    );
    assert!(second.finish(&clock).is_err());
    let committed = task.diagnostic().values();
    assert_eq!(committed.state(WorkSetIndex::new(0))?, 1);
    assert_eq!(committed.state(WorkSetIndex::new(1))?, 1);
    assert_eq!(committed.output(WorkSetIndex::new(0))?, 3);
    Ok(())
}

#[test]
fn same_cycle_staging_is_visible_and_two_active_sources_deduplicate_one_target() -> TestResult {
    let nodes = [node(0, 0, 1)?, node(1, 1, 1)?, node(2, 2, 1)?];
    let edges = [
        edge(
            0,
            0,
            WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(2)?),
            None,
        )?,
        edge(
            1,
            1,
            WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(2)?),
            None,
        )?,
        edge(2, 2, WorkflowEdgeTarget::Complete, None)?,
    ];
    let mut runtime = build_runtime(
        &nodes,
        &edges,
        &[WorkflowNodeHandle::new(0)?, WorkflowNodeHandle::new(1)?],
        1,
        1,
    )?;
    let (mut task, mut plan, clock) = setup_task(&runtime, &[0], &[0])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    let report = runtime.stage_scan(
        &mut cycle,
        &clock,
        &mut |node: WorkflowNodeHandle, context: &mut WorkflowNodeContext<'_, '_, '_>| {
            if node.get() == 0 {
                context
                    .write_state(WorkSetIndex::new(0), 41)
                    .map_err(|_| FaultReason::CapacityExceeded)?;
            } else {
                let observed = context
                    .read_state(WorkSetIndex::new(0))
                    .map_err(|_| FaultReason::CapacityExceeded)?;
                context
                    .write_output(WorkSetIndex::new(0), observed + 1)
                    .map_err(|_| FaultReason::CapacityExceeded)?;
            }
            let selected =
                WorkflowEdgeHandle::new(node.get()).map_err(|_| FaultReason::TaskExecutionFault)?;
            Ok(WorkflowNodeOutcome::Take(selected))
        },
    )?;
    assert_eq!(report.executed_nodes, 2);
    assert_eq!(report.next_active_nodes, 1);
    cycle.finish(&clock)?;
    let committed = task.diagnostic().values();
    assert_eq!(committed.state(WorkSetIndex::new(1))?, 41);
    assert_eq!(committed.output(WorkSetIndex::new(0))?, 42);

    clock.set(13);
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    let mut target_calls = 0;
    let report = runtime.stage_scan(&mut cycle, &clock, &mut |node: WorkflowNodeHandle,
                                                               _: &mut WorkflowNodeContext<
        '_,
        '_,
        '_,
    >| {
        assert_eq!(node.get(), 2);
        target_calls += 1;
        let selected = WorkflowEdgeHandle::new(2).map_err(|_| FaultReason::TaskExecutionFault)?;
        Ok(WorkflowNodeOutcome::Take(selected))
    })?;
    assert_eq!(target_calls, 1);
    assert!(report.completed);
    cycle.finish(&clock)?;
    Ok(())
}

#[test]
fn node_fault_and_later_task_fault_both_roll_back_workflow_state_and_output() -> TestResult {
    let nodes = [node(0, 0, 0)?];
    let mut runtime = build_runtime(&nodes, &[], &[WorkflowNodeHandle::new(0)?], 1, 1)?;
    let (mut task, mut plan, clock) = setup_task(&runtime, &[7], &[9])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    let error = runtime.stage_scan(
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, context: &mut WorkflowNodeContext<'_, '_, '_>| {
            context
                .write_state(WorkSetIndex::new(0), 88)
                .map_err(|_| FaultReason::CapacityExceeded)?;
            context
                .write_output(WorkSetIndex::new(0), 99)
                .map_err(|_| FaultReason::CapacityExceeded)?;
            Err(FaultReason::TaskExecutionFault)
        },
    );
    assert!(matches!(error, Err(WorkflowScanError::NodeFault { .. })));
    assert!(cycle.finish(&clock).is_err());
    let committed = task.diagnostic().values();
    assert_eq!(committed.state(WorkSetIndex::new(0))?, 1);
    assert_eq!(committed.state(WorkSetIndex::new(1))?, 7);
    assert_eq!(committed.output(WorkSetIndex::new(0))?, 9);

    let mut runtime = build_runtime(&nodes, &[], &[WorkflowNodeHandle::new(0)?], 1, 1)?;
    let (mut task, mut plan, clock) = setup_task(&runtime, &[7], &[9])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    runtime.stage_scan(
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, context: &mut WorkflowNodeContext<'_, '_, '_>| {
            context
                .write_output(WorkSetIndex::new(0), 77)
                .map_err(|_| FaultReason::CapacityExceeded)?;
            Ok(WorkflowNodeOutcome::Retain)
        },
    )?;
    assert!(matches!(
        cycle.execute(|_| Err(FaultReason::TaskExecutionFault)),
        Err(TransactionError::FaultLocked(_))
    ));
    assert!(cycle.finish(&clock).is_err());
    let committed = task.diagnostic().values();
    assert_eq!(committed.state(WorkSetIndex::new(0))?, 1);
    assert_eq!(committed.output(WorkSetIndex::new(0))?, 9);
    Ok(())
}

#[test]
fn duplicate_scan_and_foreign_edge_poison_the_whole_cycle() -> TestResult {
    let nodes = [node(0, 0, 1)?, node(1, 1, 0)?];
    let edges = [edge(
        0,
        0,
        WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(1)?),
        None,
    )?];
    let mut runtime = build_runtime(&nodes, &edges, &[WorkflowNodeHandle::new(0)?], 0, 1)?;
    let (mut task, mut plan, clock) = setup_task(&runtime, &[], &[5])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    runtime.stage_scan(&mut cycle, &clock, &mut |_node: WorkflowNodeHandle,
                                                  _: &mut WorkflowNodeContext<
        '_,
        '_,
        '_,
    >| {
        let selected = WorkflowEdgeHandle::new(0).map_err(|_| FaultReason::TaskExecutionFault)?;
        Ok(WorkflowNodeOutcome::Take(selected))
    })?;
    assert_eq!(
        runtime.stage_scan(&mut cycle, &clock, &mut |_node: WorkflowNodeHandle,
                                                     _: &mut WorkflowNodeContext<
            '_,
            '_,
            '_,
        >| {
            Ok(WorkflowNodeOutcome::Retain)
        },),
        Err(WorkflowScanError::DuplicateRelease)
    );
    assert!(cycle.finish(&clock).is_err());
    assert_eq!(task.diagnostic().values().state(WorkSetIndex::new(0))?, 1);

    let mut runtime = build_runtime(&nodes, &edges, &[WorkflowNodeHandle::new(0)?], 0, 1)?;
    let (mut task, mut plan, clock) = setup_task(&runtime, &[], &[5])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    assert_eq!(
        runtime.stage_scan(&mut cycle, &clock, &mut |_node: WorkflowNodeHandle,
                                                     _: &mut WorkflowNodeContext<
            '_,
            '_,
            '_,
        >| {
            let selected =
                WorkflowEdgeHandle::new(7).map_err(|_| FaultReason::TaskExecutionFault)?;
            Ok(WorkflowNodeOutcome::Take(selected))
        },),
        Err(WorkflowScanError::InvalidTransition)
    );
    assert!(cycle.finish(&clock).is_err());
    assert_eq!(task.diagnostic().values().state(WorkSetIndex::new(0))?, 1);
    Ok(())
}

#[test]
fn constructor_rejects_missing_duplicate_and_foreign_static_entries() -> TestResult {
    let dense_nodes = [node(0, 0, 1)?, node(1, 1, 0)?];
    let valid_edge = edge(
        0,
        0,
        WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(1)?),
        None,
    )?;
    let cases = [
        (
            [node(0, 0, 0)?, node(1, 0, 0)?],
            [valid_edge],
            WorkflowPlanError::InvalidEdgeRange,
        ),
        (
            dense_nodes,
            [edge(
                0,
                1,
                WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(1)?),
                None,
            )?],
            WorkflowPlanError::EdgeOwnerMismatch,
        ),
        (
            dense_nodes,
            [edge(
                0,
                0,
                WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(9)?),
                None,
            )?],
            WorkflowPlanError::EdgeTargetOutOfRange,
        ),
        (
            dense_nodes,
            [edge(
                0,
                0,
                WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(1)?),
                Some(0),
            )?],
            WorkflowPlanError::InvalidBackedgeLimit,
        ),
        (
            dense_nodes,
            [edge(
                0,
                0,
                WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(1)?),
                Some(1),
            )?],
            WorkflowPlanError::InvalidBackedgeLimit,
        ),
        (
            [node(0, 0, 0)?, node(1, 0, 1)?],
            [edge(
                0,
                1,
                WorkflowEdgeTarget::Node(WorkflowNodeHandle::new(0)?),
                None,
            )?],
            WorkflowPlanError::InvalidBackedgeLimit,
        ),
        (
            dense_nodes,
            [edge(0, 0, WorkflowEdgeTarget::Complete, Some(1))?],
            WorkflowPlanError::InvalidBackedgeLimit,
        ),
    ];
    for (nodes, edges, expected) in cases {
        let result = build_runtime(&nodes, &edges, &[WorkflowNodeHandle::new(0)?], 0, 1);
        assert!(matches!(result, Err(error) if error == expected));
    }
    let duplicate_initial = build_runtime(
        &dense_nodes,
        &[valid_edge],
        &[WorkflowNodeHandle::new(0)?, WorkflowNodeHandle::new(0)?],
        0,
        1,
    );
    assert!(matches!(
        duplicate_initial,
        Err(WorkflowPlanError::InvalidInitialActiveNode)
    ));

    let missing_initial = build_runtime(&dense_nodes, &[valid_edge], &[], 0, 1);
    assert!(matches!(
        missing_initial,
        Err(WorkflowPlanError::InvalidInitialActiveNode)
    ));

    let empty = build_runtime(&[], &[], &[], 0, 1)?;
    assert_eq!(empty.node_count(), 0);
    assert_eq!(empty.control_state_bytes(), 0);

    Ok(())
}

#[test]
fn empty_active_plan_completes_without_calling_executor() -> TestResult {
    let mut runtime = build_runtime(&[], &[], &[], 0, 1)?;
    let (mut task, mut plan, clock) = setup_task(&runtime, &[], &[4])?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    let called = Cell::new(false);
    let report = runtime.stage_scan(&mut cycle, &clock, &mut |_node: WorkflowNodeHandle,
                                                               _: &mut WorkflowNodeContext<
        '_,
        '_,
        '_,
    >| {
        called.set(true);
        Ok(WorkflowNodeOutcome::Retain)
    })?;
    assert!(!called.get());
    assert_eq!(report.executed_nodes, 0);
    assert_eq!(report.next_active_nodes, 0);
    assert!(report.completed);
    cycle.finish(&clock)?;
    assert_eq!(task.diagnostic().values().output(WorkSetIndex::new(0))?, 4);
    Ok(())
}

#[test]
fn deadline_miss_discards_staging_without_becoming_task_execution_fault() -> TestResult {
    let nodes = [node(0, 0, 0)?];
    let mut runtime = build_runtime(&nodes, &[], &[WorkflowNodeHandle::new(0)?], 1, 1)?;
    let (mut task, mut plan, clock) = setup_task(&runtime, &[6], &[8])?;
    clock.set(9);
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    let result = runtime.stage_scan(
        &mut cycle,
        &clock,
        &mut |_node: WorkflowNodeHandle, context: &mut WorkflowNodeContext<'_, '_, '_>| {
            context
                .write_output(WorkSetIndex::new(0), 99)
                .map_err(|_| FaultReason::CapacityExceeded)?;
            clock.set(12);
            Ok(WorkflowNodeOutcome::Retain)
        },
    );
    assert_eq!(
        result,
        Err(WorkflowScanError::Transaction(
            TransactionError::DeadlineMissed
        ))
    );
    assert!(cycle.finish(&clock).is_err());
    assert!(task.fault().is_none());
    assert_eq!(task.diagnostic().values().state(WorkSetIndex::new(0))?, 1);
    assert_eq!(task.diagnostic().values().output(WorkSetIndex::new(0))?, 8);
    Ok(())
}

#[test]
fn reset_epoch_allows_a_new_release_zero_without_false_duplicate() -> TestResult {
    let nodes = [node(0, 0, 0)?];
    let mut runtime = build_runtime(&nodes, &[], &[WorkflowNodeHandle::new(0)?], 0, 1)?;
    let (mut task, mut plan, clock) = setup_task(&runtime, &[], &[0])?;
    let mut first = begin(&mut task, &mut plan, &clock)?;
    runtime.stage_scan(&mut first, &clock, &mut |_node: WorkflowNodeHandle,
                                                  _: &mut WorkflowNodeContext<
        '_,
        '_,
        '_,
    >| {
        Ok(WorkflowNodeOutcome::Retain)
    })?;
    first.finish(&clock)?;

    let request = task
        .lock_fault(FaultReason::TaskExecutionFault)
        .reset_request;
    task.reset(
        request,
        &mut AllowExactReset(request),
        &mut plan,
        &clock,
        |_| Ok(()),
    )?;
    clock.set(13);
    let mut reset_cycle = begin(&mut task, &mut plan, &clock)?;
    assert_eq!(reset_cycle.identity().task_epoch.get(), 2);
    assert_eq!(reset_cycle.identity().release_sequence.get(), 0);
    assert!(
        runtime
            .stage_scan(
                &mut reset_cycle,
                &clock,
                &mut |_node: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| Ok(
                    WorkflowNodeOutcome::Retain
                ),
            )
            .is_ok()
    );
    reset_cycle.finish(&clock)?;
    Ok(())
}
