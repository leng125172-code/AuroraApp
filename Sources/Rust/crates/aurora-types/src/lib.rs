//! Foundational domain types shared by Aurora contracts and runtime components.

mod capability;
mod error;
mod identifier;
mod quality;
mod time;
mod version;

pub use capability::{CapabilityId, CapabilityIdError};
pub use error::{ErrorCode, ErrorCodeError, RetryClass};
pub use identifier::{
    BootEpochId, DeploymentId, DeviceId, DocumentId, IdentifierError, LocalHandle, OperationId,
    ProjectId, RequestId, TagId,
};
pub use quality::{QualityCode, QualityCodeError, QualityFlags, QualitySeverity};
pub use time::{
    DurationNanos, MonotonicTimestamp, TimeQuality, TimeQualityState, TimeSource, UtcTimestamp,
    UtcTimestampError,
};
pub use version::{ContractLifecycle, ContractVersion, SemanticVersion, VersionError};
