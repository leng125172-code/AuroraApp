//! Safe bounded SPSC double buffer with Acquire/Release publication.

use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicU64, Ordering},
};

use crate::{
    AggregateQuality, GroupDiagnostics, IMAGE_SLOT_HEADER_BYTES, ImageDirection, ImageError,
    ImageMapping, ImageSlotHeader, RegionHeader, SlotLayout, ValueMetadata,
};

struct AtomicSlot {
    generation: AtomicU64,
    bytes: Box<[AtomicU8]>,
}

impl AtomicSlot {
    fn new(byte_count: usize) -> Result<Self, ImageError> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_count)
            .map_err(|_| ImageError::AllocationFailed)?;
        for _ in 0..byte_count {
            bytes.push(AtomicU8::new(0));
        }
        Ok(Self {
            generation: AtomicU64::new(0),
            bytes: bytes.into_boxed_slice(),
        })
    }
}

pub(crate) struct SharedImageChannel {
    slots: [AtomicSlot; 2],
    publish_token: AtomicU64,
    loss_or_reject_count: AtomicU64,
}

impl SharedImageChannel {
    fn new(byte_count: usize) -> Result<Self, ImageError> {
        if byte_count < IMAGE_SLOT_HEADER_BYTES {
            return Err(ImageError::InvalidCapacity);
        }
        Ok(Self {
            slots: [AtomicSlot::new(byte_count)?, AtomicSlot::new(byte_count)?],
            publish_token: AtomicU64::new(0),
            loss_or_reject_count: AtomicU64::new(0),
        })
    }

    pub(crate) fn publish_token(&self) -> u64 {
        self.publish_token.load(Ordering::Acquire)
    }

    pub(crate) fn loss_or_reject_count(&self) -> u64 {
        self.loss_or_reject_count.load(Ordering::Acquire)
    }

    fn increment_loss_or_reject(&self) {
        let mut current = self.loss_or_reject_count.load(Ordering::Relaxed);
        loop {
            if current == u64::MAX {
                return;
            }
            match self.loss_or_reject_count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }
}

#[derive(Clone)]
pub(crate) enum ChannelBacking {
    Local(Arc<SharedImageChannel>),
    #[cfg(target_os = "linux")]
    Mapped(Arc<crate::linux::MappedImageChannel>),
}

impl ChannelBacking {
    fn publish_token(&self) -> u64 {
        match self {
            Self::Local(channel) => channel.publish_token.load(Ordering::Acquire),
            #[cfg(target_os = "linux")]
            Self::Mapped(channel) => channel.publish_token(),
        }
    }

    fn store_publish_token(&self, token: u64) {
        match self {
            Self::Local(channel) => channel.publish_token.store(token, Ordering::Release),
            #[cfg(target_os = "linux")]
            Self::Mapped(channel) => channel.store_publish_token(token),
        }
    }

    fn loss_or_reject_count(&self) -> u64 {
        match self {
            Self::Local(channel) => channel.loss_or_reject_count(),
            #[cfg(target_os = "linux")]
            Self::Mapped(channel) => channel.loss_or_reject_count(),
        }
    }

    fn increment_loss_or_reject(&self) {
        match self {
            Self::Local(channel) => channel.increment_loss_or_reject(),
            #[cfg(target_os = "linux")]
            Self::Mapped(channel) => channel.increment_loss_or_reject(),
        }
    }

    fn generation(&self, slot_index: usize) -> u64 {
        match self {
            Self::Local(channel) => channel.slots[slot_index].generation.load(Ordering::Acquire),
            #[cfg(target_os = "linux")]
            Self::Mapped(channel) => channel.generation(slot_index),
        }
    }

    fn store_generation(&self, slot_index: usize, generation: u64) {
        match self {
            Self::Local(channel) => channel.slots[slot_index]
                .generation
                .store(generation, Ordering::Release),
            #[cfg(target_os = "linux")]
            Self::Mapped(channel) => channel.store_generation(slot_index, generation),
        }
    }

    fn write_slot(&self, slot_index: usize, bytes: &[u8]) {
        match self {
            Self::Local(channel) => {
                for (target, source) in channel.slots[slot_index]
                    .bytes
                    .iter()
                    .zip(bytes.iter())
                    .skip(8)
                {
                    target.store(*source, Ordering::Relaxed);
                }
            }
            #[cfg(target_os = "linux")]
            Self::Mapped(channel) => channel.write_slot(slot_index, bytes),
        }
    }

