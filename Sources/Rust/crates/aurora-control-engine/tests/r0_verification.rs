//! R0 跨组件确定性、并发、Fault 与恢复验收套件。

use std::cell::Cell;
use std::cmp::Reverse;
use std::error::Error;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Barrier};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use aurora_control_contracts::{
    CommitSequence, EventSequence, ExecutionBudgetNanos, ExecutionContractVersion, FaultReason,
    HardLimitNanos, MissPolicy, MissWindow, RelativeDeadlineNanos, ReleaseSequence,
    SnapshotMetadata, TaskEpoch, TaskPeriodNanos, TaskPhaseNanos, TaskPriority, TaskSpec,
    TaskState, TaskTiming, TraceCapacity, TraceCounters, TraceEventKind, TraceRecord,
    TraceRecordBytes, TraceTiming, UtcObservation,
};
use aurora_control_engine::{
    CycleStart, InitializationRejected, MonotonicClock, ReleaseDecision, ResetGuard,
    ResetGuardError, ResetRequest, ScheduleAction, ScheduleControl, SnapshotChannelDefinition,
    SnapshotChannelError, SnapshotPayloadCapacity, SnapshotPublisher, SnapshotReader, SpscCapacity,
    SpscOverflowPolicy, SpscPopError, SpscPushOutcome, StaticTaskPlan, StaticTaskPlanBuilder,
    TaskTransaction, WorkSetCapacity, WorkSetIndex, WorkSetLimits, bounded_spsc,
};
use aurora_test_support::ReplayRng;
use aurora_types::{
    BootEpochId, LocalHandle, MonotonicTimestamp, QualityCode, TimeQuality, TimeQualityState,
    TimeSource, UtcTimestamp,
};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const REPLAY_SEED: u64 = 0x8a13_59d7_24c6_e0f1;
const REPLAY_CYCLES: usize = 256;
const SCHEDULE_HORIZON_NANOS: u64 = 50_000;
const SNAPSHOT_PUBLICATIONS: u64 = 4_096;
const SNAPSHOT_PAYLOAD_BYTES: usize = 32;
const CONCURRENCY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
struct CycleEvidence {
    input: u8,
    state: u8,
    output: u8,
    commit_sequence: CommitSequence,
    trace: [u8; aurora_control_contracts::TRACE_RECORD_SIZE],
}

#[test]
fn fixed_seed_input_trace_replays_identical_state_output_and_diagnostics() -> TestResult {
    let mut generator = ReplayRng::from_seed(REPLAY_SEED);
    let mut inputs = Vec::with_capacity(REPLAY_CYCLES);
    for _ in 0..REPLAY_CYCLES {
        inputs.push(generator.next_u64().to_le_bytes()[0]);
    }
    assert_eq!(inputs.len(), REPLAY_CYCLES);

    let first = replay(&inputs)?;
    let second = replay(&inputs)?;
    assert_eq!(first.len(), REPLAY_CYCLES);
    assert_eq!(first, second);
    Ok(())
}

