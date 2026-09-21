//! Checked region and slot layout arithmetic.

use crate::ImageError;

/// Required alignment for region, slot, metadata, and diagnostics boundaries.
pub const IMAGE_ALIGNMENT: u64 = 64;
/// Fixed value-metadata record size.
pub const VALUE_METADATA_BYTES: u32 = 8;
/// Fixed group-diagnostic record size.
pub const GROUP_DIAGNOSTIC_BYTES: u32 = 64;
const REGION_HEADER_BYTES_U64: u64 = 256;
const SLOT_HEADER_BYTES_U64: u64 = 128;

/// Input or output half of one shared I/O region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ImageDirection {
    /// Guardian-produced, Control-read input image.
    Input = 0,
    /// Control-produced, Guardian-read output command image.
    Output = 1,
}

/// Exact layout of both slots for one direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotLayout {
    payload_capacity_bytes: u32,
    value_count: u32,
    group_count: u16,
    metadata_offset: u32,
    metadata_bytes: u32,
    diagnostics_offset: u32,
    diagnostics_bytes: u32,
    stride_bytes: u32,
}

impl SlotLayout {
    fn new(
        payload_capacity_bytes: u32,
        value_count: u32,
        group_count: u16,
    ) -> Result<Self, ImageError> {
        let payload_end = SLOT_HEADER_BYTES_U64
            .checked_add(u64::from(payload_capacity_bytes))
            .ok_or(ImageError::ArithmeticOverflow)?;
        let metadata_offset = align_up(payload_end)?;
        let metadata_bytes = u64::from(value_count)
            .checked_mul(u64::from(VALUE_METADATA_BYTES))
            .ok_or(ImageError::ArithmeticOverflow)?;
        let metadata_end = metadata_offset
            .checked_add(metadata_bytes)
            .ok_or(ImageError::ArithmeticOverflow)?;
        let diagnostics_offset = align_up(metadata_end)?;
        let diagnostics_bytes = u64::from(group_count)
            .checked_mul(u64::from(GROUP_DIAGNOSTIC_BYTES))
            .ok_or(ImageError::ArithmeticOverflow)?;
        let diagnostics_end = diagnostics_offset
            .checked_add(diagnostics_bytes)
            .ok_or(ImageError::ArithmeticOverflow)?;
        let stride_bytes = align_up(diagnostics_end)?;
        Ok(Self {
            payload_capacity_bytes,
            value_count,
            group_count,
            metadata_offset: to_u32(metadata_offset)?,
            metadata_bytes: to_u32(metadata_bytes)?,
            diagnostics_offset: to_u32(diagnostics_offset)?,
            diagnostics_bytes: to_u32(diagnostics_bytes)?,
            stride_bytes: to_u32(stride_bytes)?,
        })
    }

    /// Returns the fixed payload capacity in bytes.
    #[must_use]
    pub const fn payload_capacity_bytes(self) -> u32 {
        self.payload_capacity_bytes
    }

    /// Returns the exact number of direction-local value records.
    #[must_use]
    pub const fn value_count(self) -> u32 {
        self.value_count
    }

    /// Returns the exact number of direction-local group records.
    #[must_use]
    pub const fn group_count(self) -> u16 {
        self.group_count
    }

    /// Returns the implied slot-relative value-metadata offset.
    #[must_use]
    pub const fn metadata_offset(self) -> u32 {
        self.metadata_offset
    }

    /// Returns the exact value-metadata byte count.
    #[must_use]
    pub const fn metadata_bytes(self) -> u32 {
        self.metadata_bytes
    }

    /// Returns the slot-relative group-diagnostics offset.
    #[must_use]
    pub const fn diagnostics_offset(self) -> u32 {
        self.diagnostics_offset
    }

    /// Returns the exact group-diagnostics byte count.
    #[must_use]
    pub const fn diagnostics_bytes(self) -> u32 {
        self.diagnostics_bytes
    }

    /// Returns the exact 64-byte-aligned slot stride.
    #[must_use]
    pub const fn stride_bytes(self) -> u32 {
        self.stride_bytes
    }
}

/// Exact checked layout of one region header and four image slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageLayout {
    input_offset: u64,
    output_offset: u64,
    total_bytes: u64,
    input: SlotLayout,
    output: SlotLayout,
    value_count: u32,
}

