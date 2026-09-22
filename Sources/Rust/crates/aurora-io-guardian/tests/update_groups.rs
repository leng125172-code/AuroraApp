//! R3-03 exact update-group planning, scheduling, queue, and diagnostic acceptance tests.

use aurora_io_guardian::{
    BitOrder, BoundedGroupQueue, ByteOrder, CapabilityDigest, GroupDescriptor, GroupHandle,
    GroupHealth, ImageDirection, ImageError, ImageLayout, ImageMapping, InterfaceHandle,
    OperationClass, OperationHandle, OperationObservation, OperationResult, ProtectionLevel,
    ProtocolSourceKind, QueueAdmission, RegionHeader, RetryProofDigest, ScalarType,
    ScheduledOperation, SourceDescriptor, SourceHandle, UpdateGroupError, UpdateGroupLimits,
    UpdateGroupPlan, UpdateGroupScheduler, UpdateGroupSpec, ValueBinding,
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
    values: [ValueBinding; 2],
    specifications: [UpdateGroupSpec; 2],
    operations: [ScheduledOperation; 2],
}

impl Fixture {
    fn new() -> Option<Self> {
        let layout = ImageLayout::new(4, 4, 1, 1, 1, 1, 4_096).ok()?;
        let configuration = GuardianConfiguration::new(
            GuardianEpoch::new(5).ok()?,
            ConfigurationGeneration::new(7).ok()?,
            ConfigurationDigest::from_sha256([0x11; 32]),
            LayoutDigest::from_sha256([0x22; 32]),
        );
        let region = RegionHeader::new(
            layout,
            LeaseIdentity::new(configuration, LeaseId::new([0x44; 16]).ok()?),
            LeaseSequence::new(9).ok()?,
            CapabilityDigest::from_sha256([0x33; 32]),
        );
        let sources = [
            SourceDescriptor::new(
                SourceHandle::new(0),
                ProtocolSourceKind::Ethercat,
                [0x51; 32],
                [0x61; 32],
            ),
            SourceDescriptor::new(
                SourceHandle::new(1),
                ProtocolSourceKind::Can,
                [0x52; 32],
                [0x62; 32],
            ),
        ];
        let groups = [
            GroupDescriptor::new(
                ImageDirection::Input,
                GroupHandle::new(0),
                SourceHandle::new(0),
            ),
            GroupDescriptor::new(
                ImageDirection::Output,
                GroupHandle::new(0),
                SourceHandle::new(1),
            ),
        ];
        let values = [
            ValueBinding::new(
                LocalHandle::new(0).ok()?,
                tag(1)?,
                ImageDirection::Input,
                ScalarType::U32,
                0,
                0,
                ByteOrder::LittleEndian,
                BitOrder::Lsb0,
                SourceHandle::new(0),
                GroupHandle::new(0),
                None,
            ),
            ValueBinding::new(
                LocalHandle::new(1).ok()?,
                tag(2)?,
                ImageDirection::Output,
                ScalarType::U32,
                0,
                0,
                ByteOrder::LittleEndian,
                BitOrder::Lsb0,
                SourceHandle::new(1),
                GroupHandle::new(0),
                Some(ProtectionLevel::DeviceWatchdogProtected),
            ),
        ];
        let specifications = [
            group_spec(groups[0], 1, 0, 1, 1, 40)?,
            group_spec(groups[1], 0, 0, 1, 1, 40)?,
        ];
        let operations = [
            ScheduledOperation::new(
                groups[0],
                OperationHandle::new(0),
                OperationClass::ReadPoll,
                1,
                None,
            )
            .ok()?,
            ScheduledOperation::new(
                groups[1],
                OperationHandle::new(0),
                OperationClass::NonIdempotent,
                1,
                None,
            )
            .ok()?,
        ];
        Some(Self {
            region,
            sources,
            groups,
            values,
            specifications,
            operations,
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

    fn plan(&self) -> Option<UpdateGroupPlan> {
        UpdateGroupPlan::new(
            self.region,
            self.mapping().ok()?,
            &self.specifications,
            &self.operations,
            UpdateGroupLimits::new(2, 2, 8).ok()?,
        )
        .ok()
    }
}

fn group_spec(
    descriptor: GroupDescriptor,
    priority: u16,
    maximum_retries: u16,
    maximum_attempts: u32,
    frame_capacity: u32,
    release_budget_ns: u64,
) -> Option<UpdateGroupSpec> {
    UpdateGroupSpec::new(
        descriptor,
        InterfaceHandle::new(0),
        100,
        0,
        priority,
        10,
        60,
        60,
        release_budget_ns,
        maximum_attempts,
        frame_capacity,
        2,
        10,
        maximum_retries,
        if maximum_retries == 0 { 0 } else { 5 },
        20,
        aurora_io_guardian::GroupMissPolicy::new(4, 2, 3).ok()?,
        2,
        1_000,
    )
    .ok()
}

fn phased_group_spec(
    descriptor: GroupDescriptor,
    period_ns: u64,
    phase_ns: u64,
    priority: u16,
) -> Option<UpdateGroupSpec> {
    UpdateGroupSpec::new(
        descriptor,
        InterfaceHandle::new(0),
        period_ns,
        phase_ns,
        priority,
        10,
        40,
        40,
        30,
        1,
        1,
        2,
        10,
        0,
        0,
        20,
        aurora_io_guardian::GroupMissPolicy::new(4, 2, 3).ok()?,
        2,
        1_000,
    )
    .ok()
}

fn tag(discriminator: u8) -> Option<TagId> {
    let mut bytes = [
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x8f,
    ];
    bytes[15] = discriminator;
    TagId::from_bytes(bytes).ok()
}

fn protocol_value(index: u32) -> Option<ValueBinding> {
    Some(ValueBinding::new(
        LocalHandle::new(index).ok()?,
        tag(u8::try_from(index.checked_add(10)?).ok()?)?,
        ImageDirection::Input,
        ScalarType::U32,
        index.checked_mul(4)?,
        0,
        ByteOrder::LittleEndian,
        BitOrder::Lsb0,
        SourceHandle::new(index),
        GroupHandle::new(u16::try_from(index).ok()?),
        None,
    ))
}

fn protocol_operation(group: GroupDescriptor) -> Option<ScheduledOperation> {
    ScheduledOperation::new(
        group,
        OperationHandle::new(0),
        OperationClass::ReadPoll,
        1,
        None,
    )
    .ok()
}

fn complete_success(
    scheduler: &mut UpdateGroupScheduler,
    ticket: aurora_io_guardian::GroupReleaseTicket,
    specification: UpdateGroupSpec,
    now_ns: u64,
) -> Result<aurora_io_guardian::GroupReleaseOutcome, UpdateGroupError> {
    let queue = BoundedGroupQueue::<u32>::new(specification)?.snapshot();
    let observations = [OperationObservation::new(
        OperationHandle::new(0),
        1,
        OperationResult::Success,
    )];
    let sample = match ticket.descriptor().direction() {
        ImageDirection::Input => Some(now_ns),
        ImageDirection::Output => None,
    };
    scheduler.complete(ticket, now_ns, sample, &observations, queue)
}

#[test]
fn exact_group_and_operation_closure_rejects_missing_extra_and_limit_drift() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let mapping = fixture.mapping();
        assert!(mapping.is_ok());
        if let Ok(mapping) = mapping {
            let reversed = [fixture.specifications[1], fixture.specifications[0]];
            let normal_limits = UpdateGroupLimits::new(2, 2, 8);
            let undersized_limits = UpdateGroupLimits::new(1, 2, 8);
            assert!(normal_limits.is_ok() && undersized_limits.is_ok());
            if let (Ok(normal_limits), Ok(undersized_limits)) = (normal_limits, undersized_limits) {
                assert!(matches!(
                    UpdateGroupPlan::new(
                        fixture.region,
                        mapping,
                        &reversed,
                        &fixture.operations,
                        normal_limits,
                    ),
                    Err(UpdateGroupError::GroupClosureMismatch)
                ));
                assert!(matches!(
                    UpdateGroupPlan::new(
                        fixture.region,
                        mapping,
                        &fixture.specifications,
                        &fixture.operations[..1],
                        normal_limits,
                    ),
                    Err(UpdateGroupError::OperationClosureMismatch)
                ));
                assert!(matches!(
                    UpdateGroupPlan::new(
                        fixture.region,
                        mapping,
                        &fixture.specifications,
                        &fixture.operations,
                        undersized_limits,
                    ),
                    Err(UpdateGroupError::InvalidCapacity)
                ));
            }
        }
    }
}

#[test]
fn direction_specific_queues_never_overwrite_or_grow() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let input = BoundedGroupQueue::<u32>::new(fixture.specifications[0]);
        let output = BoundedGroupQueue::<u32>::new(fixture.specifications[1]);
        assert!(input.is_ok() && output.is_ok());
        if let (Ok(mut input), Ok(mut output)) = (input, output) {
            assert_eq!(input.try_push(1), QueueAdmission::Enqueued);
            assert_eq!(input.try_push(2), QueueAdmission::Enqueued);
            assert_eq!(input.try_push(3), QueueAdmission::DroppedNewest(3));
            assert_eq!(input.pop(), Some(1));
            assert_eq!(input.pop(), Some(2));
            assert_eq!(input.pop(), None);
            assert_eq!(input.snapshot().capacity(), 2);
            assert_eq!(input.snapshot().high_water(), 2);
            assert_eq!(input.snapshot().dropped_newest(), 1);

            assert_eq!(output.try_push(4), QueueAdmission::Enqueued);
            assert_eq!(output.try_push(5), QueueAdmission::Enqueued);
            assert_eq!(output.try_push(6), QueueAdmission::RejectedNewest(6));
            assert_eq!(output.pop(), Some(4));
            assert_eq!(output.pop(), Some(5));
            assert_eq!(output.snapshot().rejected_newest(), 1);
        }
    }
}

