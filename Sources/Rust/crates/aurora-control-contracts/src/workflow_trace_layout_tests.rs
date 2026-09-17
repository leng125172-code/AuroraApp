use aurora_types::{BootEpochId, LocalHandle};
use sha2::{Digest, Sha256};

use super::{
    WORKFLOW_TRACE_FILE_HEADER_SIZE, WORKFLOW_TRACE_RECORD_SIZE, WorkflowTraceCodecError,
    WorkflowTraceFileHeader, WorkflowTraceFileView, WorkflowTraceRecordBytes,
};
use crate::{
    CommitSequence, EventSequence, FaultReason, ReleaseSequence, TaskEpoch,
    WorkflowTraceContractError, WorkflowTraceEventKind, WorkflowTraceRecord,
    WorkflowTraceValueFragment, WorkflowTraceVersion,
};

#[test]
fn fixed_header_and_record_round_trip_at_exact_offsets() -> Result<(), Box<dyn std::error::Error>> {
    let epoch = epoch(0x98)?;
    let digest = [0xa5; 32];
    let header = WorkflowTraceFileHeader::new(epoch, digest, 2, 0);
    let encoded_header = header.encode();
    assert_eq!(&encoded_header[..8], b"AURWFT01");
    assert_eq!(&encoded_header[24..40], &epoch.to_bytes());
    assert_eq!(&encoded_header[40..72], &digest);
    assert_eq!(WorkflowTraceFileHeader::decode(&encoded_header), Ok(header));

    let initialized = initialized_record(epoch, 0, 0, 0)?;
    let record = terminal_record(epoch, WorkflowTraceEventKind::ScanCommitted, 1)?;
    let encoded = WorkflowTraceRecordBytes::encode(record);
    assert_eq!(&encoded.as_bytes()[..8], b"AURWFR01");
    assert_eq!(encoded.decode(), Ok(record));

    let mut file = Vec::from(encoded_header);
    file.extend_from_slice(WorkflowTraceRecordBytes::encode(initialized).as_bytes());
    file.extend_from_slice(encoded.as_bytes());
    let view = WorkflowTraceFileView::parse(&file)?;
    assert_eq!(view.header(), header);
    assert!(view.completeness().is_complete());
    assert_eq!(view.record(0), Ok(initialized));
    assert_eq!(view.record(1), Ok(record));
    assert_eq!(view.records().len(), 2);
    Ok(())
}

