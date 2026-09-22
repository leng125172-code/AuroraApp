//! R3-04 Fallback plan, protection, fault isolation, recovery, and watchdog acceptance tests.

use aurora_io_guardian::{
    BitOrder, ByteOrder, CapabilityDigest, DeviceWatchdogSimulator, DeviceWatchdogState,
    DomainRecoveryPolicy, EvidenceDigest, FallbackAction, FallbackCause, FallbackController,
    FallbackDependency, FallbackDigest, FallbackDomainDiagnostics, FallbackDomainHandle,
    FallbackDomainRisk, FallbackDomainSpec, FallbackDomainState, FallbackEffect, FallbackError,
    FallbackHealthPolicy, FallbackLimits, FallbackPlan, FallbackValue, FallbackValueRange,
    FallbackVersion, GroupDescriptor, GroupHandle, HealthCheckResult, ImageDirection, ImageError,
    ImageLayout, ImageMapping, OutputFallbackSpec, PendingFallbackHealth, ProtectionEvidence,
    ProtectionLevel, ProtocolSourceKind, RecoveryAuthorization, RecoveryProgress, RegionHeader,
    ReinitializationEvidence, ScalarType, SourceDescriptor, SourceHandle, ValueBinding,
};
use aurora_io_guardian_contracts::{
    ConfigurationDigest, ConfigurationGeneration, GuardianConfiguration, GuardianEpoch,
    LayoutDigest, LeaseId, LeaseIdentity, LeaseSequence,
};
use aurora_types::{LocalHandle, TagId};

struct Fixture {
    region: RegionHeader,
    sources: [SourceDescriptor; 2],
    groups: [GroupDescriptor; 2],
    values: [ValueBinding; 3],
    domains: [FallbackDomainSpec; 2],
    outputs: [OutputFallbackSpec; 3],
    dependencies: [FallbackDependency; 1],
}

impl Fixture {
    fn new(generation: u64, lease_byte: u8, first_fixed: u32) -> Option<Self> {
        let layout = ImageLayout::new(0, 12, 0, 3, 0, 2, 8_192).ok()?;
        let configuration = GuardianConfiguration::new(
            GuardianEpoch::new(5).ok()?,
            ConfigurationGeneration::new(generation).ok()?,
            ConfigurationDigest::from_sha256([u8::try_from(generation).ok()?; 32]),
            LayoutDigest::from_sha256([0x22; 32]),
        );
        let region = RegionHeader::new(
            layout,
            LeaseIdentity::new(configuration, LeaseId::new([lease_byte; 16]).ok()?),
            LeaseSequence::new(generation).ok()?,
            CapabilityDigest::from_sha256([0x33; 32]),
        );
        let sources = [
            SourceDescriptor::new(
                SourceHandle::new(0),
                ProtocolSourceKind::Ethercat,
                [0x41; 32],
                [0x51; 32],
            ),
            SourceDescriptor::new(
                SourceHandle::new(1),
                ProtocolSourceKind::Can,
                [0x42; 32],
                [0x52; 32],
            ),
        ];
        let groups = [
            GroupDescriptor::new(
                ImageDirection::Output,
                GroupHandle::new(0),
                SourceHandle::new(0),
            ),
            GroupDescriptor::new(
                ImageDirection::Output,
                GroupHandle::new(1),
                SourceHandle::new(1),
            ),
        ];
        let values = [
            output_binding(
                0,
                0,
                SourceHandle::new(0),
                GroupHandle::new(0),
                ProtectionLevel::GuardianProtected,
            )?,
            output_binding(
                1,
                4,
                SourceHandle::new(1),
                GroupHandle::new(1),
                ProtectionLevel::DeviceWatchdogProtected,
            )?,
            output_binding(
                2,
                8,
                SourceHandle::new(0),
                GroupHandle::new(0),
                ProtectionLevel::ExternalSafetyProtected,
            )?,
        ];
        let domains = fallback_domains()?;
        let outputs = fallback_outputs(values, domains, first_fixed)?;
        let dependencies = [FallbackDependency::new(values[1].handle(), values[2].handle()).ok()?];
        Some(Self {
            region,
            sources,
            groups,
            values,
            domains,
            outputs,
            dependencies,
        })
    }

