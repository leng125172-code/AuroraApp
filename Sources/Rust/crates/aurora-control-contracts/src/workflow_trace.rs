//! R2 Cyclic Workflow Trace 的固定语义值。

use aurora_types::{BootEpochId, LocalHandle};
use thiserror::Error;

use crate::{CommitSequence, EventSequence, FaultReason, ReleaseSequence, TaskEpoch};

/// Workflow Trace Preview 布局版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTraceVersion {
    /// 不兼容版本。
    pub major: u16,
    /// 当前 reader 精确接受的次版本。
    pub minor: u16,
}

impl WorkflowTraceVersion {
    /// Preview 1.0。
    pub const V1_0: Self = Self { major: 1, minor: 0 };
}

/// 固定 Workflow Trace 事件种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u16)]
pub enum WorkflowTraceEventKind {
    /// reset/reinitialize 已建立初始活动集。
    WorkflowInitialized = 1,
    /// 一个活动节点已经执行。
    NodeExecuted = 2,
    /// 一条静态控制边被采用。
    TransitionTaken = 3,
    /// Fork 的一个分支 token 被激活。
    ForkActivated = 4,
    /// `Merge`、`JoinAll` 或 `JoinAny` 已满足。
    JoinSatisfied = 5,
    /// Wait 的本次观察结果。
    WaitObserved = 6,
    /// `JoinAny` 败方收到取消请求。
    CancelRequested = 7,
    /// 取消已在规定边界应用。
    CancelApplied = 8,
    /// 子工作流实例已复制输入并激活。
    SubworkflowActivated = 9,
    /// 子工作流实例已复制输出并完成。
    SubworkflowCompleted = 10,
    /// 一个输出值在 staging 中被观察。
    OutputStaged = 11,
    /// 一个编译期 watch 值 fragment。
    WatchedValue = 12,
    /// End 请求顶层完成。
    CompletionRequested = 13,
    /// 顶层工作流完成。
    WorkflowCompleted = 14,
    /// 已确定本 release 的主 Workflow Fault。
    WorkflowFaulted = 15,
    /// 外层提供的显式 Force 状态。
    ForceObserved = 16,
    /// 外层提供的显式 Fallback 状态。
    FallbackObserved = 17,
    /// 外层提供的 deadline 结果。
    DeadlineObserved = 18,
    /// 本 release 已整体提交。
    ScanCommitted = 19,
    /// 本 release 已整体丢弃。
    ScanDiscarded = 20,
}

impl TryFrom<u16> for WorkflowTraceEventKind {
    type Error = WorkflowTraceContractError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::WorkflowInitialized),
            2 => Ok(Self::NodeExecuted),
            3 => Ok(Self::TransitionTaken),
            4 => Ok(Self::ForkActivated),
            5 => Ok(Self::JoinSatisfied),
            6 => Ok(Self::WaitObserved),
            7 => Ok(Self::CancelRequested),
            8 => Ok(Self::CancelApplied),
            9 => Ok(Self::SubworkflowActivated),
            10 => Ok(Self::SubworkflowCompleted),
            11 => Ok(Self::OutputStaged),
            12 => Ok(Self::WatchedValue),
            13 => Ok(Self::CompletionRequested),
            14 => Ok(Self::WorkflowCompleted),
            15 => Ok(Self::WorkflowFaulted),
            16 => Ok(Self::ForceObserved),
            17 => Ok(Self::FallbackObserved),
            18 => Ok(Self::DeadlineObserved),
            19 => Ok(Self::ScanCommitted),
            20 => Ok(Self::ScanDiscarded),
            _ => Err(WorkflowTraceContractError::InvalidEventKind),
        }
    }
}

/// 一段 canonical storage value；无值事件使用 [`WorkflowTraceValueFragment::ABSENT`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTraceValueFragment {
    /// 从零开始的 fragment index。
    pub index: u16,
    /// 完整值的非零 fragment 数。
    pub count: u16,
    /// 当前 fragment 的有效字节数。
    pub bytes: u16,
    /// 完整 canonical storage value 的 SHA-256。
    pub digest: Option<[u8; 32]>,
    /// 当前 fragment，尾部未使用字节必须为零。
    pub storage: [u8; 32],
}

impl WorkflowTraceValueFragment {
    /// 无 fragment 的规范零表示。
    pub const ABSENT: Self = Self {
        index: 0,
        count: 0,
        bytes: 0,
        digest: None,
        storage: [0; 32],
    };

    /// 返回该记录是否携带 fragment。
    #[must_use]
    pub const fn is_present(self) -> bool {
        self.count != 0
    }
}

