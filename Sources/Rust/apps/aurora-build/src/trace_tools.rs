//! Host-only R0 Trace validation, text decoding, and exact per-record comparison.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use aurora_control_contracts::{TraceFileView, TraceRecord};

use crate::error::{BuildError, BuildResult};

/// Decode one Trace file into deterministic, line-oriented diagnostic text.
pub(crate) fn decode_file(path: &Path) -> BuildResult<String> {
    let bytes = read(path)?;
    decode_bytes(path, &bytes)
}

/// Compare two Trace files exactly at the semantic record boundary.
pub(crate) fn compare_files(expected_path: &Path, actual_path: &Path) -> BuildResult<String> {
    let expected_bytes = read(expected_path)?;
    let actual_bytes = read(actual_path)?;
    compare_bytes(expected_path, &expected_bytes, actual_path, &actual_bytes)
}

fn decode_bytes(path: &Path, bytes: &[u8]) -> BuildResult<String> {
    let view = parse(path, bytes)?;
    let header = view.header();
    let mut output = format!(
        "engine_epoch={} records={} dropped={}",
        epoch_hex(header.engine_epoch()),
        header.record_count(),
        header.dropped_records()
    );
    for (index, record) in view.records().enumerate() {
        let record = record.map_err(|source| BuildError::Trace {
            path: path.to_path_buf(),
            source,
        })?;
        write!(output, "\n{index}: {}", format_record(&record))
            .map_err(|_| BuildError::Validation("cannot format decoded Trace record".to_owned()))?;
    }
    Ok(output)
}

fn compare_bytes(
    expected_path: &Path,
    expected_bytes: &[u8],
    actual_path: &Path,
    actual_bytes: &[u8],
) -> BuildResult<String> {
    let expected = parse(expected_path, expected_bytes)?;
    let actual = parse(actual_path, actual_bytes)?;
    validate_records(expected_path, expected)?;
    validate_records(actual_path, actual)?;
    if expected.header() != actual.header() {
        return Err(BuildError::Validation(format!(
            "Trace header mismatch: expected {:?}, actual {:?}",
            expected.header(),
            actual.header()
        )));
    }
    for index in 0..expected.header().record_count() {
        let expected_record = expected.record(index).map_err(|source| BuildError::Trace {
            path: expected_path.to_path_buf(),
            source,
        })?;
        let actual_record = actual.record(index).map_err(|source| BuildError::Trace {
            path: actual_path.to_path_buf(),
            source,
        })?;
        if expected_record != actual_record {
            return Err(BuildError::Validation(format!(
                "Trace record {index} mismatch: expected event {}, actual event {}",
                expected_record.event_sequence().get(),
                actual_record.event_sequence().get()
            )));
        }
    }
    Ok(format!(
        "Trace files match for {} records",
        expected.header().record_count()
    ))
}

fn read(path: &Path) -> BuildResult<Vec<u8>> {
    fs::read(path).map_err(|source| BuildError::Io {
        operation: "read Trace",
        path: path.to_path_buf(),
        source,
    })
}

