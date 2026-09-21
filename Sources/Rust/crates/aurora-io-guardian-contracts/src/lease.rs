//! Fixed-capacity Guardian lease, heartbeat, freshness, and reconfiguration state.

use crate::{
    GuardianConfiguration, GuardianContractError, ImageSequence, LeaseId, LeaseIdentity,
    LeaseSequence, NegotiatedContract,
};

/// Observable Guardian lifecycle states covered by R3-01.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GuardianState {
    /// No device or active Fallback has been established.
    Cold,
    /// Active Fallback owns all affected output domains.
    Fallback,
    /// A validated lease awaits activation after the health gate.
    LeasePending,
    /// One current lease may publish output images.
    Running,
    /// The old configuration is quiesced while a new generation is validated.
    Reinitializing,
}

/// Fixed heartbeat and per-output-group freshness policy in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeasePolicy<const OUTPUT_GROUPS: usize> {
    heartbeat_interval: u64,
    heartbeat_timeout: u64,
    output_group_max_age: [u64; OUTPUT_GROUPS],
}

impl<const OUTPUT_GROUPS: usize> LeasePolicy<OUTPUT_GROUPS> {
    /// Creates a policy with one non-zero age for every statically declared output group.
    ///
    /// # Errors
    ///
    /// Rejects a zero group count, zero timing, or a heartbeat timeout shorter than its interval.
    pub fn new(
        heartbeat_interval_ns: u64,
        heartbeat_timeout_ns: u64,
        output_group_max_age_ns: [u64; OUTPUT_GROUPS],
    ) -> Result<Self, GuardianContractError> {
        if OUTPUT_GROUPS == 0
            || heartbeat_interval_ns == 0
            || heartbeat_timeout_ns < heartbeat_interval_ns
            || output_group_max_age_ns.contains(&0)
        {
            return Err(GuardianContractError::InvalidLeaseTiming);
        }
        Ok(Self {
            heartbeat_interval: heartbeat_interval_ns,
            heartbeat_timeout: heartbeat_timeout_ns,
            output_group_max_age: output_group_max_age_ns,
        })
    }

    /// Returns the declared heartbeat interval in nanoseconds.
    #[must_use]
    pub const fn heartbeat_interval_ns(&self) -> u64 {
        self.heartbeat_interval
    }

    /// Returns the heartbeat expiry timeout in nanoseconds.
    #[must_use]
    pub const fn heartbeat_timeout_ns(&self) -> u64 {
        self.heartbeat_timeout
    }

    /// Returns the exact per-output-group freshness ages in nanoseconds.
    #[must_use]
    pub const fn output_group_max_age_ns(&self) -> &[u64; OUTPUT_GROUPS] {
        &self.output_group_max_age
    }
}

/// Complete request for one new lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeaseRequest<const OUTPUT_GROUPS: usize> {
    identity: LeaseIdentity,
    negotiated_contract: NegotiatedContract,
    policy: LeasePolicy<OUTPUT_GROUPS>,
}

impl<const OUTPUT_GROUPS: usize> LeaseRequest<OUTPUT_GROUPS> {
    /// Creates a request already bound to configuration and negotiated contract identities.
    #[must_use]
    pub const fn new(
        identity: LeaseIdentity,
        negotiated_contract: NegotiatedContract,
        policy: LeasePolicy<OUTPUT_GROUPS>,
    ) -> Self {
        Self {
            identity,
            negotiated_contract,
            policy,
        }
    }

    /// Returns the requested lease identity.
    #[must_use]
    pub const fn identity(&self) -> LeaseIdentity {
        self.identity
    }

    /// Returns the previously negotiated contract identity.
    #[must_use]
    pub const fn negotiated_contract(&self) -> NegotiatedContract {
        self.negotiated_contract
    }

    /// Returns the fixed timing policy.
    #[must_use]
    pub const fn policy(&self) -> &LeasePolicy<OUTPUT_GROUPS> {
        &self.policy
    }
}

