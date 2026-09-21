//! R3-01 acceptance tests for exact catalogs, negotiation, lease, and transport descriptors.

use aurora_io_guardian_contracts::{
    CapabilitySet, ConfigurationDigest, ConfigurationGeneration, ContractOffer,
    GuardianConfiguration, GuardianContractError, GuardianEpoch, GuardianErrorCode,
    GuardianLeaseMachine, GuardianState, ImageSequence, IoCapability, LayoutDigest, LeaseId,
    LeaseIdentity, LeasePolicy, LeaseRequest, MemfdSealSet, OutputImageIdentity, PeerCredentials,
    PeerPolicy, SharedRegionOffer, negotiate,
};

const GUARDIAN_SPEC: &str = include_str!("../../../../Contracts/io/v1/guardian-contract.md");

fn capability_set(entries: &[IoCapability]) -> Result<CapabilitySet, GuardianContractError> {
    CapabilitySet::from_ordered(entries)
}

fn negotiated_contract(
    offered: CapabilitySet,
) -> Result<aurora_io_guardian_contracts::NegotiatedContract, GuardianContractError> {
    let older = ContractOffer::new(1, 0, 0, 1, 0, 0, offered, CapabilitySet::SESSION_BASE)?;
    let newer = ContractOffer::new(1, 0, 1, 1, 0, 1, offered, CapabilitySet::SESSION_BASE)?;
    negotiate(older, newer)
}

fn configuration(
    generation: u64,
    marker: u8,
) -> Result<GuardianConfiguration, GuardianContractError> {
    Ok(GuardianConfiguration::new(
        GuardianEpoch::new(7)?,
        ConfigurationGeneration::new(generation)?,
        ConfigurationDigest::from_sha256([marker; 32]),
        LayoutDigest::from_sha256([marker.wrapping_add(1); 32]),
    ))
}

fn lease_identity(
    configuration: GuardianConfiguration,
    marker: u8,
) -> Result<LeaseIdentity, GuardianContractError> {
    Ok(LeaseIdentity::new(
        configuration,
        LeaseId::new([marker; 16])?,
    ))
}

fn policy() -> Result<LeasePolicy<2>, GuardianContractError> {
    LeasePolicy::new(5, 20, [10, 30])
}

#[test]
fn specification_catalogs_match_code_without_extra_or_missing_entries() {
    let documented_capabilities: Vec<&str> = GUARDIAN_SPEC
        .lines()
        .filter_map(|line| {
            let remainder = line.strip_prefix("| `aurora.io.")?;
            let end = remainder.find('`')?;
            Some(&line[3..3 + "aurora.io.".len() + end])
        })
        .collect();
    let implemented_capabilities: Vec<&str> = IoCapability::ALL
        .iter()
        .map(|capability| capability.as_str())
        .collect();
    assert_eq!(documented_capabilities, implemented_capabilities);

    let documented_errors: Vec<(&str, &str)> = GUARDIAN_SPEC
        .lines()
        .filter_map(|line| {
            let remainder = line.strip_prefix("| IO")?;
            let code_end = remainder.find(' ')?;
            let fields: Vec<&str> = line.split('|').map(str::trim).collect();
            Some((&line[2..2 + 2 + code_end], *fields.get(2)?))
        })
        .collect();
    let implemented_errors: Vec<(&str, &str)> = GuardianErrorCode::ALL
        .iter()
        .map(|code| (code.as_str(), code.name()))
        .collect();
    assert_eq!(documented_errors, implemented_errors);
    assert!(!implemented_capabilities.contains(&"aurora.io.opc-ua@1"));
    assert!(!implemented_capabilities.contains(&"aurora.io.mqtt@1"));
}

