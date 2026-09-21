//! Platform-neutral descriptors for the Linux UDS and sealed-memfd boundary.

use crate::{GuardianContractError, LeaseIdentity};

/// Credentials observed for one local Unix-domain-socket peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerCredentials {
    pid: u32,
    uid: u32,
    gid: u32,
    service_identity: [u8; 32],
}

impl PeerCredentials {
    /// Creates observed peer credentials.
    ///
    /// `service_identity` is the fixed digest of the approved systemd service/cgroup identity.
    /// PID associates one connection but is not an authorization anchor.
    ///
    /// # Errors
    ///
    /// Returns [`GuardianContractError::ZeroIdentity`] when PID is zero.
    pub const fn new(
        pid: u32,
        uid: u32,
        gid: u32,
        service_identity: [u8; 32],
    ) -> Result<Self, GuardianContractError> {
        if pid == 0 {
            return Err(GuardianContractError::ZeroIdentity);
        }
        Ok(Self {
            pid,
            uid,
            gid,
            service_identity,
        })
    }

    /// Returns the connection-associated PID.
    #[must_use]
    pub const fn pid(self) -> u32 {
        self.pid
    }

    /// Returns the observed UID.
    #[must_use]
    pub const fn uid(self) -> u32 {
        self.uid
    }

    /// Returns the observed GID.
    #[must_use]
    pub const fn gid(self) -> u32 {
        self.gid
    }

    /// Returns the approved service/cgroup identity digest.
    #[must_use]
    pub const fn service_identity(self) -> [u8; 32] {
        self.service_identity
    }
}

/// Exact local peer authorization policy configured before connection acceptance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerPolicy {
    uid: u32,
    gid: u32,
    service_identity: [u8; 32],
}

impl PeerPolicy {
    /// Creates an exact UID/GID and service/cgroup policy.
    #[must_use]
    pub const fn new(uid: u32, gid: u32, service_identity: [u8; 32]) -> Self {
        Self {
            uid,
            gid,
            service_identity,
        }
    }

    /// Validates observed credentials without treating PID as a trust anchor.
    ///
    /// # Errors
    ///
    /// Returns [`GuardianContractError::PeerIdentityMismatch`] when UID, GID, or the approved
    /// service/cgroup identity differs.
    pub fn validate(self, credentials: PeerCredentials) -> Result<(), GuardianContractError> {
        if self.uid == credentials.uid
            && self.gid == credentials.gid
            && self.service_identity == credentials.service_identity
        {
            Ok(())
        } else {
            Err(GuardianContractError::PeerIdentityMismatch)
        }
    }
}

/// Fixed set of Linux memfd seals admitted by Preview 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MemfdSealSet(u8);

impl MemfdSealSet {
    /// `F_SEAL_SEAL` representation in the platform adapter.
    pub const SEAL: Self = Self(1 << 0);
    /// `F_SEAL_SHRINK` representation in the platform adapter.
    pub const SHRINK: Self = Self(1 << 1);
    /// `F_SEAL_GROW` representation in the platform adapter.
    pub const GROW: Self = Self(1 << 2);
    /// Exact seals required before passing the shared-region descriptor.
    pub const REQUIRED: Self = Self(Self::SEAL.0 | Self::SHRINK.0 | Self::GROW.0);

    /// Creates a seal set from the fixed contract representation.
    ///
    /// # Errors
    ///
    /// Returns [`GuardianContractError::InvalidSharedRegionOffer`] for undefined bits.
    pub const fn from_raw_bits(bits: u8) -> Result<Self, GuardianContractError> {
        if bits & !Self::REQUIRED.0 == 0 {
            Ok(Self(bits))
        } else {
            Err(GuardianContractError::InvalidSharedRegionOffer)
        }
    }

    /// Returns the fixed contract representation.
    #[must_use]
    pub const fn raw_bits(self) -> u8 {
        self.0
    }
}

/// Validated descriptor for one fixed-size, per-lease shared region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SharedRegionOffer {
    byte_length: u64,
    seals: MemfdSealSet,
    lease_identity: LeaseIdentity,
}

impl SharedRegionOffer {
    /// Creates a region descriptor without opening, mapping, or transferring an fd.
    ///
    /// # Errors
    ///
    /// Rejects zero or non-64-byte-aligned lengths and any seal set other than exactly
    /// `F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL`.
    pub const fn new(
        byte_length: u64,
        seals: MemfdSealSet,
        lease_identity: LeaseIdentity,
    ) -> Result<Self, GuardianContractError> {
        if byte_length == 0
            || !byte_length.is_multiple_of(64)
            || seals.0 != MemfdSealSet::REQUIRED.0
        {
            return Err(GuardianContractError::InvalidSharedRegionOffer);
        }
        Ok(Self {
            byte_length,
            seals,
            lease_identity,
        })
    }

    /// Returns the fixed region length in bytes.
    #[must_use]
    pub const fn byte_length(self) -> u64 {
        self.byte_length
    }

    /// Returns the exact required seal set.
    #[must_use]
    pub const fn seals(self) -> MemfdSealSet {
        self.seals
    }

    /// Returns the lease identity that owns this one-use mapping.
    #[must_use]
    pub const fn lease_identity(self) -> LeaseIdentity {
        self.lease_identity
    }
}

#[cfg(test)]
mod tests {
    use super::{MemfdSealSet, PeerCredentials, PeerPolicy};
    use crate::GuardianContractError;

    #[test]
    fn peer_policy_uses_uid_gid_and_service_identity_but_not_pid() {
        let policy = PeerPolicy::new(100, 200, [3; 32]);
        let first = PeerCredentials::new(10, 100, 200, [3; 32]);
        let second = PeerCredentials::new(11, 100, 200, [3; 32]);
        assert!(first.is_ok_and(|value| policy.validate(value).is_ok()));
        assert!(second.is_ok_and(|value| policy.validate(value).is_ok()));
        let wrong_uid = PeerCredentials::new(12, 101, 200, [3; 32]);
        assert_eq!(
            wrong_uid.and_then(|value| policy.validate(value)),
            Err(GuardianContractError::PeerIdentityMismatch)
        );
    }

    #[test]
    fn memfd_seal_set_rejects_undefined_bits() {
        assert_eq!(
            MemfdSealSet::from_raw_bits(8),
            Err(GuardianContractError::InvalidSharedRegionOffer)
        );
        assert_eq!(
            MemfdSealSet::from_raw_bits(MemfdSealSet::REQUIRED.raw_bits()),
            Ok(MemfdSealSet::REQUIRED)
        );
    }
}
