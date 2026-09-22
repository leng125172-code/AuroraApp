//! Immutable package inventory, authority, group closure, and work limits.

use aurora_io_guardian::{
    CapabilityDigest, GroupDescriptor, ImageMapping, ProtocolSourceKind, RegionHeader, SourceHandle,
};
use aurora_io_guardian_contracts::{LeaseIdentity, LeaseSequence};

use crate::DriverSdkError;

macro_rules! digest_type {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Creates a non-zero SHA-256 digest.
            ///
            /// # Errors
            ///
            /// Rejects the all-zero value so absent evidence cannot look approved.
            pub const fn new(bytes: [u8; 32]) -> Result<Self, DriverSdkError> {
                let mut index = 0;
                while index < bytes.len() {
                    if bytes[index] != 0 {
                        return Ok(Self(bytes));
                    }
                    index += 1;
                }
                Err(DriverSdkError::CatalogMismatch)
            }

            /// Returns the exact digest bytes.
            #[must_use]
            pub const fn to_sha256(self) -> [u8; 32] {
                self.0
            }
        }
    };
}

digest_type!(BackendDigest, "Stable normalized backend identity digest.");
digest_type!(SourceDigest, "Reviewed backend source/package digest.");
digest_type!(
    InterfaceIdentity,
    "Stable physical-interface identity digest."
);
digest_type!(DeviceIdentity, "Stable approved-device identity digest.");
digest_type!(
    DeviceCatalogDigest,
    "Stable normalized device/topology catalog digest."
);
digest_type!(SandboxDigest, "Reviewed sandbox/service profile digest.");
digest_type!(
    EvidenceDigest,
    "Immutable runtime admission evidence digest."
);

/// Exact normalized backend, reviewed source, and capability identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BackendIdentity {
    backend: BackendDigest,
    source: SourceDigest,
    capability: CapabilityDigest,
}

impl BackendIdentity {
    /// Creates one immutable backend identity tuple.
    #[must_use]
    pub const fn new(
        backend: BackendDigest,
        source: SourceDigest,
        capability: CapabilityDigest,
    ) -> Self {
        Self {
            backend,
            source,
            capability,
        }
    }

    /// Returns the normalized backend identity.
    #[must_use]
    pub const fn backend(self) -> BackendDigest {
        self.backend
    }

    /// Returns the reviewed source/package digest.
    #[must_use]
    pub const fn source(self) -> SourceDigest {
        self.source
    }

    /// Returns the normalized capability digest.
    #[must_use]
    pub const fn capability(self) -> CapabilityDigest {
        self.capability
    }
}

/// Dense handle for one installed backend package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BackendPackageHandle(u16);

impl BackendPackageHandle {
    /// Creates a handle whose ordering is validated by the inventory.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the compact value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Dense handle for one fixed `DriverInstance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DriverInstanceHandle(u16);

impl DriverInstanceHandle {
    /// Creates a handle whose density is validated by the owner registry.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the compact value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Exact Preview Driver Adapter version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DriverContractVersion {
    major: u16,
    minor: u16,
}

impl DriverContractVersion {
    /// Current R3 Driver Adapter contract version.
    pub const PREVIEW_1_0: Self = Self { major: 1, minor: 0 };

    /// Validates the only version implemented by R3-05.
    ///
    /// # Errors
    ///
    /// Rejects unknown major or minor versions without downgrade.
    pub const fn new(major: u16, minor: u16) -> Result<Self, DriverSdkError> {
        if major == 1 && minor == 0 {
            Ok(Self { major, minor })
        } else {
            Err(DriverSdkError::DriverFault(
                crate::DriverFaultKind::ProtocolVersionMismatch,
            ))
        }
    }

    /// Returns the major component.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the minor component.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }
}

/// Build-fixed execution boundary for a driver package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverExecutionMode {
    /// Audited first-party safe Rust driver linked into Guardian.
    StaticLinked,
    /// Driver running in one separately sandboxed process instance.
    IsolatedProcess,
}