#[test]
fn negotiation_accepts_both_nn_minus_one_directions_and_rejects_unknown_or_incompatible_offers()
-> Result<(), GuardianContractError> {
    let offered = capability_set(&[
        IoCapability::Guardian,
        IoCapability::Image,
        IoCapability::DriverSdk,
        IoCapability::Serial,
    ])?;
    let n = ContractOffer::new(1, 1, 2, 1, 3, 4, offered, CapabilitySet::SESSION_BASE)?;
    let n_minus_one = ContractOffer::new(1, 1, 1, 1, 3, 3, offered, CapabilitySet::SESSION_BASE)?;
    let control_newer = negotiate(n, n_minus_one)?;
    let guardian_newer = negotiate(n_minus_one, n)?;
    assert_eq!(control_newer.contract_minor(), 1);
    assert_eq!(guardian_newer.contract_minor(), 1);
    assert_eq!(control_newer.layout_minor(), 3);
    assert_eq!(guardian_newer.layout_minor(), 3);

    assert_eq!(
        CapabilitySet::from_raw_bits(1 << 15),
        Err(GuardianContractError::UnknownCapability)
    );
    assert_eq!(
        ContractOffer::new(1, 0, 2, 1, 0, 1, offered, CapabilitySet::SESSION_BASE),
        Err(GuardianContractError::InvalidVersionRange)
    );
    let incompatible = ContractOffer::new(2, 0, 0, 1, 0, 0, offered, CapabilitySet::SESSION_BASE)?;
    assert_eq!(
        negotiate(n_minus_one, incompatible),
        Err(GuardianContractError::UnsupportedContractVersion)
    );
    Ok(())
}

#[test]
fn lease_request_rejects_stale_epoch_and_policy_change_without_reconfiguration()
-> Result<(), GuardianContractError> {
    let offered = capability_set(&[IoCapability::Guardian, IoCapability::Image])?;
    let negotiated = negotiated_contract(offered)?;
    let configuration = configuration(1, 10)?;
    let first_identity = lease_identity(configuration, 1)?;
    let mut machine = GuardianLeaseMachine::<2, 4>::new(configuration, negotiated, policy()?)?;
    machine.arm_fallback()?;
    let stale_configuration = GuardianConfiguration::new(
        GuardianEpoch::new(6)?,
        ConfigurationGeneration::new(1)?,
        ConfigurationDigest::from_sha256([10; 32]),
        LayoutDigest::from_sha256([11; 32]),
    );
    let stale_epoch_identity = lease_identity(stale_configuration, 9)?;
    assert_eq!(
        machine.request_lease(
            LeaseRequest::new(stale_epoch_identity, negotiated, policy()?),
            99,
        ),
        Err(GuardianContractError::ConfigurationMismatch)
    );
    let changed_timing = LeasePolicy::new(5, 20, [11, 30])?;
    assert_eq!(
        machine.request_lease(
            LeaseRequest::new(first_identity, negotiated, changed_timing),
            99,
        ),
        Err(GuardianContractError::ConfigurationMismatch)
    );
    machine.request_lease(
        LeaseRequest::new(first_identity, negotiated, policy()?),
        100,
    )?;
    assert_eq!(machine.state(), GuardianState::LeasePending);
    Ok(())
}

#[test]
fn pending_lease_requires_fresh_heartbeat_and_starts_group_age_at_activation()
-> Result<(), GuardianContractError> {
    let offered = capability_set(&[IoCapability::Guardian, IoCapability::Image])?;
    let negotiated = negotiated_contract(offered)?;
    let configuration = configuration(1, 12)?;
    let identity = lease_identity(configuration, 8)?;
    let mut machine = GuardianLeaseMachine::<2, 2>::new(configuration, negotiated, policy()?)?;
    machine.arm_fallback()?;
    machine.request_lease(LeaseRequest::new(identity, negotiated, policy()?), 100)?;
    machine.record_heartbeat(identity, 110)?;
    let pending_observation = machine.check_deadlines(115)?;
    assert!(!pending_observation.heartbeat_expired());
    assert_eq!(pending_observation.expired_output_groups(), &[false, false]);
    machine.activate_pending(125)?;
    machine.record_heartbeat(identity, 126)?;
    assert_eq!(
        machine.check_deadlines(134)?.expired_output_groups(),
        &[false, false]
    );
    assert_eq!(
        machine.check_deadlines(135)?.expired_output_groups(),
        &[true, false]
    );

    let expired_identity = lease_identity(configuration, 9)?;
    let mut expired = GuardianLeaseMachine::<2, 1>::new(configuration, negotiated, policy()?)?;
    expired.arm_fallback()?;
    expired.request_lease(
        LeaseRequest::new(expired_identity, negotiated, policy()?),
        100,
    )?;
    assert_eq!(
        expired.activate_pending(120),
        Err(GuardianContractError::HeartbeatExpired)
    );
    assert_eq!(expired.state(), GuardianState::Fallback);
    Ok(())
}

