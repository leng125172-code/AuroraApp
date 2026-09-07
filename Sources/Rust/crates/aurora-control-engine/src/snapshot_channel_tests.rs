use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aurora_control_contracts::{
    CommitSequence, ExecutionContractVersion, ReleaseSequence, SnapshotFreshness, SnapshotMetadata,
    SnapshotProgress, TaskEpoch, UtcObservation,
};
use aurora_types::{
    BootEpochId, MonotonicTimestamp, QualityCode, TimeQuality, TimeQualityState, TimeSource,
    UtcTimestamp,
};

use super::{
    SnapshotChannelDefinition, SnapshotChannelError, SnapshotLatchProgress,
    SnapshotPayloadCapacity, SnapshotPublisher,
};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn readers_keep_independent_versions_gaps_and_stale_quality() -> TestResult {
    let (mut publisher, mut fast_reader, mut slow_reader, epoch) = channel(8)?;
    publisher.publish(metadata(epoch, 1, 0, 0, 4)?, &[1, 1, 1, 1])?;

    let first = fast_reader.try_latch(MonotonicTimestamp::new(epoch, 5), 5)?;
    assert_eq!(first.progress(), SnapshotLatchProgress::First);
    assert_eq!(first.payload(), &[1, 1, 1, 1]);
    assert!(first.metadata().utc().is_some());
    assert_eq!(first.observation().freshness(), SnapshotFreshness::Fresh);
    assert_eq!(first.observed_quality(), QualityCode::GOOD);
    assert_eq!(
        slow_reader
            .try_latch(MonotonicTimestamp::new(epoch, 6), 5)?
            .progress(),
        SnapshotLatchProgress::First
    );

    publisher.publish(metadata(epoch, 1, 1, 10, 4)?, &[2, 2, 2, 2])?;
    assert_eq!(
        fast_reader
            .try_latch(MonotonicTimestamp::new(epoch, 11), 5)?
            .progress(),
        SnapshotLatchProgress::Advanced(SnapshotProgress::Next)
    );
    publisher.publish(metadata(epoch, 1, 2, 20, 4)?, &[3, 3, 3, 3])?;
    publisher.publish(metadata(epoch, 1, 3, 30, 4)?, &[4, 4, 4, 4])?;

    let fast = fast_reader.try_latch(MonotonicTimestamp::new(epoch, 36), 5)?;
    assert_eq!(
        fast.progress(),
        SnapshotLatchProgress::Advanced(SnapshotProgress::Gap { missed_commits: 1 })
    );
    assert_eq!(fast.observation().freshness(), SnapshotFreshness::Stale);
    assert!(fast.observed_quality().flags().bits() != 0);
    let slow = slow_reader.try_latch(MonotonicTimestamp::new(epoch, 31), 5)?;
    assert_eq!(
        slow.progress(),
        SnapshotLatchProgress::Advanced(SnapshotProgress::Gap { missed_commits: 2 })
    );
    assert_eq!(slow.payload(), &[4, 4, 4, 4]);

    let unchanged = fast_reader.try_latch(MonotonicTimestamp::new(epoch, 37), 10)?;
    assert_eq!(unchanged.progress(), SnapshotLatchProgress::Unchanged);
    assert_eq!(fast_reader.statistics().missed_commits, 1);
    assert_eq!(slow_reader.statistics().missed_commits, 2);

    publisher.publish(metadata(epoch, 2, 0, 40, 2)?, &[5, 5])?;
    let reinitialized = fast_reader.try_latch(MonotonicTimestamp::new(epoch, 40), 0)?;
    assert_eq!(
        reinitialized.progress(),
        SnapshotLatchProgress::Advanced(SnapshotProgress::NewTaskEpoch)
    );
    assert_eq!(reinitialized.payload(), &[5, 5]);
    Ok(())
}

