use std::cell::Cell;
use std::error::Error;

use aurora_control_contracts::{
    ExecutionBudgetNanos, ExecutionContractVersion, HardLimitNanos, MissPolicy, MissWindow,
    RelativeDeadlineNanos, ReleaseSequence, TaskPeriodNanos, TaskPhaseNanos, TaskPriority,
    TaskTiming,
};
use aurora_types::MonotonicTimestamp;

use super::*;
use crate::{ScheduleAction, StaticTaskPlanBuilder};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn deadline_miss_does_not_mask_later_hard_limit_or_clock_fault() -> TestResult {
    for clock_fault in [false, true] {
        let (mut task, mut plan, clock) = setup()?;
        clock.set(9);
        let mut cycle = begin(&mut task, &mut plan, &clock)?;
        cycle.write_output(WorkSetIndex::new(0), 99)?;
        clock.set(12);
        assert_eq!(
            cycle.checkpoint(&clock),
            Err(TransactionError::DeadlineMissed)
        );
        clock.set(if clock_fault { 11 } else { 15 });
        assert!(cycle.finish(&clock).is_err());
        assert_eq!(
            task.fault().map(|fault| fault.reason),
            Some(if clock_fault {
                FaultReason::ClockContractViolation
            } else {
                FaultReason::HardLimitExceeded
            })
        );
        assert_eq!(image(&task)?, [1, 2, 3, 4]);
        assert!(task.publishable().is_none());
    }
    Ok(())
}

#[test]
fn caught_task_unwind_cannot_commit_partial_state() -> TestResult {
    let (mut task, mut plan, clock) = setup()?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cycle.execute(|cycle| {
            cycle
                .write_state(WorkSetIndex::new(0), 99)
                .map_err(|_| FaultReason::CapacityExceeded)?;
            std::panic::resume_unwind(Box::new("injected task unwind"));
        })
    }));
    assert!(caught.is_err());
    let called = Cell::new(false);
    assert!(
        cycle
            .execute(|_| {
                called.set(true);
                Ok(())
            })
            .is_err()
    );
    assert!(!called.get());
    assert!(cycle.write_output(WorkSetIndex::new(0), 50).is_err());
    assert!(cycle.finish(&clock).is_err());
    assert_eq!(
        task.fault().map(|fault| fault.reason),
        Some(FaultReason::TaskExecutionFault)
    );
    assert_eq!(image(&task)?, [1, 2, 3, 4]);
    Ok(())
}

#[test]
fn initialization_unwind_invalidates_the_old_reset_request() -> TestResult {
    let (mut task, mut plan, clock) = setup()?;
    let request = task
        .lock_fault(FaultReason::TaskExecutionFault)
        .reset_request;
    let mut guard = Guard::new(request);
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        task.reset(request, &mut guard, &mut plan, &clock, |_| {
            std::panic::resume_unwind(Box::new("injected initializer unwind"));
        })
    }));
    assert!(caught.is_err());
    assert_eq!(
        task.fault().map(|fault| fault.reason),
        Some(FaultReason::ReinitializationFailed)
    );
    assert_eq!(
        task.reset(request, &mut guard, &mut plan, &clock, |_| Ok(())),
        Err(TransactionError::StaleResetRequest)
    );
    assert_eq!(image(&task)?, [1, 2, 3, 4]);
    assert_eq!(task.diagnostic().version().task_epoch.get(), 1);
    let next_request = task
        .fault()
        .ok_or("missing initialization fault")?
        .reset_request;
    assert_eq!(
        next_request.fault_generation.get(),
        request.fault_generation.get() + 1
    );
    task.reset(
        next_request,
        &mut Guard::new(next_request),
        &mut plan,
        &clock,
        |_| Ok(()),
    )?;
    assert_eq!(task.diagnostic().version().task_epoch.get(), 2);
    Ok(())
}

#[test]
fn caught_checkpoint_unwind_locks_the_task_without_committing() -> TestResult {
    struct UnwindingClock;
    impl MonotonicClock for UnwindingClock {
        fn now(&self) -> MonotonicTimestamp {
            std::panic::resume_unwind(Box::new("injected clock unwind"));
        }
    }
    let (mut task, mut plan, clock) = setup()?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    cycle.write_output(WorkSetIndex::new(0), 99)?;
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cycle.checkpoint(&UnwindingClock)
    }));
    assert!(caught.is_err());
    assert!(cycle.finish(&clock).is_err());
    assert_eq!(
        task.fault().map(|fault| fault.reason),
        Some(FaultReason::ClockContractViolation)
    );
    assert_eq!(image(&task)?, [1, 2, 3, 4]);
    Ok(())
}

