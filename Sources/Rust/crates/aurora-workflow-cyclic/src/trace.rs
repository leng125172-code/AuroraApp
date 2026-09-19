//! 单个 release 的固定容量 Workflow Trace staging 与 watch 采集。
//!
//! 构造期完成全部分配与 watch 范围验证；周期路径只写预分配槽、读取 staging image、
//! 计算有界 SHA-256 并执行一次非阻塞 publish。容量不足会使整个 release draft 无效，
//! 不会发布截断事件流。

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use aurora_control_contracts::{
    CommitSequence, EventSequence, FaultReason, MissOutcome, WorkflowTraceContractError,
    WorkflowTraceEventKind, WorkflowTraceRecord, WorkflowTraceValueFragment, WorkflowTraceVersion,
};
use aurora_control_engine::{
    CycleCommit, CycleDiscard, CycleFinishFailure, CycleIdentity, CycleTransaction,
    TransactionError, WorkSetIndex, WorkflowTracePublishError, WorkflowTracePublishOutcome,
    WorkflowTracePublisher,
};
use sha2::{Digest, Sha256};

use crate::{StructuredInstanceHandle, WorkflowEdgeHandle, WorkflowNodeHandle};

/// watch 读取的 task image 区域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowTraceWatchArea {
    /// task application state（不含 Workflow control prefix）。
    State,
    /// task output staging image。
    Output,
}

/// 编译期生成的一个 watch 绑定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTraceWatchBinding {
    /// 展开调用实例；同一源工作流的不同调用点不得共用该身份。
    pub workflow_instance: StructuredInstanceHandle,
    /// 稠密 value handle。
    pub value_handle: u32,
    /// canonical storage type handle。
    pub type_handle: u32,
    /// 读取区域。
    pub area: WorkflowTraceWatchArea,
    /// 相对 application state 或 output 的首字节。
    pub offset: usize,
    /// canonical storage 的非零固定字节数。
    pub byte_count: usize,
}

/// recorder 初始化错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowTraceRecorderBuildError {
    /// 单 release 容量必须至少容纳一个 terminal event。
    InvalidEventCapacity,
    /// 数量、区间或 fragment 计算无法表示。
    CapacityOverflow,
    /// watch 使用保留句柄、零长度、重复 value 或越过 task image。
    InvalidWatchBinding,
    /// 初始化分配失败。
    AllocationFailed,
}

impl Display for WorkflowTraceRecorderBuildError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "Workflow Trace recorder build error: {self:?}")
    }
}
impl Error for WorkflowTraceRecorderBuildError {}

/// 单 release staging/finalize/flush 错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowTraceError {
    /// begin/finalize/flush 调用顺序错误或 release identity 不匹配。
    InvalidLifecycle,
    /// 单 release 最坏事件数超过预分配容量；draft 已失效且不得 flush。
    StageCapacityExceeded,
    /// watch staging image 读取失败。
    Transaction(TransactionError),
    /// terminal commit 序列不符合提交/丢弃语义。
    InvalidCommitTransition,
    /// Action output sample 的句柄、长度或 fragment 不可表示。
    InvalidOutputSample,
    /// `EventSequence` 已耗尽。
    EventSequenceExhausted,
    /// 固定 record 语义构造失败。
    Contract(WorkflowTraceContractError),
    /// 非阻塞 producer 拒绝 record。
    Publish(WorkflowTracePublishError),
}

impl Display for WorkflowTraceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "Workflow Trace error: {self:?}")
    }
}
impl Error for WorkflowTraceError {}

