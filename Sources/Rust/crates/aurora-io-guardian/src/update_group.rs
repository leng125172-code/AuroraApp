//! Fixed update-group plans and bounded absolute-grid scheduling.

use std::{cmp::min, ops::Range};

use aurora_io_guardian_contracts::{
    ConfigurationGeneration, GuardianEpoch, LeaseIdentity, LeaseSequence,
};
use thiserror::Error;

use crate::{
    AggregateQuality, CapabilityDigest, GapReason, GroupDescriptor, GroupHandle,
    GroupQueueSnapshot, ImageDirection, ImageMapping, RegionHeader,
};

/// Failure returned before an invalid plan or runtime observation can affect another group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum UpdateGroupError {
    /// A fixed capacity or limit is zero or exceeded.
    #[error("update-group capacity is zero or exceeds its declared limit")]
    InvalidCapacity,
    /// Period, phase, window, jitter, timeout, or recovery timing is inconsistent.
    #[error("update-group timing is inconsistent")]
    InvalidTiming,
    /// Checked time, work, sequence, or capacity arithmetic overflowed.
    #[error("update-group arithmetic overflowed")]
    ArithmeticOverflow,
    /// The plan does not belong to the exact image/configuration identity.
    #[error("update-group plan identity does not match the image mapping")]
    MappingIdentityMismatch,
    /// A group is missing, duplicated, extra, reordered, or bound to another source.
    #[error("update-group catalog is not the exact canonical mapping closure")]
    GroupClosureMismatch,
    /// An operation is missing, duplicated, extra, reordered, or references another group.
    #[error("update-group operation catalog is not an exact dense closure")]
    OperationClosureMismatch,
    /// Retry was requested without a non-zero proof for an absolute idempotent set.
    #[error("update-group retry is not proven idempotent")]
    RetryNotProven,
    /// Worst-case attempts, frames/requests, or timeout work exceed the release budget.
    #[error("update-group worst-case work exceeds its fixed release budget")]
    BudgetExceeded,
    /// A bounded initialization allocation failed.
    #[error("update-group initialization allocation failed")]
    AllocationFailed,
    /// A completion refers to an expired, completed, or foreign release ticket.
    #[error("update-group release ticket is stale or foreign")]
    StaleOrForeignTicket,
    /// Runtime operation observations are incomplete, extra, reordered, or inconsistent.
    #[error("update-group operation observation closure is invalid")]
    InvalidObservation,
    /// Queue evidence does not belong to the group or regresses prior counters.
    #[error("update-group queue snapshot is inconsistent")]
    QueueSnapshotMismatch,
}

/// Stable identity of one physical/logical bus interface in a schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InterfaceHandle(u16);

impl InterfaceHandle {
    /// Creates a fixed interface handle.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the integer handle used for deterministic ordering.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Direction-local dense operation handle inside one update group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationHandle(u16);

impl OperationHandle {
    /// Creates an operation handle whose density is validated by the plan.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the direction-local integer value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Build-time operation semantics that determine timeout and retry behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationClass {
    /// Absolute set with a reviewed device/protocol idempotency proof.
    IdempotentSet,
    /// Write whose repeated execution can change behavior or state.
    NonIdempotent,
    /// Pulse or edge command that must never be synthesized twice.
    PulseOrEdge,
    /// Read-only poll; Preview 1.0 does not retry it automatically.
    ReadPoll,
}

/// Non-zero digest of the reviewed evidence permitting idempotent retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RetryProofDigest([u8; 32]);

impl RetryProofDigest {
    /// Creates a proof digest.
    ///
    /// # Errors
    ///
    /// Rejects the all-zero value so an omitted proof cannot be mistaken for approval.
    pub const fn new(bytes: [u8; 32]) -> Result<Self, UpdateGroupError> {
        let mut index = 0;
        while index < bytes.len() {
            if bytes[index] != 0 {
                return Ok(Self(bytes));
            }
            index += 1;
        }
        Err(UpdateGroupError::RetryNotProven)
    }

    /// Returns all digest bytes.
    #[must_use]
    pub const fn to_sha256(self) -> [u8; 32] {
        self.0
    }
}

/// Fixed rolling/consecutive miss admission policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupMissPolicy {
    window: u16,
    maximum_misses: u16,
    consecutive_misses: u16,
}

impl GroupMissPolicy {
    /// Creates a policy with a meaningful rejecting boundary.
    ///
    /// # Errors
    ///
    /// Rejects zero fields, a maximum equal to the whole window, or a consecutive threshold
    /// larger than the window.
    pub const fn new(
        window: u16,
        maximum_misses: u16,
        consecutive_misses: u16,
    ) -> Result<Self, UpdateGroupError> {
        if window == 0
            || maximum_misses >= window
            || consecutive_misses == 0
            || consecutive_misses > window
        {
            return Err(UpdateGroupError::InvalidCapacity);
        }
        Ok(Self {
            window,
            maximum_misses,
            consecutive_misses,
        })
    }

    /// Returns the fixed rolling-window length.
    #[must_use]
    pub const fn window(self) -> u16 {
        self.window
    }

    /// Returns the maximum admitted misses in the rolling window.
    #[must_use]
    pub const fn maximum_misses(self) -> u16 {
        self.maximum_misses
    }

    /// Returns the rejecting consecutive-miss threshold.
    #[must_use]
    pub const fn consecutive_misses(self) -> u16 {
        self.consecutive_misses
    }
}

/// Complete immutable timing and resource specification for one mapped update group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UpdateGroupSpec {
    descriptor: GroupDescriptor,
    interface: InterfaceHandle,
    period_ns: u64,
    phase_ns: u64,
    priority: u16,
    maximum_jitter_ns: u64,
    input_sample_window_ns: u64,
    output_refresh_window_ns: u64,
    release_budget_ns: u64,
    maximum_operations_per_release: u32,
    frame_or_request_capacity: u32,
    queue_capacity: u32,
    timeout_ns: u64,
    maximum_retries: u16,
    retry_backoff_ns: u64,
    stale_after_ns: u64,
    miss_policy: GroupMissPolicy,
    maximum_recovery_attempts: u16,
    recovery_window_ns: u64,
}