#[test]
fn lease_machine_separates_heartbeat_and_group_freshness_and_rejects_replay()
-> Result<(), GuardianContractError> {
    let offered = capability_set(&[IoCapability::Guardian, IoCapability::Image])?;
    let negotiated = negotiated_contract(offered)?;
    let configuration = configuration(1, 10)?;
    let first_identity = lease_identity(configuration, 1)?;
    let mut machine = GuardianLeaseMachine::<2, 4>::new(configuration, negotiated, policy()?)?;

    machine.arm_fallback()?;
    let first_sequence = machine.request_lease(
        LeaseRequest::new(first_identity, negotiated, policy()?),
        100,
    )?;
    assert_eq!(first_sequence.get(), 1);
    machine.activate_pending(100)?;
    machine.accept_output_image(
        OutputImageIdentity::new(first_identity, ImageSequence::new(1)?),
        [true, false],
        105,
    )?;
    machine.record_heartbeat(first_identity, 110)?;

    let freshness = machine.check_deadlines(115)?;
    assert!(!freshness.heartbeat_expired());
    assert_eq!(freshness.expired_output_groups(), &[true, false]);
    assert!(machine.output_group_is_stale(0)?);
    assert!(!machine.output_group_is_stale(1)?);
    assert_eq!(
        machine.accept_output_image(
            OutputImageIdentity::new(first_identity, ImageSequence::new(2)?),
            [true, false],
            116,
        ),
        Err(GuardianContractError::OutputGroupExpired)
    );
    machine.accept_output_image(
        OutputImageIdentity::new(first_identity, ImageSequence::new(2)?),
        [false, true],
        116,
    )?;
    assert_eq!(
        machine.accept_output_image(
            OutputImageIdentity::new(first_identity, ImageSequence::new(2)?),
            [false, true],
            117,
        ),
        Err(GuardianContractError::ImageSequenceViolation)
    );
    assert_eq!(
        machine.accept_output_image(
            OutputImageIdentity::new(first_identity, ImageSequence::new(4)?),
            [false, true],
            118,
        ),
        Err(GuardianContractError::ImageSequenceViolation)
    );
    machine.accept_output_image(
        OutputImageIdentity::new(first_identity, ImageSequence::new(3)?),
        [false, true],
        118,
    )?;

    let heartbeat = machine.check_deadlines(130)?;
    assert!(heartbeat.heartbeat_expired());
    assert_eq!(machine.state(), GuardianState::Fallback);
    assert_eq!(machine.active_lease_identity(), None);

    machine.arm_fallback()?;
    assert_eq!(
        machine.request_lease(
            LeaseRequest::new(first_identity, negotiated, policy()?),
            131,
        ),
        Err(GuardianContractError::LeaseIdReused)
    );
    let second_identity = lease_identity(configuration, 2)?;
    let second_sequence = machine.request_lease(
        LeaseRequest::new(second_identity, negotiated, policy()?),
        131,
    )?;
    assert_eq!(second_sequence.get(), 2);
    machine.activate_pending(131)?;
    assert_eq!(
        machine.accept_output_image(
            OutputImageIdentity::new(first_identity, ImageSequence::new(3)?),
            [false, true],
            132,
        ),
        Err(GuardianContractError::StaleOrForeignLease)
    );
    Ok(())
}

#[test]
fn configuration_change_is_exact_next_and_requires_rearming_and_a_new_lease()
-> Result<(), GuardianContractError> {
    let base = capability_set(&[IoCapability::Guardian, IoCapability::Image])?;
    let expanded = capability_set(&[
        IoCapability::Guardian,
        IoCapability::Image,
        IoCapability::DriverSdk,
    ])?;
    let old_negotiated = negotiated_contract(base)?;
    let new_negotiated = negotiated_contract(expanded)?;
    let old_configuration = configuration(1, 20)?;
    let new_configuration = configuration(2, 21)?;
    let mut machine =
        GuardianLeaseMachine::<2, 4>::new(old_configuration, old_negotiated, policy()?)?;
    machine.arm_fallback()?;
    assert_eq!(
        machine.begin_reconfiguration(configuration(3, 22)?, new_negotiated, policy()?),
        Err(GuardianContractError::ConfigurationGenerationViolation)
    );
    machine.begin_reconfiguration(new_configuration, new_negotiated, policy()?)?;
    assert_eq!(machine.state(), GuardianState::Reinitializing);
    assert_eq!(machine.configuration(), old_configuration);
    machine.complete_reconfiguration()?;
    assert_eq!(machine.configuration(), new_configuration);
    assert_eq!(machine.state(), GuardianState::Fallback);

    let new_identity = lease_identity(new_configuration, 3)?;
    assert_eq!(
        machine.request_lease(
            LeaseRequest::new(new_identity, new_negotiated, policy()?),
            200,
        ),
        Err(GuardianContractError::FallbackNotArmed)
    );
    machine.arm_fallback()?;
    machine.request_lease(
        LeaseRequest::new(new_identity, new_negotiated, policy()?),
        200,
    )?;
    machine.activate_pending(200)?;
    assert_eq!(machine.state(), GuardianState::Running);
    Ok(())
}

