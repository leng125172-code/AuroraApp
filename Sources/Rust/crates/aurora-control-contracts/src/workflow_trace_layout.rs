//! R2 Workflow Trace 的 96-byte header 与 192-byte record codec。

use aurora_types::{BootEpochId, LocalHandle};
use thiserror::Error;

use crate::{
    CommitSequence, EventSequence, FaultReason, ReleaseSequence, TaskEpoch,
    WorkflowTraceContractError, WorkflowTraceEventKind, WorkflowTraceRecord,
    WorkflowTraceValueFragment, WorkflowTraceVersion,
};

/// Workflow Trace layout major。
pub const WORKFLOW_TRACE_LAYOUT_MAJOR: u16 = 1;
/// Workflow Trace layout minor。
pub const WORKFLOW_TRACE_LAYOUT_MINOR: u16 = 0;
/// 固定文件头字节数。
pub const WORKFLOW_TRACE_FILE_HEADER_SIZE: usize = 96;
/// 固定 record 字节数。
pub const WORKFLOW_TRACE_RECORD_SIZE: usize = 192;

const FILE_MAGIC: [u8; 8] = *b"AURWFT01";
const RECORD_MAGIC: [u8; 8] = *b"AURWFR01";
const HEADER_SIZE_U16: u16 = 96;
const RECORD_SIZE_U16: u16 = 192;
const RECORD_FLAGS_ALLOWED: u16 = 0x01ff;
const FLAG_NODE: u16 = 1 << 0;
const FLAG_EDGE: u16 = 1 << 1;
const FLAG_SOURCE: u16 = 1 << 2;
const FLAG_VALUE: u16 = 1 << 3;
const FLAG_BRANCH: u16 = 1 << 4;
const FLAG_EXECUTION: u16 = 1 << 5;
const FLAG_FAULT: u16 = 1 << 6;
const FLAG_VALUE_DIGEST: u16 = 1 << 7;
const FLAG_FRAGMENT: u16 = 1 << 8;

/// Workflow Trace 固定布局或跨记录验证错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkflowTraceCodecError {
    /// 输入长度不等于固定布局要求。
    #[error("Workflow Trace length is {actual} bytes, expected {expected}")]
    InvalidLength {
        /// 期望长度。
        expected: usize,
        /// 实际长度。
        actual: usize,
    },
    /// magic 无效。
    #[error("invalid Workflow Trace magic")]
    InvalidMagic,
    /// reader 不支持该精确版本。
    #[error("unsupported Workflow Trace layout {major}.{minor}")]
    UnsupportedVersion {
        /// major。
        major: u16,
        /// minor。
        minor: u16,
    },
    /// 固定 header/record 大小声明无效。
    #[error("invalid Workflow Trace fixed layout size")]
    InvalidLayoutSize,
    /// flags 含未知位。
    #[error("Workflow Trace flags contain unsupported bits")]
    UnsupportedFlags,
    /// reserved、absent 或 fragment 尾部不是规范零表示。
    #[error("Workflow Trace contains a non-canonical encoding")]
    NonCanonicalEncoding,
    /// record count 的总长度不可表示。
    #[error("Workflow Trace record count cannot be represented")]
    LengthOverflow,
    /// `EngineEpoch` 不是有效 `UUIDv7`。
    #[error("Workflow Trace EngineEpoch is invalid")]
    InvalidEngineEpoch,
    /// `TaskHandle` 无效。
    #[error("Workflow Trace TaskHandle is invalid")]
    InvalidTaskHandle,
    /// record `EngineEpoch` 与 header 不一致。
    #[error("Workflow Trace record EngineEpoch differs from its header")]
    EngineEpochMismatch,
    /// record index 越界。
    #[error("Workflow Trace record index is out of range")]
    RecordIndexOutOfRange,
    /// `EventSequence` 回退或重复。
    #[error("Workflow Trace EventSequence regressed")]
    EventSequenceRegression,
    /// header drop 小于可观察 sequence gap。
    #[error("Workflow Trace DroppedRecords understates observed sequence gaps")]
    DroppedRecordsUnderflow,
    /// value fragments 不连续、交错或 metadata 不一致。
    #[error("Workflow Trace value fragments are not canonical and contiguous")]
    InvalidFragmentSequence,
    /// 同 release 的 identity、commit 或分组连续性无效。
    #[error("Workflow Trace release identity is not contiguous")]
    InvalidReleaseIdentity,
    /// 可见事件违反规范 phase/order。
    #[error("Workflow Trace event order is invalid")]
    InvalidEventOrder,
    /// 完整 release 缺少唯一且最后的 terminal event。
    #[error("Workflow Trace release terminal is missing or duplicated")]
    InvalidReleaseTerminal,
    /// 相邻完整 release 的 commit chain 不连续。
    #[error("Workflow Trace commit chain is invalid")]
    InvalidCommitChain,
    /// record 字段违反 Workflow Trace 语义。
    #[error("Workflow Trace record violates its contract: {0}")]
    Contract(WorkflowTraceContractError),
}