    fn read_slot(&self, slot_index: usize, bytes: &mut [u8]) {
        match self {
            Self::Local(channel) => {
                for (target, source) in bytes
                    .iter_mut()
                    .zip(channel.slots[slot_index].bytes.iter())
                    .skip(8)
                {
                    *target = source.load(Ordering::Relaxed);
                }
            }
            #[cfg(target_os = "linux")]
            Self::Mapped(channel) => channel.read_slot(slot_index, bytes),
        }
    }
}

/// Borrowed, prevalidated complete image ready for one bounded publication attempt.
#[derive(Debug, Clone, Copy)]
pub struct PreparedImage<'a> {
    region: RegionHeader,
    direction: ImageDirection,
    header: ImageSlotHeader,
    bytes: &'a [u8],
}

impl<'a> PreparedImage<'a> {
    /// Validates exact slot size, header identity, every value metadata record, every group record,
    /// and all zero padding without allocating.
    ///
    /// # Errors
    ///
    /// Rejects missing/extra bytes, stale identity, malformed quality, mismatched source/group
    /// closure, non-zero padding, or aggregate Good that hides a non-Good value/group.
    pub fn new(
        region: RegionHeader,
        direction: ImageDirection,
        mapping: ImageMapping<'_>,
        bytes: &'a [u8],
    ) -> Result<Self, ImageError> {
        let header = validate_image_bytes(&region, direction, mapping, bytes, true)?;
        Ok(Self {
            region,
            direction,
            header,
            bytes,
        })
    }

    /// Returns the validated slot header.
    #[must_use]
    pub const fn header(self) -> ImageSlotHeader {
        self.header
    }
}

fn validate_image_bytes(
    region: &RegionHeader,
    direction: ImageDirection,
    mapping: ImageMapping<'_>,
    bytes: &[u8],
    require_zero_generation: bool,
) -> Result<ImageSlotHeader, ImageError> {
    if mapping.layout() != region.layout()
        || mapping.layout_digest() != region.lease_identity().configuration().layout_digest()
        || mapping.capability_digest() != region.capability_digest()
    {
        return Err(ImageError::StaleOrForeignIdentity);
    }
    let slot = region.layout().slot(direction);
    let expected_bytes =
        usize::try_from(slot.stride_bytes()).map_err(|_| ImageError::InvalidCapacity)?;
    if bytes.len() != expected_bytes {
        return Err(ImageError::SlotSizeMismatch);
    }
    let mut header_bytes = [0; IMAGE_SLOT_HEADER_BYTES];
    header_bytes.copy_from_slice(&bytes[..IMAGE_SLOT_HEADER_BYTES]);
    let header = ImageSlotHeader::decode(header_bytes, *region, direction)?;
    if require_zero_generation && header.generation() != 0 {
        return Err(ImageError::HeaderMismatch);
    }
    let payload_end = IMAGE_SLOT_HEADER_BYTES
        .checked_add(
            usize::try_from(slot.payload_capacity_bytes())
                .map_err(|_| ImageError::InvalidCapacity)?,
        )
        .ok_or(ImageError::ArithmeticOverflow)?;
    let metadata_offset =
        usize::try_from(slot.metadata_offset()).map_err(|_| ImageError::InvalidCapacity)?;
    if bytes[payload_end..metadata_offset]
        .iter()
        .any(|value| *value != 0)
    {
        return Err(ImageError::ReservedNonZero);
    }
    let all_values_good = validate_value_metadata(slot, mapping, direction, bytes)?;
    let metadata_end = metadata_offset
        .checked_add(
            usize::try_from(slot.metadata_bytes()).map_err(|_| ImageError::InvalidCapacity)?,
        )
        .ok_or(ImageError::ArithmeticOverflow)?;
    let diagnostics_offset =
        usize::try_from(slot.diagnostics_offset()).map_err(|_| ImageError::InvalidCapacity)?;
    if bytes[metadata_end..diagnostics_offset]
        .iter()
        .any(|value| *value != 0)
    {
        return Err(ImageError::ReservedNonZero);
    }
    let all_groups_good = validate_group_diagnostics(slot, mapping, direction, bytes)?;
    let diagnostics_end = diagnostics_offset
        .checked_add(
            usize::try_from(slot.diagnostics_bytes()).map_err(|_| ImageError::InvalidCapacity)?,
        )
        .ok_or(ImageError::ArithmeticOverflow)?;
    if bytes[diagnostics_end..].iter().any(|value| *value != 0) {
        return Err(ImageError::ReservedNonZero);
    }
    if matches!(header.aggregate_quality(), AggregateQuality::Good)
        && (!all_values_good || !all_groups_good)
    {
        return Err(ImageError::InvalidQualityMetadata);
    }
    Ok(header)
}

