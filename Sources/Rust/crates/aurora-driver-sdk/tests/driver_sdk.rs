//! R3-05 Adapter, inventory, simulator, and isolated Driver Host acceptance tests.

use std::error::Error;

use aurora_driver_sdk::{
    AdapterOperation, AppliedSandbox, BackendDigest, BackendIdentity, BackendInventory,
    BackendPackage, BackendPackageHandle, DeterministicSimulator, DeviceAccessGrant,
    DeviceCatalogDigest, DeviceIdentity, DriverAdapter, DriverContractVersion, DriverExecutionMode,
    DriverFaultKind, DriverGroupBinding, DriverHostBoundary, DriverHostEvent, DriverHostGeneration,
    DriverHostRegistry, DriverHostState, DriverImplementationKind, DriverInstanceHandle,
    DriverInstancePlan, DriverLifecycleState, DriverLimits, DriverSdkError, EvidenceDigest,
    ExchangeBuffer, ExchangeRequest, FaultInjection, InterfaceIdentity, LinuxCapability,
    MailboxCancellation, MailboxRequest, NamespaceIsolation, SandboxDigest, SandboxPolicy,
    SharedSlotGrant, SimulationFault, SourceDigest, TraceResult,
};
use aurora_io_guardian::{
    BitOrder, ByteOrder, CapabilityDigest, FallbackCause, FallbackDigest, GroupDescriptor,
    GroupHandle, ImageDirection, ImageLayout, ImageMapping, ProtectionLevel, ProtocolSourceKind,
    RegionHeader, ScalarType, SourceDescriptor, SourceHandle, ValueBinding,
};
use aurora_io_guardian_contracts::{
    ConfigurationDigest, ConfigurationGeneration, GuardianConfiguration, GuardianEpoch,
    LayoutDigest, LeaseId, LeaseIdentity, LeaseSequence, PeerCredentials, PeerPolicy,
};
use aurora_types::{LocalHandle, TagId};

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Fixture {
    region: RegionHeader,
    sources: [SourceDescriptor; 1],
    groups: [GroupDescriptor; 2],
    values: [ValueBinding; 2],
}

impl Fixture {
    fn new(generation: u64, lease_byte: u8) -> TestResult<Self> {
        let layout = ImageLayout::new(4, 4, 1, 1, 1, 1, 8_192)?;
        let configuration = GuardianConfiguration::new(
            GuardianEpoch::new(5)?,
            ConfigurationGeneration::new(generation)?,
            ConfigurationDigest::from_sha256([u8::try_from(generation)?; 32]),
            LayoutDigest::from_sha256([0x22; 32]),
        );
        let region = RegionHeader::new(
            layout,
            LeaseIdentity::new(configuration, LeaseId::new([lease_byte; 16])?),
            LeaseSequence::new(generation)?,
            CapabilityDigest::from_sha256([0x33; 32]),
        );
        let sources = [SourceDescriptor::new(
            SourceHandle::new(0),
            ProtocolSourceKind::Ethercat,
            [0x41; 32],
            [0x51; 32],
        )];
        let groups = [
            GroupDescriptor::new(
                ImageDirection::Input,
                GroupHandle::new(0),
                SourceHandle::new(0),
            ),
            GroupDescriptor::new(
                ImageDirection::Output,
                GroupHandle::new(0),
                SourceHandle::new(0),
            ),
        ];
        let values = [
            binding(0, ImageDirection::Input, None)?,
            binding(
                1,
                ImageDirection::Output,
                Some(ProtectionLevel::GuardianProtected),
            )?,
        ];
        Ok(Self {
            region,
            sources,
            groups,
            values,
        })
    }