/// Implementation risk that determines whether static execution is permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverImplementationKind {
    /// Audited first-party safe Rust with statically proven bounded cyclic work.
    FirstPartySafeRustBounded,
    /// Backend uses C FFI.
    CForeignFunctionInterface,
    /// Backend is coupled to a kernel module or kernel master.
    KernelCoupled,
    /// Backend calls a vendor SDK.
    VendorSdk,
    /// Backend can block outside the bounded cyclic contract.
    PotentiallyBlocking,
}

/// One installed backend package known at construction time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BackendPackage {
    handle: BackendPackageHandle,
    identity: BackendIdentity,
    protocol: ProtocolSourceKind,
    version: DriverContractVersion,
    implementation: DriverImplementationKind,
    mode: DriverExecutionMode,
}

impl BackendPackage {
    /// Creates a package and enforces the risk-derived isolation boundary.
    ///
    /// # Errors
    ///
    /// Rejects static execution for C FFI, kernel-coupled, vendor, or potentially blocking code.
    pub const fn new(
        handle: BackendPackageHandle,
        identity: BackendIdentity,
        protocol: ProtocolSourceKind,
        version: DriverContractVersion,
        implementation: DriverImplementationKind,
        mode: DriverExecutionMode,
    ) -> Result<Self, DriverSdkError> {
        if !matches!(
            implementation,
            DriverImplementationKind::FirstPartySafeRustBounded
        ) && matches!(mode, DriverExecutionMode::StaticLinked)
        {
            return Err(DriverSdkError::IsolationRequired);
        }
        Ok(Self {
            handle,
            identity,
            protocol,
            version,
            implementation,
            mode,
        })
    }

    /// Returns the fixed package handle.
    #[must_use]
    pub const fn handle(self) -> BackendPackageHandle {
        self.handle
    }

    /// Returns the selected protocol class.
    #[must_use]
    pub const fn protocol(self) -> ProtocolSourceKind {
        self.protocol
    }

    /// Returns the build-fixed execution mode.
    #[must_use]
    pub const fn mode(self) -> DriverExecutionMode {
        self.mode
    }

    /// Returns the reviewed backend identity.
    #[must_use]
    pub const fn backend_digest(self) -> BackendDigest {
        self.identity.backend()
    }

    /// Returns the reviewed source/package digest.
    #[must_use]
    pub const fn source_digest(self) -> SourceDigest {
        self.identity.source()
    }

    /// Returns the exact normalized capability digest implemented by this package.
    #[must_use]
    pub const fn capability_digest(self) -> CapabilityDigest {
        self.identity.capability()
    }

    /// Returns the normalized Driver Adapter version.
    #[must_use]
    pub const fn version(self) -> DriverContractVersion {
        self.version
    }

    /// Returns the implementation risk class.
    #[must_use]
    pub const fn implementation(self) -> DriverImplementationKind {
        self.implementation
    }
}

/// Immutable installed/approved inventory with one selected package.
pub struct BackendInventory {
    packages: Box<[BackendPackage]>,
    approved: Box<[BackendPackageHandle]>,
    selected: usize,
}

impl BackendInventory {
    /// Closes installed packages against build and Target Profile allowlists.
    ///
    /// # Errors
    ///
    /// Rejects empty, oversized, duplicate/reordered, unbuilt, uninstalled, unapproved, or
    /// multiply selected catalogs. No runtime mutation/discovery API is exposed.
    pub fn new(
        packages: &[BackendPackage],
        build_allowlist: &[BackendPackageHandle],
        approved: &[BackendPackageHandle],
        selected: BackendPackageHandle,
        maximum_packages: u16,
    ) -> Result<Self, DriverSdkError> {
        if packages.is_empty()
            || maximum_packages == 0
            || packages.len() > usize::from(maximum_packages)
            || !strictly_increasing(build_allowlist)
            || !strictly_increasing(approved)
        {
            return Err(DriverSdkError::CatalogMismatch);
        }
        let mut previous = None;
        let mut selected_index = None;
        for (index, package) in packages.iter().enumerate() {
            if previous.is_some_and(|handle| package.handle <= handle)
                || build_allowlist.binary_search(&package.handle).is_err()
            {
                return Err(DriverSdkError::CatalogMismatch);
            }
            if package.handle == selected {
                selected_index = Some(index);
            }
            previous = Some(package.handle);
        }
        if approved.iter().any(|handle| {
            packages
                .binary_search_by_key(handle, |package| package.handle)
                .is_err()
                || build_allowlist.binary_search(handle).is_err()
        }) || approved.binary_search(&selected).is_err()
        {
            return Err(DriverSdkError::PackageNotApproved);
        }
        let selected = selected_index.ok_or(DriverSdkError::PackageNotApproved)?;
        Ok(Self {
            packages: copy_boxed(packages)?,
            approved: copy_boxed(approved)?,
            selected,
        })
    }