fn validate_value_metadata(
    slot: SlotLayout,
    mapping: ImageMapping<'_>,
    direction: ImageDirection,
    bytes: &[u8],
) -> Result<bool, ImageError> {
    let metadata_offset =
        usize::try_from(slot.metadata_offset()).map_err(|_| ImageError::InvalidCapacity)?;
    let mut all_good = true;
    let mut count = 0_usize;
    for _value in mapping
        .values()
        .iter()
        .filter(|value| value.direction() == direction)
    {
        let offset = metadata_offset
            .checked_add(
                count
                    .checked_mul(ValueMetadata::ENCODED_BYTES)
                    .ok_or(ImageError::ArithmeticOverflow)?,
            )
            .ok_or(ImageError::ArithmeticOverflow)?;
        let metadata = ValueMetadata::decode(copy_8(bytes, offset)?)?;
        all_good &= matches!(metadata.quality(), AggregateQuality::Good);
        count += 1;
    }
    let expected = usize::try_from(slot.value_count()).map_err(|_| ImageError::InvalidCapacity)?;
    if count != expected {
        return Err(ImageError::MappingClosureMismatch);
    }
    Ok(all_good)
}

fn validate_group_diagnostics(
    slot: SlotLayout,
    mapping: ImageMapping<'_>,
    direction: ImageDirection,
    bytes: &[u8],
) -> Result<bool, ImageError> {
    let diagnostics_offset =
        usize::try_from(slot.diagnostics_offset()).map_err(|_| ImageError::InvalidCapacity)?;
    let mut all_good = true;
    let mut count = 0_usize;
    for group in mapping
        .groups()
        .iter()
        .filter(|group| group.direction() == direction)
    {
        let offset = diagnostics_offset
            .checked_add(
                count
                    .checked_mul(GroupDiagnostics::ENCODED_BYTES)
                    .ok_or(ImageError::ArithmeticOverflow)?,
            )
            .ok_or(ImageError::ArithmeticOverflow)?;
        let value_count = mapping
            .values()
            .iter()
            .filter(|value| {
                value.direction() == direction
                    && value.group() == group.handle()
                    && value.source() == group.source()
            })
            .count();
        let value_count = u32::try_from(value_count).map_err(|_| ImageError::InvalidCapacity)?;
        let diagnostics = GroupDiagnostics::decode(copy_64(bytes, offset)?, value_count)?;
        if diagnostics.group_handle() != u32::from(group.handle().get())
            || diagnostics.source_handle() != group.source().get()
        {
            return Err(ImageError::MappingClosureMismatch);
        }
        let (updated_values, stale_values, bad_values, values_good) =
            group_value_quality(slot, mapping, direction, *group, bytes)?;
        if diagnostics.updated_values() != updated_values
            || diagnostics.stale_values() != stale_values
            || diagnostics.bad_values() != bad_values
            || (matches!(diagnostics.aggregate_quality(), AggregateQuality::Good) && !values_good)
        {
            return Err(ImageError::InvalidQualityMetadata);
        }
        all_good &= matches!(diagnostics.aggregate_quality(), AggregateQuality::Good);
        count += 1;
    }
    if count != usize::from(slot.group_count()) {
        return Err(ImageError::MappingClosureMismatch);
    }
    Ok(all_good)
}

