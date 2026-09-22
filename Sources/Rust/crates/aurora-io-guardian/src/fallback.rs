//! Bounded Fallback plans, protection evidence, recovery, and watchdog simulation.

use aurora_io_guardian_contracts::{ConfigurationGeneration, LeaseId, LeaseIdentity};
use aurora_types::LocalHandle;
use thiserror::Error;

use crate::{
    GroupDescriptor, ImageDirection, ImageMapping, ProtectionLevel, ProtocolSourceKind,
    RegionHeader, ScalarType, ValueBinding,
};

/// Rejection raised before an invalid Fallback plan or transition can affect outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FallbackError {
    /// A declared fixed capacity is zero or exceeded.
    #[error("fallback capacity is zero or exceeds its fixed limit")]
    InvalidCapacity,
    /// A checked time, version, generation, or counter operation overflowed.
    #[error("fallback arithmetic overflowed")]
    ArithmeticOverflow,
    /// Plan identity does not match the exact image/configuration identity.
    #[error("fallback plan identity does not match the image mapping")]
    MappingIdentityMismatch,
    /// Domain entries are missing, duplicated, extra, reordered, or unused.
    #[error("fallback domain catalog is not an exact dense closure")]
    DomainClosureMismatch,
    /// Output entries are missing, duplicated, extra, reordered, or foreign.
    #[error("fallback output catalog is not the exact mapped output closure")]
    OutputClosureMismatch,
    /// A declared cross-output dependency is invalid or crosses domains.
    #[error("fallback dependency catalog is invalid or crosses domains")]
    DependencyClosureMismatch,
    /// A fixed value or approved range does not match the mapped scalar type.
    #[error("fallback value is outside its approved typed range")]
    ValueRangeMismatch,
    /// Protection evidence is missing, mismatched, or insufficient for the domain risk.
    #[error("fallback protection evidence is unavailable or inconsistent")]
    ProtectionUnavailable,
    /// A hazardous domain attempted to use hold-last behavior.
    #[error("hazardous fallback output cannot hold its last value")]
    UnsafeHoldLast,
    /// Pending Fallback version or configuration generation is not the exact next value.
    #[error("pending fallback is not the exact next immutable version")]
    InvalidPendingVersion,
    /// Pending health evidence failed; the previous active plan remains active.
    #[error("pending fallback health validation failed")]
    PendingHealthFailed,
    /// The fixed pending/recovery health window is not complete.
    #[error("fallback health window is incomplete")]
    HealthWindowIncomplete,
    /// The requested state transition is not valid from the current state.
    #[error("fallback state transition is invalid")]
    InvalidStateTransition,
    /// A protocol-specific failure was reported for another protocol source.
    #[error("fallback failure does not match the reported protocol source")]
    FailureSourceMismatch,
    /// A monotonic observation regressed.
    #[error("fallback monotonic time regressed")]
    MonotonicTimeRegression,
    /// Recovery backoff has not elapsed.
    #[error("fallback recovery backoff has not elapsed")]
    RecoveryTooEarly,
    /// Recovery attempts or the recovery window are exhausted.
    #[error("fallback recovery is locked")]
    RecoveryLocked,
    /// Recovery did not present a new generation, lease, and reinitialization proof.
    #[error("fallback recovery requires a new lease and explicit reinitialization")]
    ReinitializationRequired,
    /// Hazardous output recovery still requires explicit authorization and safety permission.
    #[error("hazardous fallback recovery requires explicit authorization")]
    RecoveryAuthorizationRequired,
    /// A bounded initialization allocation failed.
    #[error("fallback initialization allocation failed")]
    AllocationFailed,
}

/// Non-zero immutable Fallback version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FallbackVersion(u64);

impl FallbackVersion {
    /// Creates a non-zero version.
    ///
    /// # Errors
    ///
    /// Rejects zero, which represents an absent version.
    pub const fn new(value: u64) -> Result<Self, FallbackError> {
        if value == 0 {
            Err(FallbackError::InvalidPendingVersion)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the encoded version.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    fn checked_next(self) -> Result<Self, FallbackError> {
        self.0
            .checked_add(1)
            .map(Self)
            .ok_or(FallbackError::ArithmeticOverflow)
    }
}

macro_rules! digest_type {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Creates a non-zero SHA-256 digest.
            ///
            /// # Errors
            ///
            /// Rejects the all-zero value so absent evidence cannot look validated.
            pub const fn new(bytes: [u8; 32]) -> Result<Self, FallbackError> {
                let mut index = 0;
                while index < bytes.len() {
                    if bytes[index] != 0 {
                        return Ok(Self(bytes));
                    }
                    index += 1;
                }
                Err(FallbackError::ProtectionUnavailable)
            }

            /// Returns all digest bytes.
            #[must_use]
            pub const fn to_sha256(self) -> [u8; 32] {
                self.0
            }
        }
    };
}

digest_type!(
    FallbackDigest,
    "Digest of one immutable Fallback image/configuration."
);
digest_type!(
    EvidenceDigest,
    "Digest of reviewed protection, reinitialization, or authorization evidence."
);

/// Dense Fallback isolation-domain handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FallbackDomainHandle(u16);

impl FallbackDomainHandle {
    /// Creates a handle whose density is validated by the plan.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the compact integer value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Risk class that controls automatic output recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FallbackDomainRisk {
    /// Ordinary process output may recover after bounded health validation.
    Ordinary,
    /// Hazardous output requires explicit authorization and independent safety permission.
    Hazardous,
}

/// Exact typed value used by a fixed Fallback action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FallbackValue {
    /// Boolean value.
    Bool(bool),
    /// Unsigned 8-bit value.
    U8(u8),
    /// Signed 8-bit value.
    I8(i8),
    /// Unsigned 16-bit value.
    U16(u16),
    /// Signed 16-bit value.
    I16(i16),
    /// Unsigned 32-bit value.
    U32(u32),
    /// Signed 32-bit value.
    I32(i32),
    /// Finite IEEE-754 binary32 bits.
    F32Bits(u32),
    /// Unsigned 64-bit value.
    U64(u64),
    /// Signed 64-bit value.
    I64(i64),
    /// Finite IEEE-754 binary64 bits.
    F64Bits(u64),
}

impl FallbackValue {
    /// Creates a finite binary32 value.
    ///
    /// # Errors
    ///
    /// Rejects infinities and NaN.
    pub fn from_f32(value: f32) -> Result<Self, FallbackError> {
        if value.is_finite() {
            Ok(Self::F32Bits(value.to_bits()))
        } else {
            Err(FallbackError::ValueRangeMismatch)
        }
    }

    /// Creates a finite binary64 value.
    ///
    /// # Errors
    ///
    /// Rejects infinities and NaN.
    pub fn from_f64(value: f64) -> Result<Self, FallbackError> {
        if value.is_finite() {
            Ok(Self::F64Bits(value.to_bits()))
        } else {
            Err(FallbackError::ValueRangeMismatch)
        }
    }

    /// Returns the exact mapped scalar type.
    #[must_use]
    pub const fn scalar_type(self) -> ScalarType {
        match self {
            Self::Bool(_) => ScalarType::Bool,
            Self::U8(_) => ScalarType::U8,
            Self::I8(_) => ScalarType::I8,
            Self::U16(_) => ScalarType::U16,
            Self::I16(_) => ScalarType::I16,
            Self::U32(_) => ScalarType::U32,
            Self::I32(_) => ScalarType::I32,
            Self::F32Bits(_) => ScalarType::F32,
            Self::U64(_) => ScalarType::U64,
            Self::I64(_) => ScalarType::I64,
            Self::F64Bits(_) => ScalarType::F64,
        }
    }
}

/// Inclusive typed range approved for one output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FallbackValueRange {
    minimum: FallbackValue,
    maximum: FallbackValue,
}

impl FallbackValueRange {
    /// Creates a same-type, ordered inclusive range.
    ///
    /// # Errors
    ///
    /// Rejects different scalar types or a reversed range.
    pub fn new(minimum: FallbackValue, maximum: FallbackValue) -> Result<Self, FallbackError> {
        if minimum.scalar_type() != maximum.scalar_type()
            || !fallback_value_less_or_equal(minimum, maximum)
        {
            return Err(FallbackError::ValueRangeMismatch);
        }
        Ok(Self { minimum, maximum })
    }