fn replay(inputs: &[u8]) -> TestResult<Vec<CycleEvidence>> {
    let engine_epoch = epoch(0x81)?;
    let spec = task_spec(0, 0, 10, 0)?;
    let clock = VerificationClock::new(engine_epoch);
    let mut plan = task_plan(&[spec], clock.now())?;
    let mut task = TaskTransaction::new(spec, engine_epoch, &[0], &[0], limits(2)?)?;
    let trace_capacity = TraceCapacity::new(u32::try_from(inputs.len())?, u32::MAX)?;
    let mut evidence = Vec::with_capacity(inputs.len());

    for (index, input) in inputs.iter().enumerate() {
        let sequence = u64::try_from(index)?;
        clock.set_elapsed(
            sequence
                .checked_mul(10)
                .ok_or_else(|| test_error("time overflow"))?,
        )?;
        let selected = release(&mut plan, &clock)?;
        let release_sequence = selected.release_sequence();
        let scheduled_release = selected.scheduled_release();
        let absolute_deadline = selected.absolute_deadline();
        assert_eq!(release_sequence.get(), sequence);

        let started_at = clock.now();
        let CycleStart::Execute(mut cycle) =
            task.begin(selected, &clock, ScheduleControl::Continue)?
        else {
            return Err(test_error("replay release was not executable"));
        };
        let previous_state = cycle.read_state(WorkSetIndex::new(0))?;
        let next_state = previous_state.wrapping_add(*input).wrapping_add(1);
        let next_output = next_state.rotate_left(1) ^ *input;
        cycle.write_state(WorkSetIndex::new(0), next_state)?;
        cycle.write_output(WorkSetIndex::new(0), next_output)?;
        let commit = cycle.finish(&clock)?;

        let values = task.diagnostic().values();
        let state = values.state(WorkSetIndex::new(0))?;
        let output = values.output(WorkSetIndex::new(0))?;
        let attempted = sequence
            .checked_add(1)
            .ok_or_else(|| test_error("attempt count overflow"))?;
        let counters = TraceCounters::new(trace_capacity, 0, 1, attempted, attempted, 0, 0, false)?;
        let timing = TraceTiming::new(
            scheduled_release,
            absolute_deadline,
            Some(started_at),
            Some(clock.now()),
        )?;
        let trace = TraceRecord::new(
            ExecutionContractVersion::V1_0,
            engine_epoch,
            spec.handle(),
            commit.version.task_epoch,
            EventSequence::new(sequence),
            release_sequence,
            CommitSequence::new(sequence),
            commit.version.sequence,
            TraceEventKind::ReleaseCompleted,
            timing,
            None,
            TaskState::Running,
            TaskState::Running,
            None,
            None,
            None,
            None,
            None,
            None,
            counters,
        )?;
        evidence.push(CycleEvidence {
            input: *input,
            state,
            output,
            commit_sequence: commit.version.sequence,
            trace: *TraceRecordBytes::encode(trace).as_bytes(),
        });
    }
    if evidence.len() != inputs.len() {
        return Err(test_error("replay generated too many or too few cycles"));
    }
    Ok(evidence)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScheduledEvidence {
    release_at: u64,
    priority: i16,
    handle: u32,
    sequence: u64,
}

#[test]
fn multiperiod_multiphase_stress_preserves_exact_order_across_utc_jumps() -> TestResult {
    let specs = [
        task_spec(9, -4, 7, 0)?,
        task_spec(3, 8, 11, 0)?,
        task_spec(7, 8, 11, 0)?,
        task_spec(1, 2, 17, 5)?,
        task_spec(5, 1, 100_000, SCHEDULE_HORIZON_NANOS)?,
        task_spec(6, 1, 100_000, SCHEDULE_HORIZON_NANOS + 1)?,
    ];
    let expected = expected_schedule(&specs)?;
    let baseline = observe_schedule(&specs, &expected, false)?;
    let utc_adjusted = observe_schedule(&specs, &expected, true)?;

    assert_eq!(baseline, expected);
    assert_eq!(utc_adjusted, expected);
    assert_eq!(baseline, utc_adjusted);
    Ok(())
}

fn expected_schedule(specs: &[TaskSpec]) -> TestResult<Vec<ScheduledEvidence>> {
    let expected_count = specs.iter().try_fold(0_usize, |total, spec| {
        let timing = spec.timing();
        let releases = releases_through_horizon(timing.period().get(), timing.phase().get());
        total
            .checked_add(usize::try_from(releases)?)
            .ok_or_else(|| test_error("expected schedule count overflow"))
    })?;
    let mut expected = Vec::with_capacity(expected_count);
    for spec in specs {
        let timing = spec.timing();
        let releases = releases_through_horizon(timing.period().get(), timing.phase().get());
        for sequence in 0..releases {
            let release_at = sequence
                .checked_mul(timing.period().get())
                .and_then(|offset| timing.phase().get().checked_add(offset))
                .ok_or_else(|| test_error("expected schedule time overflow"))?;
            expected.push(ScheduledEvidence {
                release_at,
                priority: spec.priority().get(),
                handle: spec.handle().get(),
                sequence,
            });
        }
    }
    expected.sort_by_key(|entry| (entry.release_at, Reverse(entry.priority), entry.handle));
    if expected.len() != expected_count {
        return Err(test_error("expected schedule generated an incorrect count"));
    }
    Ok(expected)
}

fn releases_through_horizon(period_nanos: u64, phase_nanos: u64) -> u64 {
    SCHEDULE_HORIZON_NANOS
        .checked_sub(phase_nanos)
        .map_or(0, |span| span / period_nanos + 1)
}

fn observe_schedule(
    specs: &[TaskSpec],
    expected: &[ScheduledEvidence],
    adjust_utc: bool,
) -> TestResult<Vec<ScheduledEvidence>> {
    let engine_epoch = epoch(if adjust_utc { 0x83 } else { 0x82 })?;
    let clock = VerificationClock::new(engine_epoch);
    let mut plan = task_plan(specs, clock.now())?;
    let mut observed = Vec::with_capacity(expected.len());

    for (index, entry) in expected.iter().enumerate() {
        clock.set_elapsed(entry.release_at)?;
        if adjust_utc && index == expected.len() / 3 {
            clock.set_utc(UtcTimestamp::new(-10_000, 999_999_999)?);
        }
        if adjust_utc && index == expected.len() * 2 / 3 {
            clock.set_utc(UtcTimestamp::new(4_000_000_000, 1)?);
        }
        let selected = release(&mut plan, &clock)?;
        observed.push(ScheduledEvidence {
            release_at: selected.scheduled_release().elapsed_nanos(),
            priority: selected.task().priority().get(),
            handle: selected.task().handle().get(),
            sequence: selected.release_sequence().get(),
        });
    }
    clock.set_elapsed(SCHEDULE_HORIZON_NANOS)?;
    assert!(matches!(
        plan.observe(&clock, ScheduleControl::Continue)?,
        ScheduleAction::WaitUntil { release }
            if release.elapsed_nanos() > SCHEDULE_HORIZON_NANOS
    ));
    if observed.len() != expected.len() {
        return Err(test_error(
            "scheduler generated too many or too few releases",
        ));
    }
    Ok(observed)
}

#[test]
fn snapshot_readers_are_tear_free_and_stalled_reader_cannot_block_writer() -> TestResult {
    let engine_epoch = epoch(0x84)?;
    let capacity = SnapshotPayloadCapacity::new(SNAPSHOT_PAYLOAD_BYTES, SNAPSHOT_PAYLOAD_BYTES)?;
    let publisher = SnapshotPublisher::new(SnapshotChannelDefinition::new(
        ExecutionContractVersion::V1_0,
        engine_epoch,
        [7; 32],
        capacity,
    ))?;
    let first_reader = publisher.create_reader()?;
    let second_reader = publisher.create_reader()?;
    let _stalled_reader = publisher.create_reader()?;
    let done = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(3));
    let first_observations = Arc::new(AtomicU64::new(0));
    let second_observations = Arc::new(AtomicU64::new(0));
    let (sender, receiver) = sync_channel(3);
    let handles = vec![
        spawn_snapshot_reader(
            first_reader,
            Arc::clone(&done),
            Arc::clone(&start),
            Arc::clone(&first_observations),
            sender.clone(),
            engine_epoch,
        ),
        spawn_snapshot_reader(
            second_reader,
            Arc::clone(&done),
            Arc::clone(&start),
            Arc::clone(&second_observations),
            sender.clone(),
            engine_epoch,
        ),
        spawn_snapshot_writer(publisher, Arc::clone(&done), start, sender, engine_epoch),
    ];

    collect_concurrency_results(&receiver, handles, done.as_ref())?;
    assert!(first_observations.load(Ordering::Relaxed) > 0);
    assert!(second_observations.load(Ordering::Relaxed) > 0);
    Ok(())
}