#[test]
fn missed_cycle_finish_updates_shared_clock_history_without_false_fault() -> TestResult {
    let (mut task, mut plan, clock) = setup()?;
    clock.set(9);
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    clock.set(12);
    assert_eq!(
        cycle.checkpoint(&clock),
        Err(TransactionError::DeadlineMissed)
    );
    clock.set(14); // 恰好 HardLimit，不转 Fault，但返回点时间必须保留。
    assert_eq!(cycle.finish(&clock), Err(TransactionError::DeadlineMissed));
    assert!(task.fault().is_none());
    assert!(task.publishable().is_none());
    clock.set(13);
    assert!(matches!(
        plan.observe(&clock, ScheduleControl::Continue),
        Err(SchedulerError::ClockMovedBackwards {
            previous_elapsed_nanos: 14,
            observed_elapsed_nanos: 13
        })
    ));
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

struct Guard {
    expected: ResetRequest,
    denied: Option<ResetGuardError>,
    calls: usize,
}

impl Guard {
    fn new(expected: ResetRequest) -> Self {
        Self {
            expected,
            denied: None,
            calls: 0,
        }
    }
}

impl ResetGuard for Guard {
    fn check(&mut self, request: ResetRequest) -> Result<(), ResetGuardError> {
        self.calls += 1;
        if request != self.expected {
            return Err(ResetGuardError::Unauthorized);
        }
        match self.denied {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

fn epoch(last: u8) -> Result<BootEpochId, aurora_types::IdentifierError> {
    BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        last,
    ])
}

fn spec(handle: u32) -> Result<TaskSpec, Box<dyn Error>> {
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
        MissPolicy::new(MissWindow::new(4, 4)?, 2, 3)?,
    ))
}

fn limits() -> Result<WorkSetLimits, WorkSetError> {
    Ok(WorkSetLimits::new(WorkSetCapacity::new(16)?, 4096))
}

fn plan(specs: &[TaskSpec], start: MonotonicTimestamp) -> Result<StaticTaskPlan, SchedulerError> {
    let mut builder =
        StaticTaskPlanBuilder::new(start, WorkSetCapacity::new(specs.len())?, limits()?)?;
    for spec in specs {
        builder.add_task(*spec)?;
    }
    builder.seal()
}

fn setup() -> Result<(TaskTransaction, StaticTaskPlan, TestClock), Box<dyn Error>> {
    let epoch = epoch(1)?;
    let spec = spec(0)?;
    Ok((
        TaskTransaction::new(spec, epoch, &[1, 2], &[3, 4], limits()?)?,
        plan(&[spec], MonotonicTimestamp::new(epoch, 0))?,
        TestClock(Cell::new(MonotonicTimestamp::new(epoch, 3))),
    ))
}

fn selected<'a>(
    plan: &'a mut StaticTaskPlan,
    clock: &TestClock,
) -> Result<ReleaseDecision<'a>, Box<dyn Error>> {
    match plan.observe(clock, ScheduleControl::Continue)? {
        ScheduleAction::Release(value) => Ok(value),
        _ => Err("expected release".into()),
    }
}

fn begin<'task, 'plan>(
    task: &'task mut TaskTransaction,
    plan: &'plan mut StaticTaskPlan,
    clock: &TestClock,
) -> Result<CycleTransaction<'task, 'plan>, Box<dyn Error>> {
    match task.begin(selected(plan, clock)?, clock, ScheduleControl::Continue)? {
        CycleStart::Execute(value) => Ok(value),
        _ => Err("expected executable cycle".into()),
    }
}

fn image(task: &TaskTransaction) -> Result<[u8; 4], TransactionError> {
    let values = task.diagnostic().values();
    Ok([
        values.state(WorkSetIndex::new(0))?,
        values.state(WorkSetIndex::new(1))?,
        values.output(WorkSetIndex::new(0))?,
        values.output(WorkSetIndex::new(1))?,
    ])
}

