//! Versioned wire contracts and explicit domain conversions shared by Aurora processes and SDKs.

mod control_header;

pub use control_header::{ControlHeader, ControlHeaderError};

use std::str::FromStr;

use aurora_types::{
    BootEpochId, CapabilityId, ContractLifecycle, ContractVersion, DurationNanos, ErrorCode,
    MonotonicTimestamp, QualityCode, RetryClass, SemanticVersion, TimeQuality, TimeQualityState,
    TimeSource, UtcTimestamp,
};
use thiserror::Error;

/// Generated `aurora.common.v1` Protocol Buffer types.
// Generated field names and documentation are controlled by protoc; handwritten public APIs
// remain subject to the workspace missing-docs lint.
#[allow(missing_docs, clippy::doc_markdown, clippy::must_use_candidate)]
pub mod common_v1 {
    include!(concat!(env!("OUT_DIR"), "/aurora.common.v1.rs"));
}

/// Error returned when a wire DTO violates a domain invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WireConversionError {
    /// UUID bytes are absent, have the wrong length, or are not `UUIDv7`.
    #[error("wire UUID must contain exactly 16 canonical RFC 9562 UUIDv7 bytes")]
    InvalidUuid,
    /// Timestamp nanoseconds are not normalized.
    #[error("wire UTC nanoseconds must be less than 1,000,000,000")]
    InvalidTimestamp,
    /// A required nested message is absent.
    #[error("required nested wire message is absent")]
    MissingValue,
    /// A wire enum is unknown or uses its unspecified sentinel.
    #[error("wire enum value is unknown or unspecified")]
    InvalidEnum,
    /// A packed quality code violates a reserved-field invariant.
    #[error("wire quality code is invalid")]
    InvalidQualityCode,
    /// A packed error code violates its domain/code invariant.
    #[error("wire error code is invalid")]
    InvalidErrorCode,
    /// A semantic or contract version violates its domain invariant.
    #[error("wire version is invalid")]
    InvalidVersion,
    /// A capability identifier is not in canonical namespaced form.
    #[error("wire capability identifier is invalid")]
    InvalidCapability,
}

/// Encodes a boot epoch as a network-order UUID byte message.
#[must_use]
pub fn encode_boot_epoch(value: BootEpochId) -> common_v1::UuidValue {
    common_v1::UuidValue {
        value: value.to_bytes().to_vec(),
    }
}

/// Decodes and validates a boot epoch UUID.
///
/// # Errors
///
/// Returns [`WireConversionError::InvalidUuid`] when the byte length or UUID
/// version is invalid.
pub fn decode_boot_epoch(value: &common_v1::UuidValue) -> Result<BootEpochId, WireConversionError> {
    let bytes: [u8; 16] = value
        .value
        .as_slice()
        .try_into()
        .map_err(|_| WireConversionError::InvalidUuid)?;
    BootEpochId::from_bytes(bytes).map_err(|_| WireConversionError::InvalidUuid)
}

/// Encodes a normalized UTC timestamp.
#[must_use]
pub const fn encode_utc(value: UtcTimestamp) -> common_v1::UtcTimestamp {
    common_v1::UtcTimestamp {
        seconds: value.seconds(),
        nanos: value.nanos(),
    }
}

/// Decodes and validates a UTC timestamp.
///
/// # Errors
///
/// Returns [`WireConversionError::InvalidTimestamp`] when nanoseconds are not
/// normalized.
pub const fn decode_utc(
    value: &common_v1::UtcTimestamp,
) -> Result<UtcTimestamp, WireConversionError> {
    match UtcTimestamp::new(value.seconds, value.nanos) {
        Ok(timestamp) => Ok(timestamp),
        Err(_) => Err(WireConversionError::InvalidTimestamp),
    }
}

