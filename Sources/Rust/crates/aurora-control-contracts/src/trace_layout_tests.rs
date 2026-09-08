use std::hint::black_box;

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
    let encoded = encode_record_at_runtime(&record);
    assert_eq!(encoded.as_bytes(), &GOLDEN_FULL_RECORD);
    assert_eq!(decode_record_at_runtime(encoded.as_bytes())?, record);
    Ok(())
}

#[test]
fn header_and_file_view_reject_versions_truncation_trailing_bytes_and_epoch_mismatch()
-> Result<(), Box<dyn std::error::Error>> {
    let engine_epoch = epoch(0x98)?;
    let header = TraceFileHeader::new(engine_epoch, 1, 2);
    let encoded_header = header.encode();
    assert_eq!(encoded_header, GOLDEN_HEADER);
    assert_eq!(decode_header_at_runtime(&encoded_header)?, header);

    for length in [0, TRACE_FILE_HEADER_SIZE - 1] {
        assert!(matches!(
            parse_file_at_runtime(&encoded_header[..length]),
            Err(TraceCodecError::InvalidLength { .. })
        ));
    }

    let mut unsupported = encoded_header;
    unsupported[8..10].copy_from_slice(&2_u16.to_le_bytes());
    assert!(matches!(
        decode_header_at_runtime(&unsupported),
        Err(TraceCodecError::UnsupportedVersion { major: 2, minor: 0 })
    ));
    let impossible_count = TraceFileHeader::new(engine_epoch, u64::MAX, 0).encode();
    assert!(matches!(
        parse_file_at_runtime(&impossible_count),
        Err(TraceCodecError::LengthOverflow)
    ));

    let record = encode_record_at_runtime(&full_record(engine_epoch)?);
    let mut file = Vec::from(encoded_header);
    file.extend_from_slice(record.as_bytes());
    let view = parse_file_at_runtime(&file)?;
    assert_eq!(view.header(), header);
    assert_eq!(view.records().len(), 1);
    assert_eq!(
        view.record(0)?,
        decode_record_at_runtime(record.as_bytes())?
    );
    assert_eq!(view.record(1), Err(TraceCodecError::RecordIndexOutOfRange));

    file.push(0);
    assert!(matches!(
        parse_file_at_runtime(&file),
        Err(TraceCodecError::InvalidLength { .. })
    ));
    file.pop();
    file.pop();
    assert!(matches!(
        parse_file_at_runtime(&file),
        Err(TraceCodecError::InvalidLength { .. })
    ));

    let mut wrong_epoch_file = Vec::from(encoded_header);
    wrong_epoch_file
        .extend_from_slice(encode_record_at_runtime(&full_record(epoch(0x99)?)?).as_bytes());
    let wrong_epoch = parse_file_at_runtime(&wrong_epoch_file)?;
    assert_eq!(
        wrong_epoch.record(0),
        Err(TraceCodecError::EngineEpochMismatch)
    );
    Ok(())
}

#[test]
fn record_decoder_rejects_unknown_flags_reserved_bytes_and_noncanonical_absence()
-> Result<(), Box<dyn std::error::Error>> {
    let record = encode_record_at_runtime(&minimal_record(epoch(0x98)?)?);

    let mut unknown_flags = *record.as_bytes();
    unknown_flags[14..16].copy_from_slice(&0x8000_u16.to_le_bytes());
    assert_eq!(
        decode_record_at_runtime(&unknown_flags),
        Err(TraceCodecError::UnsupportedFlags)
    );

    let mut unsupported_version = *record.as_bytes();
    unsupported_version[8..10].copy_from_slice(&2_u16.to_le_bytes());
    assert!(matches!(
        decode_record_at_runtime(&unsupported_version),
        Err(TraceCodecError::UnsupportedVersion { major: 2, minor: 0 })
    ));

    let mut reserved = *record.as_bytes();
    reserved[319] = 1;
    assert_eq!(
        decode_record_at_runtime(&reserved),
        Err(TraceCodecError::NonCanonicalEncoding)
    );

    let mut absent_miss = *record.as_bytes();
    absent_miss[19] = MissOutcome::OnTime as u8;
    assert_eq!(
        decode_record_at_runtime(&absent_miss),
        Err(TraceCodecError::NonCanonicalEncoding)
    );

    let mut zero_capacity = *record.as_bytes();
    zero_capacity[168..172].copy_from_slice(&0_u32.to_le_bytes());
    assert_eq!(
        decode_record_at_runtime(&zero_capacity),
        Err(TraceCodecError::Contract(
            ExecutionContractError::InvalidCapacity
        ))
    );

    let mut missing_skipped_evidence = *record.as_bytes();
    missing_skipped_evidence[16] = TraceEventKind::ReleasesSkipped as u8;
    assert_eq!(
        decode_record_at_runtime(&missing_skipped_evidence),
        Err(TraceCodecError::Contract(
            ExecutionContractError::InvalidTraceEvidence
        ))
    );

    assert!(matches!(
        TraceRecordBytes::from_slice(black_box(&record.as_bytes()[..TRACE_RECORD_SIZE - 1])),
        Err(TraceCodecError::InvalidLength { .. })
    ));
    Ok(())
}