#[test]
fn every_event_kind_and_detail_boundary_is_explicit() -> Result<(), Box<dyn std::error::Error>> {
    let epoch = epoch(0x98)?;
    let kinds = [
        (WorkflowTraceEventKind::WorkflowInitialized, 0),
        (WorkflowTraceEventKind::NodeExecuted, 0),
        (WorkflowTraceEventKind::TransitionTaken, 0),
        (WorkflowTraceEventKind::ForkActivated, 0),
        (WorkflowTraceEventKind::JoinSatisfied, 3),
        (WorkflowTraceEventKind::WaitObserved, 6),
        (WorkflowTraceEventKind::CancelRequested, 2),
        (WorkflowTraceEventKind::CancelApplied, 2),
        (WorkflowTraceEventKind::SubworkflowActivated, 0),
        (WorkflowTraceEventKind::SubworkflowCompleted, 0),
        (WorkflowTraceEventKind::OutputStaged, 2),
        (WorkflowTraceEventKind::WatchedValue, 0),
        (WorkflowTraceEventKind::CompletionRequested, 0),
        (WorkflowTraceEventKind::WorkflowCompleted, 0),
        (WorkflowTraceEventKind::WorkflowFaulted, 0),
        (WorkflowTraceEventKind::ForceObserved, 3),
        (WorkflowTraceEventKind::FallbackObserved, 3),
        (WorkflowTraceEventKind::DeadlineObserved, 4),
        (WorkflowTraceEventKind::ScanCommitted, 0),
        (WorkflowTraceEventKind::ScanDiscarded, 0),
    ];
    for (kind, detail) in kinds {
        assert_eq!(WorkflowTraceEventKind::try_from(kind as u16), Ok(kind));
        let record = valid_event_record(epoch, kind, detail)?;
        assert_eq!(
            WorkflowTraceRecordBytes::encode(record).decode(),
            Ok(record)
        );
    }
    let record = valid_event_record(epoch, WorkflowTraceEventKind::OutputStaged, 1)?;
    assert_eq!(
        WorkflowTraceRecordBytes::encode(record).decode(),
        Ok(record)
    );
    assert_eq!(
        WorkflowTraceEventKind::try_from(0),
        Err(WorkflowTraceContractError::InvalidEventKind)
    );
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "每个事件形状的 required/forbidden 字段按短路顺序逐项覆盖，防止 reader 接受多生成或漏生成字段"
)]
fn every_event_shape_rejects_each_missing_or_extra_field() -> Result<(), Box<dyn std::error::Error>>
{
    let epoch = epoch(0x98)?;
    let task_epoch = TaskEpoch::new(1)?;
    macro_rules! reject {
        ($kind:expr, $detail:expr, $field:ident, $value:expr) => {{
            let mut fields = valid_event_fields($kind, $detail);
            fields.$field = $value;
            assert_eq!(
                record_from_fields(epoch, task_epoch, $kind, $detail, fields),
                Err(WorkflowTraceContractError::InvalidEventShape)
            );
        }};
    }
    macro_rules! reject_value {
        ($kind:expr, $detail:expr) => {{
            let mut fields = valid_event_fields($kind, $detail);
            fields.value = Some(0);
            fields.value_type = Some(0);
            assert_eq!(
                record_from_fields(epoch, task_epoch, $kind, $detail, fields),
                Err(WorkflowTraceContractError::InvalidEventShape)
            );
        }};
    }

    let no_field_kind = WorkflowTraceEventKind::WorkflowInitialized;
    reject!(no_field_kind, 0, node, Some(0));
    reject!(no_field_kind, 0, edge, Some(0));
    reject!(no_field_kind, 0, source, Some(0));
    reject!(no_field_kind, 0, branch, Some(0));
    reject!(no_field_kind, 0, execution, Some(0));
    reject_value!(no_field_kind, 0);

    let node_kind = WorkflowTraceEventKind::NodeExecuted;
    reject!(node_kind, 0, node, None);
    reject!(node_kind, 0, execution, None);
    reject!(node_kind, 0, edge, Some(0));
    reject!(node_kind, 0, source, Some(0));
    reject!(node_kind, 0, branch, Some(0));
    reject_value!(node_kind, 0);

    let transition = WorkflowTraceEventKind::TransitionTaken;
    reject!(transition, 0, node, None);
    reject!(transition, 0, edge, None);
    reject!(transition, 0, execution, None);
    reject!(transition, 0, source, Some(0));
    reject!(transition, 0, branch, Some(0));
    reject_value!(transition, 0);

    let fork = WorkflowTraceEventKind::ForkActivated;
    reject!(fork, 0, node, None);
    reject!(fork, 0, edge, None);
    reject!(fork, 0, branch, None);
    reject!(fork, 0, execution, None);
    reject!(fork, 0, source, Some(0));
    reject_value!(fork, 0);

    let join = WorkflowTraceEventKind::JoinSatisfied;
    reject!(join, 1, node, None);
    reject!(join, 1, execution, None);
    reject!(join, 1, edge, Some(0));
    reject!(join, 1, source, Some(0));
    reject_value!(join, 1);

    let cancel = WorkflowTraceEventKind::CancelRequested;
    reject!(cancel, 1, node, None);
    reject!(cancel, 1, branch, None);
    reject!(cancel, 1, execution, None);
    reject!(cancel, 1, edge, Some(0));
    reject!(cancel, 1, source, Some(0));
    reject_value!(cancel, 1);

    let child = WorkflowTraceEventKind::SubworkflowActivated;
    reject!(child, 0, node, None);
    reject!(child, 0, source, None);
    reject!(child, 0, execution, None);
    reject!(child, 0, edge, Some(0));
    reject!(child, 0, branch, Some(0));
    reject_value!(child, 0);

    let output = WorkflowTraceEventKind::OutputStaged;
    reject!(output, 2, node, None);
    reject!(output, 2, source, None);
    let mut missing_value = valid_event_fields(output, 2);
    missing_value.value = None;
    missing_value.value_type = None;
    assert_eq!(
        record_from_fields(epoch, task_epoch, output, 2, missing_value),
        Err(WorkflowTraceContractError::InvalidEventShape)
    );
    reject!(output, 2, execution, None);
    reject!(output, 2, edge, Some(0));
    reject!(output, 2, branch, Some(0));

    let watch = WorkflowTraceEventKind::WatchedValue;
    let mut missing_watch = valid_event_fields(watch, 0);
    missing_watch.value = None;
    missing_watch.value_type = None;
    missing_watch.fragment = WorkflowTraceValueFragment::ABSENT;
    assert_eq!(
        record_from_fields(epoch, task_epoch, watch, 0, missing_watch),
        Err(WorkflowTraceContractError::InvalidEventShape)
    );
    let mut missing_fragment = valid_event_fields(watch, 0);
    missing_fragment.fragment = WorkflowTraceValueFragment::ABSENT;
    assert_eq!(
        record_from_fields(epoch, task_epoch, watch, 0, missing_fragment),
        Err(WorkflowTraceContractError::InvalidEventShape)
    );
    reject!(watch, 0, node, Some(0));
    reject!(watch, 0, edge, Some(0));
    reject!(watch, 0, source, Some(0));
    reject!(watch, 0, branch, Some(0));
    reject!(watch, 0, execution, Some(0));

    let fault = WorkflowTraceEventKind::WorkflowFaulted;
    reject!(fault, 0, source, None);
    reject!(fault, 0, edge, Some(0));
    reject!(fault, 0, branch, Some(0));
    reject_value!(fault, 0);
    Ok(())
}