impl From<WorkflowTraceContractError> for WorkflowTraceCodecError {
    fn from(value: WorkflowTraceContractError) -> Self {
        Self::Contract(value)
    }
}

/// 一个规范 Workflow Trace 文件头。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTraceFileHeader {
    engine_epoch: BootEpochId,
    plan_digest: [u8; 32],
    record_count: u64,
    dropped_records: u64,
}

impl WorkflowTraceFileHeader {
    /// 创建固定 header。
    #[must_use]
    pub const fn new(
        engine_epoch: BootEpochId,
        plan_digest: [u8; 32],
        record_count: u64,
        dropped_records: u64,
    ) -> Self {
        Self {
            engine_epoch,
            plan_digest,
            record_count,
            dropped_records,
        }
    }
    /// 返回 `EngineEpoch`。
    #[must_use]
    pub const fn engine_epoch(self) -> BootEpochId {
        self.engine_epoch
    }
    /// 返回静态计划原始 SHA-256。
    #[must_use]
    pub const fn plan_digest(self) -> [u8; 32] {
        self.plan_digest
    }
    /// 返回实际 record 数。
    #[must_use]
    pub const fn record_count(self) -> u64 {
        self.record_count
    }
    /// 返回 producer 已知累计 drop。
    #[must_use]
    pub const fn dropped_records(self) -> u64 {
        self.dropped_records
    }

    /// 编码固定 little-endian header。
    #[must_use]
    pub fn encode(self) -> [u8; WORKFLOW_TRACE_FILE_HEADER_SIZE] {
        let mut bytes = [0; WORKFLOW_TRACE_FILE_HEADER_SIZE];
        bytes[..8].copy_from_slice(&FILE_MAGIC);
        put_u16(&mut bytes, 8, WORKFLOW_TRACE_LAYOUT_MAJOR);
        put_u16(&mut bytes, 10, WORKFLOW_TRACE_LAYOUT_MINOR);
        put_u16(&mut bytes, 12, HEADER_SIZE_U16);
        put_u16(&mut bytes, 14, RECORD_SIZE_U16);
        bytes[24..40].copy_from_slice(&self.engine_epoch.to_bytes());
        bytes[40..72].copy_from_slice(&self.plan_digest);
        put_u64(&mut bytes, 72, self.record_count);
        put_u64(&mut bytes, 80, self.dropped_records);
        bytes
    }

    /// 解码并拒绝未知版本、flags 和非零 reserved。
    ///
    /// # Errors
    /// 任一固定布局字段无效时返回精确错误。
    pub fn decode(bytes: &[u8]) -> Result<Self, WorkflowTraceCodecError> {
        require_length(bytes, WORKFLOW_TRACE_FILE_HEADER_SIZE)?;
        if bytes[..8] != FILE_MAGIC {
            return Err(WorkflowTraceCodecError::InvalidMagic);
        }
        validate_version(bytes)?;
        if get_u16(bytes, 12) != HEADER_SIZE_U16 || get_u16(bytes, 14) != RECORD_SIZE_U16 {
            return Err(WorkflowTraceCodecError::InvalidLayoutSize);
        }
        if get_u32(bytes, 16) != 0 {
            return Err(WorkflowTraceCodecError::UnsupportedFlags);
        }
        require_zero(&bytes[20..24])?;
        require_zero(&bytes[88..96])?;
        let engine_epoch = decode_epoch(&bytes[24..40])?;
        let mut plan_digest = [0; 32];
        plan_digest.copy_from_slice(&bytes[40..72]);
        Ok(Self {
            engine_epoch,
            plan_digest,
            record_count: get_u64(bytes, 72),
            dropped_records: get_u64(bytes, 80),
        })
    }
}

/// 一个已规范编码、可直接进入固定 ring 的 192-byte record。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTraceRecordBytes([u8; WORKFLOW_TRACE_RECORD_SIZE]);

