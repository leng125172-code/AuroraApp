//! Stable Aurora quality-code representation.

use thiserror::Error;

/// High-level validity of a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum QualitySeverity {
    /// The value is valid for normal use.
    Good = 0,
    /// The value is usable only with its detailed reason and flags.
    Uncertain = 1,
    /// The value is invalid for normal use.
    Bad = 2,
}

/// Orthogonal quality annotations encoded in the low eight bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QualityFlags(u8);

impl QualityFlags {
    /// No additional annotations.
    pub const NONE: Self = Self(0);
    /// The value was substituted by a defined fallback source.
    pub const SUBSTITUTED: Self = Self(1 << 0);
    /// The value is older than its configured freshness limit.
    pub const STALE: Self = Self(1 << 1);
    /// An active force lease supplied the value.
    pub const FORCED: Self = Self(1 << 2);
    /// A simulator supplied the value.
    pub const SIMULATED: Self = Self(1 << 3);
    /// The value is at or below its configured low limit.
    pub const LIMIT_LOW: Self = Self(1 << 4);
    /// The value is at or above its configured high limit.
    pub const LIMIT_HIGH: Self = Self(1 << 5);

    /// Creates flags from their stable bit representation.
    #[must_use]
    pub const fn from_bits(bits: u8) -> Self {
        Self(bits)
    }

    /// Returns the stable bit representation.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// Error returned for reserved or out-of-range quality-code fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum QualityCodeError {
    /// The six-bit domain field was exceeded.
    #[error("quality domain must be in the range 0..=63")]
    DomainOutOfRange,
    /// The encoded severity uses the reserved value.
    #[error("quality severity value 3 is reserved")]
    ReservedSeverity,
}

/// Stable 32-bit quality code: severity:2, domain:6, reason:16, flags:8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QualityCode(u32);

impl QualityCode {
    /// Canonical good quality with no domain-specific reason or flags.
    pub const GOOD: Self = Self(0);

    /// Creates a quality code from structured fields.
    ///
    /// # Errors
    ///
    /// Returns [`QualityCodeError::DomainOutOfRange`] when `domain` does not fit
    /// the allocated six bits.
    pub const fn new(
        severity: QualitySeverity,
        domain: u8,
        reason: u16,
        flags: QualityFlags,
    ) -> Result<Self, QualityCodeError> {
        if domain > 0x3f {
            return Err(QualityCodeError::DomainOutOfRange);
        }
        let raw = ((severity as u32) << 30)
            | ((domain as u32) << 24)
            | ((reason as u32) << 8)
            | flags.bits() as u32;
        Ok(Self(raw))
    }

    /// Parses and validates a stable 32-bit quality code.
    ///
    /// # Errors
    ///
    /// Returns [`QualityCodeError::ReservedSeverity`] when the encoded severity
    /// is the reserved bit pattern `3`.
    pub const fn from_raw(raw: u32) -> Result<Self, QualityCodeError> {
        if raw >> 30 == 3 {
            Err(QualityCodeError::ReservedSeverity)
        } else {
            Ok(Self(raw))
        }
    }

    /// Returns the stable packed representation.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Returns the high-level severity.
    #[must_use]
    pub const fn severity(self) -> QualitySeverity {
        match self.0 >> 30 {
            0 => QualitySeverity::Good,
            1 => QualitySeverity::Uncertain,
            _ => QualitySeverity::Bad,
        }
    }

    /// Returns the six-bit reason domain.
    #[must_use]
    pub const fn domain(self) -> u8 {
        self.0.to_be_bytes()[0] & 0x3f
    }

    /// Returns the domain-local reason number.
    #[must_use]
    pub const fn reason(self) -> u16 {
        let bytes = self.0.to_be_bytes();
        u16::from_be_bytes([bytes[1], bytes[2]])
    }

    /// Returns orthogonal quality annotations.
    #[must_use]
    pub const fn flags(self) -> QualityFlags {
        QualityFlags::from_bits(self.0.to_be_bytes()[3])
    }
}

#[cfg(test)]
mod tests {
    use super::{QualityCode, QualityCodeError, QualityFlags, QualitySeverity};

    #[test]
    fn quality_code_round_trips_all_fields() {
        let code = QualityCode::new(QualitySeverity::Uncertain, 5, 0x1234, QualityFlags::STALE)
            .unwrap_or(QualityCode::GOOD);
        assert_eq!(code.severity(), QualitySeverity::Uncertain);
        assert_eq!(code.domain(), 5);
        assert_eq!(code.reason(), 0x1234);
        assert_eq!(code.flags(), QualityFlags::STALE);
        assert_eq!(code.raw(), 0x4512_3402);
    }

    #[test]
    fn quality_code_rejects_reserved_values() {
        assert_eq!(
            QualityCode::new(QualitySeverity::Good, 64, 0, QualityFlags::NONE),
            Err(QualityCodeError::DomainOutOfRange)
        );
        assert_eq!(
            QualityCode::from_raw(0xc000_0000),
            Err(QualityCodeError::ReservedSeverity)
        );
        assert_eq!(
            QualityCode::from_raw(0).map(QualityCode::severity),
            Ok(QualitySeverity::Good)
        );
        assert_eq!(
            QualityCode::from_raw(0x8000_0000).map(QualityCode::severity),
            Ok(QualitySeverity::Bad)
        );
        assert_eq!(QualityFlags::from_bits(0x3f).bits(), 0x3f);
    }
}