#[test]
fn header_decoder_covers_structural_boundaries() -> Result<(), Box<dyn std::error::Error>> {
    let engine_epoch = epoch(0x98)?;
    let header = TraceFileHeader::new(engine_epoch, 0, 0).encode();

    let mut invalid_header_magic = header;
    invalid_header_magic[0] = 0;
    assert_eq!(
        decode_header_at_runtime(&invalid_header_magic),
        Err(TraceCodecError::InvalidMagic)
    );

    let mut invalid_header_size = header;
    invalid_header_size[12..14].copy_from_slice(&0_u16.to_le_bytes());
    assert_eq!(
        decode_header_at_runtime(&invalid_header_size),
        Err(TraceCodecError::InvalidLayoutSize)
    );

    let mut invalid_record_size = header;
    invalid_record_size[14..16].copy_from_slice(&0_u16.to_le_bytes());
    assert_eq!(
        decode_header_at_runtime(&invalid_record_size),
        Err(TraceCodecError::InvalidLayoutSize)
    );

    let mut unsupported_header_flags = header;
    unsupported_header_flags[16..20].copy_from_slice(&1_u32.to_le_bytes());
    assert_eq!(
        decode_header_at_runtime(&unsupported_header_flags),
        Err(TraceCodecError::UnsupportedFlags)
    );

    let mut unsupported_header_minor = header;
    unsupported_header_minor[10..12].copy_from_slice(&1_u16.to_le_bytes());
    assert!(matches!(
        decode_header_at_runtime(&unsupported_header_minor),
        Err(TraceCodecError::UnsupportedVersion { major: 1, minor: 1 })
    ));
    Ok(())
}