fn publish_snapshots(publisher: &mut SnapshotPublisher, engine_epoch: BootEpochId) -> TestResult {
    for sequence in 0..SNAPSHOT_PUBLICATIONS {
        let value = sequence.to_le_bytes()[0];
        let payload = [value; SNAPSHOT_PAYLOAD_BYTES];
        publisher.publish(snapshot_metadata(engine_epoch, sequence)?, &payload)?;
        std::thread::yield_now();
    }
    Ok(())
}

fn spawn_snapshot_reader(
    reader: SnapshotReader,
    done: Arc<AtomicBool>,
    start: Arc<Barrier>,
    observations: Arc<AtomicU64>,
    sender: SyncSender<Result<(), String>>,
    engine_epoch: BootEpochId,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        start.wait();
        let result =
            exercise_snapshot_reader(reader, done.as_ref(), observations.as_ref(), engine_epoch);
        let _send_result = sender.send(result);
    })
}

fn spawn_snapshot_writer(
    mut publisher: SnapshotPublisher,
    done: Arc<AtomicBool>,
    start: Arc<Barrier>,
    sender: SyncSender<Result<(), String>>,
    engine_epoch: BootEpochId,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        start.wait();
        let result =
            publish_snapshots(&mut publisher, engine_epoch).map_err(|error| error.to_string());
        done.store(true, Ordering::Release);
        let _send_result = sender.send(result);
    })
}