impl UpdateGroupSpec {
    /// Creates one fully bounded group specification.
    ///
    /// Jitter and execution budget must fit the direction-relevant input/output window, and every
    /// window fits one period so releases cannot overlap. Recovery fields are retained for the
    /// non-cyclic recovery owner and are not executed by this scheduler.
    ///
    /// # Errors
    ///
    /// Rejects zero capacities, overlapping releases, invalid phase/backoff/recovery pairs, or
    /// timing that cannot be represented within one period. Zero jitter and zero recovery attempts
    /// are explicit supported policies; disabled recovery requires a zero recovery window.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        descriptor: GroupDescriptor,
        interface: InterfaceHandle,
        period_ns: u64,
        phase_ns: u64,
        priority: u16,
        maximum_jitter_ns: u64,
        input_sample_window_ns: u64,
        output_refresh_window_ns: u64,
        release_budget_ns: u64,
        maximum_operations_per_release: u32,
        frame_or_request_capacity: u32,
        queue_capacity: u32,
        timeout_ns: u64,
        maximum_retries: u16,
        retry_backoff_ns: u64,
        stale_after_ns: u64,
        miss_policy: GroupMissPolicy,
        maximum_recovery_attempts: u16,
        recovery_window_ns: u64,
    ) -> Result<Self, UpdateGroupError> {
        if period_ns == 0
            || phase_ns >= period_ns
            || input_sample_window_ns == 0
            || output_refresh_window_ns == 0
            || release_budget_ns == 0
            || maximum_operations_per_release == 0
            || frame_or_request_capacity == 0
            || queue_capacity == 0
            || timeout_ns == 0
            || stale_after_ns == 0
        {
            return Err(UpdateGroupError::InvalidCapacity);
        }
        if input_sample_window_ns > period_ns || output_refresh_window_ns > period_ns {
            return Err(UpdateGroupError::InvalidTiming);
        }
        let active_window = match descriptor.direction() {
            ImageDirection::Input => input_sample_window_ns,
            ImageDirection::Output => output_refresh_window_ns,
        };
        if maximum_jitter_ns > release_budget_ns
            || release_budget_ns > active_window
            || timeout_ns > release_budget_ns
            || (maximum_retries == 0 && retry_backoff_ns != 0)
            || (maximum_retries != 0 && retry_backoff_ns == 0)
            || (maximum_recovery_attempts == 0 && recovery_window_ns != 0)
            || (maximum_recovery_attempts != 0 && recovery_window_ns == 0)
        {
            return Err(UpdateGroupError::InvalidTiming);
        }
        Ok(Self {
            descriptor,
            interface,
            period_ns,
            phase_ns,
            priority,
            maximum_jitter_ns,
            input_sample_window_ns,
            output_refresh_window_ns,
            release_budget_ns,
            maximum_operations_per_release,
            frame_or_request_capacity,
            queue_capacity,
            timeout_ns,
            maximum_retries,
            retry_backoff_ns,
            stale_after_ns,
            miss_policy,
            maximum_recovery_attempts,
            recovery_window_ns,
        })
    }

    /// Returns the exact mapped group descriptor.
    #[must_use]
    pub const fn descriptor(self) -> GroupDescriptor {
        self.descriptor
    }

    /// Returns the stable interface identity used in tie ordering.
    #[must_use]
    pub const fn interface(self) -> InterfaceHandle {
        self.interface
    }

    /// Returns the absolute-grid period in nanoseconds.
    #[must_use]
    pub const fn period_ns(self) -> u64 {
        self.period_ns
    }

    /// Returns the phase from scheduler start in nanoseconds.
    #[must_use]
    pub const fn phase_ns(self) -> u64 {
        self.phase_ns
    }

    /// Returns the ascending deterministic tie-break priority.
    #[must_use]
    pub const fn priority(self) -> u16 {
        self.priority
    }

    /// Returns the maximum admitted dispatch jitter in nanoseconds.
    #[must_use]
    pub const fn maximum_jitter_ns(self) -> u64 {
        self.maximum_jitter_ns
    }

    /// Returns the input sample window in nanoseconds.
    #[must_use]
    pub const fn input_sample_window_ns(self) -> u64 {
        self.input_sample_window_ns
    }

    /// Returns the output refresh window in nanoseconds.
    #[must_use]
    pub const fn output_refresh_window_ns(self) -> u64 {
        self.output_refresh_window_ns
    }

    /// Returns the execution/retry budget per release in nanoseconds.
    #[must_use]
    pub const fn release_budget_ns(self) -> u64 {
        self.release_budget_ns
    }

    /// Returns the maximum total operation attempts per release.
    #[must_use]
    pub const fn maximum_operations_per_release(self) -> u32 {
        self.maximum_operations_per_release
    }

    /// Returns the maximum total frame/request units per release.
    #[must_use]
    pub const fn frame_or_request_capacity(self) -> u32 {
        self.frame_or_request_capacity
    }

    /// Returns the immutable application queue capacity.
    #[must_use]
    pub const fn queue_capacity(self) -> u32 {
        self.queue_capacity
    }

    /// Returns the timeout of one attempt in nanoseconds.
    #[must_use]
    pub const fn timeout_ns(self) -> u64 {
        self.timeout_ns
    }

    /// Returns retries available only to proven idempotent operations.
    #[must_use]
    pub const fn maximum_retries(self) -> u16 {
        self.maximum_retries
    }

    /// Returns the fixed retry backoff in nanoseconds.
    #[must_use]
    pub const fn retry_backoff_ns(self) -> u64 {
        self.retry_backoff_ns
    }

    /// Returns the maximum admitted input age in nanoseconds.
    #[must_use]
    pub const fn stale_after_ns(self) -> u64 {
        self.stale_after_ns
    }

    /// Returns the rolling/consecutive miss policy.
    #[must_use]
    pub const fn miss_policy(self) -> GroupMissPolicy {
        self.miss_policy
    }

    /// Returns the non-cyclic recovery attempt bound.
    #[must_use]
    pub const fn maximum_recovery_attempts(self) -> u16 {
        self.maximum_recovery_attempts
    }

    /// Returns the non-cyclic recovery window in nanoseconds.
    #[must_use]
    pub const fn recovery_window_ns(self) -> u64 {
        self.recovery_window_ns
    }

    const fn active_window_ns(self) -> u64 {
        match self.descriptor.direction() {
            ImageDirection::Input => self.input_sample_window_ns,
            ImageDirection::Output => self.output_refresh_window_ns,
        }
    }
}

/// One exact scheduled protocol operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScheduledOperation {
    group: GroupDescriptor,
    handle: OperationHandle,
    class: OperationClass,
    frame_or_request_units: u32,
    retry_proof: Option<RetryProofDigest>,
}

impl ScheduledOperation {
    /// Creates one operation and validates its retry-proof shape.
    ///
    /// # Errors
    ///
    /// Requires a proof only for `IdempotentSet` and rejects zero work units.
    pub const fn new(
        group: GroupDescriptor,
        handle: OperationHandle,
        class: OperationClass,
        frame_or_request_units: u32,
        retry_proof: Option<RetryProofDigest>,
    ) -> Result<Self, UpdateGroupError> {
        if frame_or_request_units == 0 {
            return Err(UpdateGroupError::InvalidCapacity);
        }
        match (class, retry_proof) {
            (OperationClass::IdempotentSet, Some(_))
            | (
                OperationClass::NonIdempotent
                | OperationClass::PulseOrEdge
                | OperationClass::ReadPoll,
                None,
            ) => Ok(Self {
                group,
                handle,
                class,
                frame_or_request_units,
                retry_proof,
            }),
            (OperationClass::IdempotentSet, None)
            | (
                OperationClass::NonIdempotent
                | OperationClass::PulseOrEdge
                | OperationClass::ReadPoll,
                Some(_),
            ) => Err(UpdateGroupError::RetryNotProven),
        }
    }

    /// Returns the owning group.
    #[must_use]
    pub const fn group(self) -> GroupDescriptor {
        self.group
    }

    /// Returns the group-local dense operation handle.
    #[must_use]
    pub const fn handle(self) -> OperationHandle {
        self.handle
    }

    /// Returns the fixed operation semantics.
    #[must_use]
    pub const fn class(self) -> OperationClass {
        self.class
    }

    /// Returns frame/request work units consumed by one attempt.
    #[must_use]
    pub const fn frame_or_request_units(self) -> u32 {
        self.frame_or_request_units
    }

    /// Returns the retry proof for an idempotent set.
    #[must_use]
    pub const fn retry_proof(self) -> Option<RetryProofDigest> {
        self.retry_proof
    }
}

