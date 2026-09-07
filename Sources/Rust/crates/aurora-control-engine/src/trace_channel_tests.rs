use std::error::Error;

use aurora_control_contracts::{
    CommitSequence, EventSequence, ExecutionContractVersion, ReleaseSequence, TaskEpoch,
    TraceCapacity, TraceCounters, TraceEventKind, TraceRecord, TraceTiming,
};
use aurora_types::{BootEpochId, LocalHandle, MonotonicTimestamp};

use super::*;

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn stalled_observer_never_blocks_and_visible_sequence_reports_every_drop() -> TestResult {
    let capacity = TraceCapacity::new(1, 1)?;
    let (mut publisher, mut observer) = bounded_trace_channel(epoch()?, capacity)?;

    assert_eq!(
        publisher.try_publish(record(0, capacity)?)?,
        TracePublishOutcome::Published(EventSequence::ZERO)
    );
    assert_eq!(
        publisher.try_publish(record(1, capacity)?)?,
        TracePublishOutcome::DroppedNewest(EventSequence::new(1))
    );
    assert_eq!(
        publisher.try_publish(record(2, capacity)?)?,
        TracePublishOutcome::DroppedNewest(EventSequence::new(2))
    );

    let first = observer.try_observe()?;
    assert_eq!(first.record().event_sequence(), EventSequence::ZERO);
    assert_eq!(first.missed_before(), 0);
    assert_eq!(
        publisher.try_publish(record(3, capacity)?)?,
        TracePublishOutcome::Published(EventSequence::new(3))
    );
    let resumed = observer.try_observe()?;
    assert_eq!(resumed.record().event_sequence(), EventSequence::new(3));
    assert_eq!(resumed.missed_before(), 2);

    let statistics = publisher.statistics();
    assert_eq!(statistics.capacity, 1);
    assert_eq!(statistics.push_attempts, 4);
    assert_eq!(statistics.published, 2);
    assert_eq!(statistics.dropped_newest, 2);
    assert_eq!(statistics.full, 2);
    assert_eq!(statistics.high_water_mark, 1);
    Ok(())
}

#[test]
fn duplicate_and_gapped_events_are_rejected_before_touching_ring() -> TestResult {
    let capacity = TraceCapacity::new(2, 2)?;
    let engine_epoch = epoch()?;
    let (mut publisher, _observer) = bounded_trace_channel(engine_epoch, capacity)?;
    let other_epoch = BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x99, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x8f,
    ])?;
    assert!(matches!(
        publisher.try_publish(record_for_epoch(0, capacity, other_epoch)?),
        Err(TracePublishError::EngineEpochMismatch { .. })
    ));
    assert_eq!(publisher.statistics().push_attempts, 0);
    assert_eq!(
        publisher.try_publish(record(1, capacity)?),
        Err(TracePublishError::UnexpectedEventSequence {
            expected: Some(EventSequence::ZERO),
            actual: EventSequence::new(1),
        })
    );
    assert_eq!(publisher.statistics().push_attempts, 0);

    publisher.try_publish(record(0, capacity)?)?;
    assert_eq!(
        publisher.try_publish(record(0, capacity)?),
        Err(TracePublishError::UnexpectedEventSequence {
            expected: Some(EventSequence::new(1)),
            actual: EventSequence::ZERO,
        })
    );
    assert_eq!(publisher.statistics().push_attempts, 1);
    Ok(())
}

#[test]
fn dropped_observer_is_explicit_and_attempt_sequence_cannot_be_reused() -> TestResult {
    let capacity = TraceCapacity::new(1, 1)?;
    let (mut publisher, observer) = bounded_trace_channel(epoch()?, capacity)?;
    drop(observer);
    assert_eq!(
        publisher.try_publish(record(0, capacity)?),
        Err(TracePublishError::ObserverDropped)
    );
    assert_eq!(publisher.statistics().push_attempts, 1);
    assert!(publisher.statistics().consumer_dropped);
    assert_eq!(
        publisher.try_publish(record(0, capacity)?),
        Err(TracePublishError::UnexpectedEventSequence {
            expected: Some(EventSequence::new(1)),
            actual: EventSequence::ZERO,
        })
    );
    Ok(())
}

fn record(sequence: u64, capacity: TraceCapacity) -> Result<TraceRecord, Box<dyn Error>> {
    record_for_epoch(sequence, capacity, epoch()?)
}

fn record_for_epoch(
    sequence: u64,
    capacity: TraceCapacity,
    engine_epoch: BootEpochId,
) -> Result<TraceRecord, Box<dyn Error>> {
    let timing = TraceTiming::new(
        MonotonicTimestamp::new(engine_epoch, sequence * 10),
        MonotonicTimestamp::new(engine_epoch, sequence * 10 + 8),
        None,
        None,
    )?;
    let counters = TraceCounters::new(capacity, 0, 0, sequence, sequence, 0, 0, false)?;
    Ok(TraceRecord::new(
        ExecutionContractVersion::V1_0,
        engine_epoch,
        LocalHandle::ZERO,
        TaskEpoch::new(1)?,
        EventSequence::new(sequence),
        ReleaseSequence::new(sequence),
        CommitSequence::new(sequence),
        CommitSequence::new(sequence),
        TraceEventKind::ReleaseCompleted,
        timing,
        None,
        aurora_control_contracts::TaskState::Running,
        aurora_control_contracts::TaskState::Running,
        None,
        None,
        None,
        None,
        None,
        None,
        counters,
    )?)
}

fn epoch() -> Result<BootEpochId, aurora_types::IdentifierError> {
    BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x8f,
    ])
}