    fn mapping(&self) -> TestResult<ImageMapping<'_>> {
        Ok(ImageMapping::new(
            self.region.layout(),
            self.region.lease_identity().configuration().layout_digest(),
            self.region.capability_digest(),
            &self.sources,
            &self.groups,
            &self.values,
        )?)
    }

    fn plan(
        &self,
        instance: u16,
        package: BackendPackage,
        interface_byte: u8,
    ) -> TestResult<DriverInstancePlan> {
        self.plan_with_limits(instance, package, interface_byte, limits())
    }

    fn plan_with_limits(
        &self,
        instance: u16,
        package: BackendPackage,
        interface_byte: u8,
        limits: DriverLimits,
    ) -> TestResult<DriverInstancePlan> {
        let inventory = selected_inventory(package)?;
        let device = DeviceIdentity::new([interface_byte.wrapping_add(1); 32])?;
        let groups = [
            DriverGroupBinding::new(self.groups[0], 4, 2)?,
            DriverGroupBinding::new(self.groups[1], 4, 2)?,
        ];
        Ok(DriverInstancePlan::new(
            self.region,
            self.mapping()?,
            DriverInstanceHandle::new(instance),
            &inventory,
            InterfaceIdentity::new([interface_byte; 32])?,
            SourceHandle::new(0),
            DeviceCatalogDigest::new(self.sources[0].device_identity())?,
            &[device],
            &groups,
            limits,
        )?)
    }
}

fn binding(
    handle: u32,
    direction: ImageDirection,
    protection: Option<ProtectionLevel>,
) -> TestResult<ValueBinding> {
    let mut tag_bytes = [
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x8f,
    ];
    tag_bytes[15] = u8::try_from(handle.checked_add(1).ok_or("handle overflow")?)?;
    Ok(ValueBinding::new(
        LocalHandle::new(handle)?,
        TagId::from_bytes(tag_bytes)?,
        direction,
        ScalarType::U32,
        0,
        0,
        ByteOrder::LittleEndian,
        BitOrder::Lsb0,
        SourceHandle::new(0),
        GroupHandle::new(0),
        protection,
    ))
}

fn limits() -> DriverLimits {
    DriverLimits {
        maximum_devices: 2,
        maximum_groups: 2,
        maximum_frame_bytes: 64,
        maximum_cyclic_work_items: 4,
        maximum_mailbox_work_items: 2,
        maximum_mailbox_attempts: 3,
        diagnostic_capacity: 64,
        maximum_memory_bytes: 1_048_576,
    }
}

fn package(
    handle: u16,
    mode: DriverExecutionMode,
    implementation: DriverImplementationKind,
) -> TestResult<BackendPackage> {
    Ok(BackendPackage::new(
        BackendPackageHandle::new(handle),
        BackendIdentity::new(
            BackendDigest::new([u8::try_from(handle + 0x41)?; 32])?,
            SourceDigest::new([u8::try_from(handle + 11)?; 32])?,
            CapabilityDigest::from_sha256([0x33; 32]),
        ),
        ProtocolSourceKind::Ethercat,
        DriverContractVersion::PREVIEW_1_0,
        implementation,
        mode,
    )?)
}

fn selected_inventory(package: BackendPackage) -> TestResult<BackendInventory> {
    let handles = [package.handle()];
    Ok(BackendInventory::new(
        &[package],
        &handles,
        &handles,
        package.handle(),
        1,
    )?)
}

fn request(
    authority: aurora_driver_sdk::DriverAuthority,
    now_ns: u64,
) -> TestResult<aurora_driver_sdk::LifecycleRequest> {
    Ok(aurora_driver_sdk::LifecycleRequest::new(
        authority,
        now_ns,
        now_ns.checked_add(100).ok_or("deadline overflow")?,
    )?)
}

#[test]
fn inventory_is_exact_and_risk_forces_isolation() -> TestResult {
    let static_package = package(
        0,
        DriverExecutionMode::StaticLinked,
        DriverImplementationKind::FirstPartySafeRustBounded,
    )?;
    let isolated_package = package(
        1,
        DriverExecutionMode::IsolatedProcess,
        DriverImplementationKind::KernelCoupled,
    )?;
    assert!(matches!(
        package(
            2,
            DriverExecutionMode::StaticLinked,
            DriverImplementationKind::VendorSdk,
        )
        .map(|_| ()),
        Err(error)
            if error.downcast_ref::<DriverSdkError>()
                == Some(&DriverSdkError::IsolationRequired)
    ));
    let packages = [static_package, isolated_package];
    let allowlist = [BackendPackageHandle::new(0), BackendPackageHandle::new(1)];
    let inventory = BackendInventory::new(
        &packages,
        &allowlist,
        &allowlist,
        BackendPackageHandle::new(0),
        2,
    )?;
    assert_eq!(inventory.package_count(), 2);
    assert_eq!(inventory.selected(), static_package);
    assert!(inventory.is_approved_but_inactive(BackendPackageHandle::new(1)));
    let unapproved_package = package(
        2,
        DriverExecutionMode::IsolatedProcess,
        DriverImplementationKind::VendorSdk,
    )?;
    let inventory_with_unapproved = BackendInventory::new(
        &[static_package, isolated_package, unapproved_package],
        &[
            BackendPackageHandle::new(0),
            BackendPackageHandle::new(1),
            BackendPackageHandle::new(2),
        ],
        &allowlist,
        BackendPackageHandle::new(0),
        3,
    )?;
    assert!(!inventory_with_unapproved.is_approved_but_inactive(BackendPackageHandle::new(2)));
    assert!(matches!(
        BackendInventory::new(
            &packages,
            &allowlist,
            &[BackendPackageHandle::new(0)],
            BackendPackageHandle::new(1),
            2,
        ),
        Err(DriverSdkError::PackageNotApproved)
    ));
    Ok(())
}

