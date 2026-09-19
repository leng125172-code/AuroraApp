//! R2-04 结构化语义与 R0 transaction 集成验收。

use aurora_control_contracts::{
    ExecutionBudgetNanos, ExecutionContractVersion, FaultReason, HardLimitNanos, MissPolicy,
    MissWindow, RelativeDeadlineNanos, TaskPeriodNanos, TaskPhaseNanos, TaskPriority, TaskSpec,
    TaskTiming,
};
use aurora_control_engine::{
    CycleStart, MonotonicClock, ScheduleAction, ScheduleControl, StaticTaskPlan,
    StaticTaskPlanBuilder, TaskTransaction, TransactionError, WorkSetCapacity, WorkSetIndex,
    WorkSetLimits,
};
use aurora_types::{BootEpochId, LocalHandle, MonotonicTimestamp};
use aurora_workflow_cyclic::*;
use std::cell::Cell;
use std::error::Error;

type TestResult = Result<(), Box<dyn Error>>;
struct Clock(Cell<MonotonicTimestamp>);
impl MonotonicClock for Clock {
    fn now(&self) -> MonotonicTimestamp {
        self.0.get()
    }
}
fn node(
    i: u32,
    start: u32,
    count: u32,
    kind: StructuredNodeKind,
) -> Result<StructuredNodeDefinition, WorkflowPlanError> {
    Ok(StructuredNodeDefinition {
        handle: WorkflowNodeHandle::new(i)?,
        instance: StructuredInstanceHandle(0),
        kind,
        outgoing: WorkflowEdgeRange { start, count },
        cancellation_boundary: false,
    })
}
fn edge(
    i: u32,
    source: u32,
    target: Option<u32>,
    branch: Option<u32>,
) -> Result<StructuredEdgeDefinition, WorkflowPlanError> {
    Ok(StructuredEdgeDefinition {
        handle: WorkflowEdgeHandle::new(i)?,
        source: WorkflowNodeHandle::new(source)?,
        target: match target {
            Some(t) => StructuredEdgeTarget::Node(WorkflowNodeHandle::new(t)?),
            None => StructuredEdgeTarget::Complete,
        },
        branch: branch.map(StructuredBranchHandle),
        maximum_traversals_per_run: None,
    })
}
fn definition<'a>(
    nodes: &'a [StructuredNodeDefinition],
    edges: &'a [StructuredEdgeDefinition],
    initial: &'a [WorkflowNodeHandle],
    instances: &'a [StructuredInstanceDefinition],
) -> StructuredWorkflowDefinition<'a> {
    StructuredWorkflowDefinition {
        task_handle: LocalHandle::ZERO,
        nodes,
        edges,
        initial_active: initial,
        forks: &[],
        branches: &[],
        memberships: &[],
        instances,
        calls: &[],
        call_initial_nodes: &[],
        state_copies: &[],
        maximum_active_nodes: 16,
        maximum_node_executions: 16,
        maximum_pending_cancellations: 16,
        application_state_bytes: 0,
        output_bytes: 2,
    }
}
fn setup(
    runtime: &StructuredWorkflowRuntime,
) -> Result<(TaskTransaction, StaticTaskPlan, Clock), Box<dyn Error>> {
    setup_with_state(runtime, &[])
}
fn setup_with_state(
    runtime: &StructuredWorkflowRuntime,
    application: &[u8],
) -> Result<(TaskTransaction, StaticTaskPlan, Clock), Box<dyn Error>> {
    setup_with_corruption(runtime, application, None)
}
fn setup_with_corruption(
    runtime: &StructuredWorkflowRuntime,
    application: &[u8],
    corruption: Option<(usize, u8)>,
) -> Result<(TaskTransaction, StaticTaskPlan, Clock), Box<dyn Error>> {
    let epoch = BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x44,
    ])?;
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
    let limits = WorkSetLimits::new(WorkSetCapacity::new(128)?, 32768);
    let mut builder = StaticTaskPlanBuilder::new(
        MonotonicTimestamp::new(epoch, 0),
        WorkSetCapacity::new(1)?,
        limits,
    )?;
    builder.add_task(spec)?;
    let mut state = runtime.initial_control_state().to_vec();
    if let Some((offset, value)) = corruption {
        state[offset] = value;
    }
    state.extend_from_slice(application);
    Ok((
        TaskTransaction::new(spec, epoch, &state, &[0, 0], limits)?,
        builder.seal()?,
        Clock(Cell::new(MonotonicTimestamp::new(epoch, 3))),
    ))
}
fn scan<E: StructuredNodeExecutor>(
    runtime: &mut StructuredWorkflowRuntime,
    task: &mut TaskTransaction,
    plan: &mut StaticTaskPlan,
    clock: &Clock,
    k: u64,
    executor: &mut E,
) -> Result<StructuredScanReport, Box<dyn Error>> {
    clock.0.set(MonotonicTimestamp::new(
        clock.now().boot_epoch(),
        3 + k * 10,
    ));
    let ScheduleAction::Release(release) = plan.observe(clock, ScheduleControl::Continue)? else {
        return Err("release expected".into());
    };
    let CycleStart::Execute(mut cycle) = task.begin(release, clock, ScheduleControl::Continue)?
    else {
        return Err("cycle expected".into());
    };
    let result = runtime.stage_scan(&mut cycle, clock, executor)?;
    cycle.finish(clock)?;
    Ok(result)
}

#[derive(Debug)]
struct BindingBackend;

