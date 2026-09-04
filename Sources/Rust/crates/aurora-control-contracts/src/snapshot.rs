//! Versioned metadata and freshness rules for complete task snapshots.

use aurora_types::{
    BootEpochId, MonotonicTimestamp, QualityCode, QualityFlags, TimeQuality, UtcTimestamp,
};

use crate::{
    CommitSequence, ExecutionContractError, ExecutionContractVersion, ReleaseSequence, TaskEpoch,
};

/// A UTC timestamp paired with the quality observation that qualifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcObservation {
    timestamp: UtcTimestamp,
    quality: TimeQuality,
}

impl UtcObservation {
    /// Creates a complete UTC observation.
    #[must_use]
    pub const fn new(timestamp: UtcTimestamp, quality: TimeQuality) -> Self {
        Self { timestamp, quality }
    }

    /// Converts nullable wire fields while rejecting a half-present pair.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::IncompleteUtcObservation`] when only
    /// one input is present.
    pub const fn try_from_parts(
        timestamp: Option<UtcTimestamp>,
        quality: Option<TimeQuality>,
    ) -> Result<Option<Self>, ExecutionContractError> {
        match (timestamp, quality) {
            (Some(timestamp), Some(quality)) => Ok(Some(Self::new(timestamp, quality))),
            (None, None) => Ok(None),
            (Some(_), None) | (None, Some(_)) => {
                Err(ExecutionContractError::IncompleteUtcObservation)
            }
        }
    }

    /// Returns the normalized UTC timestamp.
    #[must_use]
    pub const fn timestamp(self) -> UtcTimestamp {
        self.timestamp
    }

    /// Returns the time-source quality attached to the timestamp.
    #[must_use]
    pub const fn quality(self) -> TimeQuality {
        self.quality
    }
}

/// Freshness of a snapshot at one reader latch boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum SnapshotFreshness {
    /// Age is no greater than the configured maximum.
    Fresh = 1,
    /// Age is greater than the configured maximum.
    Stale = 2,
}

impl TryFrom<u8> for SnapshotFreshness {
    type Error = ExecutionContractError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Fresh),
            2 => Ok(Self::Stale),
            _ => Err(ExecutionContractError::InvalidEnum),
        }
    }
}

/// Reader-local age and freshness derived only from monotonic time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SnapshotObservation {
    age_nanos: u64,
    freshness: SnapshotFreshness,
}

impl SnapshotObservation {
    /// Returns reader-observed age in nanoseconds.
    #[must_use]
    pub const fn age_nanos(self) -> u64 {
        self.age_nanos
    }

    /// Returns whether the configured maximum age was exceeded.
    #[must_use]
    pub const fn freshness(self) -> SnapshotFreshness {
        self.freshness
    }
}

/// Relationship between two accepted snapshots from one source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SnapshotProgress {
    /// The next successful commit in the same task epoch.
    Next,
    /// One or more successful commits were not observed by this reader.
    Gap {
        /// Number of unobserved commit versions.
        missed_commits: u64,
    },
    /// The task was reinitialized and published commit sequence zero.
    NewTaskEpoch,
}

/// Metadata attached to one complete committed task snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotMetadata {
    version: ExecutionContractVersion,
    engine_epoch: BootEpochId,
    task_epoch: TaskEpoch,
    commit_sequence: CommitSequence,
    release_sequence: ReleaseSequence,
    published_at: MonotonicTimestamp,
    utc: Option<UtcObservation>,
    quality: QualityCode,
    schema_hash: [u8; 32],
    payload_length: u32,
}

