//! Backend-neutral Adapter operations and bounded cyclic buffers.

use aurora_io_guardian::{
    AggregateQuality, FallbackCause, FallbackDigest, GapReason, GroupDescriptor,
};

use crate::{DriverAuthority, DriverInstancePlan, DriverSdkError, EvidenceDigest};

/// Exact Driver Adapter operation directory frozen by Preview 1.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdapterOperation {
    /// Validate identity, configuration, layout, capability, and capacity without opening output.
    ValidateConfiguration,
    /// Exclusively claim the approved device/interface.
    Claim,
    /// Initialize device state, mapping, and Fallback.
    Initialize,
    /// Enter cyclic operation after protection and health admission.
    Activate,
    /// Execute one fixed update group.
    Exchange,
    /// Perform a bounded amount of non-cyclic mailbox work.
    MailboxStep,
    /// Idempotently enter the active Fallback action.
    EnterFallback,
    /// Reinitialize under a new lease without replaying prior writes.
    Recover,
    /// Stop submission and prove the interface can be released for a switch.
    QuiesceForSwitch,
    /// Stop and release all device ownership.
    Release,
}

/// Observable normalized lifecycle state shared by static and isolated implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DriverLifecycleState {
    /// Constructed but not validated.
    Cold,
    /// Immutable configuration and budgets passed validation.
    Validated,
    /// The approved physical interface is exclusively claimed.
    Claimed,
    /// Device and Fallback initialization completed.
    Initialized,
    /// Cyclic exchange is eligible under the current authority.
    Active,
    /// Active Fallback owns outputs.
    Fallback,
    /// Device is stopped and ready for explicit release/switch.
    Quiesced,
    /// Device resources have been released.
    Released,
    /// The implementation crashed, blocked, or violated its contract.
    Faulted,
}

/// Common exact-authority request for non-cyclic lifecycle operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LifecycleRequest {
    authority: DriverAuthority,
    now_ns: u64,
    deadline_ns: u64,
}

impl LifecycleRequest {
    /// Creates a request with a strict absolute deadline.
    ///
    /// # Errors
    ///
    /// Rejects `now >= deadline`.
    pub const fn new(
        authority: DriverAuthority,
        now_ns: u64,
        deadline_ns: u64,
    ) -> Result<Self, DriverSdkError> {
        if now_ns >= deadline_ns {
            Err(DriverSdkError::DeadlineExceeded)
        } else {
            Ok(Self {
                authority,
                now_ns,
                deadline_ns,
            })
        }
    }

    /// Returns the exact instance/lease/configuration authority.
    #[must_use]
    pub const fn authority(self) -> DriverAuthority {
        self.authority
    }

    /// Returns the Guardian monotonic observation in nanoseconds.
    #[must_use]
    pub const fn now_ns(self) -> u64 {
        self.now_ns
    }

    /// Returns the strict absolute deadline in nanoseconds.
    #[must_use]
    pub const fn deadline_ns(self) -> u64 {
        self.deadline_ns
    }
}

/// Borrowed request for one exact group exchange.
pub struct ExchangeRequest<'a> {
    authority: DriverAuthority,
    group: GroupDescriptor,
    sequence: u64,
    now_ns: u64,
    deadline_ns: u64,
    output: &'a [u8],
}

impl<'a> ExchangeRequest<'a> {
    /// Creates a bounded exchange request.
    ///
    /// # Errors
    ///
    /// Rejects sequence zero or an already reached deadline.
    pub const fn new(
        authority: DriverAuthority,
        group: GroupDescriptor,
        sequence: u64,
        now_ns: u64,
        deadline_ns: u64,
        output: &'a [u8],
    ) -> Result<Self, DriverSdkError> {
        if sequence == 0 {
            return Err(DriverSdkError::InvalidCapacity);
        }
        if now_ns >= deadline_ns {
            return Err(DriverSdkError::DeadlineExceeded);
        }
        Ok(Self {
            authority,
            group,
            sequence,
            now_ns,
            deadline_ns,
            output,
        })
    }

    /// Returns the exact authority.
    #[must_use]
    pub const fn authority(&self) -> DriverAuthority {
        self.authority
    }