fn deterministic_trace(fixture: &Fixture) -> Option<Vec<(ImageDirection, u64, u64)>> {
    let mut scheduler = fixture.plan()?.start(1_000).ok()?;
    let mut trace = Vec::new();
    trace.try_reserve_exact(256).ok()?;
    for cycle in 0..128_u64 {
        let release = 1_000_u64.checked_add(cycle.checked_mul(100)?)?;
        for _ in 0..2 {
            let ticket = scheduler.select(release).ok()??;
            trace.push((
                ticket.descriptor().direction(),
                ticket.release_ordinal(),
                ticket.release_monotonic_ns(),
            ));
            let specification = match ticket.descriptor().direction() {
                ImageDirection::Input => fixture.specifications[0],
                ImageDirection::Output => fixture.specifications[1],
            };
            assert_eq!(
                complete_success(
                    &mut scheduler,
                    ticket,
                    specification,
                    release.checked_add(5)?
                )
                .ok()?,
                aurora_io_guardian::GroupReleaseOutcome::Completed
            );
        }
        assert!(scheduler.select(release).ok()?.is_none());
    }
    Some(trace)
}

#[test]
fn absolute_grid_trace_is_repeatable_and_uses_stable_tie_order() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let first = deterministic_trace(&fixture);
        let second = deterministic_trace(&fixture);
        assert!(first.is_some() && second.is_some());
        if let (Some(first), Some(second)) = (first, second) {
            assert_eq!(first, second);
            assert_eq!(first.len(), 256);
            assert_eq!(first[0], (ImageDirection::Output, 0, 1_000));
            assert_eq!(first[1], (ImageDirection::Input, 0, 1_000));
            assert_eq!(first[254], (ImageDirection::Output, 127, 13_700));
            assert_eq!(first[255], (ImageDirection::Input, 127, 13_700));
        }
    }
}