/// 一次 flush 的发布计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTraceFlushReport {
    /// 成功进入 ring 的 record 数。
    pub published: u32,
    /// 因 `DropNewest` 丢弃但已消耗 `EventSequence` 的 record 数。
    pub dropped_newest: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecorderState {
    Idle,
    Staging,
    Finalized,
    Invalid,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkflowTraceDraftEvent {
    pub kind: WorkflowTraceEventKind,
    pub detail: u16,
    pub workflow_instance: u32,
    pub node: Option<u32>,
    pub edge: Option<u32>,
    pub source: Option<u32>,
    pub value: Option<u32>,
    pub branch_order: Option<u32>,
    pub execution_order: Option<u32>,
    pub type_handle: Option<u32>,
    pub fault: Option<FaultReason>,
    pub fragment: WorkflowTraceValueFragment,
    ordinal: u32,
}

impl WorkflowTraceDraftEvent {
    const EMPTY: Self = Self {
        kind: WorkflowTraceEventKind::WorkflowInitialized,
        detail: 0,
        workflow_instance: 0,
        node: None,
        edge: None,
        source: None,
        value: None,
        branch_order: None,
        execution_order: None,
        type_handle: None,
        fault: None,
        fragment: WorkflowTraceValueFragment::ABSENT,
        ordinal: 0,
    };

    #[allow(
        clippy::too_many_arguments,
        reason = "固定 record 的显式 optional 字段避免隐藏事件形状"
    )]
    pub(crate) const fn simple(
        kind: WorkflowTraceEventKind,
        detail: u16,
        instance: StructuredInstanceHandle,
        node: Option<WorkflowNodeHandle>,
        edge: Option<WorkflowEdgeHandle>,
        source: Option<u32>,
        branch_order: Option<u32>,
        execution_order: Option<u32>,
    ) -> Self {
        Self {
            kind,
            detail,
            workflow_instance: instance.0,
            node: match node {
                Some(value) => Some(value.get()),
                None => None,
            },
            edge: match edge {
                Some(value) => Some(value.get()),
                None => None,
            },
            source,
            value: None,
            branch_order,
            execution_order,
            type_handle: None,
            fault: None,
            fragment: WorkflowTraceValueFragment::ABSENT,
            ordinal: 0,
        }
    }
}

/// 初始化期预分配、一次只处理一个 release 的 Trace recorder。
///
/// `stage_scan_traced` 负责 begin、结构事件与 watch 采集；调用方在同一个 R0 transaction
/// `finish` 或 `discard` 后显式 finalize，再 flush 到唯一 producer。recorder 不提供第二套
/// Workflow 解释器或提交入口。
#[derive(Debug)]
pub struct WorkflowTraceRecorder {
    events: Box<[WorkflowTraceDraftEvent]>,
    watches: Box<[WorkflowTraceWatchBinding]>,
    event_count: usize,
    state: RecorderState,
    identity: Option<CycleIdentity>,
    commit_before: CommitSequence,
    commit_after: CommitSequence,
    application_state_offset: usize,
    application_state_bytes: usize,
    output_bytes: usize,
}

impl WorkflowTraceRecorder {
    /// 验证完整 watch 表并分配单 release 最坏事件槽。
    ///
    /// `maximum_events_per_release` 包含所有 fragments 与唯一 terminal event；等于上限可用，
    /// 第一个超出项在 staging 时显式失败。
    ///
    /// # Errors
    /// 容量、watch 句柄/范围/重复项或初始化分配无效时拒绝且不返回部分 recorder。
    pub fn new(
        maximum_events_per_release: u32,
        application_state_offset: usize,
        application_state_bytes: usize,
        output_bytes: usize,
        watches: &[WorkflowTraceWatchBinding],
    ) -> Result<Self, WorkflowTraceRecorderBuildError> {
        let event_capacity = usize::try_from(maximum_events_per_release)
            .map_err(|_| WorkflowTraceRecorderBuildError::CapacityOverflow)?;
        if event_capacity == 0 {
            return Err(WorkflowTraceRecorderBuildError::InvalidEventCapacity);
        }
        application_state_offset
            .checked_add(application_state_bytes)
            .ok_or(WorkflowTraceRecorderBuildError::CapacityOverflow)?;
        validate_watches(watches, application_state_bytes, output_bytes)?;

        let mut events = Vec::new();
        events
            .try_reserve_exact(event_capacity)
            .map_err(|_| WorkflowTraceRecorderBuildError::AllocationFailed)?;
        events.resize(event_capacity, WorkflowTraceDraftEvent::EMPTY);
        let mut copied_watches = Vec::new();
        copied_watches
            .try_reserve_exact(watches.len())
            .map_err(|_| WorkflowTraceRecorderBuildError::AllocationFailed)?;
        copied_watches.extend_from_slice(watches);
        Ok(Self {
            events: events.into_boxed_slice(),
            watches: copied_watches.into_boxed_slice(),
            event_count: 0,
            state: RecorderState::Idle,
            identity: None,
            commit_before: CommitSequence::ZERO,
            commit_after: CommitSequence::ZERO,
            application_state_offset,
            application_state_bytes,
            output_bytes,
        })
    }