#[test]
fn plan_closes_mapping_groups_devices_and_budgets_exactly() -> TestResult {
    let fixture = Fixture::new(1, 0x44)?;
    let selected_package = package(
        0,
        DriverExecutionMode::StaticLinked,
        DriverImplementationKind::FirstPartySafeRustBounded,
    )?;
    let plan = fixture.plan(0, selected_package, 0x61)?;
    assert_eq!(plan.groups().len(), 2);
    assert_eq!(plan.devices().len(), 1);
    let missing = [DriverGroupBinding::new(fixture.groups[0], 4, 2)?];
    let inventory = selected_inventory(selected_package)?;
    assert!(matches!(
        DriverInstancePlan::new(
            fixture.region,
            fixture.mapping()?,
            DriverInstanceHandle::new(0),
            &inventory,
            InterfaceIdentity::new([0x61; 32])?,
            SourceHandle::new(0),
            DeviceCatalogDigest::new(fixture.sources[0].device_identity())?,
            &[DeviceIdentity::new([0x62; 32])?],
            &missing,
            limits(),
        ),
        Err(DriverSdkError::CatalogMismatch)
    ));
    let wrong_backend = package(
        1,
        DriverExecutionMode::StaticLinked,
        DriverImplementationKind::FirstPartySafeRustBounded,
    )?;
    assert!(matches!(
        fixture.plan(0, wrong_backend, 0x61).map(|_| ()),
        Err(error)
            if error.downcast_ref::<DriverSdkError>()
                == Some(&DriverSdkError::CatalogMismatch)
    ));
    let wrong_capability = BackendPackage::new(
        BackendPackageHandle::new(0),
        BackendIdentity::new(
            BackendDigest::new(fixture.sources[0].driver_identity())?,
            SourceDigest::new([0x11; 32])?,
            CapabilityDigest::from_sha256([0x34; 32]),
        ),
        ProtocolSourceKind::Ethercat,
        DriverContractVersion::PREVIEW_1_0,
        DriverImplementationKind::FirstPartySafeRustBounded,
        DriverExecutionMode::StaticLinked,
    )?;
    assert!(matches!(
        fixture.plan(0, wrong_capability, 0x61).map(|_| ()),
        Err(error)
            if error.downcast_ref::<DriverSdkError>()
                == Some(&DriverSdkError::CatalogMismatch)
    ));
    let mut insufficient_memory = limits();
    insufficient_memory.maximum_memory_bytes = 1;
    assert!(matches!(
        fixture
            .plan_with_limits(0, selected_package, 0x61, insufficient_memory)
            .map(|_| ()),
        Err(error)
            if error.downcast_ref::<DriverSdkError>()
                == Some(&DriverSdkError::InvalidCapacity)
    ));
    Ok(())
}

