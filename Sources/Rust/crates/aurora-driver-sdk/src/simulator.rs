//! Deterministic fixed-seed loopback Driver Adapter and golden trace.

use crate::{
    AdapterOperation, AdapterReport, DriverAdapter, DriverAuthority, DriverFaultKind,
    DriverInstancePlan, DriverLifecycleState, DriverSdkError, EvidenceDigest, ExchangeBuffer,
    ExchangeReport, ExchangeRequest, LifecycleRequest, MailboxRequest,
};
use aurora_io_guardian::{
    AggregateQuality, FallbackCause, FallbackDigest, GapReason, ImageDirection,
};

/// Deterministic failure injected at one exact Adapter operation step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SimulationFault {
    /// Simulate physical disconnect.
    Disconnect,
    /// Simulate a stale/reordered response.
    Reorder,
    /// Simulate CRC/integrity failure.
    Crc,
    /// Simulate `EtherCAT` working-counter failure.
    WorkingCounter,
    /// Simulate CAN bus-off.
    BusOff,
    /// Simulate fixed queue exhaustion.
    QueueFull,
    /// Simulate malformed frame/message input.
    MalformedFrame,
    /// Simulate Driver Adapter version mismatch.
    ProtocolVersion,
    /// Simulate an implementation crash.
    Crash,
    /// Simulate a blocking driver detected by its supervisor deadline.
    Blocking,
    /// Simulate an ordinary operation timeout.
    Timeout,
}

impl SimulationFault {
    const fn kind(self) -> DriverFaultKind {
        match self {
            Self::Disconnect => DriverFaultKind::Disconnected,
            Self::Reorder => DriverFaultKind::Reordered,
            Self::Crc => DriverFaultKind::CrcFailure,
            Self::WorkingCounter => DriverFaultKind::WorkingCounterFailure,
            Self::BusOff => DriverFaultKind::BusOff,
            Self::QueueFull => DriverFaultKind::QueueFull,
            Self::MalformedFrame => DriverFaultKind::MalformedFrame,
            Self::ProtocolVersion => DriverFaultKind::ProtocolVersionMismatch,
            Self::Crash => DriverFaultKind::Crashed,
            Self::Blocking => DriverFaultKind::BlockingDetected,
            Self::Timeout => DriverFaultKind::DeadlineExceeded,
        }
    }
}

/// One fault scheduled at an exact one-based Adapter call step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FaultInjection {
    step: u64,
    operation: AdapterOperation,
    fault: SimulationFault,
}

impl FaultInjection {
    /// Creates one deterministic injection.
    ///
    /// # Errors
    ///
    /// Rejects step zero.
    pub const fn new(
        step: u64,
        operation: AdapterOperation,
        fault: SimulationFault,
    ) -> Result<Self, DriverSdkError> {
        if step == 0 {
            Err(DriverSdkError::InvalidCapacity)
        } else {
            Ok(Self {
                step,
                operation,
                fault,
            })
        }
    }
}

/// Stable trace result encoded without backend-native error values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TraceResult {
    /// Operation completed successfully.
    Success,
    /// Operation returned one normalized driver fault.
    Fault(DriverFaultKind),
}

/// Fixed-layout logical golden trace record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GoldenTraceRecord {
    /// One-based Adapter call step.
    pub step: u64,
    /// Normalized operation.
    pub operation: AdapterOperation,
    /// State before the call.
    pub state_before: DriverLifecycleState,
    /// State after the call.
    pub state_after: DriverLifecycleState,
    /// Normalized result.
    pub result: TraceResult,
    /// Guardian monotonic observation.
    pub monotonic_ns: u64,
    /// Exchange sequence, or zero for non-exchange calls.
    pub exchange_sequence: u64,
    /// Stable checksum of input/output bytes observed by the simulator.
    pub payload_checksum: u64,
}