impl SnapshotMetadata {
    /// Creates metadata and validates that monotonic time belongs to the engine
    /// epoch.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::EpochMismatch`] for another epoch.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version: ExecutionContractVersion,
        engine_epoch: BootEpochId,
        task_epoch: TaskEpoch,
        commit_sequence: CommitSequence,
        release_sequence: ReleaseSequence,
        published_at: MonotonicTimestamp,
        utc: Option<UtcObservation>,
        quality: QualityCode,
        schema_hash: [u8; 32],
        payload_length: u32,
    ) -> Result<Self, ExecutionContractError> {
        if published_at.boot_epoch() != engine_epoch {
            return Err(ExecutionContractError::EpochMismatch);
        }
        Ok(Self {
            version,
            engine_epoch,
            task_epoch,
            commit_sequence,
            release_sequence,
            published_at,
            utc,
            quality,
            schema_hash,
            payload_length,
        })
    }

    /// Derives reader-local age and Stale state from monotonic time.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::EpochMismatch`] for another epoch, or
    /// [`ExecutionContractError::InvalidTimestampOrder`] for a future snapshot.
    pub fn observe_at(
        self,
        now: MonotonicTimestamp,
        maximum_age_nanos: u64,
    ) -> Result<SnapshotObservation, ExecutionContractError> {
        if now.boot_epoch() != self.engine_epoch {
            return Err(ExecutionContractError::EpochMismatch);
        }
        let age_nanos = now
            .elapsed_nanos()
            .checked_sub(self.published_at.elapsed_nanos())
            .ok_or(ExecutionContractError::InvalidTimestampOrder)?;
        let freshness = if age_nanos > maximum_age_nanos {
            SnapshotFreshness::Stale
        } else {
            SnapshotFreshness::Fresh
        };
        Ok(SnapshotObservation {
            age_nanos,
            freshness,
        })
    }

    /// Returns quality with the reader-local Stale flag applied when needed.
    ///
    /// # Errors
    ///
    /// Returns timestamp errors from [`Self::observe_at`].
    pub fn observed_quality(
        self,
        now: MonotonicTimestamp,
        maximum_age_nanos: u64,
    ) -> Result<QualityCode, ExecutionContractError> {
        let observation = self.observe_at(now, maximum_age_nanos)?;
        if matches!(observation.freshness(), SnapshotFreshness::Fresh) {
            return Ok(self.quality);
        }
        let flags =
            QualityFlags::from_bits(self.quality.flags().bits() | QualityFlags::STALE.bits());
        QualityCode::new(
            self.quality.severity(),
            self.quality.domain(),
            self.quality.reason(),
            flags,
        )
        .map_err(|_| ExecutionContractError::InvalidEnum)
    }

    /// Classifies sequence progress from an earlier accepted snapshot.
    ///
    /// # Errors
    ///
    /// Returns an epoch or sequence error for regression, engine replacement,
    /// or a new task epoch whose first commit is not zero.
    pub fn progress_after(
        self,
        previous: Self,
    ) -> Result<SnapshotProgress, ExecutionContractError> {
        if self.engine_epoch != previous.engine_epoch {
            return Err(ExecutionContractError::EpochMismatch);
        }
        if self.task_epoch < previous.task_epoch {
            return Err(ExecutionContractError::InvalidCommitSequence);
        }
        if self.task_epoch > previous.task_epoch {
            if self.commit_sequence != CommitSequence::ZERO {
                return Err(ExecutionContractError::InvalidCommitSequence);
            }
            return Ok(SnapshotProgress::NewTaskEpoch);
        }
        if self.commit_sequence <= previous.commit_sequence {
            return Err(ExecutionContractError::InvalidCommitSequence);
        }
        let delta = self.commit_sequence.get() - previous.commit_sequence.get();
        if delta == 1 {
            Ok(SnapshotProgress::Next)
        } else {
            Ok(SnapshotProgress::Gap {
                missed_commits: delta - 1,
            })
        }
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

    /// Returns the source task epoch.
    #[must_use]
    pub const fn task_epoch(self) -> TaskEpoch {
        self.task_epoch
    }

    /// Returns the successful commit sequence.
    #[must_use]
    pub const fn commit_sequence(self) -> CommitSequence {
        self.commit_sequence
    }

    /// Returns the source release sequence.
    #[must_use]
    pub const fn release_sequence(self) -> ReleaseSequence {
        self.release_sequence
    }

    /// Returns the monotonic publication time.
    #[must_use]
    pub const fn published_at(self) -> MonotonicTimestamp {
        self.published_at
    }

    /// Returns the optional complete UTC observation.
    #[must_use]
    pub const fn utc(self) -> Option<UtcObservation> {
        self.utc
    }

    /// Returns producer-supplied quality before reader-local Stale evaluation.
    #[must_use]
    pub const fn quality(self) -> QualityCode {
        self.quality
    }

    /// Returns the payload schema SHA-256.
    #[must_use]
    pub const fn schema_hash(self) -> [u8; 32] {
        self.schema_hash
    }

    /// Returns the payload length in bytes.
    #[must_use]
    pub const fn payload_length(self) -> u32 {
        self.payload_length
    }
}