#[test]
fn skipped_releases_do_not_catch_up_and_slow_group_does_not_block_peer() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let scheduler = fixture.plan().and_then(|plan| plan.start(1_000).ok());
        assert!(scheduler.is_some());
        if let Some(mut scheduler) = scheduler {
            let output_ticket = scheduler.select(1_205).ok().flatten();
            assert!(output_ticket.is_some());
            if let Some(output_ticket) = output_ticket {
                assert_eq!(
                    output_ticket.descriptor().direction(),
                    ImageDirection::Output
                );
                assert_eq!(output_ticket.release_ordinal(), 2);

                let input_ticket = scheduler.select(1_205).ok().flatten();
                assert!(input_ticket.is_some());
                if let Some(input_ticket) = input_ticket {
                    assert_eq!(input_ticket.descriptor().direction(), ImageDirection::Input);
                    assert_eq!(input_ticket.release_ordinal(), 2);
                    assert!(
                        complete_success(
                            &mut scheduler,
                            input_ticket,
                            fixture.specifications[0],
                            1_210,
                        )
                        .is_ok()
                    );
                }

                let next_input = scheduler.select(1_305).ok().flatten();
                assert!(next_input.is_some());
                if let Some(next_input) = next_input {
                    assert_eq!(next_input.descriptor().direction(), ImageDirection::Input);
                    assert_eq!(next_input.release_ordinal(), 3);
                    assert!(
                        complete_success(
                            &mut scheduler,
                            next_input,
                            fixture.specifications[0],
                            1_310,
                        )
                        .is_ok()
                    );
                }

                let output_diagnostics =
                    scheduler.diagnostics(ImageDirection::Output, GroupHandle::new(0));
                assert!(output_diagnostics.is_some());
                if let Some(output_diagnostics) = output_diagnostics {
                    assert_eq!(output_diagnostics.schedule_misses(), 2);
                    assert_eq!(
                        output_diagnostics.health(),
                        GroupHealth::PerformanceRejected
                    );
                    assert_eq!(output_diagnostics.timeout_count(), 1);
                    assert_eq!(output_diagnostics.outcome_unknown_count(), 1);
                    assert_eq!(
                        output_diagnostics.output_refresh_status(),
                        aurora_io_guardian::OutputRefreshStatus::OutcomeUnknown
                    );
                    assert_eq!(output_diagnostics.late_response_count(), 0);
                }

                let queue = BoundedGroupQueue::<u32>::new(fixture.specifications[1]);
                assert!(queue.is_ok());
                if let Ok(queue) = queue {
                    let observations = [OperationObservation::new(
                        OperationHandle::new(0),
                        1,
                        OperationResult::Success,
                    )];
                    assert_eq!(
                        scheduler.complete(
                            output_ticket,
                            1_310,
                            None,
                            &observations,
                            queue.snapshot(),
                        ),
                        Err(UpdateGroupError::StaleOrForeignTicket)
                    );
                    let diagnostics =
                        scheduler.diagnostics(ImageDirection::Output, GroupHandle::new(0));
                    assert_eq!(
                        diagnostics
                            .map(aurora_io_guardian::UpdateGroupDiagnostics::late_response_count),
                        Some(1)
                    );
                }
            }
        }
    }
}

