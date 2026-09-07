//! R0 Trace 的固定宽度 little-endian 文件头和记录布局。

use aurora_types::{
    BootEpochId, LocalHandle, MonotonicTimestamp, TimeQuality, TimeQualityState, TimeSource,
    UtcTimestamp,
};
use thiserror::Error;

use crate::{
    CommitSequence, EventSequence, ExecutionContractError, ExecutionContractVersion,
    FallbackRequestSequence, FaultReason, MissOutcome, ReleaseSequence, TaskEpoch, TaskState,
    TraceCapacity, TraceCounters, TraceEventKind, TraceRecord, TraceSkippedReleases,
    TraceSnapshotEvidence, TraceTiming, UtcObservation,
};

/// Trace 文件头布局 major。
pub const TRACE_LAYOUT_MAJOR: u16 = 1;
/// Trace 文件头布局 minor。
pub const TRACE_LAYOUT_MINOR: u16 = 0;
/// Trace 文件头固定字节数。
pub const TRACE_FILE_HEADER_SIZE: usize = 64;
/// Trace record 固定字节数。
pub const TRACE_RECORD_SIZE: usize = 320;
const TRACE_FILE_HEADER_SIZE_U16: u16 = 64;
const TRACE_RECORD_SIZE_U16: u16 = 320;

const FILE_MAGIC: [u8; 8] = *b"AURTRC01";
const RECORD_MAGIC: [u8; 8] = *b"AURTRR01";
const FILE_FLAGS_ALLOWED: u32 = 0;
const RECORD_FLAGS_ALLOWED: u16 = 0x07ff;
const FLAG_EXECUTION: u16 = 1 << 0;
const FLAG_UTC: u16 = 1 << 1;
const FLAG_MAX_ERROR: u16 = 1 << 2;
const FLAG_LAST_SYNC: u16 = 1 << 3;
const FLAG_MISS: u16 = 1 << 4;
const FLAG_FAULT: u16 = 1 << 5;
const FLAG_FALLBACK: u16 = 1 << 6;
const FLAG_COUNTER_SATURATED: u16 = 1 << 7;
const FLAG_SKIPPED_RANGE: u16 = 1 << 8;
const FLAG_INPUT_SNAPSHOT: u16 = 1 << 9;
const FLAG_OUTPUT_SNAPSHOT: u16 = 1 << 10;

/// Trace 固定布局解析或规范编码错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TraceCodecError {
    /// 输入长度与固定布局不一致。
    #[error("Trace length is {actual} bytes, expected {expected}")]
    InvalidLength {
        /// 期望字节数。
        expected: usize,
        /// 实际字节数。
        actual: usize,
    },
    /// magic 不属于 R0 Trace 文件头或 record。
    #[error("invalid Trace magic")]
    InvalidMagic,
    /// major/minor 不受当前 reader 支持。
    #[error("unsupported Trace layout {major}.{minor}")]
    UnsupportedVersion {
        /// 输入 major。
        major: u16,
        /// 输入 minor。
        minor: u16,
    },
    /// 声明的 header/record 字节数不等于固定大小。
    #[error("invalid fixed Trace layout size")]
    InvalidLayoutSize,
    /// flags 包含未定义位。
    #[error("Trace flags contain unsupported bits")]
    UnsupportedFlags,
    /// reserved 或 absent 字段含非零字节，输入不是规范编码。
    #[error("Trace reserved or absent field is non-zero")]
    NonCanonicalEncoding,
    /// record count 的长度计算无法表示。
    #[error("Trace record count cannot be represented by this process")]
    LengthOverflow,
    /// `UUIDv7` engine epoch 无效。
    #[error("Trace engine epoch is not a valid UUIDv7")]
    InvalidEngineEpoch,
    /// task handle 无效。
    #[error("Trace task handle is invalid")]
    InvalidTaskHandle,
    /// UTC timestamp 或 `TimeQuality` 枚举无效。
    #[error("Trace UTC or TimeQuality value is invalid")]
    InvalidUtc,
    /// record 的 engine epoch 与文件头不一致。
    #[error("Trace record engine epoch does not match its file header")]
    EngineEpochMismatch,
    /// record index 超出文件头声明数量。
    #[error("Trace record index is out of range")]
    RecordIndexOutOfRange,
    /// 已解析字段违反 R0 execution contract。
    #[error("Trace record violates execution contract: {0}")]
    Contract(ExecutionContractError),
}

