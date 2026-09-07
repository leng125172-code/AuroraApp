//! Semantic R0 Trace records and bounded-channel counters.

use aurora_types::{BootEpochId, LocalHandle, MonotonicTimestamp};

use crate::{
    CommitSequence, EventSequence, ExecutionContractError, ExecutionContractVersion,
    FallbackRequestSequence, FaultReason, MissOutcome, ReleaseSequence, TaskEpoch, TaskState,
    UtcObservation,
};

/// The kind of semantic event represented by a Trace record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TraceEventKind {
    /// A release was handled, whether committed or missed.
    ReleaseCompleted = 1,
    /// One or more expired releases were skipped in a bounded batch.
    ReleasesSkipped = 2,
    /// A task entered `FaultLocked`.
    TaskFaulted = 3,
    /// A task published declared initial state for a new epoch.
    TaskReinitialized = 4,
    /// A persistent Fallback request was published.
    FallbackRequested = 5,
    /// A snapshot reader observed a commit-sequence gap.
    SnapshotGap = 6,
    /// A bounded channel rejected or dropped an item.
    QueueOverflow = 7,
}

impl TryFrom<u8> for TraceEventKind {
    type Error = ExecutionContractError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::ReleaseCompleted),
            2 => Ok(Self::ReleasesSkipped),
            3 => Ok(Self::TaskFaulted),
            4 => Ok(Self::TaskReinitialized),
            5 => Ok(Self::FallbackRequested),
            6 => Ok(Self::SnapshotGap),
            7 => Ok(Self::QueueOverflow),
            _ => Err(ExecutionContractError::InvalidEnum),
        }
    }
}

/// A positive fixed Trace capacity validated against a target maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceCapacity(u32);

impl TraceCapacity {
    /// Creates a positive capacity no greater than `target_maximum`.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::InvalidCapacity`] for zero or a value
    /// exceeding the target declaration.
    pub const fn new(value: u32, target_maximum: u32) -> Result<Self, ExecutionContractError> {
        if value == 0 || value > target_maximum {
            Err(ExecutionContractError::InvalidCapacity)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the number of fixed slots.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A consistent observation of bounded Trace channel counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceCounters {
    capacity: TraceCapacity,
    occupancy: u32,
    high_water_mark: u32,
    attempted: u64,
    published: u64,
    dropped: u64,
    full: u64,
    counter_saturated: bool,
}

impl TraceCounters {
    /// Creates counters after checking capacity and outcome relationships.
    ///
    /// # Errors
    ///
    /// Returns a specific capacity, high-water, or count consistency error.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        capacity: TraceCapacity,
        occupancy: u32,
        high_water_mark: u32,
        attempted: u64,
        published: u64,
        dropped: u64,
        full: u64,
        counter_saturated: bool,
    ) -> Result<Self, ExecutionContractError> {
        if occupancy > capacity.get() {
            return Err(ExecutionContractError::OccupancyExceedsCapacity);
        }
        if high_water_mark < occupancy || high_water_mark > capacity.get() {
            return Err(ExecutionContractError::InvalidHighWaterMark);
        }
        if !counter_saturated {
            match published.checked_add(dropped) {
                Some(outcomes) if outcomes <= attempted => {}
                Some(_) | None => return Err(ExecutionContractError::InvalidTraceCounts),
            }
            if full < dropped {
                return Err(ExecutionContractError::InvalidTraceCounts);
            }
            if full > attempted {
                return Err(ExecutionContractError::InvalidTraceCounts);
            }
        }
        Ok(Self {
            capacity,
            occupancy,
            high_water_mark,
            attempted,
            published,
            dropped,
            full,
            counter_saturated,
        })
    }

    /// Returns fixed slot capacity.
    #[must_use]
    pub const fn capacity(self) -> TraceCapacity {
        self.capacity
    }

    /// Returns occupied slots at observation time.
    #[must_use]
    pub const fn occupancy(self) -> u32 {
        self.occupancy
    }

    /// Returns the lifetime maximum observed occupancy.
    #[must_use]
    pub const fn high_water_mark(self) -> u32 {
        self.high_water_mark
    }

    /// Returns attempted event publications, including drops.
    #[must_use]
    pub const fn attempted(self) -> u64 {
        self.attempted
    }

    /// Returns successfully published records.
    #[must_use]
    pub const fn published(self) -> u64 {
        self.published
    }

    /// Returns records discarded by the static `DropNewest` policy.
    #[must_use]
    pub const fn dropped(self) -> u64 {
        self.dropped
    }

    /// Returns observations of a full channel.
    #[must_use]
    pub const fn full(self) -> u64 {
        self.full
    }

    /// Returns whether any observation counter saturated.
    #[must_use]
    pub const fn counter_saturated(self) -> bool {
        self.counter_saturated
    }
}

/// Absolute release/deadline and optional execution interval for one event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceTiming {
    scheduled_release: MonotonicTimestamp,
    absolute_deadline: MonotonicTimestamp,
    started_at: Option<MonotonicTimestamp>,
    finished_at: Option<MonotonicTimestamp>,
}

/// 一个批量 skipped release 的连续、不可歧义范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceSkippedReleases {
    first: ReleaseSequence,
    last: ReleaseSequence,
    count: u64,
}

