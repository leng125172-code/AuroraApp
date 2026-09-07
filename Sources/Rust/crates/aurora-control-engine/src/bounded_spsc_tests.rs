use std::error::Error;

use super::{
    SpscBuildError, SpscCapacity, SpscOverflowPolicy, SpscPopError, SpscPushError, SpscPushOutcome,
    bounded_spsc,
};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn capacity_one_rejects_full_and_reports_high_water() -> TestResult {
    assert_eq!(
        SpscCapacity::new(0, 1),
        Err(SpscBuildError::InvalidCapacity {
            requested: 0,
            maximum: 1
        })
    );
    assert!(SpscCapacity::new(2, 1).is_err());
    let capacity = SpscCapacity::new(1, 1)?;
    let (mut producer, mut consumer) = bounded_spsc(capacity, SpscOverflowPolicy::RejectNewest);
    assert_eq!(consumer.try_pop(), Err(SpscPopError::Empty));
    assert_eq!(
        producer.try_push(10_u32),
        Ok(SpscPushOutcome::Published(super::SpscSequence(0)))
    );
    assert_eq!(producer.try_push(11), Err(SpscPushError::Full(11)));
    let item = consumer.try_pop()?;
    assert_eq!(item.sequence().get(), 0);
    assert_eq!(item.missed_before(), 0);
    assert_eq!(item.value(), 10);
    let statistics = producer.statistics();
    assert_eq!(statistics.capacity, 1);
    assert_eq!(statistics.readable, 0);
    assert_eq!(statistics.writable, 1);
    assert_eq!(statistics.push_attempts, 2);
    assert_eq!(statistics.published, 1);
    assert_eq!(statistics.rejected_full, 1);
    assert_eq!(statistics.full, 1);
    assert_eq!(statistics.pop_attempts, 2);
    assert_eq!(statistics.high_water_mark, 1);
    assert_eq!(statistics.consumed, 1);
    assert!(!statistics.producer_dropped);
    assert!(!statistics.consumer_dropped);
    Ok(())
}

#[test]
fn drop_newest_consumes_sequence_and_exposes_the_gap() -> TestResult {
    let capacity = SpscCapacity::new(2, 2)?;
    let (mut producer, mut consumer) = bounded_spsc(capacity, SpscOverflowPolicy::DropNewest);
    assert!(matches!(
        producer.try_push(10_u8),
        Ok(SpscPushOutcome::Published(_))
    ));
    assert!(matches!(
        producer.try_push(11),
        Ok(SpscPushOutcome::Published(_))
    ));
    assert_eq!(
        producer.try_push(12),
        Ok(SpscPushOutcome::DroppedNewest(super::SpscSequence(2)))
    );
    assert_eq!(consumer.try_pop()?.value(), 10);
    assert_eq!(consumer.try_pop()?.value(), 11);
    assert!(matches!(
        producer.try_push(13),
        Ok(SpscPushOutcome::Published(_))
    ));
    let after_gap = consumer.try_pop()?;
    assert_eq!(after_gap.sequence().get(), 3);
    assert_eq!(after_gap.missed_before(), 1);
    let statistics = consumer.statistics();
    assert_eq!(statistics.push_attempts, 4);
    assert_eq!(statistics.dropped_newest, 1);
    assert_eq!(statistics.full, 1);
    assert_eq!(statistics.pop_attempts, 3);
    assert_eq!(statistics.observed_sequence_gaps, 1);
    assert_eq!(statistics.high_water_mark, 2);
    Ok(())
}

#[test]
fn wraparound_preserves_order_without_growing_capacity() -> TestResult {
    let capacity = SpscCapacity::new(3, 3)?;
    let (mut producer, mut consumer) = bounded_spsc(capacity, SpscOverflowPolicy::RejectNewest);
    for value in 0_u64..10_000 {
        assert!(producer.try_push(value).is_ok());
        let read = consumer.try_pop()?;
        assert_eq!(read.value(), value);
        assert_eq!(read.sequence().get(), value);
        assert_eq!(read.missed_before(), 0);
    }
    assert_eq!(producer.statistics().high_water_mark, 1);
    Ok(())
}

#[test]
fn stalled_consumer_never_blocks_drop_newest_producer() -> TestResult {
    let capacity = SpscCapacity::new(8, 8)?;
    let (mut producer, mut consumer) = bounded_spsc(capacity, SpscOverflowPolicy::DropNewest);
    let producer_thread = std::thread::spawn(move || {
        for value in 0_u32..10_000 {
            if producer.try_push(value).is_err() {
                return Err("producer unexpectedly rejected a DropNewest item");
            }
        }
        Ok(producer.statistics())
    });
    let joined = producer_thread.join();
    assert!(joined.is_ok());
    if let Ok(result) = joined {
        assert!(result.is_ok());
        if let Ok(statistics) = result {
            assert_eq!(statistics.published, 8);
            assert_eq!(statistics.dropped_newest, 9_992);
            assert_eq!(statistics.push_attempts, 10_000);
            assert_eq!(statistics.full, 9_992);
            assert_eq!(statistics.readable, 8);
            assert_eq!(statistics.writable, 0);
            assert_eq!(statistics.high_water_mark, 8);
        }
    }
    for expected in 0_u32..8 {
        assert_eq!(consumer.try_pop()?.value(), expected);
    }
    assert_eq!(consumer.try_pop(), Err(SpscPopError::ProducerDropped));
    Ok(())
}

#[test]
fn endpoint_drop_is_visible_without_hidden_retry() -> TestResult {
    let capacity = SpscCapacity::new(1, 1)?;
    let (mut producer, consumer) = bounded_spsc(capacity, SpscOverflowPolicy::RejectNewest);
    drop(consumer);
    assert!(producer.consumer_dropped());
    assert!(producer.statistics().consumer_dropped);
    assert_eq!(
        producer.try_push(7_u8),
        Err(SpscPushError::ConsumerDropped(7))
    );

    let (producer, mut consumer) = bounded_spsc::<u8>(capacity, SpscOverflowPolicy::RejectNewest);
    drop(producer);
    assert!(consumer.producer_dropped());
    assert!(consumer.statistics().producer_dropped);
    assert_eq!(consumer.try_pop(), Err(SpscPopError::ProducerDropped));
    Ok(())
}

#[test]
fn statistics_saturate_and_set_the_sticky_flag_without_wrapping() -> TestResult {
    let capacity = SpscCapacity::new(1, 1)?;
    let (mut producer, _consumer) = bounded_spsc(capacity, SpscOverflowPolicy::RejectNewest);
    producer.push_attempts = u64::MAX;
    producer
        .shared_statistics
        .push_attempts
        .store(u64::MAX, std::sync::atomic::Ordering::Release);
    assert!(producer.try_push(1_u8).is_ok());
    let statistics = producer.statistics();
    assert_eq!(statistics.push_attempts, u64::MAX);
    assert!(statistics.saturated);

    producer.next_sequence = None;
    assert_eq!(
        producer.try_push(2),
        Err(SpscPushError::SequenceExhausted(2))
    );
    Ok(())
}
