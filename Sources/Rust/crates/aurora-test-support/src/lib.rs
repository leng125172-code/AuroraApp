//! Deterministic test facilities that must never be linked as production dependencies.

use aurora_types::{BootEpochId, DurationNanos, LocalHandle, MonotonicTimestamp, UtcTimestamp};
use thiserror::Error;

/// Error produced when deterministic simulated time overflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("simulated time overflow")]
pub struct ClockOverflow;

/// Manually advanced monotonic and UTC clock for deterministic tests.
#[derive(Debug, Clone, Copy)]
pub struct ManualClock {
    epoch: BootEpochId,
    elapsed_nanos: u64,
    utc: UtcTimestamp,
}

impl ManualClock {
    /// Creates a clock at an explicit boot epoch and UTC instant.
    #[must_use]
    pub const fn new(epoch: BootEpochId, utc: UtcTimestamp) -> Self {
        Self {
            epoch,
            elapsed_nanos: 0,
            utc,
        }
    }

    /// Returns the current monotonic instant.
    #[must_use]
    pub const fn monotonic(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::new(self.epoch, self.elapsed_nanos)
    }

    /// Returns the current UTC instant.
    #[must_use]
    pub const fn utc(&self) -> UtcTimestamp {
        self.utc
    }

    /// Advances both clocks by the same non-negative duration.
    ///
    /// # Errors
    ///
    /// Returns [`ClockOverflow`] when either simulated clock cannot represent
    /// the advanced instant.
    pub fn advance(&mut self, duration: DurationNanos) -> Result<(), ClockOverflow> {
        let elapsed = self
            .elapsed_nanos
            .checked_add(duration.get())
            .ok_or(ClockOverflow)?;
        let extra_seconds = duration.get() / u64::from(UtcTimestamp::NANOS_PER_SECOND);
        let extra_nanos = duration.get() % u64::from(UtcTimestamp::NANOS_PER_SECOND);
        let nanos_sum = u64::from(self.utc.nanos()) + extra_nanos;
        let carry = nanos_sum / u64::from(UtcTimestamp::NANOS_PER_SECOND);
        let seconds_delta = extra_seconds.checked_add(carry).ok_or(ClockOverflow)?;
        let seconds_delta = i64::try_from(seconds_delta).map_err(|_| ClockOverflow)?;
        let seconds = self
            .utc
            .seconds()
            .checked_add(seconds_delta)
            .ok_or(ClockOverflow)?;
        let nanos = u32::try_from(nanos_sum % u64::from(UtcTimestamp::NANOS_PER_SECOND))
            .map_err(|_| ClockOverflow)?;
        self.utc = UtcTimestamp::new(seconds, nanos).map_err(|_| ClockOverflow)?;
        self.elapsed_nanos = elapsed;
        Ok(())
    }
}

/// Error returned when a virtual I/O handle exceeds the configured fixed capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("virtual I/O handle is outside the fixed image capacity")]
pub struct VirtualIoError;

/// Fixed-capacity signed 64-bit virtual I/O image for deterministic tests.
#[derive(Debug, Clone)]
pub struct VirtualIoImage {
    values: Vec<Option<i64>>,
}

impl VirtualIoImage {
    /// Allocates the fixed image capacity during test setup.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            values: vec![None; capacity],
        }
    }

    /// Writes a value to a valid local handle.
    ///
    /// # Errors
    ///
    /// Returns [`VirtualIoError`] when `handle` is outside the configured image.
    pub fn write(&mut self, handle: LocalHandle, value: i64) -> Result<(), VirtualIoError> {
        let index = usize::try_from(handle.get()).map_err(|_| VirtualIoError)?;
        let slot = self.values.get_mut(index).ok_or(VirtualIoError)?;
        *slot = Some(value);
        Ok(())
    }

    /// Reads an initialized value from a valid local handle.
    ///
    /// # Errors
    ///
    /// Returns [`VirtualIoError`] when `handle` is outside the configured image.
    pub fn read(&self, handle: LocalHandle) -> Result<Option<i64>, VirtualIoError> {
        let index = usize::try_from(handle.get()).map_err(|_| VirtualIoError)?;
        self.values.get(index).copied().ok_or(VirtualIoError)
    }

    /// Returns the immutable configured capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.values.len()
    }
}

/// Deterministic fault injected at a scan or event boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultKind {
    /// Simulate an unavailable input read.
    DropRead,
    /// Simulate a rejected output write.
    RejectWrite,
    /// Simulate a configured deadline miss.
    DeadlineMiss,
    /// Simulate an explicit wall-clock adjustment event.
    ClockAdjustment,
}