impl GoldenTraceRecord {
    /// Encodes one platform-independent 48-byte golden record.
    #[must_use]
    pub fn encode(self) -> [u8; 48] {
        let mut bytes = [0_u8; 48];
        bytes[0..8].copy_from_slice(&self.step.to_le_bytes());
        bytes[8] = operation_code(self.operation);
        bytes[9] = state_code(self.state_before);
        bytes[10] = state_code(self.state_after);
        let (result, fault) = match self.result {
            TraceResult::Success => (0, 0),
            TraceResult::Fault(kind) => (1, fault_code(kind)),
        };
        bytes[11] = result;
        bytes[12] = fault;
        bytes[16..24].copy_from_slice(&self.monotonic_ns.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.exchange_sequence.to_le_bytes());
        bytes[32..40].copy_from_slice(&self.payload_checksum.to_le_bytes());
        bytes
    }
}

/// Fixed-capacity simulator implementing the same Adapter trait as future real backends.
pub struct DeterministicSimulator {
    plan: DriverInstancePlan,
    state: DriverLifecycleState,
    rng_state: u64,
    faults: Box<[FaultInjection]>,
    fault_cursor: usize,
    trace: Vec<GoldenTraceRecord>,
    trace_capacity: usize,
    loopback: Box<[u8]>,
    loopback_length: usize,
    group_sequences: Box<[u64]>,
    step: u64,
    last_monotonic_ns: Option<u64>,
    active_fallback: Option<FallbackDigest>,
    reserved_memory_bytes: u64,
}

impl DeterministicSimulator {
    /// Allocates all simulator storage before lifecycle execution.
    ///
    /// # Errors
    ///
    /// Rejects zero seed/capacity, non-canonical fault steps, or allocation failure.
    pub fn new(
        plan: DriverInstancePlan,
        seed: u64,
        faults: &[FaultInjection],
        trace_capacity: u32,
    ) -> Result<Self, DriverSdkError> {
        if seed == 0
            || trace_capacity == 0
            || faults.windows(2).any(|pair| pair[0].step >= pair[1].step)
        {
            return Err(DriverSdkError::CatalogMismatch);
        }
        let fault_bytes = faults
            .len()
            .checked_mul(core::mem::size_of::<FaultInjection>())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(DriverSdkError::ArithmeticOverflow)?;
        let reserved_memory_bytes = plan
            .limits()
            .minimum_reserved_bytes()?
            .checked_add(fault_bytes)
            .ok_or(DriverSdkError::ArithmeticOverflow)?;
        if reserved_memory_bytes > plan.limits().maximum_memory_bytes {
            return Err(DriverSdkError::InvalidCapacity);
        }
        let trace_capacity =
            usize::try_from(trace_capacity).map_err(|_| DriverSdkError::InvalidCapacity)?;
        if trace_capacity
            > usize::try_from(plan.limits().diagnostic_capacity)
                .map_err(|_| DriverSdkError::InvalidCapacity)?
        {
            return Err(DriverSdkError::InvalidCapacity);
        }
        let loopback_capacity = usize::try_from(plan.limits().maximum_frame_bytes)
            .map_err(|_| DriverSdkError::InvalidCapacity)?;
        let mut loopback = Vec::new();
        loopback
            .try_reserve_exact(loopback_capacity)
            .map_err(|_| DriverSdkError::AllocationFailed)?;
        loopback.resize(loopback_capacity, 0);
        let mut sequences = Vec::new();
        sequences
            .try_reserve_exact(plan.groups().len())
            .map_err(|_| DriverSdkError::AllocationFailed)?;
        sequences.resize(plan.groups().len(), 0);
        let mut trace = Vec::new();
        trace
            .try_reserve_exact(trace_capacity)
            .map_err(|_| DriverSdkError::AllocationFailed)?;
        Ok(Self {
            plan,
            state: DriverLifecycleState::Cold,
            rng_state: seed,
            faults: crate::model::copy_boxed(faults)?,
            fault_cursor: 0,
            trace,
            trace_capacity,
            loopback: loopback.into_boxed_slice(),
            loopback_length: 0,
            group_sequences: sequences.into_boxed_slice(),
            step: 0,
            last_monotonic_ns: None,
            active_fallback: None,
            reserved_memory_bytes,
        })
    }

