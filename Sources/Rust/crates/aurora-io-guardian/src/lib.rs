//! Bounded I/O image ABI, mapping validation, quality metadata, and atomic publication.
//!
//! R3-02 implements the portable shared-image core, R3-03 adds immutable update-group plans,
//! fixed queues, and absolute-grid scheduling, and R3-04 adds exact Fallback domains, staged
//! activation, bounded recovery, and a deterministic watchdog model. The Linux-only sealed-memfd
//! adapter preserves the same exact bytes, identities, capacities, and Acquire/Release operations.
//! Device access, Driver Host/session orchestration, persistent authorization, and real protocol
//! recovery remain outside this crate.

mod channel;
mod error;
mod fallback;
mod group_queue;
mod header;
mod layout;
#[cfg(target_os = "linux")]
mod linux;
mod mapping;
mod metadata;
mod region;
mod update_group;

pub use channel::{ImageConsumer, ImageProducer, LatchObservation, PreparedImage};
pub use error::{ImageError, LinuxOperation};
pub use fallback::{
    DeviceWatchdogSimulator, DeviceWatchdogState, DomainRecoveryPolicy, EvidenceDigest,
    FallbackAction, FallbackCause, FallbackController, FallbackDependency, FallbackDigest,
    FallbackDomainDiagnostics, FallbackDomainHandle, FallbackDomainRisk, FallbackDomainSpec,
    FallbackDomainState, FallbackEffect, FallbackError, FallbackHealthPolicy, FallbackLimits,
    FallbackPlan, FallbackValue, FallbackValueRange, FallbackVersion, HealthCheckResult,
    OutputFallbackSpec, PendingFallbackHealth, ProtectionEvidence, RecoveryAuthorization,
    RecoveryProgress, ReinitializationEvidence,
};
pub use group_queue::{BoundedGroupQueue, GroupQueueSnapshot, QueueAdmission};
pub use header::{
    CapabilityDigest, IMAGE_SLOT_HEADER_BYTES, ImageSlotHeader, REGION_HEADER_BYTES, RegionHeader,
};
pub use layout::{
    GROUP_DIAGNOSTIC_BYTES, IMAGE_ALIGNMENT, ImageDirection, ImageLayout, SlotLayout,
    VALUE_METADATA_BYTES,
};
#[cfg(target_os = "linux")]
pub use linux::{LinuxControlMappedRegion, LinuxGuardianMappedRegion};
pub use mapping::{
    BitOrder, ByteOrder, GroupDescriptor, GroupHandle, ImageMapping, ProtectionLevel,
    ProtocolSourceKind, ScalarType, SourceDescriptor, SourceHandle, ValueBinding,
};
pub use metadata::{
    AggregateQuality, GapReason, GroupDiagnostics, TimeQualityCode, UpdateMarker, ValueMetadata,
};
pub use region::{ControlImageEndpoint, GuardianImageEndpoint, SharedIoRegion};
pub use update_group::{
    GroupHealth, GroupMissPolicy, GroupReleaseOutcome, GroupReleaseTicket, InterfaceHandle,
    OperationClass, OperationHandle, OperationObservation, OperationResult, OutputRefreshStatus,
    RetryProofDigest, ScheduledOperation, UpdateGroupDiagnostics, UpdateGroupError,
    UpdateGroupLimits, UpdateGroupPlan, UpdateGroupScheduler, UpdateGroupSpec,
};