/// 一个 192-byte record 的完整语义字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTraceRecord {
    version: WorkflowTraceVersion,
    kind: WorkflowTraceEventKind,
    detail: u16,
    task_handle: LocalHandle,
    workflow_instance_handle: u32,
    node_handle: Option<u32>,
    edge_handle: Option<u32>,
    source_handle: Option<u32>,
    value_handle: Option<u32>,
    branch_order: Option<u32>,
    execution_order: Option<u32>,
    type_handle: Option<u32>,
    fault: Option<FaultReason>,
    engine_epoch: BootEpochId,
    task_epoch: TaskEpoch,
    event_sequence: EventSequence,
    release_sequence: ReleaseSequence,
    commit_before: CommitSequence,
    commit_after: CommitSequence,
    fragment: WorkflowTraceValueFragment,
}

impl WorkflowTraceRecord {
    /// 验证并创建一条 Workflow Trace record。
    ///
    /// # Errors
    /// detail、句柄、fragment、Fault 或 commit 转移违反 Preview 1.0 时拒绝。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version: WorkflowTraceVersion,
        kind: WorkflowTraceEventKind,
        detail: u16,
        task_handle: LocalHandle,
        workflow_instance_handle: u32,
        node_handle: Option<u32>,
        edge_handle: Option<u32>,
        source_handle: Option<u32>,
        value_handle: Option<u32>,
        branch_order: Option<u32>,
        execution_order: Option<u32>,
        type_handle: Option<u32>,
        fault: Option<FaultReason>,
        engine_epoch: BootEpochId,
        task_epoch: TaskEpoch,
        event_sequence: EventSequence,
        release_sequence: ReleaseSequence,
        commit_before: CommitSequence,
        commit_after: CommitSequence,
        fragment: WorkflowTraceValueFragment,
    ) -> Result<Self, WorkflowTraceContractError> {
        if version != WorkflowTraceVersion::V1_0 {
            return Err(WorkflowTraceContractError::UnsupportedVersion);
        }
        if workflow_instance_handle == u32::MAX
            || [
                node_handle,
                edge_handle,
                source_handle,
                value_handle,
                branch_order,
                execution_order,
                type_handle,
            ]
            .into_iter()
            .flatten()
            .any(|value| value == u32::MAX)
        {
            return Err(WorkflowTraceContractError::ReservedHandle);
        }
        validate_detail(kind, detail)?;
        validate_commit(kind, commit_before, commit_after)?;
        validate_fault(kind, fault)?;
        validate_fragment(fragment, value_handle, type_handle)?;
        validate_shape(
            kind,
            detail,
            node_handle,
            edge_handle,
            source_handle,
            value_handle,
            branch_order,
            execution_order,
            type_handle,
            fragment,
        )?;
        Ok(Self {
            version,
            kind,
            detail,
            task_handle,
            workflow_instance_handle,
            node_handle,
            edge_handle,
            source_handle,
            value_handle,
            branch_order,
            execution_order,
            type_handle,
            fault,
            engine_epoch,
            task_epoch,
            event_sequence,
            release_sequence,
            commit_before,
            commit_after,
            fragment,
        })
    }

    /// 返回布局版本。
    #[must_use]
    pub const fn version(self) -> WorkflowTraceVersion {
        self.version
    }
    /// 返回事件种类。
    #[must_use]
    pub const fn kind(self) -> WorkflowTraceEventKind {
        self.kind
    }
    /// 返回按事件种类解释的 detail。
    #[must_use]
    pub const fn detail(self) -> u16 {
        self.detail
    }
    /// 返回 R0 task handle。
    #[must_use]
    pub const fn task_handle(self) -> LocalHandle {
        self.task_handle
    }
    /// 返回展开实例 handle。
    #[must_use]
    pub const fn workflow_instance_handle(self) -> u32 {
        self.workflow_instance_handle
    }
    /// 返回可选节点 handle。
    #[must_use]
    pub const fn node_handle(self) -> Option<u32> {
        self.node_handle
    }
    /// 返回可选控制边 handle。
    #[must_use]
    pub const fn edge_handle(self) -> Option<u32> {
        self.edge_handle
    }
    /// 返回可选 Action/POU/Fault site handle。
    #[must_use]
    pub const fn source_handle(self) -> Option<u32> {
        self.source_handle
    }
    /// 返回可选 value handle。
    #[must_use]
    pub const fn value_handle(self) -> Option<u32> {
        self.value_handle
    }
    /// 返回可选 branch order。
    #[must_use]
    pub const fn branch_order(self) -> Option<u32> {
        self.branch_order
    }
    /// 返回可选 execution order。
    #[must_use]
    pub const fn execution_order(self) -> Option<u32> {
        self.execution_order
    }
    /// 返回可选 type handle。
    #[must_use]
    pub const fn type_handle(self) -> Option<u32> {
        self.type_handle
    }
    /// 返回可选 R0 `FaultReason`。
    #[must_use]
    pub const fn fault(self) -> Option<FaultReason> {
        self.fault
    }
    /// 返回 Control Engine epoch。
    #[must_use]
    pub const fn engine_epoch(self) -> BootEpochId {
        self.engine_epoch
    }
    /// 返回 task 初始化代际。
    #[must_use]
    pub const fn task_epoch(self) -> TaskEpoch {
        self.task_epoch
    }
    /// 返回事件尝试序列。
    #[must_use]
    pub const fn event_sequence(self) -> EventSequence {
        self.event_sequence
    }
    /// 返回调度 release 序列。
    #[must_use]
    pub const fn release_sequence(self) -> ReleaseSequence {
        self.release_sequence
    }
    /// 返回事件前 commit 序列。
    #[must_use]
    pub const fn commit_before(self) -> CommitSequence {
        self.commit_before
    }
    /// 返回事件后 commit 序列。
    #[must_use]
    pub const fn commit_after(self) -> CommitSequence {
        self.commit_after
    }
    /// 返回可选 canonical value fragment。
    #[must_use]
    pub const fn fragment(self) -> WorkflowTraceValueFragment {
        self.fragment
    }
}

