use aurora_control_contracts::{
    CommitSequence, EventSequence, ReleaseSequence, TaskEpoch, TraceCapacity,
    WorkflowTraceEventKind, WorkflowTraceRecord, WorkflowTraceValueFragment, WorkflowTraceVersion,
};
use aurora_types::{BootEpochId, LocalHandle};

use super::{
    WorkflowTraceObserveError, WorkflowTracePublishError, WorkflowTracePublishOutcome,
    bounded_workflow_trace_channel,
};
use crate::SpscPopError;

#[test]
fn full_ring_drops_newest_consumes_sequence_and_exposes_gap()
-> Result<(), Box<dyn std::error::Error>> {
    let epoch = epoch(0x98)?;
    let capacity = TraceCapacity::new(1, 1)?;
    let (mut publisher, mut observer) = bounded_workflow_trace_channel(epoch, capacity)?;
    assert_eq!(
        publisher.try_publish(record(epoch, 0)?),
        Ok(WorkflowTracePublishOutcome::Published(EventSequence::ZERO))
    );
    assert_eq!(
        publisher.try_publish(record(epoch, 1)?),
        Ok(WorkflowTracePublishOutcome::DroppedNewest(
            EventSequence::new(1)
        ))
    );
    assert_eq!(observer.try_observe()?.record().event_sequence().get(), 0);
    assert_eq!(
        publisher.try_publish(record(epoch, 2)?),
        Ok(WorkflowTracePublishOutcome::Published(EventSequence::new(
            2
        )))
    );
    let read = observer.try_observe()?;
    assert_eq!(read.record().event_sequence().get(), 2);
    assert_eq!(read.missed_before(), 1);
    let statistics = publisher.statistics();
    assert_eq!(statistics.published, 2);
    assert_eq!(statistics.dropped_newest, 1);
    assert_eq!(statistics.full, 1);
    assert_eq!(statistics.high_water_mark, 1);
    assert_eq!(observer.statistics().observed_sequence_gaps, 1);
    Ok(())
}

#[test]
fn publisher_rejects_wrong_identity_and_non_next_sequence_without_hiding_state()
-> Result<(), Box<dyn std::error::Error>> {
    let engine_epoch = epoch(0x98)?;
    let other = epoch(0x99)?;
    let capacity = TraceCapacity::new(2, 2)?;
    let (mut publisher, mut observer) = bounded_workflow_trace_channel(engine_epoch, capacity)?;
    assert!(matches!(
        publisher.try_publish(record(other, 0)?),
        Err(WorkflowTracePublishError::EngineEpochMismatch { .. })
    ));
    assert!(matches!(
        publisher.try_publish(record(engine_epoch, 1)?),
        Err(WorkflowTracePublishError::UnexpectedEventSequence { .. })
    ));
    assert!(matches!(
        observer.try_observe(),
        Err(WorkflowTraceObserveError::Spsc(SpscPopError::Empty))
    ));
    assert!(publisher.try_publish(record(engine_epoch, 0)?).is_ok());
    drop(observer);
    assert_eq!(
        publisher.try_publish(record(engine_epoch, 1)?),
        Err(WorkflowTracePublishError::ObserverDropped)
    );
    assert!(matches!(
        publisher.try_publish(record(engine_epoch, 1)?),
        Err(WorkflowTracePublishError::UnexpectedEventSequence { .. })
    ));
    Ok(())
}

fn record(
    epoch: BootEpochId,
    sequence: u64,
) -> Result<WorkflowTraceRecord, Box<dyn std::error::Error>> {
    Ok(WorkflowTraceRecord::new(
        WorkflowTraceVersion::V1_0,
        WorkflowTraceEventKind::NodeExecuted,
        0,
        LocalHandle::ZERO,
        0,
        Some(0),
        None,
        None,
        None,
        None,
        Some(0),
        None,
        None,
        epoch,
        TaskEpoch::new(1)?,
        EventSequence::new(sequence),
        ReleaseSequence::ZERO,
        CommitSequence::ZERO,
        CommitSequence::ZERO,
        WorkflowTraceValueFragment::ABSENT,
    )?)
}

fn epoch(variant: u8) -> Result<BootEpochId, aurora_types::IdentifierError> {
    BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, variant, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
        0x39, 0x8f,
    ])
}