fn group_value_quality(
    slot: SlotLayout,
    mapping: ImageMapping<'_>,
    direction: ImageDirection,
    group: crate::GroupDescriptor,
    bytes: &[u8],
) -> Result<(u32, u32, u32, bool), ImageError> {
    let metadata_offset =
        usize::try_from(slot.metadata_offset()).map_err(|_| ImageError::InvalidCapacity)?;
    let mut updated = 0_u32;
    let mut stale = 0_u32;
    let mut bad = 0_u32;
    let mut all_good = true;
    for (direction_index, value) in mapping
        .values()
        .iter()
        .filter(|value| value.direction() == direction)
        .enumerate()
    {
        let offset = metadata_offset
            .checked_add(
                direction_index
                    .checked_mul(ValueMetadata::ENCODED_BYTES)
                    .ok_or(ImageError::ArithmeticOverflow)?,
            )
            .ok_or(ImageError::ArithmeticOverflow)?;
        if value.group() != group.handle() || value.source() != group.source() {
            continue;
        }
        let metadata = ValueMetadata::decode(copy_8(bytes, offset)?)?;
        updated = updated
            .checked_add(u32::from(matches!(
                metadata.update_marker(),
                crate::UpdateMarker::Updated
            )))
            .ok_or(ImageError::ArithmeticOverflow)?;
        stale = stale
            .checked_add(u32::from(matches!(
                metadata.quality(),
                AggregateQuality::Stale
            )))
            .ok_or(ImageError::ArithmeticOverflow)?;
        bad = bad
            .checked_add(u32::from(matches!(
                metadata.quality(),
                AggregateQuality::Bad | AggregateQuality::Stale
            )))
            .ok_or(ImageError::ArithmeticOverflow)?;
        all_good &= matches!(metadata.quality(), AggregateQuality::Good);
    }
    Ok((updated, stale, bad, all_good))
}

/// Unique SPSC writer endpoint for one direction.
pub struct ImageProducer {
    shared: ChannelBacking,
    region: RegionHeader,
    direction: ImageDirection,
    last_sequence: Option<u64>,
}

impl ImageProducer {
    /// Publishes one exact-next complete image without allocating or blocking.
    ///
    /// The writer marks only the non-published slot odd, copies all bytes, marks it even, then
    /// release-stores the publication token. Any validation failure occurs before the odd marker.
    ///
    /// # Errors
    ///
    /// Rejects stale identity/direction, duplicate/skipped sequence, a busy odd target slot, or
    /// generation/token overflow. Rejections increment the direction counter saturatingly.
    pub fn try_publish(&mut self, image: PreparedImage<'_>) -> Result<(), ImageError> {
        let result = self.try_publish_inner(&image);
        if result.is_err() && self.direction == ImageDirection::Input {
            self.shared.increment_loss_or_reject();
        }
        result
    }

    fn try_publish_inner(&mut self, image: &PreparedImage<'_>) -> Result<(), ImageError> {
        if image.region.lease_identity() != self.region.lease_identity()
            || image.region.lease_sequence() != self.region.lease_sequence()
            || image.region.capability_digest() != self.region.capability_digest()
            || image.region.layout() != self.region.layout()
            || image.direction != self.direction
        {
            return Err(ImageError::StaleOrForeignIdentity);
        }
        let sequence = image.header.image_sequence().get();
        let expected = self.last_sequence.map_or(Ok(1), |last| {
            last.checked_add(1).ok_or(ImageError::ArithmeticOverflow)
        })?;
        if sequence != expected {
            return Err(ImageError::ImageSequenceViolation);
        }
        let current_token = self.shared.publish_token();
        let slot_index = if current_token == 0 {
            0_usize
        } else {
            1_usize.wrapping_sub((current_token & 1) as usize)
        };
        let stable_generation = self.shared.generation(slot_index);
        if stable_generation & 1 != 0 {
            return Err(ImageError::Contended);
        }
        let writing_generation = stable_generation
            .checked_add(1)
            .ok_or(ImageError::ArithmeticOverflow)?;
        let completed_generation = stable_generation
            .checked_add(2)
            .ok_or(ImageError::ArithmeticOverflow)?;
        let token = sequence
            .checked_shl(1)
            .and_then(|value| value.checked_add(slot_index as u64))
            .ok_or(ImageError::ArithmeticOverflow)?;
        self.shared.store_generation(slot_index, writing_generation);
        self.shared.write_slot(slot_index, image.bytes);
        self.shared
            .store_generation(slot_index, completed_generation);
        self.shared.store_publish_token(token);
        self.last_sequence = Some(sequence);
        Ok(())
    }