#[test]
fn publication_rejects_capacity_definition_and_sequence_boundaries() -> TestResult {
    assert!(SnapshotPayloadCapacity::new(0, 8).is_err());
    assert!(SnapshotPayloadCapacity::new(9, 8).is_err());
    let (mut publisher, mut reader, _unused, epoch) = channel(4)?;
    assert!(matches!(
        reader.try_latch(MonotonicTimestamp::new(epoch, 0), 0),
        Err(SnapshotChannelError::NoPublication)
    ));
    assert!(matches!(
        publisher.publish(metadata(epoch, 1, 0, 0, 5)?, &[0; 5]),
        Err(SnapshotChannelError::PayloadTooLarge { .. })
    ));
    assert!(matches!(
        publisher.publish(metadata(epoch, 1, 0, 0, 3)?, &[0; 4]),
        Err(SnapshotChannelError::PayloadLengthMismatch { .. })
    ));
    let wrong_schema = SnapshotMetadata::new(
        ExecutionContractVersion::V1_0,
        epoch,
        TaskEpoch::new(1)?,
        CommitSequence::ZERO,
        ReleaseSequence::ZERO,
        MonotonicTimestamp::new(epoch, 0),
        None,
        QualityCode::GOOD,
        [8; 32],
        4,
    )?;
    assert_eq!(
        publisher.publish(wrong_schema, &[0; 4]),
        Err(SnapshotChannelError::DefinitionMismatch)
    );

    publisher.publish(metadata(epoch, 1, 0, 0, 4)?, &[1; 4])?;
    assert!(matches!(
        publisher.publish(metadata(epoch, 1, 0, 1, 4)?, &[2; 4]),
        Err(SnapshotChannelError::InvalidPublicationOrder(_))
    ));
    publisher.publish(metadata(epoch, 2, 0, 2, 4)?, &[3; 4])?;
    assert_eq!(
        reader
            .try_latch(MonotonicTimestamp::new(epoch, 3), 10)?
            .progress(),
        SnapshotLatchProgress::First
    );
    Ok(())
}

#[test]
fn failed_latch_keeps_the_previous_reader_copy_and_generations_are_even() -> TestResult {
    let (mut publisher, mut reader, _unused, epoch) = channel(4)?;
    publisher.publish(metadata(epoch, 1, 0, 0, 4)?, &[1; 4])?;
    reader.try_latch(MonotonicTimestamp::new(epoch, 0), 0)?;
    let old_active = reader.active_buffer;
    let old_payload = reader.buffers[old_active].clone();

    publisher.publish(metadata(epoch, 1, 1, 10, 4)?, &[2; 4])?;
    assert_eq!(publisher.descriptor_generation & 1, 0);
    assert!(
        publisher
            .slot_generations
            .iter()
            .all(|generation| generation & 1 == 0)
    );

    let stable_descriptor = publisher.descriptor_generation;
    publisher
        .shared
        .descriptor
        .generation
        .store(stable_descriptor - 1, Ordering::Release);
    assert!(matches!(
        reader.try_latch(MonotonicTimestamp::new(epoch, 10), 0),
        Err(SnapshotChannelError::Contended)
    ));
    assert_eq!(reader.active_buffer, old_active);
    assert_eq!(reader.buffers[old_active], old_payload);
    assert_eq!(reader.statistics().contended_reads, 1);

    publisher
        .shared
        .descriptor
        .generation
        .store(stable_descriptor, Ordering::Release);
    let next = reader.try_latch(MonotonicTimestamp::new(epoch, 10), 0)?;
    assert_eq!(next.payload(), &[2; 4]);
    assert_eq!(next.publication_generation(), 2);
    Ok(())
}

#[test]
fn invalid_observation_time_does_not_consume_a_new_reader_version() -> TestResult {
    let (mut publisher, mut reader, _unused, epoch) = channel(4)?;
    publisher.publish(metadata(epoch, 1, 0, 0, 4)?, &[1; 4])?;
    reader.try_latch(MonotonicTimestamp::new(epoch, 0), 0)?;
    publisher.publish(metadata(epoch, 1, 1, 10, 4)?, &[2; 4])?;
    assert!(matches!(
        reader.try_latch(MonotonicTimestamp::new(epoch, 9), 0),
        Err(SnapshotChannelError::InvalidObservationTime(_))
    ));
    assert_eq!(
        reader
            .accepted_metadata
            .map(SnapshotMetadata::commit_sequence),
        Some(CommitSequence::ZERO)
    );
    let accepted = reader.try_latch(MonotonicTimestamp::new(epoch, 10), 0)?;
    assert_eq!(accepted.metadata().commit_sequence().get(), 1);
    assert_eq!(
        accepted.progress(),
        SnapshotLatchProgress::Advanced(SnapshotProgress::Next)
    );
    Ok(())
}

#[test]
fn generation_exhaustion_rejects_before_changing_the_descriptor() -> TestResult {
    let (mut publisher, mut reader, _unused, epoch) = channel(4)?;
    publisher.publish(metadata(epoch, 1, 0, 0, 4)?, &[1; 4])?;
    let published_descriptor = publisher.descriptor_generation;
    publisher.descriptor_generation = u64::MAX - 1;
    assert_eq!(
        publisher.publish(metadata(epoch, 1, 1, 1, 4)?, &[2; 4]),
        Err(SnapshotChannelError::PublicationGenerationExhausted)
    );
    assert_eq!(
        publisher
            .shared
            .descriptor
            .generation
            .load(Ordering::Acquire),
        published_descriptor
    );
    assert_eq!(
        reader
            .try_latch(MonotonicTimestamp::new(epoch, 1), 1)?
            .payload(),
        &[1; 4]
    );
    Ok(())
}

