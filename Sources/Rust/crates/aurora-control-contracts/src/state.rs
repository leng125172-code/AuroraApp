//! Exhaustive engine, task, miss, and Fault states.

use crate::ExecutionContractError;

macro_rules! exhaustive_u8_enum {
    (
        $(#[$meta:meta])*
        $name:ident {
            $($(#[$variant_meta:meta])* $variant:ident = $value:literal),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum $name {
            $($(#[$variant_meta])* $variant = $value),+
        }

        impl TryFrom<u8> for $name {
            type Error = ExecutionContractError;

            fn try_from(value: u8) -> Result<Self, Self::Error> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(ExecutionContractError::InvalidEnum),
                }
            }
        }
    };
}

exhaustive_u8_enum! {
    /// Lifecycle state of the Control Engine process.
    EngineState {
        /// The engine is not executing tasks.
        Stopped = 1,
        /// The engine is validating and initializing its static plan.
        Starting = 2,
        /// Every enabled task is running normally.
        Running = 3,
        /// At least one task is degraded while healthy tasks continue.
        Degraded = 4,
        /// An engine-level Fault prevents cyclic execution.
        FaultLocked = 5,
        /// The engine is completing a bounded boundary stop.
        Stopping = 6,
    }
}

exhaustive_u8_enum! {
    /// Lifecycle state of one cyclic task.
    TaskState {
        /// The task is not scheduled.
        Stopped = 1,
        /// The task is rebuilding declared initial state.
        Reinitializing = 2,
        /// The task has no active miss or budget degradation.
        Running = 3,
        /// The task remains executable with visible degradation.
        Degraded = 4,
        /// The task cannot execute until an authorized reinitialization.
        FaultLocked = 5,
    }
}

exhaustive_u8_enum! {
    /// Outcome used by deterministic deadline-miss accounting.
    MissOutcome {
        /// The release completed no later than its deadline.
        OnTime = 1,
        /// An older release was skipped rather than replayed.
        SkippedRelease = 2,
        /// Dispatch observed the release only after its deadline.
        StartAfterDeadline = 3,
        /// Execution completed after its absolute deadline.
        FinishAfterDeadline = 4,
    }
}

/// A stable, exhaustive reason for latching a task Fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum FaultReason {
    /// Absolute release/deadline arithmetic was not representable.
    ScheduleTimeOverflow = 1,
    /// A control-semantic generation or sequence was exhausted.
    CounterOverflow = 2,
    /// Monotonic time violated epoch or ordering invariants.
    ClockContractViolation = 3,
    /// Execution exceeded the task hard limit.
    HardLimitExceeded = 4,
    /// Deadline miss count exceeded the configured window allowance.
    MissWindowExceeded = 5,
    /// Consecutive deadline misses reached their configured threshold.
    ConsecutiveMissesReached = 6,
    /// Task code returned a declared execution Fault.
    TaskExecutionFault = 7,
    /// Reinitialization could not establish declared initial state.
    ReinitializationFailed = 8,
    /// A fixed-capacity state or output boundary was exceeded.
    CapacityExceeded = 9,
    /// Fallback request publication could not be made durable in its mailbox.
    FallbackPublicationFault = 10,
}

impl TryFrom<u16> for FaultReason {
    type Error = ExecutionContractError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::ScheduleTimeOverflow),
            2 => Ok(Self::CounterOverflow),
            3 => Ok(Self::ClockContractViolation),
            4 => Ok(Self::HardLimitExceeded),
            5 => Ok(Self::MissWindowExceeded),
            6 => Ok(Self::ConsecutiveMissesReached),
            7 => Ok(Self::TaskExecutionFault),
            8 => Ok(Self::ReinitializationFailed),
            9 => Ok(Self::CapacityExceeded),
            10 => Ok(Self::FallbackPublicationFault),
            _ => Err(ExecutionContractError::InvalidEnum),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{EngineState, FaultReason, MissOutcome, TaskState};
    use crate::ExecutionContractError;

    #[test]
    fn raw_enum_boundaries_reject_unspecified_and_unknown_values() {
        for (raw, value) in [
            (1, EngineState::Stopped),
            (2, EngineState::Starting),
            (3, EngineState::Running),
            (4, EngineState::Degraded),
            (5, EngineState::FaultLocked),
            (6, EngineState::Stopping),
        ] {
            assert_eq!(EngineState::try_from(raw), Ok(value));
        }
        for (raw, value) in [
            (1, TaskState::Stopped),
            (2, TaskState::Reinitializing),
            (3, TaskState::Running),
            (4, TaskState::Degraded),
            (5, TaskState::FaultLocked),
        ] {
            assert_eq!(TaskState::try_from(raw), Ok(value));
        }
        for (raw, value) in [
            (1, MissOutcome::OnTime),
            (2, MissOutcome::SkippedRelease),
            (3, MissOutcome::StartAfterDeadline),
            (4, MissOutcome::FinishAfterDeadline),
        ] {
            assert_eq!(MissOutcome::try_from(raw), Ok(value));
        }
        for (raw, value) in [
            (1, FaultReason::ScheduleTimeOverflow),
            (2, FaultReason::CounterOverflow),
            (3, FaultReason::ClockContractViolation),
            (4, FaultReason::HardLimitExceeded),
            (5, FaultReason::MissWindowExceeded),
            (6, FaultReason::ConsecutiveMissesReached),
            (7, FaultReason::TaskExecutionFault),
            (8, FaultReason::ReinitializationFailed),
            (9, FaultReason::CapacityExceeded),
            (10, FaultReason::FallbackPublicationFault),
        ] {
            assert_eq!(FaultReason::try_from(raw), Ok(value));
        }
        assert_eq!(
            EngineState::try_from(0),
            Err(ExecutionContractError::InvalidEnum)
        );
        assert_eq!(
            TaskState::try_from(99),
            Err(ExecutionContractError::InvalidEnum)
        );
        assert_eq!(
            MissOutcome::try_from(0),
            Err(ExecutionContractError::InvalidEnum)
        );
        assert_eq!(
            FaultReason::try_from(11),
            Err(ExecutionContractError::InvalidEnum)
        );
    }
}