    /// Returns whether the value has the exact type and lies inside the inclusive range.
    #[must_use]
    pub fn contains(self, value: FallbackValue) -> bool {
        value.scalar_type() == self.minimum.scalar_type()
            && fallback_value_less_or_equal(self.minimum, value)
            && fallback_value_less_or_equal(value, self.maximum)
    }
}

fn fallback_value_less_or_equal(left: FallbackValue, right: FallbackValue) -> bool {
    match (left, right) {
        (FallbackValue::Bool(left), FallbackValue::Bool(right)) => left <= right,
        (FallbackValue::U8(left), FallbackValue::U8(right)) => left <= right,
        (FallbackValue::I8(left), FallbackValue::I8(right)) => left <= right,
        (FallbackValue::U16(left), FallbackValue::U16(right)) => left <= right,
        (FallbackValue::I16(left), FallbackValue::I16(right)) => left <= right,
        (FallbackValue::U32(left), FallbackValue::U32(right)) => left <= right,
        (FallbackValue::I32(left), FallbackValue::I32(right)) => left <= right,
        (FallbackValue::F32Bits(left), FallbackValue::F32Bits(right)) => {
            f32::from_bits(left) <= f32::from_bits(right)
        }
        (FallbackValue::U64(left), FallbackValue::U64(right)) => left <= right,
        (FallbackValue::I64(left), FallbackValue::I64(right)) => left <= right,
        (FallbackValue::F64Bits(left), FallbackValue::F64Bits(right)) => {
            f64::from_bits(left) <= f64::from_bits(right)
        }
        _ => false,
    }
}

/// Only the three bounded Fallback action forms accepted by Preview 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FallbackAction {
    /// Immediately write one approved fixed value.
    SetFixed(FallbackValue),
    /// Hold the last confirmed value for a finite duration, then write a fixed value.
    HoldLastThenFixed {
        /// Maximum hold duration in nanoseconds.
        hold_ns: u64,
        /// Approved value applied after the hold deadline.
        fixed: FallbackValue,
    },
    /// Delegate to one reviewed device-native watchdog preset.
    DeviceWatchdogPreset {
        /// Digest of the exact preset programmed into the device.
        preset: FallbackDigest,
    },
}

/// Evidence matching one mapped output protection level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtectionEvidence {
    /// Guardian remains the only process-layer protection; hazardous domains cannot use this.
    Guardian,
    /// A device-native watchdog and exact preset have been validated.
    DeviceWatchdog {
        /// Reviewed device/firmware/capability evidence.
        evidence: EvidenceDigest,
        /// Exact preset the device applies when the watchdog expires.
        preset: FallbackDigest,
        /// Maximum time from the last valid kick to preset application.
        timeout_ns: u64,
    },
    /// An independent external protection boundary has been validated.
    ExternalSafety {
        /// Immutable reference to the reviewed external protection evidence.
        evidence: EvidenceDigest,
    },
}

impl ProtectionEvidence {
    fn level(self) -> ProtectionLevel {
        match self {
            Self::Guardian => ProtectionLevel::GuardianProtected,
            Self::DeviceWatchdog { .. } => ProtectionLevel::DeviceWatchdogProtected,
            Self::ExternalSafety { .. } => ProtectionLevel::ExternalSafetyProtected,
        }
    }
}

/// Fixed consecutive-observation and duration health gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FallbackHealthPolicy {
    required_observations: u16,
    minimum_duration_ns: u64,
}

impl FallbackHealthPolicy {
    /// Creates a non-zero fixed health gate.
    ///
    /// # Errors
    ///
    /// Rejects zero observation or duration limits.
    pub const fn new(
        required_observations: u16,
        minimum_duration_ns: u64,
    ) -> Result<Self, FallbackError> {
        if required_observations == 0 || minimum_duration_ns == 0 {
            return Err(FallbackError::InvalidCapacity);
        }
        Ok(Self {
            required_observations,
            minimum_duration_ns,
        })
    }
}

/// Fixed recovery attempts, window, backoff, and health gate for one domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DomainRecoveryPolicy {
    maximum_attempts: u16,
    window_ns: u64,
    backoff_ns: u64,
    health: FallbackHealthPolicy,
}

impl DomainRecoveryPolicy {
    /// Creates a fully bounded recovery policy.
    ///
    /// # Errors
    ///
    /// Rejects zero bounds, a backoff that cannot fit inside the recovery window, or a health
    /// duration that cannot complete before the window deadline.
    pub const fn new(
        maximum_attempts: u16,
        window_ns: u64,
        backoff_ns: u64,
        health: FallbackHealthPolicy,
    ) -> Result<Self, FallbackError> {
        if maximum_attempts == 0
            || window_ns == 0
            || backoff_ns == 0
            || backoff_ns >= window_ns
            || health.minimum_duration_ns >= window_ns
        {
            return Err(FallbackError::InvalidCapacity);
        }
        Ok(Self {
            maximum_attempts,
            window_ns,
            backoff_ns,
            health,
        })
    }
}

/// One dense Fallback isolation domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FallbackDomainSpec {
    handle: FallbackDomainHandle,
    risk: FallbackDomainRisk,
    recovery: DomainRecoveryPolicy,
}

impl FallbackDomainSpec {
    /// Creates one domain specification.
    #[must_use]
    pub const fn new(
        handle: FallbackDomainHandle,
        risk: FallbackDomainRisk,
        recovery: DomainRecoveryPolicy,
    ) -> Self {
        Self {
            handle,
            risk,
            recovery,
        }
    }

    /// Returns the dense domain handle.
    #[must_use]
    pub const fn handle(self) -> FallbackDomainHandle {
        self.handle
    }

    /// Returns the output risk class.
    #[must_use]
    pub const fn risk(self) -> FallbackDomainRisk {
        self.risk
    }
}

/// One exact mapped output, its domain, action, approved range, and protection evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputFallbackSpec {
    binding: ValueBinding,
    domain: FallbackDomainHandle,
    approved_range: FallbackValueRange,
    action: FallbackAction,
    protection: ProtectionEvidence,
}

impl OutputFallbackSpec {
    /// Creates and validates one typed output Fallback specification.
    ///
    /// # Errors
    ///
    /// Rejects input bindings, type/range drift, protection mismatch, zero hold, or an unproven
    /// watchdog preset.
    pub fn new(
        binding: ValueBinding,
        domain: FallbackDomainHandle,
        approved_range: FallbackValueRange,
        action: FallbackAction,
        protection: ProtectionEvidence,
    ) -> Result<Self, FallbackError> {
        if binding.direction() != ImageDirection::Output
            || binding.protection() != Some(protection.level())
        {
            return Err(FallbackError::ProtectionUnavailable);
        }
        if approved_range.minimum.scalar_type() != binding.scalar_type() {
            return Err(FallbackError::ValueRangeMismatch);
        }
        match action {
            FallbackAction::SetFixed(value) => {
                if !approved_range.contains(value) {
                    return Err(FallbackError::ValueRangeMismatch);
                }
            }
            FallbackAction::HoldLastThenFixed { hold_ns, fixed } => {
                if hold_ns == 0 || !approved_range.contains(fixed) {
                    return Err(FallbackError::ValueRangeMismatch);
                }
            }
            FallbackAction::DeviceWatchdogPreset { preset } => match protection {
                ProtectionEvidence::DeviceWatchdog {
                    preset: evidence_preset,
                    timeout_ns,
                    ..
                } if preset == evidence_preset && timeout_ns != 0 => {}
                ProtectionEvidence::Guardian
                | ProtectionEvidence::ExternalSafety { .. }
                | ProtectionEvidence::DeviceWatchdog { .. } => {
                    return Err(FallbackError::ProtectionUnavailable);
                }
            },
        }
        if matches!(
            protection,
            ProtectionEvidence::DeviceWatchdog { timeout_ns: 0, .. }
        ) {
            return Err(FallbackError::ProtectionUnavailable);
        }
        Ok(Self {
            binding,
            domain,
            approved_range,
            action,
            protection,
        })
    }