impl RuntimeActionBackend for BindingBackend {
    fn invoke_st_pou(
        _invocation: RuntimeActionHandle,
        target_handle: u32,
        context: &mut RuntimeBindingContext<'_, '_, '_, '_>,
    ) -> Result<(), FaultReason> {
        if target_handle != 11 {
            return Err(FaultReason::TaskExecutionFault);
        }
        context.write(0, 0, 1)
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

#[derive(Debug)]
struct FaultingBindingBackend;

impl RuntimeActionBackend for FaultingBindingBackend {
    fn invoke_st_pou(
        _invocation: RuntimeActionHandle,
        _target_handle: u32,
        context: &mut RuntimeBindingContext<'_, '_, '_, '_>,
    ) -> Result<(), FaultReason> {
        context.write_invocation_state(0, 99)?;
        Err(FaultReason::TaskExecutionFault)
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

#[test]
fn fixed_action_dispatch_stages_output_then_evaluates_guard() -> TestResult {
    let nodes = [node(0, 0, 1, StructuredNodeKind::Action)?];
    let edges = [edge(0, 0, None, None)?];
    let initial = [WorkflowNodeHandle::new(0)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let mut runtime =
        StructuredWorkflowRuntime::new(definition(&nodes, &edges, &initial, &instances))?;
    let ports = [RuntimeActionPort {
        port: 0,
        direction: RuntimePortDirection::Output,
        slot: RuntimeValueSlot {
            area: RuntimeValueArea::Output,
            offset_bytes: 0,
            value_type: RuntimeValueType::Bool,
        },
        output_trace: Some(RuntimeOutputTraceDescriptor {
            value_handle: 0,
            type_handle: 0,
        }),
    }];
    let actions = [RuntimeActionDefinition {
        handle: RuntimeActionHandle(0),
        version: RuntimeBindingVersion::V1_0,
        kind: RuntimeActionKind::StPou,
        target_handle: 11,
        invocation_state: RuntimeByteRange {
            start: 0,
            length: 0,
        },
        ports: BindingRange { start: 0, count: 1 },
    }];
    let conditions = [RuntimeConditionDefinition {
        handle: RuntimeConditionHandle(0),
        source: ports[0].slot,
    }];
    let bindings = [RuntimeNodeBindingDefinition {
        node: nodes[0].handle,
        kind: RuntimeNodeBindingKind::Action {
            action: RuntimeActionHandle(0),
            guard: Some(RuntimeConditionHandle(0)),
            success_edge: edges[0].handle,
        },
    }];
    let mut executor = RuntimeBindingExecutor::<BindingBackend>::from_untrusted_tables(
        &nodes,
        &edges,
        &bindings,
        &actions,
        &ports,
        &conditions,
        &[],
        0,
        2,
        RuntimeBindingLimits {
            maximum_actions: 1,
            maximum_conditions: 1,
            maximum_ports_per_action: 1,
            maximum_guards_per_decision: 1,
        },
    )?;
    let (mut task, mut plan, clock) = setup(&runtime)?;

    let report = scan(&mut runtime, &mut task, &mut plan, &clock, 0, &mut executor)?;

    assert!(report.completed);
    assert_eq!(task.diagnostic().values().output(WorkSetIndex::new(0))?, 1);
    Ok(())
}

#[test]
fn action_fault_rolls_back_invocation_state_in_the_cycle_transaction() -> TestResult {
    let nodes = [node(0, 0, 1, StructuredNodeKind::Action)?];
    let edges = [edge(0, 0, None, None)?];
    let initial = [WorkflowNodeHandle::new(0)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let workflow = StructuredWorkflowDefinition {
        application_state_bytes: 1,
        ..definition(&nodes, &edges, &initial, &instances)
    };
    let mut runtime = StructuredWorkflowRuntime::new(workflow)?;
    let actions = [RuntimeActionDefinition {
        handle: RuntimeActionHandle(0),
        version: RuntimeBindingVersion::V1_0,
        kind: RuntimeActionKind::StPou,
        target_handle: 11,
        invocation_state: RuntimeByteRange {
            start: 0,
            length: 1,
        },
        ports: BindingRange { start: 0, count: 0 },
    }];
    let bindings = [RuntimeNodeBindingDefinition {
        node: nodes[0].handle,
        kind: RuntimeNodeBindingKind::Action {
            action: RuntimeActionHandle(0),
            guard: None,
            success_edge: edges[0].handle,
        },
    }];
    let mut executor = RuntimeBindingExecutor::<FaultingBindingBackend>::from_untrusted_tables(
        &nodes,
        &edges,
        &bindings,
        &actions,
        &[],
        &[],
        &[],
        1,
        2,
        RuntimeBindingLimits {
            maximum_actions: 1,
            maximum_conditions: 1,
            maximum_ports_per_action: 1,
            maximum_guards_per_decision: 1,
        },
    )?;
    let (mut task, mut plan, clock) = setup_with_state(&runtime, &[7])?;

    assert!(scan(&mut runtime, &mut task, &mut plan, &clock, 0, &mut executor).is_err());
    assert_eq!(
        task.diagnostic()
            .values()
            .state(WorkSetIndex::new(runtime.control_state_bytes()))?,
        7
    );
    Ok(())
}

#[test]
fn wait_one_activates_at_k_succeeds_k_plus_one_and_executes_successor_k_plus_two() -> TestResult {
    let nodes = [
        node(0, 0, 1, StructuredNodeKind::Action)?,
        node(1, 1, 1, StructuredNodeKind::WaitCycles { wait_cycles: 1 })?,
        node(2, 2, 1, StructuredNodeKind::Action)?,
    ];
    let edges = [
        edge(0, 0, Some(1), None)?,
        edge(1, 1, Some(2), None)?,
        edge(2, 2, None, None)?,
    ];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let initial = [WorkflowNodeHandle::new(0)?];
    let mut runtime =
        StructuredWorkflowRuntime::new(definition(&nodes, &edges, &initial, &instances))?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut seen = Vec::new();
    let mut execute = |n: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| {
        seen.push(n.get());
        Ok(StructuredNodeOutcome::Take(
            WorkflowEdgeHandle::new(n.get()).map_err(|_| FaultReason::TaskExecutionFault)?,
        ))
    };
    for k in 0..3 {
        let report = scan(&mut runtime, &mut task, &mut plan, &clock, k, &mut execute)?;
        assert_eq!(report.completed, k == 2);
    }
    assert_eq!(seen, [0, 2]);
    Ok(())
}

#[test]
fn condition_has_timeout_priority_and_permanent_wait_remains_active() -> TestResult {
    for (timeout, satisfied) in [(Some(1), true), (None, false), (Some(u64::MAX), false)] {
        let nodes = [node(
            0,
            0,
            1,
            StructuredNodeKind::WaitCondition {
                timeout_cycles: timeout,
            },
        )?];
        let edges = [edge(0, 0, None, None)?];
        let initial = [WorkflowNodeHandle::new(0)?];
        let instances = [StructuredInstanceDefinition {
            handle: StructuredInstanceHandle(0),
            parent_call: None,
        }];
        let mut runtime =
            StructuredWorkflowRuntime::new(definition(&nodes, &edges, &initial, &instances))?;
        let (mut task, mut plan, clock) = setup(&runtime)?;
        let result = scan(
            &mut runtime,
            &mut task,
            &mut plan,
            &clock,
            0,
            &mut |_: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| {
                Ok(StructuredNodeOutcome::Condition(satisfied))
            },
        )?;
        assert_eq!(result.completed, satisfied);
    }
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "同一完整 Fork 表用于验证所有败方策略"
)]
fn fork_order_join_all_tokens_and_join_any_policies_are_transactional() -> TestResult {
    for mode in [
        StructuredJoinMode::All,
        StructuredJoinMode::Any(StructuredJoinPolicy::CancelOthers),
        StructuredJoinMode::Any(StructuredJoinPolicy::KeepRunning),
        StructuredJoinMode::Any(StructuredJoinPolicy::WaitAtBoundary),
    ] {
        let mut nodes = [
            node(0, 0, 2, StructuredNodeKind::Fork(StructuredForkHandle(0)))?,
            node(1, 2, 1, StructuredNodeKind::Action)?,
            node(2, 3, 1, StructuredNodeKind::Action)?,
            node(
                3,
                4,
                1,
                StructuredNodeKind::Join {
                    fork: Some(StructuredForkHandle(0)),
                    mode,
                },
            )?,
            node(4, 5, 1, StructuredNodeKind::Action)?,
        ];
        nodes[2].cancellation_boundary = true;
        let edges = [
            edge(0, 0, Some(1), Some(0))?,
            edge(1, 0, Some(2), Some(1))?,
            edge(2, 1, Some(3), Some(0))?,
            edge(3, 2, Some(3), Some(1))?,
            edge(4, 3, Some(4), None)?,
            edge(5, 4, None, None)?,
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
        let mut d = definition(&nodes, &edges, &initial, &instances);
        d.forks = &forks;
        d.branches = &branches;
        d.memberships = &memberships;
        let mut runtime = StructuredWorkflowRuntime::new(d)?;
        let (mut task, mut plan, clock) = setup(&runtime)?;
        let mut slow_scans = 0;
        let mut seen = Vec::new();
        let mut execute = |n: WorkflowNodeHandle, c: &mut WorkflowNodeContext<'_, '_, '_>| {
            seen.push(n.get());
            if n.get() == 2 {
                slow_scans += 1;
                c.write_output(WorkSetIndex::new(0), slow_scans)
                    .map_err(|_| FaultReason::TaskExecutionFault)?;
                if slow_scans < 4 {
                    return Ok(StructuredNodeOutcome::Retain);
                }
            }
            let e = match n.get() {
                1 => 2,
                2 => 3,
                4 => 5,
                _ => return Err(FaultReason::TaskExecutionFault),
            };
            Ok(StructuredNodeOutcome::Take(
                WorkflowEdgeHandle::new(e).map_err(|_| FaultReason::TaskExecutionFault)?,
            ))
        };
        let mut done = false;
        for k in 0..8 {
            let r = scan(&mut runtime, &mut task, &mut plan, &clock, k, &mut execute)?;
            if r.completed {
                done = true;
                break;
            }
        }
        assert!(done);
        assert_eq!(&seen[..2], [1, 2]);
        assert!(seen.contains(&4));
        if mode == StructuredJoinMode::Any(StructuredJoinPolicy::CancelOthers) {
            assert_eq!(slow_scans, 2);
        } else {
            assert_eq!(slow_scans, 4);
        }
        if mode == StructuredJoinMode::Any(StructuredJoinPolicy::KeepRunning) {
            let mut repeating_edges = edges;
            repeating_edges[5].target = StructuredEdgeTarget::Node(WorkflowNodeHandle::new(0)?);
            repeating_edges[5].maximum_traversals_per_run = Some(1);
            let mut repeating = d;
            repeating.edges = &repeating_edges;
            let mut runtime = StructuredWorkflowRuntime::new(repeating)?;
            let (mut task, mut plan, clock) = setup(&runtime)?;
            let mut slow_visits = 0;
            for k in 0..4 {
                let result = scan(
                    &mut runtime,
                    &mut task,
                    &mut plan,
                    &clock,
                    k,
                    &mut |n: WorkflowNodeHandle, c: &mut WorkflowNodeContext<'_, '_, '_>| {
                        if n.get() == 2 {
                            slow_visits += 1;
                            c.write_output(WorkSetIndex::new(0), slow_visits)
                                .map_err(|_| FaultReason::TaskExecutionFault)?;
                            return Ok(StructuredNodeOutcome::Retain);
                        }
                        Ok(StructuredNodeOutcome::Take(
                            WorkflowEdgeHandle::new(if n.get() == 1 { 2 } else { 5 })
                                .map_err(|_| FaultReason::TaskExecutionFault)?,
                        ))
                    },
                );
                if k == 3 {
                    assert!(matches!(
                        result
                            .err()
                            .and_then(|e| e.downcast::<StructuredScanError>().ok())
                            .as_deref(),
                        Some(StructuredScanError::InvalidControlState)
                    ));
                    assert_eq!(task.diagnostic().values().output(WorkSetIndex::new(0))?, 2);
                } else {
                    assert!(!result?.completed);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn condition_timeout_fault_discards_whole_transaction_and_true_wins_at_deadline() -> TestResult {
    for condition in [true, false] {
        let nodes = [node(
            0,
            0,
            1,
            StructuredNodeKind::WaitCondition {
                timeout_cycles: Some(1),
            },
        )?];
        let edges = [edge(0, 0, None, None)?];
        let initial = [WorkflowNodeHandle::new(0)?];
        let instances = [StructuredInstanceDefinition {
            handle: StructuredInstanceHandle(0),
            parent_call: None,
        }];
        let mut runtime =
            StructuredWorkflowRuntime::new(definition(&nodes, &edges, &initial, &instances))?;
        let (mut task, mut plan, clock) = setup(&runtime)?;
        scan(
            &mut runtime,
            &mut task,
            &mut plan,
            &clock,
            0,
            &mut |_: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| {
                Ok(StructuredNodeOutcome::Condition(false))
            },
        )?;
        let result = scan(
            &mut runtime,
            &mut task,
            &mut plan,
            &clock,
            1,
            &mut |_: WorkflowNodeHandle, c: &mut WorkflowNodeContext<'_, '_, '_>| {
                c.write_output(WorkSetIndex::new(0), 42)
                    .map_err(|_| FaultReason::TaskExecutionFault)?;
                Ok(StructuredNodeOutcome::Condition(condition))
            },
        );
        if condition {
            assert!(result?.completed);
        } else {
            assert!(result.is_err());
        }
    }
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "双展开调用和原始 Fault 验证使用同一完整计划"
)]
fn expanded_calls_are_distinct_and_child_fault_preserves_original_reason() -> TestResult {
    let mut nodes = [
        node(
            0,
            0,
            1,
            StructuredNodeKind::Subworkflow(StructuredCallHandle(0)),
        )?,
        node(
            1,
            1,
            1,
            StructuredNodeKind::Subworkflow(StructuredCallHandle(1)),
        )?,
        node(2, 2, 1, StructuredNodeKind::Action)?,
        node(3, 3, 1, StructuredNodeKind::Action)?,
    ];
    nodes[2].instance = StructuredInstanceHandle(1);
    nodes[3].instance = StructuredInstanceHandle(2);
    let edges = [
        edge(0, 0, None, None)?,
        edge(1, 1, None, None)?,
        edge(2, 2, None, None)?,
        edge(3, 3, None, None)?,
    ];
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
            node: initial[0],
            child_instance: StructuredInstanceHandle(1),
            initial_nodes: StructuredBranchRange { start: 0, count: 1 },
            input_copies: StructuredBranchRange { start: 0, count: 0 },
            output_copies: StructuredBranchRange { start: 0, count: 0 },
        },
        StructuredSubworkflowDefinition {
            handle: StructuredCallHandle(1),
            node: initial[1],
            child_instance: StructuredInstanceHandle(2),
            initial_nodes: StructuredBranchRange { start: 1, count: 1 },
            input_copies: StructuredBranchRange { start: 0, count: 0 },
            output_copies: StructuredBranchRange { start: 0, count: 0 },
        },
    ];
    let call_initial = [WorkflowNodeHandle::new(2)?, WorkflowNodeHandle::new(3)?];
    let mut d = definition(&nodes, &edges, &initial, &instances);
    d.calls = &calls;
    d.call_initial_nodes = &call_initial;
    let mut runtime = StructuredWorkflowRuntime::new(d)?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut visits = [0_u8; 2];
    for k in 0..3 {
        let report = scan(
            &mut runtime,
            &mut task,
            &mut plan,
            &clock,
            k,
            &mut |n: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| {
                let i = n.get() as usize - 2;
                visits[i] += 1;
                if i == 1 && visits[i] == 1 {
                    Ok(StructuredNodeOutcome::Retain)
                } else {
                    Ok(StructuredNodeOutcome::Take(
                        WorkflowEdgeHandle::new(n.get())
                            .map_err(|_| FaultReason::TaskExecutionFault)?,
                    ))
                }
            },
        )?;
        assert_eq!(report.completed, k == 2);
    }
    assert_eq!(visits, [1, 2]);
    let mut runtime = StructuredWorkflowRuntime::new(d)?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    scan(
        &mut runtime,
        &mut task,
        &mut plan,
        &clock,
        0,
        &mut |_: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| {
            Err(FaultReason::CapacityExceeded)
        },
    )?;
    clock
        .0
        .set(MonotonicTimestamp::new(clock.now().boot_epoch(), 13));
    let ScheduleAction::Release(release) = plan.observe(&clock, ScheduleControl::Continue)? else {
        return Err("release expected".into());
    };
    let CycleStart::Execute(mut cycle) = task.begin(release, &clock, ScheduleControl::Continue)?
    else {
        return Err("cycle expected".into());
    };
    let error = runtime.stage_scan(&mut cycle, &clock, &mut |_: WorkflowNodeHandle,
                                                             _: &mut WorkflowNodeContext<
        '_,
        '_,
        '_,
    >| {
        Err(FaultReason::CapacityExceeded)
    });
    assert!(matches!(
        error,
        Err(StructuredScanError::SubworkflowFault {
            reason: FaultReason::CapacityExceeded,
            ..
        })
    ));
    assert!(cycle.finish(&clock).is_err());
    Ok(())
}

#[test]
fn rejects_missing_duplicate_extra_and_zero_wait_tables() -> TestResult {
    let nodes = [node(0, 0, 1, StructuredNodeKind::Action)?];
    let edges = [edge(0, 0, None, None)?];
    let initial = [WorkflowNodeHandle::new(0)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let base = definition(&nodes, &edges, &initial, &instances);
    let mut d = base;
    d.edges = &[];
    assert!(StructuredWorkflowRuntime::new(d).is_err());
    d = base;
    d.initial_active = &[];
    assert!(matches!(
        StructuredWorkflowRuntime::new(d),
        Err(StructuredPlanError::InvalidCapacity)
    ));
    let duplicate = [initial[0], initial[0]];
    d = base;
    d.initial_active = &duplicate;
    assert!(matches!(
        StructuredWorkflowRuntime::new(d),
        Err(StructuredPlanError::DuplicateEntry)
    ));
    let extra = [StructuredStateCopy {
        source: 0,
        target: 0,
    }];
    d = base;
    d.state_copies = &extra;
    assert!(matches!(
        StructuredWorkflowRuntime::new(d),
        Err(StructuredPlanError::MissingOrExtraEntry)
    ));
    let invalid = [node(
        0,
        0,
        1,
        StructuredNodeKind::WaitCycles { wait_cycles: 0 },
    )?];
    d = base;
    d.nodes = &invalid;
    assert!(matches!(
        StructuredWorkflowRuntime::new(d),
        Err(StructuredPlanError::InvalidWait)
    ));
    Ok(())
}

#[test]
fn traversal_limit_is_required_exactly_for_backedges() -> TestResult {
    let nodes = [
        node(0, 0, 1, StructuredNodeKind::Action)?,
        node(1, 1, 1, StructuredNodeKind::Action)?,
    ];
    let initial = [WorkflowNodeHandle::new(0)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];

    let mut forward_edges = [edge(0, 0, Some(1), None)?, edge(1, 1, None, None)?];
    forward_edges[0].maximum_traversals_per_run = Some(1);
    assert!(matches!(
        StructuredWorkflowRuntime::new(definition(&nodes, &forward_edges, &initial, &instances,)),
        Err(StructuredPlanError::InvalidBackedge)
    ));

    let mut backedges = [edge(0, 0, None, None)?, edge(1, 1, Some(0), None)?];
    assert!(matches!(
        StructuredWorkflowRuntime::new(definition(&nodes, &backedges, &initial, &instances)),
        Err(StructuredPlanError::InvalidBackedge)
    ));
    backedges[1].maximum_traversals_per_run = Some(1);
    assert!(
        StructuredWorkflowRuntime::new(definition(&nodes, &backedges, &initial, &instances,))
            .is_ok()
    );
    Ok(())
}

#[test]
fn multiple_task_roots_initialize_and_complete_independently() -> TestResult {
    let mut nodes = [
        node(0, 0, 1, StructuredNodeKind::Action)?,
        node(1, 1, 1, StructuredNodeKind::Action)?,
    ];
    nodes[1].instance = StructuredInstanceHandle(1);
    let edges = [edge(0, 0, None, None)?, edge(1, 1, None, None)?];
    let initial = [WorkflowNodeHandle::new(0)?, WorkflowNodeHandle::new(1)?];
    let instances = [
        StructuredInstanceDefinition {
            handle: StructuredInstanceHandle(0),
            parent_call: None,
        },
        StructuredInstanceDefinition {
            handle: StructuredInstanceHandle(1),
            parent_call: None,
        },
    ];
    let mut runtime =
        StructuredWorkflowRuntime::new(definition(&nodes, &edges, &initial, &instances))?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut visits = Vec::new();
    let mut second_root_visits = 0_u32;
    for release in 0..2 {
        let report = scan(
            &mut runtime,
            &mut task,
            &mut plan,
            &clock,
            release,
            &mut |node: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| {
                visits.push(node.get());
                if node.get() == 1 {
                    second_root_visits += 1;
                    if second_root_visits == 1 {
                        return Ok(StructuredNodeOutcome::Retain);
                    }
                }
                Ok(StructuredNodeOutcome::Take(
                    WorkflowEdgeHandle::new(node.get())
                        .map_err(|_| FaultReason::TaskExecutionFault)?,
                ))
            },
        )?;
        assert_eq!(report.completed, release == 1);
    }
    assert_eq!(visits, [0, 1, 1]);
    Ok(())
}

#[test]
fn prefailed_transaction_preserves_the_original_error() -> TestResult {
    let nodes = [node(0, 0, 1, StructuredNodeKind::Action)?];
    let edges = [edge(0, 0, None, None)?];
    let initial = [WorkflowNodeHandle::new(0)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let mut runtime =
        StructuredWorkflowRuntime::new(definition(&nodes, &edges, &initial, &instances))?;
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let ScheduleAction::Release(release) = plan.observe(&clock, ScheduleControl::Continue)? else {
        return Err("release expected".into());
    };
    let CycleStart::Execute(mut cycle) = task.begin(release, &clock, ScheduleControl::Continue)?
    else {
        return Err("cycle expected".into());
    };
    assert_eq!(
        cycle.write_output(WorkSetIndex::new(2), 1),
        Err(TransactionError::ImageOutOfRange)
    );
    let result = runtime.stage_scan(&mut cycle, &clock, &mut |_: WorkflowNodeHandle,
                                                              _: &mut WorkflowNodeContext<
        '_,
        '_,
        '_,
    >| {
        Ok(StructuredNodeOutcome::Retain)
    });
    assert!(matches!(
        result,
        Err(StructuredScanError::Transaction(
            TransactionError::FaultLocked(_)
        ))
    ));
    Ok(())
}

#[test]
fn branch_membership_must_be_the_exact_reachable_closure() -> TestResult {
    let nodes = [
        node(0, 0, 2, StructuredNodeKind::Fork(StructuredForkHandle(0)))?,
        node(1, 2, 1, StructuredNodeKind::Action)?,
        node(2, 3, 1, StructuredNodeKind::Action)?,
        node(3, 4, 1, StructuredNodeKind::Action)?,
        node(
            4,
            5,
            1,
            StructuredNodeKind::Join {
                fork: Some(StructuredForkHandle(0)),
                mode: StructuredJoinMode::All,
            },
        )?,
    ];
    let edges = [
        edge(0, 0, Some(1), Some(0))?,
        edge(1, 0, Some(3), Some(1))?,
        edge(2, 1, Some(2), None)?,
        edge(3, 2, Some(4), Some(0))?,
        edge(4, 3, Some(4), Some(1))?,
        edge(5, 4, None, None)?,
    ];
    let initial = [WorkflowNodeHandle::new(0)?];
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
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
    let complete = [
        StructuredBranchMembership {
            node: WorkflowNodeHandle::new(1)?,
            branch: StructuredBranchHandle(0),
        },
        StructuredBranchMembership {
            node: WorkflowNodeHandle::new(2)?,
            branch: StructuredBranchHandle(0),
        },
        StructuredBranchMembership {
            node: WorkflowNodeHandle::new(3)?,
            branch: StructuredBranchHandle(1),
        },
    ];
    let mut valid = definition(&nodes, &edges, &initial, &instances);
    valid.forks = &forks;
    valid.branches = &branches;
    valid.memberships = &complete;
    assert!(StructuredWorkflowRuntime::new(valid).is_ok());

    let missing = [complete[0], complete[2]];
    let mut invalid = valid;
    invalid.memberships = &missing;
    assert!(matches!(
        StructuredWorkflowRuntime::new(invalid),
        Err(StructuredPlanError::MissingOrExtraEntry | StructuredPlanError::InvalidReference)
    ));

    let cross_branch = [
        complete[0],
        complete[1],
        complete[2],
        StructuredBranchMembership {
            node: WorkflowNodeHandle::new(3)?,
            branch: StructuredBranchHandle(0),
        },
    ];
    invalid.memberships = &cross_branch;
    assert!(matches!(
        StructuredWorkflowRuntime::new(invalid),
        Err(StructuredPlanError::MissingOrExtraEntry | StructuredPlanError::InvalidReference)
    ));
    Ok(())
}

#[test]
fn child_copies_input_once_and_output_at_completion_commit() -> TestResult {
    let mut nodes = [
        node(
            0,
            0,
            1,
            StructuredNodeKind::Subworkflow(StructuredCallHandle(0)),
        )?,
        node(1, 1, 1, StructuredNodeKind::Action)?,
        node(2, 2, 1, StructuredNodeKind::Action)?,
    ];
    nodes[2].instance = StructuredInstanceHandle(1);
    let edges = [
        edge(0, 0, None, None)?,
        edge(1, 1, None, None)?,
        edge(2, 2, None, None)?,
    ];
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
    ];
    let calls = [StructuredSubworkflowDefinition {
        handle: StructuredCallHandle(0),
        node: initial[0],
        child_instance: StructuredInstanceHandle(1),
        initial_nodes: StructuredBranchRange { start: 0, count: 1 },
        input_copies: StructuredBranchRange { start: 0, count: 1 },
        output_copies: StructuredBranchRange { start: 1, count: 1 },
    }];
    let call_initial = [WorkflowNodeHandle::new(2)?];
    let copies = [
        StructuredStateCopy {
            source: 0,
            target: 1,
        },
        StructuredStateCopy {
            source: 1,
            target: 2,
        },
    ];
    let mut d = definition(&nodes, &edges, &initial, &instances);
    d.calls = &calls;
    d.call_initial_nodes = &call_initial;
    d.state_copies = &copies;
    d.application_state_bytes = 3;
    let mut runtime = StructuredWorkflowRuntime::new(d)?;
    let (mut task, mut plan, clock) = setup_with_state(&runtime, &[7, 0, 0])?;
    let mut visits = 0;
    for k in 0..4 {
        let report = scan(
            &mut runtime,
            &mut task,
            &mut plan,
            &clock,
            k,
            &mut |n: WorkflowNodeHandle, c: &mut WorkflowNodeContext<'_, '_, '_>| {
                if n.get() == 1 {
                    c.write_state(WorkSetIndex::new(0), 99)
                        .map_err(|_| FaultReason::TaskExecutionFault)?;
                } else {
                    visits += 1;
                    let value = c
                        .read_state(WorkSetIndex::new(1))
                        .map_err(|_| FaultReason::TaskExecutionFault)?;
                    if value != 7 + visits - 1 {
                        return Err(FaultReason::TaskExecutionFault);
                    }
                    c.write_state(WorkSetIndex::new(1), value + 1)
                        .map_err(|_| FaultReason::TaskExecutionFault)?;
                    if visits == 1 {
                        return Ok(StructuredNodeOutcome::Retain);
                    }
                }
                Ok(StructuredNodeOutcome::Take(
                    WorkflowEdgeHandle::new(n.get())
                        .map_err(|_| FaultReason::TaskExecutionFault)?,
                ))
            },
        )?;
        let parent_result = task
            .diagnostic()
            .values()
            .state(WorkSetIndex::new(runtime.control_state_bytes() + 2))?;
        assert_eq!(parent_result, if k < 2 { 0 } else { 9 });
        assert_eq!(report.completed, k >= 2);
    }
    assert_eq!(visits, 2);
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "完整的并行子实例表必须在一个测试中保持可审计，避免 fixture 隐藏生成项"
)]
fn completed_child_output_is_visible_to_later_same_scan_node() -> TestResult {
    let mut nodes = [
        node(0, 0, 2, StructuredNodeKind::Fork(StructuredForkHandle(0)))?,
        node(
            1,
            2,
            1,
            StructuredNodeKind::Subworkflow(StructuredCallHandle(0)),
        )?,
        node(2, 3, 1, StructuredNodeKind::Action)?,
        node(3, 4, 1, StructuredNodeKind::Action)?,
        node(
            4,
            5,
            1,
            StructuredNodeKind::Join {
                fork: Some(StructuredForkHandle(0)),
                mode: StructuredJoinMode::All,
            },
        )?,
    ];
    nodes[2].instance = StructuredInstanceHandle(1);
    let edges = [
        edge(0, 0, Some(1), Some(0))?,
        edge(1, 0, Some(3), Some(1))?,
        edge(2, 1, Some(4), Some(0))?,
        edge(3, 2, None, None)?,
        edge(4, 3, Some(4), Some(1))?,
        edge(5, 4, None, None)?,
    ];
    let initial = [WorkflowNodeHandle::new(0)?];
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
            node: WorkflowNodeHandle::new(3)?,
            branch: StructuredBranchHandle(1),
        },
    ];
    let instances = [
        StructuredInstanceDefinition {
            handle: StructuredInstanceHandle(0),
            parent_call: None,
        },
        StructuredInstanceDefinition {
            handle: StructuredInstanceHandle(1),
            parent_call: Some(StructuredCallHandle(0)),
        },
    ];
    let calls = [StructuredSubworkflowDefinition {
        handle: StructuredCallHandle(0),
        node: WorkflowNodeHandle::new(1)?,
        child_instance: StructuredInstanceHandle(1),
        initial_nodes: StructuredBranchRange { start: 0, count: 1 },
        input_copies: StructuredBranchRange { start: 0, count: 0 },
        output_copies: StructuredBranchRange { start: 0, count: 1 },
    }];
    let call_initial = [WorkflowNodeHandle::new(2)?];
    let copies = [StructuredStateCopy {
        source: 0,
        target: 1,
    }];
    let mut definition = definition(&nodes, &edges, &initial, &instances);
    definition.forks = &forks;
    definition.branches = &branches;
    definition.memberships = &memberships;
    definition.calls = &calls;
    definition.call_initial_nodes = &call_initial;
    definition.state_copies = &copies;
    definition.application_state_bytes = 2;
    let mut runtime = StructuredWorkflowRuntime::new(definition)?;
    let (mut task, mut plan, clock) = setup_with_state(&runtime, &[0, 0])?;
    let mut reader_visits = 0_u8;
    let mut observed = None;

    for release in 0..4 {
        scan(
            &mut runtime,
            &mut task,
            &mut plan,
            &clock,
            release,
            &mut |node: WorkflowNodeHandle, context: &mut WorkflowNodeContext<'_, '_, '_>| {
                match node.get() {
                    2 => {
                        context
                            .write_state(WorkSetIndex::new(0), 42)
                            .map_err(|_| FaultReason::TaskExecutionFault)?;
                        Ok(StructuredNodeOutcome::Take(
                            WorkflowEdgeHandle::new(3)
                                .map_err(|_| FaultReason::TaskExecutionFault)?,
                        ))
                    }
                    3 if reader_visits == 0 => {
                        reader_visits += 1;
                        Ok(StructuredNodeOutcome::Retain)
                    }
                    3 => {
                        observed = Some(
                            context
                                .read_state(WorkSetIndex::new(1))
                                .map_err(|_| FaultReason::TaskExecutionFault)?,
                        );
                        Ok(StructuredNodeOutcome::Take(
                            WorkflowEdgeHandle::new(4)
                                .map_err(|_| FaultReason::TaskExecutionFault)?,
                        ))
                    }
                    _ => Err(FaultReason::TaskExecutionFault),
                }
            },
        )?;
    }

    assert_eq!(observed, Some(42));
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "共享完整嵌套 Fork/call 图验证两种取消与每种控制字节损坏"
)]
fn nested_call_cancellation_does_not_copy_outputs_and_rejects_corrupt_control() -> TestResult {
    for policy in [
        StructuredJoinPolicy::CancelOthers,
        StructuredJoinPolicy::WaitAtBoundary,
    ] {
        let mut nodes = [
            node(0, 0, 2, StructuredNodeKind::Fork(StructuredForkHandle(0)))?,
            node(1, 2, 1, StructuredNodeKind::Action)?,
            node(
                2,
                3,
                1,
                StructuredNodeKind::Subworkflow(StructuredCallHandle(0)),
            )?,
            node(
                3,
                4,
                1,
                StructuredNodeKind::Join {
                    fork: Some(StructuredForkHandle(0)),
                    mode: StructuredJoinMode::Any(policy),
                },
            )?,
            node(4, 5, 1, StructuredNodeKind::Action)?,
            node(
                5,
                6,
                1,
                StructuredNodeKind::Subworkflow(StructuredCallHandle(1)),
            )?,
            node(6, 7, 1, StructuredNodeKind::Action)?,
        ];
        nodes[2].cancellation_boundary = true;
        nodes[5].instance = StructuredInstanceHandle(1);
        nodes[6].instance = StructuredInstanceHandle(2);
        let edges = [
            edge(0, 0, Some(1), Some(0))?,
            edge(1, 0, Some(2), Some(1))?,
            edge(2, 1, Some(3), Some(0))?,
            edge(3, 2, Some(3), Some(1))?,
            edge(4, 3, Some(4), None)?,
            edge(5, 4, None, None)?,
            edge(6, 5, None, None)?,
            edge(7, 6, None, None)?,
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
                node: WorkflowNodeHandle::new(2)?,
                child_instance: StructuredInstanceHandle(1),
                initial_nodes: StructuredBranchRange { start: 0, count: 1 },
                input_copies: StructuredBranchRange { start: 0, count: 0 },
                output_copies: StructuredBranchRange { start: 0, count: 1 },
            },
            StructuredSubworkflowDefinition {
                handle: StructuredCallHandle(1),
                node: WorkflowNodeHandle::new(5)?,
                child_instance: StructuredInstanceHandle(2),
                initial_nodes: StructuredBranchRange { start: 1, count: 1 },
                input_copies: StructuredBranchRange { start: 1, count: 0 },
                output_copies: StructuredBranchRange { start: 1, count: 0 },
            },
        ];
        let call_initial = [WorkflowNodeHandle::new(5)?, WorkflowNodeHandle::new(6)?];
        let initial = [WorkflowNodeHandle::new(0)?];
        let copies = [StructuredStateCopy {
            source: 0,
            target: 1,
        }];
        let mut d = definition(&nodes, &edges, &initial, &instances);
        d.forks = &forks;
        d.branches = &branches;
        d.memberships = &memberships;
        d.calls = &calls;
        d.call_initial_nodes = &call_initial;
        d.state_copies = &copies;
        d.application_state_bytes = 2;
        let mut runtime = StructuredWorkflowRuntime::new(d)?;
        let (mut task, mut plan, clock) = setup_with_state(&runtime, &[17, 0])?;
        let mut leaf_visits = 0;
        for k in 0..5 {
            let report = scan(
                &mut runtime,
                &mut task,
                &mut plan,
                &clock,
                k,
                &mut |n: WorkflowNodeHandle, c: &mut WorkflowNodeContext<'_, '_, '_>| {
                    let selected = match n.get() {
                        1 => 2,
                        4 => 5,
                        6 => {
                            leaf_visits += 1;
                            c.write_output(WorkSetIndex::new(0), 44)
                                .map_err(|_| FaultReason::TaskExecutionFault)?;
                            7
                        }
                        _ => return Err(FaultReason::TaskExecutionFault),
                    };
                    Ok(StructuredNodeOutcome::Take(
                        WorkflowEdgeHandle::new(selected)
                            .map_err(|_| FaultReason::TaskExecutionFault)?,
                    ))
                },
            )?;
            assert_eq!(report.completed, k >= 3);
            assert_eq!(
                task.diagnostic()
                    .values()
                    .state(WorkSetIndex::new(runtime.control_state_bytes() + 1))?,
                0
            );
        }
        assert_eq!(
            leaf_visits,
            i32::from(policy != StructuredJoinPolicy::CancelOthers)
        );
        let instance_offset = nodes.len().div_ceil(8);
        let token_offset = instance_offset + instances.len();
        let mut corruptions = vec![
            (0, 128),
            (token_offset, 4),
            (instance_offset, 3),
            (instance_offset + 1, 2),
        ];
        if policy == StructuredJoinPolicy::WaitAtBoundary {
            corruptions.push((token_offset + 1, 2));
            corruptions.push((token_offset + 1, 1));
        }
        for (offset, value) in corruptions {
            let mut runtime = StructuredWorkflowRuntime::new(d)?;
            let (mut task, mut plan, clock) =
                setup_with_corruption(&runtime, &[17, 0], Some((offset, value)))?;
            let ScheduleAction::Release(release) =
                plan.observe(&clock, ScheduleControl::Continue)?
            else {
                return Err("release expected".into());
            };
            let CycleStart::Execute(mut cycle) =
                task.begin(release, &clock, ScheduleControl::Continue)?
            else {
                return Err("cycle expected".into());
            };
            let error =
                runtime.stage_scan(&mut cycle, &clock, &mut |_: WorkflowNodeHandle,
                                                             _: &mut WorkflowNodeContext<
                    '_,
                    '_,
                    '_,
                >| {
                    Err(FaultReason::CapacityExceeded)
                });
            assert_eq!(error, Err(StructuredScanError::InvalidControlState));
            assert!(cycle.finish(&clock).is_err());
        }
    }
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "完整稠密多分支表与不同字节边界的期望在同一用例审计"
)]
fn compact_join_tokens_and_loser_slots_cross_byte_boundaries_without_extra_state() -> TestResult {
    for branch_count in [2_u32, 8, 9, 10, 17] {
        for late_low in [false, true] {
            let mut nodes = vec![node(
                0,
                0,
                branch_count,
                StructuredNodeKind::Fork(StructuredForkHandle(0)),
            )?];
            let mut edges = Vec::new();
            let mut branches = Vec::new();
            let mut memberships = Vec::new();
            for b in 0..branch_count {
                edges.push(edge(b, 0, Some(b + 1), Some(b))?);
                branches.push(StructuredBranchDefinition {
                    handle: StructuredBranchHandle(b),
                    fork: StructuredForkHandle(0),
                    branch_order: b,
                    activation_edge: WorkflowEdgeHandle::new(b)?,
                });
                memberships.push(StructuredBranchMembership {
                    node: WorkflowNodeHandle::new(b + 1)?,
                    branch: StructuredBranchHandle(b),
                });
            }
            for b in 0..branch_count {
                let mut action = node(b + 1, branch_count + b, 1, StructuredNodeKind::Action)?;
                action.cancellation_boundary = true;
                nodes.push(action);
                edges.push(edge(
                    branch_count + b,
                    b + 1,
                    Some(branch_count + 1),
                    Some(b),
                )?);
            }
            nodes.push(node(
                branch_count + 1,
                branch_count * 2,
                1,
                StructuredNodeKind::Join {
                    fork: Some(StructuredForkHandle(0)),
                    mode: StructuredJoinMode::Any(StructuredJoinPolicy::WaitAtBoundary),
                },
            )?);
            nodes.push(node(
                branch_count + 2,
                branch_count * 2 + 1,
                1,
                StructuredNodeKind::Action,
            )?);
            edges.push(edge(
                branch_count * 2,
                branch_count + 1,
                Some(branch_count + 2),
                None,
            )?);
            edges.push(edge(branch_count * 2 + 1, branch_count + 2, None, None)?);
            let forks = [StructuredForkDefinition {
                handle: StructuredForkHandle(0),
                node: WorkflowNodeHandle::new(0)?,
                branches: StructuredBranchRange {
                    start: 0,
                    count: branch_count,
                },
            }];
            let instances = [StructuredInstanceDefinition {
                handle: StructuredInstanceHandle(0),
                parent_call: None,
            }];
            let initial = [WorkflowNodeHandle::new(0)?];
            let mut d = definition(&nodes, &edges, &initial, &instances);
            d.forks = &forks;
            d.branches = &branches;
            d.memberships = &memberships;
            d.maximum_active_nodes = branch_count + 3;
            d.maximum_node_executions = branch_count + 3;
            d.maximum_pending_cancellations = branch_count - 1;
            if branch_count > 2 {
                let mut insufficient = d;
                insufficient.maximum_pending_cancellations -= 1;
                assert!(matches!(
                    StructuredWorkflowRuntime::new(insufficient),
                    Err(StructuredPlanError::InvalidCapacity)
                ));
            }
            let mut extra_metadata = edges.clone();
            if let Some(last) = extra_metadata.last_mut() {
                last.branch = Some(StructuredBranchHandle(0));
            }
            let mut invalid = d;
            invalid.edges = &extra_metadata;
            assert!(matches!(
                StructuredWorkflowRuntime::new(invalid),
                Err(StructuredPlanError::InvalidReference)
            ));
            let mut runtime = StructuredWorkflowRuntime::new(d)?;
            let token_offset = nodes.len().div_ceil(8) + instances.len();
            assert_eq!(
                runtime.control_state_bytes(),
                token_offset
                    + (branch_count as usize).div_ceil(8)
                    + (branch_count as usize - 1).div_ceil(8)
            );
            let (mut task, mut plan, clock) = setup(&runtime)?;
            let mut visits = vec![0_u32; branch_count as usize];
            let mut successors = 0;
            for k in 0..7 {
                let report = scan(
                    &mut runtime,
                    &mut task,
                    &mut plan,
                    &clock,
                    k,
                    &mut |n: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| {
                        let handle = n.get();
                        if handle == branch_count + 2 {
                            successors += 1;
                            return Ok(StructuredNodeOutcome::Take(
                                WorkflowEdgeHandle::new(branch_count * 2 + 1)
                                    .map_err(|_| FaultReason::TaskExecutionFault)?,
                            ));
                        }
                        let b = handle - 1;
                        visits[b as usize] += 1;
                        let required = if !late_low || b == branch_count - 1 {
                            1
                        } else if b == 0 {
                            2
                        } else {
                            4
                        };
                        if visits[b as usize] < required {
                            return Ok(StructuredNodeOutcome::Retain);
                        }
                        Ok(StructuredNodeOutcome::Take(
                            WorkflowEdgeHandle::new(branch_count + b)
                                .map_err(|_| FaultReason::TaskExecutionFault)?,
                        ))
                    },
                )?;
                if k == 2 {
                    let winner = if late_low { branch_count - 1 } else { 0 } as usize;
                    for byte in 0..(branch_count as usize).div_ceil(8) {
                        let expected = if winner / 8 == byte {
                            1_u8 << (winner % 8)
                        } else {
                            0
                        };
                        assert_eq!(
                            task.diagnostic()
                                .values()
                                .state(WorkSetIndex::new(token_offset + byte))?,
                            expected
                        );
                    }
                }
                if k == 6 {
                    assert!(report.completed);
                }
            }
            assert_eq!(successors, 1);
        }
    }
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "Fork、Wait与backedge组合图及逐release原子性断言集中展示"
)]
fn backedge_fork_wait_combination_preserves_per_run_count_and_atomic_fault() -> TestResult {
    let nodes = [
        node(0, 0, 2, StructuredNodeKind::Fork(StructuredForkHandle(0)))?,
        node(1, 2, 1, StructuredNodeKind::WaitCycles { wait_cycles: 1 })?,
        node(2, 3, 1, StructuredNodeKind::Action)?,
        node(
            3,
            4,
            1,
            StructuredNodeKind::Join {
                fork: Some(StructuredForkHandle(0)),
                mode: StructuredJoinMode::All,
            },
        )?,
        node(4, 5, 2, StructuredNodeKind::Decision)?,
    ];
    let mut edges = [
        edge(0, 0, Some(1), Some(0))?,
        edge(1, 0, Some(2), Some(1))?,
        edge(2, 1, Some(3), Some(0))?,
        edge(3, 2, Some(3), Some(1))?,
        edge(4, 3, Some(4), None)?,
        edge(5, 4, Some(0), None)?,
        edge(6, 4, None, None)?,
    ];
    edges[5].maximum_traversals_per_run = Some(1);
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
    let mut d = definition(&nodes, &edges, &initial, &instances);
    d.forks = &forks;
    d.branches = &branches;
    d.memberships = &memberships;
    let mut runtime = StructuredWorkflowRuntime::new(d)?;
    assert_eq!(
        runtime.control_state_bytes(),
        nodes.len().div_ceil(8) + instances.len() + 8 + 8 + 1
    );
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut visits = [0_u8; 2];
    for k in 0..8 {
        let result = scan(
            &mut runtime,
            &mut task,
            &mut plan,
            &clock,
            k,
            &mut |n: WorkflowNodeHandle, c: &mut WorkflowNodeContext<'_, '_, '_>| {
                let i = usize::from(n.get() != 2);
                visits[i] += 1;
                c.write_output(WorkSetIndex::new(i), visits[i])
                    .map_err(|_| FaultReason::TaskExecutionFault)?;
                Ok(StructuredNodeOutcome::Take(
                    WorkflowEdgeHandle::new(if i == 0 { 3 } else { 5 })
                        .map_err(|_| FaultReason::TaskExecutionFault)?,
                ))
            },
        );
        if k == 7 {
            assert!(matches!(
                result
                    .err()
                    .and_then(|e| e.downcast::<StructuredScanError>().ok())
                    .as_deref(),
                Some(StructuredScanError::BackedgeTraversalExceeded { .. })
            ));
            assert_eq!(task.diagnostic().values().output(WorkSetIndex::new(1))?, 1);
        } else {
            assert!(!result?.completed);
        }
    }
    assert_eq!(visits, [2, 2]);
    for offset in [
        nodes.len().div_ceil(8) + instances.len(),
        nodes.len().div_ceil(8) + instances.len() + 8,
    ] {
        let mut runtime = StructuredWorkflowRuntime::new(d)?;
        let (mut task, mut plan, clock) = setup_with_corruption(&runtime, &[], Some((offset, 2)))?;
        let error = scan(
            &mut runtime,
            &mut task,
            &mut plan,
            &clock,
            0,
            &mut |_: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| {
                Err(FaultReason::TaskExecutionFault)
            },
        );
        assert!(matches!(
            error
                .err()
                .and_then(|e| e.downcast::<StructuredScanError>().ok())
                .as_deref(),
            Some(StructuredScanError::InvalidControlState)
        ));
    }
    Ok(())
}

