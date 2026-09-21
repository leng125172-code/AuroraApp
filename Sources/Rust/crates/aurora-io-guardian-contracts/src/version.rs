//! Bidirectional N/N-1 contract, layout, and capability negotiation.

use crate::{CapabilitySet, GuardianContractError};

/// One side's bounded Guardian contract offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContractOffer {
    contract_major: u16,
    contract_min_minor: u16,
    contract_max_minor: u16,
    layout_major: u16,
    layout_min_minor: u16,
    layout_max_minor: u16,
    offered_capabilities: CapabilitySet,
    required_capabilities: CapabilitySet,
}

impl ContractOffer {
    /// Creates one offer whose contract and layout ranges contain at most N/N-1.
    ///
    /// # Errors
    ///
    /// Rejects zero majors, reversed or wider ranges, a missing session base, and requirements
    /// that are not offered by the same endpoint.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        contract_major: u16,
        contract_min_minor: u16,
        contract_max_minor: u16,
        layout_major: u16,
        layout_min_minor: u16,
        layout_max_minor: u16,
        offered_capabilities: CapabilitySet,
        required_capabilities: CapabilitySet,
    ) -> Result<Self, GuardianContractError> {
        if contract_major == 0 || layout_major == 0 {
            return Err(GuardianContractError::ZeroIdentity);
        }
        if !is_nn_minus_one_range(contract_min_minor, contract_max_minor)
            || !is_nn_minus_one_range(layout_min_minor, layout_max_minor)
        {
            return Err(GuardianContractError::InvalidVersionRange);
        }
        if !offered_capabilities.contains_all(CapabilitySet::SESSION_BASE)
            || !offered_capabilities.contains_all(required_capabilities)
        {
            return Err(GuardianContractError::RequiredCapabilityUnavailable);
        }
        Ok(Self {
            contract_major,
            contract_min_minor,
            contract_max_minor,
            layout_major,
            layout_min_minor,
            layout_max_minor,
            offered_capabilities,
            required_capabilities,
        })
    }

    /// Returns the contract major.
    #[must_use]
    pub const fn contract_major(self) -> u16 {
        self.contract_major
    }

    /// Returns the inclusive minimum contract minor.
    #[must_use]
    pub const fn contract_min_minor(self) -> u16 {
        self.contract_min_minor
    }

    /// Returns the inclusive maximum contract minor.
    #[must_use]
    pub const fn contract_max_minor(self) -> u16 {
        self.contract_max_minor
    }

    /// Returns the image-layout major.
    #[must_use]
    pub const fn layout_major(self) -> u16 {
        self.layout_major
    }

    /// Returns the inclusive minimum image-layout minor.
    #[must_use]
    pub const fn layout_min_minor(self) -> u16 {
        self.layout_min_minor
    }

    /// Returns the inclusive maximum image-layout minor.
    #[must_use]
    pub const fn layout_max_minor(self) -> u16 {
        self.layout_max_minor
    }

    /// Returns all capabilities this endpoint can provide in this build.
    #[must_use]
    pub const fn offered_capabilities(self) -> CapabilitySet {
        self.offered_capabilities
    }

    /// Returns the capabilities this endpoint requires for the session.
    #[must_use]
    pub const fn required_capabilities(self) -> CapabilitySet {
        self.required_capabilities
    }
}

/// Exact result selected by successful bidirectional negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NegotiatedContract {
    contract_major: u16,
    contract_minor: u16,
    layout_major: u16,
    layout_minor: u16,
    capabilities: CapabilitySet,
}

impl NegotiatedContract {
    /// Returns the selected contract major.
    #[must_use]
    pub const fn contract_major(self) -> u16 {
        self.contract_major
    }

    /// Returns the highest mutually supported contract minor.
    #[must_use]
    pub const fn contract_minor(self) -> u16 {
        self.contract_minor
    }

    /// Returns the selected image-layout major.
    #[must_use]
    pub const fn layout_major(self) -> u16 {
        self.layout_major
    }

    /// Returns the highest mutually supported image-layout minor.
    #[must_use]
    pub const fn layout_minor(self) -> u16 {
        self.layout_minor
    }

    /// Returns the exact mutually available capability set.
    #[must_use]
    pub const fn capabilities(self) -> CapabilitySet {
        self.capabilities
    }
}