#[test]
fn construction_validates_capacity_and_initial_values() -> TestResult {
    let (task, _, _) = setup()?;
    assert_eq!(image(&task)?, [1, 2, 3, 4]);
    assert_eq!(task.diagnostic().version().sequence, CommitSequence::ZERO);
    assert_eq!(task.diagnostic().values().state_len(), 2);
    assert_eq!(task.diagnostic().values().output_len(), 2);
    assert!(task.publishable().is_none());
    assert!(matches!(
        TaskTransaction::new(spec(0)?, epoch(1)?, &[], &[], limits()?),
        Err(TransactionError::WorkSet(
            WorkSetError::InvalidCapacity { .. }
        ))
    ));
    assert!(matches!(
        TaskTransaction::new(spec(0)?, epoch(1)?, &[0; 17], &[], limits()?),
        Err(TransactionError::WorkSet(
            WorkSetError::CapacityExceedsLimit { .. }
        ))
    ));
    assert!(matches!(
        TaskTransaction::new(
            spec(0)?,
            epoch(1)?,
            &[0],
            &[],
            WorkSetLimits::new(WorkSetCapacity::new(1)?, 0)
        ),
        Err(TransactionError::WorkSet(
            WorkSetError::ResourceBudgetExceeded { .. }
        ))
    ));
    for (state, output) in [(&[][..], &[5][..]), (&[5][..], &[][..])] {
        let task = TaskTransaction::new(spec(0)?, epoch(1)?, state, output, limits()?)?;
        let values = task.diagnostic().values();
        assert_eq!(values.state_len(), state.len());
        assert_eq!(values.output_len(), output.len());
        assert_eq!(
            values.state(WorkSetIndex::new(1)),
            Err(TransactionError::ImageOutOfRange)
        );
        assert_eq!(
            values.output(WorkSetIndex::new(usize::MAX)),
            Err(TransactionError::ImageOutOfRange)
        );
    }
    Ok(())
}

#[test]
fn success_swaps_one_version_and_preserves_unwritten_bytes() -> TestResult {
    let (mut task, mut plan, clock) = setup()?;
    let storage = std::ptr::from_ref(task.bytes.get(WorkSetIndex::new(0))?);
    let allocated = task.bytes.allocation_size_bytes();
    for iteration in 0..32_u8 {
        clock.set(3 + u64::from(iteration) * 10);
        let mut cycle = begin(&mut task, &mut plan, &clock)?;
        assert_eq!(cycle.read_state(WorkSetIndex::new(1))?, 2);
        cycle.execute(|cycle| {
            cycle
                .write_state(WorkSetIndex::new(0), iteration)
                .map_err(|_| FaultReason::CapacityExceeded)?;
            cycle
                .write_output(WorkSetIndex::new(0), iteration)
                .map_err(|_| FaultReason::CapacityExceeded)
        })?;
        let committed = cycle.finish(&clock)?;
        assert_eq!(committed.version.sequence.get(), u64::from(iteration) + 1);
        assert_eq!(image(&task)?, [iteration, 2, iteration, 4]);
        assert_eq!(
            task.publishable().ok_or("missing output")?.version(),
            committed.version
        );
        assert_eq!(
            std::ptr::from_ref(task.bytes.get(WorkSetIndex::new(0))?),
            storage
        );
        assert_eq!(task.bytes.allocation_size_bytes(), allocated);
    }
    Ok(())
}

#[test]
fn fault_at_every_update_boundary_preserves_previous_whole_commit() -> TestResult {
    for fault_step in 0..=4 {
        let (mut task, mut plan, clock) = setup()?;
        let mut cycle = begin(&mut task, &mut plan, &clock)?;
        cycle.write_state(WorkSetIndex::new(0), 10)?;
        cycle.write_output(WorkSetIndex::new(0), 30)?;
        cycle.finish(&clock)?;
        let previous = image(&task)?;
        let version = task.diagnostic().version();
        clock.set(13);
        let mut cycle = begin(&mut task, &mut plan, &clock)?;
        for step in 0..fault_step {
            if step < 2 {
                cycle.write_state(WorkSetIndex::new(step), 99)?;
            } else {
                cycle.write_output(WorkSetIndex::new(step - 2), 99)?;
            }
        }
        assert!(
            cycle
                .execute(|_| Err(FaultReason::TaskExecutionFault))
                .is_err()
        );
        let called = Cell::new(false);
        assert!(
            cycle
                .execute(|_| {
                    called.set(true);
                    Ok(())
                })
                .is_err()
        );
        assert!(!called.get());
        assert!(cycle.read_state(WorkSetIndex::new(0)).is_err());
        assert!(cycle.write_state(WorkSetIndex::new(0), 50).is_err());
        assert!(cycle.finish(&clock).is_err());
        assert_eq!(image(&task)?, previous);
        assert_eq!(task.diagnostic().version(), version);
        assert!(task.publishable().is_none());
        clock.set(23);
        assert!(matches!(
            task.begin(
                selected(&mut plan, &clock)?,
                &clock,
                ScheduleControl::Continue
            ),
            Err(TransactionError::FaultLocked(_))
        ));
    }
    Ok(())
}