#[test]
fn constructor_rejects_every_fragment_and_detail_boundary() -> Result<(), Box<dyn std::error::Error>>
{
    let epoch = epoch(0x98)?;
    let task_epoch = TaskEpoch::new(1)?;
    for (kind, details) in [
        (WorkflowTraceEventKind::JoinSatisfied, [0, 4]),
        (WorkflowTraceEventKind::WaitObserved, [0, 7]),
        (WorkflowTraceEventKind::CancelRequested, [0, 3]),
        (WorkflowTraceEventKind::CancelApplied, [0, 3]),
        (WorkflowTraceEventKind::OutputStaged, [0, 3]),
        (WorkflowTraceEventKind::ForceObserved, [0, 4]),
        (WorkflowTraceEventKind::FallbackObserved, [0, 4]),
        (WorkflowTraceEventKind::DeadlineObserved, [0, 5]),
    ] {
        for detail in details {
            assert_eq!(
                record_from_fields(
                    epoch,
                    task_epoch,
                    kind,
                    detail,
                    valid_event_fields(kind, valid_detail(kind)),
                ),
                Err(WorkflowTraceContractError::InvalidEventDetail)
            );
        }
    }

    let valid_watch = valid_event_fields(WorkflowTraceEventKind::WatchedValue, 0);
    let mut fragments = Vec::new();
    let mut noncanonical_absent = WorkflowTraceValueFragment::ABSENT;
    noncanonical_absent.bytes = 1;
    fragments.push(noncanonical_absent);
    let mut no_digest = fragment(0, 1);
    no_digest.digest = None;
    fragments.push(no_digest);
    fragments.push(fragment(1, 1));
    let mut zero_bytes = fragment(0, 1);
    zero_bytes.bytes = 0;
    fragments.push(zero_bytes);
    let mut too_many_bytes = fragment(0, 1);
    too_many_bytes.bytes = 33;
    fragments.push(too_many_bytes);
    let mut short_nonfinal = fragment(0, 2);
    short_nonfinal.bytes = 31;
    fragments.push(short_nonfinal);
    let mut nonzero_tail = fragment(0, 1);
    nonzero_tail.bytes = 1;
    nonzero_tail.storage[1] = 1;
    fragments.push(nonzero_tail);
    for invalid in fragments {
        let mut fields = valid_watch;
        fields.fragment = invalid;
        assert_eq!(
            record_from_fields(
                epoch,
                task_epoch,
                WorkflowTraceEventKind::WatchedValue,
                0,
                fields,
            ),
            Err(WorkflowTraceContractError::InvalidFragment)
        );
    }
    let mut missing_handles = valid_watch;
    missing_handles.value = None;
    missing_handles.value_type = None;
    assert_eq!(
        record_from_fields(
            epoch,
            task_epoch,
            WorkflowTraceEventKind::WatchedValue,
            0,
            missing_handles,
        ),
        Err(WorkflowTraceContractError::InvalidFragment)
    );
    Ok(())
}

