//! Latched Fault and Fallback request contracts.

use aurora_types::{BootEpochId, LocalHandle, MonotonicTimestamp};

use crate::{
    CommitSequence, ExecutionContractError, ExecutionContractVersion, FallbackRequestSequence,
    FaultGeneration, FaultReason, ReleaseSequence, TaskEpoch,
};

/// Identity of the fixed output set owned by a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputSetIdentity([u8; 32]);

impl OutputSetIdentity {
    /// Creates an identity from a canonical SHA-256 digest.
    #[must_use]
    pub const fn from_sha256(value: [u8; 32]) -> Self {
        Self(value)
    }

    /// Returns the raw SHA-256 bytes.
    #[must_use]
    pub const fn to_sha256(self) -> [u8; 32] {
        self.0
    }
}

/// An idempotent, non-overwritable request for task-owned output Fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallbackRequest {
    version: ExecutionContractVersion,
    engine_epoch: BootEpochId,
    task_handle: LocalHandle,
    task_epoch: TaskEpoch,
    fault_generation: FaultGeneration,
    request_sequence: FallbackRequestSequence,
    reason: FaultReason,
    output_set: OutputSetIdentity,
    release_sequence: ReleaseSequence,
    commit_sequence: CommitSequence,
    occurred_at: MonotonicTimestamp,
}

impl FallbackRequest {
    /// Creates a request whose monotonic timestamp belongs to `engine_epoch`.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::EpochMismatch`] when the timestamp is
    /// from another engine epoch.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version: ExecutionContractVersion,
        engine_epoch: BootEpochId,
        task_handle: LocalHandle,
        task_epoch: TaskEpoch,
        fault_generation: FaultGeneration,
        request_sequence: FallbackRequestSequence,
        reason: FaultReason,
        output_set: OutputSetIdentity,
        release_sequence: ReleaseSequence,
        commit_sequence: CommitSequence,
        occurred_at: MonotonicTimestamp,
    ) -> Result<Self, ExecutionContractError> {
        if occurred_at.boot_epoch().to_bytes() != engine_epoch.to_bytes() {
            return Err(ExecutionContractError::EpochMismatch);
        }
        Ok(Self {
            version,
            engine_epoch,
            task_handle,
            task_epoch,
            fault_generation,
            request_sequence,
            reason,
            output_set,
            release_sequence,
            commit_sequence,
            occurred_at,
        })
    }

    /// Returns the contract version.
    #[must_use]
    pub const fn version(self) -> ExecutionContractVersion {
        self.version
    }

    /// Returns the engine epoch.
    #[must_use]
    pub const fn engine_epoch(self) -> BootEpochId {
        self.engine_epoch
    }

    /// Returns the task handle.
    #[must_use]
    pub const fn task_handle(self) -> LocalHandle {
        self.task_handle
    }

    /// Returns the task initialization epoch.
    #[must_use]
    pub const fn task_epoch(self) -> TaskEpoch {
        self.task_epoch
    }

    /// Returns the latched Fault generation.
    #[must_use]
    pub const fn fault_generation(self) -> FaultGeneration {
        self.fault_generation
    }

    /// Returns the idempotency sequence.
    #[must_use]
    pub const fn request_sequence(self) -> FallbackRequestSequence {
        self.request_sequence
    }

    /// Returns the exhaustive Fault reason.
    #[must_use]
    pub const fn reason(self) -> FaultReason {
        self.reason
    }

    /// Returns the task-owned output-set identity.
    #[must_use]
    pub const fn output_set(self) -> OutputSetIdentity {
        self.output_set
    }

    /// Returns the Fault release sequence.
    #[must_use]
    pub const fn release_sequence(self) -> ReleaseSequence {
        self.release_sequence
    }

    /// Returns the last complete commit sequence.
    #[must_use]
    pub const fn commit_sequence(self) -> CommitSequence {
        self.commit_sequence
    }

    /// Returns the monotonic occurrence time.
    #[must_use]
    pub const fn occurred_at(self) -> MonotonicTimestamp {
        self.occurred_at
    }

    /// Returns whether an acknowledgement identifies this exact request.
    #[must_use]
    pub fn matches_ack(
        self,
        engine_epoch: BootEpochId,
        task_epoch: TaskEpoch,
        request_sequence: FallbackRequestSequence,
    ) -> bool {
        self.engine_epoch.to_bytes()[..] == engine_epoch.to_bytes()[..]
            && self.task_epoch.get() == task_epoch.get()
            && self.request_sequence.get() == request_sequence.get()
    }
}