#[test]
fn capacity_fault_is_sticky_even_when_task_ignores_the_error() -> TestResult {
    for output in [false, true] {
        for index in [2, usize::MAX] {
            let (mut task, mut plan, clock) = setup()?;
            let mut cycle = begin(&mut task, &mut plan, &clock)?;
            cycle.write_state(WorkSetIndex::new(0), 9)?;
            let result = if output {
                cycle.write_output(WorkSetIndex::new(index), 9)
            } else {
                cycle.write_state(WorkSetIndex::new(index), 9)
            };
            assert_eq!(result, Err(TransactionError::ImageOutOfRange));
            assert!(cycle.finish(&clock).is_err());
            assert_eq!(image(&task)?, [1, 2, 3, 4]);
            assert_eq!(
                task.fault().ok_or("missing fault")?.reason,
                FaultReason::CapacityExceeded
            );
        }
    }
    Ok(())
}

#[test]
fn drop_and_explicit_discard_lock_without_partial_publication() -> TestResult {
    for explicit in [false, true] {
        let (mut task, mut plan, clock) = setup()?;
        let mut cycle = begin(&mut task, &mut plan, &clock)?;
        cycle.write_output(WorkSetIndex::new(1), 77)?;
        if explicit {
            let fault = cycle.discard(FaultReason::TaskExecutionFault);
            assert_eq!(fault.reason, FaultReason::TaskExecutionFault);
        } else {
            drop(cycle);
        }
        assert_eq!(image(&task)?, [1, 2, 3, 4]);
        assert!(!task.active);
        assert!(task.publishable().is_none());
        let fault = task.fault().ok_or("missing fault")?;
        assert_eq!(task.lock_fault(FaultReason::HardLimitExceeded), fault);
    }
    Ok(())
}

#[test]
fn forgotten_transaction_cannot_reuse_half_updated_staging() -> TestResult {
    let (mut task, mut plan, clock) = setup()?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    cycle.write_state(WorkSetIndex::new(0), 99)?;
    std::mem::forget(cycle);
    assert!(task.publishable().is_none());
    clock.set(13);
    assert!(matches!(
        task.begin(
            selected(&mut plan, &clock)?,
            &clock,
            ScheduleControl::Continue
        ),
        Err(TransactionError::CycleAlreadyActive)
    ));
    assert_eq!(image(&task)?, [1, 2, 3, 4]);
    assert!(task.fault().is_some());
    Ok(())
}

#[test]
fn final_checkpoint_enforces_budget_hard_limit_and_deadline_edges() -> TestResult {
    for (start, finish, commit, budget, fault) in [
        (3, 5, true, false, false),
        (3, 6, true, true, false),
        (3, 8, true, true, false),
        (3, 9, false, true, true),
        (9, 11, true, false, false),
        (9, 12, false, true, false),
        (9, 15, false, true, true),
    ] {
        let (mut task, mut plan, clock) = setup()?;
        clock.set(start);
        let mut cycle = begin(&mut task, &mut plan, &clock)?;
        cycle.write_state(WorkSetIndex::new(0), 9)?;
        clock.set(finish);
        let result = cycle.finish(&clock);
        assert_eq!(result.is_ok(), commit);
        if let Ok(result) = result {
            assert_eq!(result.checkpoint.execution_budget_exceeded(), budget);
        }
        assert_eq!(task.fault().is_some(), fault);
        assert_eq!(image(&task)?[0], if commit { 9 } else { 1 });
        assert_eq!(task.publishable().is_some(), commit);
    }
    Ok(())
}

#[test]
fn deadline_discard_restores_previous_state_on_next_cycle() -> TestResult {
    let (mut task, mut plan, clock) = setup()?;
    clock.set(9);
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    cycle.write_output(WorkSetIndex::new(0), 99)?;
    clock.set(12);
    assert_eq!(
        cycle.checkpoint(&clock),
        Err(TransactionError::DeadlineMissed)
    );
    assert!(cycle.write_output(WorkSetIndex::new(0), 88).is_err());
    assert_eq!(cycle.finish(&clock), Err(TransactionError::DeadlineMissed));
    assert!(task.fault().is_none());
    assert!(task.publishable().is_none());
    clock.set(13);
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    assert_eq!(cycle.read_output(WorkSetIndex::new(0))?, 3);
    cycle.finish(&clock)?;
    assert_eq!(task.diagnostic().version().sequence.get(), 1);
    Ok(())
}