    /// Returns the exact mapped output binding.
    #[must_use]
    pub const fn binding(self) -> ValueBinding {
        self.binding
    }

    /// Returns the owning Fallback domain.
    #[must_use]
    pub const fn domain(self) -> FallbackDomainHandle {
        self.domain
    }
}

/// One declared cross-output dependency that must remain inside one domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FallbackDependency {
    first: LocalHandle,
    second: LocalHandle,
}

impl FallbackDependency {
    /// Creates a canonical pair with strictly increasing handles.
    ///
    /// # Errors
    ///
    /// Rejects equal or decreasing handles.
    pub const fn new(first: LocalHandle, second: LocalHandle) -> Result<Self, FallbackError> {
        if first.get() >= second.get() {
            Err(FallbackError::DependencyClosureMismatch)
        } else {
            Ok(Self { first, second })
        }
    }
}

/// Fixed construction limits for one plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FallbackLimits {
    domains: u16,
    outputs: u32,
    dependencies: u32,
}

impl FallbackLimits {
    /// Creates non-zero plan limits.
    ///
    /// # Errors
    ///
    /// Rejects any zero limit.
    pub const fn new(
        maximum_domains: u16,
        maximum_outputs: u32,
        maximum_dependencies: u32,
    ) -> Result<Self, FallbackError> {
        if maximum_domains == 0 || maximum_outputs == 0 || maximum_dependencies == 0 {
            return Err(FallbackError::InvalidCapacity);
        }
        Ok(Self {
            domains: maximum_domains,
            outputs: maximum_outputs,
            dependencies: maximum_dependencies,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct PlannedOutput {
    specification: OutputFallbackSpec,
    protocol: ProtocolSourceKind,
}

/// Owned immutable Fallback plan bound to one exact configuration and image mapping.
pub struct FallbackPlan {
    region: RegionHeader,
    version: FallbackVersion,
    digest: FallbackDigest,
    pending_health: FallbackHealthPolicy,
    domains: Box<[FallbackDomainSpec]>,
    outputs: Box<[PlannedOutput]>,
    groups: Box<[GroupDescriptor]>,
    dependencies: Box<[FallbackDependency]>,
}

impl FallbackPlan {
    /// Validates exact output/domain/dependency closure before allocating an immutable plan.
    ///
    /// # Errors
    ///
    /// Rejects identity drift, incomplete or extra closure, unsafe protection/action choices,
    /// capacity excess, and bounded allocation failure.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        region: RegionHeader,
        mapping: ImageMapping<'_>,
        version: FallbackVersion,
        digest: FallbackDigest,
        pending_health: FallbackHealthPolicy,
        domains: &[FallbackDomainSpec],
        outputs: &[OutputFallbackSpec],
        dependencies: &[FallbackDependency],
        limits: FallbackLimits,
    ) -> Result<Self, FallbackError> {
        validate_fallback_identity(&region, mapping)?;
        if domains.is_empty()
            || domains.len() > usize::from(limits.domains)
            || outputs.is_empty()
            || outputs.len()
                > usize::try_from(limits.outputs).map_err(|_| FallbackError::InvalidCapacity)?
            || dependencies.len()
                > usize::try_from(limits.dependencies)
                    .map_err(|_| FallbackError::InvalidCapacity)?
        {
            return Err(FallbackError::InvalidCapacity);
        }
        validate_domains(domains)?;
        let planned_outputs = validate_outputs(mapping, domains, outputs)?;
        validate_dependencies(outputs, dependencies)?;
        Ok(Self {
            region,
            version,
            digest,
            pending_health,
            domains: copy_boxed(domains)?,
            outputs: planned_outputs,
            groups: copy_boxed(mapping.groups())?,
            dependencies: copy_boxed(dependencies)?,
        })
    }

    /// Returns the immutable Fallback version.
    #[must_use]
    pub const fn version(&self) -> FallbackVersion {
        self.version
    }

    /// Returns the immutable Fallback digest.
    #[must_use]
    pub const fn digest(&self) -> FallbackDigest {
        self.digest
    }

    /// Returns the exact configuration/lease identity carried by this plan.
    #[must_use]
    pub const fn lease_identity(&self) -> LeaseIdentity {
        self.region.lease_identity()
    }

    /// Returns the exact number of isolation domains.
    #[must_use]
    pub fn domain_count(&self) -> usize {
        self.domains.len()
    }

    /// Returns the exact number of mapped outputs.
    #[must_use]
    pub fn output_count(&self) -> usize {
        self.outputs.len()
    }

    /// Returns the number of explicitly declared cross-output dependencies.
    #[must_use]
    pub fn dependency_count(&self) -> usize {
        self.dependencies.len()
    }
}

fn validate_fallback_identity(
    region: &RegionHeader,
    mapping: ImageMapping<'_>,
) -> Result<(), FallbackError> {
    if region.layout() != mapping.layout()
        || region.lease_identity().configuration().layout_digest() != mapping.layout_digest()
        || region.capability_digest() != mapping.capability_digest()
    {
        return Err(FallbackError::MappingIdentityMismatch);
    }
    Ok(())
}

fn validate_domains(domains: &[FallbackDomainSpec]) -> Result<(), FallbackError> {
    for (index, domain) in domains.iter().enumerate() {
        let expected = u16::try_from(index).map_err(|_| FallbackError::InvalidCapacity)?;
        if domain.handle().get() != expected {
            return Err(FallbackError::DomainClosureMismatch);
        }
    }
    Ok(())
}

fn validate_outputs(
    mapping: ImageMapping<'_>,
    domains: &[FallbackDomainSpec],
    outputs: &[OutputFallbackSpec],
) -> Result<Box<[PlannedOutput]>, FallbackError> {
    let input_count = usize::try_from(mapping.layout().input().value_count())
        .map_err(|_| FallbackError::InvalidCapacity)?;
    let mapped_outputs = mapping
        .values()
        .get(input_count..)
        .ok_or(FallbackError::OutputClosureMismatch)?;
    if outputs.len() != mapped_outputs.len() {
        return Err(FallbackError::OutputClosureMismatch);
    }
    let mut used_domains = Vec::new();
    used_domains
        .try_reserve_exact(domains.len())
        .map_err(|_| FallbackError::AllocationFailed)?;
    used_domains.resize(domains.len(), false);
    let mut planned = Vec::new();
    planned
        .try_reserve_exact(outputs.len())
        .map_err(|_| FallbackError::AllocationFailed)?;
    for (mapped, output) in mapped_outputs.iter().zip(outputs) {
        if *mapped != output.binding {
            return Err(FallbackError::OutputClosureMismatch);
        }
        let domain_index = usize::from(output.domain.get());
        let Some(domain) = domains.get(domain_index) else {
            return Err(FallbackError::DomainClosureMismatch);
        };
        if domain.risk() == FallbackDomainRisk::Hazardous {
            if matches!(output.action, FallbackAction::HoldLastThenFixed { .. }) {
                return Err(FallbackError::UnsafeHoldLast);
            }
            if matches!(output.protection, ProtectionEvidence::Guardian) {
                return Err(FallbackError::ProtectionUnavailable);
            }
        }
        used_domains[domain_index] = true;
        let source_index = usize::try_from(mapped.source().get())
            .map_err(|_| FallbackError::OutputClosureMismatch)?;
        let source = mapping
            .sources()
            .get(source_index)
            .ok_or(FallbackError::OutputClosureMismatch)?;
        planned.push(PlannedOutput {
            specification: *output,
            protocol: source.protocol(),
        });
    }
    if used_domains.contains(&false) {
        return Err(FallbackError::DomainClosureMismatch);
    }
    Ok(planned.into_boxed_slice())
}

fn validate_dependencies(
    outputs: &[OutputFallbackSpec],
    dependencies: &[FallbackDependency],
) -> Result<(), FallbackError> {
    let mut previous: Option<(u32, u32)> = None;
    for dependency in dependencies {
        let key = (dependency.first.get(), dependency.second.get());
        if previous.is_some_and(|value| key <= value) {
            return Err(FallbackError::DependencyClosureMismatch);
        }
        let first = outputs
            .iter()
            .find(|output| output.binding.handle() == dependency.first);
        let second = outputs
            .iter()
            .find(|output| output.binding.handle() == dependency.second);
        if !matches!((first, second), (Some(first), Some(second)) if first.domain == second.domain)
        {
            return Err(FallbackError::DependencyClosureMismatch);
        }
        previous = Some(key);
    }
    Ok(())
}

fn copy_boxed<T: Copy>(values: &[T]) -> Result<Box<[T]>, FallbackError> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(values.len())
        .map_err(|_| FallbackError::AllocationFailed)?;
    copied.extend_from_slice(values);
    Ok(copied.into_boxed_slice())
}

/// Exhaustive failure classes that may move one or all domains to Fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FallbackCause {
    /// Control process exited or stopped responding.
    ControlProcessLost,
    /// Global Control heartbeat reached its deadline.
    ControlHeartbeatExpired,
    /// The active lease was revoked or became foreign.
    LeaseRevoked,
    /// Guardian process loss; only device watchdog/external protection remains available.
    GuardianProcessLost,
    /// Isolated Driver Host exited.
    DriverHostLost,
    /// Output image or group freshness expired.
    OutputStale,
    /// A fixed transport/application queue was full.
    QueueFull,
    /// `EtherCAT` working counter differed from the signed expectation.
    EthercatWorkingCounter,
    /// `EtherCAT` distributed-clock validation failed.
    EthercatDistributedClock,
    /// `EtherCAT` AL state entered an invalid/fault state.
    EthercatApplicationLayer,
    /// Modbus transaction timed out.
    ModbusTimeout,
    /// Modbus returned a protocol exception.
    ModbusException,
    /// Serial or RTU CRC validation failed.
    SerialCrc,
    /// Serial framing/overrun/break validation failed.
    SerialFraming,
    /// CAN error frame or error-passive state was observed.
    CanError,
    /// CAN controller entered bus-off.
    CanBusOff,
    /// LIN schedule timing was invalid or missed.
    LinSchedule,
    /// LIN response timed out.
    LinResponseTimeout,
}

impl FallbackCause {
    const fn is_global(self) -> bool {
        matches!(
            self,
            Self::ControlProcessLost
                | Self::ControlHeartbeatExpired
                | Self::LeaseRevoked
                | Self::GuardianProcessLost
        )
    }