#[test]
fn codec_rejects_unknown_noncanonical_truncated_and_identity_inputs()
-> Result<(), Box<dyn std::error::Error>> {
    let engine_epoch = epoch(0x98)?;
    let record = value_record(engine_epoch, 0, 0, 1)?;
    let encoded = WorkflowTraceRecordBytes::encode(record);

    let mut unknown_version = *encoded.as_bytes();
    unknown_version[10] = 1;
    assert!(matches!(
        decode(&unknown_version),
        Err(WorkflowTraceCodecError::UnsupportedVersion { .. })
    ));
    let mut unknown_flags = *encoded.as_bytes();
    unknown_flags[15] |= 0x80;
    assert_eq!(
        decode(&unknown_flags),
        Err(WorkflowTraceCodecError::UnsupportedFlags)
    );
    let mut unknown_kind = *encoded.as_bytes();
    unknown_kind[16..18].copy_from_slice(&99_u16.to_le_bytes());
    assert!(matches!(
        decode(&unknown_kind),
        Err(WorkflowTraceCodecError::Contract(
            WorkflowTraceContractError::InvalidEventKind
        ))
    ));
    let mut zero_task_epoch = *encoded.as_bytes();
    zero_task_epoch[80..88].copy_from_slice(&0_u64.to_le_bytes());
    assert_eq!(
        decode(&zero_task_epoch),
        Err(WorkflowTraceCodecError::Contract(
            WorkflowTraceContractError::InvalidTaskEpoch
        ))
    );
    let mut bad_sentinel = *encoded.as_bytes();
    bad_sentinel[28..32].copy_from_slice(&0_u32.to_le_bytes());
    assert_eq!(
        decode(&bad_sentinel),
        Err(WorkflowTraceCodecError::NonCanonicalEncoding)
    );
    let mut reserved = *encoded.as_bytes();
    reserved[191] = 1;
    assert_eq!(
        decode(&reserved),
        Err(WorkflowTraceCodecError::NonCanonicalEncoding)
    );
    assert!(matches!(
        WorkflowTraceRecordBytes::from_slice(&encoded.as_bytes()[..191]),
        Err(WorkflowTraceCodecError::InvalidLength { .. })
    ));

    let mut file = Vec::from(WorkflowTraceFileHeader::new(epoch(0x99)?, [1; 32], 1, 0).encode());
    file.extend_from_slice(encoded.as_bytes());
    assert!(matches!(
        WorkflowTraceFileView::parse(&file),
        Err(WorkflowTraceCodecError::EngineEpochMismatch)
    ));
    file.push(0);
    assert!(matches!(
        WorkflowTraceFileView::parse(&file),
        Err(WorkflowTraceCodecError::InvalidLength { .. })
    ));
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "固定布局的每个独立拒绝分支都需要畸形输入样本，避免 reader 接受非规范数据"
)]
fn codec_rejects_every_header_and_record_layout_boundary() -> Result<(), Box<dyn std::error::Error>>
{
    let engine_epoch = epoch(0x98)?;
    let header = WorkflowTraceFileHeader::new(engine_epoch, [1; 32], 0, 0).encode();

    assert!(matches!(
        WorkflowTraceFileView::parse(&header[..WORKFLOW_TRACE_FILE_HEADER_SIZE - 1]),
        Err(WorkflowTraceCodecError::InvalidLength { .. })
    ));
    let mut bad_header = header;
    bad_header[0] = 0;
    assert_eq!(
        WorkflowTraceFileHeader::decode(&bad_header),
        Err(WorkflowTraceCodecError::InvalidMagic)
    );
    let mut bad_header = header;
    bad_header[8..10].copy_from_slice(&2_u16.to_le_bytes());
    assert!(matches!(
        WorkflowTraceFileHeader::decode(&bad_header),
        Err(WorkflowTraceCodecError::UnsupportedVersion { .. })
    ));
    for offset in [12, 14] {
        let mut bad_header = header;
        bad_header[offset..offset + 2].copy_from_slice(&0_u16.to_le_bytes());
        assert_eq!(
            WorkflowTraceFileHeader::decode(&bad_header),
            Err(WorkflowTraceCodecError::InvalidLayoutSize)
        );
    }
    let mut bad_header = header;
    bad_header[16..20].copy_from_slice(&1_u32.to_le_bytes());
    assert_eq!(
        WorkflowTraceFileHeader::decode(&bad_header),
        Err(WorkflowTraceCodecError::UnsupportedFlags)
    );
    for offset in [20, 88] {
        let mut bad_header = header;
        bad_header[offset] = 1;
        assert_eq!(
            WorkflowTraceFileHeader::decode(&bad_header),
            Err(WorkflowTraceCodecError::NonCanonicalEncoding)
        );
    }
    let mut bad_header = header;
    bad_header[24..40].fill(0);
    assert_eq!(
        WorkflowTraceFileHeader::decode(&bad_header),
        Err(WorkflowTraceCodecError::InvalidEngineEpoch)
    );
    let mut overflow_header = header;
    overflow_header[72..80].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(matches!(
        WorkflowTraceFileView::parse(&overflow_header),
        Err(WorkflowTraceCodecError::LengthOverflow)
    ));

    let initialized = initialized_record(engine_epoch, 0, 0, 0)?;
    let absent = *WorkflowTraceRecordBytes::encode(initialized).as_bytes();
    let watched = value_record(engine_epoch, 0, 0, 1)?;
    let present = *WorkflowTraceRecordBytes::encode(watched).as_bytes();

    let mut bad_record = present;
    bad_record[0] = 0;
    assert_eq!(
        decode(&bad_record),
        Err(WorkflowTraceCodecError::InvalidMagic)
    );
    let mut bad_record = present;
    bad_record[12..14].copy_from_slice(&0_u16.to_le_bytes());
    assert_eq!(
        decode(&bad_record),
        Err(WorkflowTraceCodecError::InvalidLayoutSize)
    );
    let mut bad_record = present;
    bad_record[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        decode(&bad_record),
        Err(WorkflowTraceCodecError::InvalidTaskHandle)
    );
    let mut bad_record = absent;
    bad_record[14..16].copy_from_slice(&1_u16.to_le_bytes());
    assert_eq!(
        decode(&bad_record),
        Err(WorkflowTraceCodecError::NonCanonicalEncoding)
    );
    let mut bad_record = present;
    bad_record[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(
        decode(&bad_record),
        Err(WorkflowTraceCodecError::NonCanonicalEncoding)
    );
    for offset in [40, 52] {
        let mut bad_record = absent;
        bad_record[offset..offset + 4].copy_from_slice(&0_u32.to_le_bytes());
        assert_eq!(
            decode(&bad_record),
            Err(WorkflowTraceCodecError::NonCanonicalEncoding)
        );
    }
    let mut bad_record = absent;
    bad_record[62..64].copy_from_slice(&1_u16.to_le_bytes());
    assert_eq!(
        decode(&bad_record),
        Err(WorkflowTraceCodecError::NonCanonicalEncoding)
    );
    let mut bad_record = absent;
    bad_record[14..16].copy_from_slice(&(1_u16 << 6).to_le_bytes());
    assert_eq!(
        decode(&bad_record),
        Err(WorkflowTraceCodecError::Contract(
            WorkflowTraceContractError::InvalidFaultPresence
        ))
    );
    let mut bad_record = absent;
    bad_record[120] = 1;
    assert_eq!(
        decode(&bad_record),
        Err(WorkflowTraceCodecError::NonCanonicalEncoding)
    );
    for offset in [56, 152] {
        let mut bad_record = absent;
        bad_record[offset] = 1;
        assert_eq!(
            decode(&bad_record),
            Err(WorkflowTraceCodecError::NonCanonicalEncoding)
        );
    }
    let mut bad_record = absent;
    bad_record[14..16].copy_from_slice(&(1_u16 << 7).to_le_bytes());
    bad_record[120] = 1;
    assert_eq!(
        decode(&bad_record),
        Err(WorkflowTraceCodecError::NonCanonicalEncoding)
    );

    let valid_file = file(
        engine_epoch,
        0,
        &[
            initialized,
            terminal_record(engine_epoch, WorkflowTraceEventKind::ScanCommitted, 1)?,
        ],
    );
    let view = WorkflowTraceFileView::parse(&valid_file)?;
    assert_eq!(
        view.record(2),
        Err(WorkflowTraceCodecError::RecordIndexOutOfRange)
    );
    Ok(())
}

#[test]
fn fragment_groups_are_contiguous_and_gaps_are_never_hidden()
-> Result<(), Box<dyn std::error::Error>> {
    let epoch = epoch(0x98)?;
    let initialized = initialized_record(epoch, 0, 0, 0)?;
    let first = value_record(epoch, 1, 0, 2)?;
    let second = value_record(epoch, 2, 1, 2)?;
    let terminal = terminal_record(epoch, WorkflowTraceEventKind::ScanCommitted, 3)?;
    let complete_file = file(epoch, 0, &[initialized, first, second, terminal]);
    assert!(
        WorkflowTraceFileView::parse(&complete_file)?
            .completeness()
            .is_complete()
    );

    let noncontiguous = file(epoch, 1, &[first, value_record(epoch, 3, 1, 2)?]);
    assert!(matches!(
        WorkflowTraceFileView::parse(&noncontiguous),
        Err(WorkflowTraceCodecError::InvalidFragmentSequence)
    ));
    let nonzero_start = file(epoch, 1, &[value_record(epoch, 0, 1, 2)?]);
    assert!(matches!(
        WorkflowTraceFileView::parse(&nonzero_start),
        Err(WorkflowTraceCodecError::InvalidFragmentSequence)
    ));
    let unfinished = file(epoch, 1, &[first]);
    assert!(matches!(
        WorkflowTraceFileView::parse(&unfinished),
        Err(WorkflowTraceCodecError::InvalidFragmentSequence)
    ));

    let valid_single = value_record(epoch, 0, 0, 1)?;
    let mut corrupted_fragment = fragment(0, 1);
    corrupted_fragment.storage[0] = 1;
    let corrupted = value_record_with_fragment(epoch, 0, corrupted_fragment)?;
    assert!(WorkflowTraceFileView::parse(&file(epoch, 1, &[valid_single])).is_ok());
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(epoch, 1, &[corrupted])),
        Err(WorkflowTraceCodecError::InvalidFragmentSequence)
    ));

    let plain_zero = plain_record(epoch, 0)?;
    let plain_two = terminal_record(epoch, WorkflowTraceEventKind::ScanDiscarded, 2)?;
    let incomplete = file(epoch, 1, &[plain_zero, plain_two]);
    let summary = WorkflowTraceFileView::parse(&incomplete)?.completeness();
    assert_eq!(summary.observed_sequence_gaps, 1);
    assert_eq!(summary.dropped_records, 1);
    assert!(!summary.is_complete());
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(epoch, 0, &[plain_zero, plain_two])),
        Err(WorkflowTraceCodecError::DroppedRecordsUnderflow)
    ));
    Ok(())
}