fn run_golden_trace() -> TestResult<Vec<[u8; 48]>> {
    let fixture = Fixture::new(1, 0x44)?;
    let package = package(
        0,
        DriverExecutionMode::StaticLinked,
        DriverImplementationKind::FirstPartySafeRustBounded,
    )?;
    let plan = fixture.plan(0, package, 0x61)?;
    let authority = plan.authority();
    let mut simulator = DeterministicSimulator::new(plan, 0x1234, &[], 32)?;
    simulator.validate_configuration(request(authority, 0)?)?;
    simulator.claim(request(authority, 1)?)?;
    let initial_fallback = FallbackDigest::new([0x71; 32])?;
    simulator.initialize(request(authority, 2)?, initial_fallback)?;
    simulator.activate(request(authority, 3)?, initial_fallback)?;

    let output = [1_u8, 2, 3, 4];
    let mut no_input = [];
    simulator.exchange(
        ExchangeRequest::new(authority, fixture.groups[1], 1, 4, 104, &output)?,
        &mut ExchangeBuffer::new(&mut no_input),
    )?;
    let mut input = [0_u8; 4];
    simulator.exchange(
        ExchangeRequest::new(authority, fixture.groups[0], 1, 5, 105, &[])?,
        &mut ExchangeBuffer::new(&mut input),
    )?;
    assert_ne!(input, [0; 4]);
    simulator.enter_fallback(request(authority, 6)?, FallbackCause::ControlProcessLost)?;

    let replacement_fixture = Fixture::new(2, 0x45)?;
    let replacement = replacement_fixture.plan(0, package, 0x61)?;
    let replacement_authority = replacement.authority();
    simulator.recover(
        request(authority, 7)?,
        replacement,
        FallbackDigest::new([0x73; 32])?,
        EvidenceDigest::new([0x72; 32])?,
    )?;
    simulator.activate(
        request(replacement_authority, 8)?,
        FallbackDigest::new([0x73; 32])?,
    )?;
    simulator.quiesce_for_switch(request(replacement_authority, 9)?)?;
    simulator.release(request(replacement_authority, 10)?)?;
    assert_eq!(simulator.state(), DriverLifecycleState::Released);
    Ok(simulator
        .trace()
        .iter()
        .copied()
        .map(aurora_driver_sdk::GoldenTraceRecord::encode)
        .collect())
}

#[test]
fn simulator_is_fixed_seed_loopback_and_golden_trace_replayable() -> TestResult {
    let first = run_golden_trace()?;
    let second = run_golden_trace()?;
    assert_eq!(first, second);
    assert_eq!(first.len(), 11);
    assert_eq!(
        first[0],
        [
            1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]
    );
    assert_eq!(
        first[4],
        [
            5, 0, 0, 0, 0, 0, 0, 0, 5, 4, 4, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0,
            0, 0, 0, 93, 120, 101, 81, 119, 94, 122, 190, 0, 0, 0, 0, 0, 0, 0, 0,
        ]
    );
    assert_eq!(
        first[5],
        [
            6, 0, 0, 0, 0, 0, 0, 0, 5, 4, 4, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0,
            0, 0, 0, 46, 58, 246, 108, 232, 171, 27, 148, 0, 0, 0, 0, 0, 0, 0, 0,
        ]
    );
    assert_eq!(
        first[10],
        [
            11, 0, 0, 0, 0, 0, 0, 0, 10, 6, 7, 0, 0, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]
    );
    Ok(())
}

#[test]
fn every_required_fault_injection_is_normalized_and_isolated() -> TestResult {
    let cases = [
        (SimulationFault::Disconnect, DriverFaultKind::Disconnected),
        (SimulationFault::Reorder, DriverFaultKind::Reordered),
        (SimulationFault::Crc, DriverFaultKind::CrcFailure),
        (
            SimulationFault::WorkingCounter,
            DriverFaultKind::WorkingCounterFailure,
        ),
        (SimulationFault::BusOff, DriverFaultKind::BusOff),
        (SimulationFault::QueueFull, DriverFaultKind::QueueFull),
        (
            SimulationFault::MalformedFrame,
            DriverFaultKind::MalformedFrame,
        ),
        (
            SimulationFault::ProtocolVersion,
            DriverFaultKind::ProtocolVersionMismatch,
        ),
        (SimulationFault::Crash, DriverFaultKind::Crashed),
        (SimulationFault::Blocking, DriverFaultKind::BlockingDetected),
        (SimulationFault::Timeout, DriverFaultKind::DeadlineExceeded),
    ];
    for (fault, expected) in cases {
        let fixture = Fixture::new(1, 0x44)?;
        let package = package(
            0,
            DriverExecutionMode::StaticLinked,
            DriverImplementationKind::FirstPartySafeRustBounded,
        )?;
        let plan = fixture.plan(0, package, 0x61)?;
        let authority = plan.authority();
        let injection = FaultInjection::new(5, AdapterOperation::Exchange, fault)?;
        let mut simulator = DeterministicSimulator::new(plan, 7, &[injection], 8)?;
        simulator.validate_configuration(request(authority, 0)?)?;
        simulator.claim(request(authority, 1)?)?;
        let fallback = FallbackDigest::new([0x71; 32])?;
        simulator.initialize(request(authority, 2)?, fallback)?;
        simulator.activate(request(authority, 3)?, fallback)?;
        let mut input = [0_u8; 4];
        assert_eq!(
            simulator.exchange(
                ExchangeRequest::new(authority, fixture.groups[0], 1, 4, 104, &[])?,
                &mut ExchangeBuffer::new(&mut input),
            ),
            Err(DriverSdkError::DriverFault(expected))
        );
        assert!(simulator.fault_plan_complete());
        assert_eq!(
            simulator.trace().last().map(|record| record.result),
            Some(TraceResult::Fault(expected))
        );
        assert_ne!(expected.gap_reason(), aurora_io_guardian::GapReason::None);
    }
    let fixture = Fixture::new(1, 0x44)?;
    let package = package(
        0,
        DriverExecutionMode::StaticLinked,
        DriverImplementationKind::FirstPartySafeRustBounded,
    )?;
    let plan = fixture.plan(0, package, 0x61)?;
    let authority = plan.authority();
    let injection = FaultInjection::new(
        1,
        AdapterOperation::ValidateConfiguration,
        SimulationFault::Disconnect,
    )?;
    let mut unarmed = DeterministicSimulator::new(plan, 7, &[injection], 2)?;
    assert_eq!(
        unarmed.validate_configuration(request(authority, 0)?),
        Err(DriverSdkError::DriverFault(DriverFaultKind::Disconnected))
    );
    assert_eq!(unarmed.state(), DriverLifecycleState::Faulted);
    Ok(())
}