fn collect_concurrency_results(
    receiver: &Receiver<Result<(), String>>,
    handles: Vec<JoinHandle<()>>,
    done: &AtomicBool,
) -> TestResult {
    let deadline = Instant::now()
        .checked_add(CONCURRENCY_TIMEOUT)
        .ok_or_else(|| test_error("snapshot timeout deadline overflow"))?;
    for _ in 0..handles.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(RecvTimeoutError::Timeout);
        let participant_result = remaining.and_then(|duration| receiver.recv_timeout(duration));
        match participant_result {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                done.store(true, Ordering::Release);
                return Err(test_error(message));
            }
            Err(RecvTimeoutError::Timeout) => {
                done.store(true, Ordering::Release);
                return Err(test_error(
                    "snapshot participant exceeded the fixed timeout",
                ));
            }
            Err(RecvTimeoutError::Disconnected) => {
                done.store(true, Ordering::Release);
                return Err(test_error("snapshot result channel disconnected"));
            }
        }
    }
    for handle in handles {
        if handle.join().is_err() {
            return Err(test_error("snapshot participant thread unwound"));
        }
    }
    Ok(())
}

fn exercise_snapshot_reader(
    mut reader: SnapshotReader,
    done: &AtomicBool,
    observations: &AtomicU64,
    engine_epoch: BootEpochId,
) -> Result<(), String> {
    while !done.load(Ordering::Acquire) {
        match reader.try_latch(
            MonotonicTimestamp::new(engine_epoch, SNAPSHOT_PUBLICATIONS + 1),
            u64::MAX,
        ) {
            Ok(snapshot) => {
                validate_snapshot(snapshot.metadata().commit_sequence(), snapshot.payload())?;
                observations.fetch_add(1, Ordering::Relaxed);
            }
            Err(SnapshotChannelError::NoPublication | SnapshotChannelError::Contended) => {}
            Err(error) => return Err(error.to_string()),
        }
        std::thread::yield_now();
    }
    let final_snapshot = reader
        .try_latch(
            MonotonicTimestamp::new(engine_epoch, SNAPSHOT_PUBLICATIONS + 1),
            u64::MAX,
        )
        .map_err(|error| error.to_string())?;
    validate_snapshot(
        final_snapshot.metadata().commit_sequence(),
        final_snapshot.payload(),
    )?;
    if final_snapshot.metadata().commit_sequence().get() != SNAPSHOT_PUBLICATIONS - 1 {
        return Err("reader did not accept the final stable snapshot".to_owned());
    }
    Ok(())
}