    /// Records one Guardian-side dropped input saturatingly.
    ///
    /// # Errors
    ///
    /// Rejects use through the Control-owned output producer so `OutputRejectCount` remains
    /// Guardian single-writer state.
    pub fn record_input_drop(&self) -> Result<(), ImageError> {
        if self.direction != ImageDirection::Input {
            return Err(ImageError::EndpointOwnership);
        }
        self.shared.increment_loss_or_reject();
        Ok(())
    }

    /// Returns the current saturated direction counter.
    #[must_use]
    pub fn loss_or_reject_count(&self) -> u64 {
        self.shared.loss_or_reject_count()
    }
}

/// Stable, consumer-owned snapshot returned after a successful bounded latch.
#[derive(Debug, Clone, Copy)]
pub struct LatchObservation<'a> {
    header: ImageSlotHeader,
    bytes: &'a [u8],
    sequence_gap: u64,
}

impl<'a> LatchObservation<'a> {
    /// Returns the validated header.
    #[must_use]
    pub const fn header(self) -> ImageSlotHeader {
        self.header
    }

    /// Returns the stable consumer-owned full slot bytes.
    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    /// Returns the number of missing sequences before this image.
    #[must_use]
    pub const fn sequence_gap(self) -> u64 {
        self.sequence_gap
    }
}

/// Unique SPSC reader endpoint with accepted and candidate preallocated buffers.
pub struct ImageConsumer {
    shared: ChannelBacking,
    region: RegionHeader,
    direction: ImageDirection,
    accepted: Box<[u8]>,
    candidate: Box<[u8]>,
    last_token: u64,
    last_sequence: Option<u64>,
}

impl ImageConsumer {
    /// Latches the latest complete image with at most one retry and no allocation or blocking.
    ///
    /// Re-reading the current token returns the prior stable snapshot. A forward sequence gap is
    /// accepted but reported; duplicate/regressing sequences under a new token are rejected.
    /// Output is rejected at `now_ns >= valid_until_monotonic_ns` before any device consumer can
    /// observe it. Every failure preserves the prior accepted snapshot.
    ///
    /// # Errors
    ///
    /// Returns no-publication, contention, invalid identity/header/sequence, or expired output.
    pub fn try_latch(
        &mut self,
        now_ns: u64,
        mapping: ImageMapping<'_>,
    ) -> Result<LatchObservation<'_>, ImageError> {
        let current_token = self.shared.publish_token();
        if current_token == 0 {
            return Err(ImageError::NoPublication);
        }
        if current_token == self.last_token {
            let header = match validate_image_bytes(
                &self.region,
                self.direction,
                mapping,
                &self.accepted,
                false,
            ) {
                Ok(header) => header,
                Err(error) => return self.reject(error),
            };
            if let Err(error) = validate_latch_time(header, self.direction, now_ns) {
                return self.reject(error);
            }
            return Ok(LatchObservation {
                header,
                bytes: &self.accepted,
                sequence_gap: 0,
            });
        }
        for _ in 0..2 {
            let token_before = self.shared.publish_token();
            if token_before == 0 {
                return Err(ImageError::NoPublication);
            }
            let slot_index = (token_before & 1) as usize;
            let generation_before = self.shared.generation(slot_index);
            if generation_before & 1 != 0 {
                continue;
            }
            self.candidate[..8].copy_from_slice(&generation_before.to_le_bytes());
            self.shared.read_slot(slot_index, &mut self.candidate);
            let generation_after = self.shared.generation(slot_index);
            let token_after = self.shared.publish_token();
            if generation_before != generation_after
                || generation_after & 1 != 0
                || token_before != token_after
            {
                continue;
            }
            let header = match validate_image_bytes(
                &self.region,
                self.direction,
                mapping,
                &self.candidate,
                false,
            ) {
                Ok(header) => header,
                Err(error) => return self.reject(error),
            };
            if encode_token(header.image_sequence().get(), slot_index)? != token_before {
                return self.reject(ImageError::ImageSequenceViolation);
            }
            if let Err(error) = validate_latch_time(header, self.direction, now_ns) {
                return self.reject(error);
            }
            let sequence = header.image_sequence().get();
            let sequence_gap = match self.last_sequence {
                Some(last) => {
                    if sequence <= last {
                        return self.reject(ImageError::ImageSequenceViolation);
                    }
                    sequence
                        .checked_sub(last)
                        .and_then(|difference| difference.checked_sub(1))
                        .ok_or(ImageError::ArithmeticOverflow)?
                }
                None => sequence - 1,
            };
            std::mem::swap(&mut self.accepted, &mut self.candidate);
            self.last_token = token_before;
            self.last_sequence = Some(sequence);
            return Ok(LatchObservation {
                header,
                bytes: &self.accepted,
                sequence_gap,
            });
        }
        self.reject(ImageError::Contended)
    }

    /// Returns the prior accepted snapshot, if one exists.
    #[must_use]
    pub fn previous_snapshot(&self) -> Option<&[u8]> {
        (self.last_token != 0).then_some(&self.accepted)
    }

    /// Returns the current saturated direction counter.
    #[must_use]
    pub fn loss_or_reject_count(&self) -> u64 {
        self.shared.loss_or_reject_count()
    }

    fn reject<T>(&self, error: ImageError) -> Result<T, ImageError> {
        if self.direction == ImageDirection::Output {
            self.shared.increment_loss_or_reject();
        }
        Err(error)
    }
}

