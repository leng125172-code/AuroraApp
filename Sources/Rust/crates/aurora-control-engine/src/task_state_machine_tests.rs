use std::cell::Cell;
use std::error::Error;

use aurora_control_contracts::{
    ExecutionBudgetNanos, ExecutionContractVersion, FallbackRequestSequence, HardLimitNanos,
    MissPolicy, MissWindow, OutputSetIdentity, RelativeDeadlineNanos, TaskPeriodNanos,
    TaskPhaseNanos, TaskPriority, TaskTiming,
};
use aurora_types::{LocalHandle, MonotonicTimestamp};

use super::*;
use crate::{
    CycleStart, MonotonicClock, ResetGuard, ResetGuardError, ScheduleAction, ScheduleControl,
    StaticTaskPlan, StaticTaskPlanBuilder, TransactionError, WorkSetCapacity, WorkSetError,
    WorkSetLimits,
};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn miss_window_and_budget_degradation_recover_only_at_exact_boundaries() -> TestResult {
    let mut history = MissHistory::new(policy(4, 2, 4)?)?;
    assert!(history.record_misses(2).fault.is_none());
    assert_eq!(history.healthy_state(), TaskState::Degraded);
    assert_eq!(history.statistics().window_misses, 2);

    history.record_on_time(false);
    history.record_on_time(false);
    assert_eq!(history.statistics().window_misses, 2);
    assert_eq!(history.healthy_state(), TaskState::Degraded);
    history.record_on_time(false);
    assert_eq!(history.statistics().window_misses, 1);
    history.record_on_time(false);
    assert_eq!(history.statistics().window_misses, 0);
    assert_eq!(history.healthy_state(), TaskState::Running);

    history.record_on_time(true);
    assert_eq!(history.healthy_state(), TaskState::Degraded);
    assert_eq!(history.statistics().deadline_misses, 2);
    history.record_on_time(false);
    assert_eq!(history.healthy_state(), TaskState::Running);
    Ok(())
}

#[test]
fn threshold_precedence_large_batches_and_counters_are_bounded() -> TestResult {
    let mut simultaneous = MissHistory::new(policy(3, 2, 3)?)?;
    let fault = simultaneous
        .record_misses(3)
        .fault
        .ok_or("missing simultaneous threshold fault")?;
    assert_eq!(fault.reason, FaultReason::ConsecutiveMissesReached);
    assert_eq!(fault.offset, 2);

    let mut window_first = MissHistory::new(policy(4, 1, 4)?)?;
    let fault = window_first
        .record_misses(2)
        .fault
        .ok_or("missing window threshold fault")?;
    assert_eq!(fault.reason, FaultReason::MissWindowExceeded);
    assert_eq!(fault.offset, 1);

    let mut huge = MissHistory::new(policy(8, 7, 8)?)?;
    huge.scheduled_releases = u64::MAX - 1;
    huge.deadline_misses = u64::MAX - 1;
    let fault = huge
        .record_misses(u64::MAX)
        .fault
        .ok_or("missing huge-batch threshold fault")?;
    assert_eq!(fault.offset, 7);
    let statistics = huge.statistics();
    assert_eq!(statistics.window_misses, 8);
    assert_eq!(statistics.retained_releases, 8);
    assert_eq!(statistics.scheduled_releases, u64::MAX);
    assert_eq!(statistics.deadline_misses, u64::MAX);
    assert_eq!(statistics.consecutive_misses, u64::MAX);
    assert!(statistics.saturated);
    for _ in 0..7 {
        huge.record_on_time(false);
    }
    assert_eq!(huge.statistics().window_misses, 1);
    huge.record_on_time(false);
    assert_eq!(huge.statistics().window_misses, 0);
    Ok(())
}

