//! Fixed quality, gap, per-value, and group diagnostic records.

use crate::ImageError;

/// Compact UTC synchronization state stored in an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TimeQualityCode {
    /// UTC is unavailable.
    Unknown = 0,
    /// The clock is acquiring a source.
    Synchronizing = 1,
    /// UTC is synchronized within policy.
    Good = 2,
    /// UTC is temporarily in bounded holdover.
    Holdover = 3,
    /// UTC is present but outside normal accuracy.
    Degraded = 4,
    /// UTC must not be trusted.
    Invalid = 5,
}

impl TimeQualityCode {
    fn from_raw(value: u8) -> Result<Self, ImageError> {
        match value {
            0 => Ok(Self::Unknown),
            1 => Ok(Self::Synchronizing),
            2 => Ok(Self::Good),
            3 => Ok(Self::Holdover),
            4 => Ok(Self::Degraded),
            5 => Ok(Self::Invalid),
            _ => Err(ImageError::InvalidTimestamp),
        }
    }
}

/// Aggregate or per-value image quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AggregateQuality {
    /// Fresh and valid.
    Good = 0,
    /// Usable only with the accompanying reason.
    Uncertain = 1,
    /// Invalid for normal control use.
    Bad = 2,
    /// Retained beyond its configured freshness boundary.
    Stale = 3,
}

impl AggregateQuality {
    fn from_raw(value: u8) -> Result<Self, ImageError> {
        match value {
            0 => Ok(Self::Good),
            1 => Ok(Self::Uncertain),
            2 => Ok(Self::Bad),
            3 => Ok(Self::Stale),
            _ => Err(ImageError::InvalidQualityMetadata),
        }
    }
}

/// Exhaustive reason why a value or group is not fresh Good data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GapReason {
    /// No gap exists.
    None = 0,
    /// The source was not sampled in this publication.
    NotSampled = 1,
    /// A bounded protocol operation timed out.
    Timeout = 2,
    /// CRC, checksum, or frame integrity failed.
    Checksum = 3,
    /// `EtherCAT` working counter did not match.
    WorkingCounter = 4,
    /// Link or transport disconnected.
    LinkDown = 5,
    /// The physical device reported a fault.
    DeviceFault = 6,
    /// A fixed queue rejected or dropped work.
    QueueFull = 7,
    /// A source or image sequence skipped.
    SequenceGap = 8,
    /// A new immutable configuration invalidated retained data.
    ConfigurationChanged = 9,
    /// The selected backend is unavailable.
    BackendUnavailable = 10,
}

impl GapReason {
    fn from_raw(value: u8) -> Result<Self, ImageError> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::NotSampled),
            2 => Ok(Self::Timeout),
            3 => Ok(Self::Checksum),
            4 => Ok(Self::WorkingCounter),
            5 => Ok(Self::LinkDown),
            6 => Ok(Self::DeviceFault),
            7 => Ok(Self::QueueFull),
            8 => Ok(Self::SequenceGap),
            9 => Ok(Self::ConfigurationChanged),
            10 => Ok(Self::BackendUnavailable),
            _ => Err(ImageError::InvalidQualityMetadata),
        }
    }
}

/// Whether this publication refreshed a value or retained prior bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UpdateMarker {
    /// Payload bytes were retained and must not claim fresh Good quality.
    Retained = 0,
    /// The source refreshed the payload bytes in this image.
    Updated = 1,
}

impl UpdateMarker {
    fn from_raw(value: u8) -> Result<Self, ImageError> {
        match value {
            0 => Ok(Self::Retained),
            1 => Ok(Self::Updated),
            _ => Err(ImageError::InvalidQualityMetadata),
        }
    }
}

/// Exact eight-byte metadata record for one direction-local value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueMetadata {
    quality: AggregateQuality,
    gap_reason: GapReason,
    update_marker: UpdateMarker,
}

impl ValueMetadata {
    /// Fixed encoded byte size.
    pub const ENCODED_BYTES: usize = 8;

