//! Strong identifiers and payload-local handles.

use std::{fmt, str::FromStr};

use thiserror::Error;
use uuid::{Uuid, Version};

/// Error returned when an identifier or local handle violates its invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("identifier or local handle violates its required representation")]
pub struct IdentifierError;

macro_rules! define_identifier {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Generates a new time-ordered `UUIDv7` identifier.
            #[must_use]
            pub fn generate() -> Self {
                Self(Uuid::now_v7())
            }

            /// Creates the identifier from RFC 9562 network-order bytes.
            ///
            /// # Errors
            ///
            /// Returns [`IdentifierError`] when the bytes do not encode a
            /// `UUIDv7` value.
            pub fn from_bytes(bytes: [u8; 16]) -> Result<Self, IdentifierError> {
                let value = Uuid::from_bytes(bytes);
                if value.get_version() == Some(Version::SortRand) {
                    Ok(Self(value))
                } else {
                    Err(IdentifierError)
                }
            }

            /// Returns RFC 9562 network-order bytes.
            #[must_use]
            pub fn to_bytes(self) -> [u8; 16] {
                *self.0.as_bytes()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = IdentifierError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let parsed = Uuid::parse_str(value).map_err(|_| IdentifierError)?;
                Self::from_bytes(*parsed.as_bytes())
            }
        }
    };
}

define_identifier!(ProjectId, "Stable identity of an Aurora project.");
define_identifier!(DeviceId, "Stable identity of a managed target device.");
define_identifier!(DeploymentId, "Stable identity of a deployment attempt.");
define_identifier!(RequestId, "Stable identity used to deduplicate a request.");
define_identifier!(
    OperationId,
    "Stable identity of a recoverable hosted operation."
);
define_identifier!(TagId, "Stable identity of an Aurora tag.");
define_identifier!(BootEpochId, "Identity of one process or system boot epoch.");
define_identifier!(
    DocumentId,
    "Stable identity of a versioned contract document."
);

/// Dense handle that is valid only inside one resolved Payload or control layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalHandle(u32);

impl LocalHandle {
    /// First valid local handle.
    pub const ZERO: Self = Self(0);
    /// Sentinel value that is never a valid local handle.
    pub const INVALID_RAW: u32 = u32::MAX;

    /// Creates a handle, rejecting the reserved invalid sentinel.
    ///
    /// # Errors
    ///
    /// Returns [`IdentifierError`] when `value` equals [`Self::INVALID_RAW`].
    pub const fn new(value: u32) -> Result<Self, IdentifierError> {
        if value == Self::INVALID_RAW {
            Err(IdentifierError)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the compact integer representation.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{IdentifierError, LocalHandle, ProjectId};

    #[test]
    fn generated_identifier_round_trips_network_bytes() {
        let id = ProjectId::generate();
        assert_eq!(ProjectId::from_bytes(id.to_bytes()), Ok(id));
        assert_eq!(id.to_string().parse::<ProjectId>(), Ok(id));
    }

    #[test]
    fn rejects_non_v7_identifiers_and_invalid_handles() {
        assert_eq!(ProjectId::from_bytes([0; 16]), Err(IdentifierError));
        assert_eq!(ProjectId::from_str("not-a-uuid"), Err(IdentifierError));
        assert_eq!(LocalHandle::new(u32::MAX), Err(IdentifierError));
        assert_eq!(LocalHandle::new(7).map(LocalHandle::get), Ok(7));
    }
}