#[test]
fn skipped_batch_counts_once_and_does_not_execute_after_threshold() -> TestResult {
    let (mut task, mut machine, mut plan, clock) = setup(0, policy(4, 2, 3)?)?;
    clock.set(23);
    let decision = selected(&mut plan, &clock)?;
    let skipped = decision
        .skipped_releases()
        .ok_or("expected skipped releases")?;
    assert_eq!(skipped.count(), 2);
    assert_eq!(
        machine.record_skipped_releases(&mut task, skipped, decision.observed_at())?,
        TaskState::Degraded
    );
    let commit = execute_and_finish(&mut task, decision, &clock, 24)?;
    assert_eq!(machine.record_commit(&task, commit)?, TaskState::Degraded);
    assert_eq!(machine.statistics().scheduled_releases, 3);
    assert_eq!(machine.statistics().deadline_misses, 2);

    for (release, finish, expected) in [
        (33, 34, TaskState::Degraded),
        (43, 44, TaskState::Degraded),
        (53, 54, TaskState::Running),
    ] {
        clock.set(release);
        let decision = selected(&mut plan, &clock)?;
        let commit = execute_and_finish(&mut task, decision, &clock, finish)?;
        assert_eq!(machine.record_commit(&task, commit)?, expected);
    }
    assert_eq!(machine.statistics().window_misses, 0);

    let (mut task, mut machine, mut plan, clock) = setup(1, policy(4, 2, 3)?)?;
    clock.set(33);
    let decision = selected(&mut plan, &clock)?;
    let skipped = decision
        .skipped_releases()
        .ok_or("expected skipped batch")?;
    assert_eq!(skipped.count(), 3);
    assert_eq!(
        machine.record_skipped_releases(&mut task, skipped, decision.observed_at())?,
        TaskState::FaultLocked
    );
    let fault = task.fault().ok_or("task was not locked")?;
    let request = machine.fallback_request().ok_or("missing fallback")?;
    assert_eq!(fault.reason, FaultReason::ConsecutiveMissesReached);
    assert_eq!(request.release_sequence().get(), 2);
    assert_eq!(request.reason(), fault.reason);
    assert_eq!(machine.statistics().scheduled_releases, 4);
    assert_eq!(machine.statistics().deadline_misses, 4);
    assert_eq!(
        machine.statistics().next_release_sequence,
        Some(ReleaseSequence::new(4))
    );
    assert_eq!(task.diagnostic().version().sequence, CommitSequence::ZERO);
    assert!(task.publishable().is_none());
    assert!(matches!(
        task.begin(decision, &clock, ScheduleControl::Continue),
        Err(TransactionError::FaultLocked(_))
    ));
    Ok(())
}

#[test]
fn duplicate_or_gapped_release_evidence_is_rejected_without_changing_history() -> TestResult {
    let (mut task, mut machine, mut plan, clock) = setup(0, policy(4, 3, 4)?)?;
    assert_eq!(
        machine.record_deadline_miss(
            &mut task,
            MissOutcome::StartAfterDeadline,
            ReleaseSequence::new(1),
            clock.now(),
        ),
        Err(TaskStateMachineError::UnexpectedReleaseSequence {
            expected: Some(ReleaseSequence::ZERO),
            actual: ReleaseSequence::new(1),
        })
    );
    assert_eq!(machine.statistics().scheduled_releases, 0);

    let decision = selected(&mut plan, &clock)?;
    let commit = execute_and_finish(&mut task, decision, &clock, 4)?;
    assert_eq!(machine.record_commit(&task, commit)?, TaskState::Running);
    let after_first = machine.statistics();
    assert_eq!(after_first.scheduled_releases, 1);
    assert_eq!(
        after_first.next_release_sequence,
        Some(ReleaseSequence::new(1))
    );

    assert_eq!(
        machine.record_commit(&task, commit),
        Err(TaskStateMachineError::UnexpectedReleaseSequence {
            expected: Some(ReleaseSequence::new(1)),
            actual: ReleaseSequence::ZERO,
        })
    );
    assert_eq!(machine.statistics(), after_first);
    assert!(matches!(
        TaskStateMachine::new(&task, OutputSetIdentity::from_sha256([0; 32])),
        Err(TaskStateMachineError::InvalidInitialTaskVersion)
    ));
    Ok(())
}