/// Selects the highest common N/N-1 versions and the exact capability intersection.
///
/// Both Control and Guardian are symmetric participants: either side may be N while the other is
/// N-1. A requirement from either side must exist in the mutual capability intersection.
///
/// # Errors
///
/// Returns a typed rejection for incompatible contract/layout versions or unavailable required
/// capabilities.
pub const fn negotiate(
    control: ContractOffer,
    guardian: ContractOffer,
) -> Result<NegotiatedContract, GuardianContractError> {
    if control.contract_major != guardian.contract_major {
        return Err(GuardianContractError::UnsupportedContractVersion);
    }
    if control.layout_major != guardian.layout_major {
        return Err(GuardianContractError::UnsupportedLayoutVersion);
    }
    let contract_min = max_u16(control.contract_min_minor, guardian.contract_min_minor);
    let contract_max = min_u16(control.contract_max_minor, guardian.contract_max_minor);
    if contract_min > contract_max {
        return Err(GuardianContractError::UnsupportedContractVersion);
    }
    let layout_min = max_u16(control.layout_min_minor, guardian.layout_min_minor);
    let layout_max = min_u16(control.layout_max_minor, guardian.layout_max_minor);
    if layout_min > layout_max {
        return Err(GuardianContractError::UnsupportedLayoutVersion);
    }
    let capabilities = control
        .offered_capabilities
        .intersection(guardian.offered_capabilities);
    let required = control
        .required_capabilities
        .union(guardian.required_capabilities);
    if !capabilities.contains_all(required) {
        return Err(GuardianContractError::RequiredCapabilityUnavailable);
    }
    Ok(NegotiatedContract {
        contract_major: control.contract_major,
        contract_minor: contract_max,
        layout_major: control.layout_major,
        layout_minor: layout_max,
        capabilities,
    })
}

const fn is_nn_minus_one_range(minimum: u16, maximum: u16) -> bool {
    minimum <= maximum && maximum - minimum <= 1
}

const fn min_u16(left: u16, right: u16) -> u16 {
    if left < right { left } else { right }
}

const fn max_u16(left: u16, right: u16) -> u16 {
    if left > right { left } else { right }
}

#[cfg(test)]
mod tests {
    use super::{ContractOffer, negotiate};
    use crate::{CapabilitySet, GuardianContractError, IoCapability};

    fn set(entries: &[IoCapability]) -> CapabilitySet {
        match CapabilitySet::from_ordered(entries) {
            Ok(value) => value,
            Err(error) => unreachable!("test capability set must be canonical: {error}"),
        }
    }

    fn offer(minor_min: u16, minor_max: u16, offered: CapabilitySet) -> ContractOffer {
        match ContractOffer::new(
            1,
            minor_min,
            minor_max,
            1,
            minor_min,
            minor_max,
            offered,
            CapabilitySet::SESSION_BASE,
        ) {
            Ok(value) => value,
            Err(error) => unreachable!("test offer must be valid: {error}"),
        }
    }

    #[test]
    fn negotiation_is_bidirectional_and_selects_highest_common_minor() {
        let base = set(&[IoCapability::Guardian, IoCapability::Image]);
        let newer_control = negotiate(offer(1, 2, base), offer(1, 1, base));
        let newer_guardian = negotiate(offer(1, 1, base), offer(1, 2, base));
        assert!(newer_control.is_ok_and(|value| value.contract_minor() == 1));
        assert!(newer_guardian.is_ok_and(|value| value.contract_minor() == 1));
    }

    #[test]
    fn offer_rejects_wider_than_nn_minus_one_and_missing_requirement() {
        let base = set(&[IoCapability::Guardian, IoCapability::Image]);
        assert_eq!(
            ContractOffer::new(1, 0, 2, 1, 0, 1, base, base),
            Err(GuardianContractError::InvalidVersionRange)
        );
        let required = set(&[
            IoCapability::Guardian,
            IoCapability::Image,
            IoCapability::Serial,
        ]);
        assert_eq!(
            ContractOffer::new(1, 0, 1, 1, 0, 1, base, required),
            Err(GuardianContractError::RequiredCapabilityUnavailable)
        );
    }
}