#[test]
fn simulator_rejects_fallback_drift_and_records_malformed_exchange() -> TestResult {
    let fixture = Fixture::new(1, 0x44)?;
    let package = package(
        0,
        DriverExecutionMode::StaticLinked,
        DriverImplementationKind::FirstPartySafeRustBounded,
    )?;
    let plan = fixture.plan(0, package, 0x61)?;
    let authority = plan.authority();
    let mut simulator = DeterministicSimulator::new(plan, 7, &[], 8)?;
    simulator.validate_configuration(request(authority, 0)?)?;
    simulator.claim(request(authority, 1)?)?;
    let active_fallback = FallbackDigest::new([0x71; 32])?;
    simulator.initialize(request(authority, 2)?, active_fallback)?;
    assert_eq!(
        simulator.activate(request(authority, 3)?, FallbackDigest::new([0x72; 32])?),
        Err(DriverSdkError::AuthorityMismatch)
    );
    assert_eq!(simulator.state(), DriverLifecycleState::Initialized);
    simulator.activate(request(authority, 4)?, active_fallback)?;

    let invalid_output = [0x55_u8; 1];
    let mut input = [0_u8; 4];
    assert_eq!(
        simulator.exchange(
            ExchangeRequest::new(authority, fixture.groups[0], 1, 5, 105, &invalid_output,)?,
            &mut ExchangeBuffer::new(&mut input),
        ),
        Err(DriverSdkError::DriverFault(DriverFaultKind::MalformedFrame))
    );
    assert_eq!(simulator.state(), DriverLifecycleState::Fallback);
    assert_eq!(
        simulator.trace().last().map(|record| record.result),
        Some(TraceResult::Fault(DriverFaultKind::MalformedFrame))
    );
    Ok(())
}

#[test]
fn mailbox_step_has_explicit_cancel_retry_and_work_bounds() -> TestResult {
    let fixture = Fixture::new(1, 0x44)?;
    let package = package(
        0,
        DriverExecutionMode::StaticLinked,
        DriverImplementationKind::FirstPartySafeRustBounded,
    )?;
    let plan = fixture.plan(0, package, 0x61)?;
    let authority = plan.authority();
    let mut simulator = DeterministicSimulator::new(plan, 7, &[], 8)?;
    simulator.validate_configuration(request(authority, 0)?)?;
    simulator.claim(request(authority, 1)?)?;
    simulator.initialize(request(authority, 2)?, FallbackDigest::new([0x71; 32])?)?;

    let active = MailboxCancellation::new(1, false)?;
    let report = simulator.mailbox_step(MailboxRequest::new(
        request(authority, 3)?,
        2,
        1,
        3,
        active,
    )?)?;
    assert_eq!(report.work_items, 2);

    let cancelled = MailboxCancellation::new(1, true)?;
    assert_eq!(
        simulator.mailbox_step(MailboxRequest::new(
            request(authority, 4)?,
            1,
            2,
            3,
            cancelled,
        )?),
        Err(DriverSdkError::OperationCancelled)
    );
    assert_eq!(simulator.state(), DriverLifecycleState::Initialized);
    assert_eq!(
        simulator.mailbox_step(MailboxRequest::new(
            request(authority, 4)?,
            1,
            1,
            4,
            active,
        )?),
        Err(DriverSdkError::InvalidCapacity)
    );
    assert!(matches!(
        MailboxRequest::new(request(authority, 4)?, 1, 4, 3, active),
        Err(DriverSdkError::InvalidCapacity)
    ));
    Ok(())
}

