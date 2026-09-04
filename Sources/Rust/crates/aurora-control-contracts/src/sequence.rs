//! Explicit generations and sequence counters for R0 execution records.

use crate::ExecutionContractError;

macro_rules! sequence_counter {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);

        impl $name {
            /// First valid value in a new epoch.
            pub const ZERO: Self = Self(0);

            /// Creates a counter from its fixed-width representation.
            #[must_use]
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            /// Returns the fixed-width representation.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }

            /// Returns the next value without allowing wraparound.
            ///
            /// # Errors
            ///
            /// Returns [`ExecutionContractError::CounterOverflow`] at
            /// `u64::MAX`.
            pub const fn checked_next(self) -> Result<Self, ExecutionContractError> {
                match self.0.checked_add(1) {
                    Some(value) => Ok(Self(value)),
                    None => Err(ExecutionContractError::CounterOverflow),
                }
            }
        }
    };
}

sequence_counter!(
    ReleaseSequence,
    "A scheduled-release sequence scoped to one task epoch."
);
sequence_counter!(
    CommitSequence,
    "A successful-commit sequence scoped to one task epoch."
);
sequence_counter!(
    EventSequence,
    "A Trace event-attempt sequence scoped to one engine epoch."
);
sequence_counter!(
    FallbackRequestSequence,
    "A Fallback request sequence scoped to one engine epoch."
);

macro_rules! non_zero_generation {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);

        impl $name {
            /// Creates a non-zero generation.
            ///
            /// # Errors
            ///
            /// Returns [`ExecutionContractError::InvalidGeneration`] for zero.
            pub const fn new(value: u64) -> Result<Self, ExecutionContractError> {
                if value == 0 {
                    Err(ExecutionContractError::InvalidGeneration)
                } else {
                    Ok(Self(value))
                }
            }

            /// Returns the fixed-width representation.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }

            /// Returns the next generation without allowing wraparound.
            ///
            /// # Errors
            ///
            /// Returns [`ExecutionContractError::CounterOverflow`] at
            /// `u64::MAX`.
            pub const fn checked_next(self) -> Result<Self, ExecutionContractError> {
                match self.0.checked_add(1) {
                    Some(value) => Ok(Self(value)),
                    None => Err(ExecutionContractError::CounterOverflow),
                }
            }
        }
    };
}

non_zero_generation!(TaskEpoch, "A task initialization generation.");
non_zero_generation!(FaultGeneration, "A latched task-Fault generation.");

#[cfg(test)]
mod tests {
    use super::{
        CommitSequence, EventSequence, FallbackRequestSequence, FaultGeneration, ReleaseSequence,
        TaskEpoch,
    };
    use crate::ExecutionContractError;

    #[test]
    fn generations_reject_zero_and_all_counters_reject_wrap() {
        assert_eq!(
            TaskEpoch::new(0),
            Err(ExecutionContractError::InvalidGeneration)
        );
        assert_eq!(FaultGeneration::new(7).map(FaultGeneration::get), Ok(7));
        assert_eq!(
            CommitSequence::new(u64::MAX).checked_next(),
            Err(ExecutionContractError::CounterOverflow)
        );
        assert_eq!(
            CommitSequence::ZERO.checked_next().map(CommitSequence::get),
            Ok(1)
        );
        assert_eq!(
            ReleaseSequence::new(2)
                .checked_next()
                .map(ReleaseSequence::get),
            Ok(3)
        );
        assert_eq!(
            EventSequence::ZERO.checked_next().map(EventSequence::get),
            Ok(1)
        );
        assert_eq!(
            FallbackRequestSequence::ZERO
                .checked_next()
                .map(FallbackRequestSequence::get),
            Ok(1)
        );
        assert_eq!(
            TaskEpoch::new(1)
                .and_then(TaskEpoch::checked_next)
                .map(TaskEpoch::get),
            Ok(2)
        );
        assert_eq!(
            FaultGeneration::new(u64::MAX).and_then(FaultGeneration::checked_next),
            Err(ExecutionContractError::CounterOverflow)
        );
    }
}