    /// Creates one semantically valid value record.
    ///
    /// Fresh Good values must be updated with no gap. Every retained value is explicitly Bad or
    /// Stale with a non-None gap, so retained bytes can never inherit batch Good implicitly.
    ///
    /// # Errors
    ///
    /// Rejects inconsistent quality, gap, and update combinations.
    pub const fn new(
        quality: AggregateQuality,
        gap_reason: GapReason,
        update_marker: UpdateMarker,
    ) -> Result<Self, ImageError> {
        let valid = match update_marker {
            UpdateMarker::Updated => match quality {
                AggregateQuality::Good => matches!(gap_reason, GapReason::None),
                AggregateQuality::Uncertain | AggregateQuality::Bad | AggregateQuality::Stale => {
                    !matches!(gap_reason, GapReason::None)
                }
            },
            UpdateMarker::Retained => {
                matches!(quality, AggregateQuality::Bad | AggregateQuality::Stale)
                    && !matches!(gap_reason, GapReason::None)
            }
        };
        if valid {
            Ok(Self {
                quality,
                gap_reason,
                update_marker,
            })
        } else {
            Err(ImageError::InvalidQualityMetadata)
        }
    }

    /// Returns the compact quality.
    #[must_use]
    pub const fn quality(self) -> AggregateQuality {
        self.quality
    }

    /// Returns the gap reason.
    #[must_use]
    pub const fn gap_reason(self) -> GapReason {
        self.gap_reason
    }

    /// Returns whether bytes were refreshed.
    #[must_use]
    pub const fn update_marker(self) -> UpdateMarker {
        self.update_marker
    }

    /// Encodes the exact eight-byte record with zero reserved bytes.
    #[must_use]
    pub const fn encode(self) -> [u8; Self::ENCODED_BYTES] {
        [
            self.quality as u8,
            self.gap_reason as u8,
            self.update_marker as u8,
            0,
            0,
            0,
            0,
            0,
        ]
    }

    /// Decodes and validates one exact record.
    ///
    /// # Errors
    ///
    /// Rejects unknown enum values, non-zero reserved bytes, or inconsistent semantics.
    pub fn decode(bytes: [u8; Self::ENCODED_BYTES]) -> Result<Self, ImageError> {
        if bytes[3..].iter().any(|value| *value != 0) {
            return Err(ImageError::ReservedNonZero);
        }
        Self::new(
            AggregateQuality::from_raw(bytes[0])?,
            GapReason::from_raw(bytes[1])?,
            UpdateMarker::from_raw(bytes[2])?,
        )
    }
}

/// Exact 64-byte metadata record for one source sub-batch/update group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupDiagnostics {
    group_handle: u32,
    source_handle: u32,
    source_sequence: u64,
    source_monotonic_ns: u64,
    publish_monotonic_ns: u64,
    utc_seconds: i64,
    utc_nanoseconds: u32,
    time_quality: TimeQualityCode,
    aggregate_quality: AggregateQuality,
    updated_values: u32,
    stale_values: u32,
    bad_values: u32,
}

impl GroupDiagnostics {
    /// Fixed encoded byte size.
    pub const ENCODED_BYTES: usize = 64;

