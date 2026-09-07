use aurora_types::{
    BootEpochId, LocalHandle, MonotonicTimestamp, TimeQuality, TimeQualityState, TimeSource,
    UtcTimestamp,
};

use super::*;

const GOLDEN_HEADER: [u8; TRACE_FILE_HEADER_SIZE] = [
    65, 85, 82, 84, 82, 67, 48, 49, 1, 0, 0, 0, 64, 0, 64, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1, 137, 15,
    62, 76, 123, 124, 194, 152, 196, 220, 12, 12, 7, 57, 143, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

const GOLDEN_FULL_RECORD: [u8; TRACE_RECORD_SIZE] = [
    65, 85, 82, 84, 82, 82, 48, 49, 1, 0, 0, 0, 64, 1, 127, 7, 2, 4, 5, 2, 5, 0, 3, 3, 0, 0, 0, 0,
    42, 0, 0, 0, 1, 137, 15, 62, 76, 123, 124, 194, 152, 196, 220, 12, 12, 7, 57, 143, 1, 0, 0, 0,
    0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0,
    0, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 11, 0, 0, 0, 0, 0, 0, 0, 14, 0,
    0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 232, 3, 0, 0,
    0, 0, 0, 0, 254, 255, 255, 255, 255, 255, 255, 255, 9, 0, 0, 0, 0, 0, 0, 0, 5, 0, 0, 0, 0, 0,
    0, 0, 8, 0, 0, 0, 1, 0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 6, 0, 0, 0, 0, 0,
    0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0,
    0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0,
    0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
];

#[test]
fn full_record_has_stable_golden_bytes_and_round_trips() -> Result<(), Box<dyn std::error::Error>> {
    let record = full_record(epoch(0x98)?)?;
    let encoded = TraceRecordBytes::encode(record);
    assert_eq!(encoded.as_bytes(), &GOLDEN_FULL_RECORD);
    assert_eq!(encoded.decode()?, record);
    Ok(())
}

#[test]
fn header_and_file_view_reject_versions_truncation_trailing_bytes_and_epoch_mismatch()
-> Result<(), Box<dyn std::error::Error>> {
    let engine_epoch = epoch(0x98)?;
    let header = TraceFileHeader::new(engine_epoch, 1, 2);
    let encoded_header = header.encode();
    assert_eq!(encoded_header, GOLDEN_HEADER);
    assert_eq!(TraceFileHeader::decode(&encoded_header)?, header);

    for length in [0, TRACE_FILE_HEADER_SIZE - 1] {
        assert!(matches!(
            TraceFileView::parse(&encoded_header[..length]),
            Err(TraceCodecError::InvalidLength { .. })
        ));
    }

    let mut unsupported = encoded_header;
    unsupported[8..10].copy_from_slice(&2_u16.to_le_bytes());
    assert!(matches!(
        TraceFileHeader::decode(&unsupported),
        Err(TraceCodecError::UnsupportedVersion { major: 2, minor: 0 })
    ));
    let impossible_count = TraceFileHeader::new(engine_epoch, u64::MAX, 0).encode();
    assert!(matches!(
        TraceFileView::parse(&impossible_count),
        Err(TraceCodecError::LengthOverflow)
    ));

    let record = TraceRecordBytes::encode(full_record(engine_epoch)?);
    let mut file = Vec::from(encoded_header);
    file.extend_from_slice(record.as_bytes());
    let view = TraceFileView::parse(&file)?;
    assert_eq!(view.header(), header);
    assert_eq!(view.records().len(), 1);
    assert_eq!(view.record(0)?, record.decode()?);
    assert_eq!(view.record(1), Err(TraceCodecError::RecordIndexOutOfRange));

    file.push(0);
    assert!(matches!(
        TraceFileView::parse(&file),
        Err(TraceCodecError::InvalidLength { .. })
    ));
    file.pop();
    file.pop();
    assert!(matches!(
        TraceFileView::parse(&file),
        Err(TraceCodecError::InvalidLength { .. })
    ));

    let mut wrong_epoch_file = Vec::from(encoded_header);
    wrong_epoch_file
        .extend_from_slice(TraceRecordBytes::encode(full_record(epoch(0x99)?)?).as_bytes());
    let wrong_epoch = TraceFileView::parse(&wrong_epoch_file)?;
    assert_eq!(
        wrong_epoch.record(0),
        Err(TraceCodecError::EngineEpochMismatch)
    );
    Ok(())
}

#[test]
fn record_decoder_rejects_unknown_flags_reserved_bytes_and_noncanonical_absence()
-> Result<(), Box<dyn std::error::Error>> {
    let record = TraceRecordBytes::encode(minimal_record(epoch(0x98)?)?);

    let mut unknown_flags = *record.as_bytes();
    unknown_flags[14..16].copy_from_slice(&0x8000_u16.to_le_bytes());
    assert_eq!(
        TraceRecordBytes::from_slice(&unknown_flags)?.decode(),
        Err(TraceCodecError::UnsupportedFlags)
    );

    let mut unsupported_version = *record.as_bytes();
    unsupported_version[8..10].copy_from_slice(&2_u16.to_le_bytes());
    assert!(matches!(
        TraceRecordBytes::from_slice(&unsupported_version)?.decode(),
        Err(TraceCodecError::UnsupportedVersion { major: 2, minor: 0 })
    ));

    let mut reserved = *record.as_bytes();
    reserved[319] = 1;
    assert_eq!(
        TraceRecordBytes::from_slice(&reserved)?.decode(),
        Err(TraceCodecError::NonCanonicalEncoding)
    );

    let mut absent_miss = *record.as_bytes();
    absent_miss[19] = MissOutcome::OnTime as u8;
    assert_eq!(
        TraceRecordBytes::from_slice(&absent_miss)?.decode(),
        Err(TraceCodecError::NonCanonicalEncoding)
    );

    let mut zero_capacity = *record.as_bytes();
    zero_capacity[168..172].copy_from_slice(&0_u32.to_le_bytes());
    assert_eq!(
        TraceRecordBytes::from_slice(&zero_capacity)?.decode(),
        Err(TraceCodecError::Contract(
            ExecutionContractError::InvalidCapacity
        ))
    );

    let mut missing_skipped_evidence = *record.as_bytes();
    missing_skipped_evidence[16] = TraceEventKind::ReleasesSkipped as u8;
    assert_eq!(
        TraceRecordBytes::from_slice(&missing_skipped_evidence)?.decode(),
        Err(TraceCodecError::Contract(
            ExecutionContractError::InvalidTraceEvidence
        ))
    );

    assert!(matches!(
        TraceRecordBytes::from_slice(&record.as_bytes()[..TRACE_RECORD_SIZE - 1]),
        Err(TraceCodecError::InvalidLength { .. })
    ));
    Ok(())
}

#[test]
fn skipped_and_snapshot_boundaries_are_explicit() {
    assert_eq!(
        TraceSkippedReleases::new(ReleaseSequence::ZERO, ReleaseSequence::ZERO, 0),
        Err(ExecutionContractError::InvalidTraceCounts)
    );
    assert_eq!(
        TraceSkippedReleases::new(
            ReleaseSequence::new(u64::MAX),
            ReleaseSequence::new(u64::MAX),
            2,
        ),
        Err(ExecutionContractError::CounterOverflow)
    );
    assert_eq!(
        TraceSkippedReleases::new(ReleaseSequence::ZERO, ReleaseSequence::new(2), 2),
        Err(ExecutionContractError::InvalidTraceCounts)
    );
    let task_epoch = TaskEpoch::new(1);
    assert!(task_epoch.is_ok());
    if let Ok(task_epoch) = task_epoch {
        assert_eq!(
            TraceSnapshotEvidence::new(LocalHandle::ZERO, task_epoch, CommitSequence::new(3), 4,),
            Err(ExecutionContractError::InvalidTraceEvidence)
        );
    }
}

#[test]
fn utc_unknown_state_and_source_zero_values_are_canonical() -> Result<(), Box<dyn std::error::Error>>
{
    let mut encoded = *TraceRecordBytes::encode(full_record(epoch(0x98)?)?).as_bytes();
    encoded[22] = TimeQualityState::Unknown as u8;
    encoded[23] = TimeSource::Unknown as u8;

    let record = TraceRecordBytes::from_slice(&encoded)?.decode()?;
    assert!(matches!(
        record.utc().map(UtcObservation::quality),
        Some(quality)
            if quality.state() == TimeQualityState::Unknown
                && quality.source() == TimeSource::Unknown
    ));
    Ok(())
}

fn full_record(engine_epoch: BootEpochId) -> Result<TraceRecord, Box<dyn std::error::Error>> {
    let timing = TraceTiming::new(
        MonotonicTimestamp::new(engine_epoch, 10),
        MonotonicTimestamp::new(engine_epoch, 20),
        Some(MonotonicTimestamp::new(engine_epoch, 11)),
        Some(MonotonicTimestamp::new(engine_epoch, 14)),
    )?;
    let utc = UtcObservation::new(
        UtcTimestamp::new(-1, 42)?,
        TimeQuality::new(
            TimeQualityState::Holdover,
            TimeSource::Ptp,
            Some(1_000),
            Some(UtcTimestamp::new(-2, 9)?),
        ),
    );
    let capacity = TraceCapacity::new(8, 8)?;
    let counters = TraceCounters::new(capacity, 1, 3, 7, 6, 1, 1, false)?;
    let skipped = TraceSkippedReleases::new(ReleaseSequence::new(2), ReleaseSequence::new(4), 3)?;
    let task_epoch = TaskEpoch::new(1)?;
    Ok(TraceRecord::new(
        ExecutionContractVersion::V1_0,
        engine_epoch,
        LocalHandle::ZERO,
        task_epoch,
        EventSequence::new(7),
        ReleaseSequence::new(5),
        CommitSequence::new(3),
        CommitSequence::new(3),
        TraceEventKind::ReleasesSkipped,
        timing,
        Some(utc),
        TaskState::Degraded,
        TaskState::FaultLocked,
        Some(MissOutcome::SkippedRelease),
        Some(FaultReason::MissWindowExceeded),
        Some(FallbackRequestSequence::new(5)),
        Some(skipped),
        Some(TraceSnapshotEvidence::new(
            LocalHandle::new(1)?,
            TaskEpoch::new(2)?,
            CommitSequence::new(3),
            2,
        )?),
        Some(TraceSnapshotEvidence::new(
            LocalHandle::ZERO,
            task_epoch,
            CommitSequence::new(4),
            0,
        )?),
        counters,
    )?)
}

fn minimal_record(engine_epoch: BootEpochId) -> Result<TraceRecord, Box<dyn std::error::Error>> {
    let timing = TraceTiming::new(
        MonotonicTimestamp::new(engine_epoch, 10),
        MonotonicTimestamp::new(engine_epoch, 20),
        None,
        None,
    )?;
    let capacity = TraceCapacity::new(1, 1)?;
    let counters = TraceCounters::new(capacity, 0, 0, 0, 0, 0, 0, false)?;
    Ok(TraceRecord::new(
        ExecutionContractVersion::V1_0,
        engine_epoch,
        LocalHandle::ZERO,
        TaskEpoch::new(1)?,
        EventSequence::ZERO,
        ReleaseSequence::ZERO,
        CommitSequence::ZERO,
        CommitSequence::ZERO,
        TraceEventKind::ReleaseCompleted,
        timing,
        None,
        TaskState::Running,
        TaskState::Running,
        None,
        None,
        None,
        None,
        None,
        None,
        counters,
    )?)
}

fn epoch(variant: u8) -> Result<BootEpochId, aurora_types::IdentifierError> {
    BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, variant, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
        0x39, 0x8f,
    ])
}