/// Initialization limits for the complete update-group plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UpdateGroupLimits {
    groups: u16,
    operations: u32,
    miss_history_slots: u32,
}

impl UpdateGroupLimits {
    /// Creates non-zero plan-wide limits.
    ///
    /// # Errors
    ///
    /// Rejects any zero limit.
    pub const fn new(
        maximum_groups: u16,
        maximum_operations: u32,
        maximum_miss_history_slots: u32,
    ) -> Result<Self, UpdateGroupError> {
        if maximum_groups == 0 || maximum_operations == 0 || maximum_miss_history_slots == 0 {
            return Err(UpdateGroupError::InvalidCapacity);
        }
        Ok(Self {
            groups: maximum_groups,
            operations: maximum_operations,
            miss_history_slots: maximum_miss_history_slots,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedGroup {
    specification: UpdateGroupSpec,
    operations: Range<usize>,
}

/// Owned immutable update-group plan built before cyclic execution.
pub struct UpdateGroupPlan {
    region: RegionHeader,
    groups: Box<[PlannedGroup]>,
    operations: Box<[ScheduledOperation]>,
}

impl UpdateGroupPlan {
    /// Validates exact group/operation closure and all worst-case work before allocating the plan.
    ///
    /// Group specifications must appear in the exact mapping order. Operations must be grouped in
    /// that same order and use dense zero-based handles within each group. Worst-case idempotent
    /// retries are included in attempt, frame/request, and time admission.
    ///
    /// # Errors
    ///
    /// Rejects identity drift, missing/extra/reordered entries, unproven retry, any limit excess,
    /// arithmetic overflow, or bounded allocation failure.
    pub fn new(
        region: RegionHeader,
        mapping: ImageMapping<'_>,
        specifications: &[UpdateGroupSpec],
        operations: &[ScheduledOperation],
        limits: UpdateGroupLimits,
    ) -> Result<Self, UpdateGroupError> {
        validate_mapping_identity(&region, mapping)?;
        if specifications.len() != mapping.groups().len() {
            return Err(UpdateGroupError::GroupClosureMismatch);
        }
        if specifications.len() > usize::from(limits.groups)
            || operations.len()
                > usize::try_from(limits.operations)
                    .map_err(|_| UpdateGroupError::InvalidCapacity)?
        {
            return Err(UpdateGroupError::InvalidCapacity);
        }
        let miss_slots = specifications
            .iter()
            .try_fold(0_u32, |total, specification| {
                total
                    .checked_add(u32::from(specification.miss_policy().window()))
                    .ok_or(UpdateGroupError::ArithmeticOverflow)
            })?;
        if miss_slots > limits.miss_history_slots {
            return Err(UpdateGroupError::InvalidCapacity);
        }

        let mut planned = Vec::new();
        planned
            .try_reserve_exact(specifications.len())
            .map_err(|_| UpdateGroupError::AllocationFailed)?;
        let mut operation_index = 0_usize;
        for (mapping_group, specification) in mapping.groups().iter().zip(specifications) {
            if specification.descriptor() != *mapping_group {
                return Err(UpdateGroupError::GroupClosureMismatch);
            }
            let start = operation_index;
            while operation_index < operations.len()
                && operations[operation_index].group() == *mapping_group
            {
                let expected = u16::try_from(operation_index - start)
                    .map_err(|_| UpdateGroupError::InvalidCapacity)?;
                if operations[operation_index].handle().get() != expected {
                    return Err(UpdateGroupError::OperationClosureMismatch);
                }
                operation_index += 1;
            }
            if operation_index == start {
                return Err(UpdateGroupError::OperationClosureMismatch);
            }
            admit_worst_case(specification, &operations[start..operation_index])?;
            planned.push(PlannedGroup {
                specification: *specification,
                operations: start..operation_index,
            });
        }
        if operation_index != operations.len() {
            return Err(UpdateGroupError::OperationClosureMismatch);
        }
        Ok(Self {
            region,
            groups: planned.into_boxed_slice(),
            operations: copy_boxed(operations)?,
        })
    }

    /// Returns the exact number of planned groups.
    #[must_use]
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }

    /// Allocates fixed per-group runtime state and anchors every group to one monotonic start.
    ///
    /// # Errors
    ///
    /// Rejects an unrepresentable first release or bounded allocation failure.
    pub fn start(self, scheduler_start_ns: u64) -> Result<UpdateGroupScheduler, UpdateGroupError> {
        let mut runtimes = Vec::new();
        runtimes
            .try_reserve_exact(self.groups.len())
            .map_err(|_| UpdateGroupError::AllocationFailed)?;
        for group in &self.groups {
            scheduler_start_ns
                .checked_add(group.specification.phase_ns())
                .ok_or(UpdateGroupError::ArithmeticOverflow)?;
            let miss_capacity = usize::from(group.specification.miss_policy().window());
            let mut history = Vec::new();
            history
                .try_reserve_exact(miss_capacity)
                .map_err(|_| UpdateGroupError::AllocationFailed)?;
            history.resize(miss_capacity, false);
            runtimes.push(GroupRuntime::new(
                history.into_boxed_slice(),
                group.specification.descriptor().direction(),
            ));
        }
        Ok(UpdateGroupScheduler {
            plan: self,
            scheduler_start_ns,
            next_dispatch_sequence: 1,
            runtimes: runtimes.into_boxed_slice(),
        })
    }
}

fn validate_mapping_identity(
    region: &RegionHeader,
    mapping: ImageMapping<'_>,
) -> Result<(), UpdateGroupError> {
    if mapping.layout() != region.layout()
        || mapping.layout_digest() != region.lease_identity().configuration().layout_digest()
        || mapping.capability_digest() != region.capability_digest()
    {
        return Err(UpdateGroupError::MappingIdentityMismatch);
    }
    Ok(())
}

fn admit_worst_case(
    specification: &UpdateGroupSpec,
    operations: &[ScheduledOperation],
) -> Result<(), UpdateGroupError> {
    let mut attempts = 0_u32;
    let mut work_units = 0_u32;
    let mut work_ns = 0_u64;
    let mut has_retryable = false;
    for operation in operations {
        let retries = if matches!(operation.class(), OperationClass::IdempotentSet) {
            has_retryable = true;
            u32::from(specification.maximum_retries())
        } else {
            0
        };
        let operation_attempts = retries
            .checked_add(1)
            .ok_or(UpdateGroupError::ArithmeticOverflow)?;
        attempts = attempts
            .checked_add(operation_attempts)
            .ok_or(UpdateGroupError::ArithmeticOverflow)?;
        work_units = work_units
            .checked_add(
                operation
                    .frame_or_request_units()
                    .checked_mul(operation_attempts)
                    .ok_or(UpdateGroupError::ArithmeticOverflow)?,
            )
            .ok_or(UpdateGroupError::ArithmeticOverflow)?;
        work_ns = work_ns
            .checked_add(
                specification
                    .timeout_ns()
                    .checked_mul(u64::from(operation_attempts))
                    .ok_or(UpdateGroupError::ArithmeticOverflow)?,
            )
            .and_then(|value| {
                value.checked_add(
                    specification
                        .retry_backoff_ns()
                        .checked_mul(u64::from(retries))?,
                )
            })
            .ok_or(UpdateGroupError::ArithmeticOverflow)?;
    }
    if specification.maximum_retries() != 0 && !has_retryable {
        return Err(UpdateGroupError::RetryNotProven);
    }
    let release_work_ns = specification
        .maximum_jitter_ns()
        .checked_add(work_ns)
        .ok_or(UpdateGroupError::ArithmeticOverflow)?;
    if attempts > specification.maximum_operations_per_release()
        || work_units > specification.frame_or_request_capacity()
        || release_work_ns > specification.release_budget_ns()
    {
        return Err(UpdateGroupError::BudgetExceeded);
    }
    Ok(())
}

fn copy_boxed<T: Copy>(values: &[T]) -> Result<Box<[T]>, UpdateGroupError> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(values.len())
        .map_err(|_| UpdateGroupError::AllocationFailed)?;
    copied.extend_from_slice(values);
    Ok(copied.into_boxed_slice())
}

/// One result for one exact scheduled operation in a release report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationResult {
    /// The operation completed and its protocol validation succeeded.
    Success,
    /// The bounded attempt timed out.
    Timeout,
    /// CRC, checksum, or frame integrity validation failed.
    Checksum,
    /// `EtherCAT` working counter differed from the fixed expectation.
    WorkingCounter,
    /// CAN/LIN/serial or another protocol reported an error frame/state.
    ErrorFrame,
    /// A fixed application or transport queue was full.
    QueueFull,
    /// The link or transport was disconnected.
    LinkDown,
    /// The physical device reported a fault.
    DeviceFault,
    /// The operation was explicitly not attempted in this release.
    NotAttempted,
}

/// Exact runtime observation for one planned operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OperationObservation {
    handle: OperationHandle,
    attempts: u16,
    result: OperationResult,
}