impl From<ExecutionContractError> for TraceCodecError {
    fn from(value: ExecutionContractError) -> Self {
        Self::Contract(value)
    }
}

/// 一个 Trace 文件的固定 64-byte header。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceFileHeader {
    engine_epoch: BootEpochId,
    record_count: u64,
    dropped_records: u64,
}

impl TraceFileHeader {
    /// 创建离线 Trace 文件头；计数是该文件实际包含和已知丢失的 record 数。
    #[must_use]
    pub const fn new(engine_epoch: BootEpochId, record_count: u64, dropped_records: u64) -> Self {
        Self {
            engine_epoch,
            record_count,
            dropped_records,
        }
    }

    /// 返回文件所属 Control Engine 启动 epoch。
    #[must_use]
    pub const fn engine_epoch(self) -> BootEpochId {
        self.engine_epoch
    }

    /// 返回文件实际包含的 record 数。
    #[must_use]
    pub const fn record_count(self) -> u64 {
        self.record_count
    }

    /// 返回导出时已知被 producer 丢弃的 record 数。
    #[must_use]
    pub const fn dropped_records(self) -> u64 {
        self.dropped_records
    }

    /// 编码为不依赖 Rust ABI padding 的固定 little-endian bytes。
    #[must_use]
    pub fn encode(self) -> [u8; TRACE_FILE_HEADER_SIZE] {
        let mut bytes = [0; TRACE_FILE_HEADER_SIZE];
        bytes[0..8].copy_from_slice(&FILE_MAGIC);
        put_u16(&mut bytes, 8, TRACE_LAYOUT_MAJOR);
        put_u16(&mut bytes, 10, TRACE_LAYOUT_MINOR);
        put_u16(&mut bytes, 12, TRACE_FILE_HEADER_SIZE_U16);
        put_u16(&mut bytes, 14, TRACE_RECORD_SIZE_U16);
        put_u32(&mut bytes, 16, FILE_FLAGS_ALLOWED);
        bytes[24..40].copy_from_slice(&self.engine_epoch.to_bytes());
        put_u64(&mut bytes, 40, self.record_count);
        put_u64(&mut bytes, 48, self.dropped_records);
        bytes
    }

    /// 从恰好 64 bytes 解码并拒绝未知版本、flags 和非零 reserved 字段。
    ///
    /// # Errors
    ///
    /// 长度、magic、版本、固定大小、reserved 字段或 `UUIDv7` 无效时拒绝。
    pub fn decode(bytes: &[u8]) -> Result<Self, TraceCodecError> {
        require_length(bytes, TRACE_FILE_HEADER_SIZE)?;
        if bytes[0..8] != FILE_MAGIC {
            return Err(TraceCodecError::InvalidMagic);
        }
        validate_version(bytes)?;
        if get_u16(bytes, 12) != TRACE_FILE_HEADER_SIZE_U16
            || get_u16(bytes, 14) != TRACE_RECORD_SIZE_U16
        {
            return Err(TraceCodecError::InvalidLayoutSize);
        }
        if get_u32(bytes, 16) != FILE_FLAGS_ALLOWED {
            return Err(TraceCodecError::UnsupportedFlags);
        }
        require_zero(&bytes[20..24])?;
        require_zero(&bytes[56..64])?;
        let engine_epoch = decode_epoch(&bytes[24..40])?;
        Ok(Self {
            engine_epoch,
            record_count: get_u64(bytes, 40),
            dropped_records: get_u64(bytes, 48),
        })
    }
}

/// 一个已经规范编码、可直接进入固定容量 ring 的 320-byte record。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceRecordBytes([u8; TRACE_RECORD_SIZE]);