#[test]
fn begin_rechecks_actual_time_and_stop_without_executing() -> TestResult {
    for stop in [false, true] {
        let (mut task, mut plan, clock) = setup()?;
        let release = selected(&mut plan, &clock)?;
        clock.set(12);
        let result = task.begin(
            release,
            &clock,
            if stop {
                ScheduleControl::StopRequested
            } else {
                ScheduleControl::Continue
            },
        )?;
        if stop {
            assert!(matches!(result, CycleStart::Stopped));
        } else {
            assert!(matches!(result, CycleStart::StartAfterDeadline));
        }
        drop(result);
        assert_eq!(image(&task)?, [1, 2, 3, 4]);
        assert!(!task.active);
        assert!(task.fault().is_none());
    }
    Ok(())
}

#[test]
fn clock_failures_at_begin_and_during_execution_latch_fault() -> TestResult {
    for at_begin in [false, true] {
        for wrong_epoch in [false, true] {
            let (mut task, mut plan, clock) = setup()?;
            let release = selected(&mut plan, &clock)?;
            let invalid =
                MonotonicTimestamp::new(if wrong_epoch { epoch(2)? } else { epoch(1)? }, 2);
            if at_begin {
                clock.0.set(invalid);
                assert!(matches!(
                    task.begin(release, &clock, ScheduleControl::Continue),
                    Err(TransactionError::Scheduler(_))
                ));
            } else {
                let CycleStart::Execute(mut cycle) =
                    task.begin(release, &clock, ScheduleControl::Continue)?
                else {
                    return Err("no cycle".into());
                };
                clock.0.set(invalid);
                assert!(matches!(
                    cycle.checkpoint(&clock),
                    Err(TransactionError::Scheduler(_))
                ));
                assert!(cycle.finish(&clock).is_err());
            }
            assert_eq!(
                task.fault().ok_or("missing fault")?.reason,
                FaultReason::ClockContractViolation
            );
            assert_eq!(image(&task)?, [1, 2, 3, 4]);
        }
    }
    Ok(())
}

#[test]
fn release_identity_mismatch_never_changes_bank() -> TestResult {
    for variant in 0..3 {
        let (mut task, mut plan, clock) = setup()?;
        match variant {
            0 => task.spec = spec(1)?,
            1 => task.engine_epoch = epoch(2)?,
            _ => task.versions[0].task_epoch = TaskEpoch::new(2)?,
        }
        assert!(matches!(
            task.begin(
                selected(&mut plan, &clock)?,
                &clock,
                ScheduleControl::Continue
            ),
            Err(TransactionError::PlanMismatch)
        ));
        assert_eq!(image(&task)?, [1, 2, 3, 4]);
        assert!(task.fault().is_none());
    }
    Ok(())
}

#[test]
fn reset_identity_and_external_guard_are_required_before_initialization() -> TestResult {
    let (mut task, mut plan, clock) = setup()?;
    let request = task
        .lock_fault(FaultReason::TaskExecutionFault)
        .reset_request;
    let mut guard = Guard::new(request);
    let initialized = Cell::new(false);
    for stale in [
        ResetRequest {
            engine_epoch: epoch(2)?,
            ..request
        },
        ResetRequest {
            task_handle: LocalHandle::new(1)?,
            ..request
        },
        ResetRequest {
            task_epoch: TaskEpoch::new(2)?,
            ..request
        },
        ResetRequest {
            fault_generation: FaultGeneration::new(2)?,
            ..request
        },
    ] {
        assert_eq!(
            task.reset(stale, &mut guard, &mut plan, &clock, |_| {
                initialized.set(true);
                Ok(())
            }),
            Err(TransactionError::StaleResetRequest)
        );
    }
    assert_eq!(guard.calls, 0);
    for denial in [
        ResetGuardError::Unauthorized,
        ResetGuardError::FallbackNotReady,
    ] {
        guard.denied = Some(denial);
        assert_eq!(
            task.reset(request, &mut guard, &mut plan, &clock, |_| {
                initialized.set(true);
                Ok(())
            }),
            Err(TransactionError::ResetDenied(denial))
        );
    }
    assert!(!initialized.get());
    assert_eq!(image(&task)?, [1, 2, 3, 4]);
    guard.denied = None;
    task.reset(request, &mut guard, &mut plan, &clock, |_| Ok(()))?;
    assert_eq!(
        task.reset(request, &mut guard, &mut plan, &clock, |_| Ok(())),
        Err(TransactionError::StaleResetRequest)
    );
    Ok(())
}