    /// 开始一个 release；会清空上次已 flush 的槽，不分配。
    ///
    /// # Errors
    /// 上一个 release 尚未 flush 或 task image 与构造期精确布局不同则拒绝。
    pub(crate) fn begin_release(
        &mut self,
        cycle: &CycleTransaction<'_, '_>,
    ) -> Result<(), WorkflowTraceError> {
        if self.state != RecorderState::Idle
            || cycle.state_len()
                != self
                    .application_state_offset
                    .checked_add(self.application_state_bytes)
                    .ok_or(WorkflowTraceError::InvalidLifecycle)?
            || cycle.output_len() != self.output_bytes
        {
            return Err(WorkflowTraceError::InvalidLifecycle);
        }
        self.event_count = 0;
        self.identity = Some(cycle.identity());
        self.commit_before = cycle.commit_before();
        self.commit_after = self.commit_before;
        self.state = RecorderState::Staging;
        Ok(())
    }

    /// 当前 release 已 stage 的 record 数（尚未分配 `EventSequence`）。
    #[must_use]
    pub const fn staged_event_count(&self) -> usize {
        self.event_count
    }

    pub(crate) fn stage(
        &mut self,
        mut event: WorkflowTraceDraftEvent,
    ) -> Result<(), WorkflowTraceError> {
        if self.state != RecorderState::Staging {
            return Err(WorkflowTraceError::InvalidLifecycle);
        }
        // 最后一个槽永久保留给唯一 terminal event。
        if self.event_count >= self.events.len().saturating_sub(1) {
            self.state = RecorderState::Invalid;
            return Err(WorkflowTraceError::StageCapacityExceeded);
        }
        event.ordinal = u32::try_from(self.event_count)
            .map_err(|_| WorkflowTraceError::StageCapacityExceeded)?;
        self.events[self.event_count] = event;
        self.event_count += 1;
        Ok(())
    }

    /// 从当前 R0 staging image 捕获所有编译期 watch；顺序由 value handle 决定而非输入顺序。
    pub(crate) fn capture_watches(
        &mut self,
        cycle: &mut CycleTransaction<'_, '_>,
    ) -> Result<(), WorkflowTraceError> {
        if self.state != RecorderState::Staging {
            return Err(WorkflowTraceError::InvalidLifecycle);
        }
        for watch_index in 0..self.watches.len() {
            let watch = self.watches[watch_index];
            let mut hasher = Sha256::new();
            for relative in 0..watch.byte_count {
                hasher.update([self.read_watch_byte(cycle, watch, relative)?]);
            }
            let digest: [u8; 32] = hasher.finalize().into();
            let fragment_count = watch.byte_count.div_ceil(32);
            let fragment_count = u16::try_from(fragment_count)
                .map_err(|_| WorkflowTraceError::StageCapacityExceeded)?;
            for fragment_index in 0..fragment_count {
                let offset = usize::from(fragment_index) * 32;
                let remaining = watch.byte_count - offset;
                let bytes = remaining.min(32);
                let mut storage = [0_u8; 32];
                for (relative, slot) in storage[..bytes].iter_mut().enumerate() {
                    *slot = self.read_watch_byte(cycle, watch, offset + relative)?;
                }
                self.stage(WorkflowTraceDraftEvent {
                    kind: WorkflowTraceEventKind::WatchedValue,
                    detail: 0,
                    workflow_instance: watch.workflow_instance.0,
                    node: None,
                    edge: None,
                    source: None,
                    value: Some(watch.value_handle),
                    branch_order: None,
                    execution_order: None,
                    type_handle: Some(watch.type_handle),
                    fault: None,
                    fragment: WorkflowTraceValueFragment {
                        index: fragment_index,
                        count: fragment_count,
                        bytes: u16::try_from(bytes)
                            .map_err(|_| WorkflowTraceError::StageCapacityExceeded)?,
                        digest: Some(digest),
                        storage,
                    },
                    ordinal: 0,
                })?;
            }
        }
        Ok(())
    }