pub(crate) fn channel(
    region: &RegionHeader,
    direction: ImageDirection,
) -> Result<(ImageProducer, ImageConsumer, Arc<SharedImageChannel>), ImageError> {
    let stride = usize::try_from(region.layout().slot(direction).stride_bytes())
        .map_err(|_| ImageError::InvalidCapacity)?;
    let shared = Arc::new(SharedImageChannel::new(stride)?);
    let backing = ChannelBacking::Local(Arc::clone(&shared));
    let (producer, consumer) = endpoints(region, direction, backing)?;
    Ok((producer, consumer, shared))
}

#[cfg(target_os = "linux")]
pub(crate) fn mapped_producer(
    region: &RegionHeader,
    direction: ImageDirection,
    shared: Arc<crate::linux::MappedImageChannel>,
) -> ImageProducer {
    producer(region, direction, ChannelBacking::Mapped(shared))
}

#[cfg(target_os = "linux")]
pub(crate) fn mapped_consumer(
    region: &RegionHeader,
    direction: ImageDirection,
    shared: Arc<crate::linux::MappedImageChannel>,
) -> Result<ImageConsumer, ImageError> {
    consumer(region, direction, ChannelBacking::Mapped(shared))
}

fn endpoints(
    region: &RegionHeader,
    direction: ImageDirection,
    backing: ChannelBacking,
) -> Result<(ImageProducer, ImageConsumer), ImageError> {
    let producer = producer(region, direction, backing.clone());
    let consumer = consumer(region, direction, backing)?;
    Ok((producer, consumer))
}

fn producer(
    region: &RegionHeader,
    direction: ImageDirection,
    backing: ChannelBacking,
) -> ImageProducer {
    ImageProducer {
        shared: backing,
        region: *region,
        direction,
        last_sequence: None,
    }
}

fn consumer(
    region: &RegionHeader,
    direction: ImageDirection,
    backing: ChannelBacking,
) -> Result<ImageConsumer, ImageError> {
    let stride = usize::try_from(region.layout().slot(direction).stride_bytes())
        .map_err(|_| ImageError::InvalidCapacity)?;
    let accepted = zeroed_bytes(stride)?;
    let candidate = zeroed_bytes(stride)?;
    Ok(ImageConsumer {
        shared: backing,
        region: *region,
        direction,
        accepted,
        candidate,
        last_token: 0,
        last_sequence: None,
    })
}

fn validate_latch_time(
    header: ImageSlotHeader,
    direction: ImageDirection,
    now_ns: u64,
) -> Result<(), ImageError> {
    if now_ns < header.publish_monotonic_ns() {
        return Err(ImageError::InvalidTimestamp);
    }
    if direction == ImageDirection::Output && now_ns >= header.valid_until_monotonic_ns() {
        return Err(ImageError::InvalidValidityDeadline);
    }
    Ok(())
}

fn zeroed_bytes(byte_count: usize) -> Result<Box<[u8]>, ImageError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(byte_count)
        .map_err(|_| ImageError::AllocationFailed)?;
    bytes.resize(byte_count, 0);
    Ok(bytes.into_boxed_slice())
}

fn encode_token(sequence: u64, slot_index: usize) -> Result<u64, ImageError> {
    sequence
        .checked_shl(1)
        .and_then(|value| value.checked_add(slot_index as u64))
        .ok_or(ImageError::ArithmeticOverflow)
}

