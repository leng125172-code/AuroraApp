//! Exact little-endian region and image-slot header codecs.

use aurora_io_guardian_contracts::{
    ConfigurationDigest, ConfigurationGeneration, GuardianConfiguration, GuardianEpoch,
    ImageSequence, LayoutDigest, LeaseId, LeaseIdentity, LeaseSequence,
};

use crate::{AggregateQuality, ImageDirection, ImageError, ImageLayout, TimeQualityCode};

/// Fixed region-header byte count and alignment.
pub const REGION_HEADER_BYTES: usize = 256;
const REGION_HEADER_BYTES_U32: u32 = 256;
/// Fixed image-slot-header byte count and alignment.
pub const IMAGE_SLOT_HEADER_BYTES: usize = 128;
const REGION_MAGIC: [u8; 8] = *b"AURIO001";
const LAYOUT_MAJOR: u16 = 1;
const LAYOUT_MINOR: u16 = 0;

/// SHA-256 digest of the exact sorted capability catalog selected for this mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapabilityDigest([u8; 32]);

impl CapabilityDigest {
    /// Creates a digest from all SHA-256 bytes.
    #[must_use]
    pub const fn from_sha256(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns all SHA-256 bytes.
    #[must_use]
    pub const fn to_sha256(self) -> [u8; 32] {
        self.0
    }
}

/// Immutable mapping identity plus current atomic header counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegionHeader {
    layout: ImageLayout,
    lease_identity: LeaseIdentity,
    lease_sequence: LeaseSequence,
    capability_digest: CapabilityDigest,
    input_publish_token: u64,
    output_publish_token: u64,
    input_drop_count: u64,
    output_reject_count: u64,
}

impl RegionHeader {
    /// Creates a zeroed, unpublished region header for one new lease mapping.
    #[must_use]
    pub const fn new(
        layout: ImageLayout,
        lease_identity: LeaseIdentity,
        lease_sequence: LeaseSequence,
        capability_digest: CapabilityDigest,
    ) -> Self {
        Self {
            layout,
            lease_identity,
            lease_sequence,
            capability_digest,
            input_publish_token: 0,
            output_publish_token: 0,
            input_drop_count: 0,
            output_reject_count: 0,
        }
    }

    /// Returns the exact checked region layout.
    #[must_use]
    pub const fn layout(self) -> ImageLayout {
        self.layout
    }

    /// Returns the immutable lease/configuration/layout identity.
    #[must_use]
    pub const fn lease_identity(self) -> LeaseIdentity {
        self.lease_identity
    }

    /// Returns the process-local lease sequence.
    #[must_use]
    pub const fn lease_sequence(self) -> LeaseSequence {
        self.lease_sequence
    }

    /// Returns the sorted capability-catalog digest.
    #[must_use]
    pub const fn capability_digest(self) -> CapabilityDigest {
        self.capability_digest
    }

    /// Returns the input publication token snapshot.
    #[must_use]
    pub const fn input_publish_token(self) -> u64 {
        self.input_publish_token
    }

    /// Returns the output publication token snapshot.
    #[must_use]
    pub const fn output_publish_token(self) -> u64 {
        self.output_publish_token
    }

    /// Returns the saturated input drop count snapshot.
    #[must_use]
    pub const fn input_drop_count(self) -> u64 {
        self.input_drop_count
    }

    /// Returns the saturated output reject count snapshot.
    #[must_use]
    pub const fn output_reject_count(self) -> u64 {
        self.output_reject_count
    }

    pub(crate) const fn with_runtime_counters(
        mut self,
        input_publish_token: u64,
        output_publish_token: u64,
        input_drop_count: u64,
        output_reject_count: u64,
    ) -> Self {
        self.input_publish_token = input_publish_token;
        self.output_publish_token = output_publish_token;
        self.input_drop_count = input_drop_count;
        self.output_reject_count = output_reject_count;
        self
    }