#[test]
fn initialization_is_exactly_once_on_the_first_complete_release()
-> Result<(), Box<dyn std::error::Error>> {
    let epoch = epoch(0x98)?;
    let missing = file(
        epoch,
        0,
        &[terminal_record(
            epoch,
            WorkflowTraceEventKind::ScanCommitted,
            0,
        )?],
    );
    assert!(matches!(
        WorkflowTraceFileView::parse(&missing),
        Err(WorkflowTraceCodecError::InvalidInitializationLifecycle)
    ));

    let duplicate = file(
        epoch,
        0,
        &[
            initialized_record(epoch, 0, 0, 0)?,
            terminal_record(epoch, WorkflowTraceEventKind::ScanCommitted, 1)?,
            initialized_record(epoch, 2, 1, 1)?,
            terminal_record_with(epoch, WorkflowTraceEventKind::ScanDiscarded, 3, 1, 1)?,
        ],
    );
    assert!(matches!(
        WorkflowTraceFileView::parse(&duplicate),
        Err(WorkflowTraceCodecError::InvalidInitializationLifecycle)
    ));
    Ok(())
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "构造器拒绝矩阵集中覆盖互相独立的语义边界"
)]
fn constructors_reject_commit_detail_fault_fragment_and_reserved_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    let epoch = epoch(0x98)?;
    let task_epoch = TaskEpoch::new(1)?;
    let base = |kind: WorkflowTraceEventKind,
                detail: u16,
                instance: u32,
                fault: Option<FaultReason>,
                after: CommitSequence,
                fragment: WorkflowTraceValueFragment| {
        WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            kind,
            detail,
            LocalHandle::ZERO,
            instance,
            None,
            None,
            None,
            fragment.is_present().then_some(0),
            None,
            None,
            fragment.is_present().then_some(0),
            fault,
            epoch,
            task_epoch,
            EventSequence::ZERO,
            ReleaseSequence::ZERO,
            CommitSequence::new(4),
            after,
            fragment,
        )
    };
    assert_eq!(
        base(
            WorkflowTraceEventKind::NodeExecuted,
            1,
            0,
            None,
            CommitSequence::new(4),
            WorkflowTraceValueFragment::ABSENT
        ),
        Err(WorkflowTraceContractError::InvalidEventDetail)
    );
    assert_eq!(
        base(
            WorkflowTraceEventKind::NodeExecuted,
            0,
            u32::MAX,
            None,
            CommitSequence::new(4),
            WorkflowTraceValueFragment::ABSENT
        ),
        Err(WorkflowTraceContractError::ReservedHandle)
    );
    assert_eq!(
        base(
            WorkflowTraceEventKind::ScanCommitted,
            0,
            0,
            None,
            CommitSequence::new(4),
            WorkflowTraceValueFragment::ABSENT
        ),
        Err(WorkflowTraceContractError::InvalidCommitTransition)
    );
    assert_eq!(
        base(
            WorkflowTraceEventKind::WorkflowFaulted,
            0,
            0,
            None,
            CommitSequence::new(4),
            WorkflowTraceValueFragment::ABSENT
        ),
        Err(WorkflowTraceContractError::InvalidFaultPresence)
    );
    let mut bad_fragment = fragment(0, 1);
    bad_fragment.bytes = 0;
    assert_eq!(
        base(
            WorkflowTraceEventKind::WatchedValue,
            0,
            0,
            None,
            CommitSequence::new(4),
            bad_fragment
        ),
        Err(WorkflowTraceContractError::InvalidFragment)
    );
    assert_eq!(
        base(
            WorkflowTraceEventKind::NodeExecuted,
            0,
            0,
            None,
            CommitSequence::new(4),
            WorkflowTraceValueFragment::ABSENT
        ),
        Err(WorkflowTraceContractError::InvalidEventShape)
    );
    for (detail, value_fragment) in [(1, WorkflowTraceValueFragment::ABSENT), (2, fragment(0, 1))] {
        assert_eq!(
            WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::OutputStaged,
                detail,
                LocalHandle::ZERO,
                0,
                Some(0),
                None,
                Some(0),
                Some(0),
                None,
                Some(0),
                Some(0),
                None,
                epoch,
                task_epoch,
                EventSequence::ZERO,
                ReleaseSequence::ZERO,
                CommitSequence::new(4),
                CommitSequence::new(4),
                value_fragment,
            ),
            Err(WorkflowTraceContractError::InvalidEventShape)
        );
    }
    assert_eq!(
        base(
            WorkflowTraceEventKind::WorkflowFaulted,
            0,
            0,
            Some(FaultReason::TaskExecutionFault),
            CommitSequence::new(4),
            WorkflowTraceValueFragment::ABSENT
        ),
        Err(WorkflowTraceContractError::InvalidEventShape)
    );
    assert_eq!(
        WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::WorkflowFaulted,
            0,
            LocalHandle::ZERO,
            0,
            Some(0),
            Some(0),
            Some(0),
            None,
            None,
            Some(0),
            None,
            Some(FaultReason::TaskExecutionFault),
            epoch,
            task_epoch,
            EventSequence::ZERO,
            ReleaseSequence::ZERO,
            CommitSequence::new(4),
            CommitSequence::new(4),
            WorkflowTraceValueFragment::ABSENT,
        ),
        Err(WorkflowTraceContractError::InvalidEventShape)
    );
    assert_eq!(
        WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::ScanDiscarded,
            0,
            LocalHandle::ZERO,
            0,
            Some(0),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            task_epoch,
            EventSequence::ZERO,
            ReleaseSequence::ZERO,
            CommitSequence::new(4),
            CommitSequence::new(4),
            WorkflowTraceValueFragment::ABSENT,
        ),
        Err(WorkflowTraceContractError::InvalidEventShape)
    );
    Ok(())
}