    const fn matches_protocol(self, protocol: ProtocolSourceKind) -> bool {
        match self {
            Self::EthercatWorkingCounter
            | Self::EthercatDistributedClock
            | Self::EthercatApplicationLayer => matches!(protocol, ProtocolSourceKind::Ethercat),
            Self::ModbusTimeout | Self::ModbusException => matches!(
                protocol,
                ProtocolSourceKind::ModbusTcp | ProtocolSourceKind::ModbusRtu
            ),
            Self::SerialCrc | Self::SerialFraming => matches!(
                protocol,
                ProtocolSourceKind::ModbusRtu | ProtocolSourceKind::Serial
            ),
            Self::CanError | Self::CanBusOff => matches!(protocol, ProtocolSourceKind::Can),
            Self::LinSchedule | Self::LinResponseTimeout => {
                matches!(protocol, ProtocolSourceKind::Lin)
            }
            Self::DriverHostLost | Self::OutputStale | Self::QueueFull => true,
            Self::ControlProcessLost
            | Self::ControlHeartbeatExpired
            | Self::LeaseRevoked
            | Self::GuardianProcessLost => false,
        }
    }
}

/// Observable state of one Fallback domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FallbackDomainState {
    /// Current lease may drive normal output.
    Normal,
    /// Active Fallback action owns the domain.
    Fallback,
    /// A bounded recovery attempt is inside its health gate.
    RecoveryChecking,
    /// Health passed, but hazardous output authorization is still required.
    AwaitingAuthorization,
    /// Recovery attempts/window are exhausted.
    RecoveryLocked,
    /// Guardian is unavailable; only device watchdog/external protection can act.
    GuardianUnavailable,
}

/// Effective output command selected by the controller/simulator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FallbackEffect {
    /// Normal current-lease output remains eligible.
    Normal,
    /// Guardian applies an approved fixed value.
    SetFixed(FallbackValue),
    /// Guardian holds the last confirmed value until the absolute deadline.
    HoldLastUntil(u64),
    /// Device-native watchdog applies its reviewed preset.
    DeviceWatchdogPreset(FallbackDigest),
    /// Independent external protection owns the hazardous boundary.
    ExternalProtection,
    /// Guardian-only protection is unavailable after Guardian loss.
    GuardianUnavailable,
}

/// Runtime health checklist used for pending activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PendingFallbackHealth(u8);

/// One explicit health-check result; avoids ambiguous boolean argument ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HealthCheckResult {
    /// The check produced valid evidence.
    Passed,
    /// The check failed or evidence was missing.
    Failed,
}

impl PendingFallbackHealth {
    const DEVICES: u8 = 1 << 0;
    const PROTOCOLS: u8 = 1 << 1;
    const WATCHDOGS: u8 = 1 << 2;
    const EXTERNAL_PROTECTION: u8 = 1 << 3;
    const ALL: u8 = Self::DEVICES | Self::PROTOCOLS | Self::WATCHDOGS | Self::EXTERNAL_PROTECTION;

    /// Creates an exact checklist without allowing unknown bits.
    #[must_use]
    pub const fn new(
        devices_validated: HealthCheckResult,
        protocols_healthy: HealthCheckResult,
        watchdogs_armed: HealthCheckResult,
        external_protection_available: HealthCheckResult,
    ) -> Self {
        let mut bits = 0;
        if matches!(devices_validated, HealthCheckResult::Passed) {
            bits |= Self::DEVICES;
        }
        if matches!(protocols_healthy, HealthCheckResult::Passed) {
            bits |= Self::PROTOCOLS;
        }
        if matches!(watchdogs_armed, HealthCheckResult::Passed) {
            bits |= Self::WATCHDOGS;
        }
        if matches!(external_protection_available, HealthCheckResult::Passed) {
            bits |= Self::EXTERNAL_PROTECTION;
        }
        Self(bits)
    }

    /// Returns a checklist with every required health condition proven.
    #[must_use]
    pub const fn all_passed() -> Self {
        Self(Self::ALL)
    }

    const fn contains(self, check: u8) -> bool {
        self.0 & check == check
    }
}

/// Progress returned by a bounded recovery health observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RecoveryProgress {
    /// More healthy observations or duration are required.
    Checking,
    /// Ordinary output returned to normal under the new identity.
    Recovered,
    /// Hazardous output passed health but still requires explicit authorization.
    AwaitingAuthorization,
    /// The attempt failed and a later bounded retry remains possible.
    RetryScheduled,
    /// Recovery is exhausted and latched locked.
    Locked,
}

/// Proof that a recovery candidate was explicitly reinitialized under a new lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReinitializationEvidence {
    identity: LeaseIdentity,
    evidence: EvidenceDigest,
}

impl ReinitializationEvidence {
    /// Creates evidence for one exact candidate lease identity.
    #[must_use]
    pub const fn new(identity: LeaseIdentity, evidence: EvidenceDigest) -> Self {
        Self { identity, evidence }
    }

    /// Returns the candidate identity.
    #[must_use]
    pub const fn identity(self) -> LeaseIdentity {
        self.identity
    }

    /// Returns the immutable reinitialization evidence digest.
    #[must_use]
    pub const fn evidence_digest(self) -> EvidenceDigest {
        self.evidence
    }
}

/// Explicit permission required before hazardous outputs leave Fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecoveryAuthorization {
    identity: LeaseIdentity,
    authorization: EvidenceDigest,
    safety_permission: EvidenceDigest,
}

impl RecoveryAuthorization {
    /// Binds signed/local authorization and independent safety permission to one candidate.
    #[must_use]
    pub const fn new(
        identity: LeaseIdentity,
        authorization: EvidenceDigest,
        safety_permission: EvidenceDigest,
    ) -> Self {
        Self {
            identity,
            authorization,
            safety_permission,
        }
    }

    /// Returns the signed/local authorization evidence digest.
    #[must_use]
    pub const fn authorization_digest(self) -> EvidenceDigest {
        self.authorization
    }