#[cfg(test)]
mod tests {
    use aurora_types::{BootEpochId, LocalHandle, MonotonicTimestamp};

    use super::{FallbackRequest, OutputSetIdentity};
    use crate::{
        CommitSequence, ExecutionContractError, ExecutionContractVersion, FallbackRequestSequence,
        FaultGeneration, FaultReason, ReleaseSequence, TaskEpoch,
    };

    #[test]
    fn fallback_request_validates_epoch_and_ack_identity() {
        let epoch = test_epoch(0x98);
        let other_epoch = test_epoch(0x99);
        let task_epoch = TaskEpoch::new(3);
        let generation = FaultGeneration::new(4);
        assert!(epoch.is_ok());
        assert!(other_epoch.is_ok());
        assert!(task_epoch.is_ok());
        assert!(generation.is_ok());
        if let (Ok(epoch), Ok(other_epoch), Ok(task_epoch), Ok(generation)) =
            (epoch, other_epoch, task_epoch, generation)
        {
            let request = new_request(epoch, task_epoch, generation, epoch);
            assert!(request.is_ok());
            if let Ok(request) = request {
                assert!(request.matches_ack(epoch, task_epoch, FallbackRequestSequence::new(5)));
                assert!(!request.matches_ack(
                    other_epoch,
                    task_epoch,
                    FallbackRequestSequence::new(5)
                ));
                let other_task_epoch = TaskEpoch::new(4);
                if let Ok(other_task_epoch) = other_task_epoch {
                    assert!(!request.matches_ack(
                        epoch,
                        other_task_epoch,
                        FallbackRequestSequence::new(5)
                    ));
                }
                assert!(!request.matches_ack(epoch, task_epoch, FallbackRequestSequence::new(6)));
                assert_eq!(request.reason(), FaultReason::HardLimitExceeded);
                assert_eq!(request.output_set().to_sha256(), [7; 32]);
                assert_eq!(request.task_handle(), LocalHandle::ZERO);
                assert_eq!(request.release_sequence().get(), 11);
                assert_eq!(request.commit_sequence().get(), 9);
                assert_eq!(request.fault_generation().get(), 4);
                assert_eq!(request.request_sequence().get(), 5);
                assert_eq!(request.task_epoch().get(), 3);
                assert_eq!(request.engine_epoch(), epoch);
                assert_eq!(request.occurred_at().elapsed_nanos(), 100);
                assert_eq!(request.version(), ExecutionContractVersion::V1_0);
            }
            assert_eq!(
                new_request(epoch, task_epoch, generation, other_epoch),
                Err(ExecutionContractError::EpochMismatch)
            );
        }
    }

    fn new_request(
        engine_epoch: BootEpochId,
        task_epoch: TaskEpoch,
        generation: FaultGeneration,
        timestamp_epoch: BootEpochId,
    ) -> Result<FallbackRequest, ExecutionContractError> {
        FallbackRequest::new(
            ExecutionContractVersion::V1_0,
            engine_epoch,
            LocalHandle::ZERO,
            task_epoch,
            generation,
            FallbackRequestSequence::new(5),
            FaultReason::HardLimitExceeded,
            OutputSetIdentity::from_sha256([7; 32]),
            ReleaseSequence::new(11),
            CommitSequence::new(9),
            MonotonicTimestamp::new(timestamp_epoch, 100),
        )
    }

    fn test_epoch(variant: u8) -> Result<BootEpochId, aurora_types::IdentifierError> {
        BootEpochId::from_bytes([
            0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, variant, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
            0x39, 0x8f,
        ])
    }
}