#[cfg(test)]
mod tests {
    use aurora_types::{
        BootEpochId, MonotonicTimestamp, QualityCode, QualityFlags, QualitySeverity, TimeQuality,
        TimeQualityState, TimeSource, UtcTimestamp,
    };

    use super::{SnapshotFreshness, SnapshotMetadata, SnapshotProgress, UtcObservation};
    use crate::{
        CommitSequence, ExecutionContractError, ExecutionContractVersion, ReleaseSequence,
        TaskEpoch,
    };

    #[test]
    fn utc_conversion_rejects_half_present_values() {
        let utc = UtcTimestamp::new(7, 8);
        let quality = TimeQuality::new(TimeQualityState::Good, TimeSource::Ptp, Some(50), None);
        if let Ok(utc) = utc {
            assert_eq!(
                UtcObservation::try_from_parts(Some(utc), None),
                Err(ExecutionContractError::IncompleteUtcObservation)
            );
            assert_eq!(
                UtcObservation::try_from_parts(None, Some(quality)),
                Err(ExecutionContractError::IncompleteUtcObservation)
            );
            let observation = UtcObservation::try_from_parts(Some(utc), Some(quality));
            assert_eq!(
                observation.map(|value| value.map(UtcObservation::timestamp)),
                Ok(Some(utc))
            );
            assert_eq!(
                observation.map(|value| value.map(UtcObservation::quality)),
                Ok(Some(quality))
            );
        }
        assert_eq!(UtcObservation::try_from_parts(None, None), Ok(None));
    }

    #[test]
    fn freshness_uses_monotonic_time_and_marks_only_excess_age_stale() {
        let metadata = metadata(5, 100);
        assert!(metadata.is_ok());
        if let Ok(metadata) = metadata {
            let at_limit =
                metadata.observe_at(MonotonicTimestamp::new(metadata.engine_epoch(), 110), 10);
            assert_eq!(
                at_limit.map(super::SnapshotObservation::freshness),
                Ok(SnapshotFreshness::Fresh)
            );
            assert_eq!(at_limit.map(super::SnapshotObservation::age_nanos), Ok(10));
            assert_eq!(
                metadata
                    .observed_quality(MonotonicTimestamp::new(metadata.engine_epoch(), 110), 10),
                Ok(metadata.quality())
            );
            let stale_time = MonotonicTimestamp::new(metadata.engine_epoch(), 111);
            assert_eq!(
                metadata
                    .observe_at(stale_time, 10)
                    .map(super::SnapshotObservation::freshness),
                Ok(SnapshotFreshness::Stale)
            );
            assert_eq!(
                metadata
                    .observed_quality(stale_time, 10)
                    .map(QualityCode::flags),
                Ok(QualityFlags::STALE)
            );
            assert_eq!(
                metadata.observe_at(
                    MonotonicTimestamp::new(metadata.engine_epoch(), 99),
                    u64::MAX,
                ),
                Err(ExecutionContractError::InvalidTimestampOrder)
            );
        }
        assert_eq!(SnapshotFreshness::try_from(1), Ok(SnapshotFreshness::Fresh));
        assert_eq!(SnapshotFreshness::try_from(2), Ok(SnapshotFreshness::Stale));
        assert_eq!(
            SnapshotFreshness::try_from(0),
            Err(ExecutionContractError::InvalidEnum)
        );
    }