    fn mapping(&self) -> Result<ImageMapping<'_>, ImageError> {
        ImageMapping::new(
            self.region.layout(),
            self.region.lease_identity().configuration().layout_digest(),
            self.region.capability_digest(),
            &self.sources,
            &self.groups,
            &self.values,
        )
    }

    fn plan(&self, version: u64, digest_byte: u8) -> Result<FallbackPlan, FallbackError> {
        self.plan_with(version, digest_byte, &self.outputs, &self.dependencies)
    }

    fn plan_with(
        &self,
        version: u64,
        digest_byte: u8,
        outputs: &[OutputFallbackSpec],
        dependencies: &[FallbackDependency],
    ) -> Result<FallbackPlan, FallbackError> {
        self.plan_with_domains(version, digest_byte, &self.domains, outputs, dependencies)
    }

    fn plan_with_domains(
        &self,
        version: u64,
        digest_byte: u8,
        domains: &[FallbackDomainSpec],
        outputs: &[OutputFallbackSpec],
        dependencies: &[FallbackDependency],
    ) -> Result<FallbackPlan, FallbackError> {
        FallbackPlan::new(
            self.region,
            self.mapping()
                .map_err(|_| FallbackError::MappingIdentityMismatch)?,
            FallbackVersion::new(version)?,
            digest(digest_byte)?,
            FallbackHealthPolicy::new(2, 10)?,
            domains,
            outputs,
            dependencies,
            FallbackLimits::new(2, 3, 2)?,
        )
    }
}

fn fallback_domains() -> Option<[FallbackDomainSpec; 2]> {
    let health = FallbackHealthPolicy::new(2, 10).ok()?;
    let recovery = DomainRecoveryPolicy::new(2, 100, 10, health).ok()?;
    Some([
        FallbackDomainSpec::new(
            FallbackDomainHandle::new(0),
            FallbackDomainRisk::Ordinary,
            recovery,
        ),
        FallbackDomainSpec::new(
            FallbackDomainHandle::new(1),
            FallbackDomainRisk::Hazardous,
            recovery,
        ),
    ])
}

fn fallback_outputs(
    values: [ValueBinding; 3],
    domains: [FallbackDomainSpec; 2],
    first_fixed: u32,
) -> Option<[OutputFallbackSpec; 3]> {
    let range = FallbackValueRange::new(FallbackValue::U32(0), FallbackValue::U32(100)).ok()?;
    let preset = digest(0x71).ok()?;
    Some([
        OutputFallbackSpec::new(
            values[0],
            domains[0].handle(),
            range,
            FallbackAction::SetFixed(FallbackValue::U32(first_fixed)),
            ProtectionEvidence::Guardian,
        )
        .ok()?,
        OutputFallbackSpec::new(
            values[1],
            domains[1].handle(),
            range,
            FallbackAction::DeviceWatchdogPreset { preset },
            ProtectionEvidence::DeviceWatchdog {
                evidence: evidence(0x72).ok()?,
                preset,
                timeout_ns: 20,
            },
        )
        .ok()?,
        OutputFallbackSpec::new(
            values[2],
            domains[1].handle(),
            range,
            FallbackAction::SetFixed(FallbackValue::U32(7)),
            ProtectionEvidence::ExternalSafety {
                evidence: evidence(0x73).ok()?,
            },
        )
        .ok()?,
    ])
}

fn output_binding(
    handle: u32,
    byte_offset: u32,
    source: SourceHandle,
    group: GroupHandle,
    protection: ProtectionLevel,
) -> Option<ValueBinding> {
    Some(ValueBinding::new(
        LocalHandle::new(handle).ok()?,
        tag(u8::try_from(handle.checked_add(1)?).ok()?)?,
        ImageDirection::Output,
        ScalarType::U32,
        byte_offset,
        0,
        ByteOrder::LittleEndian,
        BitOrder::Lsb0,
        source,
        group,
        Some(protection),
    ))
}

fn tag(discriminator: u8) -> Option<TagId> {
    let mut bytes = [
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x8f,
    ];
    bytes[15] = discriminator;
    TagId::from_bytes(bytes).ok()
}

