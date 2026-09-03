//! Product and contract version types.

use std::{fmt, str::FromStr};

use semver::Version;
use thiserror::Error;

/// Version parsing or invariant error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VersionError {
    /// A semantic version string was invalid.
    #[error("invalid semantic version")]
    InvalidSemanticVersion,
    /// Contract major version zero is reserved.
    #[error("contract major version must be greater than zero")]
    InvalidContractMajor,
}

/// Semantic product or package version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SemanticVersion(Version);

impl SemanticVersion {
    /// Returns the semantic-version major component.
    #[must_use]
    pub const fn major(&self) -> u64 {
        self.0.major
    }

    /// Returns the semantic-version minor component.
    #[must_use]
    pub const fn minor(&self) -> u64 {
        self.0.minor
    }

    /// Returns the semantic-version patch component.
    #[must_use]
    pub const fn patch(&self) -> u64 {
        self.0.patch
    }

    /// Returns the prerelease component without the leading separator.
    #[must_use]
    pub fn pre_release(&self) -> &str {
        self.0.pre.as_str()
    }

    /// Returns the build metadata component without the leading separator.
    #[must_use]
    pub fn build_metadata(&self) -> &str {
        self.0.build.as_str()
    }
}

impl FromStr for SemanticVersion {
    type Err = VersionError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Version::parse(value)
            .map(Self)
            .map_err(|_| VersionError::InvalidSemanticVersion)
    }
}

impl fmt::Display for SemanticVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Publication lifecycle of a contract version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ContractLifecycle {
    /// Breaking changes require migration evidence but are allowed between previews.
    Preview = 0,
    /// Same-major changes must remain backward compatible.
    Stable = 1,
}

/// Independently versioned public contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ContractVersion {
    major: u32,
    minor: u32,
    lifecycle: ContractLifecycle,
}

impl ContractVersion {
    /// Creates a contract version with a non-zero major component.
    ///
    /// # Errors
    ///
    /// Returns [`VersionError::InvalidContractMajor`] when `major` is zero.
    pub const fn new(
        major: u32,
        minor: u32,
        lifecycle: ContractLifecycle,
    ) -> Result<Self, VersionError> {
        if major == 0 {
            Err(VersionError::InvalidContractMajor)
        } else {
            Ok(Self {
                major,
                minor,
                lifecycle,
            })
        }
    }

    /// Returns the breaking-change generation.
    #[must_use]
    pub const fn major(self) -> u32 {
        self.major
    }

    /// Returns the compatible additive revision.
    #[must_use]
    pub const fn minor(self) -> u32 {
        self.minor
    }

    /// Returns the publication lifecycle.
    #[must_use]
    pub const fn lifecycle(self) -> ContractLifecycle {
        self.lifecycle
    }
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::{ContractLifecycle, ContractVersion, SemanticVersion, VersionError};

    #[test]
    fn semantic_versions_use_standard_precedence() {
        let preview = "1.2.3-alpha.1"
            .parse::<SemanticVersion>()
            .unwrap_or(SemanticVersion(Version::new(0, 0, 0)));
        let stable = SemanticVersion(Version::new(1, 2, 3));
        assert!(preview < stable);
        assert_eq!(stable.major(), 1);
        assert_eq!(stable.minor(), 2);
        assert_eq!(stable.patch(), 3);
        assert_eq!(preview.pre_release(), "alpha.1");
        assert_eq!(stable.build_metadata(), "");
        assert_eq!(stable.to_string(), "1.2.3");
        assert_eq!(
            "invalid".parse::<SemanticVersion>(),
            Err(VersionError::InvalidSemanticVersion)
        );
    }

    #[test]
    fn contract_major_zero_is_rejected() {
        assert_eq!(
            ContractVersion::new(0, 1, ContractLifecycle::Preview),
            Err(VersionError::InvalidContractMajor)
        );
        let version =
            ContractVersion::new(1, 7, ContractLifecycle::Stable).unwrap_or(ContractVersion {
                major: 1,
                minor: 0,
                lifecycle: ContractLifecycle::Preview,
            });
        assert_eq!(version.major(), 1);
        assert_eq!(version.minor(), 7);
        assert_eq!(version.lifecycle(), ContractLifecycle::Stable);
    }
}