/// One fault scheduled for a deterministic tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScheduledFault {
    /// Zero-based simulation tick.
    pub tick: u64,
    /// Fault to inject at the tick boundary.
    pub kind: FaultKind,
}

/// Ordered deterministic fault plan.
#[derive(Debug, Clone)]
pub struct FaultPlan {
    faults: Vec<ScheduledFault>,
    cursor: usize,
}

impl FaultPlan {
    /// Creates a plan sorted by tick while preserving same-tick insertion order.
    #[must_use]
    pub fn new(mut faults: Vec<ScheduledFault>) -> Self {
        faults.sort_by_key(|fault| fault.tick);
        Self { faults, cursor: 0 }
    }

    /// Returns and advances over all faults scheduled for `tick`.
    pub fn take_at(&mut self, tick: u64) -> impl Iterator<Item = FaultKind> + '_ {
        let start = self.cursor;
        while self.cursor < self.faults.len() && self.faults[self.cursor].tick == tick {
            self.cursor += 1;
        }
        self.faults[start..self.cursor]
            .iter()
            .map(|fault| fault.kind)
    }

    /// Returns whether every scheduled fault has been consumed.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.cursor == self.faults.len()
    }
}

/// Small deterministic generator with a stable cross-platform `SplitMix64` sequence.
#[derive(Debug, Clone, Copy)]
pub struct ReplayRng {
    state: u64,
}

impl ReplayRng {
    /// Creates a replay generator from an explicit seed.
    #[must_use]
    pub const fn from_seed(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Returns the next deterministic value.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

#[cfg(test)]
mod tests {
    use aurora_types::{BootEpochId, DurationNanos, LocalHandle, UtcTimestamp};

    use super::{FaultKind, FaultPlan, ManualClock, ReplayRng, ScheduledFault, VirtualIoImage};

    #[test]
    fn manual_clock_advances_across_second_boundary() {
        let epoch = BootEpochId::generate();
        let utc = UtcTimestamp::new(10, 900_000_000);
        assert!(utc.is_ok());
        if let Ok(utc) = utc {
            let mut clock = ManualClock::new(epoch, utc);
            assert!(clock.advance(DurationNanos::new(200_000_000)).is_ok());
            assert_eq!(clock.utc().seconds(), 11);
            assert_eq!(clock.utc().nanos(), 100_000_000);
            assert_eq!(clock.monotonic().elapsed_nanos(), 200_000_000);
        }
    }

    #[test]
    fn virtual_io_is_fixed_capacity() {
        let mut image = VirtualIoImage::with_capacity(1);
        let zero = LocalHandle::ZERO;
        let one = LocalHandle::new(1).unwrap_or(LocalHandle::ZERO);
        assert_eq!(image.capacity(), 1);
        assert_eq!(image.read(zero), Ok(None));
        assert_eq!(image.write(zero, 42), Ok(()));
        assert_eq!(image.read(zero), Ok(Some(42)));
        assert!(image.write(one, 42).is_err());
        assert!(image.read(one).is_err());
    }

    #[test]
    fn manual_clock_reports_representable_overflow() {
        let utc = UtcTimestamp::new(i64::MAX, 0).unwrap_or(UtcTimestamp::UNIX_EPOCH);
        let mut clock = ManualClock::new(BootEpochId::generate(), utc);
        assert!(clock.advance(DurationNanos::new(1_000_000_000)).is_err());

        let mut clock = ManualClock::new(BootEpochId::generate(), UtcTimestamp::UNIX_EPOCH);
        clock.elapsed_nanos = u64::MAX;
        assert!(clock.advance(DurationNanos::new(1)).is_err());
    }

    #[test]
    fn fault_order_and_random_sequence_are_replayable() {
        let mut plan = FaultPlan::new(vec![
            ScheduledFault {
                tick: 2,
                kind: FaultKind::DeadlineMiss,
            },
            ScheduledFault {
                tick: 1,
                kind: FaultKind::DropRead,
            },
        ]);
        assert_eq!(
            plan.take_at(1).collect::<Vec<_>>(),
            vec![FaultKind::DropRead]
        );
        assert_eq!(
            plan.take_at(2).collect::<Vec<_>>(),
            vec![FaultKind::DeadlineMiss]
        );
        assert!(plan.is_complete());

        let mut left = ReplayRng::from_seed(7);
        let mut right = ReplayRng::from_seed(7);
        assert_eq!(left.next_u64(), right.next_u64());
        assert_eq!(left.next_u64(), right.next_u64());
    }
}