    /// Returns the complete bounded golden trace prefix.
    #[must_use]
    pub fn trace(&self) -> &[GoldenTraceRecord] {
        &self.trace
    }

    /// Returns whether every scheduled fault was consumed.
    #[must_use]
    pub fn fault_plan_complete(&self) -> bool {
        self.fault_cursor == self.faults.len()
    }

    fn prepare(
        &mut self,
        operation: AdapterOperation,
        authority: DriverAuthority,
        now_ns: u64,
        deadline_ns: u64,
        sequence: u64,
        payload_checksum: u64,
    ) -> Result<DriverLifecycleState, DriverSdkError> {
        self.plan.validate_authority(authority)?;
        if now_ns >= deadline_ns {
            return Err(DriverSdkError::DeadlineExceeded);
        }
        if self.last_monotonic_ns.is_some_and(|last| now_ns < last) {
            return Err(DriverSdkError::MonotonicTimeRegression);
        }
        if self.trace.len() == self.trace_capacity {
            self.state = fault_state(DriverFaultKind::QueueFull, self.active_fallback.is_some());
            return Err(DriverSdkError::DriverFault(DriverFaultKind::QueueFull));
        }
        let step = self
            .step
            .checked_add(1)
            .ok_or(DriverSdkError::ArithmeticOverflow)?;
        let before = self.state;
        if let Some(injection) = self.faults.get(self.fault_cursor).copied()
            && injection.step == step
        {
            if injection.operation != operation {
                return Err(DriverSdkError::CatalogMismatch);
            }
            self.fault_cursor += 1;
            self.step = step;
            self.last_monotonic_ns = Some(now_ns);
            let kind = injection.fault.kind();
            self.state = fault_state(kind, self.active_fallback.is_some());
            self.trace.push(GoldenTraceRecord {
                step,
                operation,
                state_before: before,
                state_after: self.state,
                result: TraceResult::Fault(kind),
                monotonic_ns: now_ns,
                exchange_sequence: sequence,
                payload_checksum,
            });
            return Err(DriverSdkError::DriverFault(kind));
        }
        if self
            .faults
            .get(self.fault_cursor)
            .is_some_and(|injection| injection.step < step)
        {
            return Err(DriverSdkError::CatalogMismatch);
        }
        self.step = step;
        self.last_monotonic_ns = Some(now_ns);
        Ok(before)
    }

    fn finish(
        &mut self,
        operation: AdapterOperation,
        before: DriverLifecycleState,
        now_ns: u64,
        sequence: u64,
        payload_checksum: u64,
        work_items: u32,
    ) -> AdapterReport {
        self.trace.push(GoldenTraceRecord {
            step: self.step,
            operation,
            state_before: before,
            state_after: self.state,
            result: TraceResult::Success,
            monotonic_ns: now_ns,
            exchange_sequence: sequence,
            payload_checksum,
        });
        AdapterReport {
            operation,
            state: self.state,
            completed_at_ns: now_ns,
            work_items,
        }
    }

    fn fail_after_prepare(
        &mut self,
        operation: AdapterOperation,
        before: DriverLifecycleState,
        now_ns: u64,
        sequence: u64,
        payload_checksum: u64,
        kind: DriverFaultKind,
    ) -> DriverSdkError {
        self.state = fault_state(kind, self.active_fallback.is_some());
        self.trace.push(GoldenTraceRecord {
            step: self.step,
            operation,
            state_before: before,
            state_after: self.state,
            result: TraceResult::Fault(kind),
            monotonic_ns: now_ns,
            exchange_sequence: sequence,
            payload_checksum,
        });
        DriverSdkError::DriverFault(kind)
    }

    fn lifecycle(
        &mut self,
        operation: AdapterOperation,
        request: LifecycleRequest,
        allowed: &[DriverLifecycleState],
        next: DriverLifecycleState,
    ) -> Result<AdapterReport, DriverSdkError> {
        if !allowed.contains(&self.state) {
            return Err(DriverSdkError::InvalidStateTransition);
        }
        let before = self.prepare(
            operation,
            request.authority(),
            request.now_ns(),
            request.deadline_ns(),
            0,
            0,
        )?;
        self.state = next;
        Ok(self.finish(operation, before, request.now_ns(), 0, 0, 1))
    }