    /// Returns the exact mapped group.
    #[must_use]
    pub const fn group(&self) -> GroupDescriptor {
        self.group
    }

    /// Returns the release-scoped sequence.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the observation time.
    #[must_use]
    pub const fn now_ns(&self) -> u64 {
        self.now_ns
    }

    /// Returns the strict deadline.
    #[must_use]
    pub const fn deadline_ns(&self) -> u64 {
        self.deadline_ns
    }

    /// Returns the caller-owned output payload.
    #[must_use]
    pub const fn output(&self) -> &'a [u8] {
        self.output
    }
}

/// Caller-owned input staging; implementations cannot retain its pointer.
pub struct ExchangeBuffer<'a> {
    input: &'a mut [u8],
}

impl<'a> ExchangeBuffer<'a> {
    /// Wraps a fixed input staging slice.
    #[must_use]
    pub const fn new(input: &'a mut [u8]) -> Self {
        Self { input }
    }

    /// Returns the fixed mutable input staging slice.
    #[must_use]
    pub fn input(&mut self) -> &mut [u8] {
        self.input
    }
}

/// Immutable cancellation observation for one mailbox operation generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MailboxCancellation {
    generation: u64,
    cancelled: bool,
}

impl MailboxCancellation {
    /// Creates one explicit cancellation observation.
    ///
    /// # Errors
    ///
    /// Rejects generation zero so a missing token cannot look active.
    pub const fn new(generation: u64, cancelled: bool) -> Result<Self, DriverSdkError> {
        if generation == 0 {
            Err(DriverSdkError::InvalidCapacity)
        } else {
            Ok(Self {
                generation,
                cancelled,
            })
        }
    }

    /// Returns the operation-scoped cancellation generation.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }

    /// Returns whether the caller cancelled before this bounded step.
    #[must_use]
    pub const fn is_cancelled(self) -> bool {
        self.cancelled
    }
}

/// Bounded non-cyclic mailbox request with explicit caller-owned retry state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MailboxRequest {
    lifecycle: LifecycleRequest,
    maximum_work_items: u32,
    attempt: u16,
    maximum_attempts: u16,
    cancellation: MailboxCancellation,
}

impl MailboxRequest {
    /// Creates one bounded mailbox step.
    ///
    /// # Errors
    ///
    /// Rejects zero work/attempts or an attempt beyond the declared bound. The Adapter executes
    /// only this step; it never starts an implicit retry loop.
    pub const fn new(
        lifecycle: LifecycleRequest,
        maximum_work_items: u32,
        attempt: u16,
        maximum_attempts: u16,
        cancellation: MailboxCancellation,
    ) -> Result<Self, DriverSdkError> {
        if maximum_work_items == 0
            || attempt == 0
            || maximum_attempts == 0
            || attempt > maximum_attempts
        {
            Err(DriverSdkError::InvalidCapacity)
        } else {
            Ok(Self {
                lifecycle,
                maximum_work_items,
                attempt,
                maximum_attempts,
                cancellation,
            })
        }
    }

    /// Returns the common authority and deadline.
    #[must_use]
    pub const fn lifecycle(self) -> LifecycleRequest {
        self.lifecycle
    }

    /// Returns the maximum work performed by this call.
    #[must_use]
    pub const fn maximum_work_items(self) -> u32 {
        self.maximum_work_items
    }

    /// Returns the one-based caller-owned attempt number.
    #[must_use]
    pub const fn attempt(self) -> u16 {
        self.attempt
    }

    /// Returns the fixed total attempt bound, including the first attempt.
    #[must_use]
    pub const fn maximum_attempts(self) -> u16 {
        self.maximum_attempts
    }

    /// Returns the immutable cancellation observation for this step.
    #[must_use]
    pub const fn cancellation(self) -> MailboxCancellation {
        self.cancellation
    }
}

/// Normalized result of a lifecycle or mailbox operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AdapterReport {
    /// Operation that completed.
    pub operation: AdapterOperation,
    /// State after the operation.
    pub state: DriverLifecycleState,
    /// Monotonic completion timestamp.
    pub completed_at_ns: u64,
    /// Bounded work items consumed.
    pub work_items: u32,
}