#[test]
fn simulator_trace_and_recovery_never_exceed_preallocated_capacity() -> TestResult {
    let fixture = Fixture::new(1, 0x44)?;
    let package = package(
        0,
        DriverExecutionMode::StaticLinked,
        DriverImplementationKind::FirstPartySafeRustBounded,
    )?;
    let plan = fixture.plan(0, package, 0x61)?;
    let authority = plan.authority();
    let mut simulator = DeterministicSimulator::new(plan, 7, &[], 4)?;
    simulator.validate_configuration(request(authority, 0)?)?;
    simulator.claim(request(authority, 1)?)?;
    let fallback = FallbackDigest::new([0x71; 32])?;
    simulator.initialize(request(authority, 2)?, fallback)?;
    simulator.activate(request(authority, 3)?, fallback)?;
    let mut input = [0_u8; 4];
    assert_eq!(
        simulator.exchange(
            ExchangeRequest::new(authority, fixture.groups[0], 1, 4, 104, &[])?,
            &mut ExchangeBuffer::new(&mut input),
        ),
        Err(DriverSdkError::DriverFault(DriverFaultKind::QueueFull))
    );
    assert_eq!(simulator.state(), DriverLifecycleState::Fallback);
    assert_eq!(simulator.trace().len(), 4);

    let replacement_fixture = Fixture::new(2, 0x45)?;
    let mut expanded_limits = limits();
    expanded_limits.maximum_frame_bytes = 128;
    let replacement = replacement_fixture.plan_with_limits(0, package, 0x61, expanded_limits)?;
    assert_eq!(
        simulator.recover(
            request(authority, 5)?,
            replacement,
            FallbackDigest::new([0x72; 32])?,
            EvidenceDigest::new([0x73; 32])?,
        ),
        Err(DriverSdkError::AuthorityMismatch)
    );
    assert_eq!(simulator.state(), DriverLifecycleState::Fallback);
    Ok(())
}

fn sandbox(
    device: DeviceIdentity,
    profile_byte: u8,
) -> TestResult<(SandboxPolicy, AppliedSandbox)> {
    let service = [profile_byte; 32];
    let peer = PeerCredentials::new(42, 1000, 1001, service)?;
    let policy = SandboxPolicy::new(
        SandboxDigest::new([profile_byte; 32])?,
        PeerPolicy::new(1000, 1001, service),
        peer,
        true,
        true,
        NamespaceIsolation::COMPLETE,
        SandboxDigest::new([profile_byte.wrapping_add(1); 32])?,
        EvidenceDigest::new([profile_byte.wrapping_add(2); 32])?,
        &[DeviceAccessGrant {
            identity: device,
            read: true,
            write: true,
        }],
        &[LinuxCapability::NetRaw, LinuxCapability::SysNice],
        1_048_576,
        32,
        4_096,
    )?;
    let applied = AppliedSandbox {
        profile: SandboxDigest::new([profile_byte; 32])?,
        seccomp: SandboxDigest::new([profile_byte.wrapping_add(1); 32])?,
        controls: EvidenceDigest::new([profile_byte.wrapping_add(2); 32])?,
        peer,
        no_new_privileges: true,
        read_only_root: true,
        namespaces: NamespaceIsolation::COMPLETE,
        maximum_memory_bytes: 1_048_576,
        maximum_open_files: 32,
        maximum_control_message_bytes: 4_096,
    };
    Ok((policy, applied))
}