    fn next_random(&mut self) -> u64 {
        self.rng_state = self.rng_state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.rng_state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

impl DriverAdapter for DeterministicSimulator {
    fn state(&self) -> DriverLifecycleState {
        self.state
    }

    fn validate_configuration(
        &mut self,
        request: LifecycleRequest,
    ) -> Result<AdapterReport, DriverSdkError> {
        self.lifecycle(
            AdapterOperation::ValidateConfiguration,
            request,
            &[DriverLifecycleState::Cold],
            DriverLifecycleState::Validated,
        )
    }

    fn claim(&mut self, request: LifecycleRequest) -> Result<AdapterReport, DriverSdkError> {
        self.lifecycle(
            AdapterOperation::Claim,
            request,
            &[DriverLifecycleState::Validated],
            DriverLifecycleState::Claimed,
        )
    }

    fn initialize(
        &mut self,
        request: LifecycleRequest,
        fallback: FallbackDigest,
    ) -> Result<AdapterReport, DriverSdkError> {
        let report = self.lifecycle(
            AdapterOperation::Initialize,
            request,
            &[DriverLifecycleState::Claimed],
            DriverLifecycleState::Initialized,
        )?;
        self.active_fallback = Some(fallback);
        Ok(report)
    }

    fn activate(
        &mut self,
        request: LifecycleRequest,
        fallback: FallbackDigest,
    ) -> Result<AdapterReport, DriverSdkError> {
        if self.active_fallback != Some(fallback) {
            return Err(DriverSdkError::AuthorityMismatch);
        }
        let report = self.lifecycle(
            AdapterOperation::Activate,
            request,
            &[DriverLifecycleState::Initialized],
            DriverLifecycleState::Active,
        )?;
        self.active_fallback = Some(fallback);
        Ok(report)
    }

    fn exchange(
        &mut self,
        request: ExchangeRequest<'_>,
        input: &mut ExchangeBuffer<'_>,
    ) -> Result<ExchangeReport, DriverSdkError> {
        if self.state != DriverLifecycleState::Active {
            return Err(DriverSdkError::InvalidStateTransition);
        }
        let binding = self
            .plan
            .group(request.group())
            .ok_or(DriverSdkError::UnauthorizedResource)?;
        let expected_bytes = usize::try_from(binding.payload_bytes())
            .map_err(|_| DriverSdkError::InvalidCapacity)?;
        let output_checksum = checksum(request.output());
        let group_index = self
            .plan
            .groups()
            .iter()
            .position(|candidate| candidate.descriptor() == request.group())
            .ok_or(DriverSdkError::UnauthorizedResource)?;
        let expected_sequence = self.group_sequences[group_index]
            .checked_add(1)
            .ok_or(DriverSdkError::ArithmeticOverflow)?;
        let before = self.prepare(
            AdapterOperation::Exchange,
            request.authority(),
            request.now_ns(),
            request.deadline_ns(),
            request.sequence(),
            output_checksum,
        )?;
        let valid_buffers = match request.group().direction() {
            ImageDirection::Input => {
                request.output().is_empty() && input.input().len() == expected_bytes
            }
            ImageDirection::Output => {
                request.output().len() == expected_bytes && input.input().is_empty()
            }
        };
        if !valid_buffers {
            return Err(self.fail_after_prepare(
                AdapterOperation::Exchange,
                before,
                request.now_ns(),
                request.sequence(),
                output_checksum,
                DriverFaultKind::MalformedFrame,
            ));
        }
        if request.sequence() != expected_sequence {
            return Err(self.fail_after_prepare(
                AdapterOperation::Exchange,
                before,
                request.now_ns(),
                request.sequence(),
                output_checksum,
                DriverFaultKind::Reordered,
            ));
        }
        self.group_sequences[group_index] = request.sequence();
        match request.group().direction() {
            ImageDirection::Input => {
                for (index, value) in input.input().iter_mut().enumerate() {
                    let loopback = if self.loopback_length == 0 {
                        0
                    } else {
                        self.loopback[index % self.loopback_length]
                    };
                    *value = loopback ^ self.next_random().to_le_bytes()[0];
                }
            }
            ImageDirection::Output => {
                self.loopback[..expected_bytes].copy_from_slice(request.output());
                self.loopback_length = expected_bytes;
            }
        }
        let final_checksum = if request.group().direction() == ImageDirection::Input {
            checksum(input.input())
        } else {
            output_checksum
        };
        let _report = self.finish(
            AdapterOperation::Exchange,
            before,
            request.now_ns(),
            request.sequence(),
            final_checksum,
            binding.maximum_work_items(),
        );
        Ok(ExchangeReport {
            group: request.group(),
            sequence: request.sequence(),
            sampled_at_ns: request.now_ns(),
            quality: AggregateQuality::Good,
            gap: GapReason::None,
            work_items: binding.maximum_work_items(),
        })
    }

    fn mailbox_step(&mut self, request: MailboxRequest) -> Result<AdapterReport, DriverSdkError> {
        if request.cancellation().is_cancelled() {
            return Err(DriverSdkError::OperationCancelled);
        }
        if request.maximum_work_items() > self.plan.limits().maximum_mailbox_work_items
            || request.maximum_attempts() > self.plan.limits().maximum_mailbox_attempts
        {
            return Err(DriverSdkError::InvalidCapacity);
        }
        self.lifecycle(
            AdapterOperation::MailboxStep,
            request.lifecycle(),
            &[
                DriverLifecycleState::Initialized,
                DriverLifecycleState::Active,
                DriverLifecycleState::Fallback,
            ],
            self.state,
        )
        .map(|mut report| {
            report.work_items = request.maximum_work_items();
            report
        })
    }

    fn enter_fallback(
        &mut self,
        request: LifecycleRequest,
        _cause: FallbackCause,
    ) -> Result<AdapterReport, DriverSdkError> {
        if self.active_fallback.is_none() {
            return Err(DriverSdkError::InvalidStateTransition);
        }
        self.lifecycle(
            AdapterOperation::EnterFallback,
            request,
            &[
                DriverLifecycleState::Initialized,
                DriverLifecycleState::Active,
                DriverLifecycleState::Fallback,
            ],
            DriverLifecycleState::Fallback,
        )
    }

    fn recover(
        &mut self,
        request: LifecycleRequest,
        replacement: DriverInstancePlan,
        fallback: FallbackDigest,
        _evidence: EvidenceDigest,
    ) -> Result<AdapterReport, DriverSdkError> {
        if self.state != DriverLifecycleState::Fallback {
            return Err(DriverSdkError::InvalidStateTransition);
        }
        let current = self.plan.authority();
        let candidate = replacement.authority();
        let current_configuration = current.lease_identity().configuration();
        let candidate_configuration = candidate.lease_identity().configuration();
        if candidate.instance() != current.instance()
            || candidate.package() != current.package()
            || candidate.interface() != current.interface()
            || candidate_configuration.epoch() != current_configuration.epoch()
            || candidate_configuration.generation()
                != current_configuration
                    .generation()
                    .checked_next()
                    .map_err(|_| DriverSdkError::ArithmeticOverflow)?
            || candidate.lease_identity().lease_id() == current.lease_identity().lease_id()
            || candidate.lease_sequence()
                != current
                    .lease_sequence()
                    .checked_next()
                    .map_err(|_| DriverSdkError::ArithmeticOverflow)?
            || replacement.groups().len() > self.group_sequences.len()
            || usize::try_from(replacement.limits().diagnostic_capacity)
                .map_or(true, |capacity| capacity < self.trace_capacity)
            || usize::try_from(replacement.limits().maximum_frame_bytes)
                .map_or(true, |capacity| capacity > self.loopback.len())
            || replacement.limits().maximum_memory_bytes < self.reserved_memory_bytes
        {
            return Err(DriverSdkError::AuthorityMismatch);
        }
        let before = self.prepare(
            AdapterOperation::Recover,
            request.authority(),
            request.now_ns(),
            request.deadline_ns(),
            0,
            0,
        )?;
        self.plan = replacement;
        self.group_sequences.fill(0);
        self.loopback.fill(0);
        self.loopback_length = 0;
        self.active_fallback = Some(fallback);
        self.state = DriverLifecycleState::Initialized;
        Ok(self.finish(AdapterOperation::Recover, before, request.now_ns(), 0, 0, 1))
    }

    fn quiesce_for_switch(
        &mut self,
        request: LifecycleRequest,
    ) -> Result<AdapterReport, DriverSdkError> {
        self.lifecycle(
            AdapterOperation::QuiesceForSwitch,
            request,
            &[DriverLifecycleState::Active, DriverLifecycleState::Fallback],
            DriverLifecycleState::Quiesced,
        )
    }

    fn release(&mut self, request: LifecycleRequest) -> Result<AdapterReport, DriverSdkError> {
        self.lifecycle(
            AdapterOperation::Release,
            request,
            &[
                DriverLifecycleState::Initialized,
                DriverLifecycleState::Fallback,
                DriverLifecycleState::Quiesced,
            ],
            DriverLifecycleState::Released,
        )
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in bytes {
        hash ^= u64::from(*value);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

const fn fault_state(kind: DriverFaultKind, fallback_armed: bool) -> DriverLifecycleState {
    if !fallback_armed {
        return DriverLifecycleState::Faulted;
    }
    match kind {
        DriverFaultKind::Crashed
        | DriverFaultKind::BlockingDetected
        | DriverFaultKind::ProtocolVersionMismatch => DriverLifecycleState::Faulted,
        DriverFaultKind::DeadlineExceeded
        | DriverFaultKind::QueueFull
        | DriverFaultKind::MalformedFrame
        | DriverFaultKind::Disconnected
        | DriverFaultKind::Reordered
        | DriverFaultKind::CrcFailure
        | DriverFaultKind::WorkingCounterFailure
        | DriverFaultKind::BusOff => DriverLifecycleState::Fallback,
    }
}

const fn operation_code(operation: AdapterOperation) -> u8 {
    match operation {
        AdapterOperation::ValidateConfiguration => 1,
        AdapterOperation::Claim => 2,
        AdapterOperation::Initialize => 3,
        AdapterOperation::Activate => 4,
        AdapterOperation::Exchange => 5,
        AdapterOperation::MailboxStep => 6,
        AdapterOperation::EnterFallback => 7,
        AdapterOperation::Recover => 8,
        AdapterOperation::QuiesceForSwitch => 9,
        AdapterOperation::Release => 10,
    }
}

const fn state_code(state: DriverLifecycleState) -> u8 {
    match state {
        DriverLifecycleState::Cold => 0,
        DriverLifecycleState::Validated => 1,
        DriverLifecycleState::Claimed => 2,
        DriverLifecycleState::Initialized => 3,
        DriverLifecycleState::Active => 4,
        DriverLifecycleState::Fallback => 5,
        DriverLifecycleState::Quiesced => 6,
        DriverLifecycleState::Released => 7,
        DriverLifecycleState::Faulted => 8,
    }
}

const fn fault_code(kind: DriverFaultKind) -> u8 {
    match kind {
        DriverFaultKind::Crashed => 1,
        DriverFaultKind::BlockingDetected => 2,
        DriverFaultKind::DeadlineExceeded => 3,
        DriverFaultKind::QueueFull => 4,
        DriverFaultKind::MalformedFrame => 5,
        DriverFaultKind::ProtocolVersionMismatch => 6,
        DriverFaultKind::Disconnected => 7,
        DriverFaultKind::Reordered => 8,
        DriverFaultKind::CrcFailure => 9,
        DriverFaultKind::WorkingCounterFailure => 10,
        DriverFaultKind::BusOff => 11,
    }
}