fn digest(value: u8) -> Result<FallbackDigest, FallbackError> {
    FallbackDigest::new([value; 32])
}

fn evidence(value: u8) -> Result<EvidenceDigest, FallbackError> {
    EvidenceDigest::new([value; 32])
}

fn candidate_identity(generation: u64, lease_byte: u8) -> Option<LeaseIdentity> {
    Some(LeaseIdentity::new(
        GuardianConfiguration::new(
            GuardianEpoch::new(5).ok()?,
            ConfigurationGeneration::new(generation).ok()?,
            ConfigurationDigest::from_sha256([u8::try_from(generation).ok()?; 32]),
            LayoutDigest::from_sha256([0x22; 32]),
        ),
        LeaseId::new([lease_byte; 16]).ok()?,
    ))
}

#[test]
fn plan_requires_exact_output_and_domain_closure() {
    let fixture = Fixture::new(7, 0x44, 5);
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let plan = fixture.plan(1, 0x61);
        assert!(plan.is_ok());
        if let Ok(plan) = plan {
            assert_eq!(plan.domain_count(), 2);
            assert_eq!(plan.output_count(), 3);
            assert_eq!(plan.dependency_count(), 1);
        }
        assert!(matches!(
            fixture.plan_with(1, 0x61, &fixture.outputs[..2], &fixture.dependencies),
            Err(FallbackError::OutputClosureMismatch)
        ));
        let reversed = [fixture.domains[1], fixture.domains[0]];
        let plan =
            fixture.plan_with_domains(1, 0x61, &reversed, &fixture.outputs, &fixture.dependencies);
        assert!(matches!(plan, Err(FallbackError::DomainClosureMismatch)));
    }
}

#[test]
fn cross_protocol_dependency_cannot_cross_fallback_domains() {
    let fixture = Fixture::new(7, 0x44, 5);
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let cross_domain =
            FallbackDependency::new(fixture.values[0].handle(), fixture.values[1].handle());
        assert!(cross_domain.is_ok());
        if let Ok(cross_domain) = cross_domain {
            let dependencies = [cross_domain];
            assert!(matches!(
                fixture.plan_with(1, 0x61, &fixture.outputs, &dependencies),
                Err(FallbackError::DependencyClosureMismatch)
            ));
        }
    }
}

#[test]
fn hazardous_domain_rejects_hold_last_even_with_watchdog_evidence() {
    let fixture = Fixture::new(7, 0x44, 5);
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let range = FallbackValueRange::new(FallbackValue::U32(0), FallbackValue::U32(100));
        let watchdog_evidence = evidence(0x72);
        let watchdog_preset = digest(0x71);
        assert!(range.is_ok() && watchdog_evidence.is_ok() && watchdog_preset.is_ok());
        if let (Ok(range), Ok(watchdog_evidence), Ok(watchdog_preset)) =
            (range, watchdog_evidence, watchdog_preset)
        {
            let hazardous_hold = OutputFallbackSpec::new(
                fixture.values[1],
                FallbackDomainHandle::new(1),
                range,
                FallbackAction::HoldLastThenFixed {
                    hold_ns: 5,
                    fixed: FallbackValue::U32(0),
                },
                ProtectionEvidence::DeviceWatchdog {
                    evidence: watchdog_evidence,
                    preset: watchdog_preset,
                    timeout_ns: 20,
                },
            );
            assert!(hazardous_hold.is_ok());
            if let Ok(hazardous_hold) = hazardous_hold {
                let outputs = [fixture.outputs[0], hazardous_hold, fixture.outputs[2]];
                assert!(matches!(
                    fixture.plan_with(1, 0x61, &outputs, &fixture.dependencies),
                    Err(FallbackError::UnsafeHoldLast)
                ));
            }
        }
    }
}

