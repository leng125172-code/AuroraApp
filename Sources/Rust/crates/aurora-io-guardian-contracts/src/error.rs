//! Exhaustive Guardian error identifiers and typed contract failures.

use thiserror::Error;

/// Stable error catalog frozen by SPEC-R3-001.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GuardianErrorCode {
    /// Contract major/minor/layout negotiation failed.
    UnsupportedContractVersion,
    /// A required capability is unknown or unavailable.
    UnknownRequiredCapability,
    /// A shared-region descriptor or layout is invalid.
    InvalidRegionLayout,
    /// Required atomic behavior is unavailable.
    AtomicRequirementUnavailable,
    /// Peer, device, or topology identity does not match.
    IdentityOrTopologyMismatch,
    /// Configuration generation or digest does not match.
    ConfigurationDigestMismatch,
    /// A fixed capacity or time budget is invalid or exceeded.
    CapacityOrBudgetExceeded,
    /// An interface already has an owner.
    DeviceAlreadyOwned,
    /// A lease is absent, stale, foreign, expired, or reused.
    StaleOrForeignLease,
    /// An image sequence is duplicated, skipped, or regresses.
    ImageSequenceViolation,
    /// A complete image could not be latched without contention.
    ImageContended,
    /// Heartbeat or output freshness expired.
    ImageExpired,
    /// An update group missed its declared window.
    UpdateGroupMiss,
    /// A protocol operation timed out.
    ProtocolTimeout,
    /// A frame or protocol integrity check failed.
    ProtocolIntegrityFailure,
    /// A link or device was lost.
    LinkOrDeviceLost,
    /// A fixed queue is full.
    QueueFull,
    /// The selected backend is unavailable.
    BackendUnavailable,
    /// Active Fallback has not been armed.
    FallbackNotArmed,
    /// Required watchdog or external protection is unavailable.
    WatchdogProtectionUnavailable,
    /// Recovery requires an explicit authorization.
    RecoveryAuthorizationRequired,
}

impl GuardianErrorCode {
    /// Exact catalog order used by specifications and golden tests.
    pub const ALL: [Self; 21] = [
        Self::UnsupportedContractVersion,
        Self::UnknownRequiredCapability,
        Self::InvalidRegionLayout,
        Self::AtomicRequirementUnavailable,
        Self::IdentityOrTopologyMismatch,
        Self::ConfigurationDigestMismatch,
        Self::CapacityOrBudgetExceeded,
        Self::DeviceAlreadyOwned,
        Self::StaleOrForeignLease,
        Self::ImageSequenceViolation,
        Self::ImageContended,
        Self::ImageExpired,
        Self::UpdateGroupMiss,
        Self::ProtocolTimeout,
        Self::ProtocolIntegrityFailure,
        Self::LinkOrDeviceLost,
        Self::QueueFull,
        Self::BackendUnavailable,
        Self::FallbackNotArmed,
        Self::WatchdogProtectionUnavailable,
        Self::RecoveryAuthorizationRequired,
    ];

    /// Returns the stable textual identifier from SPEC-R3-001.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedContractVersion => "IO0001",
            Self::UnknownRequiredCapability => "IO0002",
            Self::InvalidRegionLayout => "IO0003",
            Self::AtomicRequirementUnavailable => "IO0004",
            Self::IdentityOrTopologyMismatch => "IO0005",
            Self::ConfigurationDigestMismatch => "IO0006",
            Self::CapacityOrBudgetExceeded => "IO0007",
            Self::DeviceAlreadyOwned => "IO0008",
            Self::StaleOrForeignLease => "IO1001",
            Self::ImageSequenceViolation => "IO1002",
            Self::ImageContended => "IO1003",
            Self::ImageExpired => "IO1004",
            Self::UpdateGroupMiss => "IO2001",
            Self::ProtocolTimeout => "IO2002",
            Self::ProtocolIntegrityFailure => "IO2003",
            Self::LinkOrDeviceLost => "IO2004",
            Self::QueueFull => "IO2005",
            Self::BackendUnavailable => "IO2006",
            Self::FallbackNotArmed => "IO3001",
            Self::WatchdogProtectionUnavailable => "IO3002",
            Self::RecoveryAuthorizationRequired => "IO3003",
        }
    }

    /// Returns the stable symbolic name from SPEC-R3-001.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::UnsupportedContractVersion => "UnsupportedContractVersion",
            Self::UnknownRequiredCapability => "UnknownRequiredCapability",
            Self::InvalidRegionLayout => "InvalidRegionLayout",
            Self::AtomicRequirementUnavailable => "AtomicRequirementUnavailable",
            Self::IdentityOrTopologyMismatch => "IdentityOrTopologyMismatch",
            Self::ConfigurationDigestMismatch => "ConfigurationDigestMismatch",
            Self::CapacityOrBudgetExceeded => "CapacityOrBudgetExceeded",
            Self::DeviceAlreadyOwned => "DeviceAlreadyOwned",
            Self::StaleOrForeignLease => "StaleOrForeignLease",
            Self::ImageSequenceViolation => "ImageSequenceViolation",
            Self::ImageContended => "ImageContended",
            Self::ImageExpired => "ImageExpired",
            Self::UpdateGroupMiss => "UpdateGroupMiss",
            Self::ProtocolTimeout => "ProtocolTimeout",
            Self::ProtocolIntegrityFailure => "ProtocolIntegrityFailure",
            Self::LinkOrDeviceLost => "LinkOrDeviceLost",
            Self::QueueFull => "QueueFull",
            Self::BackendUnavailable => "BackendUnavailable",
            Self::FallbackNotArmed => "FallbackNotArmed",
            Self::WatchdogProtectionUnavailable => "WatchdogProtectionUnavailable",
            Self::RecoveryAuthorizationRequired => "RecoveryAuthorizationRequired",
        }
    }
}

