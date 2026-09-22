//! Admission and fault-isolation model for one sandboxed Driver Host per fixed instance.

use aurora_io_guardian::GroupDescriptor;
use aurora_io_guardian_contracts::{PeerCredentials, PeerPolicy};

use crate::model::copy_boxed;
use crate::{
    DeviceIdentity, DriverAuthority, DriverExecutionMode, DriverFaultKind, DriverInstanceHandle,
    DriverInstancePlan, DriverSdkError, EvidenceDigest, InterfaceIdentity, SandboxDigest,
};

/// Non-zero process generation for one fixed Driver Host instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DriverHostGeneration(u64);

impl DriverHostGeneration {
    /// Creates a non-zero generation.
    ///
    /// # Errors
    ///
    /// Rejects zero so an absent generation cannot authorize a host message.
    pub const fn new(value: u64) -> Result<Self, DriverSdkError> {
        if value == 0 {
            Err(DriverSdkError::InvalidCapacity)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the encoded generation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Returns the exact next restart generation without wrapping.
    ///
    /// # Errors
    ///
    /// Rejects counter overflow.
    pub const fn checked_next(self) -> Result<Self, DriverSdkError> {
        match self.0.checked_add(1) {
            Some(value) => Ok(Self(value)),
            None => Err(DriverSdkError::ArithmeticOverflow),
        }
    }
}

/// Linux capability names allowed in an explicit minimal profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LinuxCapability {
    /// Raw packet socket access for an approved network interface.
    NetRaw,
    /// Network-interface configuration for an approved dedicated interface.
    NetAdmin,
    /// Bounded scheduler-priority setup approved by the Target Profile.
    SysNice,
    /// Memory locking approved by the Target Profile.
    IpcLock,
}

/// Required namespace isolation switches for one host process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NamespaceIsolation(u8);

impl NamespaceIsolation {
    const USER: u8 = 1 << 0;
    const MOUNT: u8 = 1 << 1;
    const PID: u8 = 1 << 2;
    const IPC: u8 = 1 << 3;
    const NETWORK: u8 = 1 << 4;
    const ALL: u8 = Self::USER | Self::MOUNT | Self::PID | Self::IPC | Self::NETWORK;

    /// Complete user/mount/PID/IPC/network isolation required by R3-05.
    pub const COMPLETE: Self = Self(Self::ALL);

    /// Creates a namespace set from the fixed representation.
    ///
    /// # Errors
    ///
    /// Rejects unknown bits; incomplete sets remain representable only for rejection tests.
    pub const fn from_raw_bits(bits: u8) -> Result<Self, DriverSdkError> {
        if bits & !Self::ALL == 0 {
            Ok(Self(bits))
        } else {
            Err(DriverSdkError::SandboxUnavailable)
        }
    }

    /// Returns the fixed representation.
    #[must_use]
    pub const fn raw_bits(self) -> u8 {
        self.0
    }

    const fn complete(self) -> bool {
        self.0 == Self::ALL
    }
}

/// One exact approved device in the host's `DeviceAllow` set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceAccessGrant {
    /// Stable device identity.
    pub identity: DeviceIdentity,
    /// Whether bounded reads are allowed.
    pub read: bool,
    /// Whether bounded writes are allowed.
    pub write: bool,
}

/// Fixed Target Profile sandbox policy for one isolated `DriverInstance`.
pub struct SandboxPolicy {
    profile: SandboxDigest,
    peer: PeerPolicy,
    namespaces: NamespaceIsolation,
    seccomp: SandboxDigest,
    controls: EvidenceDigest,
    device_allow: Box<[DeviceAccessGrant]>,
    capabilities: Box<[LinuxCapability]>,
    maximum_memory_bytes: u64,
    maximum_open_files: u32,
    maximum_control_message_bytes: u32,
}