fn validate_snapshot(sequence: CommitSequence, payload: &[u8]) -> Result<(), String> {
    let expected = sequence.get().to_le_bytes()[0];
    if payload.len() != SNAPSHOT_PAYLOAD_BYTES {
        return Err("snapshot payload length changed".to_owned());
    }
    if payload.iter().any(|value| *value != expected) {
        return Err("snapshot reader accepted a torn payload".to_owned());
    }
    Ok(())
}

fn snapshot_metadata(engine_epoch: BootEpochId, sequence: u64) -> TestResult<SnapshotMetadata> {
    let utc = UtcTimestamp::new(1_700_000_000, 123)?;
    SnapshotMetadata::new(
        ExecutionContractVersion::V1_0,
        engine_epoch,
        TaskEpoch::new(1)?,
        CommitSequence::new(sequence),
        ReleaseSequence::new(sequence),
        MonotonicTimestamp::new(engine_epoch, sequence),
        Some(UtcObservation::new(
            utc,
            TimeQuality::new(TimeQualityState::Good, TimeSource::Ptp, Some(50), Some(utc)),
        )),
        QualityCode::GOOD,
        [7; 32],
        u32::try_from(SNAPSHOT_PAYLOAD_BYTES)?,
    )
    .map_err(Into::into)
}

#[derive(Debug, Clone, Copy)]
enum FaultBoundary {
    ExecuteStep,
    CapacityAccess,
    HardLimitCheckpoint,
    ExplicitDiscard,
    UnresolvedDrop,
}

#[test]
fn fault_boundary_matrix_discards_partial_banks_and_recovers_from_initial_values() -> TestResult {
    let boundaries = [
        FaultBoundary::ExecuteStep,
        FaultBoundary::CapacityAccess,
        FaultBoundary::HardLimitCheckpoint,
        FaultBoundary::ExplicitDiscard,
        FaultBoundary::UnresolvedDrop,
    ];
    for (index, boundary) in boundaries.into_iter().enumerate() {
        exercise_fault_boundary(boundary, u8::try_from(0x90 + index)?)?;
    }
    Ok(())
}

fn exercise_fault_boundary(boundary: FaultBoundary, epoch_variant: u8) -> TestResult {
    let engine_epoch = epoch(epoch_variant)?;
    let spec = task_spec(0, 0, 10, 0)?;
    let clock = VerificationClock::new(engine_epoch);
    let mut plan = task_plan(&[spec], clock.now())?;
    let mut task = TaskTransaction::new(spec, engine_epoch, &[1], &[2], limits(2)?)?;
    let selected = release(&mut plan, &clock)?;
    let CycleStart::Execute(mut cycle) = task.begin(selected, &clock, ScheduleControl::Continue)?
    else {
        return Err(test_error("fault matrix release was not executable"));
    };
    cycle.write_state(WorkSetIndex::new(0), 99)?;
    cycle.write_output(WorkSetIndex::new(0), 100)?;

    let expected_reason = match boundary {
        FaultBoundary::ExecuteStep => {
            assert!(
                cycle
                    .execute(|_| Err(FaultReason::TaskExecutionFault))
                    .is_err()
            );
            assert!(cycle.finish(&clock).is_err());
            FaultReason::TaskExecutionFault
        }
        FaultBoundary::CapacityAccess => {
            assert!(cycle.write_output(WorkSetIndex::new(1), 5).is_err());
            assert!(cycle.finish(&clock).is_err());
            FaultReason::CapacityExceeded
        }
        FaultBoundary::HardLimitCheckpoint => {
            clock.set_elapsed(6)?;
            assert!(cycle.finish(&clock).is_err());
            FaultReason::HardLimitExceeded
        }
        FaultBoundary::ExplicitDiscard => {
            let fault = cycle.discard(FaultReason::TaskExecutionFault);
            assert_eq!(fault.reason, FaultReason::TaskExecutionFault);
            FaultReason::TaskExecutionFault
        }
        FaultBoundary::UnresolvedDrop => {
            drop(cycle);
            FaultReason::TaskExecutionFault
        }
    };

    assert_eq!(task_image(&task)?, [1, 2]);
    assert!(task.publishable().is_none());
    let fault = task
        .fault()
        .ok_or_else(|| test_error("fault was not latched"))?;
    assert_eq!(fault.reason, expected_reason);
    let mut guard = ExactResetGuard::new(fault.reset_request);
    let version = task.reset(
        fault.reset_request,
        &mut guard,
        &mut plan,
        &clock,
        |values| {
            if values.state(WorkSetIndex::new(0)) == Ok(1)
                && values.output(WorkSetIndex::new(0)) == Ok(2)
            {
                Ok(())
            } else {
                Err(InitializationRejected)
            }
        },
    )?;
    assert_eq!(guard.calls, 1);
    assert_eq!(version.task_epoch.get(), 2);
    assert_eq!(version.sequence, CommitSequence::ZERO);
    assert_eq!(task_image(&task)?, [1, 2]);
    assert!(task.publishable().is_none());

    let next_release = match plan.observe(&clock, ScheduleControl::Continue)? {
        ScheduleAction::WaitUntil { release } => release,
        ScheduleAction::Release(_) | ScheduleAction::Stopped { .. } => {
            return Err(test_error("reset did not select a strictly future release"));
        }
    };
    clock.set_elapsed(next_release.elapsed_nanos())?;
    let selected = release(&mut plan, &clock)?;
    let CycleStart::Execute(cycle) = task.begin(selected, &clock, ScheduleControl::Continue)?
    else {
        return Err(test_error("recovered task was not executable"));
    };
    let commit = cycle.finish(&clock)?;
    assert_eq!(commit.version.task_epoch.get(), 2);
    assert_eq!(commit.version.sequence.get(), 1);
    assert!(task.publishable().is_some());
    Ok(())
}