impl TraceSkippedReleases {
    /// 创建与 `first..=last` 精确一致的非空范围。
    ///
    /// # Errors
    ///
    /// count 为零、加法溢出或末项不一致时拒绝。
    pub fn new(
        first: ReleaseSequence,
        last: ReleaseSequence,
        count: u64,
    ) -> Result<Self, ExecutionContractError> {
        let expected_last = first
            .get()
            .checked_add(count.saturating_sub(1))
            .ok_or(ExecutionContractError::CounterOverflow)?;
        if count == 0 || expected_last != last.get() {
            return Err(ExecutionContractError::InvalidTraceCounts);
        }
        Ok(Self { first, last, count })
    }

    /// 返回首个 skipped release。
    #[must_use]
    pub const fn first(self) -> ReleaseSequence {
        self.first
    }

    /// 返回最后一个 skipped release。
    #[must_use]
    pub const fn last(self) -> ReleaseSequence {
        self.last
    }

    /// 返回批量数量。
    #[must_use]
    pub const fn count(self) -> u64 {
        self.count
    }
}

/// 一项输入或输出 snapshot 的来源版本及其前序缺口。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceSnapshotEvidence {
    source_task: LocalHandle,
    source_task_epoch: TaskEpoch,
    commit_sequence: CommitSequence,
    missed_commits: u64,
}

impl TraceSnapshotEvidence {
    /// 创建 snapshot 证据；`missed_commits = 0` 表示未观察到前序缺口。
    ///
    /// # Errors
    ///
    /// gap 大于当前 commit sequence 时拒绝，避免表示不存在的负序列区间。
    pub const fn new(
        source_task: LocalHandle,
        source_task_epoch: TaskEpoch,
        commit_sequence: CommitSequence,
        missed_commits: u64,
    ) -> Result<Self, ExecutionContractError> {
        if missed_commits > commit_sequence.get() {
            Err(ExecutionContractError::InvalidTraceEvidence)
        } else {
            Ok(Self {
                source_task,
                source_task_epoch,
                commit_sequence,
                missed_commits,
            })
        }
    }

    /// 返回 snapshot writer 的任务 handle。
    #[must_use]
    pub const fn source_task(self) -> LocalHandle {
        self.source_task
    }

    /// 返回 snapshot writer 的 task epoch。
    #[must_use]
    pub const fn source_task_epoch(self) -> TaskEpoch {
        self.source_task_epoch
    }

    /// 返回观察到的 snapshot commit sequence。
    #[must_use]
    pub const fn commit_sequence(self) -> CommitSequence {
        self.commit_sequence
    }

    /// 返回这项证据之前未观察到的 commit 数。
    #[must_use]
    pub const fn missed_commits(self) -> u64 {
        self.missed_commits
    }
}

