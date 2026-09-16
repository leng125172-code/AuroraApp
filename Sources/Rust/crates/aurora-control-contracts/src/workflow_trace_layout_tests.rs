use aurora_types::{BootEpochId, LocalHandle};

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
    let header = WorkflowTraceFileHeader::new(epoch, digest, 1, 0);
    let encoded_header = header.encode();
    assert_eq!(&encoded_header[..8], b"AURWFT01");
    assert_eq!(&encoded_header[24..40], &epoch.to_bytes());
    assert_eq!(&encoded_header[40..72], &digest);
    assert_eq!(WorkflowTraceFileHeader::decode(&encoded_header), Ok(header));

    let record = terminal_record(epoch, WorkflowTraceEventKind::ScanCommitted, 0)?;
    let encoded = WorkflowTraceRecordBytes::encode(record);
    assert_eq!(&encoded.as_bytes()[..8], b"AURWFR01");
    assert_eq!(encoded.decode(), Ok(record));

    let mut file = Vec::from(encoded_header);
    file.extend_from_slice(encoded.as_bytes());
    let view = WorkflowTraceFileView::parse(&file)?;
    assert_eq!(view.header(), header);
    assert!(view.completeness().is_complete());
    assert_eq!(view.record(0), Ok(record));
    assert_eq!(view.records().len(), 1);
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
    for (kind, _detail) in kinds {
        assert_eq!(WorkflowTraceEventKind::try_from(kind as u16), Ok(kind));
    }
    let record = terminal_record(epoch, WorkflowTraceEventKind::ScanCommitted, 0)?;
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
fn fragment_groups_are_contiguous_and_gaps_are_never_hidden()
-> Result<(), Box<dyn std::error::Error>> {
    let epoch = epoch(0x98)?;
    let first = value_record(epoch, 0, 0, 2)?;
    let second = value_record(epoch, 1, 1, 2)?;
    let terminal = terminal_record(epoch, WorkflowTraceEventKind::ScanCommitted, 2)?;
    let complete_file = file(epoch, 0, &[first, second, terminal]);
    assert!(
        WorkflowTraceFileView::parse(&complete_file)?
            .completeness()
            .is_complete()
    );

    let noncontiguous = file(epoch, 1, &[first, value_record(epoch, 2, 1, 2)?]);
    assert!(matches!(
        WorkflowTraceFileView::parse(&noncontiguous),
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
    Ok(())
}

fn decode(bytes: &[u8]) -> Result<WorkflowTraceRecord, WorkflowTraceCodecError> {
    WorkflowTraceRecordBytes::from_slice(bytes)?.decode()
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
        fragment(index, count),
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
    WorkflowTraceValueFragment {
        index,
        count,
        bytes: if index + 1 == count { 1 } else { 32 },
        digest: Some([9; 32]),
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