#[test]
fn release_groups_require_canonical_order_terminal_and_commit_chain()
-> Result<(), Box<dyn std::error::Error>> {
    let epoch = epoch(0x98)?;
    let node = plain_record_with(epoch, 0, 0, 0)?;
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(epoch, 0, &[node])),
        Err(WorkflowTraceCodecError::InvalidReleaseTerminal)
    ));
    let incomplete_file = file(epoch, 1, &[node]);
    let incomplete = WorkflowTraceFileView::parse(&incomplete_file)?;
    assert!(!incomplete.completeness().is_complete());

    let terminal = terminal_record_with(epoch, WorkflowTraceEventKind::ScanCommitted, 0, 0, 0)?;
    let after_terminal = plain_record_with(epoch, 1, 0, 0)?;
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(epoch, 0, &[terminal, after_terminal])),
        Err(WorkflowTraceCodecError::InvalidReleaseTerminal)
    ));

    let watch = value_record(epoch, 0, 0, 1)?;
    let later_node = plain_record_with(epoch, 1, 0, 0)?;
    let last = terminal_record_with(epoch, WorkflowTraceEventKind::ScanCommitted, 2, 0, 0)?;
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(epoch, 0, &[watch, later_node, last])),
        Err(WorkflowTraceCodecError::InvalidEventOrder)
    ));

    let first_release =
        terminal_record_with(epoch, WorkflowTraceEventKind::ScanCommitted, 0, 0, 0)?;
    let wrong_next = terminal_record_with(epoch, WorkflowTraceEventKind::ScanCommitted, 1, 1, 0)?;
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(epoch, 0, &[first_release, wrong_next])),
        Err(WorkflowTraceCodecError::InvalidCommitChain)
    ));

    let new_release_before_terminal =
        terminal_record_with(epoch, WorkflowTraceEventKind::ScanDiscarded, 1, 1, 0)?;
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(epoch, 0, &[node, new_release_before_terminal])),
        Err(WorkflowTraceCodecError::InvalidReleaseTerminal)
    ));

    let release_zero = terminal_record_with(epoch, WorkflowTraceEventKind::ScanDiscarded, 0, 0, 0)?;
    let release_one = terminal_record_with(epoch, WorkflowTraceEventKind::ScanDiscarded, 1, 1, 0)?;
    let repeated_release_zero =
        terminal_record_with(epoch, WorkflowTraceEventKind::ScanDiscarded, 2, 0, 0)?;
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(
            epoch,
            0,
            &[release_zero, release_one, repeated_release_zero]
        )),
        Err(WorkflowTraceCodecError::InvalidReleaseIdentity)
    ));
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(epoch, 0, &[release_one, repeated_release_zero])),
        Err(WorkflowTraceCodecError::InvalidInitializationLifecycle)
    ));

    let initialized = initialized_record(epoch, 0, 0, 0)?;
    let wrong_intra_release_commit = plain_record_with(epoch, 1, 0, 1)?;
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(epoch, 0, &[initialized, wrong_intra_release_commit])),
        Err(WorkflowTraceCodecError::InvalidCommitChain)
    ));
    let duplicate_sequence = initialized_record(epoch, 0, 0, 0)?;
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(epoch, 0, &[initialized, duplicate_sequence])),
        Err(WorkflowTraceCodecError::EventSequenceRegression)
    ));
    let duplicate_initialization = initialized_record(epoch, 1, 0, 0)?;
    assert!(matches!(
        WorkflowTraceFileView::parse(&file(epoch, 0, &[initialized, duplicate_initialization])),
        Err(WorkflowTraceCodecError::InvalidEventOrder)
    ));
    Ok(())
}