impl TraceTiming {
    /// Creates timing while enforcing one epoch and chronological order.
    ///
    /// # Errors
    ///
    /// Rejects half-present execution intervals, epoch mismatches, a deadline
    /// before release, execution before release, or finish before start.
    pub fn new(
        scheduled_release: MonotonicTimestamp,
        absolute_deadline: MonotonicTimestamp,
        started_at: Option<MonotonicTimestamp>,
        finished_at: Option<MonotonicTimestamp>,
    ) -> Result<Self, ExecutionContractError> {
        let epoch = scheduled_release.boot_epoch();
        if absolute_deadline.boot_epoch() != epoch
            || started_at.is_some_and(|value| value.boot_epoch() != epoch)
            || finished_at.is_some_and(|value| value.boot_epoch() != epoch)
        {
            return Err(ExecutionContractError::EpochMismatch);
        }
        if absolute_deadline.elapsed_nanos() < scheduled_release.elapsed_nanos() {
            return Err(ExecutionContractError::InvalidTimestampOrder);
        }
        match (started_at, finished_at) {
            (Some(start), Some(finish)) => {
                if start.elapsed_nanos() < scheduled_release.elapsed_nanos()
                    || finish.elapsed_nanos() < start.elapsed_nanos()
                {
                    return Err(ExecutionContractError::InvalidTimestampOrder);
                }
            }
            (None, None) => {}
            (Some(_), None) | (None, Some(_)) => {
                return Err(ExecutionContractError::IncompleteExecutionTiming);
            }
        }
        Ok(Self {
            scheduled_release,
            absolute_deadline,
            started_at,
            finished_at,
        })
    }

    /// Returns the scheduled release time.
    #[must_use]
    pub const fn scheduled_release(self) -> MonotonicTimestamp {
        self.scheduled_release
    }

    /// Returns the absolute deadline.
    #[must_use]
    pub const fn absolute_deadline(self) -> MonotonicTimestamp {
        self.absolute_deadline
    }

    /// Returns execution start, if the task was invoked.
    #[must_use]
    pub const fn started_at(self) -> Option<MonotonicTimestamp> {
        self.started_at
    }

    /// Returns execution finish, if the task was invoked.
    #[must_use]
    pub const fn finished_at(self) -> Option<MonotonicTimestamp> {
        self.finished_at
    }

    /// Returns execution elapsed nanoseconds when the task was invoked.
    #[must_use]
    pub const fn execution_elapsed_nanos(self) -> Option<u64> {
        match (self.started_at, self.finished_at) {
            (Some(start), Some(finish)) => Some(finish.elapsed_nanos() - start.elapsed_nanos()),
            (None | Some(_), None) | (None, Some(_)) => None,
        }
    }
}

/// A semantic Trace record before R0-07 assigns binary offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceRecord {
    version: ExecutionContractVersion,
    engine_epoch: BootEpochId,
    task_handle: LocalHandle,
    task_epoch: TaskEpoch,
    event_sequence: EventSequence,
    release_sequence: ReleaseSequence,
    commit_before: CommitSequence,
    commit_after: CommitSequence,
    kind: TraceEventKind,
    timing: TraceTiming,
    utc: Option<UtcObservation>,
    state_before: TaskState,
    state_after: TaskState,
    miss: Option<MissOutcome>,
    fault: Option<FaultReason>,
    fallback_request: Option<FallbackRequestSequence>,
    skipped_releases: Option<TraceSkippedReleases>,
    input_snapshot: Option<TraceSnapshotEvidence>,
    output_snapshot: Option<TraceSnapshotEvidence>,
    counters: TraceCounters,
}