/// Normalized exchange result with explicit quality and gap semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExchangeReport {
    /// Exact group that was processed.
    pub group: GroupDescriptor,
    /// Release-scoped sequence.
    pub sequence: u64,
    /// Physical/sample completion time.
    pub sampled_at_ns: u64,
    /// Aggregate quality after validation.
    pub quality: AggregateQuality,
    /// Explicit reason when quality is not Good.
    pub gap: GapReason,
    /// Bounded work items consumed.
    pub work_items: u32,
}

/// Compile-time Adapter contract; no runtime plugin or backend-native type crosses this boundary.
pub trait DriverAdapter {
    /// Returns the current normalized lifecycle state.
    fn state(&self) -> DriverLifecycleState;

    /// Validates the immutable signed configuration without opening outputs.
    ///
    /// # Errors
    ///
    /// Rejects authority, deadline, lifecycle, capacity, or normalized driver failures.
    fn validate_configuration(
        &mut self,
        request: LifecycleRequest,
    ) -> Result<AdapterReport, DriverSdkError>;

    /// Exclusively claims the approved interface before its deadline.
    ///
    /// # Errors
    ///
    /// Rejects authority, deadline, ownership, lifecycle, or normalized driver failures.
    fn claim(&mut self, request: LifecycleRequest) -> Result<AdapterReport, DriverSdkError>;

    /// Initializes device state, mapping, and active Fallback.
    ///
    /// # Errors
    ///
    /// Rejects authority, deadline, lifecycle, protection, or normalized driver failures.
    fn initialize(
        &mut self,
        request: LifecycleRequest,
        fallback: FallbackDigest,
    ) -> Result<AdapterReport, DriverSdkError>;

    /// Enters cyclic operation only after Fallback/protection health admission.
    ///
    /// # Errors
    ///
    /// Rejects authority, deadline, lifecycle, protection, or normalized driver failures.
    fn activate(
        &mut self,
        request: LifecycleRequest,
        fallback: FallbackDigest,
    ) -> Result<AdapterReport, DriverSdkError>;

    /// Performs one fixed group exchange without allocation or retained pointers.
    ///
    /// # Errors
    ///
    /// Rejects foreign groups, buffers, sequences, deadlines, budgets, or driver faults.
    fn exchange(
        &mut self,
        request: ExchangeRequest<'_>,
        input: &mut ExchangeBuffer<'_>,
    ) -> Result<ExchangeReport, DriverSdkError>;

    /// Performs at most the requested non-cyclic work.
    ///
    /// # Errors
    ///
    /// Rejects authority, deadline, lifecycle, work-budget, or normalized driver failures.
    fn mailbox_step(&mut self, request: MailboxRequest) -> Result<AdapterReport, DriverSdkError>;

    /// Idempotently applies the active Fallback action.
    ///
    /// # Errors
    ///
    /// Rejects authority, deadline, lifecycle, protection, or normalized driver failures.
    fn enter_fallback(
        &mut self,
        request: LifecycleRequest,
        cause: FallbackCause,
    ) -> Result<AdapterReport, DriverSdkError>;

    /// Reinitializes under a new authority; callers must not replay earlier writes.
    ///
    /// # Errors
    ///
    /// Rejects reused/skipped identities, invalid lifecycle, deadline, or driver failures.
    fn recover(
        &mut self,
        request: LifecycleRequest,
        replacement: DriverInstancePlan,
        fallback: FallbackDigest,
        evidence: EvidenceDigest,
    ) -> Result<AdapterReport, DriverSdkError>;

    /// Stops submission and proves the interface is ready to release.
    ///
    /// # Errors
    ///
    /// Rejects authority, deadline, lifecycle, release, or normalized driver failures.
    fn quiesce_for_switch(
        &mut self,
        request: LifecycleRequest,
    ) -> Result<AdapterReport, DriverSdkError>;

    /// Releases device ownership while retaining Fallback semantics.
    ///
    /// # Errors
    ///
    /// Rejects authority, deadline, lifecycle, release, or normalized driver failures.
    fn release(&mut self, request: LifecycleRequest) -> Result<AdapterReport, DriverSdkError>;
}