impl WorkflowTraceRecordBytes {
    /// 将已验证 record 编码为固定字节。
    #[must_use]
    pub fn encode(record: WorkflowTraceRecord) -> Self {
        let mut bytes = [0; WORKFLOW_TRACE_RECORD_SIZE];
        bytes[..8].copy_from_slice(&RECORD_MAGIC);
        put_u16(&mut bytes, 8, WORKFLOW_TRACE_LAYOUT_MAJOR);
        put_u16(&mut bytes, 10, WORKFLOW_TRACE_LAYOUT_MINOR);
        put_u16(&mut bytes, 12, RECORD_SIZE_U16);
        let mut flags = 0_u16;
        put_optional_u32(&mut bytes, 28, record.node_handle(), FLAG_NODE, &mut flags);
        put_optional_u32(&mut bytes, 32, record.edge_handle(), FLAG_EDGE, &mut flags);
        put_optional_u32(
            &mut bytes,
            36,
            record.source_handle(),
            FLAG_SOURCE,
            &mut flags,
        );
        if let (Some(value), Some(value_type)) = (record.value_handle(), record.type_handle()) {
            flags |= FLAG_VALUE;
            put_u32(&mut bytes, 40, value);
            put_u32(&mut bytes, 52, value_type);
        } else {
            put_u32(&mut bytes, 40, u32::MAX);
            put_u32(&mut bytes, 52, u32::MAX);
        }
        put_optional_u32(
            &mut bytes,
            44,
            record.branch_order(),
            FLAG_BRANCH,
            &mut flags,
        );
        put_optional_u32(
            &mut bytes,
            48,
            record.execution_order(),
            FLAG_EXECUTION,
            &mut flags,
        );
        if let Some(fault) = record.fault() {
            flags |= FLAG_FAULT;
            put_u16(&mut bytes, 62, fault as u16);
        }
        let fragment = record.fragment();
        if let Some(digest) = fragment.digest {
            flags |= FLAG_VALUE_DIGEST;
            bytes[120..152].copy_from_slice(&digest);
        }
        if fragment.is_present() {
            flags |= FLAG_FRAGMENT;
            put_u16(&mut bytes, 56, fragment.index);
            put_u16(&mut bytes, 58, fragment.count);
            put_u16(&mut bytes, 60, fragment.bytes);
            bytes[152..184].copy_from_slice(&fragment.storage);
        }
        put_u16(&mut bytes, 14, flags);
        put_u16(&mut bytes, 16, record.kind() as u16);
        put_u16(&mut bytes, 18, record.detail());
        put_u32(&mut bytes, 20, record.task_handle().get());
        put_u32(&mut bytes, 24, record.workflow_instance_handle());
        bytes[64..80].copy_from_slice(&record.engine_epoch().to_bytes());
        put_u64(&mut bytes, 80, record.task_epoch().get());
        put_u64(&mut bytes, 88, record.event_sequence().get());
        put_u64(&mut bytes, 96, record.release_sequence().get());
        put_u64(&mut bytes, 104, record.commit_before().get());
        put_u64(&mut bytes, 112, record.commit_after().get());
        Self(bytes)
    }

    /// 从恰好 192 bytes 复制 record。
    ///
    /// # Errors
    /// 长度不等于固定 record 时拒绝。
    pub fn from_slice(bytes: &[u8]) -> Result<Self, WorkflowTraceCodecError> {
        require_length(bytes, WORKFLOW_TRACE_RECORD_SIZE)?;
        let mut fixed = [0; WORKFLOW_TRACE_RECORD_SIZE];
        fixed.copy_from_slice(bytes);
        Ok(Self(fixed))
    }
    /// 返回固定字节。
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; WORKFLOW_TRACE_RECORD_SIZE] {
        &self.0
    }
    /// 解码并验证 record。
    ///
    /// # Errors
    /// 布局、规范表示或语义无效时拒绝。
    pub fn decode(self) -> Result<WorkflowTraceRecord, WorkflowTraceCodecError> {
        decode_record(&self.0)
    }
}

/// 已验证文件的完整性摘要。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTraceCompleteness {
    /// 从 `EventSequence` 观察到的缺失 record 数。
    pub observed_sequence_gaps: u64,
    /// header 报告的 producer drop 数。
    pub dropped_records: u64,
}

impl WorkflowTraceCompleteness {
    /// 只有无 drop 且无 gap 时完整。
    #[must_use]
    pub const fn is_complete(self) -> bool {
        self.observed_sequence_gaps == 0 && self.dropped_records == 0
    }
}

/// 借用离线 Workflow Trace bytes 的只读 view。
#[derive(Debug, Clone, Copy)]
pub struct WorkflowTraceFileView<'bytes> {
    header: WorkflowTraceFileHeader,
    bytes: &'bytes [u8],
    count: usize,
    completeness: WorkflowTraceCompleteness,
}