/// Encodes a monotonic timestamp with its boot epoch.
#[must_use]
pub fn encode_monotonic(value: MonotonicTimestamp) -> common_v1::MonotonicTimestamp {
    common_v1::MonotonicTimestamp {
        boot_epoch_id: Some(encode_boot_epoch(value.boot_epoch())),
        elapsed_nanos: value.elapsed_nanos(),
    }
}

/// Decodes a monotonic timestamp and rejects a missing or invalid boot epoch.
///
/// # Errors
///
/// Returns [`WireConversionError::MissingValue`] for an absent boot epoch, or
/// [`WireConversionError::InvalidUuid`] for an invalid epoch identifier.
pub fn decode_monotonic(
    value: &common_v1::MonotonicTimestamp,
) -> Result<MonotonicTimestamp, WireConversionError> {
    let epoch = value
        .boot_epoch_id
        .as_ref()
        .ok_or(WireConversionError::MissingValue)
        .and_then(decode_boot_epoch)?;
    Ok(MonotonicTimestamp::new(epoch, value.elapsed_nanos))
}

/// Encodes a non-negative duration.
#[must_use]
pub const fn encode_duration(value: DurationNanos) -> common_v1::DurationNanos {
    common_v1::DurationNanos { value: value.get() }
}

/// Decodes a non-negative duration.
#[must_use]
pub const fn decode_duration(value: &common_v1::DurationNanos) -> DurationNanos {
    DurationNanos::new(value.value)
}

/// Encodes the stable quality-code representation.
#[must_use]
pub const fn encode_quality_code(value: QualityCode) -> common_v1::QualityCode {
    common_v1::QualityCode { value: value.raw() }
}

/// Decodes and validates a packed quality code.
///
/// # Errors
///
/// Returns [`WireConversionError::InvalidQualityCode`] for a reserved severity.
pub fn decode_quality_code(
    value: &common_v1::QualityCode,
) -> Result<QualityCode, WireConversionError> {
    QualityCode::from_raw(value.value).map_err(|_| WireConversionError::InvalidQualityCode)
}

/// Encodes the stable error-code representation.
#[must_use]
pub const fn encode_error_code(value: ErrorCode) -> common_v1::ErrorCode {
    common_v1::ErrorCode { value: value.raw() }
}

/// Decodes and validates a packed error code.
///
/// # Errors
///
/// Returns [`WireConversionError::InvalidErrorCode`] when exactly one packed
/// component is zero.
pub fn decode_error_code(value: &common_v1::ErrorCode) -> Result<ErrorCode, WireConversionError> {
    ErrorCode::from_raw(value.value).map_err(|_| WireConversionError::InvalidErrorCode)
}

/// Encodes retry guidance without relying on matching numeric discriminants.
#[must_use]
pub const fn encode_retry_class(value: RetryClass) -> common_v1::RetryClass {
    match value {
        RetryClass::Never => common_v1::RetryClass::Never,
        RetryClass::Immediate => common_v1::RetryClass::Immediate,
        RetryClass::Backoff => common_v1::RetryClass::Backoff,
        RetryClass::AfterOperatorAction => common_v1::RetryClass::AfterOperatorAction,
    }
}

/// Decodes retry guidance and rejects unknown or unspecified wire values.
///
/// # Errors
///
/// Returns [`WireConversionError::InvalidEnum`] for an unknown or unspecified value.
pub fn decode_retry_class(value: i32) -> Result<RetryClass, WireConversionError> {
    match common_v1::RetryClass::try_from(value) {
        Ok(common_v1::RetryClass::Never) => Ok(RetryClass::Never),
        Ok(common_v1::RetryClass::Immediate) => Ok(RetryClass::Immediate),
        Ok(common_v1::RetryClass::Backoff) => Ok(RetryClass::Backoff),
        Ok(common_v1::RetryClass::AfterOperatorAction) => Ok(RetryClass::AfterOperatorAction),
        Ok(common_v1::RetryClass::Unspecified) | Err(_) => Err(WireConversionError::InvalidEnum),
    }
}