    /// 追加一次主 Fault；之后 runtime 不得再执行节点。
    pub(crate) fn stage_fault(
        &mut self,
        instance: StructuredInstanceHandle,
        node: WorkflowNodeHandle,
        source: WorkflowNodeHandle,
        execution_order: Option<u32>,
        reason: FaultReason,
    ) -> Result<(), WorkflowTraceError> {
        let mut event = WorkflowTraceDraftEvent::simple(
            WorkflowTraceEventKind::WorkflowFaulted,
            0,
            instance,
            Some(node),
            None,
            Some(source.get()),
            None,
            execution_order,
        );
        event.fault = Some(reason);
        self.stage(event)
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "Output Trace 必须显式携带完整 provenance 与稳定 value/type identity"
    )]
    pub(crate) fn stage_output(
        &mut self,
        instance: StructuredInstanceHandle,
        node: WorkflowNodeHandle,
        execution_order: u32,
        source_handle: u32,
        value_handle: u32,
        type_handle: u32,
        before: &[u8],
        after: &[u8],
    ) -> Result<(), WorkflowTraceError> {
        if source_handle == u32::MAX
            || value_handle == u32::MAX
            || type_handle == u32::MAX
            || before.len() != after.len()
            || after.is_empty()
            || after.len().div_ceil(32) > usize::from(u16::MAX)
        {
            return Err(WorkflowTraceError::InvalidOutputSample);
        }
        if before == after {
            let mut event = WorkflowTraceDraftEvent::simple(
                WorkflowTraceEventKind::OutputStaged,
                2,
                instance,
                Some(node),
                None,
                Some(source_handle),
                None,
                Some(execution_order),
            );
            event.value = Some(value_handle);
            event.type_handle = Some(type_handle);
            return self.stage(event);
        }
        let digest: [u8; 32] = Sha256::digest(after).into();
        let fragment_count = u16::try_from(after.len().div_ceil(32))
            .map_err(|_| WorkflowTraceError::InvalidOutputSample)?;
        for fragment_index in 0..fragment_count {
            let offset = usize::from(fragment_index) * 32;
            let bytes = (after.len() - offset).min(32);
            let mut storage = [0_u8; 32];
            storage[..bytes].copy_from_slice(&after[offset..offset + bytes]);
            self.stage(WorkflowTraceDraftEvent {
                kind: WorkflowTraceEventKind::OutputStaged,
                detail: 1,
                workflow_instance: instance.0,
                node: Some(node.get()),
                edge: None,
                source: Some(source_handle),
                value: Some(value_handle),
                branch_order: None,
                execution_order: Some(execution_order),
                type_handle: Some(type_handle),
                fault: None,
                fragment: WorkflowTraceValueFragment {
                    index: fragment_index,
                    count: fragment_count,
                    bytes: u16::try_from(bytes)
                        .map_err(|_| WorkflowTraceError::InvalidOutputSample)?,
                    digest: Some(digest),
                    storage,
                },
                ordinal: 0,
            })?;
        }
        Ok(())
    }

    /// 标记 transaction 已成功提交；只追加唯一 terminal event。
    ///
    /// # Errors
    /// commit 不是 `before + 1` 或 draft 非 staging 时拒绝。
    pub fn finalize_committed(&mut self, commit: CycleCommit) -> Result<(), WorkflowTraceError> {
        let identity = self.identity.ok_or(WorkflowTraceError::InvalidLifecycle)?;
        let version = commit.version();
        if self.state != RecorderState::Staging
            || identity != commit.identity()
            || self.commit_before.checked_next() != Ok(version.sequence)
            || commit.checkpoint().deadline_missed()
        {
            return Err(WorkflowTraceError::InvalidCommitTransition);
        }
        self.commit_after = version.sequence;
        self.stage(WorkflowTraceDraftEvent::simple(
            WorkflowTraceEventKind::DeadlineObserved,
            MissOutcome::OnTime as u16,
            StructuredInstanceHandle(0),
            None,
            None,
            None,
            None,
            None,
        ))?;
        self.stage_terminal(WorkflowTraceEventKind::ScanCommitted)
    }

    /// 标记 transaction 已丢弃；commit sequence 必须保持不变。
    ///
    /// # Errors
    /// draft 非 staging 时拒绝。
    pub fn finalize_discarded(&mut self, discard: CycleDiscard) -> Result<(), WorkflowTraceError> {
        let identity = self.identity.ok_or(WorkflowTraceError::InvalidLifecycle)?;
        let reset = discard.fault().reset_request;
        if self.state != RecorderState::Staging
            || discard.identity() != identity
            || discard.commit_before() != self.commit_before
            || reset.engine_epoch != identity.engine_epoch
            || reset.task_handle != identity.task_handle
            || reset.task_epoch != identity.task_epoch
        {
            return Err(WorkflowTraceError::InvalidLifecycle);
        }
        self.commit_after = self.commit_before;
        self.discard_commit_dependent_events();
        self.stage_terminal(WorkflowTraceEventKind::ScanDiscarded)
    }

    /// 使用 Control Engine 的不可伪造失败 receipt 标记真实 finish deadline miss。
    ///
    /// 只接受 `TransactionError::DeadlineMissed`；Fault、时钟错误和其他 transaction 失败
    /// 不得改写成 deadline 结果。失败 release 不推进 commit sequence。
    ///
    /// # Errors
    /// receipt identity、`commit_before`、失败种类或 recorder 生命周期不匹配时拒绝。
    pub fn finalize_deadline_discarded(
        &mut self,
        failure: CycleFinishFailure,
    ) -> Result<(), WorkflowTraceError> {
        let identity = self.identity.ok_or(WorkflowTraceError::InvalidLifecycle)?;
        if self.state != RecorderState::Staging
            || failure.identity() != identity
            || failure.commit_before() != self.commit_before
            || failure.error() != TransactionError::DeadlineMissed
        {
            return Err(WorkflowTraceError::InvalidLifecycle);
        }
        self.commit_after = self.commit_before;
        self.discard_commit_dependent_events();
        self.stage(WorkflowTraceDraftEvent::simple(
            WorkflowTraceEventKind::DeadlineObserved,
            MissOutcome::FinishAfterDeadline as u16,
            StructuredInstanceHandle(0),
            None,
            None,
            None,
            None,
            None,
        ))?;
        self.stage_terminal(WorkflowTraceEventKind::ScanDiscarded)
    }

    /// 按规范顺序将 finalized release 非阻塞 flush 到 Control Engine producer。
    ///
    /// 每个 record 无论 Published 或 `DropNewest` 都恰好消耗一个 `EventSequence`。publisher
    /// identity/sequence 错误会使 draft 失效，调用方不得重试部分 release；observer 已退出
    /// 等价于 `DropNewest`，只计入丢弃数，不能反向使周期控制失败。
    ///
    /// # Errors
    /// draft 尚未 finalize、序列耗尽、record 契约失败或 producer 拒绝时返回。
    pub fn flush(
        &mut self,
        publisher: &mut WorkflowTracePublisher,
    ) -> Result<WorkflowTraceFlushReport, WorkflowTraceError> {
        if self.state != RecorderState::Finalized {
            return Err(WorkflowTraceError::InvalidLifecycle);
        }
        self.events[..self.event_count].sort_unstable_by_key(event_sort_key);
        let identity = self.identity.ok_or(WorkflowTraceError::InvalidLifecycle)?;
        // 本地契约和 sequence 先完整预检，防止第 N 条 shape 错误造成 release 半发布。
        let mut preflight_sequence = publisher.next_event_sequence();
        for event in &self.events[..self.event_count] {
            let sequence = preflight_sequence.ok_or(WorkflowTraceError::EventSequenceExhausted)?;
            self.build_record(identity, *event, sequence)?;
            preflight_sequence = sequence.checked_next().ok();
        }
        let mut published_count = 0_u32;
        let mut dropped_newest = 0_u32;
        for index in 0..self.event_count {
            let event = self.events[index];
            let sequence = publisher
                .next_event_sequence()
                .ok_or(WorkflowTraceError::EventSequenceExhausted)?;
            let record = self.build_record(identity, event, sequence)?;
            match publisher.try_publish(record) {
                Ok(WorkflowTracePublishOutcome::Published(_)) => published_count += 1,
                Ok(WorkflowTracePublishOutcome::DroppedNewest(_)) => dropped_newest += 1,
                Err(WorkflowTracePublishError::ObserverDropped) => {
                    dropped_newest += 1;
                }
                Err(error) => {
                    self.state = RecorderState::Invalid;
                    return Err(WorkflowTraceError::Publish(error));
                }
            }
        }
        self.state = RecorderState::Idle;
        self.identity = None;
        Ok(WorkflowTraceFlushReport {
            published: published_count,
            dropped_newest,
        })
    }

    fn build_record(
        &self,
        identity: CycleIdentity,
        event: WorkflowTraceDraftEvent,
        sequence: EventSequence,
    ) -> Result<WorkflowTraceRecord, WorkflowTraceError> {
        let after = if event.kind == WorkflowTraceEventKind::ScanCommitted {
            self.commit_after
        } else {
            self.commit_before
        };
        WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            event.kind,
            event.detail,
            identity.task_handle,
            event.workflow_instance,
            event.node,
            event.edge,
            event.source,
            event.value,
            event.branch_order,
            event.execution_order,
            event.type_handle,
            event.fault,
            identity.engine_epoch,
            identity.task_epoch,
            sequence,
            identity.release_sequence,
            self.commit_before,
            after,
            event.fragment,
        )
        .map_err(WorkflowTraceError::Contract)
    }

    fn read_watch_byte(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
        watch: WorkflowTraceWatchBinding,
        relative: usize,
    ) -> Result<u8, WorkflowTraceError> {
        let raw = watch
            .offset
            .checked_add(relative)
            .ok_or(WorkflowTraceError::InvalidLifecycle)?;
        match watch.area {
            WorkflowTraceWatchArea::State => {
                let index = self
                    .application_state_offset
                    .checked_add(raw)
                    .ok_or(WorkflowTraceError::InvalidLifecycle)?;
                cycle
                    .read_state(WorkSetIndex::new(index))
                    .map_err(WorkflowTraceError::Transaction)
            }
            WorkflowTraceWatchArea::Output => cycle
                .read_output(WorkSetIndex::new(raw))
                .map_err(WorkflowTraceError::Transaction),
        }
    }

    fn stage_terminal(&mut self, kind: WorkflowTraceEventKind) -> Result<(), WorkflowTraceError> {
        if self.event_count >= self.events.len() {
            self.state = RecorderState::Invalid;
            return Err(WorkflowTraceError::StageCapacityExceeded);
        }
        let ordinal = u32::try_from(self.event_count)
            .map_err(|_| WorkflowTraceError::StageCapacityExceeded)?;
        self.events[self.event_count] = WorkflowTraceDraftEvent {
            kind,
            ordinal,
            ..WorkflowTraceDraftEvent::EMPTY
        };
        self.event_count += 1;
        self.state = RecorderState::Finalized;
        Ok(())
    }

    fn discard_commit_dependent_events(&mut self) {
        let mut retained = 0_usize;
        for index in 0..self.event_count {
            let event = self.events[index];
            if matches!(
                event.kind,
                WorkflowTraceEventKind::CancelApplied
                    | WorkflowTraceEventKind::WorkflowCompleted
                    | WorkflowTraceEventKind::WatchedValue
            ) {
                continue;
            }
            self.events[retained] = event;
            retained += 1;
        }
        self.event_count = retained;
    }
}