/// Identity carried by one complete output image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputImageIdentity {
    lease_identity: LeaseIdentity,
    image_sequence: ImageSequence,
}

impl OutputImageIdentity {
    /// Creates an output identity.
    #[must_use]
    pub const fn new(lease_identity: LeaseIdentity, image_sequence: ImageSequence) -> Self {
        Self {
            lease_identity,
            image_sequence,
        }
    }

    /// Returns the lease identity that owns the image.
    #[must_use]
    pub const fn lease_identity(self) -> LeaseIdentity {
        self.lease_identity
    }

    /// Returns the strictly increasing image sequence.
    #[must_use]
    pub const fn image_sequence(self) -> ImageSequence {
        self.image_sequence
    }
}

/// One monotonic deadline observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeadlineObservation<const OUTPUT_GROUPS: usize> {
    heartbeat_expired: bool,
    expired_output_groups: [bool; OUTPUT_GROUPS],
}

impl<const OUTPUT_GROUPS: usize> DeadlineObservation<OUTPUT_GROUPS> {
    /// Returns whether the global heartbeat expired and revoked the lease.
    #[must_use]
    pub const fn heartbeat_expired(&self) -> bool {
        self.heartbeat_expired
    }

    /// Returns the exact group expiry bitmap for this observation.
    #[must_use]
    pub const fn expired_output_groups(&self) -> &[bool; OUTPUT_GROUPS] {
        &self.expired_output_groups
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LeaseSession<const OUTPUT_GROUPS: usize> {
    request: LeaseRequest<OUTPUT_GROUPS>,
    lease_sequence: LeaseSequence,
    heartbeat_deadline_ns: u64,
    output_group_deadline_ns: [u64; OUTPUT_GROUPS],
    stale_output_groups: [bool; OUTPUT_GROUPS],
    last_image_sequence: Option<ImageSequence>,
}

/// Fixed-capacity R3-01 state machine for one Guardian process epoch.
///
/// `OUTPUT_GROUPS` is the exact configured group count. `LEASE_CAPACITY` is the maximum number of
/// unique lease identifiers retained for the process lifetime; exhaustion rejects a new lease
/// instead of allocating or forgetting replay history.
#[derive(Debug, PartialEq, Eq)]
pub struct GuardianLeaseMachine<const OUTPUT_GROUPS: usize, const LEASE_CAPACITY: usize> {
    state: GuardianState,
    configuration: GuardianConfiguration,
    negotiated_contract: NegotiatedContract,
    lease_policy: LeasePolicy<OUTPUT_GROUPS>,
    fallback_armed: bool,
    pending_configuration: Option<GuardianConfiguration>,
    pending_negotiated_contract: Option<NegotiatedContract>,
    pending_lease_policy: Option<LeasePolicy<OUTPUT_GROUPS>>,
    pending_lease: Option<LeaseSession<OUTPUT_GROUPS>>,
    active_lease: Option<LeaseSession<OUTPUT_GROUPS>>,
    lease_history: [Option<LeaseId>; LEASE_CAPACITY],
    lease_history_len: usize,
    last_lease_sequence: Option<LeaseSequence>,
    last_monotonic_ns: Option<u64>,
}

impl<const OUTPUT_GROUPS: usize, const LEASE_CAPACITY: usize>
    GuardianLeaseMachine<OUTPUT_GROUPS, LEASE_CAPACITY>
{
    /// Creates a cold state machine with no device or Fallback authorization.
    ///
    /// # Errors
    ///
    /// Rejects zero output-group or lease-history capacity.
    pub const fn new(
        configuration: GuardianConfiguration,
        negotiated_contract: NegotiatedContract,
        lease_policy: LeasePolicy<OUTPUT_GROUPS>,
    ) -> Result<Self, GuardianContractError> {
        if OUTPUT_GROUPS == 0 || LEASE_CAPACITY == 0 {
            return Err(GuardianContractError::InvalidLeaseTiming);
        }
        Ok(Self {
            state: GuardianState::Cold,
            configuration,
            negotiated_contract,
            lease_policy,
            fallback_armed: false,
            pending_configuration: None,
            pending_negotiated_contract: None,
            pending_lease_policy: None,
            pending_lease: None,
            active_lease: None,
            lease_history: [None; LEASE_CAPACITY],
            lease_history_len: 0,
            last_lease_sequence: None,
            last_monotonic_ns: None,
        })
    }

    /// Returns the current state.
    #[must_use]
    pub const fn state(&self) -> GuardianState {
        self.state
    }

    /// Returns the immutable active configuration.
    #[must_use]
    pub const fn configuration(&self) -> GuardianConfiguration {
        self.configuration
    }

    /// Returns the immutable active lease timing policy.
    #[must_use]
    pub const fn lease_policy(&self) -> &LeasePolicy<OUTPUT_GROUPS> {
        &self.lease_policy
    }

    /// Returns the active lease identity while Running.
    #[must_use]
    pub const fn active_lease_identity(&self) -> Option<LeaseIdentity> {
        match self.active_lease {
            Some(session) => Some(session.request.identity),
            None => None,
        }
    }

    /// Returns the active lease sequence while Running.
    #[must_use]
    pub const fn active_lease_sequence(&self) -> Option<LeaseSequence> {
        match self.active_lease {
            Some(session) => Some(session.lease_sequence),
            None => None,
        }
    }

    /// Returns whether a declared output group is latched stale for the active lease.
    ///
    /// # Errors
    ///
    /// Rejects an out-of-range group or an absent active lease.
    pub fn output_group_is_stale(&self, group_index: usize) -> Result<bool, GuardianContractError> {
        if group_index >= OUTPUT_GROUPS {
            return Err(GuardianContractError::OutputGroupOutOfRange);
        }
        match self.active_lease {
            Some(session) => Ok(session.stale_output_groups[group_index]),
            None => Err(GuardianContractError::StaleOrForeignLease),
        }
    }

    /// Establishes or re-establishes active Fallback after the external device-health gate.
    ///
    /// # Errors
    ///
    /// Only Cold or Fallback may be armed.
    pub const fn arm_fallback(&mut self) -> Result<(), GuardianContractError> {
        if !matches!(self.state, GuardianState::Cold | GuardianState::Fallback) {
            return Err(GuardianContractError::InvalidStateTransition);
        }
        self.state = GuardianState::Fallback;
        self.fallback_armed = true;
        Ok(())
    }

    /// Validates and records a new lease without activating output.
    ///
    /// The operation is atomic on rejection: no sequence, lease history, or deadline changes.
    ///
    /// # Errors
    ///
    /// Rejects a non-Fallback state, unarmed Fallback, stale configuration or negotiation,
    /// reused `LeaseId`, exhausted fixed history, regressed time, or deadline overflow.
    pub fn request_lease(
        &mut self,
        request: LeaseRequest<OUTPUT_GROUPS>,
        now_ns: u64,
    ) -> Result<LeaseSequence, GuardianContractError> {
        if self.state != GuardianState::Fallback {
            return Err(GuardianContractError::InvalidStateTransition);
        }
        if !self.fallback_armed {
            return Err(GuardianContractError::FallbackNotArmed);
        }
        if request.identity.configuration() != self.configuration
            || request.negotiated_contract != self.negotiated_contract
            || request.policy != self.lease_policy
        {
            return Err(GuardianContractError::ConfigurationMismatch);
        }
        self.validate_monotonic(now_ns)?;
        if self.lease_history[..self.lease_history_len].contains(&Some(request.identity.lease_id()))
        {
            return Err(GuardianContractError::LeaseIdReused);
        }
        if self.lease_history_len == LEASE_CAPACITY {
            return Err(GuardianContractError::LeaseCapacityExceeded);
        }
        let lease_sequence = match self.last_lease_sequence {
            Some(value) => value.checked_next()?,
            None => LeaseSequence::new(1)?,
        };
        let heartbeat_deadline_ns = now_ns
            .checked_add(request.policy.heartbeat_timeout)
            .ok_or(GuardianContractError::CounterOverflow)?;
        let mut output_group_deadline_ns = [0; OUTPUT_GROUPS];
        let mut index = 0;
        while index < OUTPUT_GROUPS {
            output_group_deadline_ns[index] = now_ns
                .checked_add(request.policy.output_group_max_age[index])
                .ok_or(GuardianContractError::CounterOverflow)?;
            index += 1;
        }
        let session = LeaseSession {
            request,
            lease_sequence,
            heartbeat_deadline_ns,
            output_group_deadline_ns,
            stale_output_groups: [false; OUTPUT_GROUPS],
            last_image_sequence: None,
        };
        self.lease_history[self.lease_history_len] = Some(request.identity.lease_id());
        self.lease_history_len += 1;
        self.last_lease_sequence = Some(lease_sequence);
        self.last_monotonic_ns = Some(now_ns);
        self.pending_lease = Some(session);
        self.fallback_armed = false;
        self.state = GuardianState::LeasePending;
        Ok(lease_sequence)
    }

    /// Activates the pending lease after the external health window has completed.
    ///
    /// # Errors
    ///
    /// Rejects every state except `LeasePending`.
    pub fn activate_pending(&mut self) -> Result<(), GuardianContractError> {
        if self.state != GuardianState::LeasePending {
            return Err(GuardianContractError::InvalidStateTransition);
        }
        let Some(session) = self.pending_lease.take() else {
            return Err(GuardianContractError::InvalidStateTransition);
        };
        self.active_lease = Some(session);
        self.state = GuardianState::Running;
        Ok(())
    }

    /// Revokes a pending or active lease and returns to unarmed Fallback.
    ///
    /// # Errors
    ///
    /// Rejects states that have no lease to revoke.
    pub const fn revoke_lease(&mut self) -> Result<(), GuardianContractError> {
        if !matches!(
            self.state,
            GuardianState::LeasePending | GuardianState::Running
        ) {
            return Err(GuardianContractError::InvalidStateTransition);
        }
        self.pending_lease = None;
        self.active_lease = None;
        self.fallback_armed = false;
        self.state = GuardianState::Fallback;
        Ok(())
    }

    /// Records an independent Control heartbeat for the exact active lease.
    ///
    /// Heartbeats extend only the heartbeat deadline and never group freshness.
    ///
    /// # Errors
    ///
    /// Rejects a foreign lease, time regression, deadline overflow, or an observation at/after
    /// the current deadline. Expiry revokes the lease and enters unarmed Fallback.
    pub fn record_heartbeat(
        &mut self,
        identity: LeaseIdentity,
        now_ns: u64,
    ) -> Result<(), GuardianContractError> {
        self.validate_running_identity(identity)?;
        self.validate_monotonic(now_ns)?;
        let session = self
            .active_lease
            .as_ref()
            .ok_or(GuardianContractError::StaleOrForeignLease)?;
        if now_ns >= session.heartbeat_deadline_ns {
            self.expire_heartbeat(now_ns);
            return Err(GuardianContractError::HeartbeatExpired);
        }
        let new_deadline = now_ns
            .checked_add(session.request.policy.heartbeat_timeout)
            .ok_or(GuardianContractError::CounterOverflow)?;
        let session = self
            .active_lease
            .as_mut()
            .ok_or(GuardianContractError::StaleOrForeignLease)?;
        session.heartbeat_deadline_ns = new_deadline;
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Accepts one exact-next complete output image and refreshes only its declared groups.
    ///
    /// Output publication never extends the Control heartbeat. A group that has reached its
    /// deadline or was previously latched stale cannot be revived inside the same lease.
    /// Rejections leave image sequence and group deadlines unchanged.
    ///
    /// # Errors
    ///
    /// Rejects a foreign/expired lease, duplicate/skipped sequence, empty update bitmap,
    /// stale group, regressed time, or deadline overflow.
    pub fn accept_output_image(
        &mut self,
        image: OutputImageIdentity,
        updated_groups: [bool; OUTPUT_GROUPS],
        now_ns: u64,
    ) -> Result<(), GuardianContractError> {
        self.validate_running_identity(image.lease_identity)?;
        self.validate_monotonic(now_ns)?;
        if !updated_groups.contains(&true) {
            return Err(GuardianContractError::NoOutputGroupUpdated);
        }
        let session = self
            .active_lease
            .as_ref()
            .ok_or(GuardianContractError::StaleOrForeignLease)?;
        if now_ns >= session.heartbeat_deadline_ns {
            self.expire_heartbeat(now_ns);
            return Err(GuardianContractError::HeartbeatExpired);
        }
        let expected_sequence = match session.last_image_sequence {
            Some(value) => value.checked_next()?,
            None => ImageSequence::new(1)?,
        };
        if image.image_sequence != expected_sequence {
            return Err(GuardianContractError::ImageSequenceViolation);
        }
        let mut new_deadlines = session.output_group_deadline_ns;
        let mut new_stale_groups = session.stale_output_groups;
        let mut index = 0;
        while index < OUTPUT_GROUPS {
            if updated_groups[index] {
                if session.stale_output_groups[index]
                    || now_ns >= session.output_group_deadline_ns[index]
                {
                    return Err(GuardianContractError::OutputGroupExpired);
                }
                new_deadlines[index] = now_ns
                    .checked_add(session.request.policy.output_group_max_age[index])
                    .ok_or(GuardianContractError::CounterOverflow)?;
            } else if now_ns >= session.output_group_deadline_ns[index] {
                new_stale_groups[index] = true;
            }
            index += 1;
        }
        let session = self
            .active_lease
            .as_mut()
            .ok_or(GuardianContractError::StaleOrForeignLease)?;
        session.last_image_sequence = Some(expected_sequence);
        session.output_group_deadline_ns = new_deadlines;
        session.stale_output_groups = new_stale_groups;
        self.last_monotonic_ns = Some(now_ns);
        Ok(())
    }

    /// Applies one monotonic deadline observation.
    ///
    /// Reaching a heartbeat deadline revokes the whole lease. Reaching only group deadlines
    /// latches those groups stale while the lease and other groups remain Running.
    ///
    /// # Errors
    ///
    /// Rejects an absent active lease or regressed time.
    pub fn check_deadlines(
        &mut self,
        now_ns: u64,
    ) -> Result<DeadlineObservation<OUTPUT_GROUPS>, GuardianContractError> {
        self.validate_monotonic(now_ns)?;
        let session = self
            .active_lease
            .as_ref()
            .ok_or(GuardianContractError::StaleOrForeignLease)?;
        let heartbeat_expired = now_ns >= session.heartbeat_deadline_ns;
        let mut expired_output_groups = [false; OUTPUT_GROUPS];
        let mut index = 0;
        while index < OUTPUT_GROUPS {
            expired_output_groups[index] = !session.stale_output_groups[index]
                && now_ns >= session.output_group_deadline_ns[index];
            index += 1;
        }
        if heartbeat_expired {
            self.expire_heartbeat(now_ns);
        } else {
            let session = self
                .active_lease
                .as_mut()
                .ok_or(GuardianContractError::StaleOrForeignLease)?;
            let mut group = 0;
            while group < OUTPUT_GROUPS {
                session.stale_output_groups[group] |= expired_output_groups[group];
                group += 1;
            }
            self.last_monotonic_ns = Some(now_ns);
        }
        Ok(DeadlineObservation {
            heartbeat_expired,
            expired_output_groups,
        })
    }

    /// Begins an exact-next immutable configuration generation from Fallback.
    ///
    /// The current process epoch cannot change inside this machine; a new epoch requires a new
    /// machine. Pending configuration never becomes active until completion.
    ///
    /// # Errors
    ///
    /// Rejects non-Fallback state, a different epoch, or skipped/repeated generation.
    pub fn begin_reconfiguration(
        &mut self,
        pending: GuardianConfiguration,
        pending_negotiated_contract: NegotiatedContract,
        pending_lease_policy: LeasePolicy<OUTPUT_GROUPS>,
    ) -> Result<(), GuardianContractError> {
        if self.state != GuardianState::Fallback {
            return Err(GuardianContractError::InvalidStateTransition);
        }
        let expected = self.configuration.generation().checked_next()?;
        if pending.epoch() != self.configuration.epoch() || pending.generation() != expected {
            return Err(GuardianContractError::ConfigurationGenerationViolation);
        }
        self.pending_configuration = Some(pending);
        self.pending_negotiated_contract = Some(pending_negotiated_contract);
        self.pending_lease_policy = Some(pending_lease_policy);
        self.fallback_armed = false;
        self.state = GuardianState::Reinitializing;
        Ok(())
    }

    /// Publishes the fully validated pending configuration and returns to unarmed Fallback.
    ///
    /// A separate health/Fallback arm and a new lease are required before Running.
    ///
    /// # Errors
    ///
    /// Rejects every state except Reinitializing or a missing pending configuration.
    pub fn complete_reconfiguration(&mut self) -> Result<(), GuardianContractError> {
        if self.state != GuardianState::Reinitializing {
            return Err(GuardianContractError::InvalidStateTransition);
        }
        let (Some(configuration), Some(negotiated_contract), Some(lease_policy)) = (
            self.pending_configuration,
            self.pending_negotiated_contract,
            self.pending_lease_policy,
        ) else {
            return Err(GuardianContractError::InvalidStateTransition);
        };
        self.pending_configuration = None;
        self.pending_negotiated_contract = None;
        self.pending_lease_policy = None;
        self.configuration = configuration;
        self.negotiated_contract = negotiated_contract;
        self.lease_policy = lease_policy;
        self.state = GuardianState::Fallback;
        self.fallback_armed = false;
        Ok(())
    }

    /// Discards a pending configuration and returns the old generation to unarmed Fallback.
    ///
    /// # Errors
    ///
    /// Rejects every state except Reinitializing.
    pub fn abort_reconfiguration(&mut self) -> Result<(), GuardianContractError> {
        if self.state != GuardianState::Reinitializing {
            return Err(GuardianContractError::InvalidStateTransition);
        }
        self.pending_configuration = None;
        self.pending_negotiated_contract = None;
        self.pending_lease_policy = None;
        self.state = GuardianState::Fallback;
        self.fallback_armed = false;
        Ok(())
    }

    fn validate_running_identity(
        &self,
        identity: LeaseIdentity,
    ) -> Result<(), GuardianContractError> {
        if self.state != GuardianState::Running {
            return Err(GuardianContractError::StaleOrForeignLease);
        }
        match self.active_lease {
            Some(session) if session.request.identity == identity => Ok(()),
            Some(_) | None => Err(GuardianContractError::StaleOrForeignLease),
        }
    }

    fn validate_monotonic(&self, now_ns: u64) -> Result<(), GuardianContractError> {
        if self.last_monotonic_ns.is_some_and(|last| now_ns < last) {
            Err(GuardianContractError::MonotonicTimeRegression)
        } else {
            Ok(())
        }
    }

    fn expire_heartbeat(&mut self, now_ns: u64) {
        self.active_lease = None;
        self.pending_lease = None;
        self.state = GuardianState::Fallback;
        self.fallback_armed = false;
        self.last_monotonic_ns = Some(now_ns);
    }
}