#[test]
fn reset_restores_declarations_and_rejects_old_fault_generation() -> TestResult {
    let (mut task, mut plan, clock) = setup()?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    cycle.write_state(WorkSetIndex::new(0), 20)?;
    cycle.write_output(WorkSetIndex::new(0), 30)?;
    cycle.finish(&clock)?;
    clock.set(13);
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    cycle.write_state(WorkSetIndex::new(1), 99)?;
    let request = cycle.discard(FaultReason::TaskExecutionFault).reset_request;
    let mut guard = Guard::new(request);
    let version = task.reset(request, &mut guard, &mut plan, &clock, |values| {
        assert_eq!(values.state(WorkSetIndex::new(0)), Ok(1));
        assert_eq!(values.state(WorkSetIndex::new(1)), Ok(2));
        assert_eq!(values.output(WorkSetIndex::new(0)), Ok(3));
        Ok(())
    })?;
    assert_eq!(version.task_epoch.get(), 2);
    assert_eq!(version.sequence, CommitSequence::ZERO);
    assert_eq!(image(&task)?, [1, 2, 3, 4]);
    assert!(task.publishable().is_none());
    clock.set(23);
    begin(&mut task, &mut plan, &clock)?.finish(&clock)?;
    let next = task
        .lock_fault(FaultReason::TaskExecutionFault)
        .reset_request;
    assert_eq!(next.task_epoch.get(), 2);
    assert_eq!(
        next.fault_generation.get(),
        request.fault_generation.get() + 1
    );
    assert_eq!(
        task.reset(request, &mut guard, &mut plan, &clock, |_| Ok(())),
        Err(TransactionError::StaleResetRequest)
    );
    Ok(())
}

#[test]
fn failed_initialization_keeps_commit_and_invalidates_reset_request() -> TestResult {
    let (mut task, mut plan, clock) = setup()?;
    let mut cycle = begin(&mut task, &mut plan, &clock)?;
    cycle.write_state(WorkSetIndex::new(0), 9)?;
    cycle.finish(&clock)?;
    let version = task.diagnostic().version();
    let request = task
        .lock_fault(FaultReason::TaskExecutionFault)
        .reset_request;
    let mut guard = Guard::new(request);
    assert_eq!(
        task.reset(request, &mut guard, &mut plan, &clock, |_| Err(
            InitializationRejected
        )),
        Err(TransactionError::ReinitializationFailed)
    );
    assert_eq!(task.diagnostic().version(), version);
    assert_eq!(image(&task)?, [9, 2, 3, 4]);
    let fault = task.fault().ok_or("missing fault")?;
    assert_eq!(fault.reason, FaultReason::ReinitializationFailed);
    assert_ne!(fault.reset_request, request);
    assert_eq!(
        task.reset(request, &mut guard, &mut plan, &clock, |_| Ok(())),
        Err(TransactionError::StaleResetRequest)
    );
    let mut guard = Guard::new(fault.reset_request);
    task.reset(fault.reset_request, &mut guard, &mut plan, &clock, |_| {
        Ok(())
    })?;
    assert_eq!(image(&task)?, [1, 2, 3, 4]);
    Ok(())
}

#[test]
fn reset_uses_first_strictly_future_grid_release_with_sequence_zero() -> TestResult {
    for (completed_at, expected) in [
        (0, 3),
        (2, 3),
        (3, 13),
        (4, 13),
        (12, 13),
        (13, 23),
        (14, 23),
        (1003, 1013),
    ] {
        let (mut task, mut plan, clock) = setup()?;
        let request = task
            .lock_fault(FaultReason::TaskExecutionFault)
            .reset_request;
        clock.set(completed_at);
        task.reset(request, &mut Guard::new(request), &mut plan, &clock, |_| {
            Ok(())
        })?;
        assert!(matches!(plan.observe(&clock, ScheduleControl::Continue)?,
            ScheduleAction::WaitUntil { release } if release.elapsed_nanos() == expected));
        clock.set(expected);
        let release = selected(&mut plan, &clock)?;
        assert_eq!(release.release_sequence(), ReleaseSequence::ZERO);
        assert_eq!(release.skipped_releases(), None);
        assert_eq!(release.scheduled_release().elapsed_nanos(), expected);
        let CycleStart::Execute(cycle) = task.begin(release, &clock, ScheduleControl::Continue)?
        else {
            return Err("not executable".into());
        };
        let commit = cycle.finish(&clock)?;
        assert_eq!(commit.version.task_epoch.get(), 2);
    }
    Ok(())
}