#[test]
fn concurrent_readers_never_accept_torn_payloads_and_stalled_reader_does_not_block() -> TestResult {
    const PUBLICATIONS: u64 = 20_000;
    const PAYLOAD_BYTES: usize = 64;

    let (mut publisher, first_reader, second_reader, epoch) = channel(PAYLOAD_BYTES)?;
    let _stalled_reader = publisher.create_reader()?;
    let done = Arc::new(AtomicBool::new(false));
    let first_done = Arc::clone(&done);
    let second_done = Arc::clone(&done);
    let first =
        std::thread::spawn(move || exercise_reader(first_reader, first_done.as_ref(), epoch));
    let second =
        std::thread::spawn(move || exercise_reader(second_reader, second_done.as_ref(), epoch));

    for sequence in 0..PUBLICATIONS {
        let byte = sequence.to_le_bytes()[0];
        let payload = [byte; PAYLOAD_BYTES];
        publisher.publish(
            metadata(epoch, 1, sequence, sequence, u32::try_from(PAYLOAD_BYTES)?)?,
            &payload,
        )?;
    }
    done.store(true, Ordering::Release);

    let first_result = first.join();
    let second_result = second.join();
    assert!(first_result.is_ok());
    assert!(second_result.is_ok());
    if let Ok(result) = first_result {
        assert!(result.is_ok(), "{result:?}");
    }
    if let Ok(result) = second_result {
        assert!(result.is_ok(), "{result:?}");
    }
    Ok(())
}

fn exercise_reader(
    mut reader: super::SnapshotReader,
    done: &AtomicBool,
    epoch: BootEpochId,
) -> Result<(), String> {
    while !done.load(Ordering::Acquire) {
        match reader.try_latch(MonotonicTimestamp::new(epoch, 30_000), u64::MAX) {
            Ok(snapshot) => validate_snapshot(snapshot)?,
            Err(SnapshotChannelError::NoPublication | SnapshotChannelError::Contended) => {}
            Err(error) => return Err(error.to_string()),
        }
        std::thread::yield_now();
    }
    let final_snapshot = reader
        .try_latch(MonotonicTimestamp::new(epoch, 30_000), u64::MAX)
        .map_err(|error| error.to_string())?;
    validate_snapshot(final_snapshot)?;
    if final_snapshot.metadata().commit_sequence().get() != 19_999 {
        return Err("reader did not observe the final stable publication".to_owned());
    }
    Ok(())
}

fn validate_snapshot(snapshot: super::LatchedSnapshot<'_>) -> Result<(), String> {
    let expected = snapshot.metadata().commit_sequence().get().to_le_bytes()[0];
    if snapshot.payload().iter().any(|value| *value != expected) {
        return Err("accepted payload was torn".to_owned());
    }
    Ok(())
}

fn channel(
    payload_capacity: usize,
) -> Result<
    (
        SnapshotPublisher,
        super::SnapshotReader,
        super::SnapshotReader,
        BootEpochId,
    ),
    Box<dyn Error>,
> {
    let epoch = epoch()?;
    let capacity = SnapshotPayloadCapacity::new(payload_capacity, payload_capacity)?;
    let publisher = SnapshotPublisher::new(SnapshotChannelDefinition::new(
        ExecutionContractVersion::V1_0,
        epoch,
        [7; 32],
        capacity,
    ))?;
    let first = publisher.create_reader()?;
    let second = publisher.create_reader()?;
    Ok((publisher, first, second, epoch))
}

fn metadata(
    epoch: BootEpochId,
    task_epoch: u64,
    commit_sequence: u64,
    published_nanos: u64,
    payload_length: u32,
) -> Result<SnapshotMetadata, Box<dyn Error>> {
    let utc_timestamp = UtcTimestamp::new(1_700_000_000, 123)?;
    SnapshotMetadata::new(
        ExecutionContractVersion::V1_0,
        epoch,
        TaskEpoch::new(task_epoch)?,
        CommitSequence::new(commit_sequence),
        ReleaseSequence::new(commit_sequence),
        MonotonicTimestamp::new(epoch, published_nanos),
        Some(UtcObservation::new(
            utc_timestamp,
            TimeQuality::new(
                TimeQualityState::Good,
                TimeSource::Ptp,
                Some(50),
                Some(utc_timestamp),
            ),
        )),
        QualityCode::GOOD,
        [7; 32],
        payload_length,
    )
    .map_err(Into::into)
}

fn epoch() -> Result<BootEpochId, Box<dyn Error>> {
    BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x8f,
    ])
    .map_err(Into::into)
}