#[test]
fn fallback_mailbox_is_single_slot_idempotent_and_requires_exact_ack() -> TestResult {
    let (mut task, mut machine, _plan, clock) = setup(0, policy(4, 2, 3)?)?;
    let fault = task.lock_fault(FaultReason::TaskExecutionFault);
    assert_eq!(
        machine.synchronize_fault(&task, ReleaseSequence::new(7), clock.now())?,
        TaskState::FaultLocked
    );
    let request = machine.fallback_request().ok_or("missing fallback")?;
    assert_eq!(machine.mailbox_state(), FallbackMailboxState::Pending);
    assert_eq!(request.request_sequence(), FallbackRequestSequence::ZERO);

    machine.synchronize_fault(&task, ReleaseSequence::new(99), clock.now())?;
    assert_eq!(machine.fallback_request(), Some(request));
    assert_eq!(machine.fault, Some(fault));
    assert_eq!(
        machine.acknowledge_fallback(
            machine.engine_epoch,
            fault.reset_request.task_epoch,
            FallbackRequestSequence::new(1),
        ),
        Err(TaskStateMachineError::FallbackAckMismatch)
    );
    assert_eq!(machine.mailbox_state(), FallbackMailboxState::Pending);

    assert_eq!(
        machine.acknowledge_fallback(
            machine.engine_epoch,
            fault.reset_request.task_epoch,
            request.request_sequence(),
        )?,
        request
    );
    assert_eq!(machine.mailbox_state(), FallbackMailboxState::Acknowledged);
    assert_eq!(
        machine.acknowledge_fallback(
            machine.engine_epoch,
            fault.reset_request.task_epoch,
            request.request_sequence(),
        )?,
        request
    );
    assert!(matches!(
        machine.record_deadline_miss(
            &mut task,
            MissOutcome::StartAfterDeadline,
            ReleaseSequence::new(8),
            clock.now(),
        ),
        Err(TaskStateMachineError::FaultLocked(_))
    ));
    Ok(())
}

#[test]
fn hard_limit_discards_cycle_and_does_not_block_an_independent_task() -> TestResult {
    let (mut faulted, mut faulted_machine, mut faulted_plan, faulted_clock) =
        setup(0, policy(4, 3, 4)?)?;
    let decision = selected(&mut faulted_plan, &faulted_clock)?;
    let release = decision.release_sequence();
    let CycleStart::Execute(mut cycle) =
        faulted.begin(decision, &faulted_clock, ScheduleControl::Continue)?
    else {
        return Err("expected executable faulted task".into());
    };
    cycle.write_output(crate::WorkSetIndex::new(0), 99)?;
    faulted_clock.set(9);
    assert!(matches!(
        cycle.finish(&faulted_clock),
        Err(TransactionError::FaultLocked(_))
    ));
    assert_eq!(
        faulted.diagnostic().version().sequence,
        CommitSequence::ZERO
    );
    assert!(faulted.publishable().is_none());
    faulted_machine.synchronize_fault(&faulted, release, faulted_clock.now())?;
    assert_eq!(
        faulted_machine
            .fallback_request()
            .ok_or("missing hard-limit fallback")?
            .reason(),
        FaultReason::HardLimitExceeded
    );

    let (mut healthy, mut healthy_machine, mut healthy_plan, healthy_clock) =
        setup(1, policy(4, 3, 4)?)?;
    let decision = selected(&mut healthy_plan, &healthy_clock)?;
    let commit = execute_and_finish(&mut healthy, decision, &healthy_clock, 4)?;
    assert_eq!(
        healthy_machine.record_commit(&healthy, commit)?,
        TaskState::Running
    );
    assert_eq!(healthy.diagnostic().version().sequence.get(), 1);
    assert!(healthy_machine.fallback_request().is_none());
    Ok(())
}

#[test]
fn reset_requires_ack_and_clears_history_only_after_new_epoch_commit_zero() -> TestResult {
    let (mut task, mut machine, mut plan, clock) = setup(0, policy(4, 2, 3)?)?;
    let fault = task.lock_fault(FaultReason::TaskExecutionFault);
    machine.synchronize_fault(&task, ReleaseSequence::ZERO, clock.now())?;
    assert_eq!(
        machine.begin_reinitialization(fault.reset_request),
        Err(TaskStateMachineError::FallbackNotAcknowledged)
    );
    let request = machine.fallback_request().ok_or("missing fallback")?;
    machine.acknowledge_fallback(
        request.engine_epoch(),
        request.task_epoch(),
        request.request_sequence(),
    )?;
    machine.begin_reinitialization(fault.reset_request)?;
    assert_eq!(machine.state(), TaskState::Reinitializing);

    clock.set(9);
    task.reset(
        fault.reset_request,
        &mut AllowReset(fault.reset_request),
        &mut plan,
        &clock,
        |_| Ok(()),
    )?;
    assert_eq!(
        machine.complete_reinitialization(&task)?,
        TaskState::Running
    );
    assert_eq!(machine.mailbox_state(), FallbackMailboxState::Empty);
    assert_eq!(machine.statistics().scheduled_releases, 0);

    clock.set(13);
    let decision = selected(&mut plan, &clock)?;
    let release = decision.release_sequence();
    let CycleStart::Execute(cycle) = task.begin(decision, &clock, ScheduleControl::Continue)?
    else {
        return Err("expected post-reset cycle".into());
    };
    let next_fault = cycle.discard(FaultReason::TaskExecutionFault);
    machine.synchronize_fault(&task, release, clock.now())?;
    let next_request = machine.fallback_request().ok_or("missing next fallback")?;
    assert_eq!(next_fault.reset_request.task_epoch.get(), 2);
    assert_eq!(next_request.request_sequence().get(), 1);
    Ok(())
}