    #[test]
    fn snapshot_progress_detects_next_gap_reset_and_regression() {
        let previous = metadata(5, 100);
        let next = metadata(6, 101);
        let gap = metadata(9, 102);
        assert!(previous.is_ok());
        assert!(next.is_ok());
        assert!(gap.is_ok());
        if let (Ok(previous), Ok(next), Ok(gap)) = (previous, next, gap) {
            assert_eq!(next.progress_after(previous), Ok(SnapshotProgress::Next));
            assert_eq!(
                gap.progress_after(previous),
                Ok(SnapshotProgress::Gap { missed_commits: 3 })
            );
            assert_eq!(
                previous.progress_after(next),
                Err(ExecutionContractError::InvalidCommitSequence)
            );
            let next_epoch = metadata_for_epoch(2, 0, 103);
            let invalid_epoch_commit = metadata_for_epoch(2, 1, 103);
            assert_eq!(
                next_epoch.and_then(|value| value.progress_after(previous)),
                Ok(SnapshotProgress::NewTaskEpoch)
            );
            assert_eq!(
                invalid_epoch_commit.and_then(|value| value.progress_after(previous)),
                Err(ExecutionContractError::InvalidCommitSequence)
            );
            let previous_later_epoch = metadata_for_epoch(3, 0, 103);
            assert_eq!(
                previous_later_epoch.and_then(|value| previous.progress_after(value)),
                Err(ExecutionContractError::InvalidCommitSequence)
            );
        }
    }

    #[test]
    fn snapshot_rejects_engine_epoch_mismatches() {
        let engine_epoch = test_epoch();
        let timestamp_epoch = BootEpochId::from_bytes([
            0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x99, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
            0x39, 0x8f,
        ])
        .map_err(|_| ExecutionContractError::InvalidEnum);
        assert!(engine_epoch.is_ok());
        assert!(timestamp_epoch.is_ok());
        if let (Ok(engine_epoch), Ok(timestamp_epoch)) = (engine_epoch, timestamp_epoch) {
            let mismatched_metadata = TaskEpoch::new(1).and_then(|task_epoch| {
                SnapshotMetadata::new(
                    ExecutionContractVersion::V1_0,
                    engine_epoch,
                    task_epoch,
                    CommitSequence::ZERO,
                    ReleaseSequence::ZERO,
                    MonotonicTimestamp::new(timestamp_epoch, 10),
                    None,
                    QualityCode::GOOD,
                    [0; 32],
                    0,
                )
            });
            assert_eq!(
                mismatched_metadata,
                Err(ExecutionContractError::EpochMismatch)
            );
            let current_metadata = metadata(0, 10);
            assert_eq!(
                current_metadata.and_then(|value| {
                    value.observe_at(MonotonicTimestamp::new(timestamp_epoch, 11), 1)
                }),
                Err(ExecutionContractError::EpochMismatch)
            );
            let previous = metadata(0, 10);
            let current = TaskEpoch::new(1).and_then(|task_epoch| {
                SnapshotMetadata::new(
                    ExecutionContractVersion::V1_0,
                    timestamp_epoch,
                    task_epoch,
                    CommitSequence::new(1),
                    ReleaseSequence::new(1),
                    MonotonicTimestamp::new(timestamp_epoch, 11),
                    None,
                    QualityCode::GOOD,
                    [0; 32],
                    0,
                )
            });
            assert_eq!(
                current.and_then(|value| previous.and_then(|old| value.progress_after(old))),
                Err(ExecutionContractError::EpochMismatch)
            );
        }
    }

    fn metadata(
        commit_sequence: u64,
        published_nanos: u64,
    ) -> Result<SnapshotMetadata, ExecutionContractError> {
        metadata_for_epoch(1, commit_sequence, published_nanos)
    }

    fn metadata_for_epoch(
        task_epoch: u64,
        commit_sequence: u64,
        published_nanos: u64,
    ) -> Result<SnapshotMetadata, ExecutionContractError> {
        let epoch = test_epoch()?;
        SnapshotMetadata::new(
            ExecutionContractVersion::V1_0,
            epoch,
            TaskEpoch::new(task_epoch)?,
            CommitSequence::new(commit_sequence),
            ReleaseSequence::new(commit_sequence),
            MonotonicTimestamp::new(epoch, published_nanos),
            None,
            QualityCode::new(QualitySeverity::Good, 0, 0, QualityFlags::NONE)
                .map_err(|_| ExecutionContractError::InvalidEnum)?,
            [3; 32],
            64,
        )
    }

    fn test_epoch() -> Result<BootEpochId, ExecutionContractError> {
        BootEpochId::from_bytes([
            0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
            0x39, 0x8f,
        ])
        .map_err(|_| ExecutionContractError::InvalidEnum)
    }
}