    /// Encodes the exact 256-byte little-endian header.
    #[must_use]
    pub fn encode(self) -> [u8; REGION_HEADER_BYTES] {
        let mut bytes = [0; REGION_HEADER_BYTES];
        bytes[0..8].copy_from_slice(&REGION_MAGIC);
        put_u16(&mut bytes, 8, LAYOUT_MAJOR);
        put_u16(&mut bytes, 10, LAYOUT_MINOR);
        put_u32(&mut bytes, 12, REGION_HEADER_BYTES_U32);
        put_u64(&mut bytes, 16, self.layout.total_bytes());
        let configuration = self.lease_identity.configuration();
        put_u64(&mut bytes, 24, configuration.epoch().get());
        bytes[32..64].copy_from_slice(&configuration.configuration_digest().to_sha256());
        put_u64(&mut bytes, 64, self.layout.input_offset());
        put_u32(&mut bytes, 72, self.layout.input().stride_bytes());
        put_u32(&mut bytes, 76, self.layout.input().payload_capacity_bytes());
        put_u64(&mut bytes, 80, self.layout.output_offset());
        put_u32(&mut bytes, 88, self.layout.output().stride_bytes());
        put_u32(
            &mut bytes,
            92,
            self.layout.output().payload_capacity_bytes(),
        );
        put_u32(&mut bytes, 96, self.layout.value_count());
        put_u16(&mut bytes, 100, self.layout.input().group_count());
        put_u16(&mut bytes, 102, self.layout.output().group_count());
        bytes[112..144].copy_from_slice(&self.capability_digest.to_sha256());
        bytes[144..160].copy_from_slice(&self.lease_identity.lease_id().to_bytes());
        put_u64(&mut bytes, 160, self.input_publish_token);
        put_u64(&mut bytes, 168, self.output_publish_token);
        put_u64(&mut bytes, 176, self.input_drop_count);
        put_u64(&mut bytes, 184, self.output_reject_count);
        put_u64(&mut bytes, 192, configuration.generation().get());
        put_u64(&mut bytes, 200, self.lease_sequence.get());
        bytes[208..240].copy_from_slice(&configuration.layout_digest().to_sha256());
        put_u32(&mut bytes, 240, self.layout.input().value_count());
        put_u32(&mut bytes, 244, self.layout.output().value_count());
        bytes
    }

    /// Decodes and validates the exact header, including recomputed offsets and sizes.
    ///
    /// # Errors
    ///
    /// Rejects ABI drift, unknown flags, non-zero reserved bytes, invalid identities, inconsistent
    /// counts, or any layout that cannot be recomputed exactly from its declared capacities.
    pub fn decode(bytes: [u8; REGION_HEADER_BYTES]) -> Result<Self, ImageError> {
        if bytes[0..8] != REGION_MAGIC
            || get_u16(&bytes, 8) != LAYOUT_MAJOR
            || get_u16(&bytes, 10) != LAYOUT_MINOR
            || get_u32(&bytes, 12) != REGION_HEADER_BYTES_U32
        {
            return Err(ImageError::HeaderMismatch);
        }
        if get_u64(&bytes, 104) != 0 || bytes[248..].iter().any(|value| *value != 0) {
            return Err(ImageError::ReservedNonZero);
        }
        let total_bytes = get_u64(&bytes, 16);
        let input_value_count = get_u32(&bytes, 240);
        let output_value_count = get_u32(&bytes, 244);
        let layout = ImageLayout::new(
            get_u32(&bytes, 76),
            get_u32(&bytes, 92),
            input_value_count,
            output_value_count,
            get_u16(&bytes, 100),
            get_u16(&bytes, 102),
            total_bytes,
        )?;
        if layout.total_bytes() != total_bytes
            || layout.input_offset() != get_u64(&bytes, 64)
            || layout.input().stride_bytes() != get_u32(&bytes, 72)
            || layout.output_offset() != get_u64(&bytes, 80)
            || layout.output().stride_bytes() != get_u32(&bytes, 88)
            || layout.value_count() != get_u32(&bytes, 96)
        {
            return Err(ImageError::HeaderMismatch);
        }
        let epoch =
            GuardianEpoch::new(get_u64(&bytes, 24)).map_err(|_| ImageError::HeaderMismatch)?;
        let generation = ConfigurationGeneration::new(get_u64(&bytes, 192))
            .map_err(|_| ImageError::HeaderMismatch)?;
        let configuration_digest = ConfigurationDigest::from_sha256(copy_32(&bytes, 32));
        let layout_digest = LayoutDigest::from_sha256(copy_32(&bytes, 208));
        let lease_id =
            LeaseId::new(copy_16(&bytes, 144)).map_err(|_| ImageError::HeaderMismatch)?;
        let lease_sequence =
            LeaseSequence::new(get_u64(&bytes, 200)).map_err(|_| ImageError::HeaderMismatch)?;
        let input_publish_token = get_u64(&bytes, 160);
        let output_publish_token = get_u64(&bytes, 168);
        validate_publish_token(input_publish_token)?;
        validate_publish_token(output_publish_token)?;
        Ok(Self {
            layout,
            lease_identity: LeaseIdentity::new(
                GuardianConfiguration::new(epoch, generation, configuration_digest, layout_digest),
                lease_id,
            ),
            lease_sequence,
            capability_digest: CapabilityDigest::from_sha256(copy_32(&bytes, 112)),
            input_publish_token,
            output_publish_token,
            input_drop_count: get_u64(&bytes, 176),
            output_reject_count: get_u64(&bytes, 184),
        })
    }
}