#[test]
fn reset_reads_clock_after_initializer_and_counts_only_new_epoch_skips() -> TestResult {
    let (mut task, mut plan, clock) = setup()?;
    let request = task
        .lock_fault(FaultReason::TaskExecutionFault)
        .reset_request;
    clock.set(4);
    task.reset(request, &mut Guard::new(request), &mut plan, &clock, |_| {
        clock.set(23);
        Ok(())
    })?;
    assert!(matches!(plan.observe(&clock, ScheduleControl::Continue)?,
        ScheduleAction::WaitUntil { release } if release.elapsed_nanos() == 33));
    clock.set(53);
    let release = selected(&mut plan, &clock)?;
    assert_eq!(release.release_sequence().get(), 2);
    let skipped = release.skipped_releases().ok_or("missing skips")?;
    assert_eq!(
        (skipped.first().get(), skipped.last().get(), skipped.count()),
        (0, 1, 2)
    );
    Ok(())
}

#[test]
fn reset_rejects_wrong_plan_stop_clock_and_unrepresentable_future() -> TestResult {
    for variant in 0..7 {
        let (mut task, mut task_plan, clock) = setup()?;
        let request = task
            .lock_fault(FaultReason::TaskExecutionFault)
            .reset_request;
        match variant {
            0 => task_plan = plan(&[spec(1)?], MonotonicTimestamp::new(epoch(1)?, 0))?,
            1 => task_plan = plan(&[spec(0)?], MonotonicTimestamp::new(epoch(2)?, 0))?,
            2 => {
                task_plan.observe(&clock, ScheduleControl::StopRequested)?;
            }
            3 => clock.0.set(MonotonicTimestamp::new(epoch(2)?, 3)),
            4 => {
                let _release = selected(&mut task_plan, &clock)?;
                clock.set(2);
            }
            5 => clock.set(u64::MAX),
            _ => clock.set(u64::MAX - 3),
        }
        let version = task.diagnostic().version();
        assert!(
            task.reset(
                request,
                &mut Guard::new(request),
                &mut task_plan,
                &clock,
                |_| Ok(())
            )
            .is_err()
        );
        assert_eq!(task.diagnostic().version(), version);
        assert_eq!(image(&task)?, [1, 2, 3, 4]);
        assert!(task.fault().is_some());
    }
    Ok(())
}

#[test]
fn commit_epoch_and_fault_generation_cannot_wrap() -> TestResult {
    let (mut task, mut plan, clock) = setup()?;
    task.versions[0].sequence = CommitSequence::new(u64::MAX);
    let cycle = begin(&mut task, &mut plan, &clock)?;
    assert_eq!(cycle.finish(&clock), Err(TransactionError::CounterOverflow));
    assert_eq!(task.diagnostic().version().sequence.get(), u64::MAX);
    assert_eq!(
        task.fault().ok_or("missing fault")?.reason,
        FaultReason::CounterOverflow
    );
    for epoch_exhausted in [false, true] {
        let (mut task, mut plan, clock) = setup()?;
        if epoch_exhausted {
            task.versions[0].task_epoch = TaskEpoch::new(u64::MAX)?;
        } else {
            task.fault_generation = FaultGeneration::new(u64::MAX)?;
        }
        let request = task
            .lock_fault(FaultReason::TaskExecutionFault)
            .reset_request;
        assert_eq!(
            task.reset(
                request,
                &mut Guard::new(request),
                &mut plan,
                &clock,
                |_| Ok(())
            ),
            Err(TransactionError::CounterOverflow)
        );
        assert_eq!(task.diagnostic().version().task_epoch, request.task_epoch);
    }
    Ok(())
}

#[test]
fn faulted_task_does_not_block_healthy_task_transaction() -> TestResult {
    let (mut faulted, _, clock) = setup()?;
    let mut healthy = TaskTransaction::new(spec(1)?, epoch(1)?, &[5, 6], &[7, 8], limits()?)?;
    let mut plan = plan(&[spec(0)?, spec(1)?], MonotonicTimestamp::new(epoch(1)?, 0))?;
    let fault = begin(&mut faulted, &mut plan, &clock)?.discard(FaultReason::TaskExecutionFault);
    assert_eq!(faulted.fault(), Some(fault));
    begin(&mut healthy, &mut plan, &clock)?.finish(&clock)?;
    assert_eq!(image(&healthy)?, [5, 6, 7, 8]);
    clock.set(13);
    assert!(
        faulted
            .begin(
                selected(&mut plan, &clock)?,
                &clock,
                ScheduleControl::Continue
            )
            .is_err()
    );
    begin(&mut healthy, &mut plan, &clock)?.finish(&clock)?;
    assert_eq!(healthy.diagnostic().version().sequence.get(), 2);
    Ok(())
}