impl<'bytes> WorkflowTraceFileView<'bytes> {
    /// 验证精确文件长度、全部 records、sequence 与 fragments。
    ///
    /// gap 本身不伪造 record，而是通过 [`WorkflowTraceCompleteness`] 标记 incomplete；
    /// header drop 小于已观察 gap 时输入自相矛盾并拒绝。
    ///
    /// # Errors
    /// 任一 header、record、identity、sequence 或 fragment 规则失败时拒绝。
    pub fn parse(bytes: &'bytes [u8]) -> Result<Self, WorkflowTraceCodecError> {
        if bytes.len() < WORKFLOW_TRACE_FILE_HEADER_SIZE {
            return Err(WorkflowTraceCodecError::InvalidLength {
                expected: WORKFLOW_TRACE_FILE_HEADER_SIZE,
                actual: bytes.len(),
            });
        }
        let header = WorkflowTraceFileHeader::decode(&bytes[..WORKFLOW_TRACE_FILE_HEADER_SIZE])?;
        let count = usize::try_from(header.record_count())
            .map_err(|_| WorkflowTraceCodecError::LengthOverflow)?;
        let record_bytes = count
            .checked_mul(WORKFLOW_TRACE_RECORD_SIZE)
            .ok_or(WorkflowTraceCodecError::LengthOverflow)?;
        let expected = WORKFLOW_TRACE_FILE_HEADER_SIZE
            .checked_add(record_bytes)
            .ok_or(WorkflowTraceCodecError::LengthOverflow)?;
        require_length(bytes, expected)?;
        let mut view = Self {
            header,
            bytes,
            count,
            completeness: WorkflowTraceCompleteness {
                observed_sequence_gaps: 0,
                dropped_records: header.dropped_records(),
            },
        };
        view.completeness.observed_sequence_gaps = validate_records(view)?;
        if header.dropped_records() < view.completeness.observed_sequence_gaps {
            return Err(WorkflowTraceCodecError::DroppedRecordsUnderflow);
        }
        Ok(view)
    }
    /// 返回文件头。
    #[must_use]
    pub const fn header(self) -> WorkflowTraceFileHeader {
        self.header
    }
    /// 返回完整性摘要。
    #[must_use]
    pub const fn completeness(self) -> WorkflowTraceCompleteness {
        self.completeness
    }
    /// 解码指定 record。
    ///
    /// # Errors
    /// index 越界、record 无效或 epoch 不匹配时拒绝。
    pub fn record(self, index: u64) -> Result<WorkflowTraceRecord, WorkflowTraceCodecError> {
        if index >= self.header.record_count() {
            return Err(WorkflowTraceCodecError::RecordIndexOutOfRange);
        }
        let index = usize::try_from(index).map_err(|_| WorkflowTraceCodecError::LengthOverflow)?;
        self.record_at(index)
    }
    /// 返回无分配顺序 iterator。
    #[must_use]
    pub const fn records(self) -> WorkflowTraceRecordIterator<'bytes> {
        WorkflowTraceRecordIterator {
            view: self,
            next: 0,
        }
    }
    fn record_at(self, index: usize) -> Result<WorkflowTraceRecord, WorkflowTraceCodecError> {
        let start = WORKFLOW_TRACE_FILE_HEADER_SIZE + index * WORKFLOW_TRACE_RECORD_SIZE;
        let record = WorkflowTraceRecordBytes::from_slice(
            &self.bytes[start..start + WORKFLOW_TRACE_RECORD_SIZE],
        )?
        .decode()?;
        if record.engine_epoch() != self.header.engine_epoch() {
            return Err(WorkflowTraceCodecError::EngineEpochMismatch);
        }
        Ok(record)
    }
}

/// Workflow Trace record iterator。
#[derive(Debug)]
pub struct WorkflowTraceRecordIterator<'bytes> {
    view: WorkflowTraceFileView<'bytes>,
    next: usize,
}

impl Iterator for WorkflowTraceRecordIterator<'_> {
    type Item = Result<WorkflowTraceRecord, WorkflowTraceCodecError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.view.count {
            return None;
        }
        let index = self.next;
        self.next += 1;
        Some(self.view.record_at(index))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.view.count - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for WorkflowTraceRecordIterator<'_> {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FragmentKey {
    kind: WorkflowTraceEventKind,
    task: u32,
    instance: u32,
    node: Option<u32>,
    edge: Option<u32>,
    source: Option<u32>,
    value: Option<u32>,
    value_type: Option<u32>,
    release: u64,
    commit: u64,
    count: u16,
    digest: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ReleaseKey {
    task: u32,
    task_epoch: u64,
    release: u64,
}