    /// Creates a group record and validates its timestamps and exact value accounting.
    ///
    /// # Errors
    ///
    /// Rejects zero source sequence, invalid timestamps, counters above the group value count, or
    /// aggregate Good when any value is stale/bad/not updated.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        group_handle: u32,
        source_handle: u32,
        source_sequence: u64,
        source_monotonic_ns: u64,
        publish_monotonic_ns: u64,
        utc_seconds: i64,
        utc_nanoseconds: u32,
        time_quality: TimeQualityCode,
        aggregate_quality: AggregateQuality,
        updated_values: u32,
        stale_values: u32,
        bad_values: u32,
        group_value_count: u32,
    ) -> Result<Self, ImageError> {
        if source_sequence == 0 || publish_monotonic_ns < source_monotonic_ns {
            return Err(ImageError::InvalidTimestamp);
        }
        if utc_nanoseconds >= 1_000_000_000
            || (matches!(time_quality, TimeQualityCode::Unknown)
                && (utc_seconds != 0 || utc_nanoseconds != 0))
        {
            return Err(ImageError::InvalidTimestamp);
        }
        if updated_values > group_value_count
            || stale_values > group_value_count
            || bad_values > group_value_count
            || stale_values > bad_values
            || (matches!(aggregate_quality, AggregateQuality::Good)
                && (updated_values != group_value_count || stale_values != 0 || bad_values != 0))
        {
            return Err(ImageError::InvalidQualityMetadata);
        }
        Ok(Self {
            group_handle,
            source_handle,
            source_sequence,
            source_monotonic_ns,
            publish_monotonic_ns,
            utc_seconds,
            utc_nanoseconds,
            time_quality,
            aggregate_quality,
            updated_values,
            stale_values,
            bad_values,
        })
    }

    /// Returns the group handle encoded in this record.
    #[must_use]
    pub const fn group_handle(self) -> u32 {
        self.group_handle
    }

    /// Returns the source handle encoded in this record.
    #[must_use]
    pub const fn source_handle(self) -> u32 {
        self.source_handle
    }

    /// Returns the source sequence.
    #[must_use]
    pub const fn source_sequence(self) -> u64 {
        self.source_sequence
    }

    /// Returns the source monotonic timestamp.
    #[must_use]
    pub const fn source_monotonic_ns(self) -> u64 {
        self.source_monotonic_ns
    }

    /// Returns the publish monotonic timestamp.
    #[must_use]
    pub const fn publish_monotonic_ns(self) -> u64 {
        self.publish_monotonic_ns
    }

    /// Returns the UTC seconds field.
    #[must_use]
    pub const fn utc_seconds(self) -> i64 {
        self.utc_seconds
    }

    /// Returns the normalized UTC nanoseconds field.
    #[must_use]
    pub const fn utc_nanoseconds(self) -> u32 {
        self.utc_nanoseconds
    }

    /// Returns UTC time quality.
    #[must_use]
    pub const fn time_quality(self) -> TimeQualityCode {
        self.time_quality
    }

    /// Returns aggregate group quality.
    #[must_use]
    pub const fn aggregate_quality(self) -> AggregateQuality {
        self.aggregate_quality
    }

    /// Returns the number of values refreshed in this publication.
    #[must_use]
    pub const fn updated_values(self) -> u32 {
        self.updated_values
    }

    /// Returns the number of explicitly stale values.
    #[must_use]
    pub const fn stale_values(self) -> u32 {
        self.stale_values
    }

    /// Returns the number of Bad values, including the stale subset.
    #[must_use]
    pub const fn bad_values(self) -> u32 {
        self.bad_values
    }

    /// Encodes the exact 64-byte record.
    #[must_use]
    pub fn encode(self) -> [u8; Self::ENCODED_BYTES] {
        let mut bytes = [0; Self::ENCODED_BYTES];
        put_u32(&mut bytes, 0, self.group_handle);
        put_u32(&mut bytes, 4, self.source_handle);
        put_u64(&mut bytes, 8, self.source_sequence);
        put_u64(&mut bytes, 16, self.source_monotonic_ns);
        put_u64(&mut bytes, 24, self.publish_monotonic_ns);
        put_i64(&mut bytes, 32, self.utc_seconds);
        put_u32(&mut bytes, 40, self.utc_nanoseconds);
        bytes[44] = self.time_quality as u8;
        bytes[45] = self.aggregate_quality as u8;
        put_u32(&mut bytes, 48, self.updated_values);
        put_u32(&mut bytes, 52, self.stale_values);
        put_u32(&mut bytes, 56, self.bad_values);
        bytes
    }

    /// Decodes one record against its exact group value count.
    ///
    /// # Errors
    ///
    /// Rejects unknown fields, non-zero flags/reserved bytes, or inconsistent counters.
    pub fn decode(
        bytes: [u8; Self::ENCODED_BYTES],
        group_value_count: u32,
    ) -> Result<Self, ImageError> {
        if bytes[46] != 0 || bytes[47] != 0 || bytes[60..].iter().any(|value| *value != 0) {
            return Err(ImageError::ReservedNonZero);
        }
        Self::new(
            get_u32(&bytes, 0),
            get_u32(&bytes, 4),
            get_u64(&bytes, 8),
            get_u64(&bytes, 16),
            get_u64(&bytes, 24),
            get_i64(&bytes, 32),
            get_u32(&bytes, 40),
            TimeQualityCode::from_raw(bytes[44])?,
            AggregateQuality::from_raw(bytes[45])?,
            get_u32(&bytes, 48),
            get_u32(&bytes, 52),
            get_u32(&bytes, 56),
            group_value_count,
        )
    }
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_i64(bytes: &mut [u8], offset: usize, value: i64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn get_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}