/// Exact typed content of one 128-byte slot header, excluding mutable payload bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageSlotHeader {
    generation: u64,
    guardian_epoch: GuardianEpoch,
    image_sequence: ImageSequence,
    source_monotonic_ns: u64,
    publish_monotonic_ns: u64,
    utc_seconds: i64,
    utc_nanoseconds: u32,
    time_quality: TimeQualityCode,
    aggregate_quality: AggregateQuality,
    value_count: u32,
    payload_bytes: u32,
    layout_digest: LayoutDigest,
    dropped_before: u64,
    diagnostics_offset: u32,
    diagnostics_bytes: u32,
    valid_until_monotonic_ns: u64,
    lease_sequence: LeaseSequence,
}

impl ImageSlotHeader {
    /// Creates one header bound to a region identity and direction layout.
    ///
    /// # Errors
    ///
    /// Rejects odd generation, timestamp inversion, non-canonical UTC, incorrect output validity,
    /// or a zero/expired validity deadline.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        region: RegionHeader,
        direction: ImageDirection,
        generation: u64,
        image_sequence: ImageSequence,
        source_monotonic_ns: u64,
        publish_monotonic_ns: u64,
        utc_seconds: i64,
        utc_nanoseconds: u32,
        time_quality: TimeQualityCode,
        aggregate_quality: AggregateQuality,
        dropped_before: u64,
        valid_until_monotonic_ns: u64,
    ) -> Result<Self, ImageError> {
        if generation & 1 != 0 {
            return Err(ImageError::HeaderMismatch);
        }
        if publish_monotonic_ns < source_monotonic_ns
            || utc_nanoseconds >= 1_000_000_000
            || (matches!(time_quality, TimeQualityCode::Unknown)
                && (utc_seconds != 0 || utc_nanoseconds != 0))
        {
            return Err(ImageError::InvalidTimestamp);
        }
        match direction {
            ImageDirection::Input if valid_until_monotonic_ns != 0 => {
                return Err(ImageError::InvalidValidityDeadline);
            }
            ImageDirection::Output if valid_until_monotonic_ns <= publish_monotonic_ns => {
                return Err(ImageError::InvalidValidityDeadline);
            }
            ImageDirection::Input | ImageDirection::Output => {}
        }
        let slot = region.layout.slot(direction);
        Ok(Self {
            generation,
            guardian_epoch: region.lease_identity.configuration().epoch(),
            image_sequence,
            source_monotonic_ns,
            publish_monotonic_ns,
            utc_seconds,
            utc_nanoseconds,
            time_quality,
            aggregate_quality,
            value_count: slot.value_count(),
            payload_bytes: slot.payload_capacity_bytes(),
            layout_digest: region.lease_identity.configuration().layout_digest(),
            dropped_before,
            diagnostics_offset: slot.diagnostics_offset(),
            diagnostics_bytes: slot.diagnostics_bytes(),
            valid_until_monotonic_ns,
            lease_sequence: region.lease_sequence,
        })
    }

    /// Returns the current stable slot generation.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns the image sequence.
    #[must_use]
    pub const fn image_sequence(self) -> ImageSequence {
        self.image_sequence
    }

    /// Returns the source monotonic timestamp.
    #[must_use]
    pub const fn source_monotonic_ns(self) -> u64 {
        self.source_monotonic_ns
    }

    /// Returns the completed-publication monotonic timestamp.
    #[must_use]
    pub const fn publish_monotonic_ns(self) -> u64 {
        self.publish_monotonic_ns
    }

    /// Returns the image aggregate quality.
    #[must_use]
    pub const fn aggregate_quality(self) -> AggregateQuality {
        self.aggregate_quality
    }

    /// Returns the direction-local value count.
    #[must_use]
    pub const fn value_count(self) -> u32 {
        self.value_count
    }

    /// Returns the exact fixed payload bytes.
    #[must_use]
    pub const fn payload_bytes(self) -> u32 {
        self.payload_bytes
    }

    /// Returns the cumulative pre-publication drop count.
    #[must_use]
    pub const fn dropped_before(self) -> u64 {
        self.dropped_before
    }

    /// Returns the output validity deadline, or zero for input.
    #[must_use]
    pub const fn valid_until_monotonic_ns(self) -> u64 {
        self.valid_until_monotonic_ns
    }

    /// Returns the lease sequence encoded in the slot.
    #[must_use]
    pub const fn lease_sequence(self) -> LeaseSequence {
        self.lease_sequence
    }

    /// Encodes the exact 128-byte little-endian slot header.
    #[must_use]
    pub fn encode(self) -> [u8; IMAGE_SLOT_HEADER_BYTES] {
        let mut bytes = [0; IMAGE_SLOT_HEADER_BYTES];
        put_u64(&mut bytes, 0, self.generation);
        put_u64(&mut bytes, 8, self.guardian_epoch.get());
        put_u64(&mut bytes, 16, self.image_sequence.get());
        put_u64(&mut bytes, 24, self.source_monotonic_ns);
        put_u64(&mut bytes, 32, self.publish_monotonic_ns);
        put_i64(&mut bytes, 40, self.utc_seconds);
        put_u32(&mut bytes, 48, self.utc_nanoseconds);
        bytes[52] = self.time_quality as u8;
        bytes[53] = self.aggregate_quality as u8;
        put_u32(&mut bytes, 56, self.value_count);
        put_u32(&mut bytes, 60, self.payload_bytes);
        bytes[64..96].copy_from_slice(&self.layout_digest.to_sha256());
        put_u64(&mut bytes, 96, self.dropped_before);
        put_u32(&mut bytes, 104, self.diagnostics_offset);
        put_u32(&mut bytes, 108, self.diagnostics_bytes);
        put_u64(&mut bytes, 112, self.valid_until_monotonic_ns);
        put_u64(&mut bytes, 120, self.lease_sequence.get());
        bytes
    }

    /// Decodes one header against the exact region and direction contract.
    ///
    /// # Errors
    ///
    /// Rejects odd generation, identity/layout/count drift, unknown flags/enums, timestamp errors,
    /// or validity semantics before any payload is accepted.
    pub fn decode(
        bytes: [u8; IMAGE_SLOT_HEADER_BYTES],
        region: RegionHeader,
        direction: ImageDirection,
    ) -> Result<Self, ImageError> {
        if get_u16(&bytes, 54) != 0 {
            return Err(ImageError::ReservedNonZero);
        }
        let epoch =
            GuardianEpoch::new(get_u64(&bytes, 8)).map_err(|_| ImageError::HeaderMismatch)?;
        let sequence = ImageSequence::new(get_u64(&bytes, 16))
            .map_err(|_| ImageError::ImageSequenceViolation)?;
        let time_quality = decode_time_quality(bytes[52])?;
        let aggregate_quality = decode_aggregate_quality(bytes[53])?;
        let decoded = Self {
            generation: get_u64(&bytes, 0),
            guardian_epoch: epoch,
            image_sequence: sequence,
            source_monotonic_ns: get_u64(&bytes, 24),
            publish_monotonic_ns: get_u64(&bytes, 32),
            utc_seconds: get_i64(&bytes, 40),
            utc_nanoseconds: get_u32(&bytes, 48),
            time_quality,
            aggregate_quality,
            value_count: get_u32(&bytes, 56),
            payload_bytes: get_u32(&bytes, 60),
            layout_digest: LayoutDigest::from_sha256(copy_32(&bytes, 64)),
            dropped_before: get_u64(&bytes, 96),
            diagnostics_offset: get_u32(&bytes, 104),
            diagnostics_bytes: get_u32(&bytes, 108),
            valid_until_monotonic_ns: get_u64(&bytes, 112),
            lease_sequence: LeaseSequence::new(get_u64(&bytes, 120))
                .map_err(|_| ImageError::HeaderMismatch)?,
        };
        let expected_configuration = region.lease_identity.configuration();
        let slot = region.layout.slot(direction);
        if decoded.generation & 1 != 0
            || decoded.guardian_epoch != expected_configuration.epoch()
            || decoded.layout_digest != expected_configuration.layout_digest()
            || decoded.lease_sequence != region.lease_sequence
            || decoded.value_count != slot.value_count()
            || decoded.payload_bytes != slot.payload_capacity_bytes()
            || decoded.diagnostics_offset != slot.diagnostics_offset()
            || decoded.diagnostics_bytes != slot.diagnostics_bytes()
        {
            return Err(ImageError::StaleOrForeignIdentity);
        }
        if decoded.publish_monotonic_ns < decoded.source_monotonic_ns
            || decoded.utc_nanoseconds >= 1_000_000_000
            || (matches!(decoded.time_quality, TimeQualityCode::Unknown)
                && (decoded.utc_seconds != 0 || decoded.utc_nanoseconds != 0))
        {
            return Err(ImageError::InvalidTimestamp);
        }
        match direction {
            ImageDirection::Input if decoded.valid_until_monotonic_ns != 0 => {
                Err(ImageError::InvalidValidityDeadline)
            }
            ImageDirection::Output
                if decoded.valid_until_monotonic_ns <= decoded.publish_monotonic_ns =>
            {
                Err(ImageError::InvalidValidityDeadline)
            }
            ImageDirection::Input | ImageDirection::Output => Ok(decoded),
        }
    }
}