#[test]
fn empty_root_and_empty_child_complete_without_unproved_control_bytes() -> TestResult {
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let mut runtime = StructuredWorkflowRuntime::new(definition(&[], &[], &[], &instances))?;
    assert_eq!(runtime.control_state_bytes(), 1);
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let report = scan(
        &mut runtime,
        &mut task,
        &mut plan,
        &clock,
        0,
        &mut |_: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| {
            Err(FaultReason::TaskExecutionFault)
        },
    )?;
    assert!(report.completed);
    let nodes = [node(
        0,
        0,
        1,
        StructuredNodeKind::Subworkflow(StructuredCallHandle(0)),
    )?];
    let edges = [edge(0, 0, None, None)?];
    let initial = [WorkflowNodeHandle::new(0)?];
    let instances = [
        instances[0],
        StructuredInstanceDefinition {
            handle: StructuredInstanceHandle(1),
            parent_call: Some(StructuredCallHandle(0)),
        },
    ];
    let calls = [StructuredSubworkflowDefinition {
        handle: StructuredCallHandle(0),
        node: initial[0],
        child_instance: StructuredInstanceHandle(1),
        initial_nodes: StructuredBranchRange { start: 0, count: 0 },
        input_copies: StructuredBranchRange { start: 0, count: 1 },
        output_copies: StructuredBranchRange { start: 1, count: 1 },
    }];
    let copies = [
        StructuredStateCopy {
            source: 0,
            target: 1,
        },
        StructuredStateCopy {
            source: 1,
            target: 2,
        },
    ];
    let mut d = definition(&nodes, &edges, &initial, &instances);
    d.calls = &calls;
    d.state_copies = &copies;
    d.application_state_bytes = 3;
    let mut runtime = StructuredWorkflowRuntime::new(d)?;
    assert_eq!(runtime.control_state_bytes(), 3);
    let (mut task, mut plan, clock) = setup_with_state(&runtime, &[11, 0, 0])?;
    let report = scan(
        &mut runtime,
        &mut task,
        &mut plan,
        &clock,
        0,
        &mut |_: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| {
            Err(FaultReason::TaskExecutionFault)
        },
    )?;
    assert!(report.completed);
    assert_eq!(
        task.diagnostic()
            .values()
            .state(WorkSetIndex::new(runtime.control_state_bytes() + 1))?,
        11
    );
    assert_eq!(
        task.diagnostic()
            .values()
            .state(WorkSetIndex::new(runtime.control_state_bytes() + 2))?,
        11
    );
    let report = scan(
        &mut runtime,
        &mut task,
        &mut plan,
        &clock,
        1,
        &mut |_: WorkflowNodeHandle, _: &mut WorkflowNodeContext<'_, '_, '_>| {
            Err(FaultReason::TaskExecutionFault)
        },
    )?;
    assert!(report.completed);
    assert_eq!(report.executed_nodes, 0);
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "嵌套Fork完整归属表与跨层取消回归放在同一用例审计"
)]
fn outer_cancel_clears_inner_pending_bits_and_keeps_same_scan_branch_writes() -> TestResult {
    let mut nodes = [
        node(0, 0, 2, StructuredNodeKind::Fork(StructuredForkHandle(0)))?,
        node(1, 2, 1, StructuredNodeKind::Action)?,
        node(2, 3, 2, StructuredNodeKind::Fork(StructuredForkHandle(1)))?,
        node(3, 5, 1, StructuredNodeKind::Action)?,
        node(4, 6, 1, StructuredNodeKind::Action)?,
        node(
            5,
            7,
            1,
            StructuredNodeKind::Join {
                fork: Some(StructuredForkHandle(1)),
                mode: StructuredJoinMode::Any(StructuredJoinPolicy::WaitAtBoundary),
            },
        )?,
        node(
            6,
            8,
            1,
            StructuredNodeKind::Join {
                fork: Some(StructuredForkHandle(0)),
                mode: StructuredJoinMode::Any(StructuredJoinPolicy::CancelOthers),
            },
        )?,
        node(7, 9, 1, StructuredNodeKind::Action)?,
    ];
    nodes[3].cancellation_boundary = true;
    let edges = [
        edge(0, 0, Some(1), Some(0))?,
        edge(1, 0, Some(2), Some(1))?,
        edge(2, 1, Some(6), Some(0))?,
        edge(3, 2, Some(3), Some(2))?,
        edge(4, 2, Some(4), Some(3))?,
        edge(5, 3, Some(5), Some(2))?,
        edge(6, 4, Some(5), Some(3))?,
        edge(7, 5, Some(6), Some(1))?,
        edge(8, 6, Some(7), None)?,
        edge(9, 7, None, None)?,
    ];
    let forks = [
        StructuredForkDefinition {
            handle: StructuredForkHandle(0),
            node: WorkflowNodeHandle::new(0)?,
            branches: StructuredBranchRange { start: 0, count: 2 },
        },
        StructuredForkDefinition {
            handle: StructuredForkHandle(1),
            node: WorkflowNodeHandle::new(2)?,
            branches: StructuredBranchRange { start: 2, count: 2 },
        },
    ];
    let mut branches = Vec::new();
    for (b, f, activation) in [(0, 0, 0), (1, 0, 1), (2, 1, 3), (3, 1, 4)] {
        branches.push(StructuredBranchDefinition {
            handle: StructuredBranchHandle(b),
            fork: StructuredForkHandle(f),
            branch_order: b % 2,
            activation_edge: WorkflowEdgeHandle::new(activation)?,
        });
    }
    let mut memberships = Vec::new();
    for (n, b) in [(1, 0), (2, 1), (3, 1), (4, 1), (5, 1), (3, 2), (4, 3)] {
        memberships.push(StructuredBranchMembership {
            node: WorkflowNodeHandle::new(n)?,
            branch: StructuredBranchHandle(b),
        });
    }
    let instances = [StructuredInstanceDefinition {
        handle: StructuredInstanceHandle(0),
        parent_call: None,
    }];
    let initial = [WorkflowNodeHandle::new(0)?];
    let mut d = definition(&nodes, &edges, &initial, &instances);
    d.forks = &forks;
    d.branches = &branches;
    d.memberships = &memberships;
    d.maximum_pending_cancellations = 1;
    let mut runtime = StructuredWorkflowRuntime::new(d)?;
    assert_eq!(runtime.control_state_bytes(), 1 + 1 + 2 + 1);
    let (mut task, mut plan, clock) = setup(&runtime)?;
    let mut winner_visits = 0;
    let mut slow_visits = 0;
    for k in 0..7 {
        let report = scan(
            &mut runtime,
            &mut task,
            &mut plan,
            &clock,
            k,
            &mut |n: WorkflowNodeHandle, c: &mut WorkflowNodeContext<'_, '_, '_>| {
                let e = match n.get() {
                    1 => {
                        winner_visits += 1;
                        if winner_visits < 3 {
                            return Ok(StructuredNodeOutcome::Retain);
                        }
                        2
                    }
                    3 => {
                        slow_visits += 1;
                        c.write_output(WorkSetIndex::new(0), slow_visits)
                            .map_err(|_| FaultReason::TaskExecutionFault)?;
                        return Ok(StructuredNodeOutcome::Retain);
                    }
                    4 => 6,
                    7 => 9,
                    _ => return Err(FaultReason::TaskExecutionFault),
                };
                Ok(StructuredNodeOutcome::Take(
                    WorkflowEdgeHandle::new(e).map_err(|_| FaultReason::TaskExecutionFault)?,
                ))
            },
        )?;
        assert_eq!(report.completed, k >= 5);
    }
    assert_eq!(slow_visits, 3);
    assert_eq!(task.diagnostic().values().output(WorkSetIndex::new(0))?, 3);
    Ok(())
}