fn decode(bytes: &[u8]) -> Result<WorkflowTraceRecord, WorkflowTraceCodecError> {
    WorkflowTraceRecordBytes::from_slice(bytes)?.decode()
}

#[derive(Clone, Copy)]
struct EventFields {
    node: Option<u32>,
    edge: Option<u32>,
    source: Option<u32>,
    value: Option<u32>,
    branch: Option<u32>,
    execution: Option<u32>,
    value_type: Option<u32>,
    fault: Option<FaultReason>,
    fragment: WorkflowTraceValueFragment,
}

impl EventFields {
    const EMPTY: Self = Self {
        node: None,
        edge: None,
        source: None,
        value: None,
        branch: None,
        execution: None,
        value_type: None,
        fault: None,
        fragment: WorkflowTraceValueFragment::ABSENT,
    };
}

const fn valid_detail(kind: WorkflowTraceEventKind) -> u16 {
    match kind {
        WorkflowTraceEventKind::JoinSatisfied
        | WorkflowTraceEventKind::WaitObserved
        | WorkflowTraceEventKind::CancelRequested
        | WorkflowTraceEventKind::CancelApplied
        | WorkflowTraceEventKind::OutputStaged
        | WorkflowTraceEventKind::ForceObserved
        | WorkflowTraceEventKind::FallbackObserved
        | WorkflowTraceEventKind::DeadlineObserved => 1,
        _ => 0,
    }
}

fn valid_event_fields(kind: WorkflowTraceEventKind, detail: u16) -> EventFields {
    let mut fields = EventFields::EMPTY;
    match kind {
        WorkflowTraceEventKind::NodeExecuted
        | WorkflowTraceEventKind::WaitObserved
        | WorkflowTraceEventKind::CompletionRequested => {
            fields.node = Some(0);
            fields.execution = Some(0);
        }
        WorkflowTraceEventKind::TransitionTaken => {
            fields.node = Some(0);
            fields.edge = Some(0);
            fields.execution = Some(0);
        }
        WorkflowTraceEventKind::ForkActivated => {
            fields.node = Some(0);
            fields.edge = Some(0);
            fields.branch = Some(0);
            fields.execution = Some(0);
        }
        WorkflowTraceEventKind::JoinSatisfied
        | WorkflowTraceEventKind::CancelRequested
        | WorkflowTraceEventKind::CancelApplied => {
            fields.node = Some(0);
            fields.branch = Some(0);
            fields.execution = Some(0);
        }
        WorkflowTraceEventKind::SubworkflowActivated
        | WorkflowTraceEventKind::SubworkflowCompleted => {
            fields.node = Some(0);
            fields.source = Some(0);
            fields.execution = Some(0);
        }
        WorkflowTraceEventKind::OutputStaged => {
            fields.node = Some(0);
            fields.source = Some(0);
            fields.value = Some(0);
            fields.execution = Some(0);
            fields.value_type = Some(0);
            if detail == 1 {
                fields.fragment = fragment(0, 1);
            }
        }
        WorkflowTraceEventKind::WatchedValue => {
            fields.value = Some(0);
            fields.value_type = Some(0);
            fields.fragment = fragment(0, 1);
        }
        WorkflowTraceEventKind::WorkflowFaulted => {
            fields.node = Some(0);
            fields.source = Some(0);
            fields.execution = Some(0);
            fields.fault = Some(FaultReason::TaskExecutionFault);
        }
        WorkflowTraceEventKind::WorkflowInitialized
        | WorkflowTraceEventKind::WorkflowCompleted
        | WorkflowTraceEventKind::ForceObserved
        | WorkflowTraceEventKind::FallbackObserved
        | WorkflowTraceEventKind::DeadlineObserved
        | WorkflowTraceEventKind::ScanCommitted
        | WorkflowTraceEventKind::ScanDiscarded => {}
    }
    fields
}

fn record_from_fields(
    epoch: BootEpochId,
    task_epoch: TaskEpoch,
    kind: WorkflowTraceEventKind,
    detail: u16,
    fields: EventFields,
) -> Result<WorkflowTraceRecord, WorkflowTraceContractError> {
    let commit_before = CommitSequence::new(4);
    let commit_after = if kind == WorkflowTraceEventKind::ScanCommitted {
        CommitSequence::new(5)
    } else {
        commit_before
    };
    WorkflowTraceRecord::new(
        WorkflowTraceVersion::V1_0,
        kind,
        detail,
        LocalHandle::ZERO,
        0,
        fields.node,
        fields.edge,
        fields.source,
        fields.value,
        fields.branch,
        fields.execution,
        fields.value_type,
        fields.fault,
        epoch,
        task_epoch,
        EventSequence::ZERO,
        ReleaseSequence::ZERO,
        commit_before,
        commit_after,
        fields.fragment,
    )
}