/// Encodes absolute-time confidence and synchronization evidence.
#[must_use]
pub fn encode_time_quality(value: TimeQuality) -> common_v1::TimeQuality {
    common_v1::TimeQuality {
        state: encode_time_quality_state(value.state()) as i32,
        source: encode_time_source(value.source()) as i32,
        max_error_nanos: value.max_error_nanos(),
        last_sync_utc: value.last_sync_utc().map(encode_utc),
    }
}

/// Decodes absolute-time confidence and validates every nested field.
///
/// # Errors
///
/// Returns [`WireConversionError::InvalidEnum`] for invalid enum values or
/// [`WireConversionError::InvalidTimestamp`] for invalid synchronization time.
pub fn decode_time_quality(
    value: &common_v1::TimeQuality,
) -> Result<TimeQuality, WireConversionError> {
    let state = decode_time_quality_state(value.state)?;
    let source = decode_time_source(value.source)?;
    let last_sync_utc = value.last_sync_utc.as_ref().map(decode_utc).transpose()?;
    Ok(TimeQuality::new(
        state,
        source,
        value.max_error_nanos,
        last_sync_utc,
    ))
}

/// Encodes a semantic product or package version.
#[must_use]
pub fn encode_semantic_version(value: &SemanticVersion) -> common_v1::SemanticVersion {
    common_v1::SemanticVersion {
        major: value.major(),
        minor: value.minor(),
        patch: value.patch(),
        pre_release: value.pre_release().to_owned(),
        build_metadata: value.build_metadata().to_owned(),
    }
}

/// Decodes a semantic version using the canonical `SemVer` parser.
///
/// # Errors
///
/// Returns [`WireConversionError::InvalidVersion`] for invalid prerelease or
/// build metadata.
pub fn decode_semantic_version(
    value: &common_v1::SemanticVersion,
) -> Result<SemanticVersion, WireConversionError> {
    let mut text = format!("{}.{}.{}", value.major, value.minor, value.patch);
    if !value.pre_release.is_empty() {
        text.push('-');
        text.push_str(&value.pre_release);
    }
    if !value.build_metadata.is_empty() {
        text.push('+');
        text.push_str(&value.build_metadata);
    }
    SemanticVersion::from_str(&text).map_err(|_| WireConversionError::InvalidVersion)
}

/// Encodes an independently versioned contract.
#[must_use]
pub const fn encode_contract_version(value: ContractVersion) -> common_v1::ContractVersion {
    common_v1::ContractVersion {
        major: value.major(),
        minor: value.minor(),
        lifecycle: encode_contract_lifecycle(value.lifecycle()) as i32,
    }
}

/// Decodes a contract version and rejects zero major or unspecified lifecycle.
///
/// # Errors
///
/// Returns [`WireConversionError::InvalidEnum`] for lifecycle errors and
/// [`WireConversionError::InvalidVersion`] for a zero major.
pub fn decode_contract_version(
    value: &common_v1::ContractVersion,
) -> Result<ContractVersion, WireConversionError> {
    let lifecycle = decode_contract_lifecycle(value.lifecycle)?;
    ContractVersion::new(value.major, value.minor, lifecycle)
        .map_err(|_| WireConversionError::InvalidVersion)
}

/// Encodes a canonical capability identifier.
#[must_use]
pub fn encode_capability(value: &CapabilityId) -> common_v1::CapabilityId {
    common_v1::CapabilityId {
        value: value.as_str().to_owned(),
    }
}

/// Decodes and validates a canonical capability identifier.
///
/// # Errors
///
/// Returns [`WireConversionError::InvalidCapability`] for malformed identifiers.
pub fn decode_capability(
    value: &common_v1::CapabilityId,
) -> Result<CapabilityId, WireConversionError> {
    CapabilityId::from_str(&value.value).map_err(|_| WireConversionError::InvalidCapability)
}