#[test]
fn record_decoder_covers_structural_and_optional_field_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    let engine_epoch = epoch(0x98)?;
    let minimal = *encode_record_at_runtime(&minimal_record(engine_epoch)?).as_bytes();

    let mut invalid_record_magic = minimal;
    invalid_record_magic[0] = 0;
    assert_eq!(
        decode_record_at_runtime(&invalid_record_magic),
        Err(TraceCodecError::InvalidMagic)
    );

    let mut invalid_layout_size = minimal;
    invalid_layout_size[12..14].copy_from_slice(&0_u16.to_le_bytes());
    assert_eq!(
        decode_record_at_runtime(&invalid_layout_size),
        Err(TraceCodecError::InvalidLayoutSize)
    );

    let mut unsupported_record_minor = minimal;
    unsupported_record_minor[10..12].copy_from_slice(&1_u16.to_le_bytes());
    assert!(matches!(
        decode_record_at_runtime(&unsupported_record_minor),
        Err(TraceCodecError::UnsupportedVersion { major: 1, minor: 1 })
    ));

    let mut absent_fault = minimal;
    absent_fault[20..22].copy_from_slice(&(FaultReason::HardLimitExceeded as u16).to_le_bytes());
    assert_eq!(
        decode_record_at_runtime(&absent_fault),
        Err(TraceCodecError::NonCanonicalEncoding)
    );

    let mut absent_fallback = minimal;
    absent_fallback[160..168].copy_from_slice(&1_u64.to_le_bytes());
    assert_eq!(
        decode_record_at_runtime(&absent_fallback),
        Err(TraceCodecError::NonCanonicalEncoding)
    );

    let mut absent_skipped_range = minimal;
    absent_skipped_range[216..224].copy_from_slice(&1_u64.to_le_bytes());
    assert_eq!(
        decode_record_at_runtime(&absent_skipped_range),
        Err(TraceCodecError::NonCanonicalEncoding)
    );

    let mut absent_input_snapshot = minimal;
    absent_input_snapshot[240..244].copy_from_slice(&1_u32.to_le_bytes());
    assert_eq!(
        decode_record_at_runtime(&absent_input_snapshot),
        Err(TraceCodecError::NonCanonicalEncoding)
    );

    for flags in [
        FLAG_UTC,
        FLAG_UTC | FLAG_MAX_ERROR,
        FLAG_UTC | FLAG_LAST_SYNC,
    ] {
        let mut utc = minimal;
        put_u16(&mut utc, 14, flags);
        assert!(decode_record_at_runtime(&utc).is_ok());
    }

    for flag_without_utc in [FLAG_MAX_ERROR, FLAG_LAST_SYNC] {
        let mut invalid_utc_flags = minimal;
        put_u16(&mut invalid_utc_flags, 14, flag_without_utc);
        assert_eq!(
            decode_record_at_runtime(&invalid_utc_flags),
            Err(TraceCodecError::NonCanonicalEncoding)
        );
    }

    let optional_absence = record_with_utc_and_saturated_counters(engine_epoch)?;
    let encoded_optional_absence = encode_record_at_runtime(&optional_absence);
    assert_eq!(
        decode_record_at_runtime(encoded_optional_absence.as_bytes())?,
        optional_absence
    );

    let mut invalid_time_state = minimal;
    put_u16(&mut invalid_time_state, 14, FLAG_UTC);
    invalid_time_state[22] = u8::MAX;
    assert_eq!(
        decode_record_at_runtime(&invalid_time_state),
        Err(TraceCodecError::InvalidUtc)
    );

    let mut invalid_time_source = minimal;
    put_u16(&mut invalid_time_source, 14, FLAG_UTC);
    invalid_time_source[23] = u8::MAX;
    assert_eq!(
        decode_record_at_runtime(&invalid_time_source),
        Err(TraceCodecError::InvalidUtc)
    );

    let mut noncanonical_elapsed =
        *encode_record_at_runtime(&full_record(engine_epoch)?).as_bytes();
    noncanonical_elapsed[120..128].copy_from_slice(&4_u64.to_le_bytes());
    assert_eq!(
        decode_record_at_runtime(&noncanonical_elapsed),
        Err(TraceCodecError::NonCanonicalEncoding)
    );

    let one_record_header = TraceFileHeader::new(engine_epoch, 1, 0).encode();
    let mut file = Vec::from(one_record_header);
    file.extend_from_slice(&minimal);
    let view = parse_file_at_runtime(&file)?;
    let mut records = view.records();
    assert!(records.next().transpose()?.is_some());
    assert!(records.next().is_none());
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
    let mut encoded = *encode_record_at_runtime(&full_record(epoch(0x98)?)?).as_bytes();
    encoded[22] = TimeQualityState::Unknown as u8;
    encoded[23] = TimeSource::Unknown as u8;

    let record = decode_record_at_runtime(&encoded)?;
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

fn record_with_utc_and_saturated_counters(
    engine_epoch: BootEpochId,
) -> Result<TraceRecord, Box<dyn std::error::Error>> {
    let timing = TraceTiming::new(
        MonotonicTimestamp::new(engine_epoch, 10),
        MonotonicTimestamp::new(engine_epoch, 20),
        None,
        None,
    )?;
    let capacity = TraceCapacity::new(1, 1)?;
    let counters = TraceCounters::new(capacity, 0, 0, 0, 0, 0, 0, true)?;
    let utc = UtcObservation::new(
        UtcTimestamp::new(0, 0)?,
        TimeQuality::new(TimeQualityState::Unknown, TimeSource::Unknown, None, None),
    );
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
        Some(utc),
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

fn encode_record_at_runtime(record: &TraceRecord) -> TraceRecordBytes {
    // 避免测试常量在 LLVM coverage 插桩前被折叠，确保实际经过编码/解码分支。
    TraceRecordBytes::encode(*black_box(record))
}

fn decode_record_at_runtime(bytes: &[u8]) -> Result<TraceRecord, TraceCodecError> {
    TraceRecordBytes::from_slice(black_box(bytes))?.decode()
}

fn decode_header_at_runtime(bytes: &[u8]) -> Result<TraceFileHeader, TraceCodecError> {
    TraceFileHeader::decode(black_box(bytes))
}

fn parse_file_at_runtime(bytes: &[u8]) -> Result<TraceFileView<'_>, TraceCodecError> {
    TraceFileView::parse(black_box(bytes))
}

fn epoch(variant: u8) -> Result<BootEpochId, aurora_types::IdentifierError> {
    BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, variant, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
        0x39, 0x8f,
    ])
}
