//! Validation failures for the R0 execution contract.

use thiserror::Error;

/// A rejected R0 execution-contract value or conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ExecutionContractError {
    /// A required contract version was absent at a conversion boundary.
    #[error("R0 execution contract version is required")]
    MissingContractVersion,
    /// The contract version is not the supported Preview 1.0 contract.
    #[error("unsupported R0 execution contract version")]
    UnsupportedContractVersion,
    /// A raw enum value is unspecified or unknown.
    #[error("R0 execution enum value is unspecified or unknown")]
    InvalidEnum,
    /// A period was zero.
    #[error("task period must be greater than zero nanoseconds")]
    InvalidPeriod,
    /// A phase was not smaller than its period.
    #[error("task phase must be smaller than its period")]
    InvalidPhase,
    /// A deadline was zero or greater than its period.
    #[error("task deadline must be in the range 1..=period nanoseconds")]
    InvalidDeadline,
    /// An execution budget was zero or greater than the hard limit.
    #[error("execution budget must be in the range 1..=hard limit nanoseconds")]
    InvalidExecutionBudget,
    /// A hard limit was zero or greater than the relative deadline.
    #[error("hard limit must be in the range execution budget..=deadline nanoseconds")]
    InvalidHardLimit,
    /// A miss window was zero or exceeded its target-declared capacity.
    #[error("miss window must be in the range 1..=target capacity")]
    InvalidMissWindow,
    /// `MaxMisses` exceeded the miss-window length.
    #[error("maximum misses must not exceed the miss-window length")]
    InvalidMaxMisses,
    /// The consecutive-miss threshold was zero or exceeded the window length.
    #[error("consecutive misses must be in the range 1..=miss window")]
    InvalidConsecutiveMisses,
    /// A task epoch or fault generation used its reserved zero value.
    #[error("task epoch and fault generation must be non-zero")]
    InvalidGeneration,
    /// A sequence or generation could not be incremented without wrapping.
    #[error("execution sequence or generation is exhausted")]
    CounterOverflow,
    /// A fixed capacity was zero or exceeded its target-declared maximum.
    #[error("capacity must be in the range 1..=target maximum")]
    InvalidCapacity,
    /// UTC and its `TimeQuality` were not both present or both absent.
    #[error("UTC timestamp and TimeQuality must be present or absent together")]
    IncompleteUtcObservation,
    /// Trace execution start and finish were not both present or both absent.
    #[error("trace execution start and finish must be present or absent together")]
    IncompleteExecutionTiming,
    /// Monotonic timestamps came from different boot epochs.
    #[error("monotonic timestamps must use the same boot epoch")]
    EpochMismatch,
    /// A monotonic timestamp was earlier than a required predecessor.
    #[error("monotonic timestamps are not in contract order")]
    InvalidTimestampOrder,
    /// A commit sequence moved backwards or skipped without an explicit gap.
    #[error("commit sequence transition is invalid")]
    InvalidCommitSequence,
    /// A Trace occupancy was greater than its fixed capacity.
    #[error("trace occupancy must not exceed capacity")]
    OccupancyExceedsCapacity,
    /// A Trace high-water mark was inconsistent with occupancy or capacity.
    #[error("trace high-water mark must be between occupancy and capacity")]
    InvalidHighWaterMark,
    /// Published and dropped counts exceeded attempted count.
    #[error("trace outcome counts must not exceed attempted count")]
    InvalidTraceCounts,
}
