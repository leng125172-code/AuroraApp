//! UTC, monotonic, duration, and time-quality types.

use thiserror::Error;

use crate::BootEpochId;

/// Error returned when a UTC timestamp is not normalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("nanoseconds must be less than 1,000,000,000")]
pub struct UtcTimestampError;

/// A normalized UTC timestamp without an implied local time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcTimestamp {
    seconds: i64,
    nanos: u32,
}

impl UtcTimestamp {
    /// Unix epoch represented as a normalized UTC timestamp.
    pub const UNIX_EPOCH: Self = Self {
        seconds: 0,
        nanos: 0,
    };
    /// Number of nanoseconds in one second.
    pub const NANOS_PER_SECOND: u32 = 1_000_000_000;

    /// Creates a normalized UTC timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`UtcTimestampError`] when `nanos` is not less than one second.
    pub const fn new(seconds: i64, nanos: u32) -> Result<Self, UtcTimestampError> {
        if nanos < Self::NANOS_PER_SECOND {
            Ok(Self { seconds, nanos })
        } else {
            Err(UtcTimestampError)
        }
    }

    /// Returns whole seconds from the Unix epoch.
    #[must_use]
    pub const fn seconds(self) -> i64 {
        self.seconds
    }

    /// Returns the normalized sub-second nanosecond part.
    #[must_use]
    pub const fn nanos(self) -> u32 {
        self.nanos
    }
}

/// Non-negative duration represented in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurationNanos(u64);

impl DurationNanos {
    /// Creates a duration from nanoseconds.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the duration in nanoseconds.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Monotonic time within one explicitly identified boot epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MonotonicTimestamp {
    boot_epoch: BootEpochId,
    elapsed_nanos: u64,
}

impl MonotonicTimestamp {
    /// Creates a monotonic timestamp scoped to `boot_epoch`.
    #[must_use]
    pub const fn new(boot_epoch: BootEpochId, elapsed_nanos: u64) -> Self {
        Self {
            boot_epoch,
            elapsed_nanos,
        }
    }

    /// Returns the boot epoch in which the elapsed value is meaningful.
    #[must_use]
    pub const fn boot_epoch(self) -> BootEpochId {
        self.boot_epoch
    }

    /// Returns nanoseconds elapsed within the boot epoch.
    #[must_use]
    pub const fn elapsed_nanos(self) -> u64 {
        self.elapsed_nanos
    }
}

/// State of absolute-time synchronization quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TimeQualityState {
    /// No usable time-source information is available.
    Unknown = 0,
    /// The clock is acquiring and validating a time source.
    Synchronizing = 1,
    /// The clock meets the configured synchronization limits.
    Good = 2,
    /// The source is unavailable but the clock remains within holdover limits.
    Holdover = 3,
    /// The clock is usable only by explicitly tolerant consumers.
    Degraded = 4,
    /// Absolute time must not be used for time-sensitive decisions.
    Invalid = 5,
}

/// Kind of source used to establish UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TimeSource {
    /// No source is known.
    Unknown = 0,
    /// Operating-system wall clock.
    System = 1,
    /// Network Time Protocol.
    Ntp = 2,
    /// Precision Time Protocol.
    Ptp = 3,
    /// Global navigation satellite system.
    Gnss = 4,
    /// Explicit operator-supplied time.
    Manual = 5,
}

/// Absolute-time quality with an explicit uncertainty bound and synchronization evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeQuality {
    state: TimeQualityState,
    source: TimeSource,
    max_error_nanos: Option<u64>,
    last_sync_utc: Option<UtcTimestamp>,
}

impl TimeQuality {
    /// Creates a time-quality observation.
    #[must_use]
    pub const fn new(
        state: TimeQualityState,
        source: TimeSource,
        max_error_nanos: Option<u64>,
        last_sync_utc: Option<UtcTimestamp>,
    ) -> Self {
        Self {
            state,
            source,
            max_error_nanos,
            last_sync_utc,
        }
    }

    /// Returns the synchronization state.
    #[must_use]
    pub const fn state(self) -> TimeQualityState {
        self.state
    }

    /// Returns the active time-source kind.
    #[must_use]
    pub const fn source(self) -> TimeSource {
        self.source
    }

    /// Returns the proven UTC error bound, when known.
    #[must_use]
    pub const fn max_error_nanos(self) -> Option<u64> {
        self.max_error_nanos
    }

    /// Returns the most recent successful synchronization time, when known.
    #[must_use]
    pub const fn last_sync_utc(self) -> Option<UtcTimestamp> {
        self.last_sync_utc
    }
}

#[cfg(test)]
mod tests {
    use crate::BootEpochId;

    use super::{
        DurationNanos, MonotonicTimestamp, TimeQuality, TimeQualityState, TimeSource, UtcTimestamp,
        UtcTimestampError,
    };

    #[test]
    fn utc_timestamp_requires_normalized_nanoseconds() {
        assert!(UtcTimestamp::new(0, 999_999_999).is_ok());
        assert_eq!(UtcTimestamp::new(0, 1_000_000_000), Err(UtcTimestampError));
    }

    #[test]
    fn duration_monotonic_and_quality_accessors_preserve_values() {
        let utc = UtcTimestamp::new(-1, 42).unwrap_or(UtcTimestamp {
            seconds: 0,
            nanos: 0,
        });
        let epoch = BootEpochId::from_bytes([
            0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
            0x39, 0x8f,
        ]);
        assert!(epoch.is_ok());
        if let Ok(epoch) = epoch {
            let monotonic = MonotonicTimestamp::new(epoch, 99);
            assert_eq!(monotonic.boot_epoch(), epoch);
            assert_eq!(monotonic.elapsed_nanos(), 99);
        }
        assert_eq!(DurationNanos::new(17).get(), 17);

        let quality = TimeQuality::new(
            TimeQualityState::Holdover,
            TimeSource::Ptp,
            Some(1_000),
            Some(utc),
        );
        assert_eq!(quality.state(), TimeQualityState::Holdover);
        assert_eq!(quality.source(), TimeSource::Ptp);
        assert_eq!(quality.max_error_nanos(), Some(1_000));
        assert_eq!(quality.last_sync_utc(), Some(utc));
    }
}