#[test]
fn failed_reinitialization_replaces_acknowledged_request_once() -> TestResult {
    let (mut task, mut machine, mut plan, clock) = setup(0, policy(4, 2, 3)?)?;
    let fault = task.lock_fault(FaultReason::TaskExecutionFault);
    machine.synchronize_fault(&task, ReleaseSequence::ZERO, clock.now())?;
    let request = machine.fallback_request().ok_or("missing fallback")?;
    machine.acknowledge_fallback(
        request.engine_epoch(),
        request.task_epoch(),
        request.request_sequence(),
    )?;
    machine.begin_reinitialization(fault.reset_request)?;

    assert_eq!(
        task.reset(
            fault.reset_request,
            &mut AllowReset(fault.reset_request),
            &mut plan,
            &clock,
            |_| Err(crate::InitializationRejected),
        ),
        Err(TransactionError::ReinitializationFailed)
    );
    assert_eq!(
        machine.synchronize_fault(&task, ReleaseSequence::ZERO, clock.now())?,
        TaskState::FaultLocked
    );
    let replacement = machine
        .fallback_request()
        .ok_or("missing replacement fallback")?;
    assert_eq!(machine.mailbox_state(), FallbackMailboxState::Pending);
    assert_eq!(replacement.request_sequence().get(), 1);
    assert_eq!(replacement.reason(), FaultReason::ReinitializationFailed);
    machine.synchronize_fault(&task, ReleaseSequence::new(9), clock.now())?;
    assert_eq!(machine.fallback_request(), Some(replacement));
    Ok(())
}

#[test]
fn real_budget_and_deadline_results_drive_exact_health_transitions() -> TestResult {
    let (mut task, mut machine, mut plan, clock) = setup(0, policy(4, 3, 4)?)?;
    let decision = selected(&mut plan, &clock)?;
    let commit = execute_and_finish(&mut task, decision, &clock, 6)?;
    assert!(commit.checkpoint.execution_budget_exceeded());
    assert_eq!(machine.record_commit(&task, commit)?, TaskState::Degraded);
    assert_eq!(machine.statistics().deadline_misses, 0);

    clock.set(13);
    let decision = selected(&mut plan, &clock)?;
    let commit = execute_and_finish(&mut task, decision, &clock, 14)?;
    assert_eq!(machine.record_commit(&task, commit)?, TaskState::Running);

    let (mut late_start, mut late_machine, mut late_plan, late_clock) = setup(1, policy(4, 3, 4)?)?;
    late_clock.set(12);
    let decision = selected(&mut late_plan, &late_clock)?;
    let release = decision.release_sequence();
    assert!(matches!(
        late_start.begin(decision, &late_clock, ScheduleControl::Continue)?,
        CycleStart::StartAfterDeadline
    ));
    assert_eq!(
        late_machine.record_deadline_miss(
            &mut late_start,
            MissOutcome::StartAfterDeadline,
            release,
            late_clock.now(),
        )?,
        TaskState::Degraded
    );

    let (mut late_finish, mut finish_machine, mut finish_plan, finish_clock) =
        setup(2, policy(4, 3, 4)?)?;
    finish_clock.set(10);
    let decision = selected(&mut finish_plan, &finish_clock)?;
    let release = decision.release_sequence();
    let CycleStart::Execute(cycle) =
        late_finish.begin(decision, &finish_clock, ScheduleControl::Continue)?
    else {
        return Err("expected late-start executable cycle".into());
    };
    finish_clock.set(12);
    assert_eq!(
        cycle.finish(&finish_clock),
        Err(TransactionError::DeadlineMissed)
    );
    assert_eq!(
        finish_machine.record_deadline_miss(
            &mut late_finish,
            MissOutcome::FinishAfterDeadline,
            release,
            finish_clock.now(),
        )?,
        TaskState::Degraded
    );
    Ok(())
}