#[test]
fn retry_proof_budget_stale_input_and_uncertain_output_are_explicit() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        assert_eq!(
            ScheduledOperation::new(
                fixture.groups[1],
                OperationHandle::new(0),
                OperationClass::IdempotentSet,
                1,
                None,
            ),
            Err(UpdateGroupError::RetryNotProven)
        );
        let proof = RetryProofDigest::new([0x7a; 32]);
        assert!(proof.is_ok());
        if let Ok(proof) = proof {
            let retry_spec = group_spec(fixture.groups[1], 0, 2, 3, 3, 50);
            let under_budget = group_spec(fixture.groups[1], 0, 2, 3, 3, 49);
            let retry_operation = ScheduledOperation::new(
                fixture.groups[1],
                OperationHandle::new(0),
                OperationClass::IdempotentSet,
                1,
                Some(proof),
            );
            assert!(retry_spec.is_some() && under_budget.is_some() && retry_operation.is_ok());
            if let (Some(retry_spec), Some(under_budget), Ok(retry_operation)) =
                (retry_spec, under_budget, retry_operation)
            {
                let specs = [fixture.specifications[0], retry_spec];
                let operations = [fixture.operations[0], retry_operation];
                let mapping = fixture.mapping();
                let limits = UpdateGroupLimits::new(2, 2, 8);
                assert!(mapping.is_ok() && limits.is_ok());
                if let (Ok(mapping), Ok(limits)) = (mapping, limits) {
                    let plan =
                        UpdateGroupPlan::new(fixture.region, mapping, &specs, &operations, limits);
                    assert!(plan.is_ok());
                    let under_budget_specs = [fixture.specifications[0], under_budget];
                    assert!(matches!(
                        UpdateGroupPlan::new(
                            fixture.region,
                            mapping,
                            &under_budget_specs,
                            &operations,
                            limits,
                        ),
                        Err(UpdateGroupError::BudgetExceeded)
                    ));
                }
            }
        }

        let scheduler = fixture.plan().and_then(|plan| plan.start(1_000).ok());
        assert!(scheduler.is_some());
        if let Some(mut scheduler) = scheduler {
            let output = scheduler.select(1_000).ok().flatten();
            assert!(output.is_some());
            if let Some(output) = output {
                let queue = BoundedGroupQueue::<u32>::new(fixture.specifications[1]);
                assert!(queue.is_ok());
                if let Ok(queue) = queue {
                    let illegal_retry = [OperationObservation::new(
                        OperationHandle::new(0),
                        2,
                        OperationResult::Timeout,
                    )];
                    assert_eq!(
                        scheduler.complete(output, 1_005, None, &illegal_retry, queue.snapshot()),
                        Err(UpdateGroupError::RetryNotProven)
                    );
                    let timeout = [OperationObservation::new(
                        OperationHandle::new(0),
                        1,
                        OperationResult::Timeout,
                    )];
                    assert_eq!(
                        scheduler.complete(output, 1_005, None, &timeout, queue.snapshot()),
                        Ok(aurora_io_guardian::GroupReleaseOutcome::OutcomeUnknown)
                    );
                }
            }

            let input = scheduler.select(1_000).ok().flatten();
            assert!(input.is_some());
            if let Some(input) = input {
                let queue = BoundedGroupQueue::<u32>::new(fixture.specifications[0]);
                assert!(queue.is_ok());
                if let Ok(queue) = queue {
                    let success = [OperationObservation::new(
                        OperationHandle::new(0),
                        1,
                        OperationResult::Success,
                    )];
                    assert_eq!(
                        scheduler.complete(input, 1_030, Some(1_000), &success, queue.snapshot()),
                        Ok(aurora_io_guardian::GroupReleaseOutcome::StaleInput)
                    );
                }
            }
        }
    }
}