impl TraceRecord {
    /// Creates a record and validates epoch and commit progression.
    ///
    /// # Errors
    ///
    /// Rejects a timing epoch mismatch, commit regression, or a commit jump
    /// larger than one successful cycle.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version: ExecutionContractVersion,
        engine_epoch: BootEpochId,
        task_handle: LocalHandle,
        task_epoch: TaskEpoch,
        event_sequence: EventSequence,
        release_sequence: ReleaseSequence,
        commit_before: CommitSequence,
        commit_after: CommitSequence,
        kind: TraceEventKind,
        timing: TraceTiming,
        utc: Option<UtcObservation>,
        state_before: TaskState,
        state_after: TaskState,
        miss: Option<MissOutcome>,
        fault: Option<FaultReason>,
        fallback_request: Option<FallbackRequestSequence>,
        skipped_releases: Option<TraceSkippedReleases>,
        input_snapshot: Option<TraceSnapshotEvidence>,
        output_snapshot: Option<TraceSnapshotEvidence>,
        counters: TraceCounters,
    ) -> Result<Self, ExecutionContractError> {
        if timing.scheduled_release().boot_epoch() != engine_epoch {
            return Err(ExecutionContractError::EpochMismatch);
        }
        let commit_delta = commit_after.get().checked_sub(commit_before.get());
        if !matches!(commit_delta, Some(0 | 1)) {
            return Err(ExecutionContractError::InvalidCommitSequence);
        }
        if matches!(kind, TraceEventKind::ReleasesSkipped) != skipped_releases.is_some() {
            return Err(ExecutionContractError::InvalidTraceEvidence);
        }
        if matches!(kind, TraceEventKind::SnapshotGap)
            && !input_snapshot
                .into_iter()
                .chain(output_snapshot)
                .any(|evidence| evidence.missed_commits() > 0)
        {
            return Err(ExecutionContractError::InvalidTraceEvidence);
        }
        Ok(Self {
            version,
            engine_epoch,
            task_handle,
            task_epoch,
            event_sequence,
            release_sequence,
            commit_before,
            commit_after,
            kind,
            timing,
            utc,
            state_before,
            state_after,
            miss,
            fault,
            fallback_request,
            skipped_releases,
            input_snapshot,
            output_snapshot,
            counters,
        })
    }

    /// Returns the execution-contract version.
    #[must_use]
    pub const fn version(self) -> ExecutionContractVersion {
        self.version
    }

    /// Returns the engine epoch.
    #[must_use]
    pub const fn engine_epoch(self) -> BootEpochId {
        self.engine_epoch
    }

    /// Returns the task handle.
    #[must_use]
    pub const fn task_handle(self) -> LocalHandle {
        self.task_handle
    }

    /// Returns the task epoch.
    #[must_use]
    pub const fn task_epoch(self) -> TaskEpoch {
        self.task_epoch
    }

    /// Returns the event-attempt sequence.
    #[must_use]
    pub const fn event_sequence(self) -> EventSequence {
        self.event_sequence
    }

    /// Returns the scheduled release sequence.
    #[must_use]
    pub const fn release_sequence(self) -> ReleaseSequence {
        self.release_sequence
    }

    /// Returns the commit sequence before this event.
    #[must_use]
    pub const fn commit_before(self) -> CommitSequence {
        self.commit_before
    }

    /// Returns the commit sequence after this event.
    #[must_use]
    pub const fn commit_after(self) -> CommitSequence {
        self.commit_after
    }

    /// Returns the event kind.
    #[must_use]
    pub const fn kind(self) -> TraceEventKind {
        self.kind
    }

    /// Returns release/deadline and optional execution timing.
    #[must_use]
    pub const fn timing(self) -> TraceTiming {
        self.timing
    }

    /// Returns the optional UTC observation.
    #[must_use]
    pub const fn utc(self) -> Option<UtcObservation> {
        self.utc
    }

    /// Returns task state before the event.
    #[must_use]
    pub const fn state_before(self) -> TaskState {
        self.state_before
    }

    /// Returns task state after the event.
    #[must_use]
    pub const fn state_after(self) -> TaskState {
        self.state_after
    }

    /// Returns the optional deadline-miss result.
    #[must_use]
    pub const fn miss(self) -> Option<MissOutcome> {
        self.miss
    }

    /// Returns the optional Fault reason.
    #[must_use]
    pub const fn fault(self) -> Option<FaultReason> {
        self.fault
    }

    /// Returns the optional Fallback request sequence.
    #[must_use]
    pub const fn fallback_request(self) -> Option<FallbackRequestSequence> {
        self.fallback_request
    }

    /// 返回可选的 skipped release 批次证据。
    #[must_use]
    pub const fn skipped_releases(self) -> Option<TraceSkippedReleases> {
        self.skipped_releases
    }

    /// 返回本周期锁存的可选输入 snapshot 证据。
    #[must_use]
    pub const fn input_snapshot(self) -> Option<TraceSnapshotEvidence> {
        self.input_snapshot
    }

    /// 返回本周期发布的可选输出 snapshot 证据。
    #[must_use]
    pub const fn output_snapshot(self) -> Option<TraceSnapshotEvidence> {
        self.output_snapshot
    }

    /// Returns the bounded-channel counters captured with the event.
    #[must_use]
    pub const fn counters(self) -> TraceCounters {
        self.counters
    }
}

#[cfg(test)]
mod tests {
    use aurora_types::{BootEpochId, LocalHandle, MonotonicTimestamp};

    use super::{TraceCapacity, TraceCounters, TraceEventKind, TraceRecord, TraceTiming};
    use crate::{
        CommitSequence, EventSequence, ExecutionContractError, ExecutionContractVersion,
        ReleaseSequence, TaskEpoch, TaskState,
    };