    /// Returns the only package eligible to start for this instance.
    #[must_use]
    pub const fn selected(&self) -> BackendPackage {
        self.packages[self.selected]
    }

    /// Returns whether an installed package is approved but deliberately inactive.
    #[must_use]
    pub fn is_approved_but_inactive(&self, handle: BackendPackageHandle) -> bool {
        self.approved.binary_search(&handle).is_ok() && self.selected().handle != handle
    }

    /// Returns the immutable installed package count.
    #[must_use]
    pub fn package_count(&self) -> usize {
        self.packages.len()
    }
}

fn strictly_increasing(values: &[BackendPackageHandle]) -> bool {
    !values.is_empty() && values.windows(2).all(|pair| pair[0] < pair[1])
}

/// Fixed work and memory limits for one `DriverInstance`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DriverLimits {
    /// Maximum device identities in this instance.
    pub maximum_devices: u16,
    /// Maximum exact update groups in this instance.
    pub maximum_groups: u16,
    /// Maximum frame or request bytes per exchange.
    pub maximum_frame_bytes: u32,
    /// Maximum work items in one cyclic call.
    pub maximum_cyclic_work_items: u32,
    /// Maximum work items in one mailbox step.
    pub maximum_mailbox_work_items: u32,
    /// Maximum explicit mailbox attempts, including the first attempt.
    pub maximum_mailbox_attempts: u16,
    /// Fixed diagnostic/trace queue capacity.
    pub diagnostic_capacity: u32,
    /// Fixed memory budget for the driver or Driver Host.
    pub maximum_memory_bytes: u64,
}

impl DriverLimits {
    fn validate(self) -> Result<(), DriverSdkError> {
        if self.maximum_devices == 0
            || self.maximum_groups == 0
            || self.maximum_frame_bytes == 0
            || self.maximum_cyclic_work_items == 0
            || self.maximum_mailbox_work_items == 0
            || self.maximum_mailbox_attempts == 0
            || self.diagnostic_capacity == 0
            || self.maximum_memory_bytes == 0
            || self.minimum_reserved_bytes()? > self.maximum_memory_bytes
        {
            Err(DriverSdkError::InvalidCapacity)
        } else {
            Ok(())
        }
    }

    /// Returns the conservative logical bytes reserved by declared maximum capacities.
    ///
    /// This includes one maximum frame, the complete diagnostic record budget, per-group
    /// sequence state, and approved-device identity storage. The process-level cgroup/RLIMIT
    /// remains authoritative for allocator and backend-private overhead.
    ///
    /// # Errors
    ///
    /// Rejects arithmetic overflow.
    pub fn minimum_reserved_bytes(self) -> Result<u64, DriverSdkError> {
        u64::from(self.maximum_frame_bytes)
            .checked_add(
                u64::from(self.diagnostic_capacity)
                    .checked_mul(48)
                    .ok_or(DriverSdkError::ArithmeticOverflow)?,
            )
            .and_then(|value| value.checked_add(u64::from(self.maximum_groups) * 8))
            .and_then(|value| value.checked_add(u64::from(self.maximum_devices) * 32))
            .ok_or(DriverSdkError::ArithmeticOverflow)
    }
}