const fn encode_time_quality_state(value: TimeQualityState) -> common_v1::TimeQualityState {
    match value {
        TimeQualityState::Unknown => common_v1::TimeQualityState::Unknown,
        TimeQualityState::Synchronizing => common_v1::TimeQualityState::Synchronizing,
        TimeQualityState::Good => common_v1::TimeQualityState::Good,
        TimeQualityState::Holdover => common_v1::TimeQualityState::Holdover,
        TimeQualityState::Degraded => common_v1::TimeQualityState::Degraded,
        TimeQualityState::Invalid => common_v1::TimeQualityState::Invalid,
    }
}

fn decode_time_quality_state(value: i32) -> Result<TimeQualityState, WireConversionError> {
    match common_v1::TimeQualityState::try_from(value) {
        Ok(common_v1::TimeQualityState::Unknown) => Ok(TimeQualityState::Unknown),
        Ok(common_v1::TimeQualityState::Synchronizing) => Ok(TimeQualityState::Synchronizing),
        Ok(common_v1::TimeQualityState::Good) => Ok(TimeQualityState::Good),
        Ok(common_v1::TimeQualityState::Holdover) => Ok(TimeQualityState::Holdover),
        Ok(common_v1::TimeQualityState::Degraded) => Ok(TimeQualityState::Degraded),
        Ok(common_v1::TimeQualityState::Invalid) => Ok(TimeQualityState::Invalid),
        Ok(common_v1::TimeQualityState::Unspecified) | Err(_) => {
            Err(WireConversionError::InvalidEnum)
        }
    }
}

const fn encode_time_source(value: TimeSource) -> common_v1::TimeSource {
    match value {
        TimeSource::Unknown => common_v1::TimeSource::Unknown,
        TimeSource::System => common_v1::TimeSource::System,
        TimeSource::Ntp => common_v1::TimeSource::Ntp,
        TimeSource::Ptp => common_v1::TimeSource::Ptp,
        TimeSource::Gnss => common_v1::TimeSource::Gnss,
        TimeSource::Manual => common_v1::TimeSource::Manual,
    }
}

fn decode_time_source(value: i32) -> Result<TimeSource, WireConversionError> {
    match common_v1::TimeSource::try_from(value) {
        Ok(common_v1::TimeSource::Unknown) => Ok(TimeSource::Unknown),
        Ok(common_v1::TimeSource::System) => Ok(TimeSource::System),
        Ok(common_v1::TimeSource::Ntp) => Ok(TimeSource::Ntp),
        Ok(common_v1::TimeSource::Ptp) => Ok(TimeSource::Ptp),
        Ok(common_v1::TimeSource::Gnss) => Ok(TimeSource::Gnss),
        Ok(common_v1::TimeSource::Manual) => Ok(TimeSource::Manual),
        Ok(common_v1::TimeSource::Unspecified) | Err(_) => Err(WireConversionError::InvalidEnum),
    }
}

const fn encode_contract_lifecycle(value: ContractLifecycle) -> common_v1::ContractLifecycle {
    match value {
        ContractLifecycle::Preview => common_v1::ContractLifecycle::Preview,
        ContractLifecycle::Stable => common_v1::ContractLifecycle::Stable,
    }
}

fn decode_contract_lifecycle(value: i32) -> Result<ContractLifecycle, WireConversionError> {
    match common_v1::ContractLifecycle::try_from(value) {
        Ok(common_v1::ContractLifecycle::Preview) => Ok(ContractLifecycle::Preview),
        Ok(common_v1::ContractLifecycle::Stable) => Ok(ContractLifecycle::Stable),
        Ok(common_v1::ContractLifecycle::Unspecified) | Err(_) => {
            Err(WireConversionError::InvalidEnum)
        }
    }
}

#[cfg(test)]
mod tests {
    use aurora_types::{
        BootEpochId, CapabilityId, ContractLifecycle, ContractVersion, DurationNanos, ErrorCode,
        MonotonicTimestamp, QualityCode, QualityFlags, QualitySeverity, RetryClass,
        SemanticVersion, TimeQuality, TimeQualityState, TimeSource, UtcTimestamp,
    };
    use prost::Message;