impl OperationObservation {
    /// Creates an observation whose full semantics are checked against the plan at completion.
    #[must_use]
    pub const fn new(handle: OperationHandle, attempts: u16, result: OperationResult) -> Self {
        Self {
            handle,
            attempts,
            result,
        }
    }
}

/// Opaque bounded release authority issued by one scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupReleaseTicket {
    group_index: u16,
    dispatch_sequence: u64,
    release_ordinal: u64,
    release_monotonic_ns: u64,
    deadline_monotonic_ns: u64,
    descriptor: GroupDescriptor,
    lease_identity: LeaseIdentity,
    lease_sequence: LeaseSequence,
    capability_digest: CapabilityDigest,
}

impl GroupReleaseTicket {
    /// Returns the mapped group.
    #[must_use]
    pub const fn descriptor(self) -> GroupDescriptor {
        self.descriptor
    }

    /// Returns the absolute-grid release ordinal.
    #[must_use]
    pub const fn release_ordinal(self) -> u64 {
        self.release_ordinal
    }

    /// Returns the absolute monotonic release time.
    #[must_use]
    pub const fn release_monotonic_ns(self) -> u64 {
        self.release_monotonic_ns
    }

    /// Returns the latest admitted completion time.
    #[must_use]
    pub const fn deadline_monotonic_ns(self) -> u64 {
        self.deadline_monotonic_ns
    }

    /// Returns the exact Guardian epoch.
    #[must_use]
    pub const fn guardian_epoch(self) -> GuardianEpoch {
        self.lease_identity.configuration().epoch()
    }

    /// Returns the exact immutable configuration generation.
    #[must_use]
    pub const fn configuration_generation(self) -> ConfigurationGeneration {
        self.lease_identity.configuration().generation()
    }

    /// Returns the exact lease sequence.
    #[must_use]
    pub const fn lease_sequence(self) -> LeaseSequence {
        self.lease_sequence
    }
}

/// Current update-group health; rejection is latched until a new scheduler/lease is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupHealth {
    /// No miss remains in the rolling window and the latest release was healthy.
    Healthy,
    /// A bounded failure occurred but policy thresholds have not been crossed.
    Degraded,
    /// Rolling or consecutive miss admission failed; cyclic recovery is forbidden.
    PerformanceRejected,
}

/// Physical-output refresh observation for one output group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputRefreshStatus {
    /// The group is an input group.
    NotApplicable,
    /// The output group has not reached its first release yet.
    Pending,
    /// The latest output release completed inside its window.
    Refreshed,
    /// The latest output release missed, failed, or exceeded its window.
    Missed,
    /// A non-idempotent or edge timeout left physical execution uncertain.
    OutcomeUnknown,
}

/// Final result of one consumed release ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupReleaseOutcome {
    /// All operations completed and input freshness/output timing passed.
    Completed,
    /// Input bytes were sampled but exceeded the fixed age boundary.
    StaleInput,
    /// A typed protocol/transport failure occurred.
    Failed(GapReason),
    /// Not all fixed work fit the release.
    BudgetExceeded,
    /// Dispatch or completion exceeded the release window.
    WindowMissed,
    /// A non-idempotent or edge timeout may have changed the physical device.
    OutcomeUnknown,
}

/// Immutable diagnostics for one update group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateGroupDiagnostics {
    descriptor: GroupDescriptor,
    health: GroupHealth,
    next_release_ordinal: u64,
    release_count: u64,
    successful_releases: u64,
    schedule_misses: u64,
    total_misses: u64,
    rolling_misses: u16,
    consecutive_misses: u64,
    timeout_count: u64,
    checksum_count: u64,
    working_counter_count: u64,
    error_frame_count: u64,
    queue_full_count: u64,
    budget_exceeded_count: u64,
    stale_input_count: u64,
    outcome_unknown_count: u64,
    late_response_count: u64,
    queue_depth: u32,
    queue_high_water: u32,
    dropped_newest: u64,
    rejected_newest: u64,
    last_sample_monotonic_ns: Option<u64>,
    last_input_age_ns: Option<u64>,
    last_quality: AggregateQuality,
    last_gap_reason: GapReason,
    output_refresh_status: OutputRefreshStatus,
}

impl UpdateGroupDiagnostics {
    /// Returns the mapped group.
    #[must_use]
    pub const fn descriptor(self) -> GroupDescriptor {
        self.descriptor
    }

    /// Returns current latched health.
    #[must_use]
    pub const fn health(self) -> GroupHealth {
        self.health
    }

    /// Returns the next absolute release ordinal.
    #[must_use]
    pub const fn next_release_ordinal(self) -> u64 {
        self.next_release_ordinal
    }

    /// Returns dispatched release count.
    #[must_use]
    pub const fn release_count(self) -> u64 {
        self.release_count
    }

    /// Returns successful release count.
    #[must_use]
    pub const fn successful_releases(self) -> u64 {
        self.successful_releases
    }

    /// Returns releases skipped before dispatch.
    #[must_use]
    pub const fn schedule_misses(self) -> u64 {
        self.schedule_misses
    }

    /// Returns all saturated failed/missed release count.
    #[must_use]
    pub const fn total_misses(self) -> u64 {
        self.total_misses
    }

    /// Returns misses currently present in the fixed rolling window.
    #[must_use]
    pub const fn rolling_misses(self) -> u16 {
        self.rolling_misses
    }

    /// Returns current consecutive misses.
    #[must_use]
    pub const fn consecutive_misses(self) -> u64 {
        self.consecutive_misses
    }

    /// Returns bounded operation timeout count.
    #[must_use]
    pub const fn timeout_count(self) -> u64 {
        self.timeout_count
    }

    /// Returns CRC/checksum failure count.
    #[must_use]
    pub const fn checksum_count(self) -> u64 {
        self.checksum_count
    }

