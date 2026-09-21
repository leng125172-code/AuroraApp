//! Non-zero generations and exact identities carried by Guardian sessions.

use crate::GuardianContractError;

macro_rules! non_zero_counter {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);

        impl $name {
            /// Creates a non-zero value.
            ///
            /// # Errors
            ///
            /// Returns [`GuardianContractError::ZeroIdentity`] when `value` is zero.
            pub const fn new(value: u64) -> Result<Self, GuardianContractError> {
                if value == 0 {
                    Err(GuardianContractError::ZeroIdentity)
                } else {
                    Ok(Self(value))
                }
            }

            /// Returns the encoded value.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }

            /// Returns the exact next value without wrapping.
            ///
            /// # Errors
            ///
            /// Returns [`GuardianContractError::CounterOverflow`] at `u64::MAX`.
            pub const fn checked_next(self) -> Result<Self, GuardianContractError> {
                match self.0.checked_add(1) {
                    Some(value) => Ok(Self(value)),
                    None => Err(GuardianContractError::CounterOverflow),
                }
            }
        }
    };
}

non_zero_counter!(GuardianEpoch, "One Guardian process lifetime.");
non_zero_counter!(
    ConfigurationGeneration,
    "One immutable Guardian configuration generation."
);
non_zero_counter!(
    LeaseSequence,
    "Monotonic lease sequence within one Guardian epoch."
);

/// Monotonic image sequence within one lease, bounded for `(sequence << 1) | slot` encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageSequence(u64);

impl ImageSequence {
    /// Maximum sequence that can be encoded without losing the slot bit.
    pub const MAX: u64 = (1_u64 << 63) - 1;

    /// Creates a non-zero, publish-token-safe sequence.
    ///
    /// # Errors
    ///
    /// Returns [`GuardianContractError::ZeroIdentity`] for zero and
    /// [`GuardianContractError::CounterOverflow`] above [`Self::MAX`].
    pub const fn new(value: u64) -> Result<Self, GuardianContractError> {
        if value == 0 {
            Err(GuardianContractError::ZeroIdentity)
        } else if value > Self::MAX {
            Err(GuardianContractError::CounterOverflow)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the encoded value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the exact next sequence without crossing the publish-token limit.
    ///
    /// # Errors
    ///
    /// Returns [`GuardianContractError::CounterOverflow`] at [`Self::MAX`].
    pub const fn checked_next(self) -> Result<Self, GuardianContractError> {
        if self.0 == Self::MAX {
            Err(GuardianContractError::CounterOverflow)
        } else {
            Ok(Self(self.0 + 1))
        }
    }
}

/// SHA-256 digest of the normalized active configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConfigurationDigest([u8; 32]);

impl ConfigurationDigest {
    /// Creates a digest from all 32 SHA-256 bytes.
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

/// SHA-256 digest of the selected shared-image layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LayoutDigest([u8; 32]);

impl LayoutDigest {
    /// Creates a digest from all 32 SHA-256 bytes.
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

/// Opaque non-zero lease identifier generated outside the cyclic path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeaseId([u8; 16]);

impl LeaseId {
    /// Creates a lease identifier.
    ///
    /// # Errors
    ///
    /// Returns [`GuardianContractError::ZeroIdentity`] for the all-zero value.
    pub const fn new(bytes: [u8; 16]) -> Result<Self, GuardianContractError> {
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                return Ok(Self(bytes));
            }
            index += 1;
        }
        Err(GuardianContractError::ZeroIdentity)
    }

    /// Returns the opaque bytes.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; 16] {
        self.0
    }
}

/// Exact immutable configuration identity used for negotiation and leases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GuardianConfiguration {
    epoch: GuardianEpoch,
    generation: ConfigurationGeneration,
    configuration_digest: ConfigurationDigest,
    layout_digest: LayoutDigest,
}

impl GuardianConfiguration {
    /// Creates an immutable configuration identity.
    #[must_use]
    pub const fn new(
        epoch: GuardianEpoch,
        generation: ConfigurationGeneration,
        configuration_digest: ConfigurationDigest,
        layout_digest: LayoutDigest,
    ) -> Self {
        Self {
            epoch,
            generation,
            configuration_digest,
            layout_digest,
        }
    }

    /// Returns the Guardian process epoch.
    #[must_use]
    pub const fn epoch(self) -> GuardianEpoch {
        self.epoch
    }

    /// Returns the immutable configuration generation.
    #[must_use]
    pub const fn generation(self) -> ConfigurationGeneration {
        self.generation
    }

    /// Returns the normalized configuration digest.
    #[must_use]
    pub const fn configuration_digest(self) -> ConfigurationDigest {
        self.configuration_digest
    }

    /// Returns the shared-layout digest.
    #[must_use]
    pub const fn layout_digest(self) -> LayoutDigest {
        self.layout_digest
    }
}

/// Complete identity that binds an operation to one active lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeaseIdentity {
    configuration: GuardianConfiguration,
    lease_id: LeaseId,
}

impl LeaseIdentity {
    /// Creates a lease identity bound to one immutable configuration.
    #[must_use]
    pub const fn new(configuration: GuardianConfiguration, lease_id: LeaseId) -> Self {
        Self {
            configuration,
            lease_id,
        }
    }

    /// Returns the immutable configuration identity.
    #[must_use]
    pub const fn configuration(self) -> GuardianConfiguration {
        self.configuration
    }

    /// Returns the opaque lease identifier.
    #[must_use]
    pub const fn lease_id(self) -> LeaseId {
        self.lease_id
    }
}

#[cfg(test)]
mod tests {
    use super::{GuardianEpoch, ImageSequence, LeaseId};
    use crate::GuardianContractError;

    #[test]
    fn non_zero_counters_reject_zero_and_never_wrap() {
        assert_eq!(
            GuardianEpoch::new(0),
            Err(GuardianContractError::ZeroIdentity)
        );
        assert_eq!(
            ImageSequence::new(ImageSequence::MAX).and_then(ImageSequence::checked_next),
            Err(GuardianContractError::CounterOverflow)
        );
        assert_eq!(
            ImageSequence::new(ImageSequence::MAX + 1),
            Err(GuardianContractError::CounterOverflow)
        );
    }

    #[test]
    fn lease_id_rejects_only_the_all_zero_value() {
        assert_eq!(
            LeaseId::new([0; 16]),
            Err(GuardianContractError::ZeroIdentity)
        );
        let mut bytes = [0; 16];
        bytes[15] = 1;
        assert_eq!(LeaseId::new(bytes).map(LeaseId::to_bytes), Ok(bytes));
    }
}