#[test]
fn hazardous_domain_rejects_guardian_only_protection() {
    let fixture = Fixture::new(7, 0x44, 5);
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let health = FallbackHealthPolicy::new(2, 10);
        assert!(health.is_ok());
        if let Ok(health) = health {
            let recovery = DomainRecoveryPolicy::new(2, 100, 10, health);
            assert!(recovery.is_ok());
            if let Ok(recovery) = recovery {
                let domains = [
                    FallbackDomainSpec::new(
                        FallbackDomainHandle::new(0),
                        FallbackDomainRisk::Hazardous,
                        recovery,
                    ),
                    fixture.domains[1],
                ];
                assert!(matches!(
                    fixture.plan_with_domains(
                        1,
                        0x61,
                        &domains,
                        &fixture.outputs,
                        &fixture.dependencies,
                    ),
                    Err(FallbackError::ProtectionUnavailable)
                ));
            }
        }
    }
}

#[test]
fn recovery_policy_rejects_a_health_gate_that_cannot_fit() {
    let health = FallbackHealthPolicy::new(2, 100);
    assert!(health.is_ok());
    if let Ok(health) = health {
        assert_eq!(
            DomainRecoveryPolicy::new(2, 100, 10, health),
            Err(FallbackError::InvalidCapacity)
        );
    }
}

#[test]
fn finite_hold_last_switches_to_fixed_at_the_exact_deadline() {
    let fixture = Fixture::new(7, 0x44, 5);
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let range = FallbackValueRange::new(FallbackValue::U32(0), FallbackValue::U32(100));
        assert!(range.is_ok());
        if let Ok(range) = range {
            let held = OutputFallbackSpec::new(
                fixture.values[0],
                FallbackDomainHandle::new(0),
                range,
                FallbackAction::HoldLastThenFixed {
                    hold_ns: 5,
                    fixed: FallbackValue::U32(3),
                },
                ProtectionEvidence::Guardian,
            );
            assert!(held.is_ok());
            if let Ok(held) = held {
                let outputs = [held, fixture.outputs[1], fixture.outputs[2]];
                let controller = fixture
                    .plan_with(1, 0x61, &outputs, &fixture.dependencies)
                    .ok()
                    .and_then(|plan| FallbackController::new(plan).ok());
                assert!(controller.is_some());
                if let Some(mut controller) = controller {
                    assert_eq!(
                        controller.activate_running(fixture.region.lease_identity(), 0),
                        Ok(())
                    );
                    assert_eq!(
                        controller.trigger_global_failure(FallbackCause::LeaseRevoked, 100),
                        Ok(())
                    );
                    assert_eq!(
                        controller.output_effect(LocalHandle::ZERO, 104),
                        Ok(FallbackEffect::HoldLastUntil(105))
                    );
                    assert_eq!(
                        controller.output_effect(LocalHandle::ZERO, 103),
                        Err(FallbackError::MonotonicTimeRegression)
                    );
                    assert_eq!(
                        controller.output_effect(LocalHandle::ZERO, 105),
                        Ok(FallbackEffect::SetFixed(FallbackValue::U32(3)))
                    );
                }
            }
        }
    }
}

#[test]
fn pending_requires_new_digest_generation_version_and_lease() {
    let active = Fixture::new(7, 0x44, 5).and_then(|fixture| fixture.plan(1, 0x61).ok());
    let same_digest = Fixture::new(8, 0x45, 6).and_then(|fixture| fixture.plan(2, 0x61).ok());
    let reused_lease = Fixture::new(8, 0x44, 6).and_then(|fixture| fixture.plan(2, 0x62).ok());
    assert!(active.is_some() && same_digest.is_some() && reused_lease.is_some());
    if let (Some(active), Some(same_digest), Some(reused_lease)) =
        (active, same_digest, reused_lease)
    {
        let controller = FallbackController::new(active);
        assert!(controller.is_ok());
        if let Ok(mut controller) = controller {
            assert_eq!(
                controller.stage_pending(same_digest, 1),
                Err(FallbackError::InvalidPendingVersion)
            );
            assert_eq!(
                controller.stage_pending(reused_lease, 2),
                Err(FallbackError::InvalidPendingVersion)
            );
            assert_eq!(controller.pending_version(), None);
        }
    }
}