#[derive(Debug, Clone, Copy)]
struct ReleaseValidation {
    key: ReleaseKey,
    commit_before: u64,
    phase: u8,
    terminal_after: Option<u64>,
    initialized: bool,
    visible_events: u32,
    previous_observation: Option<u16>,
    node_execution: Option<u32>,
    node_phase: u8,
    output_value: Option<(u32, u16)>,
    transition_edge: Option<u32>,
    fork_branch: Option<u32>,
    previous_structural: Option<(u32, u16)>,
    previous_watch: Option<(u32, u16)>,
    faulted: bool,
}

#[allow(
    clippy::too_many_lines,
    reason = "跨记录 sequence、release、commit、order 与 fragment 状态必须在一次有序扫描中联合验证"
)]
fn validate_records(view: WorkflowTraceFileView<'_>) -> Result<u64, WorkflowTraceCodecError> {
    use std::collections::{BTreeMap, BTreeSet};

    let mut previous = None::<u64>;
    let mut gaps = 0_u64;
    let mut active = None::<(FragmentKey, u16)>;
    let mut release = None::<ReleaseValidation>;
    let mut closed = BTreeSet::<ReleaseKey>::new();
    let mut commits = BTreeMap::<(u32, u64), u64>::new();
    for record in view.records() {
        let record = record?;
        let sequence = record.event_sequence().get();
        let gap = match previous {
            None => sequence,
            Some(value) => sequence
                .checked_sub(
                    value
                        .checked_add(1)
                        .ok_or(WorkflowTraceCodecError::EventSequenceRegression)?,
                )
                .ok_or(WorkflowTraceCodecError::EventSequenceRegression)?,
        };
        gaps = gaps
            .checked_add(gap)
            .ok_or(WorkflowTraceCodecError::LengthOverflow)?;
        previous = Some(sequence);
        let key = ReleaseKey {
            task: record.task_handle().get(),
            task_epoch: record.task_epoch().get(),
            release: record.release_sequence().get(),
        };
        if release.is_none_or(|current| current.key != key) {
            if let Some(current) = release {
                closed.insert(current.key);
                if gap == 0 && current.terminal_after.is_none() {
                    return Err(WorkflowTraceCodecError::InvalidReleaseTerminal);
                }
            }
            if closed.contains(&key) {
                return Err(WorkflowTraceCodecError::InvalidReleaseIdentity);
            }
            if gap == 0
                && let Some(expected) = commits.get(&(key.task, key.task_epoch))
                && *expected != record.commit_before().get()
            {
                return Err(WorkflowTraceCodecError::InvalidCommitChain);
            }
            release = Some(ReleaseValidation {
                key,
                commit_before: record.commit_before().get(),
                phase: 0,
                terminal_after: None,
                initialized: false,
                visible_events: 0,
                previous_observation: None,
                node_execution: None,
                node_phase: 0,
                output_value: None,
                transition_edge: None,
                fork_branch: None,
                previous_structural: None,
                previous_watch: None,
                faulted: false,
            });
        }
        let current = release
            .as_mut()
            .ok_or(WorkflowTraceCodecError::InvalidReleaseIdentity)?;
        if record.commit_before().get() != current.commit_before {
            return Err(WorkflowTraceCodecError::InvalidCommitChain);
        }
        if current.terminal_after.is_some() {
            return Err(WorkflowTraceCodecError::InvalidReleaseTerminal);
        }
        validate_event_order(current, record)?;
        if matches!(
            record.kind(),
            WorkflowTraceEventKind::ScanCommitted | WorkflowTraceEventKind::ScanDiscarded
        ) {
            current.terminal_after = Some(record.commit_after().get());
            commits.insert((key.task, key.task_epoch), record.commit_after().get());
        }
        let fragment = record.fragment();
        if let Some((key, expected_index)) = active {
            let current =
                fragment_key(record).ok_or(WorkflowTraceCodecError::InvalidFragmentSequence)?;
            if current != key || fragment.index != expected_index || gap != 0 {
                return Err(WorkflowTraceCodecError::InvalidFragmentSequence);
            }
            active = if fragment.index + 1 == fragment.count {
                None
            } else {
                Some((key, fragment.index + 1))
            };
        } else if fragment.is_present() {
            if fragment.index != 0 {
                return Err(WorkflowTraceCodecError::InvalidFragmentSequence);
            }
            if fragment.count > 1 {
                let key =
                    fragment_key(record).ok_or(WorkflowTraceCodecError::InvalidFragmentSequence)?;
                active = Some((key, 1));
            }
        }
    }
    if active.is_some() {
        return Err(WorkflowTraceCodecError::InvalidFragmentSequence);
    }
    let complete = gaps == 0 && view.header.dropped_records() == 0;
    if complete && release.is_some_and(|current| current.terminal_after.is_none()) {
        return Err(WorkflowTraceCodecError::InvalidReleaseTerminal);
    }
    Ok(gaps)
}