    use super::{
        WireConversionError, common_v1, decode_boot_epoch, decode_capability,
        decode_contract_version, decode_duration, decode_error_code, decode_monotonic,
        decode_quality_code, decode_retry_class, decode_semantic_version, decode_time_quality,
        decode_utc, encode_capability, encode_contract_version, encode_duration, encode_error_code,
        encode_monotonic, encode_quality_code, encode_retry_class, encode_semantic_version,
        encode_time_quality, encode_utc,
    };

    #[test]
    fn utc_wire_encoding_has_a_stable_golden_vector() {
        let domain = UtcTimestamp::new(1_700_000_000, 123_456_789);
        assert!(domain.is_ok());
        if let Ok(domain) = domain {
            let bytes = encode_utc(domain).encode_to_vec();
            assert_eq!(
                bytes,
                vec![
                    0x08, 0x80, 0xc4, 0x9f, 0xd5, 0x0c, 0x10, 0x95, 0x9a, 0xef, 0x3a
                ]
            );
        }
    }

    #[test]
    fn monotonic_wire_value_round_trips() {
        let domain = MonotonicTimestamp::new(BootEpochId::generate(), 42);
        let encoded = encode_monotonic(domain);
        assert_eq!(decode_monotonic(&encoded), Ok(domain));
    }

    #[test]
    fn invalid_timestamp_is_rejected() {
        let encoded = common_v1::UtcTimestamp {
            seconds: 0,
            nanos: 1_000_000_000,
        };
        assert_eq!(
            decode_utc(&encoded),
            Err(WireConversionError::InvalidTimestamp)
        );
    }

    #[test]
    fn valid_utc_and_invalid_epoch_shapes_take_both_conversion_paths() {
        let valid = common_v1::UtcTimestamp {
            seconds: -7,
            nanos: 8,
        };
        assert_eq!(decode_utc(&valid).map(UtcTimestamp::seconds), Ok(-7));

        assert_eq!(
            decode_boot_epoch(&common_v1::UuidValue { value: vec![0; 15] }),
            Err(WireConversionError::InvalidUuid)
        );
        assert_eq!(
            decode_boot_epoch(&common_v1::UuidValue { value: vec![0; 16] }),
            Err(WireConversionError::InvalidUuid)
        );
        assert_eq!(
            decode_monotonic(&common_v1::MonotonicTimestamp {
                boot_epoch_id: None,
                elapsed_nanos: 0,
            }),
            Err(WireConversionError::MissingValue)
        );
    }

    #[test]
    fn scalar_codes_and_duration_round_trip() {
        let duration = DurationNanos::new(u64::MAX);
        assert_eq!(decode_duration(&encode_duration(duration)), duration);

        let quality = QualityCode::new(QualitySeverity::Bad, 63, u16::MAX, QualityFlags::FORCED)
            .unwrap_or(QualityCode::GOOD);
        assert_eq!(
            decode_quality_code(&encode_quality_code(quality)),
            Ok(quality)
        );
        let golden_quality = QualityCode::from_raw(0x4512_3402).unwrap_or(QualityCode::GOOD);
        assert_eq!(
            encode_quality_code(golden_quality).encode_to_vec(),
            vec![0x0d, 0x02, 0x34, 0x12, 0x45]
        );
        assert_eq!(
            decode_quality_code(&common_v1::QualityCode { value: 0xc000_0000 }),
            Err(WireConversionError::InvalidQualityCode)
        );

        let error = ErrorCode::new(9, 17).unwrap_or(ErrorCode::OK);
        assert_eq!(decode_error_code(&encode_error_code(error)), Ok(error));
        assert_eq!(
            encode_error_code(error).encode_to_vec(),
            vec![0x0d, 0x11, 0x00, 0x09, 0x00]
        );
        assert_eq!(
            decode_error_code(&common_v1::ErrorCode { value: 1 }),
            Err(WireConversionError::InvalidErrorCode)
        );
    }