#[test]
fn error_conversion_and_display_preserve_explicit_categories() {
    let error = TransactionError::from(ExecutionContractError::CounterOverflow);
    assert_eq!(
        error,
        TransactionError::Contract(ExecutionContractError::CounterOverflow)
    );
    assert!(error.to_string().contains("CounterOverflow"));
    assert!(error.source().is_none());
}

#[test]
fn ignored_out_of_range_reads_also_prevent_commit() -> TestResult {
    for output in [false, true] {
        let (mut task, mut plan, clock) = setup()?;
        let mut cycle = begin(&mut task, &mut plan, &clock)?;
        let error = if output {
            cycle.read_output(WorkSetIndex::new(2))
        } else {
            cycle.read_state(WorkSetIndex::new(usize::MAX))
        };
        assert_eq!(error, Err(TransactionError::ImageOutOfRange));
        assert!(cycle.finish(&clock).is_err());
        assert_eq!(
            task.fault().ok_or("missing fault")?.reason,
            FaultReason::CapacityExceeded
        );
        assert_eq!(image(&task)?, [1, 2, 3, 4]);
    }
    Ok(())
}

#[test]
fn task_reset_does_not_change_other_task_grid_or_sequence() -> TestResult {
    let (mut task, _, clock) = setup()?;
    let mut plan = plan(&[spec(0)?, spec(1)?], MonotonicTimestamp::new(epoch(1)?, 0))?;
    let request = begin(&mut task, &mut plan, &clock)?
        .discard(FaultReason::TaskExecutionFault)
        .reset_request;
    let other = selected(&mut plan, &clock)?;
    assert_eq!(other.task().handle().get(), 1);
    assert_eq!(other.release_sequence().get(), 0);
    clock.set(23);
    task.reset(request, &mut Guard::new(request), &mut plan, &clock, |_| {
        Ok(())
    })?;
    let other = selected(&mut plan, &clock)?;
    assert_eq!(other.task().handle().get(), 1);
    assert_eq!(other.release_sequence().get(), 2);
    assert_eq!(other.scheduled_release().elapsed_nanos(), 23);
    assert_eq!(other.skipped_releases().ok_or("missing skip")?.count(), 1);
    clock.set(33);
    let resumed = selected(&mut plan, &clock)?;
    assert_eq!(resumed.task().handle().get(), 0);
    assert_eq!(resumed.release_sequence().get(), 0);
    Ok(())
}

#[test]
fn reset_ordinal_and_origin_overflow_fail_before_publication() -> TestResult {
    for origin_overflow in [false, true] {
        let engine_epoch = epoch(1)?;
        let (spec, start, now) = if origin_overflow {
            (spec(0)?, u64::MAX - 1, u64::MAX - 1)
        } else {
            let spec = TaskSpec::new(
                ExecutionContractVersion::V1_0,
                LocalHandle::ZERO,
                TaskPriority::new(0),
                TaskTiming::new(
                    TaskPeriodNanos::new(1)?,
                    TaskPhaseNanos::new(0),
                    RelativeDeadlineNanos::new(1)?,
                    ExecutionBudgetNanos::new(1)?,
                    HardLimitNanos::new(1)?,
                )?,
                MissPolicy::new(MissWindow::new(1, 1)?, 0, 1)?,
            );
            (spec, 0, u64::MAX)
        };
        let mut task = TaskTransaction::new(spec, engine_epoch, &[1], &[], limits()?)?;
        let mut plan = plan(&[spec], MonotonicTimestamp::new(engine_epoch, start))?;
        let clock = TestClock(Cell::new(MonotonicTimestamp::new(engine_epoch, now)));
        let request = task
            .lock_fault(FaultReason::ScheduleTimeOverflow)
            .reset_request;
        assert!(matches!(
            task.reset(
                request,
                &mut Guard::new(request),
                &mut plan,
                &clock,
                |_| Ok(())
            ),
            Err(TransactionError::Scheduler(
                SchedulerError::ScheduleTimeOverflow { .. }
            ))
        ));
        assert_eq!(task.diagnostic().version().task_epoch.get(), 1);
        assert!(task.publishable().is_none());
    }
    Ok(())
}
