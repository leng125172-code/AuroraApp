//! Bounded I/O image ABI, mapping validation, quality metadata, and atomic publication.
//!
//! R3-02 implements the portable shared-image core and R3-03 adds immutable update-group plans,
//! fixed queues, and absolute-grid scheduling without device or protocol access. The Linux-only
//! sealed-memfd adapter preserves the same exact bytes, identities, capacities, and Acquire/Release
//! operations; UDS session orchestration and real protocol recovery remain outside this crate.

mod channel;
mod error;
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