    /// Returns the independent safety permission evidence digest.
    #[must_use]
    pub const fn safety_permission_digest(self) -> EvidenceDigest {
        self.safety_permission
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RecoveryCandidate {
    evidence: ReinitializationEvidence,
    health_start_ns: Option<u64>,
    health_observations: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DomainRuntime {
    state: FallbackDomainState,
    current_identity: LeaseIdentity,
    cause: Option<FallbackCause>,
    fault_since_ns: Option<u64>,
    recovery_window_deadline_ns: Option<u64>,
    recovery_attempts: u16,
    next_recovery_ns: u64,
    last_candidate_generation: Option<ConfigurationGeneration>,
    last_candidate_lease: Option<LeaseId>,
    candidate: Option<RecoveryCandidate>,
}

impl DomainRuntime {
    const fn new(identity: LeaseIdentity) -> Self {
        Self {
            state: FallbackDomainState::Fallback,
            current_identity: identity,
            cause: None,
            fault_since_ns: None,
            recovery_window_deadline_ns: None,
            recovery_attempts: 0,
            next_recovery_ns: 0,
            last_candidate_generation: None,
            last_candidate_lease: None,
            candidate: None,
        }
    }
}

struct PendingFallback {
    plan: FallbackPlan,
    runtimes: Box<[DomainRuntime]>,
    health_start_ns: Option<u64>,
    last_health_ns: Option<u64>,
    health_observations: u16,
}

/// Immutable diagnostics for one Fallback domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackDomainDiagnostics {
    state: FallbackDomainState,
    current_identity: LeaseIdentity,
    cause: Option<FallbackCause>,
    fault_since_ns: Option<u64>,
    recovery_attempts: u16,
    next_recovery_ns: u64,
}

impl FallbackDomainDiagnostics {
    /// Returns current domain state.
    #[must_use]
    pub const fn state(self) -> FallbackDomainState {
        self.state
    }

    /// Returns the exact identity admitted for this domain.
    #[must_use]
    pub const fn current_identity(self) -> LeaseIdentity {
        self.current_identity
    }

    /// Returns the last failure cause.
    #[must_use]
    pub const fn cause(self) -> Option<FallbackCause> {
        self.cause
    }

    /// Returns the saturated number of recovery attempts in the current incident.
    #[must_use]
    pub const fn recovery_attempts(self) -> u16 {
        self.recovery_attempts
    }

    /// Returns the first monotonic time at which the current incident entered Fallback.
    #[must_use]
    pub const fn fault_since_ns(self) -> Option<u64> {
        self.fault_since_ns
    }

    /// Returns the earliest monotonic time at which another attempt may start.
    #[must_use]
    pub const fn next_recovery_ns(self) -> u64 {
        self.next_recovery_ns
    }
}

/// Active/pending Fallback controller with fixed per-domain runtime state.
pub struct FallbackController {
    active: FallbackPlan,
    runtimes: Box<[DomainRuntime]>,
    pending: Option<PendingFallback>,
    last_monotonic_ns: Option<u64>,
}

impl FallbackController {
    /// Creates a controller in armed Fallback; normal output still requires exact lease activation.
    ///
    /// # Errors
    ///
    /// Returns a bounded allocation failure while creating exact per-domain runtime state.
    pub fn new(active: FallbackPlan) -> Result<Self, FallbackError> {
        let runtimes = make_runtimes(&active)?;
        Ok(Self {
            active,
            runtimes,
            pending: None,
            last_monotonic_ns: None,
        })
    }

    /// Returns the active immutable Fallback version.
    #[must_use]
    pub const fn active_version(&self) -> FallbackVersion {
        self.active.version
    }

    /// Returns the staged version, which is never eligible to drive outputs.
    #[must_use]
    pub fn pending_version(&self) -> Option<FallbackVersion> {
        self.pending.as_ref().map(|pending| pending.plan.version)
    }

    /// Moves every domain to normal only for the exact active lease identity.
    ///
    /// # Errors
    ///
    /// Rejects a foreign identity, pending plan, non-Fallback domain, or time regression.
    pub fn activate_running(
        &mut self,
        identity: LeaseIdentity,
        now_ns: u64,
    ) -> Result<(), FallbackError> {
        self.validate_monotonic(now_ns)?;
        if self.pending.is_some()
            || identity != self.active.region.lease_identity()
            || self
                .runtimes
                .iter()
                .any(|runtime| runtime.state != FallbackDomainState::Fallback)
        {
            return Err(FallbackError::InvalidStateTransition);
        }
        for runtime in &mut self.runtimes {
            runtime.state = FallbackDomainState::Normal;
            runtime.current_identity = identity;
            runtime.cause = None;
            runtime.fault_since_ns = None;
        }
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Stages an exact-next plan while the previous active plan continues to own outputs.
    ///
    /// # Errors
    ///
    /// Rejects a non-Fallback state, an existing pending plan, skipped version/generation, epoch
    /// drift, time regression, or bounded allocation failure.
    pub fn stage_pending(
        &mut self,
        pending: FallbackPlan,
        now_ns: u64,
    ) -> Result<(), FallbackError> {
        self.validate_monotonic(now_ns)?;
        if self.pending.is_some()
            || self.runtimes.iter().any(|runtime| {
                !matches!(
                    runtime.state,
                    FallbackDomainState::Fallback | FallbackDomainState::RecoveryLocked
                )
            })
        {
            return Err(FallbackError::InvalidStateTransition);
        }
        let active_configuration = self.active.region.lease_identity().configuration();
        let pending_configuration = pending.region.lease_identity().configuration();
        if pending.version != self.active.version.checked_next()?
            || pending.digest == self.active.digest
            || pending_configuration.epoch() != active_configuration.epoch()
            || pending_configuration.generation()
                != active_configuration
                    .generation()
                    .checked_next()
                    .map_err(|_| FallbackError::ArithmeticOverflow)?
            || pending.region.lease_identity().lease_id()
                == self.active.region.lease_identity().lease_id()
        {
            return Err(FallbackError::InvalidPendingVersion);
        }
        let runtimes = make_runtimes(&pending)?;
        self.pending = Some(PendingFallback {
            plan: pending,
            runtimes,
            health_start_ns: None,
            last_health_ns: None,
            health_observations: 0,
        });
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Records one complete pending health observation.
    ///
    /// A failed observation atomically discards pending and retains the previous active plan.
    ///
    /// # Errors
    ///
    /// Rejects an absent pending plan or time regression; a failed checklist returns
    /// [`FallbackError::PendingHealthFailed`].
    pub fn observe_pending_health(
        &mut self,
        now_ns: u64,
        health: PendingFallbackHealth,
    ) -> Result<(), FallbackError> {
        self.validate_monotonic(now_ns)?;
        if self.pending.is_none() {
            return Err(FallbackError::InvalidStateTransition);
        }
        let health_passed = self
            .pending
            .as_ref()
            .is_some_and(|pending| pending_health_passed(&pending.plan, health));
        if !health_passed {
            self.pending = None;
            self.last_monotonic_ns = Some(now_ns);
            return Err(FallbackError::PendingHealthFailed);
        }
        let pending = self
            .pending
            .as_mut()
            .ok_or(FallbackError::InvalidStateTransition)?;
        if pending.health_start_ns.is_none() {
            pending.health_start_ns = Some(now_ns);
        }
        pending.last_health_ns = Some(now_ns);
        pending.health_observations = pending.health_observations.saturating_add(1);
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Atomically promotes a fully healthy pending plan and leaves every new domain in Fallback.
    ///
    /// # Errors
    ///
    /// Rejects an absent pending plan, incomplete health window, or time regression.
    pub fn commit_pending(&mut self, now_ns: u64) -> Result<(), FallbackError> {
        self.validate_monotonic(now_ns)?;
        let pending = self
            .pending
            .as_ref()
            .ok_or(FallbackError::InvalidStateTransition)?;
        let start = pending
            .health_start_ns
            .ok_or(FallbackError::HealthWindowIncomplete)?;
        let last_health_ns = pending
            .last_health_ns
            .ok_or(FallbackError::HealthWindowIncomplete)?;
        let duration = last_health_ns
            .checked_sub(start)
            .ok_or(FallbackError::MonotonicTimeRegression)?;
        if pending.health_observations < pending.plan.pending_health.required_observations
            || duration < pending.plan.pending_health.minimum_duration_ns
        {
            return Err(FallbackError::HealthWindowIncomplete);
        }
        let pending = self
            .pending
            .take()
            .ok_or(FallbackError::InvalidStateTransition)?;
        self.active = pending.plan;
        self.runtimes = pending.runtimes;
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Discards pending without changing the active plan.
    ///
    /// # Errors
    ///
    /// Rejects when no pending plan exists.
    pub fn abort_pending(&mut self) -> Result<(), FallbackError> {
        if self.pending.take().is_none() {
            return Err(FallbackError::InvalidStateTransition);
        }
        Ok(())
    }

    /// Applies a global Control/lease/Guardian failure to every domain in one bounded pass.
    ///
    /// # Errors
    ///
    /// Rejects a group-local cause or time regression.
    pub fn trigger_global_failure(
        &mut self,
        cause: FallbackCause,
        now_ns: u64,
    ) -> Result<(), FallbackError> {
        self.validate_monotonic(now_ns)?;
        if !cause.is_global() {
            return Err(FallbackError::FailureSourceMismatch);
        }
        for index in 0..self.runtimes.len() {
            enter_domain_fallback(
                &mut self.runtimes[index],
                self.active.domains[index].recovery,
                cause,
                now_ns,
            );
        }
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Applies a protocol/group-local fault only to domains owning related outputs.
    ///
    /// # Errors
    ///
    /// Rejects a foreign group, global cause, protocol mismatch, unrelated group, or time
    /// regression.
    pub fn trigger_group_failure(
        &mut self,
        group: GroupDescriptor,
        cause: FallbackCause,
        now_ns: u64,
    ) -> Result<(), FallbackError> {
        self.validate_monotonic(now_ns)?;
        if cause.is_global() || !self.active.groups.contains(&group) {
            return Err(FallbackError::FailureSourceMismatch);
        }
        let protocol = self
            .active
            .outputs
            .iter()
            .find(|output| output.specification.binding.source() == group.source())
            .map(|output| output.protocol)
            .ok_or(FallbackError::FailureSourceMismatch)?;
        if !cause.matches_protocol(protocol) {
            return Err(FallbackError::FailureSourceMismatch);
        }
        let mut affected = false;
        for domain_index in 0..self.active.domains.len() {
            let handle = self.active.domains[domain_index].handle();
            let owns_related_output = self.active.outputs.iter().any(|output| {
                let binding = output.specification.binding;
                output.specification.domain == handle
                    && binding.source() == group.source()
                    && (group.direction() == ImageDirection::Input
                        || binding.group() == group.handle())
            });
            if owns_related_output {
                enter_domain_fallback(
                    &mut self.runtimes[domain_index],
                    self.active.domains[domain_index].recovery,
                    cause,
                    now_ns,
                );
                affected = true;
            }
        }
        if !affected {
            return Err(FallbackError::FailureSourceMismatch);
        }
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Returns the effective action for one exact output without changing state.
    ///
    /// # Errors
    ///
    /// Rejects an output or domain handle outside the active exact closure or a regressed clock.
    pub fn output_effect(
        &mut self,
        handle: LocalHandle,
        now_ns: u64,
    ) -> Result<FallbackEffect, FallbackError> {
        self.validate_monotonic(now_ns)?;
        let output = self
            .active
            .outputs
            .iter()
            .find(|output| output.specification.binding.handle() == handle)
            .ok_or(FallbackError::OutputClosureMismatch)?;
        let runtime = self
            .runtimes
            .get(usize::from(output.specification.domain.get()))
            .ok_or(FallbackError::DomainClosureMismatch)?;
        if runtime.state == FallbackDomainState::Normal {
            self.last_monotonic_ns = Some(now_ns);
            return Ok(FallbackEffect::Normal);
        }
        let effect = if runtime.state == FallbackDomainState::GuardianUnavailable {
            match output.specification.protection {
                ProtectionEvidence::Guardian => FallbackEffect::GuardianUnavailable,
                ProtectionEvidence::DeviceWatchdog { preset, .. } => {
                    FallbackEffect::DeviceWatchdogPreset(preset)
                }
                ProtectionEvidence::ExternalSafety { .. } => FallbackEffect::ExternalProtection,
            }
        } else {
            effect_for_action(output.specification.action, runtime.fault_since_ns, now_ns)
        };
        self.last_monotonic_ns = Some(now_ns);
        Ok(effect)
    }

    /// Starts one bounded recovery attempt using a new generation, lease, and reinit proof.
    ///
    /// # Errors
    ///
    /// Rejects foreign domains, invalid states, early/exhausted attempts, time regression, or
    /// reused/skipped recovery identities.
    pub fn begin_recovery(
        &mut self,
        domain: FallbackDomainHandle,
        evidence: ReinitializationEvidence,
        now_ns: u64,
    ) -> Result<(), FallbackError> {
        self.validate_monotonic(now_ns)?;
        let index = usize::from(domain.get());
        let specification = *self
            .active
            .domains
            .get(index)
            .ok_or(FallbackError::DomainClosureMismatch)?;
        let runtime = self
            .runtimes
            .get_mut(index)
            .ok_or(FallbackError::DomainClosureMismatch)?;
        if runtime.state == FallbackDomainState::RecoveryLocked {
            return Err(FallbackError::RecoveryLocked);
        }
        if runtime.state != FallbackDomainState::Fallback {
            return Err(FallbackError::InvalidStateTransition);
        }
        if now_ns < runtime.next_recovery_ns {
            return Err(FallbackError::RecoveryTooEarly);
        }
        let window_deadline = match runtime.recovery_window_deadline_ns {
            Some(value) => value,
            None => now_ns
                .checked_add(specification.recovery.window_ns)
                .ok_or(FallbackError::ArithmeticOverflow)?,
        };
        if now_ns >= window_deadline
            || runtime.recovery_attempts >= specification.recovery.maximum_attempts
        {
            runtime.state = FallbackDomainState::RecoveryLocked;
            return Err(FallbackError::RecoveryLocked);
        }
        validate_reinitialization(runtime, evidence)?;
        runtime.recovery_window_deadline_ns = Some(window_deadline);
        runtime.recovery_attempts = runtime.recovery_attempts.saturating_add(1);
        runtime.last_candidate_generation = Some(evidence.identity.configuration().generation());
        runtime.last_candidate_lease = Some(evidence.identity.lease_id());
        runtime.candidate = Some(RecoveryCandidate {
            evidence,
            health_start_ns: None,
            health_observations: 0,
        });
        runtime.state = FallbackDomainState::RecoveryChecking;
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Records one recovery health observation and enforces bounded retry/authorization rules.
    ///
    /// # Errors
    ///
    /// Rejects foreign domains, invalid state, time regression, or unrepresentable backoff.
    pub fn observe_recovery_health(
        &mut self,
        domain: FallbackDomainHandle,
        now_ns: u64,
        healthy: bool,
    ) -> Result<RecoveryProgress, FallbackError> {
        self.validate_monotonic(now_ns)?;
        let index = usize::from(domain.get());
        let specification = *self
            .active
            .domains
            .get(index)
            .ok_or(FallbackError::DomainClosureMismatch)?;
        let runtime = self
            .runtimes
            .get_mut(index)
            .ok_or(FallbackError::DomainClosureMismatch)?;
        if runtime.state != FallbackDomainState::RecoveryChecking {
            return Err(FallbackError::InvalidStateTransition);
        }
        let window_deadline = runtime
            .recovery_window_deadline_ns
            .ok_or(FallbackError::InvalidStateTransition)?;
        if !healthy || now_ns >= window_deadline {
            let progress = fail_recovery(runtime, specification.recovery, now_ns);
            self.last_monotonic_ns = Some(now_ns);
            return Ok(progress);
        }
        let candidate = runtime
            .candidate
            .as_mut()
            .ok_or(FallbackError::InvalidStateTransition)?;
        if candidate.health_start_ns.is_none() {
            candidate.health_start_ns = Some(now_ns);
        }
        candidate.health_observations = candidate.health_observations.saturating_add(1);
        let duration = now_ns
            .checked_sub(
                candidate
                    .health_start_ns
                    .ok_or(FallbackError::InvalidStateTransition)?,
            )
            .ok_or(FallbackError::MonotonicTimeRegression)?;
        if candidate.health_observations < specification.recovery.health.required_observations
            || duration < specification.recovery.health.minimum_duration_ns
        {
            self.last_monotonic_ns = Some(now_ns);
            return Ok(RecoveryProgress::Checking);
        }
        if specification.risk == FallbackDomainRisk::Hazardous {
            runtime.state = FallbackDomainState::AwaitingAuthorization;
            self.last_monotonic_ns = Some(now_ns);
            return Ok(RecoveryProgress::AwaitingAuthorization);
        }
        let identity = candidate.evidence.identity;
        runtime.current_identity = identity;
        runtime.candidate = None;
        runtime.state = FallbackDomainState::Normal;
        runtime.cause = None;
        runtime.fault_since_ns = None;
        self.last_monotonic_ns = Some(now_ns);
        Ok(RecoveryProgress::Recovered)
    }

    /// Releases a healthy hazardous domain only with exact authorization and safety permission.
    ///
    /// # Errors
    ///
    /// Rejects ordinary/foreign domains, wrong candidate identity, missing authorization, or time
    /// regression.
    pub fn authorize_hazardous_recovery(
        &mut self,
        domain: FallbackDomainHandle,
        authorization: RecoveryAuthorization,
        now_ns: u64,
    ) -> Result<(), FallbackError> {
        self.validate_monotonic(now_ns)?;
        let index = usize::from(domain.get());
        let specification = self
            .active
            .domains
            .get(index)
            .ok_or(FallbackError::DomainClosureMismatch)?;
        let runtime = self
            .runtimes
            .get_mut(index)
            .ok_or(FallbackError::DomainClosureMismatch)?;
        let candidate = runtime
            .candidate
            .ok_or(FallbackError::RecoveryAuthorizationRequired)?;
        let deadline = runtime
            .recovery_window_deadline_ns
            .ok_or(FallbackError::RecoveryAuthorizationRequired)?;
        if now_ns >= deadline {
            runtime.state = FallbackDomainState::RecoveryLocked;
            runtime.candidate = None;
            return Err(FallbackError::RecoveryLocked);
        }
        if specification.risk != FallbackDomainRisk::Hazardous
            || runtime.state != FallbackDomainState::AwaitingAuthorization
            || authorization.identity != candidate.evidence.identity
        {
            return Err(FallbackError::RecoveryAuthorizationRequired);
        }
        runtime.current_identity = authorization.identity;
        runtime.candidate = None;
        runtime.state = FallbackDomainState::Normal;
        runtime.cause = None;
        runtime.fault_since_ns = None;
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Returns diagnostics for one exact domain.
    #[must_use]
    pub fn diagnostics(&self, domain: FallbackDomainHandle) -> Option<FallbackDomainDiagnostics> {
        self.runtimes
            .get(usize::from(domain.get()))
            .map(|runtime| FallbackDomainDiagnostics {
                state: runtime.state,
                current_identity: runtime.current_identity,
                cause: runtime.cause,
                fault_since_ns: runtime.fault_since_ns,
                recovery_attempts: runtime.recovery_attempts,
                next_recovery_ns: runtime.next_recovery_ns,
            })
    }

    fn validate_monotonic(&self, now_ns: u64) -> Result<(), FallbackError> {
        if self.last_monotonic_ns.is_some_and(|last| now_ns < last) {
            Err(FallbackError::MonotonicTimeRegression)
        } else {
            Ok(())
        }
    }
}

fn make_runtimes(plan: &FallbackPlan) -> Result<Box<[DomainRuntime]>, FallbackError> {
    let mut runtimes = Vec::new();
    runtimes
        .try_reserve_exact(plan.domains.len())
        .map_err(|_| FallbackError::AllocationFailed)?;
    for _ in &plan.domains {
        runtimes.push(DomainRuntime::new(plan.region.lease_identity()));
    }
    Ok(runtimes.into_boxed_slice())
}

fn pending_health_passed(plan: &FallbackPlan, health: PendingFallbackHealth) -> bool {
    if !health.contains(PendingFallbackHealth::DEVICES | PendingFallbackHealth::PROTOCOLS) {
        return false;
    }
    let requires_watchdog = plan.outputs.iter().any(|output| {
        matches!(
            output.specification.protection,
            ProtectionEvidence::DeviceWatchdog { .. }
        )
    });
    let requires_external = plan.outputs.iter().any(|output| {
        matches!(
            output.specification.protection,
            ProtectionEvidence::ExternalSafety { .. }
        )
    });
    (!requires_watchdog || health.contains(PendingFallbackHealth::WATCHDOGS))
        && (!requires_external || health.contains(PendingFallbackHealth::EXTERNAL_PROTECTION))
}

fn enter_domain_fallback(
    runtime: &mut DomainRuntime,
    recovery: DomainRecoveryPolicy,
    cause: FallbackCause,
    now_ns: u64,
) {
    if cause == FallbackCause::GuardianProcessLost {
        runtime.state = FallbackDomainState::GuardianUnavailable;
        runtime.cause = Some(cause);
        runtime.fault_since_ns.get_or_insert(now_ns);
        runtime.candidate = None;
        return;
    }
    if runtime.state == FallbackDomainState::GuardianUnavailable {
        runtime.fault_since_ns.get_or_insert(now_ns);
        runtime.candidate = None;
        return;
    }
    if runtime.state == FallbackDomainState::RecoveryLocked {
        runtime.cause = Some(cause);
        runtime.fault_since_ns.get_or_insert(now_ns);
        return;
    }
    if runtime.state == FallbackDomainState::Normal {
        runtime.recovery_window_deadline_ns = None;
        runtime.recovery_attempts = 0;
        runtime.next_recovery_ns = now_ns;
        runtime.last_candidate_generation = None;
        runtime.last_candidate_lease = None;
    } else if matches!(
        runtime.state,
        FallbackDomainState::RecoveryChecking | FallbackDomainState::AwaitingAuthorization
    ) {
        let next = now_ns.checked_add(recovery.backoff_ns);
        if runtime.recovery_attempts >= recovery.maximum_attempts
            || next.is_none_or(|value| {
                runtime
                    .recovery_window_deadline_ns
                    .is_none_or(|deadline| value >= deadline)
            })
        {
            runtime.state = FallbackDomainState::RecoveryLocked;
        } else if let Some(next) = next {
            runtime.state = FallbackDomainState::Fallback;
            runtime.next_recovery_ns = next;
        }
    }
    if runtime.state != FallbackDomainState::RecoveryLocked {
        runtime.state = FallbackDomainState::Fallback;
    }
    runtime.cause = Some(cause);
    runtime.fault_since_ns.get_or_insert(now_ns);
    runtime.candidate = None;
}

fn effect_for_action(
    action: FallbackAction,
    fault_since_ns: Option<u64>,
    now_ns: u64,
) -> FallbackEffect {
    match action {
        FallbackAction::SetFixed(value) => FallbackEffect::SetFixed(value),
        FallbackAction::HoldLastThenFixed { hold_ns, fixed } => {
            let deadline = fault_since_ns.and_then(|start| start.checked_add(hold_ns));
            match deadline {
                Some(value) if now_ns < value => FallbackEffect::HoldLastUntil(value),
                Some(_) | None => FallbackEffect::SetFixed(fixed),
            }
        }
        FallbackAction::DeviceWatchdogPreset { preset } => {
            FallbackEffect::DeviceWatchdogPreset(preset)
        }
    }
}

fn validate_reinitialization(
    runtime: &DomainRuntime,
    evidence: ReinitializationEvidence,
) -> Result<(), FallbackError> {
    let current = runtime.current_identity.configuration();
    let candidate = evidence.identity.configuration();
    let previous_generation = runtime
        .last_candidate_generation
        .unwrap_or(current.generation());
    let expected_generation = previous_generation
        .checked_next()
        .map_err(|_| FallbackError::ArithmeticOverflow)?;
    let lease_reused = evidence.identity.lease_id() == runtime.current_identity.lease_id()
        || runtime
            .last_candidate_lease
            .is_some_and(|lease| lease == evidence.identity.lease_id());
    if candidate.epoch() != current.epoch()
        || candidate.layout_digest() != current.layout_digest()
        || candidate.generation() != expected_generation
        || lease_reused
    {
        return Err(FallbackError::ReinitializationRequired);
    }
    Ok(())
}

fn fail_recovery(
    runtime: &mut DomainRuntime,
    policy: DomainRecoveryPolicy,
    now_ns: u64,
) -> RecoveryProgress {
    runtime.candidate = None;
    let next = now_ns.checked_add(policy.backoff_ns);
    if runtime.recovery_attempts >= policy.maximum_attempts
        || next.is_none_or(|value| {
            runtime
                .recovery_window_deadline_ns
                .is_none_or(|deadline| value >= deadline)
        })
    {
        runtime.state = FallbackDomainState::RecoveryLocked;
        RecoveryProgress::Locked
    } else if let Some(next) = next {
        runtime.state = FallbackDomainState::Fallback;
        runtime.next_recovery_ns = next;
        RecoveryProgress::RetryScheduled
    } else {
        runtime.state = FallbackDomainState::RecoveryLocked;
        RecoveryProgress::Locked
    }
}

/// Device-native watchdog simulator state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeviceWatchdogState {
    /// No preset deadline is active.
    Disarmed,
    /// A finite deadline is active.
    Armed,
    /// Deadline expired and the device preset is latched.
    PresetApplied,
}

/// Deterministic adapter/simulator for one reviewed device-native watchdog.
pub struct DeviceWatchdogSimulator {
    timeout_ns: u64,
    preset: FallbackDigest,
    state: DeviceWatchdogState,
    deadline_ns: Option<u64>,
    last_monotonic_ns: Option<u64>,
}

impl DeviceWatchdogSimulator {
    /// Creates a disarmed simulator from exact device watchdog evidence.
    ///
    /// # Errors
    ///
    /// Rejects a zero timeout.
    pub const fn new(timeout_ns: u64, preset: FallbackDigest) -> Result<Self, FallbackError> {
        if timeout_ns == 0 {
            return Err(FallbackError::ProtectionUnavailable);
        }
        Ok(Self {
            timeout_ns,
            preset,
            state: DeviceWatchdogState::Disarmed,
            deadline_ns: None,
            last_monotonic_ns: None,
        })
    }

    /// Arms the watchdog with an absolute monotonic deadline.
    ///
    /// # Errors
    ///
    /// Rejects an already armed/latched simulator or deadline overflow.
    pub fn arm(&mut self, now_ns: u64) -> Result<(), FallbackError> {
        self.validate_monotonic(now_ns)?;
        if self.state != DeviceWatchdogState::Disarmed {
            return Err(FallbackError::InvalidStateTransition);
        }
        let deadline = now_ns
            .checked_add(self.timeout_ns)
            .ok_or(FallbackError::ArithmeticOverflow)?;
        self.state = DeviceWatchdogState::Armed;
        self.deadline_ns = Some(deadline);
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Records one valid kick before the current deadline.
    ///
    /// # Errors
    ///
    /// Rejects a disarmed/expired watchdog, time regression, or deadline overflow.
    pub fn kick(&mut self, now_ns: u64) -> Result<(), FallbackError> {
        self.validate_monotonic(now_ns)?;
        let deadline = self
            .deadline_ns
            .ok_or(FallbackError::InvalidStateTransition)?;
        if self.state != DeviceWatchdogState::Armed || now_ns >= deadline {
            self.state = DeviceWatchdogState::PresetApplied;
            self.last_monotonic_ns = Some(now_ns);
            return Err(FallbackError::ProtectionUnavailable);
        }
        self.deadline_ns = Some(
            now_ns
                .checked_add(self.timeout_ns)
                .ok_or(FallbackError::ArithmeticOverflow)?,
        );
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Applies the preset exactly when the deadline is reached.
    ///
    /// # Errors
    ///
    /// Rejects a monotonic time regression.
    pub fn observe(&mut self, now_ns: u64) -> Result<DeviceWatchdogState, FallbackError> {
        self.validate_monotonic(now_ns)?;
        if self.state == DeviceWatchdogState::Armed
            && self.deadline_ns.is_some_and(|deadline| now_ns >= deadline)
        {
            self.state = DeviceWatchdogState::PresetApplied;
        }
        self.last_monotonic_ns = Some(now_ns);
        Ok(self.state)
    }

    /// Returns the exact preset digest.
    #[must_use]
    pub const fn preset(&self) -> FallbackDigest {
        self.preset
    }

    /// Reinitializes a latched watchdog; it remains disarmed until explicitly armed again.
    ///
    /// # Errors
    ///
    /// Rejects every state except [`DeviceWatchdogState::PresetApplied`].
    pub fn reinitialize(&mut self) -> Result<(), FallbackError> {
        if self.state != DeviceWatchdogState::PresetApplied {
            return Err(FallbackError::InvalidStateTransition);
        }
        self.state = DeviceWatchdogState::Disarmed;
        self.deadline_ns = None;
        Ok(())
    }

    fn validate_monotonic(&self, now_ns: u64) -> Result<(), FallbackError> {
        if self.last_monotonic_ns.is_some_and(|last| now_ns < last) {
            Err(FallbackError::MonotonicTimeRegression)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::FallbackCause;
    use crate::ProtocolSourceKind;

    #[test]
    fn protocol_failure_catalog_is_exact_and_global_causes_never_match_a_source() {
        let ethercat = [
            FallbackCause::EthercatWorkingCounter,
            FallbackCause::EthercatDistributedClock,
            FallbackCause::EthercatApplicationLayer,
        ];
        for cause in ethercat {
            assert!(cause.matches_protocol(ProtocolSourceKind::Ethercat));
            assert!(!cause.matches_protocol(ProtocolSourceKind::Can));
        }
        let modbus = [FallbackCause::ModbusTimeout, FallbackCause::ModbusException];
        for cause in modbus {
            assert!(cause.matches_protocol(ProtocolSourceKind::ModbusTcp));
            assert!(cause.matches_protocol(ProtocolSourceKind::ModbusRtu));
            assert!(!cause.matches_protocol(ProtocolSourceKind::Serial));
        }
        let serial = [FallbackCause::SerialCrc, FallbackCause::SerialFraming];
        for cause in serial {
            assert!(cause.matches_protocol(ProtocolSourceKind::ModbusRtu));
            assert!(cause.matches_protocol(ProtocolSourceKind::Serial));
            assert!(!cause.matches_protocol(ProtocolSourceKind::Can));
        }
        for cause in [FallbackCause::CanError, FallbackCause::CanBusOff] {
            assert!(cause.matches_protocol(ProtocolSourceKind::Can));
            assert!(!cause.matches_protocol(ProtocolSourceKind::Lin));
        }
        for cause in [
            FallbackCause::LinSchedule,
            FallbackCause::LinResponseTimeout,
        ] {
            assert!(cause.matches_protocol(ProtocolSourceKind::Lin));
            assert!(!cause.matches_protocol(ProtocolSourceKind::Can));
        }
        for cause in [
            FallbackCause::DriverHostLost,
            FallbackCause::OutputStale,
            FallbackCause::QueueFull,
        ] {
            for protocol in [
                ProtocolSourceKind::Ethercat,
                ProtocolSourceKind::ModbusTcp,
                ProtocolSourceKind::ModbusRtu,
                ProtocolSourceKind::Serial,
                ProtocolSourceKind::Can,
                ProtocolSourceKind::Lin,
            ] {
                assert!(cause.matches_protocol(protocol));
            }
        }
        for cause in [
            FallbackCause::ControlProcessLost,
            FallbackCause::ControlHeartbeatExpired,
            FallbackCause::LeaseRevoked,
            FallbackCause::GuardianProcessLost,
        ] {
            assert!(cause.is_global());
            assert!(!cause.matches_protocol(ProtocolSourceKind::Ethercat));
        }
    }
}