/// Workflow Trace 语义拒绝原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkflowTraceContractError {
    /// reader/writer 版本不是精确 Preview 1.0。
    #[error("unsupported Workflow Trace version")]
    UnsupportedVersion,
    /// event kind 未知。
    #[error("invalid Workflow Trace event kind")]
    InvalidEventKind,
    /// `TaskEpoch` 为零。
    #[error("invalid Workflow Trace TaskEpoch")]
    InvalidTaskEpoch,
    /// `EventDetail` 不属于当前 event kind。
    #[error("invalid Workflow Trace event detail")]
    InvalidEventDetail,
    /// 必需 handle 使用了 `u32::MAX` sentinel。
    #[error("Workflow Trace handle uses the reserved sentinel")]
    ReservedHandle,
    /// commit before/after 不符合事件语义。
    #[error("invalid Workflow Trace commit transition")]
    InvalidCommitTransition,
    /// `FaultReason` presence 与 event kind 不一致。
    #[error("invalid Workflow Trace FaultReason presence")]
    InvalidFaultPresence,
    /// value fragment、digest、handle 或尾部规范表示无效。
    #[error("invalid Workflow Trace value fragment")]
    InvalidFragment,
    /// event kind 的 required/forbidden optional 字段不闭合。
    #[error("invalid Workflow Trace event shape")]
    InvalidEventShape,
}

fn validate_detail(
    kind: WorkflowTraceEventKind,
    detail: u16,
) -> Result<(), WorkflowTraceContractError> {
    let valid = match kind {
        WorkflowTraceEventKind::JoinSatisfied => (1..=3).contains(&detail),
        WorkflowTraceEventKind::WaitObserved => (1..=6).contains(&detail),
        WorkflowTraceEventKind::CancelRequested
        | WorkflowTraceEventKind::CancelApplied
        | WorkflowTraceEventKind::OutputStaged => (1..=2).contains(&detail),
        WorkflowTraceEventKind::ForceObserved | WorkflowTraceEventKind::FallbackObserved => {
            (1..=3).contains(&detail)
        }
        WorkflowTraceEventKind::DeadlineObserved => (1..=4).contains(&detail),
        _ => detail == 0,
    };
    if valid {
        Ok(())
    } else {
        Err(WorkflowTraceContractError::InvalidEventDetail)
    }
}

fn validate_commit(
    kind: WorkflowTraceEventKind,
    before: CommitSequence,
    after: CommitSequence,
) -> Result<(), WorkflowTraceContractError> {
    let valid = if kind == WorkflowTraceEventKind::ScanCommitted {
        before.checked_next() == Ok(after)
    } else {
        before == after
    };
    if valid {
        Ok(())
    } else {
        Err(WorkflowTraceContractError::InvalidCommitTransition)
    }
}

fn validate_fault(
    kind: WorkflowTraceEventKind,
    fault: Option<FaultReason>,
) -> Result<(), WorkflowTraceContractError> {
    if (kind == WorkflowTraceEventKind::WorkflowFaulted) == fault.is_some() {
        Ok(())
    } else {
        Err(WorkflowTraceContractError::InvalidFaultPresence)
    }
}