fn validate_event_order(
    state: &mut ReleaseValidation,
    record: WorkflowTraceRecord,
) -> Result<(), WorkflowTraceCodecError> {
    if state.faulted && record.kind() != WorkflowTraceEventKind::ScanDiscarded {
        return Err(WorkflowTraceCodecError::InvalidEventOrder);
    }
    let phase = event_phase(record.kind());
    if phase < state.phase {
        return Err(WorkflowTraceCodecError::InvalidEventOrder);
    }
    state.phase = phase;
    if record.kind() == WorkflowTraceEventKind::WorkflowInitialized {
        if state.initialized || state.visible_events != 0 {
            return Err(WorkflowTraceCodecError::InvalidEventOrder);
        }
        state.initialized = true;
    } else if phase == 1 {
        let kind = record.kind() as u16;
        if state
            .previous_observation
            .is_some_and(|previous| kind <= previous)
        {
            return Err(WorkflowTraceCodecError::InvalidEventOrder);
        }
        state.previous_observation = Some(kind);
    } else if phase == 2 {
        validate_node_event_order(state, record)?;
    } else if phase == 3 {
        let key = (
            record.execution_order().unwrap_or(u32::MAX),
            record.kind() as u16,
        );
        if state
            .previous_structural
            .is_some_and(|previous| key < previous)
        {
            return Err(WorkflowTraceCodecError::InvalidEventOrder);
        }
        state.previous_structural = Some(key);
    }
    if record.kind() == WorkflowTraceEventKind::WatchedValue {
        let key = (
            record
                .value_handle()
                .ok_or(WorkflowTraceCodecError::InvalidEventOrder)?,
            record.fragment().index,
        );
        if state.previous_watch.is_some_and(|previous| key <= previous) {
            return Err(WorkflowTraceCodecError::InvalidEventOrder);
        }
        state.previous_watch = Some(key);
    }
    if record.kind() == WorkflowTraceEventKind::WorkflowFaulted {
        state.faulted = true;
    }
    state.visible_events = state
        .visible_events
        .checked_add(1)
        .ok_or(WorkflowTraceCodecError::LengthOverflow)?;
    Ok(())
}

fn validate_node_event_order(
    state: &mut ReleaseValidation,
    record: WorkflowTraceRecord,
) -> Result<(), WorkflowTraceCodecError> {
    let execution = record
        .execution_order()
        .ok_or(WorkflowTraceCodecError::InvalidEventOrder)?;
    let new_node = match state.node_execution {
        None => {
            if record.kind() != WorkflowTraceEventKind::NodeExecuted {
                return Err(WorkflowTraceCodecError::InvalidEventOrder);
            }
            state.node_execution = Some(execution);
            state.node_phase = 0;
            true
        }
        Some(previous) if execution > previous => {
            if record.kind() != WorkflowTraceEventKind::NodeExecuted {
                return Err(WorkflowTraceCodecError::InvalidEventOrder);
            }
            state.node_execution = Some(execution);
            state.node_phase = 0;
            state.output_value = None;
            state.transition_edge = None;
            state.fork_branch = None;
            true
        }
        Some(previous) if execution < previous => {
            return Err(WorkflowTraceCodecError::InvalidEventOrder);
        }
        Some(_) => false,
    };

    let event_phase = match record.kind() {
        WorkflowTraceEventKind::NodeExecuted => 0,
        WorkflowTraceEventKind::OutputStaged => 1,
        WorkflowTraceEventKind::TransitionTaken => 2,
        WorkflowTraceEventKind::ForkActivated => 3,
        _ => return Err(WorkflowTraceCodecError::InvalidEventOrder),
    };
    if event_phase < state.node_phase || (event_phase == 0 && !new_node) {
        return Err(WorkflowTraceCodecError::InvalidEventOrder);
    }
    state.node_phase = event_phase;
    match record.kind() {
        WorkflowTraceEventKind::OutputStaged => {
            let key = (
                record
                    .value_handle()
                    .ok_or(WorkflowTraceCodecError::InvalidEventOrder)?,
                record.fragment().index,
            );
            if state.output_value.is_some_and(|previous| key <= previous) {
                return Err(WorkflowTraceCodecError::InvalidEventOrder);
            }
            state.output_value = Some(key);
        }
        WorkflowTraceEventKind::TransitionTaken => {
            let edge = record
                .edge_handle()
                .ok_or(WorkflowTraceCodecError::InvalidEventOrder)?;
            if state
                .transition_edge
                .is_some_and(|previous| edge <= previous)
            {
                return Err(WorkflowTraceCodecError::InvalidEventOrder);
            }
            state.transition_edge = Some(edge);
        }
        WorkflowTraceEventKind::ForkActivated => {
            let branch = record
                .branch_order()
                .ok_or(WorkflowTraceCodecError::InvalidEventOrder)?;
            if state.fork_branch.is_some_and(|previous| branch <= previous) {
                return Err(WorkflowTraceCodecError::InvalidEventOrder);
            }
            state.fork_branch = Some(branch);
        }
        WorkflowTraceEventKind::NodeExecuted => {}
        _ => return Err(WorkflowTraceCodecError::InvalidEventOrder),
    }
    Ok(())
}