fn valid_event_record(
    epoch: BootEpochId,
    kind: WorkflowTraceEventKind,
    detail: u16,
) -> Result<WorkflowTraceRecord, Box<dyn std::error::Error>> {
    Ok(record_from_fields(
        epoch,
        TaskEpoch::new(1)?,
        kind,
        detail,
        valid_event_fields(kind, detail),
    )?)
}

fn file(epoch: BootEpochId, dropped: u64, records: &[WorkflowTraceRecord]) -> Vec<u8> {
    let mut bytes = Vec::from(
        WorkflowTraceFileHeader::new(epoch, [1; 32], records.len() as u64, dropped).encode(),
    );
    for record in records {
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(*record).as_bytes());
    }
    bytes
}

fn value_record(
    epoch: BootEpochId,
    sequence: u64,
    index: u16,
    count: u16,
) -> Result<WorkflowTraceRecord, Box<dyn std::error::Error>> {
    value_record_with_fragment(epoch, sequence, fragment(index, count))
}

fn value_record_with_fragment(
    epoch: BootEpochId,
    sequence: u64,
    fragment: WorkflowTraceValueFragment,
) -> Result<WorkflowTraceRecord, Box<dyn std::error::Error>> {
    Ok(WorkflowTraceRecord::new(
        WorkflowTraceVersion::V1_0,
        WorkflowTraceEventKind::WatchedValue,
        0,
        LocalHandle::ZERO,
        0,
        None,
        None,
        None,
        Some(3),
        None,
        None,
        Some(4),
        None,
        epoch,
        TaskEpoch::new(1)?,
        EventSequence::new(sequence),
        ReleaseSequence::ZERO,
        CommitSequence::ZERO,
        CommitSequence::ZERO,
        fragment,
    )?)
}

fn plain_record(
    epoch: BootEpochId,
    sequence: u64,
) -> Result<WorkflowTraceRecord, Box<dyn std::error::Error>> {
    plain_record_with(epoch, sequence, 0, 0)
}

fn plain_record_with(
    epoch: BootEpochId,
    sequence: u64,
    release: u64,
    commit_before: u64,
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
        ReleaseSequence::new(release),
        CommitSequence::new(commit_before),
        CommitSequence::new(commit_before),
        WorkflowTraceValueFragment::ABSENT,
    )?)
}

fn initialized_record(
    epoch: BootEpochId,
    sequence: u64,
    release: u64,
    commit_before: u64,
) -> Result<WorkflowTraceRecord, Box<dyn std::error::Error>> {
    Ok(WorkflowTraceRecord::new(
        WorkflowTraceVersion::V1_0,
        WorkflowTraceEventKind::WorkflowInitialized,
        0,
        LocalHandle::ZERO,
        0,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        epoch,
        TaskEpoch::new(1)?,
        EventSequence::new(sequence),
        ReleaseSequence::new(release),
        CommitSequence::new(commit_before),
        CommitSequence::new(commit_before),
        WorkflowTraceValueFragment::ABSENT,
    )?)
}

fn terminal_record(
    epoch: BootEpochId,
    kind: WorkflowTraceEventKind,
    sequence: u64,
) -> Result<WorkflowTraceRecord, Box<dyn std::error::Error>> {
    terminal_record_with(epoch, kind, sequence, 0, 0)
}

fn terminal_record_with(
    epoch: BootEpochId,
    kind: WorkflowTraceEventKind,
    sequence: u64,
    release: u64,
    commit_before: u64,
) -> Result<WorkflowTraceRecord, Box<dyn std::error::Error>> {
    let before = CommitSequence::new(commit_before);
    let after = if kind == WorkflowTraceEventKind::ScanCommitted {
        CommitSequence::new(1)
    } else {
        before
    };
    Ok(WorkflowTraceRecord::new(
        WorkflowTraceVersion::V1_0,
        kind,
        0,
        LocalHandle::ZERO,
        0,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        epoch,
        TaskEpoch::new(1)?,
        EventSequence::new(sequence),
        ReleaseSequence::new(release),
        before,
        after,
        WorkflowTraceValueFragment::ABSENT,
    )?)
}

fn fragment(index: u16, count: u16) -> WorkflowTraceValueFragment {
    let mut hasher = Sha256::new();
    for fragment_index in 0..count {
        let bytes = if fragment_index + 1 == count { 1 } else { 32 };
        hasher.update(&[0; 32][..bytes]);
    }
    WorkflowTraceValueFragment {
        index,
        count,
        bytes: if index + 1 == count { 1 } else { 32 },
        digest: Some(hasher.finalize().into()),
        storage: [0; 32],
    }
}

fn epoch(variant: u8) -> Result<BootEpochId, aurora_types::IdentifierError> {
    BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, variant, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
        0x39, 0x8f,
    ])
}

const _: [(); WORKFLOW_TRACE_FILE_HEADER_SIZE] = [(); 96];
const _: [(); WORKFLOW_TRACE_RECORD_SIZE] = [(); 192];