impl TraceRecordBytes {
    /// 将已验证语义 record 编码为固定 little-endian bytes；不分配、不格式化。
    #[must_use]
    pub fn encode(record: TraceRecord) -> Self {
        let mut bytes = [0; TRACE_RECORD_SIZE];
        bytes[0..8].copy_from_slice(&RECORD_MAGIC);
        put_u16(&mut bytes, 8, TRACE_LAYOUT_MAJOR);
        put_u16(&mut bytes, 10, TRACE_LAYOUT_MINOR);
        put_u16(&mut bytes, 12, TRACE_RECORD_SIZE_U16);

        let mut flags = 0;
        let timing = record.timing();
        if let (Some(started), Some(finished)) = (timing.started_at(), timing.finished_at()) {
            flags |= FLAG_EXECUTION;
            put_u64(&mut bytes, 104, started.elapsed_nanos());
            put_u64(&mut bytes, 112, finished.elapsed_nanos());
            put_u64(
                &mut bytes,
                120,
                finished.elapsed_nanos() - started.elapsed_nanos(),
            );
        }
        if let Some(utc) = record.utc() {
            flags |= FLAG_UTC;
            let timestamp = utc.timestamp();
            put_i64(&mut bytes, 128, timestamp.seconds());
            put_u32(&mut bytes, 28, timestamp.nanos());
            let quality = utc.quality();
            bytes[22] = quality.state() as u8;
            bytes[23] = quality.source() as u8;
            if let Some(max_error) = quality.max_error_nanos() {
                flags |= FLAG_MAX_ERROR;
                put_u64(&mut bytes, 136, max_error);
            }
            if let Some(last_sync) = quality.last_sync_utc() {
                flags |= FLAG_LAST_SYNC;
                put_i64(&mut bytes, 144, last_sync.seconds());
                put_u32(&mut bytes, 152, last_sync.nanos());
            }
        }
        if let Some(miss) = record.miss() {
            flags |= FLAG_MISS;
            bytes[19] = miss as u8;
        }
        if let Some(fault) = record.fault() {
            flags |= FLAG_FAULT;
            put_u16(&mut bytes, 20, fault as u16);
        }
        if let Some(fallback) = record.fallback_request() {
            flags |= FLAG_FALLBACK;
            put_u64(&mut bytes, 160, fallback.get());
        }
        if record.counters().counter_saturated() {
            flags |= FLAG_COUNTER_SATURATED;
        }
        if let Some(skipped) = record.skipped_releases() {
            flags |= FLAG_SKIPPED_RANGE;
            put_u64(&mut bytes, 216, skipped.first().get());
            put_u64(&mut bytes, 224, skipped.last().get());
            put_u64(&mut bytes, 232, skipped.count());
        }
        if let Some(snapshot) = record.input_snapshot() {
            flags |= FLAG_INPUT_SNAPSHOT;
            encode_snapshot(&mut bytes, 240, snapshot);
        }
        if let Some(snapshot) = record.output_snapshot() {
            flags |= FLAG_OUTPUT_SNAPSHOT;
            encode_snapshot(&mut bytes, 272, snapshot);
        }
        put_u16(&mut bytes, 14, flags);
        bytes[16] = record.kind() as u8;
        bytes[17] = record.state_before() as u8;
        bytes[18] = record.state_after() as u8;
        put_u32(&mut bytes, 24, record.task_handle().get());
        bytes[32..48].copy_from_slice(&record.engine_epoch().to_bytes());
        put_u64(&mut bytes, 48, record.task_epoch().get());
        put_u64(&mut bytes, 56, record.event_sequence().get());
        put_u64(&mut bytes, 64, record.release_sequence().get());
        put_u64(&mut bytes, 72, record.commit_before().get());
        put_u64(&mut bytes, 80, record.commit_after().get());
        put_u64(&mut bytes, 88, timing.scheduled_release().elapsed_nanos());
        put_u64(&mut bytes, 96, timing.absolute_deadline().elapsed_nanos());
        let counters = record.counters();
        put_u32(&mut bytes, 168, counters.capacity().get());
        put_u32(&mut bytes, 172, counters.occupancy());
        put_u32(&mut bytes, 176, counters.high_water_mark());
        put_u64(&mut bytes, 184, counters.attempted());
        put_u64(&mut bytes, 192, counters.published());
        put_u64(&mut bytes, 200, counters.dropped());
        put_u64(&mut bytes, 208, counters.full());
        Self(bytes)
    }