fn validate_watches(
    watches: &[WorkflowTraceWatchBinding],
    application_state_bytes: usize,
    output_bytes: usize,
) -> Result<(), WorkflowTraceRecorderBuildError> {
    for (index, watch) in watches.iter().enumerate() {
        if watch.workflow_instance.0 == u32::MAX
            || watch.value_handle == u32::MAX
            || watch.type_handle == u32::MAX
            || watch.byte_count == 0
        {
            return Err(WorkflowTraceRecorderBuildError::InvalidWatchBinding);
        }
        let end = watch
            .offset
            .checked_add(watch.byte_count)
            .ok_or(WorkflowTraceRecorderBuildError::CapacityOverflow)?;
        let length = match watch.area {
            WorkflowTraceWatchArea::State => application_state_bytes,
            WorkflowTraceWatchArea::Output => output_bytes,
        };
        if end > length
            || watch.byte_count.div_ceil(32) > usize::from(u16::MAX)
            || watches[..index]
                .iter()
                .any(|other| other.value_handle == watch.value_handle)
        {
            return Err(WorkflowTraceRecorderBuildError::InvalidWatchBinding);
        }
    }
    Ok(())
}

fn event_sort_key(event: &WorkflowTraceDraftEvent) -> (u8, u32, u8, u16, u32, u32, u16, u32) {
    let tier = match event.kind {
        WorkflowTraceEventKind::WorkflowInitialized => 0,
        WorkflowTraceEventKind::ForceObserved
        | WorkflowTraceEventKind::FallbackObserved
        | WorkflowTraceEventKind::DeadlineObserved => 1,
        WorkflowTraceEventKind::NodeExecuted
        | WorkflowTraceEventKind::OutputStaged
        | WorkflowTraceEventKind::TransitionTaken
        | WorkflowTraceEventKind::ForkActivated => 2,
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
    };
    let node_phase = match event.kind {
        WorkflowTraceEventKind::OutputStaged => 1,
        WorkflowTraceEventKind::TransitionTaken => 2,
        WorkflowTraceEventKind::ForkActivated => 3,
        _ => 0,
    };
    (
        tier,
        event.execution_order.unwrap_or(u32::MAX),
        node_phase,
        event.kind as u16,
        event.value.unwrap_or(u32::MAX),
        event.branch_order.unwrap_or(u32::MAX),
        event.fragment.index,
        event.ordinal,
    )
}