/// Exact direction-specific buffer and work budget for one mapped group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DriverGroupBinding {
    descriptor: GroupDescriptor,
    payload_bytes: u32,
    maximum_work_items: u32,
}

impl DriverGroupBinding {
    /// Creates a non-zero fixed group binding.
    ///
    /// # Errors
    ///
    /// Rejects zero payload/work boundaries.
    pub const fn new(
        descriptor: GroupDescriptor,
        payload_bytes: u32,
        maximum_work_items: u32,
    ) -> Result<Self, DriverSdkError> {
        if payload_bytes == 0 || maximum_work_items == 0 {
            Err(DriverSdkError::InvalidCapacity)
        } else {
            Ok(Self {
                descriptor,
                payload_bytes,
                maximum_work_items,
            })
        }
    }

    /// Returns the exact mapped group.
    #[must_use]
    pub const fn descriptor(self) -> GroupDescriptor {
        self.descriptor
    }

    /// Returns the exact input or output payload size in bytes.
    #[must_use]
    pub const fn payload_bytes(self) -> u32 {
        self.payload_bytes
    }

    /// Returns the maximum bounded work items for one exchange.
    #[must_use]
    pub const fn maximum_work_items(self) -> u32 {
        self.maximum_work_items
    }
}

/// Copyable authority attached to every Adapter call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DriverAuthority {
    instance: DriverInstanceHandle,
    package: BackendPackageHandle,
    interface: InterfaceIdentity,
    lease: LeaseIdentity,
    lease_sequence: LeaseSequence,
    capability: CapabilityDigest,
}

impl DriverAuthority {
    /// Returns the owning instance.
    #[must_use]
    pub const fn instance(self) -> DriverInstanceHandle {
        self.instance
    }

    /// Returns the selected package handle.
    #[must_use]
    pub const fn package(self) -> BackendPackageHandle {
        self.package
    }

    /// Returns the exact approved physical interface identity.
    #[must_use]
    pub const fn interface(self) -> InterfaceIdentity {
        self.interface
    }

    /// Returns the exact active lease/configuration identity.
    #[must_use]
    pub const fn lease_identity(self) -> LeaseIdentity {
        self.lease
    }

    /// Returns the exact process-local lease sequence carried by the shared region.
    #[must_use]
    pub const fn lease_sequence(self) -> LeaseSequence {
        self.lease_sequence
    }

    /// Returns the exact normalized capability digest.
    #[must_use]
    pub const fn capability_digest(self) -> CapabilityDigest {
        self.capability
    }
}

/// Immutable, exact `DriverInstance` plan built before any device is opened.
pub struct DriverInstancePlan {
    authority: DriverAuthority,
    package: BackendPackage,
    source: SourceHandle,
    device_catalog: DeviceCatalogDigest,
    devices: Box<[DeviceIdentity]>,
    groups: Box<[DriverGroupBinding]>,
    limits: DriverLimits,
}

