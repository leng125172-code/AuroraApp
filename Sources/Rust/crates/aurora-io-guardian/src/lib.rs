//! Bounded I/O image ABI, mapping validation, quality metadata, and atomic publication.
//!
//! R3-02 implements the portable shared-image core without device or protocol access. The Linux
//! sealed-memfd adapter remains outside this safe Rust crate and must preserve these exact bytes,
//! identities, capacities, and Acquire/Release operations.

mod channel;
mod error;
mod header;
mod layout;
#[cfg(target_os = "linux")]
mod linux;
mod mapping;
mod metadata;
mod region;

pub use channel::{ImageConsumer, ImageProducer, LatchObservation, PreparedImage};
pub use error::{ImageError, LinuxOperation};
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
