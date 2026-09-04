//! Version marker for the R0 execution contract.

use aurora_types::{ContractLifecycle, ContractVersion};

use crate::ExecutionContractError;

/// The validated Preview 1.0 R0 execution-contract version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExecutionContractVersion;

impl ExecutionContractVersion {
    /// The only version accepted by the R0 implementation.
    pub const V1_0: Self = Self;

    /// Validates a generic contract version at a boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::UnsupportedContractVersion`] unless
    /// `value` is exactly Preview 1.0.
    pub const fn try_from_contract(value: ContractVersion) -> Result<Self, ExecutionContractError> {
        if value.major() == 1
            && value.minor() == 0
            && matches!(value.lifecycle(), ContractLifecycle::Preview)
        {
            Ok(Self)
        } else {
            Err(ExecutionContractError::UnsupportedContractVersion)
        }
    }

    /// Validates a required version from a nullable wire boundary.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::MissingContractVersion`] when absent,
    /// or [`ExecutionContractError::UnsupportedContractVersion`] when unknown.
    pub const fn try_from_optional_contract(
        value: Option<ContractVersion>,
    ) -> Result<Self, ExecutionContractError> {
        match value {
            Some(version) => Self::try_from_contract(version),
            None => Err(ExecutionContractError::MissingContractVersion),
        }
    }

    /// Returns the independently versioned contract representation.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::UnsupportedContractVersion`] if the
    /// shared version type ever rejects the statically defined version.
    pub const fn try_as_contract_version(self) -> Result<ContractVersion, ExecutionContractError> {
        match ContractVersion::new(1, 0, ContractLifecycle::Preview) {
            Ok(version) => Ok(version),
            Err(_) => Err(ExecutionContractError::UnsupportedContractVersion),
        }
    }
}

#[cfg(test)]
mod tests {
    use aurora_types::{ContractLifecycle, ContractVersion};

    use super::ExecutionContractVersion;
    use crate::ExecutionContractError;

    #[test]
    fn accepts_only_present_preview_v1_0() {
        let preview = ContractVersion::new(1, 0, ContractLifecycle::Preview);
        assert_eq!(
            preview.map(ExecutionContractVersion::try_from_contract),
            Ok(Ok(ExecutionContractVersion::V1_0))
        );
        assert_eq!(
            ExecutionContractVersion::try_from_optional_contract(None),
            Err(ExecutionContractError::MissingContractVersion)
        );

        for unsupported in [
            ContractVersion::new(1, 1, ContractLifecycle::Preview),
            ContractVersion::new(2, 0, ContractLifecycle::Preview),
            ContractVersion::new(1, 0, ContractLifecycle::Stable),
        ] {
            assert_eq!(
                unsupported.map(ExecutionContractVersion::try_from_contract),
                Ok(Err(ExecutionContractError::UnsupportedContractVersion))
            );
        }
    }

    #[test]
    fn exposes_the_exact_contract_version() {
        let version = ExecutionContractVersion::V1_0.try_as_contract_version();
        assert_eq!(version.map(ContractVersion::major), Ok(1));
        assert_eq!(version.map(ContractVersion::minor), Ok(0));
        assert_eq!(
            version.map(ContractVersion::lifecycle),
            Ok(ContractLifecycle::Preview)
        );
    }
}