/// Rejection returned by R3-01 contract validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum GuardianContractError {
    /// A non-zero identity or generation used zero.
    #[error("identity or generation must be non-zero")]
    ZeroIdentity,
    /// A checked counter or time computation overflowed.
    #[error("counter or monotonic deadline overflowed")]
    CounterOverflow,
    /// A version range is reversed or spans more than N/N-1.
    #[error("contract or layout range is invalid")]
    InvalidVersionRange,
    /// Contract majors do not match or have no common minor.
    #[error("contract versions have no supported intersection")]
    UnsupportedContractVersion,
    /// Layout majors do not match or have no common minor.
    #[error("layout versions have no supported intersection")]
    UnsupportedLayoutVersion,
    /// A raw capability set includes a bit outside the frozen catalog.
    #[error("capability set includes an unknown entry")]
    UnknownCapability,
    /// A capability was repeated while constructing the fixed set.
    #[error("capability set repeats an entry")]
    DuplicateCapability,
    /// Capability entries were not supplied in catalog order.
    #[error("capability set is not in canonical catalog order")]
    CapabilityOutOfOrder,
    /// Required capabilities are not a subset of offered capabilities.
    #[error("required capability is unavailable")]
    RequiredCapabilityUnavailable,
    /// Heartbeat or group freshness timing is zero or inconsistent.
    #[error("lease timing policy is invalid")]
    InvalidLeaseTiming,
    /// The requested transition is not legal from the current state.
    #[error("guardian state transition is invalid")]
    InvalidStateTransition,
    /// Active Fallback has not been established.
    #[error("active fallback is not armed")]
    FallbackNotArmed,
    /// Epoch, generation, digest, or negotiated contract does not match.
    #[error("lease or configuration identity does not match")]
    ConfigurationMismatch,
    /// A lease identifier was reused within one Guardian process lifetime.
    #[error("lease identity must not be reused")]
    LeaseIdReused,
    /// The fixed process-lifetime lease history is full.
    #[error("fixed lease history capacity is exhausted")]
    LeaseCapacityExceeded,
    /// A lease is absent or does not identify the active lease.
    #[error("lease is stale or foreign")]
    StaleOrForeignLease,
    /// Output sequence was not the exact next sequence.
    #[error("output image sequence is not the exact next value")]
    ImageSequenceViolation,
    /// A monotonic observation regressed.
    #[error("monotonic time regressed")]
    MonotonicTimeRegression,
    /// The heartbeat deadline is reached and the lease is revoked.
    #[error("control heartbeat expired")]
    HeartbeatExpired,
    /// One output group reached its freshness deadline.
    #[error("output group freshness expired")]
    OutputGroupExpired,
    /// An output group index is outside the fixed group set.
    #[error("output group index is outside the fixed group set")]
    OutputGroupOutOfRange,
    /// An output image claimed no refreshed group.
    #[error("output image must refresh at least one declared group")]
    NoOutputGroupUpdated,
    /// A configuration generation skipped, repeated, or regressed.
    #[error("configuration generation must advance exactly once")]
    ConfigurationGenerationViolation,
    /// UDS peer credentials do not match the expected service identity.
    #[error("local peer identity does not match")]
    PeerIdentityMismatch,
    /// The shared-region offer is zero, misaligned, or has wrong seals.
    #[error("shared-region offer is invalid")]
    InvalidSharedRegionOffer,
}

impl GuardianContractError {
    /// Maps the detailed rejection to the stable SPEC-R3-001 catalog.
    #[must_use]
    pub const fn code(self) -> GuardianErrorCode {
        match self {
            Self::InvalidVersionRange
            | Self::UnsupportedContractVersion
            | Self::UnsupportedLayoutVersion => GuardianErrorCode::UnsupportedContractVersion,
            Self::UnknownCapability
            | Self::DuplicateCapability
            | Self::CapabilityOutOfOrder
            | Self::RequiredCapabilityUnavailable => GuardianErrorCode::UnknownRequiredCapability,
            Self::InvalidSharedRegionOffer => GuardianErrorCode::InvalidRegionLayout,
            Self::PeerIdentityMismatch => GuardianErrorCode::IdentityOrTopologyMismatch,
            Self::ConfigurationMismatch | Self::ConfigurationGenerationViolation => {
                GuardianErrorCode::ConfigurationDigestMismatch
            }
            Self::ZeroIdentity
            | Self::CounterOverflow
            | Self::InvalidLeaseTiming
            | Self::LeaseCapacityExceeded
            | Self::OutputGroupOutOfRange
            | Self::NoOutputGroupUpdated => GuardianErrorCode::CapacityOrBudgetExceeded,
            Self::LeaseIdReused | Self::StaleOrForeignLease => {
                GuardianErrorCode::StaleOrForeignLease
            }
            Self::ImageSequenceViolation => GuardianErrorCode::ImageSequenceViolation,
            Self::MonotonicTimeRegression | Self::HeartbeatExpired | Self::OutputGroupExpired => {
                GuardianErrorCode::ImageExpired
            }
            Self::InvalidStateTransition | Self::FallbackNotArmed => {
                GuardianErrorCode::FallbackNotArmed
            }
        }
    }
}
