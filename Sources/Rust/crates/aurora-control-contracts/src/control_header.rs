//! Fixed-width control shared-memory header.

use thiserror::Error;

/// Validation failure for a control-layout header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ControlHeaderError {
    /// Magic does not identify the Aurora v1 control layout.
    #[error("control header magic must be AURCTL01")]
    InvalidMagic,
    /// The layout major is not supported by this reader.
    #[error("control layout major must be 1")]
    UnsupportedMajor,
    /// The declared header size is not the fixed v1 size.
    #[error("control header size must be 64 bytes")]
    InvalidHeaderSize,
    /// F0 does not define any header flag bits.
    #[error("control header contains undefined flags")]
    UndefinedFlags,
    /// Total mapped size is smaller than the header.
    #[error("control mapping total size must include the 64-byte header")]
    InvalidTotalSize,
    /// Capability offset/count presence does not agree.
    #[error("capability offset and count must either both be zero or both be present")]
    InvalidCapabilityTable,
}

/// Validated 64-byte little-endian control-layout header v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlHeader {
    layout_minor: u16,
    total_size: u64,
    schema_hash: [u8; 32],
    capability_table_offset: u32,
    capability_count: u32,
}

impl ControlHeader {
    /// Fixed serialized size of the v1 header.
    pub const SIZE: usize = 64;
    const SIZE_FIELD: u16 = 64;
    /// Supported layout major.
    pub const LAYOUT_MAJOR: u16 = 1;
    const MAGIC: [u8; 8] = *b"AURCTL01";

    /// Creates a validated control-layout header.
    ///
    /// # Errors
    ///
    /// Returns [`ControlHeaderError::InvalidTotalSize`] when `total_size` is
    /// smaller than the header, or [`ControlHeaderError::InvalidCapabilityTable`]
    /// when only one of capability offset/count is zero.
    pub const fn new(
        layout_minor: u16,
        total_size: u64,
        schema_hash: [u8; 32],
        capability_table_offset: u32,
        capability_count: u32,
    ) -> Result<Self, ControlHeaderError> {
        if total_size < Self::SIZE as u64 {
            return Err(ControlHeaderError::InvalidTotalSize);
        }
        if (capability_table_offset == 0) != (capability_count == 0) {
            return Err(ControlHeaderError::InvalidCapabilityTable);
        }
        Ok(Self {
            layout_minor,
            total_size,
            schema_hash,
            capability_table_offset,
            capability_count,
        })
    }

    /// Decodes and validates exactly one v1 header.
    ///
    /// # Errors
    ///
    /// Returns a specific [`ControlHeaderError`] when magic, version, size,
    /// flags, total size, or capability-table presence is invalid.
    pub fn decode(bytes: &[u8; Self::SIZE]) -> Result<Self, ControlHeaderError> {
        if bytes[0..8] != Self::MAGIC {
            return Err(ControlHeaderError::InvalidMagic);
        }
        if read_u16(bytes, 8) != Self::LAYOUT_MAJOR {
            return Err(ControlHeaderError::UnsupportedMajor);
        }
        if read_u16(bytes, 12) != Self::SIZE_FIELD {
            return Err(ControlHeaderError::InvalidHeaderSize);
        }
        if read_u16(bytes, 14) != 0 {
            return Err(ControlHeaderError::UndefinedFlags);
        }
        let total_size = read_u64(bytes, 16);
        let mut schema_hash = [0_u8; 32];
        schema_hash.copy_from_slice(&bytes[24..56]);
        Self::new(
            read_u16(bytes, 10),
            total_size,
            schema_hash,
            read_u32(bytes, 56),
            read_u32(bytes, 60),
        )
    }

    /// Encodes the validated header without language ABI padding.
    #[must_use]
    pub fn encode(self) -> [u8; Self::SIZE] {
        let mut bytes = [0_u8; Self::SIZE];
        bytes[0..8].copy_from_slice(&Self::MAGIC);
        bytes[8..10].copy_from_slice(&Self::LAYOUT_MAJOR.to_le_bytes());
        bytes[10..12].copy_from_slice(&self.layout_minor.to_le_bytes());
        bytes[12..14].copy_from_slice(&Self::SIZE_FIELD.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.total_size.to_le_bytes());
        bytes[24..56].copy_from_slice(&self.schema_hash);
        bytes[56..60].copy_from_slice(&self.capability_table_offset.to_le_bytes());
        bytes[60..64].copy_from_slice(&self.capability_count.to_le_bytes());
        bytes
    }

    /// Returns the additive layout revision.
    #[must_use]
    pub const fn layout_minor(self) -> u16 {
        self.layout_minor
    }

    /// Returns total mapped bytes including this header.
    #[must_use]
    pub const fn total_size(self) -> u64 {
        self.total_size
    }

    /// Returns the raw SHA-256 of the associated schema.
    #[must_use]
    pub const fn schema_hash(self) -> [u8; 32] {
        self.schema_hash
    }

    /// Returns the capability-table byte offset, or zero when absent.
    #[must_use]
    pub const fn capability_table_offset(self) -> u32 {
        self.capability_table_offset
    }

    /// Returns the capability-table entry count.
    #[must_use]
    pub const fn capability_count(self) -> u32 {
        self.capability_count
    }
}

fn read_u16(bytes: &[u8; ControlHeader::SIZE], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8; ControlHeader::SIZE], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8; ControlHeader::SIZE], offset: usize) -> u64 {
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

#[cfg(test)]
mod tests {
    use super::{ControlHeader, ControlHeaderError};

    #[test]
    fn f0_golden_header_round_trips() {
        let header = ControlHeader::new(0, 64, [0; 32], 0, 0).unwrap_or_else(|_| fallback());
        let expected = [
            0x41, 0x55, 0x52, 0x43, 0x54, 0x4c, 0x30, 0x31, 0x01, 0x00, 0x00, 0x00, 0x40, 0x00,
            0x00, 0x00, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(header.encode(), expected);
        assert_eq!(ControlHeader::decode(&expected), Ok(header));
        assert_eq!(header.layout_minor(), 0);
        assert_eq!(header.total_size(), 64);
        assert_eq!(header.schema_hash(), [0; 32]);
        assert_eq!(header.capability_table_offset(), 0);
        assert_eq!(header.capability_count(), 0);
    }

    #[test]
    fn invalid_headers_are_rejected_before_mapping() {
        assert_eq!(
            ControlHeader::new(0, 63, [0; 32], 0, 0),
            Err(ControlHeaderError::InvalidTotalSize)
        );
        assert_eq!(
            ControlHeader::new(0, 64, [0; 32], 64, 0),
            Err(ControlHeaderError::InvalidCapabilityTable)
        );

        let base = fallback().encode();
        for (offset, value, error) in [
            (0, 0, ControlHeaderError::InvalidMagic),
            (8, 2, ControlHeaderError::UnsupportedMajor),
            (12, 63, ControlHeaderError::InvalidHeaderSize),
            (14, 1, ControlHeaderError::UndefinedFlags),
            (16, 63, ControlHeaderError::InvalidTotalSize),
            (56, 64, ControlHeaderError::InvalidCapabilityTable),
        ] {
            let mut invalid = base;
            invalid[offset] = value;
            assert_eq!(ControlHeader::decode(&invalid), Err(error));
        }
    }

    fn fallback() -> ControlHeader {
        ControlHeader {
            layout_minor: 0,
            total_size: 64,
            schema_hash: [0; 32],
            capability_table_offset: 0,
            capability_count: 0,
        }
    }
}
