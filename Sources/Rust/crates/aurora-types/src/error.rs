//! Stable cross-component error identifiers.

use thiserror::Error;

/// Error returned for invalid domain/code combinations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("non-success error codes require non-zero domain and local code")]
pub struct ErrorCodeError;

/// Stable error identifier encoded as domain:u16 and local-code:u16.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ErrorCode(u32);

impl ErrorCode {
    /// Successful result sentinel.
    pub const OK: Self = Self(0);

    /// Creates a non-success error code.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCodeError`] when either component is zero; only [`Self::OK`]
    /// may use the zero domain and code.
    pub const fn new(domain: u16, code: u16) -> Result<Self, ErrorCodeError> {
        if domain == 0 || code == 0 {
            Err(ErrorCodeError)
        } else {
            Ok(Self(((domain as u32) << 16) | code as u32))
        }
    }

    /// Parses the stable packed representation.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorCodeError`] when exactly one of the domain or local-code
    /// components is zero.
    pub const fn from_raw(raw: u32) -> Result<Self, ErrorCodeError> {
        if raw == 0 || ((raw >> 16) != 0 && (raw & 0xffff) != 0) {
            Ok(Self(raw))
        } else {
            Err(ErrorCodeError)
        }
    }

    /// Returns the packed representation.
    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }

    /// Returns whether the code represents success.
    #[must_use]
    pub const fn is_ok(self) -> bool {
        self.0 == 0
    }

    /// Returns the domain number, or zero for success.
    #[must_use]
    pub const fn domain(self) -> u16 {
        let bytes = self.0.to_be_bytes();
        u16::from_be_bytes([bytes[0], bytes[1]])
    }

    /// Returns the domain-local number, or zero for success.
    #[must_use]
    pub const fn code(self) -> u16 {
        let bytes = self.0.to_be_bytes();
        u16::from_be_bytes([bytes[2], bytes[3]])
    }
}

/// Machine-readable retry guidance attached to an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum RetryClass {
    /// Retrying cannot succeed without changing the request or software.
    Never = 0,
    /// One immediate retry is safe.
    Immediate = 1,
    /// Retry only with bounded exponential backoff.
    Backoff = 2,
    /// Retry only after an operator or approval workflow changes external state.
    AfterOperatorAction = 3,
}

#[cfg(test)]
mod tests {
    use super::{ErrorCode, ErrorCodeError};

    #[test]
    fn error_codes_preserve_domain_and_local_number() {
        let code = ErrorCode::new(0x1234, 0x5678).unwrap_or(ErrorCode::OK);
        assert_eq!(code.raw(), 0x1234_5678);
        assert_eq!(code.domain(), 0x1234);
        assert_eq!(code.code(), 0x5678);
        assert!(!code.is_ok());
        assert!(ErrorCode::OK.is_ok());
    }

    #[test]
    fn only_zero_can_represent_success() {
        assert_eq!(ErrorCode::from_raw(0), Ok(ErrorCode::OK));
        assert_eq!(ErrorCode::new(0, 1), Err(ErrorCodeError));
        assert_eq!(ErrorCode::new(1, 0), Err(ErrorCodeError));
        assert_eq!(
            ErrorCode::from_raw(0x0001_0001).map(ErrorCode::raw),
            Ok(0x0001_0001)
        );
        assert_eq!(ErrorCode::from_raw(0x0000_0001), Err(ErrorCodeError));
        assert_eq!(ErrorCode::from_raw(0x0001_0000), Err(ErrorCodeError));
    }
}