impl DriverInstancePlan {
    /// Validates one source's exact mapping closure and all fixed budgets.
    ///
    /// # Errors
    ///
    /// Rejects identity drift, package/protocol mismatch, missing/extra/reordered groups or
    /// devices, capacity excess, and initialization allocation failure.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        region: RegionHeader,
        mapping: ImageMapping<'_>,
        instance: DriverInstanceHandle,
        inventory: &BackendInventory,
        interface: InterfaceIdentity,
        source: SourceHandle,
        device_catalog: DeviceCatalogDigest,
        devices: &[DeviceIdentity],
        groups: &[DriverGroupBinding],
        limits: DriverLimits,
    ) -> Result<Self, DriverSdkError> {
        limits.validate()?;
        if region.layout() != mapping.layout()
            || region.lease_identity().configuration().layout_digest() != mapping.layout_digest()
            || region.capability_digest() != mapping.capability_digest()
        {
            return Err(DriverSdkError::AuthorityMismatch);
        }
        let package = inventory.selected();
        let source_descriptor = mapping
            .sources()
            .get(usize::try_from(source.get()).map_err(|_| DriverSdkError::CatalogMismatch)?)
            .ok_or(DriverSdkError::CatalogMismatch)?;
        if source_descriptor.handle() != source
            || source_descriptor.protocol() != package.protocol
            || source_descriptor.driver_identity() != package.backend_digest().to_sha256()
            || source_descriptor.device_identity() != device_catalog.to_sha256()
            || region.capability_digest() != package.capability_digest()
        {
            return Err(DriverSdkError::CatalogMismatch);
        }
        if devices.is_empty()
            || devices.len() > usize::from(limits.maximum_devices)
            || groups.is_empty()
            || groups.len() > usize::from(limits.maximum_groups)
            || devices.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(DriverSdkError::CatalogMismatch);
        }
        let mapped_groups = mapping
            .groups()
            .iter()
            .filter(|group| group.source() == source);
        let mut expected_count = 0_usize;
        for (mapped, binding) in mapped_groups.zip(groups) {
            if *mapped != binding.descriptor
                || binding.payload_bytes > limits.maximum_frame_bytes
                || binding.maximum_work_items > limits.maximum_cyclic_work_items
            {
                return Err(DriverSdkError::CatalogMismatch);
            }
            expected_count = expected_count
                .checked_add(1)
                .ok_or(DriverSdkError::ArithmeticOverflow)?;
        }
        let actual_mapped_count = mapping
            .groups()
            .iter()
            .filter(|group| group.source() == source)
            .count();
        if expected_count != groups.len() || groups.len() != actual_mapped_count {
            return Err(DriverSdkError::CatalogMismatch);
        }
        let authority = DriverAuthority {
            instance,
            package: package.handle,
            interface,
            lease: region.lease_identity(),
            lease_sequence: region.lease_sequence(),
            capability: region.capability_digest(),
        };
        Ok(Self {
            authority,
            package,
            source,
            device_catalog,
            devices: copy_boxed(devices)?,
            groups: copy_boxed(groups)?,
            limits,
        })
    }

    /// Returns the exact authority required on every Adapter call.
    #[must_use]
    pub const fn authority(&self) -> DriverAuthority {
        self.authority
    }

    /// Returns the selected installed package.
    #[must_use]
    pub const fn package(&self) -> BackendPackage {
        self.package
    }

    /// Returns the exact mapped source.
    #[must_use]
    pub const fn source(&self) -> SourceHandle {
        self.source
    }

    /// Returns the exact normalized device/topology catalog digest.
    #[must_use]
    pub const fn device_catalog_digest(&self) -> DeviceCatalogDigest {
        self.device_catalog
    }

    /// Returns the fixed group catalog.
    #[must_use]
    pub const fn groups(&self) -> &[DriverGroupBinding] {
        &self.groups
    }

    /// Returns the fixed approved device catalog.
    #[must_use]
    pub const fn devices(&self) -> &[DeviceIdentity] {
        &self.devices
    }

    /// Returns the fixed resource limits.
    #[must_use]
    pub const fn limits(&self) -> DriverLimits {
        self.limits
    }

    /// Finds one exact group without accepting a direction-compatible substitute.
    #[must_use]
    pub fn group(&self, descriptor: GroupDescriptor) -> Option<DriverGroupBinding> {
        self.groups
            .iter()
            .find(|binding| binding.descriptor == descriptor)
            .copied()
    }

    pub(crate) fn validate_authority(
        &self,
        authority: DriverAuthority,
    ) -> Result<(), DriverSdkError> {
        if authority == self.authority {
            Ok(())
        } else {
            Err(DriverSdkError::AuthorityMismatch)
        }
    }
}

pub(crate) fn copy_boxed<T: Copy>(values: &[T]) -> Result<Box<[T]>, DriverSdkError> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(values.len())
        .map_err(|_| DriverSdkError::AllocationFailed)?;
    copied.extend_from_slice(values);
    Ok(copied.into_boxed_slice())
}