    #[test]
    fn capacity_and_counters_reject_every_inconsistent_boundary() {
        assert_eq!(
            TraceCapacity::new(0, 8),
            Err(ExecutionContractError::InvalidCapacity)
        );
        assert_eq!(
            TraceCapacity::new(9, 8),
            Err(ExecutionContractError::InvalidCapacity)
        );
        let capacity = TraceCapacity::new(8, 8);
        if let Ok(capacity) = capacity {
            assert_eq!(
                TraceCounters::new(capacity, 9, 9, 1, 1, 0, 0, false),
                Err(ExecutionContractError::OccupancyExceedsCapacity)
            );
            assert_eq!(
                TraceCounters::new(capacity, 4, 3, 4, 4, 0, 0, false),
                Err(ExecutionContractError::InvalidHighWaterMark)
            );
            assert_eq!(
                TraceCounters::new(capacity, 0, 9, 4, 4, 0, 0, false),
                Err(ExecutionContractError::InvalidHighWaterMark)
            );
            assert_eq!(
                TraceCounters::new(capacity, 0, 8, 1, 1, 1, 1, false),
                Err(ExecutionContractError::InvalidTraceCounts)
            );
            assert_eq!(
                TraceCounters::new(capacity, 0, 8, 2, 1, 1, 0, false),
                Err(ExecutionContractError::InvalidTraceCounts)
            );
            assert_eq!(
                TraceCounters::new(capacity, 0, 8, 1, 0, 0, 2, false),
                Err(ExecutionContractError::InvalidTraceCounts)
            );
            assert!(
                TraceCounters::new(capacity, 0, 8, u64::MAX, u64::MAX, u64::MAX, u64::MAX, true,)
                    .is_ok()
            );
            let valid = TraceCounters::new(capacity, 4, 7, 10, 8, 2, 2, false);
            assert!(valid.is_ok());
            if let Ok(valid) = valid {
                assert_eq!(valid.capacity(), capacity);
                assert_eq!(valid.occupancy(), 4);
                assert_eq!(valid.high_water_mark(), 7);
                assert_eq!(valid.attempted(), 10);
                assert_eq!(valid.published(), 8);
                assert_eq!(valid.dropped(), 2);
                assert_eq!(valid.full(), 2);
                assert!(!valid.counter_saturated());
            }
        }
    }

    #[test]
    fn timing_rejects_missing_epoch_and_order_errors() {
        let epoch = test_epoch(0x98);
        let other = test_epoch(0x99);
        assert!(epoch.is_ok());
        assert!(other.is_ok());
        if let (Ok(epoch), Ok(other)) = (epoch, other) {
            assert_eq!(
                TraceTiming::new(
                    MonotonicTimestamp::new(epoch, 10),
                    MonotonicTimestamp::new(epoch, 20),
                    Some(MonotonicTimestamp::new(epoch, 11)),
                    None,
                ),
                Err(ExecutionContractError::IncompleteExecutionTiming)
            );
            assert_eq!(
                TraceTiming::new(
                    MonotonicTimestamp::new(epoch, 10),
                    MonotonicTimestamp::new(other, 20),
                    None,
                    None,
                ),
                Err(ExecutionContractError::EpochMismatch)
            );
            assert_eq!(
                TraceTiming::new(
                    MonotonicTimestamp::new(epoch, 10),
                    MonotonicTimestamp::new(epoch, 20),
                    Some(MonotonicTimestamp::new(other, 11)),
                    Some(MonotonicTimestamp::new(epoch, 12)),
                ),
                Err(ExecutionContractError::EpochMismatch)
            );
            assert_eq!(
                TraceTiming::new(
                    MonotonicTimestamp::new(epoch, 10),
                    MonotonicTimestamp::new(epoch, 20),
                    Some(MonotonicTimestamp::new(epoch, 11)),
                    Some(MonotonicTimestamp::new(other, 12)),
                ),
                Err(ExecutionContractError::EpochMismatch)
            );
            assert_eq!(
                TraceTiming::new(
                    MonotonicTimestamp::new(epoch, 10),
                    MonotonicTimestamp::new(epoch, 9),
                    None,
                    None,
                ),
                Err(ExecutionContractError::InvalidTimestampOrder)
            );
            assert_eq!(
                TraceTiming::new(
                    MonotonicTimestamp::new(epoch, 10),
                    MonotonicTimestamp::new(epoch, 20),
                    Some(MonotonicTimestamp::new(epoch, 9)),
                    Some(MonotonicTimestamp::new(epoch, 12)),
                ),
                Err(ExecutionContractError::InvalidTimestampOrder)
            );
            assert_eq!(
                TraceTiming::new(
                    MonotonicTimestamp::new(epoch, 10),
                    MonotonicTimestamp::new(epoch, 20),
                    Some(MonotonicTimestamp::new(epoch, 11)),
                    Some(MonotonicTimestamp::new(epoch, 10)),
                ),
                Err(ExecutionContractError::InvalidTimestampOrder)
            );
        }
    }