#[test]
fn pending_is_never_applied_before_the_complete_health_window() {
    let active = Fixture::new(7, 0x44, 5).and_then(|fixture| fixture.plan(1, 0x61).ok());
    let pending = Fixture::new(8, 0x45, 99).and_then(|fixture| fixture.plan(2, 0x62).ok());
    assert!(active.is_some() && pending.is_some());
    if let (Some(active), Some(pending)) = (active, pending) {
        let controller = FallbackController::new(active);
        assert!(controller.is_ok());
        if let Ok(mut controller) = controller {
            assert_eq!(
                controller.output_effect(LocalHandle::ZERO, 0),
                Ok(FallbackEffect::SetFixed(FallbackValue::U32(5)))
            );
            assert_eq!(controller.stage_pending(pending, 10), Ok(()));
            assert_eq!(
                controller.pending_version().map(FallbackVersion::get),
                Some(2)
            );
            assert_eq!(
                controller.output_effect(LocalHandle::ZERO, 10),
                Ok(FallbackEffect::SetFixed(FallbackValue::U32(5)))
            );
            assert_eq!(
                controller.observe_pending_health(10, PendingFallbackHealth::all_passed()),
                Ok(())
            );
            assert_eq!(
                controller.observe_pending_health(11, PendingFallbackHealth::all_passed()),
                Ok(())
            );
            assert_eq!(
                controller.commit_pending(20),
                Err(FallbackError::HealthWindowIncomplete)
            );
            assert_eq!(
                controller.observe_pending_health(20, PendingFallbackHealth::all_passed()),
                Ok(())
            );
            assert_eq!(controller.commit_pending(20), Ok(()));
            assert_eq!(controller.active_version().get(), 2);
            assert_eq!(controller.pending_version(), None);
            assert_eq!(
                controller.output_effect(LocalHandle::ZERO, 20),
                Ok(FallbackEffect::SetFixed(FallbackValue::U32(99)))
            );

            let rejected = Fixture::new(9, 0x46, 77).and_then(|fixture| fixture.plan(3, 0x63).ok());
            assert!(rejected.is_some());
            if let Some(rejected) = rejected {
                assert_eq!(controller.stage_pending(rejected, 21), Ok(()));
                assert_eq!(
                    controller.observe_pending_health(
                        22,
                        PendingFallbackHealth::new(
                            HealthCheckResult::Passed,
                            HealthCheckResult::Failed,
                            HealthCheckResult::Passed,
                            HealthCheckResult::Passed,
                        ),
                    ),
                    Err(FallbackError::PendingHealthFailed)
                );
                assert_eq!(controller.active_version().get(), 2);
                assert_eq!(controller.pending_version(), None);
            }
        }
    }
}

#[test]
fn pending_health_rejects_an_absent_staged_plan() {
    let active = Fixture::new(7, 0x44, 5).and_then(|fixture| fixture.plan(1, 0x61).ok());
    assert!(active.is_some());
    if let Some(active) = active {
        let controller = FallbackController::new(active);
        assert!(controller.is_ok());
        if let Ok(mut controller) = controller {
            assert_eq!(
                controller.observe_pending_health(1, PendingFallbackHealth::all_passed()),
                Err(FallbackError::InvalidStateTransition)
            );
            assert_eq!(controller.pending_version(), None);
        }
    }
}

#[test]
fn local_protocol_faults_are_isolated_and_global_loss_affects_every_domain() {
    let fixture = Fixture::new(7, 0x44, 5);
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let plan = fixture.plan(1, 0x61);
        assert!(plan.is_ok());
        if let Ok(plan) = plan {
            let controller = FallbackController::new(plan);
            assert!(controller.is_ok());
            if let Ok(mut controller) = controller {
                assert_eq!(
                    controller.activate_running(fixture.region.lease_identity(), 0),
                    Ok(())
                );
                assert_eq!(
                    controller.trigger_group_failure(
                        fixture.groups[1],
                        FallbackCause::CanBusOff,
                        10,
                    ),
                    Ok(())
                );
                assert_eq!(
                    controller
                        .diagnostics(FallbackDomainHandle::new(0))
                        .map(FallbackDomainDiagnostics::state),
                    Some(FallbackDomainState::Normal)
                );
                assert_eq!(
                    controller
                        .diagnostics(FallbackDomainHandle::new(1))
                        .map(FallbackDomainDiagnostics::state),
                    Some(FallbackDomainState::Fallback)
                );
                assert_eq!(
                    controller.trigger_group_failure(
                        fixture.groups[1],
                        FallbackCause::ModbusTimeout,
                        11,
                    ),
                    Err(FallbackError::FailureSourceMismatch)
                );
                assert_eq!(
                    controller.trigger_global_failure(FallbackCause::ControlHeartbeatExpired, 12,),
                    Ok(())
                );
                assert_eq!(
                    controller
                        .diagnostics(FallbackDomainHandle::new(0))
                        .map(FallbackDomainDiagnostics::state),
                    Some(FallbackDomainState::Fallback)
                );
            }
        }
    }
}