const fn event_phase(kind: WorkflowTraceEventKind) -> u8 {
    match kind {
        WorkflowTraceEventKind::WorkflowInitialized => 0,
        WorkflowTraceEventKind::ForceObserved
        | WorkflowTraceEventKind::FallbackObserved
        | WorkflowTraceEventKind::DeadlineObserved => 1,
        WorkflowTraceEventKind::NodeExecuted
        | WorkflowTraceEventKind::TransitionTaken
        | WorkflowTraceEventKind::ForkActivated
        | WorkflowTraceEventKind::OutputStaged => 2,
        WorkflowTraceEventKind::JoinSatisfied
        | WorkflowTraceEventKind::WaitObserved
        | WorkflowTraceEventKind::CancelRequested
        | WorkflowTraceEventKind::CancelApplied
        | WorkflowTraceEventKind::SubworkflowActivated
        | WorkflowTraceEventKind::SubworkflowCompleted
        | WorkflowTraceEventKind::CompletionRequested
        | WorkflowTraceEventKind::WorkflowCompleted
        | WorkflowTraceEventKind::WorkflowFaulted => 3,
        WorkflowTraceEventKind::WatchedValue => 4,
        WorkflowTraceEventKind::ScanCommitted | WorkflowTraceEventKind::ScanDiscarded => 5,
    }
}

fn fragment_key(record: WorkflowTraceRecord) -> Option<FragmentKey> {
    let fragment = record.fragment();
    Some(FragmentKey {
        kind: record.kind(),
        task: record.task_handle().get(),
        instance: record.workflow_instance_handle(),
        node: record.node_handle(),
        edge: record.edge_handle(),
        source: record.source_handle(),
        value: record.value_handle(),
        value_type: record.type_handle(),
        release: record.release_sequence().get(),
        commit: record.commit_before().get(),
        count: fragment.count,
        digest: fragment.digest?,
    })
}

fn decode_record(
    bytes: &[u8; WORKFLOW_TRACE_RECORD_SIZE],
) -> Result<WorkflowTraceRecord, WorkflowTraceCodecError> {
    if bytes[..8] != RECORD_MAGIC {
        return Err(WorkflowTraceCodecError::InvalidMagic);
    }
    validate_version(bytes)?;
    if get_u16(bytes, 12) != RECORD_SIZE_U16 {
        return Err(WorkflowTraceCodecError::InvalidLayoutSize);
    }
    let flags = get_u16(bytes, 14);
    if flags & !RECORD_FLAGS_ALLOWED != 0 {
        return Err(WorkflowTraceCodecError::UnsupportedFlags);
    }
    require_zero(&bytes[184..192])?;
    let task_handle = LocalHandle::new(get_u32(bytes, 20))
        .map_err(|_| WorkflowTraceCodecError::InvalidTaskHandle)?;
    let instance = get_u32(bytes, 24);
    let node = decode_optional_u32(bytes, flags, FLAG_NODE, 28)?;
    let edge = decode_optional_u32(bytes, flags, FLAG_EDGE, 32)?;
    let source = decode_optional_u32(bytes, flags, FLAG_SOURCE, 36)?;
    let (value, value_type) = if flags & FLAG_VALUE != 0 {
        (
            Some(required_handle(bytes, 40)?),
            Some(required_handle(bytes, 52)?),
        )
    } else {
        require_sentinel(bytes, 40)?;
        require_sentinel(bytes, 52)?;
        (None, None)
    };
    let branch = decode_optional_u32(bytes, flags, FLAG_BRANCH, 44)?;
    let execution = decode_optional_u32(bytes, flags, FLAG_EXECUTION, 48)?;
    let fault = if flags & FLAG_FAULT != 0 {
        Some(
            FaultReason::try_from(get_u16(bytes, 62))
                .map_err(|_| WorkflowTraceContractError::InvalidFaultPresence)?,
        )
    } else {
        if get_u16(bytes, 62) != 0 {
            return Err(WorkflowTraceCodecError::NonCanonicalEncoding);
        }
        None
    };
    let digest = if flags & FLAG_VALUE_DIGEST != 0 {
        let mut value = [0; 32];
        value.copy_from_slice(&bytes[120..152]);
        Some(value)
    } else {
        require_zero(&bytes[120..152])?;
        None
    };
    let fragment = if flags & FLAG_FRAGMENT != 0 {
        let mut storage = [0; 32];
        storage.copy_from_slice(&bytes[152..184]);
        WorkflowTraceValueFragment {
            index: get_u16(bytes, 56),
            count: get_u16(bytes, 58),
            bytes: get_u16(bytes, 60),
            digest,
            storage,
        }
    } else {
        require_zero(&bytes[56..62])?;
        require_zero(&bytes[152..184])?;
        if digest.is_some() {
            return Err(WorkflowTraceCodecError::NonCanonicalEncoding);
        }
        WorkflowTraceValueFragment::ABSENT
    };
    WorkflowTraceRecord::new(
        WorkflowTraceVersion::V1_0,
        WorkflowTraceEventKind::try_from(get_u16(bytes, 16))?,
        get_u16(bytes, 18),
        task_handle,
        instance,
        node,
        edge,
        source,
        value,
        branch,
        execution,
        value_type,
        fault,
        decode_epoch(&bytes[64..80])?,
        TaskEpoch::new(get_u64(bytes, 80))
            .map_err(|_| WorkflowTraceContractError::InvalidTaskEpoch)?,
        EventSequence::new(get_u64(bytes, 88)),
        ReleaseSequence::new(get_u64(bytes, 96)),
        CommitSequence::new(get_u64(bytes, 104)),
        CommitSequence::new(get_u64(bytes, 112)),
        fragment,
    )
    .map_err(Into::into)
}