fn validate_fragment(
    fragment: WorkflowTraceValueFragment,
    value_handle: Option<u32>,
    type_handle: Option<u32>,
) -> Result<(), WorkflowTraceContractError> {
    if value_handle.is_some() != type_handle.is_some() {
        return Err(WorkflowTraceContractError::InvalidFragment);
    }
    if !fragment.is_present() {
        if fragment != WorkflowTraceValueFragment::ABSENT {
            return Err(WorkflowTraceContractError::InvalidFragment);
        }
        return Ok(());
    }
    let byte_count = usize::from(fragment.bytes);
    let final_index = fragment.count - 1;
    if value_handle.is_none()
        || type_handle.is_none()
        || fragment.digest.is_none()
        || fragment.index >= fragment.count
        || !(1..=32).contains(&fragment.bytes)
        || (fragment.index != final_index && fragment.bytes != 32)
        || fragment.storage[byte_count..].iter().any(|byte| *byte != 0)
    {
        Err(WorkflowTraceContractError::InvalidFragment)
    } else {
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_shape(
    kind: WorkflowTraceEventKind,
    detail: u16,
    node: Option<u32>,
    edge: Option<u32>,
    source: Option<u32>,
    value: Option<u32>,
    branch: Option<u32>,
    execution: Option<u32>,
    value_type: Option<u32>,
    fragment: WorkflowTraceValueFragment,
) -> Result<(), WorkflowTraceContractError> {
    let no_value = value.is_none() && value_type.is_none() && !fragment.is_present();
    let shape = match kind {
        WorkflowTraceEventKind::WorkflowInitialized
        | WorkflowTraceEventKind::WorkflowCompleted
        | WorkflowTraceEventKind::ForceObserved
        | WorkflowTraceEventKind::FallbackObserved
        | WorkflowTraceEventKind::DeadlineObserved
        | WorkflowTraceEventKind::ScanCommitted
        | WorkflowTraceEventKind::ScanDiscarded => {
            node.is_none()
                && edge.is_none()
                && source.is_none()
                && branch.is_none()
                && execution.is_none()
                && no_value
        }
        WorkflowTraceEventKind::NodeExecuted | WorkflowTraceEventKind::CompletionRequested => {
            node.is_some()
                && execution.is_some()
                && edge.is_none()
                && source.is_none()
                && branch.is_none()
                && no_value
        }
        WorkflowTraceEventKind::TransitionTaken => {
            node.is_some()
                && edge.is_some()
                && execution.is_some()
                && source.is_none()
                && branch.is_none()
                && no_value
        }
        WorkflowTraceEventKind::ForkActivated => {
            node.is_some()
                && edge.is_some()
                && branch.is_some()
                && execution.is_some()
                && source.is_none()
                && no_value
        }
        WorkflowTraceEventKind::JoinSatisfied | WorkflowTraceEventKind::WaitObserved => {
            node.is_some() && execution.is_some() && edge.is_none() && source.is_none() && no_value
        }
        WorkflowTraceEventKind::CancelRequested | WorkflowTraceEventKind::CancelApplied => {
            node.is_some()
                && branch.is_some()
                && execution.is_some()
                && edge.is_none()
                && source.is_none()
                && no_value
        }
        WorkflowTraceEventKind::SubworkflowActivated
        | WorkflowTraceEventKind::SubworkflowCompleted => {
            node.is_some()
                && source.is_some()
                && execution.is_some()
                && edge.is_none()
                && branch.is_none()
                && no_value
        }
        WorkflowTraceEventKind::OutputStaged => {
            node.is_some()
                && source.is_some()
                && value.is_some()
                && value_type.is_some()
                && execution.is_some()
                && edge.is_none()
                && branch.is_none()
                && ((detail == 1 && fragment.is_present())
                    || (detail == 2 && !fragment.is_present()))
        }
        WorkflowTraceEventKind::WatchedValue => {
            value.is_some()
                && value_type.is_some()
                && fragment.is_present()
                && node.is_none()
                && edge.is_none()
                && source.is_none()
                && branch.is_none()
                && execution.is_none()
        }
        WorkflowTraceEventKind::WorkflowFaulted => {
            source.is_some()
                && edge.is_none()
                && branch.is_none()
                && value.is_none()
                && value_type.is_none()
                && !fragment.is_present()
        }
    };
    if shape {
        Ok(())
    } else {
        Err(WorkflowTraceContractError::InvalidEventShape)
    }
}