#[test]
fn guardian_loss_exposes_exact_second_layer_effects() {
    let fixture = Fixture::new(7, 0x44, 5);
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let controller = fixture
            .plan(1, 0x61)
            .ok()
            .and_then(|plan| FallbackController::new(plan).ok());
        assert!(controller.is_some());
        if let Some(mut controller) = controller {
            assert_eq!(
                controller.activate_running(fixture.region.lease_identity(), 0),
                Ok(())
            );
            assert_eq!(
                controller.trigger_global_failure(FallbackCause::GuardianProcessLost, 10),
                Ok(())
            );
            assert_eq!(
                controller.output_effect(LocalHandle::ZERO, 10),
                Ok(FallbackEffect::GuardianUnavailable)
            );
            assert_eq!(
                controller.output_effect(fixture.values[1].handle(), 10),
                digest(0x71).map(FallbackEffect::DeviceWatchdogPreset)
            );
            assert_eq!(
                controller.output_effect(fixture.values[2].handle(), 10),
                Ok(FallbackEffect::ExternalProtection)
            );
            assert_eq!(
                controller.trigger_global_failure(FallbackCause::LeaseRevoked, 11),
                Ok(())
            );
            assert_eq!(
                controller
                    .diagnostics(FallbackDomainHandle::new(0))
                    .map(FallbackDomainDiagnostics::state),
                Some(FallbackDomainState::GuardianUnavailable)
            );
            assert_eq!(
                controller.output_effect(LocalHandle::ZERO, 11),
                Ok(FallbackEffect::GuardianUnavailable)
            );
        }
    }
}

#[test]
fn watchdog_uses_an_exact_deadline_and_requires_reinitialization() {
    let preset = digest(0x71);
    assert!(preset.is_ok());
    if let Ok(preset) = preset {
        let watchdog = DeviceWatchdogSimulator::new(10, preset);
        assert!(watchdog.is_ok());
        if let Ok(mut watchdog) = watchdog {
            assert_eq!(watchdog.arm(100), Ok(()));
            assert_eq!(watchdog.observe(109), Ok(DeviceWatchdogState::Armed));
            assert_eq!(watchdog.kick(109), Ok(()));
            assert_eq!(watchdog.observe(118), Ok(DeviceWatchdogState::Armed));
            assert_eq!(
                watchdog.observe(119),
                Ok(DeviceWatchdogState::PresetApplied)
            );
            assert_eq!(watchdog.preset(), preset);
            assert_eq!(
                watchdog.kick(120),
                Err(FallbackError::ProtectionUnavailable)
            );
            assert_eq!(watchdog.reinitialize(), Ok(()));
            assert_eq!(
                watchdog.arm(119),
                Err(FallbackError::MonotonicTimeRegression)
            );
            assert_eq!(watchdog.arm(200), Ok(()));
        }
    }
}

#[test]
fn watchdog_arm_rejects_a_regressed_clock() {
    let watchdog = digest(0x71).and_then(|preset| DeviceWatchdogSimulator::new(10, preset));
    assert!(watchdog.is_ok());
    if let Ok(mut watchdog) = watchdog {
        assert_eq!(watchdog.observe(100), Ok(DeviceWatchdogState::Disarmed));
        assert_eq!(
            watchdog.arm(99),
            Err(FallbackError::MonotonicTimeRegression)
        );
        assert_eq!(watchdog.arm(100), Ok(()));
    }
}