fn validate_version(bytes: &[u8]) -> Result<(), WorkflowTraceCodecError> {
    let major = get_u16(bytes, 8);
    let minor = get_u16(bytes, 10);
    if major == WORKFLOW_TRACE_LAYOUT_MAJOR && minor == WORKFLOW_TRACE_LAYOUT_MINOR {
        Ok(())
    } else {
        Err(WorkflowTraceCodecError::UnsupportedVersion { major, minor })
    }
}
fn decode_epoch(bytes: &[u8]) -> Result<BootEpochId, WorkflowTraceCodecError> {
    let fixed: [u8; 16] = bytes
        .try_into()
        .map_err(|_| WorkflowTraceCodecError::InvalidEngineEpoch)?;
    BootEpochId::from_bytes(fixed).map_err(|_| WorkflowTraceCodecError::InvalidEngineEpoch)
}
fn put_optional_u32(
    bytes: &mut [u8],
    offset: usize,
    value: Option<u32>,
    flag: u16,
    flags: &mut u16,
) {
    if let Some(value) = value {
        *flags |= flag;
        put_u32(bytes, offset, value);
    } else {
        put_u32(bytes, offset, u32::MAX);
    }
}
fn decode_optional_u32(
    bytes: &[u8],
    flags: u16,
    flag: u16,
    offset: usize,
) -> Result<Option<u32>, WorkflowTraceCodecError> {
    if flags & flag != 0 {
        Ok(Some(required_handle(bytes, offset)?))
    } else {
        require_sentinel(bytes, offset)?;
        Ok(None)
    }
}
fn required_handle(bytes: &[u8], offset: usize) -> Result<u32, WorkflowTraceCodecError> {
    let value = get_u32(bytes, offset);
    if value == u32::MAX {
        Err(WorkflowTraceCodecError::NonCanonicalEncoding)
    } else {
        Ok(value)
    }
}
fn require_sentinel(bytes: &[u8], offset: usize) -> Result<(), WorkflowTraceCodecError> {
    if get_u32(bytes, offset) == u32::MAX {
        Ok(())
    } else {
        Err(WorkflowTraceCodecError::NonCanonicalEncoding)
    }
}
fn require_length(bytes: &[u8], expected: usize) -> Result<(), WorkflowTraceCodecError> {
    if bytes.len() == expected {
        Ok(())
    } else {
        Err(WorkflowTraceCodecError::InvalidLength {
            expected,
            actual: bytes.len(),
        })
    }
}
fn require_zero(bytes: &[u8]) -> Result<(), WorkflowTraceCodecError> {
    if bytes.iter().all(|byte| *byte == 0) {
        Ok(())
    } else {
        Err(WorkflowTraceCodecError::NonCanonicalEncoding)
    }
}
fn get_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn get_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap_or([0; 4]))
}
fn get_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap_or([0; 8]))
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

#[cfg(test)]
#[path = "workflow_trace_layout_tests.rs"]
mod tests;
