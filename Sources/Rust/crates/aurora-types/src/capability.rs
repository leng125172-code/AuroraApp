//! Namespaced and versioned capability identifiers.

use std::{fmt, str::FromStr};

use thiserror::Error;

/// Error returned for a malformed capability identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("capability must use lowercase dot-separated segments followed by @<positive-major>")]
pub struct CapabilityIdError;

/// Versioned capability name such as `aurora.io.read@1`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapabilityId(String);

impl CapabilityId {
    /// Returns the canonical capability string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for CapabilityId {
    type Err = CapabilityIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some((name, major)) = value.rsplit_once('@') else {
            return Err(CapabilityIdError);
        };
        let valid_name = name.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && segment
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_lowercase)
        });
        let valid_major = major.parse::<u32>().is_ok_and(|number| number > 0);
        if valid_name && name.contains('.') && valid_major {
            Ok(Self(value.to_owned()))
        } else {
            Err(CapabilityIdError)
        }
    }
}

impl fmt::Display for CapabilityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[cfg(test)]
mod tests {
    use super::CapabilityId;

    #[test]
    fn accepts_only_canonical_capability_ids() {
        let valid = "aurora.io.read@1".parse::<CapabilityId>();
        assert_eq!(
            valid.as_ref().map(CapabilityId::as_str),
            Ok("aurora.io.read@1")
        );
        assert_eq!(
            valid.map(|value| value.to_string()),
            Ok("aurora.io.read@1".to_owned())
        );

        for invalid in [
            "aurora.io.read",
            "Aurora.IO.Read@1",
            "aurora..read@1",
            "aurora.1read@1",
            "aurora_read@1",
            "aurora@1",
            "aurora.io.read@0",
            "aurora.io.read@4294967296",
        ] {
            assert!(
                invalid.parse::<CapabilityId>().is_err(),
                "accepted {invalid}"
            );
        }
    }
}