#[cfg(test)]
mod tests {
    use aurora_control_contracts::WorkflowTraceEventKind;

    use super::{WorkflowTraceDraftEvent, event_sort_key};
    use crate::{StructuredInstanceHandle, WorkflowEdgeHandle, WorkflowNodeHandle};

    #[test]
    fn node_phase_orders_output_before_transition_and_fork() {
        let node = WorkflowNodeHandle::new(0).ok();
        let edge = WorkflowEdgeHandle::new(0).ok();
        let event = |kind| {
            WorkflowTraceDraftEvent::simple(
                kind,
                if kind == WorkflowTraceEventKind::OutputStaged {
                    2
                } else {
                    0
                },
                StructuredInstanceHandle(0),
                node,
                if matches!(
                    kind,
                    WorkflowTraceEventKind::TransitionTaken | WorkflowTraceEventKind::ForkActivated
                ) {
                    edge
                } else {
                    None
                },
                if kind == WorkflowTraceEventKind::OutputStaged {
                    Some(0)
                } else {
                    None
                },
                if kind == WorkflowTraceEventKind::ForkActivated {
                    Some(0)
                } else {
                    None
                },
                Some(0),
            )
        };
        let node = event(WorkflowTraceEventKind::NodeExecuted);
        let output = event(WorkflowTraceEventKind::OutputStaged);
        let transition = event(WorkflowTraceEventKind::TransitionTaken);
        let fork = event(WorkflowTraceEventKind::ForkActivated);
        assert!(event_sort_key(&node) < event_sort_key(&output));
        assert!(event_sort_key(&output) < event_sort_key(&transition));
        assert!(event_sort_key(&transition) < event_sort_key(&fork));
    }

    #[test]
    fn structural_events_use_kind_before_optional_branch_order() {
        let join = WorkflowTraceDraftEvent::simple(
            WorkflowTraceEventKind::JoinSatisfied,
            2,
            StructuredInstanceHandle(0),
            WorkflowNodeHandle::new(3).ok(),
            None,
            None,
            None,
            Some(3),
        );
        let cancel = WorkflowTraceDraftEvent::simple(
            WorkflowTraceEventKind::CancelRequested,
            1,
            StructuredInstanceHandle(0),
            WorkflowNodeHandle::new(3).ok(),
            None,
            None,
            Some(0),
            Some(3),
        );
        assert!(event_sort_key(&join) < event_sort_key(&cancel));
    }
}