#[test]
fn isolated_hosts_enforce_grants_and_faults_do_not_spread() -> TestResult {
    let fixture = Fixture::new(1, 0x44)?;
    let isolated = package(
        0,
        DriverExecutionMode::IsolatedProcess,
        DriverImplementationKind::VendorSdk,
    )?;
    let first_plan = fixture.plan(0, isolated, 0x61)?;
    let second_plan = fixture.plan(1, isolated, 0x71)?;
    let first_device = first_plan.devices()[0];
    let second_device = second_plan.devices()[0];
    let (first_policy, first_applied) = sandbox(first_device, 0x31)?;
    let (second_policy, second_applied) = sandbox(second_device, 0x41)?;
    let first_slots = SharedSlotGrant::from_plan(&first_plan)?;
    let second_slots = SharedSlotGrant::from_plan(&second_plan)?;
    let first_authority = first_plan.authority();
    let second_authority = second_plan.authority();
    let generation = DriverHostGeneration::new(1)?;
    let mut first = DriverHostBoundary::admit(
        &first_plan,
        generation,
        &first_policy,
        first_applied,
        first_slots,
    )?;
    first.observe(DriverHostEvent::Started, 0)?;
    assert_eq!(
        first.authorize_group(generation, second_authority, fixture.groups[0]),
        Err(DriverSdkError::UnauthorizedResource)
    );
    assert_eq!(
        first.authorize_group(generation, first_authority, fixture.groups[0]),
        Ok(())
    );
    let mut second = DriverHostBoundary::admit(
        &second_plan,
        generation,
        &second_policy,
        second_applied,
        second_slots,
    )?;
    second.observe(DriverHostEvent::Started, 0)?;
    let mut registry = DriverHostRegistry::new(vec![first, second], 2)?;
    assert_eq!(
        registry.observe(
            DriverInstanceHandle::new(0),
            DriverHostEvent::BlockingDetected,
            1,
        )?,
        DriverHostState::Faulted(DriverFaultKind::BlockingDetected)
    );
    assert_eq!(
        registry.state(DriverInstanceHandle::new(1)),
        Some(DriverHostState::Running)
    );
    Ok(())
}

#[test]
fn host_rejects_static_package_and_incomplete_sandbox_evidence() -> TestResult {
    let fixture = Fixture::new(1, 0x44)?;
    let static_package = package(
        0,
        DriverExecutionMode::StaticLinked,
        DriverImplementationKind::FirstPartySafeRustBounded,
    )?;
    let plan = fixture.plan(0, static_package, 0x61)?;
    let (policy, mut applied) = sandbox(plan.devices()[0], 0x31)?;
    let slots = SharedSlotGrant::from_plan(&plan)?;
    assert!(matches!(
        DriverHostBoundary::admit(
            &plan,
            DriverHostGeneration::new(1)?,
            &policy,
            applied,
            slots,
        ),
        Err(DriverSdkError::SandboxUnavailable)
    ));

    let isolated = package(
        0,
        DriverExecutionMode::IsolatedProcess,
        DriverImplementationKind::VendorSdk,
    )?;
    let isolated_plan = fixture.plan(0, isolated, 0x61)?;
    let (isolated_policy, valid_applied) = sandbox(isolated_plan.devices()[0], 0x31)?;
    let mut wrong_controls = valid_applied;
    wrong_controls.controls = EvidenceDigest::new([0x77; 32])?;
    assert!(matches!(
        DriverHostBoundary::admit(
            &isolated_plan,
            DriverHostGeneration::new(1)?,
            &isolated_policy,
            wrong_controls,
            SharedSlotGrant::from_plan(&isolated_plan)?,
        ),
        Err(DriverSdkError::SandboxUnavailable)
    ));
    applied = valid_applied;
    applied.no_new_privileges = false;
    let isolated_slots = SharedSlotGrant::from_plan(&isolated_plan)?;
    assert!(matches!(
        DriverHostBoundary::admit(
            &isolated_plan,
            DriverHostGeneration::new(1)?,
            &isolated_policy,
            applied,
            isolated_slots,
        ),
        Err(DriverSdkError::SandboxUnavailable)
    ));
    Ok(())
}