    #[test]
    fn enum_adapters_map_every_declared_value_and_reject_unknowns() {
        for (domain, wire) in [
            (RetryClass::Never, common_v1::RetryClass::Never),
            (RetryClass::Immediate, common_v1::RetryClass::Immediate),
            (RetryClass::Backoff, common_v1::RetryClass::Backoff),
            (
                RetryClass::AfterOperatorAction,
                common_v1::RetryClass::AfterOperatorAction,
            ),
        ] {
            assert_eq!(encode_retry_class(domain), wire);
            assert_eq!(decode_retry_class(wire as i32), Ok(domain));
        }
        assert_eq!(decode_retry_class(0), Err(WireConversionError::InvalidEnum));
        assert_eq!(
            decode_retry_class(99),
            Err(WireConversionError::InvalidEnum)
        );

        for lifecycle in [ContractLifecycle::Preview, ContractLifecycle::Stable] {
            let version = ContractVersion::new(1, 2, lifecycle);
            if let Ok(version) = version {
                assert_eq!(
                    decode_contract_version(&encode_contract_version(version)),
                    Ok(version)
                );
            }
        }
        assert_eq!(
            decode_contract_version(&common_v1::ContractVersion {
                major: 0,
                minor: 0,
                lifecycle: common_v1::ContractLifecycle::Preview as i32,
            }),
            Err(WireConversionError::InvalidVersion)
        );
        assert_eq!(
            decode_contract_version(&common_v1::ContractVersion::default()),
            Err(WireConversionError::InvalidEnum)
        );
    }

    #[test]
    fn time_quality_maps_all_states_and_sources() {
        let states = [
            TimeQualityState::Unknown,
            TimeQualityState::Synchronizing,
            TimeQualityState::Good,
            TimeQualityState::Holdover,
            TimeQualityState::Degraded,
            TimeQualityState::Invalid,
        ];
        let sources = [
            TimeSource::Unknown,
            TimeSource::System,
            TimeSource::Ntp,
            TimeSource::Ptp,
            TimeSource::Gnss,
            TimeSource::Manual,
        ];
        for (state, source) in states.into_iter().zip(sources) {
            let domain = TimeQuality::new(state, source, Some(42), Some(UtcTimestamp::UNIX_EPOCH));
            assert_eq!(
                decode_time_quality(&encode_time_quality(domain)),
                Ok(domain)
            );
        }
        assert_eq!(
            decode_time_quality(&common_v1::TimeQuality::default()),
            Err(WireConversionError::InvalidEnum)
        );
        assert_eq!(
            decode_time_quality(&common_v1::TimeQuality {
                state: 99,
                source: common_v1::TimeSource::System as i32,
                max_error_nanos: None,
                last_sync_utc: None,
            }),
            Err(WireConversionError::InvalidEnum)
        );
    }

    #[test]
    fn version_and_capability_adapters_validate_text_components() {
        let version = "1.2.3-alpha.1+build.7".parse::<SemanticVersion>();
        if let Ok(version) = version {
            let wire = encode_semantic_version(&version);
            assert_eq!(decode_semantic_version(&wire), Ok(version));
        }
        assert_eq!(
            decode_semantic_version(&common_v1::SemanticVersion {
                major: 1,
                minor: 0,
                patch: 0,
                pre_release: "01".to_owned(),
                build_metadata: String::new(),
            }),
            Err(WireConversionError::InvalidVersion)
        );

        let capability = "aurora.io.read@1".parse::<CapabilityId>();
        if let Ok(capability) = capability {
            assert_eq!(
                decode_capability(&encode_capability(&capability)),
                Ok(capability)
            );
        }
        assert_eq!(
            decode_capability(&common_v1::CapabilityId {
                value: "not-canonical".to_owned(),
            }),
            Err(WireConversionError::InvalidCapability)
        );
    }
}