impl SandboxPolicy {
    /// Validates a complete, immutable, least-privilege host policy.
    ///
    /// # Errors
    ///
    /// Rejects root peers, disabled controls, empty/duplicate grants, zero budgets, or allocation
    /// failure. Exact syscall/device application is attested separately by [`AppliedSandbox`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profile: SandboxDigest,
        peer: PeerPolicy,
        peer_credentials: PeerCredentials,
        no_new_privileges: bool,
        read_only_root: bool,
        namespaces: NamespaceIsolation,
        seccomp: SandboxDigest,
        controls: EvidenceDigest,
        device_allow: &[DeviceAccessGrant],
        capabilities: &[LinuxCapability],
        maximum_memory_bytes: u64,
        maximum_open_files: u32,
        maximum_control_message_bytes: u32,
    ) -> Result<Self, DriverSdkError> {
        if peer_credentials.uid() == 0
            || peer_credentials.gid() == 0
            || peer.validate(peer_credentials).is_err()
            || !no_new_privileges
            || !read_only_root
            || !namespaces.complete()
            || device_allow.is_empty()
            || maximum_memory_bytes == 0
            || maximum_open_files == 0
            || maximum_control_message_bytes == 0
            || device_allow
                .windows(2)
                .any(|pair| pair[0].identity >= pair[1].identity)
            || device_allow.iter().any(|grant| !grant.read && !grant.write)
            || capabilities.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(DriverSdkError::SandboxUnavailable);
        }
        Ok(Self {
            profile,
            peer,
            namespaces,
            seccomp,
            controls,
            device_allow: copy_boxed(device_allow)?,
            capabilities: copy_boxed(capabilities)?,
            maximum_memory_bytes,
            maximum_open_files,
            maximum_control_message_bytes,
        })
    }

    /// Returns the reviewed policy digest.
    #[must_use]
    pub const fn profile_digest(&self) -> SandboxDigest {
        self.profile
    }

    /// Returns the exact granted device set.
    #[must_use]
    pub const fn device_allow(&self) -> &[DeviceAccessGrant] {
        &self.device_allow
    }

    /// Returns the exact Linux capability set.
    #[must_use]
    pub const fn capabilities(&self) -> &[LinuxCapability] {
        &self.capabilities
    }
}

/// Evidence reported by the trusted launcher after applying one sandbox policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppliedSandbox {
    /// Exact reviewed policy digest.
    pub profile: SandboxDigest,
    /// Exact seccomp policy digest.
    pub seccomp: SandboxDigest,
    /// Evidence of applied DeviceAllow/capability/namespace/resource controls.
    pub controls: EvidenceDigest,
    /// Observed local peer credentials.
    pub peer: PeerCredentials,
    /// Applied `NoNewPrivileges` state.
    pub no_new_privileges: bool,
    /// Applied read-only root filesystem state.
    pub read_only_root: bool,
    /// Applied namespace state.
    pub namespaces: NamespaceIsolation,
    /// Applied memory limit in bytes.
    pub maximum_memory_bytes: u64,
    /// Applied open-file limit.
    pub maximum_open_files: u32,
    /// Applied UDS control-message byte limit.
    pub maximum_control_message_bytes: u32,
}

/// Exact shared-region subset visible to one Driver Host.
pub struct SharedSlotGrant {
    instance: DriverInstanceHandle,
    interface: InterfaceIdentity,
    groups: Box<[GroupDescriptor]>,
}

impl SharedSlotGrant {
    /// Derives the only accessible slots from an already validated instance plan.
    ///
    /// # Errors
    ///
    /// Returns a bounded allocation failure.
    pub fn from_plan(plan: &DriverInstancePlan) -> Result<Self, DriverSdkError> {
        let mut groups = Vec::new();
        groups
            .try_reserve_exact(plan.groups().len())
            .map_err(|_| DriverSdkError::AllocationFailed)?;
        groups.extend(plan.groups().iter().map(|binding| binding.descriptor()));
        Ok(Self {
            instance: plan.authority().instance(),
            interface: plan.authority().interface(),
            groups: groups.into_boxed_slice(),
        })
    }

    /// Returns whether this exact group was granted.
    #[must_use]
    pub fn contains(&self, group: GroupDescriptor) -> bool {
        self.groups.contains(&group)
    }
}

/// Observable lifecycle of an admitted isolated host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverHostState {
    /// Sandbox and grants are admitted; process has not reported ready.
    Admitted,
    /// Host is alive under its exact instance authority.
    Running,
    /// Host is isolated after a normalized fault.
    Faulted(DriverFaultKind),
    /// Host has stopped and cannot be revived in place.
    Stopped,
}

/// Bounded supervisor event for one host process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverHostEvent {
    /// Process passed peer/contract/configuration admission.
    Started,
    /// Valid heartbeat on the local control channel.
    Heartbeat,
    /// Process exited unexpectedly.
    Crashed,
    /// Process exceeded a deadline or heartbeat boundary.
    TimedOut,
    /// Supervisor detected a blocking call beyond its bounded allowance.
    BlockingDetected,
    /// Fixed control/data queue was full.
    QueueFull,
    /// Host sent a malformed bounded message.
    MalformedMessage,
    /// Host used an unsupported Adapter contract version.
    ProtocolVersionError,
    /// Process stopped after explicit quiesce/release.
    Stopped,
}