#[test]
fn host_restart_requires_next_generation_and_full_authority() -> TestResult {
    let fixture = Fixture::new(1, 0x44)?;
    let isolated = package(
        0,
        DriverExecutionMode::IsolatedProcess,
        DriverImplementationKind::VendorSdk,
    )?;
    let plan = fixture.plan(0, isolated, 0x61)?;
    let authority = plan.authority();
    let device = plan.devices()[0];
    let (policy, applied) = sandbox(device, 0x31)?;
    let first = DriverHostBoundary::admit(
        &plan,
        DriverHostGeneration::new(1)?,
        &policy,
        applied,
        SharedSlotGrant::from_plan(&plan)?,
    )?;
    let mut registry = DriverHostRegistry::new(vec![first], 1)?;
    registry.observe(DriverInstanceHandle::new(0), DriverHostEvent::Started, 0)?;
    registry.observe(DriverInstanceHandle::new(0), DriverHostEvent::Stopped, 1)?;

    let (_, replacement_applied) = sandbox(device, 0x31)?;
    let replacement = DriverHostBoundary::admit(
        &plan,
        DriverHostGeneration::new(2)?,
        &policy,
        replacement_applied,
        SharedSlotGrant::from_plan(&plan)?,
    )?;
    registry.replace_stopped(replacement)?;
    registry.observe(DriverInstanceHandle::new(0), DriverHostEvent::Started, 2)?;
    assert_eq!(
        registry.authorize_group(
            DriverInstanceHandle::new(0),
            DriverHostGeneration::new(1)?,
            authority,
            fixture.groups[0],
        ),
        Err(DriverSdkError::UnauthorizedResource)
    );
    assert_eq!(
        registry.authorize_group(
            DriverInstanceHandle::new(0),
            DriverHostGeneration::new(2)?,
            authority,
            fixture.groups[0],
        ),
        Ok(())
    );

    let drifted_fixture = Fixture::new(2, 0x45)?;
    let drifted_authority = drifted_fixture.plan(0, isolated, 0x61)?.authority();
    assert_eq!(
        registry.authorize_group(
            DriverInstanceHandle::new(0),
            DriverHostGeneration::new(2)?,
            drifted_authority,
            fixture.groups[0],
        ),
        Err(DriverSdkError::UnauthorizedResource)
    );
    Ok(())
}

#[test]
fn every_host_fault_is_local_to_its_instance() -> TestResult {
    let cases = [
        (DriverHostEvent::Crashed, DriverFaultKind::Crashed),
        (DriverHostEvent::TimedOut, DriverFaultKind::DeadlineExceeded),
        (
            DriverHostEvent::BlockingDetected,
            DriverFaultKind::BlockingDetected,
        ),
        (DriverHostEvent::QueueFull, DriverFaultKind::QueueFull),
        (
            DriverHostEvent::MalformedMessage,
            DriverFaultKind::MalformedFrame,
        ),
        (
            DriverHostEvent::ProtocolVersionError,
            DriverFaultKind::ProtocolVersionMismatch,
        ),
    ];
    for (event, fault) in cases {
        let fixture = Fixture::new(1, 0x44)?;
        let isolated = package(
            0,
            DriverExecutionMode::IsolatedProcess,
            DriverImplementationKind::VendorSdk,
        )?;
        let first_plan = fixture.plan(0, isolated, 0x61)?;
        let second_plan = fixture.plan(1, isolated, 0x71)?;
        let (first_policy, first_applied) = sandbox(first_plan.devices()[0], 0x31)?;
        let (second_policy, second_applied) = sandbox(second_plan.devices()[0], 0x41)?;
        let first = DriverHostBoundary::admit(
            &first_plan,
            DriverHostGeneration::new(1)?,
            &first_policy,
            first_applied,
            SharedSlotGrant::from_plan(&first_plan)?,
        )?;
        let second = DriverHostBoundary::admit(
            &second_plan,
            DriverHostGeneration::new(1)?,
            &second_policy,
            second_applied,
            SharedSlotGrant::from_plan(&second_plan)?,
        )?;
        let mut registry = DriverHostRegistry::new(vec![first, second], 2)?;
        registry.observe(DriverInstanceHandle::new(0), DriverHostEvent::Started, 0)?;
        registry.observe(DriverInstanceHandle::new(1), DriverHostEvent::Started, 0)?;
        assert_eq!(
            registry.observe(DriverInstanceHandle::new(0), event, 1)?,
            DriverHostState::Faulted(fault)
        );
        assert_eq!(
            registry.state(DriverInstanceHandle::new(1)),
            Some(DriverHostState::Running)
        );
    }
    Ok(())
}