    /// 从恰好 320 bytes 复制固定 record，不解释字段。
    ///
    /// # Errors
    ///
    /// 输入长度不等于 [`TRACE_RECORD_SIZE`] 时拒绝。
    pub fn from_slice(bytes: &[u8]) -> Result<Self, TraceCodecError> {
        require_length(bytes, TRACE_RECORD_SIZE)?;
        let mut fixed = [0; TRACE_RECORD_SIZE];
        fixed.copy_from_slice(bytes);
        Ok(Self(fixed))
    }

    /// 返回固定 record bytes。
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; TRACE_RECORD_SIZE] {
        &self.0
    }

    /// 验证并解码所有字段；不存在静默默认值或未知版本兼容。
    ///
    /// # Errors
    ///
    /// 任一布局、规范编码、枚举、时间、容量或契约约束失败时拒绝整个 record。
    pub fn decode(self) -> Result<TraceRecord, TraceCodecError> {
        decode_record(&self.0)
    }
}

/// 一个借用离线文件 bytes、按 header count 精确切片的只读 Trace view。
#[derive(Debug, Clone, Copy)]
pub struct TraceFileView<'bytes> {
    header: TraceFileHeader,
    bytes: &'bytes [u8],
    record_count: usize,
}

impl<'bytes> TraceFileView<'bytes> {
    /// 验证 header 以及 `64 + count * 320` 的精确文件长度。
    ///
    /// # Errors
    ///
    /// 截断、尾随字节、count 乘法溢出或 header 无效时拒绝。
    pub fn parse(bytes: &'bytes [u8]) -> Result<Self, TraceCodecError> {
        if bytes.len() < TRACE_FILE_HEADER_SIZE {
            return Err(TraceCodecError::InvalidLength {
                expected: TRACE_FILE_HEADER_SIZE,
                actual: bytes.len(),
            });
        }
        let header = TraceFileHeader::decode(&bytes[..TRACE_FILE_HEADER_SIZE])?;
        let count =
            usize::try_from(header.record_count()).map_err(|_| TraceCodecError::LengthOverflow)?;
        let records_size = count
            .checked_mul(TRACE_RECORD_SIZE)
            .ok_or(TraceCodecError::LengthOverflow)?;
        let expected = TRACE_FILE_HEADER_SIZE
            .checked_add(records_size)
            .ok_or(TraceCodecError::LengthOverflow)?;
        require_length(bytes, expected)?;
        Ok(Self {
            header,
            bytes,
            record_count: count,
        })
    }

    /// 返回已验证文件头。
    #[must_use]
    pub const fn header(self) -> TraceFileHeader {
        self.header
    }

    /// 按零基 index 解码一项 record，并校验其 engine epoch。
    ///
    /// # Errors
    ///
    /// index 越界、record 无效或 epoch 与 header 不一致时拒绝。
    pub fn record(self, index: u64) -> Result<TraceRecord, TraceCodecError> {
        if index >= self.header.record_count() {
            return Err(TraceCodecError::RecordIndexOutOfRange);
        }
        let index = usize::try_from(index).map_err(|_| TraceCodecError::LengthOverflow)?;
        self.record_at(index)
    }

    fn record_at(self, index: usize) -> Result<TraceRecord, TraceCodecError> {
        let start = TRACE_FILE_HEADER_SIZE + index * TRACE_RECORD_SIZE;
        let record = TraceRecordBytes::from_slice(&self.bytes[start..start + TRACE_RECORD_SIZE])?
            .decode()?;
        if record.engine_epoch() != self.header.engine_epoch() {
            return Err(TraceCodecError::EngineEpochMismatch);
        }
        Ok(record)
    }