fn validate_publish_token(token: u64) -> Result<(), ImageError> {
    if token == 0 {
        return Ok(());
    }
    ImageSequence::new(token >> 1)
        .map(|_| ())
        .map_err(|_| ImageError::ImageSequenceViolation)
}

fn decode_time_quality(value: u8) -> Result<TimeQualityCode, ImageError> {
    match value {
        0 => Ok(TimeQualityCode::Unknown),
        1 => Ok(TimeQualityCode::Synchronizing),
        2 => Ok(TimeQualityCode::Good),
        3 => Ok(TimeQualityCode::Holdover),
        4 => Ok(TimeQualityCode::Degraded),
        5 => Ok(TimeQualityCode::Invalid),
        _ => Err(ImageError::InvalidTimestamp),
    }
}

fn decode_aggregate_quality(value: u8) -> Result<AggregateQuality, ImageError> {
    match value {
        0 => Ok(AggregateQuality::Good),
        1 => Ok(AggregateQuality::Uncertain),
        2 => Ok(AggregateQuality::Bad),
        3 => Ok(AggregateQuality::Stale),
        _ => Err(ImageError::InvalidQualityMetadata),
    }
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
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

fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
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

fn copy_16(bytes: &[u8], offset: usize) -> [u8; 16] {
    let mut result = [0; 16];
    result.copy_from_slice(&bytes[offset..offset + 16]);
    result
}

fn copy_32(bytes: &[u8], offset: usize) -> [u8; 32] {
    let mut result = [0; 32];
    result.copy_from_slice(&bytes[offset..offset + 32]);
    result
}