#[test]
fn spsc_full_empty_wrap_gap_and_high_water_are_exactly_accounted() -> TestResult {
    const CAPACITY: usize = 3;
    const WRAP_PUBLICATIONS: u64 = 4_096;

    let capacity = SpscCapacity::new(CAPACITY, CAPACITY)?;
    let (mut producer, mut consumer) = bounded_spsc(capacity, SpscOverflowPolicy::DropNewest)?;
    assert_eq!(consumer.try_pop(), Err(SpscPopError::Empty));

    for value in 0..3_u64 {
        assert!(matches!(
            producer.try_push(value)?,
            SpscPushOutcome::Published(sequence) if sequence.get() == value
        ));
    }
    for value in 3..5_u64 {
        assert!(matches!(
            producer.try_push(value)?,
            SpscPushOutcome::DroppedNewest(sequence) if sequence.get() == value
        ));
    }
    for value in 0..3_u64 {
        let read = consumer.try_pop()?;
        assert_eq!(read.sequence().get(), value);
        assert_eq!(read.value(), value);
        assert_eq!(read.missed_before(), 0);
    }
    assert_eq!(consumer.try_pop(), Err(SpscPopError::Empty));

    assert!(matches!(
        producer.try_push(5)?,
        SpscPushOutcome::Published(sequence) if sequence.get() == 5
    ));
    let resumed = consumer.try_pop()?;
    assert_eq!(resumed.sequence().get(), 5);
    assert_eq!(resumed.missed_before(), 2);

    for value in 6..6 + WRAP_PUBLICATIONS {
        assert!(matches!(
            producer.try_push(value)?,
            SpscPushOutcome::Published(sequence) if sequence.get() == value
        ));
        let read = consumer.try_pop()?;
        assert_eq!(read.sequence().get(), value);
        assert_eq!(read.value(), value);
        assert_eq!(read.missed_before(), 0);
    }
    let statistics = producer.statistics();
    assert_eq!(statistics.capacity, CAPACITY);
    assert_eq!(statistics.readable, 0);
    assert_eq!(statistics.high_water_mark, CAPACITY);
    assert_eq!(statistics.dropped_newest, 2);
    assert_eq!(statistics.full, 2);
    assert_eq!(statistics.observed_sequence_gaps, 2);
    drop(producer);
    assert_eq!(consumer.try_pop(), Err(SpscPopError::ProducerDropped));
    assert!(consumer.statistics().producer_dropped);
    Ok(())
}