#[test]
fn fallback_publication_failure_is_sticky_and_never_reports_success() -> TestResult {
    let (mut task, mut machine, _plan, clock) = setup(0, policy(4, 2, 3)?)?;
    machine.next_request_sequence = None;
    task.lock_fault(FaultReason::TaskExecutionFault);
    assert_eq!(
        machine.synchronize_fault(&task, ReleaseSequence::ZERO, clock.now()),
        Err(TaskStateMachineError::FallbackPublicationFault)
    );
    assert_eq!(machine.state(), TaskState::FaultLocked);
    assert!(machine.engine_faulted());
    assert!(machine.fallback_request().is_none());
    Ok(())
}

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

struct AllowReset(ResetRequest);

impl ResetGuard for AllowReset {
    fn check(&mut self, request: ResetRequest) -> Result<(), ResetGuardError> {
        if request == self.0 {
            Ok(())
        } else {
            Err(ResetGuardError::Unauthorized)
        }
    }
}

fn setup(
    handle: u32,
    miss_policy: MissPolicy,
) -> Result<(TaskTransaction, TaskStateMachine, StaticTaskPlan, TestClock), Box<dyn Error>> {
    let engine_epoch = epoch()?;
    let task_spec = spec(handle, miss_policy)?;
    let task = TaskTransaction::new(task_spec, engine_epoch, &[1], &[2], limits()?)?;
    let machine = TaskStateMachine::new(
        &task,
        OutputSetIdentity::from_sha256([handle.to_le_bytes()[0]; 32]),
    )?;
    let plan = plan(&[task_spec], MonotonicTimestamp::new(engine_epoch, 0))?;
    let clock = TestClock(Cell::new(MonotonicTimestamp::new(engine_epoch, 3)));
    Ok((task, machine, plan, clock))
}

fn policy(window: u32, max_misses: u32, consecutive: u32) -> Result<MissPolicy, Box<dyn Error>> {
    Ok(MissPolicy::new(
        MissWindow::new(window, window)?,
        max_misses,
        consecutive,
    )?)
}

fn spec(handle: u32, miss_policy: MissPolicy) -> Result<TaskSpec, Box<dyn Error>> {
    Ok(TaskSpec::new(
        ExecutionContractVersion::V1_0,
        LocalHandle::new(handle)?,
        TaskPriority::new(0),
        TaskTiming::new(
            TaskPeriodNanos::new(10)?,
            TaskPhaseNanos::new(3),
            RelativeDeadlineNanos::new(8)?,
            ExecutionBudgetNanos::new(2)?,
            HardLimitNanos::new(5)?,
        )?,
        miss_policy,
    ))
}

fn selected<'plan>(
    plan: &'plan mut StaticTaskPlan,
    clock: &TestClock,
) -> Result<crate::ReleaseDecision<'plan>, Box<dyn Error>> {
    match plan.observe(clock, ScheduleControl::Continue)? {
        ScheduleAction::Release(decision) => Ok(decision),
        _ => Err("expected release".into()),
    }
}

fn execute_and_finish(
    task: &mut TaskTransaction,
    decision: crate::ReleaseDecision<'_>,
    clock: &TestClock,
    finish_nanos: u64,
) -> Result<CycleCommit, Box<dyn Error>> {
    let CycleStart::Execute(cycle) = task.begin(decision, clock, ScheduleControl::Continue)? else {
        return Err("expected executable cycle".into());
    };
    clock.set(finish_nanos);
    cycle.finish(clock).map_err(Into::into)
}

fn limits() -> Result<WorkSetLimits, WorkSetError> {
    Ok(WorkSetLimits::new(WorkSetCapacity::new(16)?, 4096))
}

fn plan(
    specs: &[TaskSpec],
    start: MonotonicTimestamp,
) -> Result<StaticTaskPlan, crate::SchedulerError> {
    let mut builder =
        StaticTaskPlanBuilder::new(start, WorkSetCapacity::new(specs.len())?, limits()?)?;
    for task_spec in specs {
        builder.add_task(*task_spec)?;
    }
    builder.seal()
}

fn epoch() -> Result<BootEpochId, aurora_types::IdentifierError> {
    BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x8f,
    ])
}