    /// Returns `EtherCAT` working-counter failure count.
    #[must_use]
    pub const fn working_counter_count(self) -> u64 {
        self.working_counter_count
    }

    /// Returns protocol error-frame/state count.
    #[must_use]
    pub const fn error_frame_count(self) -> u64 {
        self.error_frame_count
    }

    /// Returns queue-full operation count.
    #[must_use]
    pub const fn queue_full_count(self) -> u64 {
        self.queue_full_count
    }

    /// Returns releases whose fixed work was incomplete or above capacity.
    #[must_use]
    pub const fn budget_exceeded_count(self) -> u64 {
        self.budget_exceeded_count
    }

    /// Returns stale input release count.
    #[must_use]
    pub const fn stale_input_count(self) -> u64 {
        self.stale_input_count
    }

    /// Returns non-idempotent/edge timeout count.
    #[must_use]
    pub const fn outcome_unknown_count(self) -> u64 {
        self.outcome_unknown_count
    }

    /// Returns completion reports received after their bounded deadline.
    #[must_use]
    pub const fn late_response_count(self) -> u64 {
        self.late_response_count
    }

    /// Returns current queue depth.
    #[must_use]
    pub const fn queue_depth(self) -> u32 {
        self.queue_depth
    }

    /// Returns queue high-water mark.
    #[must_use]
    pub const fn queue_high_water(self) -> u32 {
        self.queue_high_water
    }

    /// Returns input `DropNewest` count.
    #[must_use]
    pub const fn dropped_newest(self) -> u64 {
        self.dropped_newest
    }

    /// Returns output `RejectNewest` count.
    #[must_use]
    pub const fn rejected_newest(self) -> u64 {
        self.rejected_newest
    }

    /// Returns the last reported input sample time.
    #[must_use]
    pub const fn last_sample_monotonic_ns(self) -> Option<u64> {
        self.last_sample_monotonic_ns
    }

    /// Returns the last computed input age.
    #[must_use]
    pub const fn last_input_age_ns(self) -> Option<u64> {
        self.last_input_age_ns
    }

    /// Returns the latest derived aggregate quality.
    #[must_use]
    pub const fn last_quality(self) -> AggregateQuality {
        self.last_quality
    }

    /// Returns the latest explicit gap reason.
    #[must_use]
    pub const fn last_gap_reason(self) -> GapReason {
        self.last_gap_reason
    }