#[test]
fn hazardous_recovery_requires_explicit_authorization() {
    let fixture = Fixture::new(7, 0x44, 5);
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let controller = fixture
            .plan(1, 0x61)
            .ok()
            .and_then(|plan| FallbackController::new(plan).ok());
        assert!(controller.is_some());
        if let Some(mut controller) = controller {
            assert_eq!(
                controller.activate_running(fixture.region.lease_identity(), 0),
                Ok(())
            );
            assert_eq!(
                controller.trigger_group_failure(fixture.groups[1], FallbackCause::CanError, 10,),
                Ok(())
            );
            let candidate = candidate_identity(8, 0x45);
            assert!(candidate.is_some());
            if let Some(candidate) = candidate {
                let reinit =
                    evidence(0x81).map(|digest| ReinitializationEvidence::new(candidate, digest));
                assert!(reinit.is_ok());
                if let Ok(reinit) = reinit {
                    assert_eq!(
                        controller.begin_recovery(FallbackDomainHandle::new(1), reinit, 10),
                        Ok(())
                    );
                    assert_eq!(
                        controller.observe_recovery_health(FallbackDomainHandle::new(1), 10, true,),
                        Ok(RecoveryProgress::Checking)
                    );
                    assert_eq!(
                        controller.observe_recovery_health(FallbackDomainHandle::new(1), 20, true,),
                        Ok(RecoveryProgress::AwaitingAuthorization)
                    );
                    assert_eq!(
                        controller
                            .diagnostics(FallbackDomainHandle::new(1))
                            .map(FallbackDomainDiagnostics::state),
                        Some(FallbackDomainState::AwaitingAuthorization)
                    );
                    let authorization = evidence(0x82).and_then(|authorization| {
                        evidence(0x83).map(|safety| {
                            RecoveryAuthorization::new(candidate, authorization, safety)
                        })
                    });
                    assert!(authorization.is_ok());
                    if let Ok(authorization) = authorization {
                        assert_eq!(
                            controller.authorize_hazardous_recovery(
                                FallbackDomainHandle::new(1),
                                authorization,
                                21,
                            ),
                            Ok(())
                        );
                        assert_eq!(
                            controller
                                .diagnostics(FallbackDomainHandle::new(1))
                                .map(FallbackDomainDiagnostics::current_identity),
                            Some(candidate)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn hazardous_authorization_expires_with_the_recovery_window() {
    let fixture = Fixture::new(7, 0x44, 5);
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let controller = fixture
            .plan(1, 0x61)
            .ok()
            .and_then(|plan| FallbackController::new(plan).ok());
        let candidate = candidate_identity(8, 0x45);
        assert!(controller.is_some() && candidate.is_some());
        if let (Some(mut controller), Some(candidate)) = (controller, candidate) {
            assert_eq!(
                controller.activate_running(fixture.region.lease_identity(), 0),
                Ok(())
            );
            assert_eq!(
                controller.trigger_group_failure(fixture.groups[1], FallbackCause::CanError, 10),
                Ok(())
            );
            let reinit =
                evidence(0x85).map(|digest| ReinitializationEvidence::new(candidate, digest));
            let authorization = evidence(0x86).and_then(|authorization| {
                evidence(0x87)
                    .map(|safety| RecoveryAuthorization::new(candidate, authorization, safety))
            });
            assert!(reinit.is_ok() && authorization.is_ok());
            if let (Ok(reinit), Ok(authorization)) = (reinit, authorization) {
                assert_eq!(
                    controller.begin_recovery(FallbackDomainHandle::new(1), reinit, 10),
                    Ok(())
                );
                assert_eq!(
                    controller.observe_recovery_health(FallbackDomainHandle::new(1), 10, true,),
                    Ok(RecoveryProgress::Checking)
                );
                assert_eq!(
                    controller.observe_recovery_health(FallbackDomainHandle::new(1), 20, true,),
                    Ok(RecoveryProgress::AwaitingAuthorization)
                );
                assert_eq!(
                    controller.authorize_hazardous_recovery(
                        FallbackDomainHandle::new(1),
                        authorization,
                        110,
                    ),
                    Err(FallbackError::RecoveryLocked)
                );
            }
        }
    }
}

#[test]
fn ordinary_recovery_returns_after_the_bounded_health_gate() {
    let fixture = Fixture::new(7, 0x44, 5);
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let ordinary = fixture
            .plan(1, 0x61)
            .ok()
            .and_then(|plan| FallbackController::new(plan).ok());
        assert!(ordinary.is_some());
        if let Some(mut ordinary) = ordinary {
            assert_eq!(
                ordinary.activate_running(fixture.region.lease_identity(), 0),
                Ok(())
            );
            assert_eq!(
                ordinary.trigger_group_failure(
                    fixture.groups[0],
                    FallbackCause::EthercatWorkingCounter,
                    10,
                ),
                Ok(())
            );
            let candidate = candidate_identity(8, 0x45);
            assert!(candidate.is_some());
            if let Some(candidate) = candidate {
                let reinit =
                    evidence(0x84).map(|digest| ReinitializationEvidence::new(candidate, digest));
                assert!(reinit.is_ok());
                if let Ok(reinit) = reinit {
                    assert_eq!(
                        ordinary.begin_recovery(FallbackDomainHandle::new(0), reinit, 10),
                        Ok(())
                    );
                    assert_eq!(
                        ordinary.observe_recovery_health(FallbackDomainHandle::new(0), 10, true,),
                        Ok(RecoveryProgress::Checking)
                    );
                    assert_eq!(
                        ordinary.observe_recovery_health(FallbackDomainHandle::new(0), 20, true,),
                        Ok(RecoveryProgress::Recovered)
                    );
                }
            }
        }
    }
}

#[test]
fn failed_recovery_obeys_backoff_and_locks_at_the_attempt_limit() {
    let fixture = Fixture::new(7, 0x44, 5);
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let controller = fixture
            .plan(1, 0x61)
            .ok()
            .and_then(|plan| FallbackController::new(plan).ok());
        assert!(controller.is_some());
        if let Some(mut controller) = controller {
            assert_eq!(
                controller.activate_running(fixture.region.lease_identity(), 0),
                Ok(())
            );
            assert_eq!(
                controller.trigger_group_failure(
                    fixture.groups[0],
                    FallbackCause::EthercatApplicationLayer,
                    10,
                ),
                Ok(())
            );
            let first = candidate_identity(8, 0x45).and_then(|identity| {
                evidence(0x91)
                    .ok()
                    .map(|digest| ReinitializationEvidence::new(identity, digest))
            });
            let second = candidate_identity(9, 0x46).and_then(|identity| {
                evidence(0x92)
                    .ok()
                    .map(|digest| ReinitializationEvidence::new(identity, digest))
            });
            assert!(first.is_some() && second.is_some());
            if let (Some(first), Some(second)) = (first, second) {
                assert_eq!(
                    controller.begin_recovery(FallbackDomainHandle::new(0), first, 10),
                    Ok(())
                );
                assert_eq!(
                    controller.observe_recovery_health(FallbackDomainHandle::new(0), 11, false,),
                    Ok(RecoveryProgress::RetryScheduled)
                );
                assert_eq!(
                    controller.begin_recovery(FallbackDomainHandle::new(0), second, 20),
                    Err(FallbackError::RecoveryTooEarly)
                );
                assert_eq!(
                    controller.begin_recovery(FallbackDomainHandle::new(0), second, 21),
                    Ok(())
                );
                assert_eq!(
                    controller.observe_recovery_health(FallbackDomainHandle::new(0), 22, false,),
                    Ok(RecoveryProgress::Locked)
                );
                assert_eq!(
                    controller
                        .diagnostics(FallbackDomainHandle::new(0))
                        .map(FallbackDomainDiagnostics::state),
                    Some(FallbackDomainState::RecoveryLocked)
                );
                assert_eq!(
                    controller.trigger_global_failure(FallbackCause::ControlProcessLost, 23),
                    Ok(())
                );
                assert_eq!(
                    controller
                        .diagnostics(FallbackDomainHandle::new(0))
                        .map(FallbackDomainDiagnostics::state),
                    Some(FallbackDomainState::RecoveryLocked)
                );
            }
        }
    }
}