/// One isolated process boundary; it never exposes Control Engine memory or backend pointers.
pub struct DriverHostBoundary {
    generation: DriverHostGeneration,
    authority: DriverAuthority,
    state: DriverHostState,
    slots: SharedSlotGrant,
    last_monotonic_ns: Option<u64>,
}

impl DriverHostBoundary {
    /// Admits an isolated package only when policy, applied evidence, device grants, and slots are
    /// exact.
    ///
    /// # Errors
    ///
    /// Rejects static packages, sandbox drift, ungranted devices/slots, or insufficient memory.
    pub fn admit(
        plan: &DriverInstancePlan,
        generation: DriverHostGeneration,
        policy: &SandboxPolicy,
        applied: AppliedSandbox,
        slots: SharedSlotGrant,
    ) -> Result<Self, DriverSdkError> {
        if plan.package().mode() != DriverExecutionMode::IsolatedProcess
            || applied.profile != policy.profile
            || applied.seccomp != policy.seccomp
            || applied.controls != policy.controls
            || !applied.no_new_privileges
            || !applied.read_only_root
            || applied.namespaces != policy.namespaces
            || applied.maximum_memory_bytes != policy.maximum_memory_bytes
            || applied.maximum_open_files != policy.maximum_open_files
            || applied.maximum_control_message_bytes != policy.maximum_control_message_bytes
            || policy.peer.validate(applied.peer).is_err()
            || applied.maximum_memory_bytes < plan.limits().maximum_memory_bytes
            || slots.instance != plan.authority().instance()
            || slots.interface != plan.authority().interface()
            || slots.groups.len() != plan.groups().len()
            || plan
                .groups()
                .iter()
                .any(|binding| !slots.contains(binding.descriptor()))
            || policy.device_allow.len() != plan.devices().len()
            || plan.devices().iter().any(|identity| {
                !policy
                    .device_allow
                    .iter()
                    .any(|grant| grant.identity == *identity)
            })
        {
            return Err(DriverSdkError::SandboxUnavailable);
        }
        Ok(Self {
            generation,
            authority: plan.authority(),
            state: DriverHostState::Admitted,
            slots,
            last_monotonic_ns: None,
        })
    }

    /// Applies one event to only this host boundary.
    ///
    /// # Errors
    ///
    /// Rejects time regression or invalid resurrection/state transitions.
    pub fn observe(
        &mut self,
        event: DriverHostEvent,
        now_ns: u64,
    ) -> Result<DriverHostState, DriverSdkError> {
        if self.last_monotonic_ns.is_some_and(|last| now_ns < last) {
            return Err(DriverSdkError::MonotonicTimeRegression);
        }
        self.state = match (self.state, event) {
            (DriverHostState::Admitted, DriverHostEvent::Started)
            | (DriverHostState::Running, DriverHostEvent::Heartbeat) => DriverHostState::Running,
            (DriverHostState::Admitted | DriverHostState::Running, DriverHostEvent::Crashed) => {
                DriverHostState::Faulted(DriverFaultKind::Crashed)
            }
            (DriverHostState::Admitted | DriverHostState::Running, DriverHostEvent::TimedOut) => {
                DriverHostState::Faulted(DriverFaultKind::DeadlineExceeded)
            }
            (
                DriverHostState::Admitted | DriverHostState::Running,
                DriverHostEvent::BlockingDetected,
            ) => DriverHostState::Faulted(DriverFaultKind::BlockingDetected),
            (DriverHostState::Admitted | DriverHostState::Running, DriverHostEvent::QueueFull) => {
                DriverHostState::Faulted(DriverFaultKind::QueueFull)
            }
            (
                DriverHostState::Admitted | DriverHostState::Running,
                DriverHostEvent::MalformedMessage,
            ) => DriverHostState::Faulted(DriverFaultKind::MalformedFrame),
            (
                DriverHostState::Admitted | DriverHostState::Running,
                DriverHostEvent::ProtocolVersionError,
            ) => DriverHostState::Faulted(DriverFaultKind::ProtocolVersionMismatch),
            (
                DriverHostState::Admitted | DriverHostState::Running | DriverHostState::Faulted(_),
                DriverHostEvent::Stopped,
            ) => DriverHostState::Stopped,
            _ => return Err(DriverSdkError::InvalidStateTransition),
        };
        self.last_monotonic_ns = Some(now_ns);
        Ok(self.state)
    }