#[test]
fn aborted_reconfiguration_cannot_reuse_previous_health_approval()
-> Result<(), GuardianContractError> {
    let offered = capability_set(&[IoCapability::Guardian, IoCapability::Image])?;
    let negotiated = negotiated_contract(offered)?;
    let old_configuration = configuration(1, 24)?;
    let mut machine = GuardianLeaseMachine::<2, 2>::new(old_configuration, negotiated, policy()?)?;
    machine.arm_fallback()?;
    machine.begin_reconfiguration(configuration(2, 25)?, negotiated, policy()?)?;
    machine.abort_reconfiguration()?;
    assert_eq!(machine.configuration(), old_configuration);
    assert_eq!(machine.state(), GuardianState::Fallback);
    let identity = lease_identity(old_configuration, 7)?;
    assert_eq!(
        machine.request_lease(LeaseRequest::new(identity, negotiated, policy()?), 1,),
        Err(GuardianContractError::FallbackNotArmed)
    );
    Ok(())
}

#[test]
fn fixed_lease_history_rejects_capacity_instead_of_forgetting_old_ids()
-> Result<(), GuardianContractError> {
    let offered = capability_set(&[IoCapability::Guardian, IoCapability::Image])?;
    let negotiated = negotiated_contract(offered)?;
    let configuration = configuration(1, 30)?;
    let first = lease_identity(configuration, 4)?;
    let second = lease_identity(configuration, 5)?;
    let mut machine = GuardianLeaseMachine::<2, 1>::new(configuration, negotiated, policy()?)?;
    machine.arm_fallback()?;
    machine.request_lease(LeaseRequest::new(first, negotiated, policy()?), 1)?;
    machine.activate_pending(1)?;
    machine.revoke_lease()?;
    machine.arm_fallback()?;
    assert_eq!(
        machine.request_lease(LeaseRequest::new(second, negotiated, policy()?), 2),
        Err(GuardianContractError::LeaseCapacityExceeded)
    );
    assert_eq!(machine.state(), GuardianState::Fallback);
    Ok(())
}

#[test]
fn transport_descriptor_requires_exact_peer_and_memfd_contract() -> Result<(), GuardianContractError>
{
    let configuration = configuration(1, 40)?;
    let identity = lease_identity(configuration, 6)?;
    let policy = PeerPolicy::new(1000, 1001, [9; 32]);
    policy.validate(PeerCredentials::new(20, 1000, 1001, [9; 32])?)?;
    policy.validate(PeerCredentials::new(21, 1000, 1001, [9; 32])?)?;
    assert_eq!(
        policy.validate(PeerCredentials::new(22, 1000, 1002, [9; 32])?),
        Err(GuardianContractError::PeerIdentityMismatch)
    );
    assert_eq!(
        SharedRegionOffer::new(4097, MemfdSealSet::REQUIRED, identity),
        Err(GuardianContractError::InvalidSharedRegionOffer)
    );
    let incomplete_seals = MemfdSealSet::from_raw_bits(
        MemfdSealSet::GROW.raw_bits() | MemfdSealSet::SHRINK.raw_bits(),
    )?;
    assert_eq!(
        SharedRegionOffer::new(4096, incomplete_seals, identity),
        Err(GuardianContractError::InvalidSharedRegionOffer)
    );
    let offer = SharedRegionOffer::new(4096, MemfdSealSet::REQUIRED, identity)?;
    assert_eq!(offer.byte_length(), 4096);
    assert_eq!(offer.lease_identity(), identity);
    Ok(())
}