#[derive(Debug)]
struct VerificationClock {
    monotonic: Cell<MonotonicTimestamp>,
    utc: Cell<UtcTimestamp>,
}

impl VerificationClock {
    fn new(engine_epoch: BootEpochId) -> Self {
        Self {
            monotonic: Cell::new(MonotonicTimestamp::new(engine_epoch, 0)),
            utc: Cell::new(UtcTimestamp::UNIX_EPOCH),
        }
    }

    fn set_elapsed(&self, elapsed_nanos: u64) -> TestResult {
        let current = self.monotonic.get();
        if elapsed_nanos < current.elapsed_nanos() {
            return Err(test_error("verification clock moved backwards"));
        }
        self.monotonic
            .set(MonotonicTimestamp::new(current.boot_epoch(), elapsed_nanos));
        Ok(())
    }

    fn set_utc(&self, utc: UtcTimestamp) {
        self.utc.set(utc);
    }
}

impl MonotonicClock for VerificationClock {
    fn now(&self) -> MonotonicTimestamp {
        self.monotonic.get()
    }
}

#[derive(Debug)]
struct ExactResetGuard {
    expected: ResetRequest,
    calls: usize,
}

impl ExactResetGuard {
    const fn new(expected: ResetRequest) -> Self {
        Self { expected, calls: 0 }
    }
}

impl ResetGuard for ExactResetGuard {
    fn check(&mut self, request: ResetRequest) -> Result<(), ResetGuardError> {
        self.calls += 1;
        if request == self.expected {
            Ok(())
        } else {
            Err(ResetGuardError::Unauthorized)
        }
    }
}

fn task_spec(
    handle: u32,
    priority: i16,
    period_nanos: u64,
    phase_nanos: u64,
) -> TestResult<TaskSpec> {
    Ok(TaskSpec::new(
        ExecutionContractVersion::V1_0,
        LocalHandle::new(handle)?,
        TaskPriority::new(priority),
        TaskTiming::new(
            TaskPeriodNanos::new(period_nanos)?,
            TaskPhaseNanos::new(phase_nanos),
            RelativeDeadlineNanos::new(period_nanos)?,
            ExecutionBudgetNanos::new(3)?,
            HardLimitNanos::new(5)?,
        )?,
        MissPolicy::new(MissWindow::new(8, 8)?, 4, 5)?,
    ))
}

fn task_plan(specs: &[TaskSpec], start: MonotonicTimestamp) -> TestResult<StaticTaskPlan> {
    let capacity = WorkSetCapacity::new(specs.len())?;
    let mut builder =
        StaticTaskPlanBuilder::new(start, capacity, WorkSetLimits::new(capacity, 16 * 1024))?;
    for spec in specs {
        builder.add_task(*spec)?;
    }
    builder.seal().map_err(Into::into)
}

fn limits(capacity: usize) -> TestResult<WorkSetLimits> {
    let capacity = WorkSetCapacity::new(capacity)?;
    Ok(WorkSetLimits::new(capacity, 16 * 1024))
}

fn release<'plan>(
    plan: &'plan mut StaticTaskPlan,
    clock: &VerificationClock,
) -> TestResult<ReleaseDecision<'plan>> {
    match plan.observe(clock, ScheduleControl::Continue)? {
        ScheduleAction::Release(selected) => Ok(selected),
        ScheduleAction::WaitUntil { .. } | ScheduleAction::Stopped { .. } => {
            Err(test_error("expected a scheduled release"))
        }
    }
}

fn task_image(task: &TaskTransaction) -> TestResult<[u8; 2]> {
    let values = task.diagnostic().values();
    Ok([
        values.state(WorkSetIndex::new(0))?,
        values.output(WorkSetIndex::new(0))?,
    ])
}

fn epoch(variant: u8) -> TestResult<BootEpochId> {
    BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, variant, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
        0x39, 0x8f,
    ])
    .map_err(Into::into)
}

fn test_error(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(io::Error::other(message.into()))
}