    /// Returns this boundary's exact instance.
    #[must_use]
    pub const fn instance(&self) -> DriverInstanceHandle {
        self.authority.instance()
    }

    /// Returns the process generation that every host message must carry.
    #[must_use]
    pub const fn generation(&self) -> DriverHostGeneration {
        self.generation
    }

    /// Returns current isolated state.
    #[must_use]
    pub const fn state(&self) -> DriverHostState {
        self.state
    }

    /// Checks access to one exact granted group.
    ///
    /// # Errors
    ///
    /// Rejects every group outside this host's shared slot grant.
    pub fn authorize_group(
        &self,
        generation: DriverHostGeneration,
        authority: DriverAuthority,
        group: GroupDescriptor,
    ) -> Result<(), DriverSdkError> {
        if self.state == DriverHostState::Running
            && generation == self.generation
            && authority == self.authority
            && self.slots.contains(group)
        {
            Ok(())
        } else {
            Err(DriverSdkError::UnauthorizedResource)
        }
    }
}

/// Fixed registry proving one owner per interface and fault isolation between hosts.
pub struct DriverHostRegistry {
    hosts: Box<[DriverHostBoundary]>,
}

impl DriverHostRegistry {
    /// Takes ownership of a complete dense instance catalog.
    ///
    /// # Errors
    ///
    /// Rejects zero/excess hosts, non-dense instances, or duplicate interface ownership.
    pub fn new(hosts: Vec<DriverHostBoundary>, maximum_hosts: u16) -> Result<Self, DriverSdkError> {
        if hosts.is_empty() || maximum_hosts == 0 || hosts.len() > usize::from(maximum_hosts) {
            return Err(DriverSdkError::InvalidCapacity);
        }
        for (index, host) in hosts.iter().enumerate() {
            let expected = u16::try_from(index).map_err(|_| DriverSdkError::InvalidCapacity)?;
            if host.instance().get() != expected
                || hosts[..index]
                    .iter()
                    .any(|prior| prior.authority.interface() == host.authority.interface())
            {
                return Err(DriverSdkError::DeviceAlreadyOwned);
            }
        }
        Ok(Self {
            hosts: hosts.into_boxed_slice(),
        })
    }

    /// Applies an event only to the addressed host.
    ///
    /// # Errors
    ///
    /// Rejects foreign instance handles and forwards host transition errors.
    pub fn observe(
        &mut self,
        instance: DriverInstanceHandle,
        event: DriverHostEvent,
        now_ns: u64,
    ) -> Result<DriverHostState, DriverSdkError> {
        self.hosts
            .get_mut(usize::from(instance.get()))
            .ok_or(DriverSdkError::UnauthorizedResource)?
            .observe(event, now_ns)
    }

    /// Returns one exact host state.
    #[must_use]
    pub fn state(&self, instance: DriverInstanceHandle) -> Option<DriverHostState> {
        self.hosts
            .get(usize::from(instance.get()))
            .map(DriverHostBoundary::state)
    }

    /// Authorizes one running host message against generation, authority, and shared slot.
    ///
    /// # Errors
    ///
    /// Rejects foreign instances, stale generations, inactive hosts, authority drift, and
    /// ungranted groups.
    pub fn authorize_group(
        &self,
        instance: DriverInstanceHandle,
        generation: DriverHostGeneration,
        authority: DriverAuthority,
        group: GroupDescriptor,
    ) -> Result<(), DriverSdkError> {
        self.hosts
            .get(usize::from(instance.get()))
            .ok_or(DriverSdkError::UnauthorizedResource)?
            .authorize_group(generation, authority, group)
    }

    /// Replaces one stopped process with a fully re-admitted exact-next generation.
    ///
    /// # Errors
    ///
    /// Rejects in-place resurrection, skipped/reused generations, identity drift, or foreign
    /// instance handles. The replacement must already have passed complete sandbox admission.
    pub fn replace_stopped(
        &mut self,
        replacement: DriverHostBoundary,
    ) -> Result<(), DriverSdkError> {
        let index = usize::from(replacement.instance().get());
        let current = self
            .hosts
            .get(index)
            .ok_or(DriverSdkError::UnauthorizedResource)?;
        if current.state != DriverHostState::Stopped
            || replacement.state != DriverHostState::Admitted
            || replacement.generation != current.generation.checked_next()?
            || replacement.authority != current.authority
        {
            return Err(DriverSdkError::AuthorityMismatch);
        }
        self.hosts[index] = replacement;
        Ok(())
    }
}