fn parse<'bytes>(path: &Path, bytes: &'bytes [u8]) -> BuildResult<TraceFileView<'bytes>> {
    TraceFileView::parse(bytes).map_err(|source| BuildError::Trace {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_records(path: &Path, view: TraceFileView<'_>) -> BuildResult<()> {
    for record in view.records() {
        record.map_err(|source| BuildError::Trace {
            path: path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

fn epoch_hex(epoch: aurora_types::BootEpochId) -> String {
    let mut output = String::with_capacity(32);
    for byte in epoch.to_bytes() {
        let _result = write!(output, "{byte:02x}");
    }
    output
}

fn format_record(record: &TraceRecord) -> String {
    let timing = record.timing();
    let started = timing
        .started_at()
        .map_or_else(|| "-".to_owned(), |value| value.elapsed_nanos().to_string());
    let finished = timing
        .finished_at()
        .map_or_else(|| "-".to_owned(), |value| value.elapsed_nanos().to_string());
    let elapsed = timing
        .execution_elapsed_nanos()
        .map_or_else(|| "-".to_owned(), |value| value.to_string());
    let counters = record.counters();
    format!(
        "event={} kind={:?} task={} epoch={} release={} commit={}->{} schedule={} start={} finish={} deadline={} elapsed={} state={:?}->{:?} miss={:?} fault={:?} fallback={:?} skipped={:?} input={:?} output={:?} utc={:?} ring={}/{}/{} attempted={} published={} dropped={} full={} saturated={}",
        record.event_sequence().get(),
        record.kind(),
        record.task_handle().get(),
        record.task_epoch().get(),
        record.release_sequence().get(),
        record.commit_before().get(),
        record.commit_after().get(),
        timing.scheduled_release().elapsed_nanos(),
        started,
        finished,
        timing.absolute_deadline().elapsed_nanos(),
        elapsed,
        record.state_before(),
        record.state_after(),
        record.miss(),
        record.fault(),
        record.fallback_request(),
        record.skipped_releases(),
        record.input_snapshot(),
        record.output_snapshot(),
        record.utc(),
        counters.occupancy(),
        counters.high_water_mark(),
        counters.capacity().get(),
        counters.attempted(),
        counters.published(),
        counters.dropped(),
        counters.full(),
        counters.counter_saturated(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use aurora_control_contracts::{
        CommitSequence, EventSequence, ExecutionContractVersion, ReleaseSequence,
        TRACE_FILE_HEADER_SIZE, TaskEpoch, TaskState, TraceCapacity, TraceCounters, TraceEventKind,
        TraceFileHeader, TraceRecord, TraceRecordBytes, TraceTiming,
    };
    use aurora_types::{BootEpochId, LocalHandle, MonotonicTimestamp};

    use super::{compare_bytes, decode_bytes};
    use crate::error::BuildError;

    #[test]
    fn empty_trace_decodes_and_compares_without_creating_records()
    -> Result<(), Box<dyn std::error::Error>> {
        let header = TraceFileHeader::new(epoch()?, 0, 3).encode();
        let path = Path::new("memory.trace");
        let decoded = decode_bytes(path, &header);
        assert!(decoded.is_ok());
        if let Ok(decoded) = decoded {
            assert!(decoded.contains("records=0 dropped=3"));
        }
        assert!(matches!(
            compare_bytes(path, &header, path, &header),
            Ok(message) if message == "Trace files match for 0 records"
        ));

        let mut changed = header;
        changed[48] = 4;
        assert!(compare_bytes(path, &header, path, &changed).is_err());
        assert!(decode_bytes(path, &header[..TRACE_FILE_HEADER_SIZE - 1]).is_err());

        let expected = one_record_file(epoch()?, 0)?;
        let actual = one_record_file(epoch()?, 1)?;
        assert!(matches!(
            compare_bytes(path, &expected, path, &expected),
            Ok(message) if message == "Trace files match for 1 records"
        ));
        assert!(compare_bytes(path, &expected, path, &actual).is_err());

        let mut malformed_actual = actual;
        malformed_actual[48] = 1;
        malformed_actual[TRACE_FILE_HEADER_SIZE] = 0;
        assert!(matches!(
            compare_bytes(path, &expected, path, &malformed_actual),
            Err(BuildError::Trace { .. })
        ));
        Ok(())
    }

    fn one_record_file(
        engine_epoch: BootEpochId,
        event_sequence: u64,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let capacity = TraceCapacity::new(2, 2)?;
        let timing = TraceTiming::new(
            MonotonicTimestamp::new(engine_epoch, 10),
            MonotonicTimestamp::new(engine_epoch, 20),
            None,
            None,
        )?;
        let record = TraceRecord::new(
            ExecutionContractVersion::V1_0,
            engine_epoch,
            LocalHandle::ZERO,
            TaskEpoch::new(1)?,
            EventSequence::new(event_sequence),
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
            TraceCounters::new(capacity, 0, 0, 1, 1, 0, 0, false)?,
        )?;
        let mut file = Vec::from(TraceFileHeader::new(engine_epoch, 1, 0).encode());
        file.extend_from_slice(TraceRecordBytes::encode(record).as_bytes());
        Ok(file)
    }

    fn epoch() -> Result<BootEpochId, aurora_types::IdentifierError> {
        BootEpochId::from_bytes([
            0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
            0x39, 0x8f,
        ])
    }
}