#[test]
fn structured_deadline_and_node_fault_never_publish_half_state() -> TestResult {
    for deadline in [true, false] {
        let nodes = [node(0, 0, 1, StructuredNodeKind::Action)?];
        let edges = [edge(0, 0, None, None)?];
        let instances = [StructuredInstanceDefinition {
            handle: StructuredInstanceHandle(0),
            parent_call: None,
        }];
        let initial = [WorkflowNodeHandle::new(0)?];
        let mut runtime =
            StructuredWorkflowRuntime::new(definition(&nodes, &edges, &initial, &instances))?;
        let (mut task, mut plan, clock) = setup(&runtime)?;
        if deadline {
            clock
                .0
                .set(MonotonicTimestamp::new(clock.now().boot_epoch(), 9));
        }
        let ScheduleAction::Release(release) = plan.observe(&clock, ScheduleControl::Continue)?
        else {
            return Err("release expected".into());
        };
        let CycleStart::Execute(mut cycle) =
            task.begin(release, &clock, ScheduleControl::Continue)?
        else {
            return Err("cycle expected".into());
        };
        let error = runtime.stage_scan(&mut cycle, &clock, &mut |_: WorkflowNodeHandle,
                                                                 c: &mut WorkflowNodeContext<
            '_,
            '_,
            '_,
        >| {
            c.write_output(WorkSetIndex::new(0), 88)
                .map_err(|_| FaultReason::CapacityExceeded)?;
            if deadline {
                clock
                    .0
                    .set(MonotonicTimestamp::new(clock.now().boot_epoch(), 12));
                Ok(StructuredNodeOutcome::Retain)
            } else {
                Err(FaultReason::CapacityExceeded)
            }
        });
        let error = error.err().ok_or("expected runtime error")?;
        if deadline {
            assert_eq!(
                error,
                StructuredScanError::Transaction(
                    aurora_control_engine::TransactionError::DeadlineMissed
                )
            );
        } else {
            assert!(matches!(
                error,
                StructuredScanError::NodeFault {
                    reason: FaultReason::CapacityExceeded,
                    ..
                }
            ));
        }
        assert!(cycle.finish(&clock).is_err());
        assert_eq!(task.diagnostic().values().output(WorkSetIndex::new(0))?, 0);
        assert_eq!(task.diagnostic().values().state(WorkSetIndex::new(0))?, 1);
    }
    Ok(())
}