fn assert_late_non_idempotent_output(
    scheduler: &mut UpdateGroupScheduler,
    specification: UpdateGroupSpec,
) {
    let late_ticket = scheduler.select(1_100).ok().flatten();
    let queue = BoundedGroupQueue::<u32>::new(specification);
    assert!(late_ticket.is_some() && queue.is_ok());
    if let (Some(late_ticket), Ok(queue)) = (late_ticket, queue) {
        let timeout = [OperationObservation::new(
            OperationHandle::new(0),
            1,
            OperationResult::Timeout,
        )];
        assert_eq!(
            scheduler.complete(late_ticket, 1_141, None, &timeout, queue.snapshot()),
            Ok(aurora_io_guardian::GroupReleaseOutcome::OutcomeUnknown)
        );
        assert_eq!(
            scheduler
                .diagnostics(ImageDirection::Output, GroupHandle::new(0))
                .map(aurora_io_guardian::UpdateGroupDiagnostics::late_response_count),
            Some(1)
        );
    }
}

#[test]
fn release_tickets_bind_the_exact_lease_and_timestamps_cannot_precede_release() {
    let old_fixture = Fixture::new();
    let new_fixture = Fixture::new();
    assert!(old_fixture.is_some() && new_fixture.is_some());
    if let (Some(old_fixture), Some(mut new_fixture)) = (old_fixture, new_fixture) {
        let new_lease = LeaseId::new([0x55; 16]);
        assert!(new_lease.is_ok());
        if let Ok(new_lease) = new_lease {
            new_fixture.region = RegionHeader::new(
                new_fixture.region.layout(),
                LeaseIdentity::new(
                    new_fixture.region.lease_identity().configuration(),
                    new_lease,
                ),
                new_fixture.region.lease_sequence(),
                new_fixture.region.capability_digest(),
            );
            let old_scheduler = old_fixture.plan().and_then(|plan| plan.start(1_000).ok());
            let new_scheduler = new_fixture.plan().and_then(|plan| plan.start(1_000).ok());
            assert!(old_scheduler.is_some() && new_scheduler.is_some());
            if let (Some(mut old_scheduler), Some(mut new_scheduler)) =
                (old_scheduler, new_scheduler)
            {
                assert_eq!(
                    new_scheduler
                        .diagnostics(ImageDirection::Output, GroupHandle::new(0))
                        .map(aurora_io_guardian::UpdateGroupDiagnostics::output_refresh_status),
                    Some(aurora_io_guardian::OutputRefreshStatus::Pending)
                );
                let old_ticket = old_scheduler.select(1_000).ok().flatten();
                let new_ticket = new_scheduler.select(1_000).ok().flatten();
                assert!(old_ticket.is_some() && new_ticket.is_some());
                if let (Some(old_ticket), Some(new_ticket)) = (old_ticket, new_ticket) {
                    let queue = BoundedGroupQueue::<u32>::new(new_fixture.specifications[1]);
                    assert!(queue.is_ok());
                    if let Ok(queue) = queue {
                        let observations = [OperationObservation::new(
                            OperationHandle::new(0),
                            1,
                            OperationResult::Success,
                        )];
                        assert_eq!(
                            new_scheduler.complete(
                                old_ticket,
                                1_005,
                                None,
                                &observations,
                                queue.snapshot(),
                            ),
                            Err(UpdateGroupError::StaleOrForeignTicket)
                        );
                        assert_eq!(
                            new_scheduler.complete(
                                new_ticket,
                                999,
                                None,
                                &observations,
                                queue.snapshot(),
                            ),
                            Err(UpdateGroupError::InvalidObservation)
                        );
                        assert_eq!(
                            new_scheduler.complete(
                                new_ticket,
                                1_005,
                                None,
                                &observations,
                                queue.snapshot(),
                            ),
                            Ok(aurora_io_guardian::GroupReleaseOutcome::Completed)
                        );
                        assert_late_non_idempotent_output(
                            &mut new_scheduler,
                            new_fixture.specifications[1],
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn input_sample_before_release_and_queue_overflow_cannot_publish_good() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let scheduler = fixture.plan().and_then(|plan| plan.start(1_000).ok());
        assert!(scheduler.is_some());
        if let Some(mut scheduler) = scheduler {
            let output = scheduler.select(1_000).ok().flatten();
            assert!(output.is_some());
            if let Some(output) = output {
                let queue = BoundedGroupQueue::<u32>::new(fixture.specifications[1]);
                assert!(queue.is_ok());
                if let Ok(mut queue) = queue {
                    assert_eq!(queue.try_push(1), QueueAdmission::Enqueued);
                    assert_eq!(queue.try_push(2), QueueAdmission::Enqueued);
                    assert_eq!(queue.try_push(3), QueueAdmission::RejectedNewest(3));
                    let observations = [OperationObservation::new(
                        OperationHandle::new(0),
                        1,
                        OperationResult::Success,
                    )];
                    assert_eq!(
                        scheduler.complete(output, 1_005, None, &observations, queue.snapshot(),),
                        Ok(aurora_io_guardian::GroupReleaseOutcome::Failed(
                            aurora_io_guardian::GapReason::QueueFull
                        ))
                    );
                    let diagnostics =
                        scheduler.diagnostics(ImageDirection::Output, GroupHandle::new(0));
                    assert_eq!(
                        diagnostics
                            .map(aurora_io_guardian::UpdateGroupDiagnostics::queue_full_count),
                        Some(1)
                    );
                    assert_eq!(
                        diagnostics
                            .map(aurora_io_guardian::UpdateGroupDiagnostics::rejected_newest),
                        Some(1)
                    );
                }
            }

            let input = scheduler.select(1_000).ok().flatten();
            assert!(input.is_some());
            if let Some(input) = input {
                let queue = BoundedGroupQueue::<u32>::new(fixture.specifications[0]);
                assert!(queue.is_ok());
                if let Ok(queue) = queue {
                    let observations = [OperationObservation::new(
                        OperationHandle::new(0),
                        1,
                        OperationResult::Success,
                    )];
                    assert_eq!(
                        scheduler.complete(
                            input,
                            1_005,
                            Some(999),
                            &observations,
                            queue.snapshot(),
                        ),
                        Err(UpdateGroupError::InvalidObservation)
                    );
                    assert_eq!(
                        scheduler.complete(
                            input,
                            1_005,
                            Some(1_000),
                            &observations,
                            queue.snapshot(),
                        ),
                        Ok(aurora_io_guardian::GroupReleaseOutcome::Completed)
                    );
                }
            }
        }
    }
}

#[test]
fn queue_snapshot_cannot_cross_group_identity() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let scheduler = fixture.plan().and_then(|plan| plan.start(1_000).ok());
        assert!(scheduler.is_some());
        if let Some(mut scheduler) = scheduler {
            assert!(scheduler.select(1_000).ok().flatten().is_some());
            let input = scheduler.select(1_000).ok().flatten();
            let foreign_specification = group_spec(
                GroupDescriptor::new(
                    ImageDirection::Input,
                    GroupHandle::new(1),
                    SourceHandle::new(0),
                ),
                1,
                0,
                1,
                1,
                40,
            );
            assert!(input.is_some() && foreign_specification.is_some());
            if let (Some(input), Some(foreign_specification)) = (input, foreign_specification) {
                let foreign_queue = BoundedGroupQueue::<u32>::new(foreign_specification);
                let correct_queue = BoundedGroupQueue::<u32>::new(fixture.specifications[0]);
                assert!(foreign_queue.is_ok() && correct_queue.is_ok());
                if let (Ok(foreign_queue), Ok(correct_queue)) = (foreign_queue, correct_queue) {
                    let observations = [OperationObservation::new(
                        OperationHandle::new(0),
                        1,
                        OperationResult::Success,
                    )];
                    assert_eq!(
                        scheduler.complete(
                            input,
                            1_005,
                            Some(1_000),
                            &observations,
                            foreign_queue.snapshot(),
                        ),
                        Err(UpdateGroupError::QueueSnapshotMismatch)
                    );
                    assert_eq!(
                        scheduler.complete(
                            input,
                            1_005,
                            Some(1_000),
                            &observations,
                            correct_queue.snapshot(),
                        ),
                        Ok(aurora_io_guardian::GroupReleaseOutcome::Completed)
                    );
                }
            }
        }
    }
}

#[test]
fn equal_release_uses_phase_before_priority_and_disabled_recovery_is_explicit() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let input = phased_group_spec(fixture.groups[0], 100, 50, 0);
        let output = phased_group_spec(fixture.groups[1], 50, 0, 9);
        let no_recovery = aurora_io_guardian::GroupMissPolicy::new(4, 2, 3).and_then(|policy| {
            UpdateGroupSpec::new(
                fixture.groups[0],
                InterfaceHandle::new(0),
                100,
                0,
                0,
                0,
                60,
                60,
                20,
                1,
                1,
                2,
                10,
                0,
                0,
                20,
                policy,
                0,
                0,
            )
        });
        assert!(input.is_some() && output.is_some() && no_recovery.is_ok());
        if let (Some(input), Some(output), Ok(no_recovery)) = (input, output, no_recovery) {
            let limits = UpdateGroupLimits::new(2, 2, 8);
            let mapping = fixture.mapping();
            assert!(limits.is_ok() && mapping.is_ok());
            if let (Ok(limits), Ok(mapping)) = (limits, mapping) {
                let no_recovery_specs = [no_recovery, fixture.specifications[1]];
                assert!(
                    UpdateGroupPlan::new(
                        fixture.region,
                        mapping,
                        &no_recovery_specs,
                        &fixture.operations,
                        limits,
                    )
                    .is_ok()
                );

                let specifications = [input, output];
                let scheduler = UpdateGroupPlan::new(
                    fixture.region,
                    mapping,
                    &specifications,
                    &fixture.operations,
                    limits,
                )
                .ok()
                .and_then(|plan| plan.start(1_000).ok());
                assert!(scheduler.is_some());
                if let Some(mut scheduler) = scheduler {
                    let initial_output = scheduler.select(1_000).ok().flatten();
                    assert!(initial_output.is_some());
                    if let Some(initial_output) = initial_output {
                        assert!(
                            complete_success(&mut scheduler, initial_output, output, 1_005,)
                                .is_ok()
                        );
                    }
                    let equal_release = scheduler.select(1_050).ok().flatten();
                    assert!(equal_release.is_some());
                    if let Some(equal_release) = equal_release {
                        assert_eq!(
                            equal_release.descriptor().direction(),
                            ImageDirection::Output
                        );
                        assert_eq!(equal_release.release_ordinal(), 1);
                    }
                }
            }
        }
    }
}

fn protocol_plan_group_count() -> Option<usize> {
    let protocols = [
        ProtocolSourceKind::Ethercat,
        ProtocolSourceKind::ModbusTcp,
        ProtocolSourceKind::ModbusRtu,
        ProtocolSourceKind::Serial,
        ProtocolSourceKind::Can,
        ProtocolSourceKind::Lin,
    ];
    let layout = ImageLayout::new(24, 0, 6, 0, 6, 0, 8_192).ok()?;
    let configuration = GuardianConfiguration::new(
        GuardianEpoch::new(1).ok()?,
        ConfigurationGeneration::new(1).ok()?,
        ConfigurationDigest::from_sha256([1; 32]),
        LayoutDigest::from_sha256([2; 32]),
    );
    let region = RegionHeader::new(
        layout,
        LeaseIdentity::new(configuration, LeaseId::new([3; 16]).ok()?),
        LeaseSequence::new(1).ok()?,
        CapabilityDigest::from_sha256([4; 32]),
    );
    let sources: [SourceDescriptor; 6] = core::array::from_fn(|index| {
        let byte = u8::try_from(index + 1).unwrap_or(u8::MAX);
        SourceDescriptor::new(
            SourceHandle::new(u32::try_from(index).unwrap_or(u32::MAX)),
            protocols[index],
            [byte; 32],
            [byte.saturating_add(16); 32],
        )
    });
    let groups: [GroupDescriptor; 6] = core::array::from_fn(|index| {
        GroupDescriptor::new(
            ImageDirection::Input,
            GroupHandle::new(u16::try_from(index).unwrap_or(u16::MAX)),
            SourceHandle::new(u32::try_from(index).unwrap_or(u32::MAX)),
        )
    });
    let values = [
        protocol_value(0)?,
        protocol_value(1)?,
        protocol_value(2)?,
        protocol_value(3)?,
        protocol_value(4)?,
        protocol_value(5)?,
    ];
    let specifications = [
        group_spec(groups[0], 0, 0, 1, 1, 40)?,
        group_spec(groups[1], 1, 0, 1, 1, 40)?,
        group_spec(groups[2], 2, 0, 1, 1, 40)?,
        group_spec(groups[3], 3, 0, 1, 1, 40)?,
        group_spec(groups[4], 4, 0, 1, 1, 40)?,
        group_spec(groups[5], 5, 0, 1, 1, 40)?,
    ];
    let operations = [
        protocol_operation(groups[0])?,
        protocol_operation(groups[1])?,
        protocol_operation(groups[2])?,
        protocol_operation(groups[3])?,
        protocol_operation(groups[4])?,
        protocol_operation(groups[5])?,
    ];
    let mapping = ImageMapping::new(
        region.layout(),
        region.lease_identity().configuration().layout_digest(),
        region.capability_digest(),
        &sources,
        &groups,
        &values,
    )
    .ok()?;
    let plan = UpdateGroupPlan::new(
        region,
        mapping,
        &specifications,
        &operations,
        UpdateGroupLimits::new(6, 6, 24).ok()?,
    )
    .ok()?;
    Some(plan.group_count())
}

#[test]
fn all_frozen_protocol_classes_share_the_same_bounded_plan_contract() {
    assert_eq!(protocol_plan_group_count(), Some(6));
}
