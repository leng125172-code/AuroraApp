//! Driver SDK rejection and normalized fault categories.

use aurora_io_guardian::GapReason;
use thiserror::Error;

/// Backend-neutral driver fault and injection categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverFaultKind {
    /// Driver process or execution context terminated unexpectedly.
    Crashed,
    /// A call exceeded its declared blocking/work boundary.
    BlockingDetected,
    /// The absolute operation deadline was reached.
    DeadlineExceeded,
    /// A fixed request, response, or diagnostic queue was full.
    QueueFull,
    /// A frame violated its normalized length or structural contract.
    MalformedFrame,
    /// The peer used an unsupported Driver Adapter contract version.
    ProtocolVersionMismatch,
    /// The configured device or link disconnected.
    Disconnected,
    /// A response arrived with a stale or reordered sequence.
    Reordered,
    /// CRC or equivalent frame-integrity validation failed.
    CrcFailure,
    /// `EtherCAT` working-counter validation failed.
    WorkingCounterFailure,
    /// CAN entered bus-off.
    BusOff,
}

impl DriverFaultKind {
    /// Maps the normalized fault to shared-image gap semantics without backend-native codes.
    #[must_use]
    pub const fn gap_reason(self) -> GapReason {
        match self {
            Self::QueueFull => GapReason::QueueFull,
            Self::DeadlineExceeded | Self::BlockingDetected => GapReason::Timeout,
            Self::MalformedFrame | Self::CrcFailure => GapReason::Checksum,
            Self::WorkingCounterFailure => GapReason::WorkingCounter,
            Self::Crashed | Self::ProtocolVersionMismatch | Self::BusOff => {
                GapReason::BackendUnavailable
            }
            Self::Disconnected => GapReason::LinkDown,
            Self::Reordered => GapReason::SequenceGap,
        }
    }
}

/// Typed failure returned before a driver can exceed its authority or fixed budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DriverSdkError {
    /// A capacity, duration, work count, or fixed buffer boundary is invalid.
    #[error("driver fixed capacity or work boundary is invalid")]
    InvalidCapacity,
    /// A catalog is missing, duplicated, reordered, or contains an extra entry.
    #[error("driver catalog is not an exact canonical closure")]
    CatalogMismatch,
    /// A package is not in both the build allowlist and Target Profile approval set.
    #[error("driver package is not installed and approved")]
    PackageNotApproved,
    /// The selected execution mode is not allowed for the implementation risk.
    #[error("driver implementation must use the isolated execution mode")]
    IsolationRequired,
    /// Configuration, layout, capability, instance, interface, or lease authority drifted.
    #[error("driver authority does not match the immutable instance plan")]
    AuthorityMismatch,
    /// A lifecycle operation is invalid from the current state.
    #[error("driver lifecycle transition is invalid")]
    InvalidStateTransition,
    /// An interface already has another owner.
    #[error("physical interface already has a driver owner")]
    DeviceAlreadyOwned,
    /// The requested device, shared slot, group, or capability was not granted.
    #[error("driver attempted to access a resource outside its fixed grant")]
    UnauthorizedResource,
    /// Required sandbox controls are missing or differ from the approved policy.
    #[error("isolated Driver Host sandbox evidence is incomplete")]
    SandboxUnavailable,
    /// A monotonic timestamp regressed.
    #[error("driver monotonic time regressed")]
    MonotonicTimeRegression,
    /// An absolute deadline was already reached.
    #[error("driver operation deadline was reached")]
    DeadlineExceeded,
    /// A bounded non-cyclic operation was cancelled before this step.
    #[error("driver operation was cancelled")]
    OperationCancelled,
    /// A normalized driver fault was observed.
    #[error("driver operation reported a normalized fault")]
    DriverFault(DriverFaultKind),
    /// Checked size, sequence, timestamp, or counter arithmetic overflowed.
    #[error("driver checked arithmetic overflowed")]
    ArithmeticOverflow,
    /// Fixed initialization allocation failed.
    #[error("driver fixed initialization allocation failed")]
    AllocationFailed,
}
