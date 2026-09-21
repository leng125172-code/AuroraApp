//! Typed R3-02 layout, metadata, and publication failures.

use thiserror::Error;

/// Failure returned before an invalid or partial image can be observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ImageError {
    /// A required count, capacity, or allocation budget is zero or exceeded.
    #[error("image capacity is zero or exceeds its declared bound")]
    InvalidCapacity,
    /// Checked offset, size, generation, sequence, or token arithmetic overflowed.
    #[error("image arithmetic overflowed")]
    ArithmeticOverflow,
    /// The host could not reserve a bounded image buffer during initialization.
    #[error("bounded image allocation failed")]
    AllocationFailed,
    /// A fixed ABI field, magic, version, size, or identity differs.
    #[error("image header does not match the fixed ABI or expected identity")]
    HeaderMismatch,
    /// A reserved byte or flag is non-zero.
    #[error("reserved image bytes or flags must be zero")]
    ReservedNonZero,
    /// A fixed descriptor handle is missing, duplicated, or out of order.
    #[error("mapping handles must form one exact dense catalog")]
    NonDenseHandle,
    /// A stable tag identity is duplicated.
    #[error("mapping repeats a stable tag identity")]
    DuplicateTag,
    /// A mapping references an unknown source.
    #[error("mapping references an unknown source")]
    UnknownSource,
    /// A mapping references an unknown or wrong-direction group.
    #[error("mapping references an unknown update group")]
    UnknownGroup,
    /// A value range is outside its direction payload.
    #[error("value range exceeds its fixed payload")]
    ValueOutOfBounds,
    /// Two values own at least one common payload bit.
    #[error("value payload ranges overlap")]
    ValueOverlap,
    /// A scalar mapping uses an invalid bit offset, byte order, or alignment.
    #[error("scalar mapping is not canonical")]
    InvalidScalarMapping,
    /// A source, group, or value descriptor is unreferenced or multiply represented.
    #[error("mapping descriptor closure has missing or extra entries")]
    MappingClosureMismatch,
    /// UTC or monotonic timestamp fields are inconsistent.
    #[error("image timestamps are invalid")]
    InvalidTimestamp,
    /// A quality, gap, update marker, or diagnostic combination is invalid.
    #[error("image quality metadata is invalid")]
    InvalidQualityMetadata,
    /// Output validity is absent/expired, or input incorrectly declares it.
    #[error("image validity deadline is invalid")]
    InvalidValidityDeadline,
    /// The image sequence is zero, duplicated, skipped, regressed, or cannot form a token.
    #[error("image sequence is invalid")]
    ImageSequenceViolation,
    /// The supplied byte count differs from the exact slot stride.
    #[error("image slot byte count is not exact")]
    SlotSizeMismatch,
    /// No image has been published yet.
    #[error("no image has been published")]
    NoPublication,
    /// Two bounded latch attempts could not observe one stable publication.
    #[error("image remained contended after the bounded retry")]
    Contended,
    /// A producer endpoint has already been consumed or a role is incorrect.
    #[error("image endpoint ownership is invalid")]
    EndpointOwnership,
    /// The publication belongs to another direction, lease, configuration, or layout.
    #[error("image publication identity is stale or foreign")]
    StaleOrForeignIdentity,
}