    #[test]
    fn record_preserves_fields_and_rejects_commit_jumps() {
        let epoch = test_epoch(0x98);
        let capacity = TraceCapacity::new(8, 8);
        let task_epoch = TaskEpoch::new(1);
        assert!(epoch.is_ok());
        assert!(capacity.is_ok());
        assert!(task_epoch.is_ok());
        if let (Ok(epoch), Ok(capacity), Ok(task_epoch)) = (epoch, capacity, task_epoch) {
            let timing = TraceTiming::new(
                MonotonicTimestamp::new(epoch, 10),
                MonotonicTimestamp::new(epoch, 20),
                Some(MonotonicTimestamp::new(epoch, 11)),
                Some(MonotonicTimestamp::new(epoch, 14)),
            );
            let counters = TraceCounters::new(capacity, 1, 3, 7, 6, 1, 1, false);
            assert!(timing.is_ok());
            assert!(counters.is_ok());
            if let (Ok(timing), Ok(counters)) = (timing, counters) {
                let record = new_record(epoch, task_epoch, timing, counters, 4);
                assert!(record.is_ok());
                if let Ok(record) = record {
                    assert_eq!(record.version(), ExecutionContractVersion::V1_0);
                    assert_eq!(record.engine_epoch(), epoch);
                    assert_eq!(record.task_handle(), LocalHandle::ZERO);
                    assert_eq!(record.task_epoch(), task_epoch);
                    assert_eq!(record.event_sequence().get(), 7);
                    assert_eq!(record.release_sequence().get(), 5);
                    assert_eq!(record.commit_before().get(), 3);
                    assert_eq!(record.commit_after().get(), 4);
                    assert_eq!(record.kind(), TraceEventKind::ReleaseCompleted);
                    assert_eq!(record.timing().execution_elapsed_nanos(), Some(3));
                    assert_eq!(record.state_before(), TaskState::Running);
                    assert_eq!(record.state_after(), TaskState::Running);
                    assert_eq!(record.utc(), None);
                    assert_eq!(record.miss(), None);
                    assert_eq!(record.fault(), None);
                    assert_eq!(record.fallback_request(), None);
                    assert_eq!(record.counters(), counters);
                }
                assert_eq!(
                    new_record(epoch, task_epoch, timing, counters, 5),
                    Err(ExecutionContractError::InvalidCommitSequence)
                );
                let other_epoch = test_epoch(0x99);
                if let Ok(other_epoch) = other_epoch {
                    assert_eq!(
                        new_record(other_epoch, task_epoch, timing, counters, 4),
                        Err(ExecutionContractError::EpochMismatch)
                    );
                }
            }
        }
        for (raw, value) in [
            (1, TraceEventKind::ReleaseCompleted),
            (2, TraceEventKind::ReleasesSkipped),
            (3, TraceEventKind::TaskFaulted),
            (4, TraceEventKind::TaskReinitialized),
            (5, TraceEventKind::FallbackRequested),
            (6, TraceEventKind::SnapshotGap),
            (7, TraceEventKind::QueueOverflow),
        ] {
            assert_eq!(TraceEventKind::try_from(raw), Ok(value));
        }
        assert_eq!(
            TraceEventKind::try_from(0),
            Err(ExecutionContractError::InvalidEnum)
        );
    }

    fn new_record(
        epoch: BootEpochId,
        task_epoch: TaskEpoch,
        timing: TraceTiming,
        counters: TraceCounters,
        commit_after: u64,
    ) -> Result<TraceRecord, ExecutionContractError> {
        TraceRecord::new(
            ExecutionContractVersion::V1_0,
            epoch,
            LocalHandle::ZERO,
            task_epoch,
            EventSequence::new(7),
            ReleaseSequence::new(5),
            CommitSequence::new(3),
            CommitSequence::new(commit_after),
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
        )
    }

    fn test_epoch(variant: u8) -> Result<BootEpochId, aurora_types::IdentifierError> {
        BootEpochId::from_bytes([
            0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, variant, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
            0x39, 0x8f,
        ])
    }
}