impl ImageLayout {
    /// Computes the only admitted region ordering and padding.
    ///
    /// `maximum_region_bytes` is a caller-supplied Target Profile/allocation limit. Equality is
    /// accepted; the first byte above it is rejected before allocation.
    ///
    /// # Errors
    ///
    /// Rejects count/arithmetic overflow, a zero maximum, or a computed region above the maximum.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input_payload_capacity_bytes: u32,
        output_payload_capacity_bytes: u32,
        input_value_count: u32,
        output_value_count: u32,
        input_group_count: u16,
        output_group_count: u16,
        maximum_region_bytes: u64,
    ) -> Result<Self, ImageError> {
        if maximum_region_bytes == 0 {
            return Err(ImageError::InvalidCapacity);
        }
        let value_count = input_value_count
            .checked_add(output_value_count)
            .ok_or(ImageError::ArithmeticOverflow)?;
        let input = SlotLayout::new(
            input_payload_capacity_bytes,
            input_value_count,
            input_group_count,
        )?;
        let output = SlotLayout::new(
            output_payload_capacity_bytes,
            output_value_count,
            output_group_count,
        )?;
        let input_offset = REGION_HEADER_BYTES_U64;
        let input_slots_end = input_offset
            .checked_add(
                u64::from(input.stride_bytes)
                    .checked_mul(2)
                    .ok_or(ImageError::ArithmeticOverflow)?,
            )
            .ok_or(ImageError::ArithmeticOverflow)?;
        let output_offset = align_up(input_slots_end)?;
        let output_slots_end = output_offset
            .checked_add(
                u64::from(output.stride_bytes)
                    .checked_mul(2)
                    .ok_or(ImageError::ArithmeticOverflow)?,
            )
            .ok_or(ImageError::ArithmeticOverflow)?;
        let total_bytes = align_up(output_slots_end)?;
        if total_bytes > maximum_region_bytes {
            return Err(ImageError::InvalidCapacity);
        }
        Ok(Self {
            input_offset,
            output_offset,
            total_bytes,
            input,
            output,
            value_count,
        })
    }

    /// Returns the first input-slot offset.
    #[must_use]
    pub const fn input_offset(self) -> u64 {
        self.input_offset
    }

    /// Returns the first output-slot offset.
    #[must_use]
    pub const fn output_offset(self) -> u64 {
        self.output_offset
    }

    /// Returns the exact 64-byte-aligned region size.
    #[must_use]
    pub const fn total_bytes(self) -> u64 {
        self.total_bytes
    }

    /// Returns the input slot layout.
    #[must_use]
    pub const fn input(self) -> SlotLayout {
        self.input
    }

    /// Returns the output slot layout.
    #[must_use]
    pub const fn output(self) -> SlotLayout {
        self.output
    }

    /// Returns the exact combined value count.
    #[must_use]
    pub const fn value_count(self) -> u32 {
        self.value_count
    }

    /// Returns one direction's slot layout.
    #[must_use]
    pub const fn slot(self, direction: ImageDirection) -> SlotLayout {
        match direction {
            ImageDirection::Input => self.input,
            ImageDirection::Output => self.output,
        }
    }
}

fn align_up(value: u64) -> Result<u64, ImageError> {
    value
        .checked_add(IMAGE_ALIGNMENT - 1)
        .map(|expanded| expanded & !(IMAGE_ALIGNMENT - 1))
        .ok_or(ImageError::ArithmeticOverflow)
}

fn to_u32(value: u64) -> Result<u32, ImageError> {
    u32::try_from(value).map_err(|_| ImageError::InvalidCapacity)
}

#[cfg(test)]
mod tests {
    use super::{IMAGE_ALIGNMENT, ImageLayout};
    use crate::ImageError;

    #[test]
    fn exact_layout_has_four_slots_and_only_required_alignment() {
        let layout = ImageLayout::new(17, 33, 2, 3, 1, 2, 2_048);
        assert!(layout.is_ok());
        if let Ok(layout) = layout {
            assert_eq!(layout.input_offset(), 256);
            assert_eq!(layout.input().metadata_offset(), 192);
            assert_eq!(layout.input().diagnostics_offset(), 256);
            assert_eq!(layout.input().stride_bytes(), 320);
            assert_eq!(layout.output().metadata_offset(), 192);
            assert_eq!(layout.output().diagnostics_offset(), 256);
            assert_eq!(layout.output().stride_bytes(), 384);
            assert_eq!(layout.output_offset(), 896);
            assert_eq!(layout.total_bytes(), 1_664);
            assert_eq!(layout.total_bytes() % IMAGE_ALIGNMENT, 0);
        }
    }

    #[test]
    fn region_budget_accepts_equality_and_rejects_first_byte_below() {
        let exact = ImageLayout::new(17, 33, 2, 3, 1, 2, 1_664);
        assert!(exact.is_ok());
        assert_eq!(
            ImageLayout::new(17, 33, 2, 3, 1, 2, 1_663),
            Err(ImageError::InvalidCapacity)
        );
    }
}