fn copy_8(bytes: &[u8], offset: usize) -> Result<[u8; 8], ImageError> {
    let end = offset
        .checked_add(8)
        .ok_or(ImageError::ArithmeticOverflow)?;
    let Some(source) = bytes.get(offset..end) else {
        return Err(ImageError::SlotSizeMismatch);
    };
    let mut result = [0; 8];
    result.copy_from_slice(source);
    Ok(result)
}

fn copy_64(bytes: &[u8], offset: usize) -> Result<[u8; 64], ImageError> {
    let end = offset
        .checked_add(64)
        .ok_or(ImageError::ArithmeticOverflow)?;
    let Some(source) = bytes.get(offset..end) else {
        return Err(ImageError::SlotSizeMismatch);
    };
    let mut result = [0; 64];
    result.copy_from_slice(source);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{AtomicSlot, SharedImageChannel, channel};
    use crate::{
        CapabilityDigest, GroupDescriptor, ImageDirection, ImageError, ImageLayout, ImageMapping,
        RegionHeader, SourceDescriptor, ValueBinding,
    };
    use aurora_io_guardian_contracts::{
        ConfigurationDigest, ConfigurationGeneration, GuardianConfiguration, GuardianEpoch,
        LayoutDigest, LeaseId, LeaseIdentity, LeaseSequence,
    };
    use std::sync::atomic::Ordering;

    #[test]
    fn allocation_is_exact_and_loss_counter_saturates() {
        let channel = SharedImageChannel::new(128);
        assert!(channel.is_ok());
        if let Ok(channel) = channel {
            channel
                .loss_or_reject_count
                .store(u64::MAX - 1, Ordering::Relaxed);
            channel.increment_loss_or_reject();
            channel.increment_loss_or_reject();
            assert_eq!(channel.loss_or_reject_count(), u64::MAX);
        }
        assert!(AtomicSlot::new(0).is_ok());
        assert!(matches!(
            SharedImageChannel::new(127),
            Err(ImageError::InvalidCapacity)
        ));
    }

    #[test]
    fn crashed_writer_odd_generation_is_rejected_after_two_attempts() {
        let layout = ImageLayout::new(0, 0, 0, 0, 0, 0, 1_024);
        assert!(layout.is_ok());
        if let Ok(layout) = layout {
            let epoch = GuardianEpoch::new(1);
            let generation = ConfigurationGeneration::new(1);
            let lease_id = LeaseId::new([1; 16]);
            let lease_sequence = LeaseSequence::new(1);
            assert!(epoch.is_ok());
            assert!(generation.is_ok());
            assert!(lease_id.is_ok());
            assert!(lease_sequence.is_ok());
            if let (Ok(epoch), Ok(generation), Ok(lease_id), Ok(lease_sequence)) =
                (epoch, generation, lease_id, lease_sequence)
            {
                let configuration = GuardianConfiguration::new(
                    epoch,
                    generation,
                    ConfigurationDigest::from_sha256([2; 32]),
                    LayoutDigest::from_sha256([3; 32]),
                );
                let region = RegionHeader::new(
                    layout,
                    LeaseIdentity::new(configuration, lease_id),
                    lease_sequence,
                    CapabilityDigest::from_sha256([4; 32]),
                );
                let sources: [SourceDescriptor; 0] = [];
                let groups: [GroupDescriptor; 0] = [];
                let values: [ValueBinding; 0] = [];
                let mapping = ImageMapping::new(
                    layout,
                    configuration.layout_digest(),
                    region.capability_digest(),
                    &sources,
                    &groups,
                    &values,
                );
                assert!(mapping.is_ok());
                if let Ok(mapping) = mapping {
                    let endpoints = channel(&region, ImageDirection::Output);
                    assert!(endpoints.is_ok());
                    if let Ok((_producer, mut consumer, shared)) = endpoints {
                        shared.publish_token.store(2, Ordering::Release);
                        shared.slots[0].generation.store(1, Ordering::Release);
                        assert_eq!(
                            consumer.try_latch(0, mapping).err(),
                            Some(ImageError::Contended)
                        );
                        assert!(consumer.previous_snapshot().is_none());
                        assert_eq!(consumer.loss_or_reject_count(), 1);
                    }
                }
            }
        }
    }
}