    /// 按文件顺序创建不分配的 record iterator。
    #[must_use]
    pub const fn records(self) -> TraceRecordIterator<'bytes> {
        TraceRecordIterator {
            view: self,
            next_index: 0,
        }
    }
}

/// 离线 Trace view 的顺序只读 iterator。
#[derive(Debug)]
pub struct TraceRecordIterator<'bytes> {
    view: TraceFileView<'bytes>,
    next_index: usize,
}

impl Iterator for TraceRecordIterator<'_> {
    type Item = Result<TraceRecord, TraceCodecError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_index >= self.view.record_count {
            return None;
        }
        let index = self.next_index;
        self.next_index += 1;
        Some(self.view.record_at(index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.view.record_count - self.next_index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for TraceRecordIterator<'_> {}

fn decode_record(bytes: &[u8; TRACE_RECORD_SIZE]) -> Result<TraceRecord, TraceCodecError> {
    if bytes[0..8] != RECORD_MAGIC {
        return Err(TraceCodecError::InvalidMagic);
    }
    validate_version(bytes)?;
    if get_u16(bytes, 12) != TRACE_RECORD_SIZE_U16 {
        return Err(TraceCodecError::InvalidLayoutSize);
    }
    let flags = get_u16(bytes, 14);
    if flags & !RECORD_FLAGS_ALLOWED != 0 {
        return Err(TraceCodecError::UnsupportedFlags);
    }
    require_zero(&bytes[156..160])?;
    require_zero(&bytes[180..184])?;
    require_zero(&bytes[244..248])?;
    require_zero(&bytes[276..280])?;
    require_zero(&bytes[304..320])?;

    let engine_epoch = decode_epoch(&bytes[32..48])?;
    let task_handle =
        LocalHandle::new(get_u32(bytes, 24)).map_err(|_| TraceCodecError::InvalidTaskHandle)?;
    let task_epoch = TaskEpoch::new(get_u64(bytes, 48))?;
    let kind = TraceEventKind::try_from(bytes[16])?;
    let state_before = TaskState::try_from(bytes[17])?;
    let state_after = TaskState::try_from(bytes[18])?;
    let miss = decode_optional_u8(flags, FLAG_MISS, bytes[19], MissOutcome::try_from)?;
    let fault = decode_optional_u16(flags, FLAG_FAULT, get_u16(bytes, 20))?;
    let fallback_request = decode_optional_sequence(flags, FLAG_FALLBACK, get_u64(bytes, 160))?;

    let scheduled = MonotonicTimestamp::new(engine_epoch, get_u64(bytes, 88));
    let deadline = MonotonicTimestamp::new(engine_epoch, get_u64(bytes, 96));
    let execution_present = has_flag(flags, FLAG_EXECUTION);
    let (started, finished) = if execution_present {
        (
            Some(MonotonicTimestamp::new(engine_epoch, get_u64(bytes, 104))),
            Some(MonotonicTimestamp::new(engine_epoch, get_u64(bytes, 112))),
        )
    } else {
        require_zero(&bytes[104..128])?;
        (None, None)
    };
    let timing = TraceTiming::new(scheduled, deadline, started, finished)?;
    if execution_present && timing.execution_elapsed_nanos() != Some(get_u64(bytes, 120)) {
        return Err(TraceCodecError::NonCanonicalEncoding);
    }

    let utc = decode_utc(bytes, flags)?;
    let capacity = TraceCapacity::new(get_u32(bytes, 168), u32::MAX)?;
    let counters = TraceCounters::new(
        capacity,
        get_u32(bytes, 172),
        get_u32(bytes, 176),
        get_u64(bytes, 184),
        get_u64(bytes, 192),
        get_u64(bytes, 200),
        get_u64(bytes, 208),
        has_flag(flags, FLAG_COUNTER_SATURATED),
    )?;
    let skipped_releases = decode_skipped(bytes, flags)?;
    let input_snapshot = decode_snapshot(bytes, flags, FLAG_INPUT_SNAPSHOT, 240)?;
    let output_snapshot = decode_snapshot(bytes, flags, FLAG_OUTPUT_SNAPSHOT, 272)?;

    TraceRecord::new(
        ExecutionContractVersion::V1_0,
        engine_epoch,
        task_handle,
        task_epoch,
        EventSequence::new(get_u64(bytes, 56)),
        ReleaseSequence::new(get_u64(bytes, 64)),
        CommitSequence::new(get_u64(bytes, 72)),
        CommitSequence::new(get_u64(bytes, 80)),
        kind,
        timing,
        utc,
        state_before,
        state_after,
        miss,
        fault,
        fallback_request,
        skipped_releases,
        input_snapshot,
        output_snapshot,
        counters,
    )
    .map_err(Into::into)
}

fn decode_utc(
    bytes: &[u8; TRACE_RECORD_SIZE],
    flags: u16,
) -> Result<Option<UtcObservation>, TraceCodecError> {
    if !has_flag(flags, FLAG_UTC) {
        if has_flag(flags, FLAG_MAX_ERROR) || has_flag(flags, FLAG_LAST_SYNC) {
            return Err(TraceCodecError::NonCanonicalEncoding);
        }
        require_zero(&bytes[22..24])?;
        require_zero(&bytes[28..32])?;
        require_zero(&bytes[128..156])?;
        return Ok(None);
    }
    let timestamp = UtcTimestamp::new(get_i64(bytes, 128), get_u32(bytes, 28))
        .map_err(|_| TraceCodecError::InvalidUtc)?;
    let state = decode_time_state(bytes[22])?;
    let source = decode_time_source(bytes[23])?;
    let max_error = if has_flag(flags, FLAG_MAX_ERROR) {
        Some(get_u64(bytes, 136))
    } else {
        require_zero(&bytes[136..144])?;
        None
    };
    let last_sync = if has_flag(flags, FLAG_LAST_SYNC) {
        Some(
            UtcTimestamp::new(get_i64(bytes, 144), get_u32(bytes, 152))
                .map_err(|_| TraceCodecError::InvalidUtc)?,
        )
    } else {
        require_zero(&bytes[144..156])?;
        None
    };
    Ok(Some(UtcObservation::new(
        timestamp,
        TimeQuality::new(state, source, max_error, last_sync),
    )))
}

fn decode_skipped(
    bytes: &[u8; TRACE_RECORD_SIZE],
    flags: u16,
) -> Result<Option<TraceSkippedReleases>, TraceCodecError> {
    if has_flag(flags, FLAG_SKIPPED_RANGE) {
        Ok(Some(TraceSkippedReleases::new(
            ReleaseSequence::new(get_u64(bytes, 216)),
            ReleaseSequence::new(get_u64(bytes, 224)),
            get_u64(bytes, 232),
        )?))
    } else {
        require_zero(&bytes[216..240])?;
        Ok(None)
    }
}

fn encode_snapshot(
    bytes: &mut [u8; TRACE_RECORD_SIZE],
    offset: usize,
    snapshot: TraceSnapshotEvidence,
) {
    put_u32(bytes, offset, snapshot.source_task().get());
    put_u64(bytes, offset + 8, snapshot.source_task_epoch().get());
    put_u64(bytes, offset + 16, snapshot.commit_sequence().get());
    put_u64(bytes, offset + 24, snapshot.missed_commits());
}

fn decode_snapshot(
    bytes: &[u8; TRACE_RECORD_SIZE],
    flags: u16,
    flag: u16,
    offset: usize,
) -> Result<Option<TraceSnapshotEvidence>, TraceCodecError> {
    if !has_flag(flags, flag) {
        require_zero(&bytes[offset..offset + 32])?;
        return Ok(None);
    }
    let source_task =
        LocalHandle::new(get_u32(bytes, offset)).map_err(|_| TraceCodecError::InvalidTaskHandle)?;
    let source_task_epoch = TaskEpoch::new(get_u64(bytes, offset + 8))?;
    Ok(Some(TraceSnapshotEvidence::new(
        source_task,
        source_task_epoch,
        CommitSequence::new(get_u64(bytes, offset + 16)),
        get_u64(bytes, offset + 24),
    )?))
}

fn decode_optional_u8<T>(
    flags: u16,
    flag: u16,
    raw: u8,
    decode: impl FnOnce(u8) -> Result<T, ExecutionContractError>,
) -> Result<Option<T>, TraceCodecError> {
    if has_flag(flags, flag) {
        decode(raw).map(Some).map_err(Into::into)
    } else if raw == 0 {
        Ok(None)
    } else {
        Err(TraceCodecError::NonCanonicalEncoding)
    }
}

fn decode_optional_u16(
    flags: u16,
    flag: u16,
    raw: u16,
) -> Result<Option<FaultReason>, TraceCodecError> {
    if has_flag(flags, flag) {
        FaultReason::try_from(raw).map(Some).map_err(Into::into)
    } else if raw == 0 {
        Ok(None)
    } else {
        Err(TraceCodecError::NonCanonicalEncoding)
    }
}

fn decode_optional_sequence(
    flags: u16,
    flag: u16,
    raw: u64,
) -> Result<Option<FallbackRequestSequence>, TraceCodecError> {
    if has_flag(flags, flag) {
        Ok(Some(FallbackRequestSequence::new(raw)))
    } else if raw == 0 {
        Ok(None)
    } else {
        Err(TraceCodecError::NonCanonicalEncoding)
    }
}

fn decode_time_state(raw: u8) -> Result<TimeQualityState, TraceCodecError> {
    match raw {
        0 => Ok(TimeQualityState::Unknown),
        1 => Ok(TimeQualityState::Synchronizing),
        2 => Ok(TimeQualityState::Good),
        3 => Ok(TimeQualityState::Holdover),
        4 => Ok(TimeQualityState::Degraded),
        5 => Ok(TimeQualityState::Invalid),
        _ => Err(TraceCodecError::InvalidUtc),
    }
}

fn decode_time_source(raw: u8) -> Result<TimeSource, TraceCodecError> {
    match raw {
        0 => Ok(TimeSource::Unknown),
        1 => Ok(TimeSource::System),
        2 => Ok(TimeSource::Ntp),
        3 => Ok(TimeSource::Ptp),
        4 => Ok(TimeSource::Gnss),
        5 => Ok(TimeSource::Manual),
        _ => Err(TraceCodecError::InvalidUtc),
    }
}

fn validate_version(bytes: &[u8]) -> Result<(), TraceCodecError> {
    let major = get_u16(bytes, 8);
    let minor = get_u16(bytes, 10);
    if major != TRACE_LAYOUT_MAJOR || minor != TRACE_LAYOUT_MINOR {
        Err(TraceCodecError::UnsupportedVersion { major, minor })
    } else {
        Ok(())
    }
}

fn decode_epoch(bytes: &[u8]) -> Result<BootEpochId, TraceCodecError> {
    let mut fixed = [0; 16];
    fixed.copy_from_slice(bytes);
    BootEpochId::from_bytes(fixed).map_err(|_| TraceCodecError::InvalidEngineEpoch)
}

fn require_length(bytes: &[u8], expected: usize) -> Result<(), TraceCodecError> {
    if bytes.len() == expected {
        Ok(())
    } else {
        Err(TraceCodecError::InvalidLength {
            expected,
            actual: bytes.len(),
        })
    }
}

fn require_zero(bytes: &[u8]) -> Result<(), TraceCodecError> {
    if bytes.iter().all(|value| *value == 0) {
        Ok(())
    } else {
        Err(TraceCodecError::NonCanonicalEncoding)
    }
}

const fn has_flag(flags: u16, flag: u16) -> bool {
    flags & flag != 0
}

fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn get_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn put_i64(bytes: &mut [u8], offset: usize, value: i64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
#[path = "trace_layout_tests.rs"]
mod tests;
