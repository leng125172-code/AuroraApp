//! Linux sealed-memfd realization of the fixed I/O image ABI.

#![allow(unsafe_code)]

use std::{
    ffi::c_void,
    os::fd::OwnedFd,
    ptr::{self, NonNull},
    sync::{
        Arc,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
};

use rustix::{
    fs::{MemfdFlags, SealFlags, fcntl_add_seals, fcntl_get_seals, fstat, ftruncate, memfd_create},
    io::{FdFlags, fcntl_dupfd_cloexec, fcntl_getfd},
    mm::{MapFlags, ProtFlags, mmap, munmap},
};

use crate::{
    ImageConsumer, ImageDirection, ImageError, ImageMapping, ImageProducer, LinuxOperation,
    REGION_HEADER_BYTES, RegionHeader,
    channel::{mapped_consumer, mapped_producer},
};

const INPUT_PUBLISH_TOKEN_OFFSET: usize = 160;
const OUTPUT_PUBLISH_TOKEN_OFFSET: usize = 168;
const INPUT_DROP_COUNT_OFFSET: usize = 176;
const OUTPUT_REJECT_COUNT_OFFSET: usize = 184;
const FIRST_RUNTIME_ATOMIC_OFFSET: usize = INPUT_PUBLISH_TOKEN_OFFSET;
const AFTER_RUNTIME_ATOMICS_OFFSET: usize = 192;
const IMAGE_ALIGNMENT_USIZE: usize = 64;

fn required_seals() -> SealFlags {
    SealFlags::GROW | SealFlags::SHRINK | SealFlags::SEAL
}

fn system_error(operation: LinuxOperation, error: rustix::io::Errno) -> ImageError {
    ImageError::LinuxSystemCall {
        operation,
        raw_os_error: error.raw_os_error(),
    }
}

struct LinuxMapping {
    pointer: NonNull<u8>,
    byte_count: usize,
    descriptor: OwnedFd,
}

// SAFETY: The mapping has a fixed sealed length, immutable bytes are written only before sharing,
// and every concurrently mutable location is accessed exclusively through AtomicU8/AtomicU64.
unsafe impl Send for LinuxMapping {}
// SAFETY: See Send. Shared references expose no non-atomic mutation and the mapping outlives every
// atomic reference because all channel views retain the same Arc<LinuxMapping>.
unsafe impl Sync for LinuxMapping {}

impl LinuxMapping {
    fn create(header: &RegionHeader) -> Result<Arc<Self>, ImageError> {
        validate_fresh_header(header)?;
        let descriptor = memfd_create(
            "aurora-io-image",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )
        .map_err(|error| system_error(LinuxOperation::CreateMemfd, error))?;
        ftruncate(&descriptor, header.layout().total_bytes())
            .map_err(|error| system_error(LinuxOperation::ResizeMemfd, error))?;
        let mapping = Arc::new(Self::map(descriptor, header.layout().total_bytes())?);
        mapping.initialize((*header).encode());
        fcntl_add_seals(&mapping.descriptor, required_seals())
            .map_err(|error| system_error(LinuxOperation::AddSeals, error))?;
        validate_descriptor(&mapping.descriptor, mapping.byte_count)?;
        Ok(mapping)
    }

    fn import(descriptor: OwnedFd, expected: &RegionHeader) -> Result<Arc<Self>, ImageError> {
        let byte_count = usize::try_from(expected.layout().total_bytes())
            .map_err(|_| ImageError::InvalidCapacity)?;
        validate_descriptor(&descriptor, byte_count)?;
        let mapping = Arc::new(Self::map(descriptor, expected.layout().total_bytes())?);
        let actual = RegionHeader::decode(mapping.header_bytes())?;
        if actual.layout() != expected.layout()
            || actual.lease_identity() != expected.lease_identity()
            || actual.lease_sequence() != expected.lease_sequence()
            || actual.capability_digest() != expected.capability_digest()
        {
            return Err(ImageError::StaleOrForeignIdentity);
        }
        Ok(mapping)
    }

    fn map(descriptor: OwnedFd, total_bytes: u64) -> Result<Self, ImageError> {
        let byte_count = usize::try_from(total_bytes).map_err(|_| ImageError::InvalidCapacity)?;
        if byte_count < REGION_HEADER_BYTES {
            return Err(ImageError::InvalidCapacity);
        }
        // SAFETY: A null hint requests a kernel-selected page-aligned address. byte_count is the
        // exact positive fstat-validated memfd length, offset is zero, and descriptor ownership is
        // retained until after munmap in Drop. No Rust reference exists yet.
        let raw = unsafe {
            mmap(
                ptr::null_mut(),
                byte_count,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &descriptor,
                0,
            )
        }
        .map_err(|error| system_error(LinuxOperation::MapRegion, error))?;
        let pointer = NonNull::new(raw.cast::<u8>()).ok_or(ImageError::InvalidLinuxMapping)?;
        if pointer.as_ptr().addr() % IMAGE_ALIGNMENT_USIZE != 0 {
            // SAFETY: raw is the successful mapping returned immediately above and no reference
            // has been created. This branch relinquishes it before returning an error.
            let _ = unsafe { munmap(raw, byte_count) };
            return Err(ImageError::InvalidLinuxMapping);
        }
        Ok(Self {
            pointer,
            byte_count,
            descriptor,
        })
    }

    fn initialize(&self, header: [u8; REGION_HEADER_BYTES]) {
        // SAFETY: This is a fresh private mapping before Arc publication. The mapping covers
        // byte_count writable bytes and header is exactly the first 256 non-overlapping bytes.
        unsafe {
            ptr::write_bytes(self.pointer.as_ptr(), 0, self.byte_count);
            ptr::copy_nonoverlapping(header.as_ptr(), self.pointer.as_ptr(), header.len());
        }
    }

    fn duplicate_descriptor(&self) -> Result<OwnedFd, ImageError> {
        fcntl_dupfd_cloexec(&self.descriptor, 0)
            .map_err(|error| system_error(LinuxOperation::DuplicateDescriptor, error))
    }

    fn header_bytes(&self) -> [u8; REGION_HEADER_BYTES] {
        let mut bytes = [0; REGION_HEADER_BYTES];
        // SAFETY: These two ranges are inside the mapping, never mutate after initialization, and
        // exclude the four AtomicU64 runtime fields at 160..192.
        unsafe {
            ptr::copy_nonoverlapping(
                self.pointer.as_ptr(),
                bytes.as_mut_ptr(),
                FIRST_RUNTIME_ATOMIC_OFFSET,
            );
            ptr::copy_nonoverlapping(
                self.pointer.as_ptr().add(AFTER_RUNTIME_ATOMICS_OFFSET),
                bytes.as_mut_ptr().add(AFTER_RUNTIME_ATOMICS_OFFSET),
                REGION_HEADER_BYTES - AFTER_RUNTIME_ATOMICS_OFFSET,
            );
        }
        for offset in [
            INPUT_PUBLISH_TOKEN_OFFSET,
            OUTPUT_PUBLISH_TOKEN_OFFSET,
            INPUT_DROP_COUNT_OFFSET,
            OUTPUT_REJECT_COUNT_OFFSET,
        ] {
            bytes[offset..offset + 8].copy_from_slice(
                &self
                    .atomic_u64(offset)
                    .load(Ordering::Acquire)
                    .to_le_bytes(),
            );
        }
        bytes
    }

    #[allow(clippy::cast_ptr_alignment)]
    fn atomic_u64(&self, offset: usize) -> &AtomicU64 {
        // SAFETY: All callers use construction-validated 8-byte-aligned offsets fully inside the
        // mapping. These bytes are accessed only as AtomicU64 for the mapping lifetime.
        unsafe { AtomicU64::from_ptr(self.pointer.as_ptr().add(offset).cast::<u64>()) }
    }

    fn atomic_u8(&self, offset: usize) -> &AtomicU8 {
        // SAFETY: All callers use offsets fully inside the mapping and exclude every AtomicU64
        // field. AtomicU8 alignment is one and these bytes are never accessed non-atomically after
        // endpoint publication.
        unsafe { AtomicU8::from_ptr(self.pointer.as_ptr().add(offset)) }
    }
}

impl Drop for LinuxMapping {
    fn drop(&mut self) {
        // SAFETY: Arc uniqueness at Drop proves no channel view or atomic reference remains. The
        // pointer and length are exactly those returned by mmap and have not been unmapped before.
        let _ = unsafe { munmap(self.pointer.as_ptr().cast::<c_void>(), self.byte_count) };
    }
}

/// One direction view over a sealed Linux mapping.
pub(crate) struct MappedImageChannel {
    mapping: Arc<LinuxMapping>,
    slot_base: usize,
    slot_stride: usize,
    publish_token_offset: usize,
    loss_or_reject_offset: usize,
}

impl MappedImageChannel {
    fn new(
        mapping: Arc<LinuxMapping>,
        header: &RegionHeader,
        direction: ImageDirection,
    ) -> Result<Self, ImageError> {
        let layout = header.layout();
        let slot = layout.slot(direction);
        let slot_base = usize::try_from(match direction {
            ImageDirection::Input => layout.input_offset(),
            ImageDirection::Output => layout.output_offset(),
        })
        .map_err(|_| ImageError::InvalidCapacity)?;
        let slot_stride =
            usize::try_from(slot.stride_bytes()).map_err(|_| ImageError::InvalidCapacity)?;
        let slots_end = slot_base
            .checked_add(
                slot_stride
                    .checked_mul(2)
                    .ok_or(ImageError::ArithmeticOverflow)?,
            )
            .ok_or(ImageError::ArithmeticOverflow)?;
        if slots_end > mapping.byte_count
            || slot_base % IMAGE_ALIGNMENT_USIZE != 0
            || slot_stride % IMAGE_ALIGNMENT_USIZE != 0
        {
            return Err(ImageError::InvalidLinuxMapping);
        }
        let (publish_token_offset, loss_or_reject_offset) = match direction {
            ImageDirection::Input => (INPUT_PUBLISH_TOKEN_OFFSET, INPUT_DROP_COUNT_OFFSET),
            ImageDirection::Output => (OUTPUT_PUBLISH_TOKEN_OFFSET, OUTPUT_REJECT_COUNT_OFFSET),
        };
        Ok(Self {
            mapping,
            slot_base,
            slot_stride,
            publish_token_offset,
            loss_or_reject_offset,
        })
    }

    pub(crate) fn publish_token(&self) -> u64 {
        self.mapping
            .atomic_u64(self.publish_token_offset)
            .load(Ordering::Acquire)
    }

    pub(crate) fn store_publish_token(&self, token: u64) {
        self.mapping
            .atomic_u64(self.publish_token_offset)
            .store(token, Ordering::Release);
    }

    pub(crate) fn loss_or_reject_count(&self) -> u64 {
        self.mapping
            .atomic_u64(self.loss_or_reject_offset)
            .load(Ordering::Acquire)
    }

    pub(crate) fn increment_loss_or_reject(&self) {
        let counter = self.mapping.atomic_u64(self.loss_or_reject_offset);
        let mut current = counter.load(Ordering::Relaxed);
        loop {
            if current == u64::MAX {
                return;
            }
            match counter.compare_exchange_weak(
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

    pub(crate) fn generation(&self, slot_index: usize) -> u64 {
        self.mapping
            .atomic_u64(self.slot_offset(slot_index))
            .load(Ordering::Acquire)
    }

    pub(crate) fn store_generation(&self, slot_index: usize, generation: u64) {
        self.mapping
            .atomic_u64(self.slot_offset(slot_index))
            .store(generation, Ordering::Release);
    }

    pub(crate) fn write_slot(&self, slot_index: usize, bytes: &[u8]) {
        let slot_offset = self.slot_offset(slot_index);
        for (index, value) in bytes.iter().enumerate().skip(8) {
            self.mapping
                .atomic_u8(slot_offset + index)
                .store(*value, Ordering::Relaxed);
        }
    }

    pub(crate) fn read_slot(&self, slot_index: usize, bytes: &mut [u8]) {
        let slot_offset = self.slot_offset(slot_index);
        for (index, value) in bytes.iter_mut().enumerate().skip(8) {
            *value = self
                .mapping
                .atomic_u8(slot_offset + index)
                .load(Ordering::Relaxed);
        }
    }

    fn slot_offset(&self, slot_index: usize) -> usize {
        self.slot_base + slot_index * self.slot_stride
    }
}

/// Guardian-side owner of one fresh sealed memfd mapping.
pub struct LinuxGuardianMappedRegion {
    header: RegionHeader,
    mapping: Arc<LinuxMapping>,
    input: ImageProducer,
    output: ImageConsumer,
    input_shared: Arc<MappedImageChannel>,
    output_shared: Arc<MappedImageChannel>,
    control_descriptor_exported: bool,
}

impl LinuxGuardianMappedRegion {
    /// Creates a new memfd, initializes the exact region, adds the required seals, and constructs
    /// only Guardian-owned endpoints.
    ///
    /// # Errors
    ///
    /// Rejects non-fresh headers, mapping/layout mismatch, allocation overflow, or any Linux
    /// syscall/seal/descriptor failure. A failed call returns no endpoint or descriptor.
    pub fn create(header: &RegionHeader, mapping: ImageMapping<'_>) -> Result<Self, ImageError> {
        if mapping.layout() != header.layout() {
            return Err(ImageError::HeaderMismatch);
        }
        let linux_mapping = LinuxMapping::create(header)?;
        let input_shared = Arc::new(MappedImageChannel::new(
            Arc::clone(&linux_mapping),
            header,
            ImageDirection::Input,
        )?);
        let output_shared = Arc::new(MappedImageChannel::new(
            Arc::clone(&linux_mapping),
            header,
            ImageDirection::Output,
        )?);
        let input = mapped_producer(header, ImageDirection::Input, Arc::clone(&input_shared));
        let output = mapped_consumer(header, ImageDirection::Output, Arc::clone(&output_shared))?;
        Ok(Self {
            header: *header,
            mapping: linux_mapping,
            input,
            output,
            input_shared,
            output_shared,
            control_descriptor_exported: false,
        })
    }

    /// Duplicates the sealed descriptor exactly once with `FD_CLOEXEC` for authenticated
    /// `SCM_RIGHTS` transfer to the selected Control process.
    ///
    /// # Errors
    ///
    /// Rejects a second export from the same lease owner or descriptor duplication failure.
    pub fn export_control_descriptor(&mut self) -> Result<OwnedFd, ImageError> {
        if self.control_descriptor_exported {
            return Err(ImageError::EndpointOwnership);
        }
        let descriptor = self.mapping.duplicate_descriptor()?;
        self.control_descriptor_exported = true;
        Ok(descriptor)
    }

    /// Returns the unique Guardian input producer.
    #[must_use]
    pub fn input_producer(&mut self) -> &mut ImageProducer {
        &mut self.input
    }

    /// Returns the unique Guardian output consumer.
    #[must_use]
    pub fn output_consumer(&mut self) -> &mut ImageConsumer {
        &mut self.output
    }

    /// Returns an observational header snapshot with live atomic counters.
    #[must_use]
    pub fn header_snapshot(&self) -> RegionHeader {
        runtime_header(&self.header, &self.input_shared, &self.output_shared)
    }
}

/// Control-side imported view of one exact sealed lease mapping.
pub struct LinuxControlMappedRegion {
    header: RegionHeader,
    input: ImageConsumer,
    output: ImageProducer,
    input_shared: Arc<MappedImageChannel>,
    output_shared: Arc<MappedImageChannel>,
}

impl LinuxControlMappedRegion {
    /// Consumes an authenticated received descriptor, verifies its immutable identity/layout/seals,
    /// and constructs only Control-owned endpoints.
    ///
    /// The caller remains responsible for transporting this descriptor over an authenticated UDS
    /// session that satisfies the R3-01 peer policy.
    ///
    /// # Errors
    ///
    /// Rejects descriptor length/seal/flag drift, stale identity, malformed layout, or allocation
    /// failure without returning a partially usable endpoint.
    pub fn import(
        descriptor: OwnedFd,
        expected: &RegionHeader,
        mapping: ImageMapping<'_>,
    ) -> Result<Self, ImageError> {
        if mapping.layout() != expected.layout() {
            return Err(ImageError::HeaderMismatch);
        }
        let linux_mapping = LinuxMapping::import(descriptor, expected)?;
        let input_shared = Arc::new(MappedImageChannel::new(
            Arc::clone(&linux_mapping),
            expected,
            ImageDirection::Input,
        )?);
        let output_shared = Arc::new(MappedImageChannel::new(
            linux_mapping,
            expected,
            ImageDirection::Output,
        )?);
        let input = mapped_consumer(expected, ImageDirection::Input, Arc::clone(&input_shared))?;
        let output = mapped_producer(expected, ImageDirection::Output, Arc::clone(&output_shared));
        Ok(Self {
            header: *expected,
            input,
            output,
            input_shared,
            output_shared,
        })
    }

    /// Returns the unique Control input consumer.
    #[must_use]
    pub fn input_consumer(&mut self) -> &mut ImageConsumer {
        &mut self.input
    }

    /// Returns the unique Control output producer.
    #[must_use]
    pub fn output_producer(&mut self) -> &mut ImageProducer {
        &mut self.output
    }

    /// Returns an observational header snapshot with live atomic counters.
    #[must_use]
    pub fn header_snapshot(&self) -> RegionHeader {
        runtime_header(&self.header, &self.input_shared, &self.output_shared)
    }
}

fn validate_fresh_header(header: &RegionHeader) -> Result<(), ImageError> {
    if header.input_publish_token() != 0
        || header.output_publish_token() != 0
        || header.input_drop_count() != 0
        || header.output_reject_count() != 0
    {
        return Err(ImageError::HeaderMismatch);
    }
    Ok(())
}

fn validate_descriptor(descriptor: &OwnedFd, expected_bytes: usize) -> Result<(), ImageError> {
    let metadata =
        fstat(descriptor).map_err(|error| system_error(LinuxOperation::ReadMetadata, error))?;
    if u64::try_from(metadata.st_size).ok() != Some(expected_bytes as u64) {
        return Err(ImageError::InvalidLinuxMapping);
    }
    let seals = fcntl_get_seals(descriptor)
        .map_err(|error| system_error(LinuxOperation::ReadSeals, error))?;
    if seals != required_seals() {
        return Err(ImageError::InvalidLinuxMapping);
    }
    let descriptor_flags = fcntl_getfd(descriptor)
        .map_err(|error| system_error(LinuxOperation::ReadDescriptorFlags, error))?;
    if !descriptor_flags.contains(FdFlags::CLOEXEC) {
        return Err(ImageError::InvalidLinuxMapping);
    }
    Ok(())
}

fn runtime_header(
    header: &RegionHeader,
    input: &MappedImageChannel,
    output: &MappedImageChannel,
) -> RegionHeader {
    (*header).with_runtime_counters(
        input.publish_token(),
        output.publish_token(),
        input.loss_or_reject_count(),
        output.loss_or_reject_count(),
    )
}

#[cfg(test)]
mod tests {
    use super::{LinuxControlMappedRegion, LinuxGuardianMappedRegion};
    use crate::{
        CapabilityDigest, GroupDescriptor, ImageError, ImageLayout, ImageMapping, RegionHeader,
        SourceDescriptor, ValueBinding,
    };
    use aurora_io_guardian_contracts::{
        ConfigurationDigest, ConfigurationGeneration, GuardianConfiguration, GuardianEpoch,
        LayoutDigest, LeaseId, LeaseIdentity, LeaseSequence,
    };

    #[test]
    fn crashed_mapped_writer_preserves_no_snapshot_and_reports_contention() {
        let layout = ImageLayout::new(0, 0, 0, 0, 0, 0, 1_024);
        assert!(layout.is_ok());
        let epoch = GuardianEpoch::new(1);
        let generation = ConfigurationGeneration::new(1);
        let lease_id = LeaseId::new([1; 16]);
        let lease_sequence = LeaseSequence::new(1);
        assert!(epoch.is_ok());
        assert!(generation.is_ok());
        assert!(lease_id.is_ok());
        assert!(lease_sequence.is_ok());
        if let (Ok(layout), Ok(epoch), Ok(generation), Ok(lease_id), Ok(lease_sequence)) =
            (layout, epoch, generation, lease_id, lease_sequence)
        {
            let configuration = GuardianConfiguration::new(
                epoch,
                generation,
                ConfigurationDigest::from_sha256([2; 32]),
                LayoutDigest::from_sha256([3; 32]),
            );
            let header = RegionHeader::new(
                layout,
                LeaseIdentity::new(configuration, lease_id),
                lease_sequence,
                CapabilityDigest::from_sha256([4; 32]),
            );
            let sources: [SourceDescriptor; 0] = [];
            let groups: [GroupDescriptor; 0] = [];
            let values: [ValueBinding; 0] = [];
            let mapping = ImageMapping::new(layout, &sources, &groups, &values);
            assert!(mapping.is_ok());
            if let Ok(mapping) = mapping {
                let guardian = LinuxGuardianMappedRegion::create(&header, mapping);
                assert!(guardian.is_ok());
                if let Ok(mut guardian) = guardian {
                    let descriptor = guardian.export_control_descriptor();
                    assert!(descriptor.is_ok());
                    if let Ok(descriptor) = descriptor {
                        let control =
                            LinuxControlMappedRegion::import(descriptor, &header, mapping);
                        assert!(control.is_ok());
                        if let Ok(mut control) = control {
                            guardian.input_shared.store_generation(0, 1);
                            guardian.input_shared.store_publish_token(2);
                            assert_eq!(
                                control.input_consumer().try_latch(0, mapping).err(),
                                Some(ImageError::Contended)
                            );
                            assert!(control.input_consumer().previous_snapshot().is_none());
                            assert_eq!(control.header_snapshot().input_drop_count(), 0);
                            assert_eq!(guardian.header_snapshot().input_publish_token(), 2);
                        }
                    }
                }
            }
        }
    }
}