    /// Returns latest output refresh state.
    #[must_use]
    pub const fn output_refresh_status(self) -> OutputRefreshStatus {
        self.output_refresh_status
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlight {
    dispatch_sequence: u64,
    release_ordinal: u64,
    deadline_monotonic_ns: u64,
}

struct GroupRuntime {
    next_release_ordinal: u64,
    in_flight: Option<InFlight>,
    last_expired: Option<InFlight>,
    miss_history: Box<[bool]>,
    miss_cursor: usize,
    rolling_misses: u16,
    consecutive_misses: u64,
    health: GroupHealth,
    release_count: u64,
    successful_releases: u64,
    schedule_misses: u64,
    total_misses: u64,
    timeout_count: u64,
    checksum_count: u64,
    working_counter_count: u64,
    error_frame_count: u64,
    queue_full_count: u64,
    budget_exceeded_count: u64,
    stale_input_count: u64,
    outcome_unknown_count: u64,
    late_response_count: u64,
    queue_depth: u32,
    queue_high_water: u32,
    dropped_newest: u64,
    rejected_newest: u64,
    last_sample_monotonic_ns: Option<u64>,
    last_input_age_ns: Option<u64>,
    last_quality: AggregateQuality,
    last_gap_reason: GapReason,
    output_refresh_status: OutputRefreshStatus,
}

impl GroupRuntime {
    fn new(miss_history: Box<[bool]>, direction: ImageDirection) -> Self {
        Self {
            next_release_ordinal: 0,
            in_flight: None,
            last_expired: None,
            miss_history,
            miss_cursor: 0,
            rolling_misses: 0,
            consecutive_misses: 0,
            health: GroupHealth::Healthy,
            release_count: 0,
            successful_releases: 0,
            schedule_misses: 0,
            total_misses: 0,
            timeout_count: 0,
            checksum_count: 0,
            working_counter_count: 0,
            error_frame_count: 0,
            queue_full_count: 0,
            budget_exceeded_count: 0,
            stale_input_count: 0,
            outcome_unknown_count: 0,
            late_response_count: 0,
            queue_depth: 0,
            queue_high_water: 0,
            dropped_newest: 0,
            rejected_newest: 0,
            last_sample_monotonic_ns: None,
            last_input_age_ns: None,
            last_quality: AggregateQuality::Stale,
            last_gap_reason: GapReason::NotSampled,
            output_refresh_status: match direction {
                ImageDirection::Input => OutputRefreshStatus::NotApplicable,
                ImageDirection::Output => OutputRefreshStatus::Pending,
            },
        }
    }
}

/// Active bounded scheduler for one immutable lease/configuration.
pub struct UpdateGroupScheduler {
    plan: UpdateGroupPlan,
    scheduler_start_ns: u64,
    next_dispatch_sequence: u64,
    runtimes: Box<[GroupRuntime]>,
}

type SelectionKey = (u64, u16, u64, u16, u8, u16);

impl UpdateGroupScheduler {
    /// Selects at most one due group in deterministic absolute-grid order.
    ///
    /// The method scans the fixed group array once. Releases whose dispatch jitter has elapsed are
    /// folded into bounded miss history and skipped directly to the next legal grid point; no
    /// catch-up burst is emitted. In-flight or rejected slow groups are ignored so healthy groups
    /// remain selectable.
    ///
    /// # Errors
    ///
    /// Returns only for global dispatch-sequence or time arithmetic exhaustion.
    pub fn select(
        &mut self,
        now_monotonic_ns: u64,
    ) -> Result<Option<GroupReleaseTicket>, UpdateGroupError> {
        self.expire_in_flight(now_monotonic_ns);
        let mut selected: Option<(usize, SelectionKey)> = None;
        for index in 0..self.plan.groups.len() {
            let group = &self.plan.groups[index];
            let runtime = &mut self.runtimes[index];
            if runtime.health == GroupHealth::PerformanceRejected || runtime.in_flight.is_some() {
                continue;
            }
            let mut release = release_time(
                self.scheduler_start_ns,
                group.specification,
                runtime.next_release_ordinal,
            )?;
            let dispatch_deadline = release
                .checked_add(group.specification.maximum_jitter_ns())
                .ok_or(UpdateGroupError::ArithmeticOverflow)?;
            if now_monotonic_ns > dispatch_deadline {
                let late = now_monotonic_ns - dispatch_deadline;
                let skips = (late - 1) / group.specification.period_ns() + 1;
                runtime.next_release_ordinal = runtime
                    .next_release_ordinal
                    .checked_add(skips)
                    .ok_or(UpdateGroupError::ArithmeticOverflow)?;
                runtime.schedule_misses = runtime.schedule_misses.saturating_add(skips);
                record_misses(runtime, group.specification.miss_policy(), skips);
                runtime.last_quality = AggregateQuality::Stale;
                runtime.last_gap_reason = GapReason::NotSampled;
                if group.specification.descriptor().direction() == ImageDirection::Output {
                    runtime.output_refresh_status = OutputRefreshStatus::Missed;
                }
                if runtime.health == GroupHealth::PerformanceRejected {
                    continue;
                }
                release = release_time(
                    self.scheduler_start_ns,
                    group.specification,
                    runtime.next_release_ordinal,
                )?;
            }
            if release > now_monotonic_ns {
                continue;
            }
            let descriptor = group.specification.descriptor();
            let key = (
                release,
                group.specification.interface().get(),
                group.specification.phase_ns(),
                group.specification.priority(),
                descriptor.direction() as u8,
                descriptor.handle().get(),
            );
            if selected.as_ref().is_none_or(|(_, current)| key < *current) {
                selected = Some((index, key));
            }
        }
        let Some((index, _)) = selected else {
            return Ok(None);
        };
        self.issue_ticket(index)
    }

    fn issue_ticket(
        &mut self,
        group_index: usize,
    ) -> Result<Option<GroupReleaseTicket>, UpdateGroupError> {
        let group = &self.plan.groups[group_index];
        let runtime = &mut self.runtimes[group_index];
        let ordinal = runtime.next_release_ordinal;
        let release = release_time(self.scheduler_start_ns, group.specification, ordinal)?;
        let deadline = min(
            release
                .checked_add(group.specification.release_budget_ns())
                .ok_or(UpdateGroupError::ArithmeticOverflow)?,
            release
                .checked_add(group.specification.active_window_ns())
                .ok_or(UpdateGroupError::ArithmeticOverflow)?,
        );
        let dispatch_sequence = self.next_dispatch_sequence;
        self.next_dispatch_sequence = self
            .next_dispatch_sequence
            .checked_add(1)
            .ok_or(UpdateGroupError::ArithmeticOverflow)?;
        runtime.next_release_ordinal = runtime
            .next_release_ordinal
            .checked_add(1)
            .ok_or(UpdateGroupError::ArithmeticOverflow)?;
        runtime.release_count = runtime.release_count.saturating_add(1);
        runtime.in_flight = Some(InFlight {
            dispatch_sequence,
            release_ordinal: ordinal,
            deadline_monotonic_ns: deadline,
        });
        Ok(Some(GroupReleaseTicket {
            group_index: u16::try_from(group_index)
                .map_err(|_| UpdateGroupError::InvalidCapacity)?,
            dispatch_sequence,
            release_ordinal: ordinal,
            release_monotonic_ns: release,
            deadline_monotonic_ns: deadline,
            descriptor: group.specification.descriptor(),
            lease_identity: self.plan.region.lease_identity(),
            lease_sequence: self.plan.region.lease_sequence(),
            capability_digest: self.plan.region.capability_digest(),
        }))
    }

    fn expire_in_flight(&mut self, now_monotonic_ns: u64) {
        for (group, runtime) in self.plan.groups.iter().zip(self.runtimes.iter_mut()) {
            let Some(in_flight) = runtime.in_flight else {
                continue;
            };
            if now_monotonic_ns <= in_flight.deadline_monotonic_ns {
                continue;
            }
            runtime.in_flight = None;
            runtime.last_expired = Some(in_flight);
            runtime.timeout_count = runtime.timeout_count.saturating_add(1);
            runtime.last_gap_reason = GapReason::Timeout;
            let outcome_unknown = group.specification.descriptor().direction()
                == ImageDirection::Output
                && self.plan.operations[group.operations.clone()]
                    .iter()
                    .any(|operation| {
                        matches!(
                            operation.class(),
                            OperationClass::NonIdempotent | OperationClass::PulseOrEdge
                        )
                    });
            if outcome_unknown {
                runtime.outcome_unknown_count = runtime.outcome_unknown_count.saturating_add(1);
                runtime.last_quality = AggregateQuality::Uncertain;
                runtime.output_refresh_status = OutputRefreshStatus::OutcomeUnknown;
            } else {
                runtime.last_quality = AggregateQuality::Bad;
            }
            if group.specification.descriptor().direction() == ImageDirection::Output
                && !outcome_unknown
            {
                runtime.output_refresh_status = OutputRefreshStatus::Missed;
            }
            record_misses(runtime, group.specification.miss_policy(), 1);
        }
    }

    /// Consumes one current ticket with an exact operation observation set.
    ///
    /// `observations` must contain one entry for every planned operation, in dense handle order;
    /// an unexecuted entry is explicit `NotAttempted` with zero attempts. This prevents omitted or
    /// duplicated work from looking complete. The method never retries operations itself.
    ///
    /// # Errors
    ///
    /// Rejects stale tickets, missing/extra/reordered observations, illegal retries, capacity
    /// excess, timestamp drift, or inconsistent queue evidence without consuming the ticket.
    pub fn complete(
        &mut self,
        ticket: GroupReleaseTicket,
        completed_monotonic_ns: u64,
        sample_monotonic_ns: Option<u64>,
        observations: &[OperationObservation],
        queue: GroupQueueSnapshot,
    ) -> Result<GroupReleaseOutcome, UpdateGroupError> {
        let index = usize::from(ticket.group_index);
        let Some(group) = self.plan.groups.get(index) else {
            return Err(UpdateGroupError::StaleOrForeignTicket);
        };
        if ticket.lease_identity != self.plan.region.lease_identity()
            || ticket.lease_sequence != self.plan.region.lease_sequence()
            || ticket.capability_digest != self.plan.region.capability_digest()
        {
            return Err(UpdateGroupError::StaleOrForeignTicket);
        }
        if completed_monotonic_ns < ticket.release_monotonic_ns {
            return Err(UpdateGroupError::InvalidObservation);
        }
        let runtime = &mut self.runtimes[index];
        if runtime.last_expired.is_some_and(|expired| {
            expired.dispatch_sequence == ticket.dispatch_sequence
                && expired.release_ordinal == ticket.release_ordinal
                && expired.deadline_monotonic_ns == ticket.deadline_monotonic_ns
                && ticket.descriptor == group.specification.descriptor()
        }) {
            runtime.last_expired = None;
            runtime.late_response_count = runtime.late_response_count.saturating_add(1);
            return Err(UpdateGroupError::StaleOrForeignTicket);
        }
        let Some(in_flight) = runtime.in_flight else {
            return Err(UpdateGroupError::StaleOrForeignTicket);
        };
        if in_flight.dispatch_sequence != ticket.dispatch_sequence
            || in_flight.release_ordinal != ticket.release_ordinal
            || in_flight.deadline_monotonic_ns != ticket.deadline_monotonic_ns
            || ticket.descriptor != group.specification.descriptor()
        {
            return Err(UpdateGroupError::StaleOrForeignTicket);
        }
        validate_queue_snapshot(group.specification, runtime, queue)?;
        let operation_slice = &self.plan.operations[group.operations.clone()];
        let mut summary =
            validate_observations(group.specification, operation_slice, observations)?;
        let queue_full_delta = queue
            .dropped_newest
            .saturating_sub(runtime.dropped_newest)
            .saturating_add(
                queue
                    .rejected_newest
                    .saturating_sub(runtime.rejected_newest),
            );
        if queue_full_delta != 0 {
            summary.all_success = false;
            summary.queue_full = summary.queue_full.saturating_add(queue_full_delta);
            if summary.first_gap.is_none() {
                summary.first_gap = Some(GapReason::QueueFull);
            }
        }
        let (sample_time, input_age) = validate_sample(
            group.specification,
            ticket.release_monotonic_ns,
            completed_monotonic_ns,
            sample_monotonic_ns,
            summary.all_success,
        )?;
        let late = completed_monotonic_ns > ticket.deadline_monotonic_ns;

        runtime.in_flight = None;
        apply_queue_snapshot(runtime, queue);
        runtime.timeout_count = runtime.timeout_count.saturating_add(summary.timeouts);
        runtime.checksum_count = runtime.checksum_count.saturating_add(summary.checksums);
        runtime.working_counter_count = runtime
            .working_counter_count
            .saturating_add(summary.working_counters);
        runtime.error_frame_count = runtime
            .error_frame_count
            .saturating_add(summary.error_frames);
        runtime.queue_full_count = runtime.queue_full_count.saturating_add(summary.queue_full);
        if late {
            runtime.late_response_count = runtime.late_response_count.saturating_add(1);
        }
        runtime.last_sample_monotonic_ns = sample_time;
        runtime.last_input_age_ns = input_age;

        let outcome = derive_outcome(group.specification, runtime, summary, late, input_age);
        if outcome == GroupReleaseOutcome::Completed {
            runtime.successful_releases = runtime.successful_releases.saturating_add(1);
            runtime.last_quality = AggregateQuality::Good;
            runtime.last_gap_reason = GapReason::None;
            if group.specification.descriptor().direction() == ImageDirection::Output {
                runtime.output_refresh_status = OutputRefreshStatus::Refreshed;
            }
            record_success(runtime, group.specification.miss_policy());
        } else {
            record_misses(runtime, group.specification.miss_policy(), 1);
        }
        Ok(outcome)
    }

    /// Returns diagnostics for one exact direction-local group.
    #[must_use]
    pub fn diagnostics(
        &self,
        direction: ImageDirection,
        handle: GroupHandle,
    ) -> Option<UpdateGroupDiagnostics> {
        self.plan
            .groups
            .iter()
            .zip(self.runtimes.iter())
            .find(|(group, _)| {
                group.specification.descriptor().direction() == direction
                    && group.specification.descriptor().handle() == handle
            })
            .map(|(group, runtime)| snapshot(group.specification, runtime))
    }
}

#[derive(Debug, Clone, Copy)]
struct ObservationSummary {
    all_success: bool,
    first_gap: Option<GapReason>,
    outcome_unknown: bool,
    budget_exceeded: bool,
    timeouts: u64,
    checksums: u64,
    working_counters: u64,
    error_frames: u64,
    queue_full: u64,
}

fn validate_observations(
    specification: UpdateGroupSpec,
    operations: &[ScheduledOperation],
    observations: &[OperationObservation],
) -> Result<ObservationSummary, UpdateGroupError> {
    if observations.len() != operations.len() {
        return Err(UpdateGroupError::InvalidObservation);
    }
    let mut total_attempts = 0_u32;
    let mut total_units = 0_u32;
    let mut summary = ObservationSummary {
        all_success: true,
        first_gap: None,
        outcome_unknown: false,
        budget_exceeded: false,
        timeouts: 0,
        checksums: 0,
        working_counters: 0,
        error_frames: 0,
        queue_full: 0,
    };
    for (operation, observation) in operations.iter().zip(observations) {
        if operation.handle() != observation.handle {
            return Err(UpdateGroupError::InvalidObservation);
        }
        let not_attempted = observation.result == OperationResult::NotAttempted;
        if (not_attempted && observation.attempts != 0)
            || (!not_attempted && observation.attempts == 0)
        {
            return Err(UpdateGroupError::InvalidObservation);
        }
        let permitted_attempts = if matches!(operation.class(), OperationClass::IdempotentSet) {
            specification
                .maximum_retries()
                .checked_add(1)
                .ok_or(UpdateGroupError::ArithmeticOverflow)?
        } else {
            1
        };
        if observation.attempts > permitted_attempts
            || (observation.attempts > 1
                && !matches!(operation.class(), OperationClass::IdempotentSet))
        {
            return Err(UpdateGroupError::RetryNotProven);
        }
        total_attempts = total_attempts
            .checked_add(u32::from(observation.attempts))
            .ok_or(UpdateGroupError::ArithmeticOverflow)?;
        total_units = total_units
            .checked_add(
                operation
                    .frame_or_request_units()
                    .checked_mul(u32::from(observation.attempts))
                    .ok_or(UpdateGroupError::ArithmeticOverflow)?,
            )
            .ok_or(UpdateGroupError::ArithmeticOverflow)?;
        apply_operation_result(&mut summary, operation.class(), observation.result);
    }
    if total_attempts > specification.maximum_operations_per_release()
        || total_units > specification.frame_or_request_capacity()
    {
        return Err(UpdateGroupError::BudgetExceeded);
    }
    Ok(summary)
}

fn apply_operation_result(
    summary: &mut ObservationSummary,
    class: OperationClass,
    result: OperationResult,
) {
    let gap = match result {
        OperationResult::Success => return,
        OperationResult::Timeout => {
            summary.timeouts = summary.timeouts.saturating_add(1);
            if matches!(
                class,
                OperationClass::NonIdempotent | OperationClass::PulseOrEdge
            ) {
                summary.outcome_unknown = true;
            }
            GapReason::Timeout
        }
        OperationResult::Checksum => {
            summary.checksums = summary.checksums.saturating_add(1);
            GapReason::Checksum
        }
        OperationResult::WorkingCounter => {
            summary.working_counters = summary.working_counters.saturating_add(1);
            GapReason::WorkingCounter
        }
        OperationResult::ErrorFrame => {
            summary.error_frames = summary.error_frames.saturating_add(1);
            GapReason::DeviceFault
        }
        OperationResult::QueueFull => {
            summary.queue_full = summary.queue_full.saturating_add(1);
            GapReason::QueueFull
        }
        OperationResult::LinkDown => GapReason::LinkDown,
        OperationResult::DeviceFault => GapReason::DeviceFault,
        OperationResult::NotAttempted => {
            summary.budget_exceeded = true;
            GapReason::NotSampled
        }
    };
    summary.all_success = false;
    if summary.first_gap.is_none() {
        summary.first_gap = Some(gap);
    }
}

fn validate_sample(
    specification: UpdateGroupSpec,
    release_monotonic_ns: u64,
    completed_monotonic_ns: u64,
    sample_monotonic_ns: Option<u64>,
    all_success: bool,
) -> Result<(Option<u64>, Option<u64>), UpdateGroupError> {
    match specification.descriptor().direction() {
        ImageDirection::Input => {
            if all_success && sample_monotonic_ns.is_none() {
                return Err(UpdateGroupError::InvalidObservation);
            }
            let age = sample_monotonic_ns
                .map(|sample| {
                    if sample < release_monotonic_ns {
                        return Err(UpdateGroupError::InvalidObservation);
                    }
                    completed_monotonic_ns
                        .checked_sub(sample)
                        .ok_or(UpdateGroupError::InvalidObservation)
                })
                .transpose()?;
            Ok((sample_monotonic_ns, age))
        }
        ImageDirection::Output => {
            if sample_monotonic_ns.is_some() {
                return Err(UpdateGroupError::InvalidObservation);
            }
            Ok((None, None))
        }
    }
}

fn derive_outcome(
    specification: UpdateGroupSpec,
    runtime: &mut GroupRuntime,
    summary: ObservationSummary,
    late: bool,
    input_age: Option<u64>,
) -> GroupReleaseOutcome {
    if summary.outcome_unknown {
        runtime.outcome_unknown_count = runtime.outcome_unknown_count.saturating_add(1);
        runtime.last_quality = AggregateQuality::Uncertain;
        runtime.last_gap_reason = GapReason::Timeout;
        runtime.output_refresh_status = OutputRefreshStatus::OutcomeUnknown;
        return GroupReleaseOutcome::OutcomeUnknown;
    }
    if late {
        runtime.last_quality = AggregateQuality::Bad;
        runtime.last_gap_reason = GapReason::Timeout;
        if specification.descriptor().direction() == ImageDirection::Output {
            runtime.output_refresh_status = OutputRefreshStatus::Missed;
        }
        return GroupReleaseOutcome::WindowMissed;
    }
    if summary.budget_exceeded {
        runtime.budget_exceeded_count = runtime.budget_exceeded_count.saturating_add(1);
        runtime.last_quality = AggregateQuality::Bad;
        runtime.last_gap_reason = GapReason::NotSampled;
        if specification.descriptor().direction() == ImageDirection::Output {
            runtime.output_refresh_status = OutputRefreshStatus::Missed;
        }
        return GroupReleaseOutcome::BudgetExceeded;
    }
    if let Some(gap) = summary.first_gap {
        runtime.last_quality = AggregateQuality::Bad;
        runtime.last_gap_reason = gap;
        if specification.descriptor().direction() == ImageDirection::Output {
            runtime.output_refresh_status = OutputRefreshStatus::Missed;
        }
        return GroupReleaseOutcome::Failed(gap);
    }
    if input_age.is_some_and(|age| age > specification.stale_after_ns()) {
        runtime.stale_input_count = runtime.stale_input_count.saturating_add(1);
        runtime.last_quality = AggregateQuality::Stale;
        runtime.last_gap_reason = GapReason::NotSampled;
        return GroupReleaseOutcome::StaleInput;
    }
    GroupReleaseOutcome::Completed
}

fn validate_queue_snapshot(
    specification: UpdateGroupSpec,
    runtime: &GroupRuntime,
    queue: GroupQueueSnapshot,
) -> Result<(), UpdateGroupError> {
    if queue.direction != specification.descriptor().direction()
        || queue.capacity != specification.queue_capacity()
        || queue.depth > queue.capacity
        || queue.high_water < queue.depth
        || queue.high_water > queue.capacity
        || queue.high_water < runtime.queue_high_water
        || queue.dropped_newest < runtime.dropped_newest
        || queue.rejected_newest < runtime.rejected_newest
        || (queue.direction == ImageDirection::Input && queue.rejected_newest != 0)
        || (queue.direction == ImageDirection::Output && queue.dropped_newest != 0)
    {
        return Err(UpdateGroupError::QueueSnapshotMismatch);
    }
    Ok(())
}

fn apply_queue_snapshot(runtime: &mut GroupRuntime, queue: GroupQueueSnapshot) {
    runtime.queue_depth = queue.depth;
    runtime.queue_high_water = runtime.queue_high_water.max(queue.high_water);
    runtime.dropped_newest = queue.dropped_newest;
    runtime.rejected_newest = queue.rejected_newest;
}

fn release_time(
    scheduler_start_ns: u64,
    specification: UpdateGroupSpec,
    ordinal: u64,
) -> Result<u64, UpdateGroupError> {
    scheduler_start_ns
        .checked_add(specification.phase_ns())
        .and_then(|first| {
            specification
                .period_ns()
                .checked_mul(ordinal)
                .and_then(|offset| first.checked_add(offset))
        })
        .ok_or(UpdateGroupError::ArithmeticOverflow)
}

fn record_success(runtime: &mut GroupRuntime, policy: GroupMissPolicy) {
    replace_history_slot(runtime, false);
    runtime.consecutive_misses = 0;
    if runtime.health != GroupHealth::PerformanceRejected {
        runtime.health = if runtime.rolling_misses == 0 {
            GroupHealth::Healthy
        } else {
            GroupHealth::Degraded
        };
    }
    apply_health_threshold(runtime, policy);
}

fn record_misses(runtime: &mut GroupRuntime, policy: GroupMissPolicy, count: u64) {
    if count == 0 {
        return;
    }
    runtime.total_misses = runtime.total_misses.saturating_add(count);
    runtime.consecutive_misses = runtime.consecutive_misses.saturating_add(count);
    let history_len = runtime.miss_history.len();
    let history_len_u64 = u64::try_from(history_len).unwrap_or(u64::MAX);
    if count >= history_len_u64 {
        runtime.miss_history.fill(true);
        runtime.rolling_misses = u16::try_from(history_len).unwrap_or(u16::MAX);
        let offset = usize::try_from(count % history_len_u64).unwrap_or(0);
        runtime.miss_cursor = (runtime.miss_cursor + offset) % history_len;
    } else {
        let bounded = usize::try_from(count).unwrap_or(history_len);
        for _ in 0..bounded {
            replace_history_slot(runtime, true);
        }
    }
    if runtime.health != GroupHealth::PerformanceRejected {
        runtime.health = GroupHealth::Degraded;
    }
    apply_health_threshold(runtime, policy);
}

fn replace_history_slot(runtime: &mut GroupRuntime, missed: bool) {
    let previous = runtime.miss_history[runtime.miss_cursor];
    if previous && !missed {
        runtime.rolling_misses = runtime.rolling_misses.saturating_sub(1);
    } else if !previous && missed {
        runtime.rolling_misses = runtime.rolling_misses.saturating_add(1);
    }
    runtime.miss_history[runtime.miss_cursor] = missed;
    runtime.miss_cursor = (runtime.miss_cursor + 1) % runtime.miss_history.len();
}

fn apply_health_threshold(runtime: &mut GroupRuntime, policy: GroupMissPolicy) {
    if runtime.rolling_misses > policy.maximum_misses()
        || runtime.consecutive_misses >= u64::from(policy.consecutive_misses())
    {
        runtime.health = GroupHealth::PerformanceRejected;
    }
}

fn snapshot(specification: UpdateGroupSpec, runtime: &GroupRuntime) -> UpdateGroupDiagnostics {
    UpdateGroupDiagnostics {
        descriptor: specification.descriptor(),
        health: runtime.health,
        next_release_ordinal: runtime.next_release_ordinal,
        release_count: runtime.release_count,
        successful_releases: runtime.successful_releases,
        schedule_misses: runtime.schedule_misses,
        total_misses: runtime.total_misses,
        rolling_misses: runtime.rolling_misses,
        consecutive_misses: runtime.consecutive_misses,
        timeout_count: runtime.timeout_count,
        checksum_count: runtime.checksum_count,
        working_counter_count: runtime.working_counter_count,
        error_frame_count: runtime.error_frame_count,
        queue_full_count: runtime.queue_full_count,
        budget_exceeded_count: runtime.budget_exceeded_count,
        stale_input_count: runtime.stale_input_count,
        outcome_unknown_count: runtime.outcome_unknown_count,
        late_response_count: runtime.late_response_count,
        queue_depth: runtime.queue_depth,
        queue_high_water: runtime.queue_high_water,
        dropped_newest: runtime.dropped_newest,
        rejected_newest: runtime.rejected_newest,
        last_sample_monotonic_ns: runtime.last_sample_monotonic_ns,
        last_input_age_ns: runtime.last_input_age_ns,
        last_quality: runtime.last_quality,
        last_gap_reason: runtime.last_gap_reason,
        output_refresh_status: runtime.output_refresh_status,
    }
}
