//! Deadline miss、任务健康状态与不可覆盖 Fallback mailbox。
//!
//! 本模块只协调 R0 已有的调度结果和事务故障，不复制调度器、事务 bank 或契约序列。
//! miss 环形窗口在初始化期分配；周期更新最多读取固定 `MissWindow` 个槽定位首个阈值，
//! 再最多更新同样数量的槽，超大 skipped 批次的其余部分以常数次算术折叠。

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use aurora_control_contracts::{
    CommitSequence, ExecutionContractError, FallbackRequest, FallbackRequestSequence, FaultReason,
    MissOutcome, OutputSetIdentity, ReleaseSequence, TaskSpec, TaskState,
};
use aurora_types::{BootEpochId, MonotonicTimestamp};

use crate::{CycleCommit, LatchedTaskFault, ResetRequest, SkippedReleases, TaskTransaction};

/// Fallback 单槽 mailbox 的显式生命周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackMailboxState {
    /// 尚无请求，允许发布一个新请求。
    Empty,
    /// 请求等待精确确认；不得被另一请求覆盖。
    Pending,
    /// 同一请求已经确认，等待对应任务完成 reinitialize。
    Acknowledged,
}

/// miss 窗口和当前 task epoch 累计计数的只读快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskHealthStatistics {
    /// 固定 miss 窗口容量。
    pub window_capacity: u32,
    /// 当前窗口已写入的 release 数；启动后逐步增长到容量。
    pub retained_releases: u32,
    /// 当前窗口内的 deadline miss 数。
    pub window_misses: u32,
    /// 当前连续 miss 数。
    pub consecutive_misses: u64,
    /// 当前 task epoch 观察到的 scheduled release 总数。
    pub scheduled_releases: u64,
    /// 当前 task epoch 观察到的 deadline miss 总数。
    pub deadline_misses: u64,
    /// 下一项必须消费的 release；`None` 表示序列已经耗尽且不得继续执行。
    pub next_release_sequence: Option<ReleaseSequence>,
    /// 最近一次成功提交是否超过 `ExecutionBudget`。
    pub last_success_budget_exceeded: bool,
    /// 任一累计计数是否已经在 `u64::MAX` 饱和。
    pub saturated: bool,
}

/// R0-06 状态协调、mailbox 或证据错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStateMachineError {
    /// 初始化期无法为固定 miss 窗口预分配存储。
    MissWindowAllocationFailed {
        /// 请求的固定窗口槽位数。
        window_capacity: u32,
    },
    /// 传入事务不属于本状态机，或其版本未经过本状态机推进。
    TaskIdentityMismatch,
    /// 状态机只能随首次 task epoch 的声明初值建立，不能在运行中补建而遗漏历史。
    InvalidInitialTaskVersion,
    /// release 不是当前 task epoch 严格连续的下一项；拒绝且不改变统计。
    UnexpectedReleaseSequence {
        /// 当前应消费的 release；`None` 表示计数已耗尽。
        expected: Option<ReleaseSequence>,
        /// 调用方提交的 release。
        actual: ReleaseSequence,
    },
    /// skipped range 的 first/last/count 不能表示同一连续区间。
    InvalidSkippedRange,
    /// 单项 miss API 收到了 `OnTime` 或 `SkippedRelease`。
    InvalidMissOutcome,
    /// 成功提交证据包含 deadline/HardLimit 超限或版本不连续。
    InvalidCommitEvidence,
    /// 任务已锁定；后续 release 不得继续计数、执行或提交。
    FaultLocked(LatchedTaskFault),
    /// 事务尚未锁存可发布的故障。
    MissingTaskFault,
    /// ack 未完全匹配 pending 请求的 engine/task epoch 与 request sequence。
    FallbackAckMismatch,
    /// reset 前尚未精确确认当前 Fallback 请求。
    FallbackNotAcknowledged,
    /// Fallback 请求无法写入固定 mailbox；已设置 engine-level sticky Fault。
    FallbackPublicationFault,
    /// 当前状态不允许请求的 stop/reset 状态转移。
    InvalidStateTransition,
    /// 版本或请求序列耗尽，禁止回绕。
    CounterOverflow,
    /// 底层契约值校验失败。
    Contract(ExecutionContractError),
}

impl Display for TaskStateMachineError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "task state-machine error: {self:?}")
    }
}

impl Error for TaskStateMachineError {}

impl From<ExecutionContractError> for TaskStateMachineError {
    fn from(value: ExecutionContractError) -> Self {
        Self::Contract(value)
    }
}

/// 一个任务的固定 miss 窗口、健康状态和 Fallback 单槽 mailbox。
///
/// 调用方把 [`SkippedReleases`]、未开始/完成超时结果、成功 [`CycleCommit`] 或事务已经
/// 锁存的故障依次交给本对象。所有更新需要独占借用，因此同一任务不会生成两个并发
/// Fallback 请求；重复同步同一故障是幂等读取，不会多消耗 request sequence。
#[derive(Debug)]
pub struct TaskStateMachine {
    task_spec: TaskSpec,
    engine_epoch: BootEpochId,
    task_epoch: u64,
    commit_sequence: CommitSequence,
    output_set: OutputSetIdentity,
    state: TaskState,
    miss_history: MissHistory,
    fault: Option<LatchedTaskFault>,
    mailbox: FallbackMailbox,
    next_request_sequence: Option<FallbackRequestSequence>,
    engine_faulted: bool,
}

impl TaskStateMachine {
    /// 从首次 task epoch 的声明初值创建 Running 状态机并预分配 miss 窗口。
    ///
    /// # Errors
    ///
    /// 非 epoch 1 / commit 0、已锁定事务或 miss 窗口无法预分配时拒绝构造；
    /// 不安装会遗漏既有 release/Fallback 历史的部分状态机。
    pub fn new(
        task: &TaskTransaction,
        output_set: OutputSetIdentity,
    ) -> Result<Self, TaskStateMachineError> {
        let version = task.diagnostic().version();
        if task.fault().is_some()
            || version.task_epoch.get() != 1
            || version.sequence != CommitSequence::ZERO
        {
            return Err(TaskStateMachineError::InvalidInitialTaskVersion);
        }
        Ok(Self {
            task_spec: task.spec(),
            engine_epoch: task.engine_epoch(),
            task_epoch: version.task_epoch.get(),
            commit_sequence: version.sequence,
            output_set,
            state: TaskState::Running,
            miss_history: MissHistory::new(task.spec().miss_policy())?,
            fault: None,
            mailbox: FallbackMailbox::Empty,
            next_request_sequence: Some(FallbackRequestSequence::ZERO),
            engine_faulted: false,
        })
    }

    /// 返回当前任务状态。
    #[must_use]
    pub const fn state(&self) -> TaskState {
        self.state
    }

    /// 返回当前 task epoch 的健康统计。
    #[must_use]
    pub fn statistics(&self) -> TaskHealthStatistics {
        self.miss_history.statistics()
    }

    /// 返回 mailbox 生命周期；不确认或清除请求。
    #[must_use]
    pub const fn mailbox_state(&self) -> FallbackMailboxState {
        self.mailbox.state()
    }

    /// 幂等读取当前 pending/acknowledged 请求。
    #[must_use]
    pub const fn fallback_request(&self) -> Option<FallbackRequest> {
        self.mailbox.request()
    }

    /// 返回 Fallback publication 是否曾失败。该标志只随进程重启清除。
    #[must_use]
    pub const fn engine_faulted(&self) -> bool {
        self.engine_faulted
    }

    /// 用有界批量算法记录调度器折叠出的连续 skipped releases。
    ///
    /// 如果阈值在批次中越过，事务先锁定，再发布恰好一个 Fallback 请求；当前被选中的
    /// 后续 release 作为未执行 miss 计入一次，并且不得再传给 `TaskTransaction::begin`。
    ///
    /// # Errors
    ///
    /// 拒绝空/不连续 range、错误事务身份、已锁定状态或 Fallback publication 失败。
    pub fn record_skipped_releases(
        &mut self,
        task: &mut TaskTransaction,
        skipped: SkippedReleases,
        observed_at: MonotonicTimestamp,
    ) -> Result<TaskState, TaskStateMachineError> {
        self.ensure_active_task(task)?;
        self.ensure_expected_release(skipped.first())?;
        let expected_last = skipped
            .first()
            .get()
            .checked_add(skipped.count().saturating_sub(1))
            .ok_or(TaskStateMachineError::InvalidSkippedRange)?;
        if skipped.count() == 0 || expected_last != skipped.last().get() {
            return Err(TaskStateMachineError::InvalidSkippedRange);
        }
        let selected_release = skipped
            .last()
            .checked_next()
            .map_err(|_| TaskStateMachineError::InvalidSkippedRange)?;
        let update = self.miss_history.record_misses(skipped.count());
        self.miss_history.next_release_sequence = Some(selected_release);
        if let Some(fault) = update.fault {
            // 调度器已经消费紧随 skipped range 的当前 release；阈值锁定后它不得执行，
            // 因此作为未执行 miss 计入，但不能生成第二个 Fault/Fallback。
            self.miss_history.record_misses(1);
            self.miss_history.next_release_sequence = selected_release.checked_next().ok();
            let release = skipped
                .first()
                .get()
                .checked_add(fault.offset)
                .map(ReleaseSequence::new)
                .ok_or(TaskStateMachineError::CounterOverflow)?;
            return self.lock_and_publish(task, fault.reason, release, observed_at);
        }
        self.state = TaskState::Degraded;
        Ok(self.state)
    }

    /// 记录一个 `StartAfterDeadline` 或 `FinishAfterDeadline`。
    ///
    /// # Errors
    ///
    /// `OnTime`、`SkippedRelease`、错误事务身份、锁定状态或 publication 失败均显式返回。
    pub fn record_deadline_miss(
        &mut self,
        task: &mut TaskTransaction,
        outcome: MissOutcome,
        release_sequence: ReleaseSequence,
        observed_at: MonotonicTimestamp,
    ) -> Result<TaskState, TaskStateMachineError> {
        if !matches!(
            outcome,
            MissOutcome::StartAfterDeadline | MissOutcome::FinishAfterDeadline
        ) {
            return Err(TaskStateMachineError::InvalidMissOutcome);
        }
        self.ensure_active_task(task)?;
        self.ensure_expected_release(release_sequence)?;
        let update = self.miss_history.record_misses(1);
        self.miss_history.next_release_sequence = release_sequence.checked_next().ok();
        if let Some(fault) = update.fault {
            return self.lock_and_publish(task, fault.reason, release_sequence, observed_at);
        }
        self.state = TaskState::Degraded;
        Ok(self.state)
    }

    /// 接受一次事务已经整体提交的 `OnTime` 结果并更新预算降级。
    ///
    /// # Errors
    ///
    /// commit 不是严格连续的 release 和本 task epoch 下一提交版本，或 checkpoint 显示
    /// deadline/HardLimit 超限时拒绝；拒绝不会改变窗口或状态。
    pub fn record_commit(
        &mut self,
        task: &TaskTransaction,
        commit: CycleCommit,
    ) -> Result<TaskState, TaskStateMachineError> {
        self.ensure_base_identity(task)?;
        if let Some(fault) = self.fault.or_else(|| task.fault()) {
            return Err(TaskStateMachineError::FaultLocked(fault));
        }
        if !matches!(self.state, TaskState::Running | TaskState::Degraded) {
            return Err(TaskStateMachineError::InvalidStateTransition);
        }
        self.ensure_expected_release(commit.release_sequence)?;
        let expected = self
            .commit_sequence
            .checked_next()
            .map_err(|_| TaskStateMachineError::CounterOverflow)?;
        if commit.version != task.diagnostic().version()
            || commit.version.task_epoch.get() != self.task_epoch
            || commit.version.sequence != expected
            || commit.checkpoint.hard_limit_exceeded()
            || commit.checkpoint.deadline_missed()
        {
            return Err(TaskStateMachineError::InvalidCommitEvidence);
        }
        self.commit_sequence = commit.version.sequence;
        self.miss_history
            .record_on_time(commit.checkpoint.execution_budget_exceeded());
        self.miss_history.next_release_sequence = commit.release_sequence.checked_next().ok();
        self.state = self.miss_history.healthy_state();
        Ok(self.state)
    }

    /// 把事务层已经锁存的 HardLimit、执行、容量、时钟或计数故障发布到 mailbox。
    /// 重复传入同一个锁存故障只返回既有状态，不会生成额外请求。
    ///
    /// # Errors
    ///
    /// 事务未锁存故障、身份不匹配或 publication 失败时返回显式错误。
    pub fn synchronize_fault(
        &mut self,
        task: &TaskTransaction,
        release_sequence: ReleaseSequence,
        observed_at: MonotonicTimestamp,
    ) -> Result<TaskState, TaskStateMachineError> {
        self.ensure_base_identity(task)?;
        let fault = task
            .fault()
            .ok_or(TaskStateMachineError::MissingTaskFault)?;
        let version = task.diagnostic().version();
        if version.task_epoch.get() != self.task_epoch || version.sequence != self.commit_sequence {
            self.state = TaskState::FaultLocked;
            self.fault = Some(fault);
            return self.fail_publication();
        }
        self.publish_fault(fault, release_sequence, observed_at)
    }

    /// 精确确认当前请求。重复提交同一 ack 是幂等成功，旧或错误 ack 被拒绝。
    ///
    /// # Errors
    ///
    /// 没有请求或身份不完全匹配时返回 [`TaskStateMachineError::FallbackAckMismatch`]。
    pub fn acknowledge_fallback(
        &mut self,
        engine_epoch: BootEpochId,
        task_epoch: aurora_control_contracts::TaskEpoch,
        request_sequence: FallbackRequestSequence,
    ) -> Result<FallbackRequest, TaskStateMachineError> {
        self.mailbox
            .acknowledge(engine_epoch, task_epoch, request_sequence)
    }

    /// 在外部授权通过、事务开始 reset 前进入 `Reinitializing`。
    ///
    /// # Errors
    ///
    /// 请求不是当前 fault 身份或 Fallback 尚未确认时拒绝，状态保持 `FaultLocked`。
    pub fn begin_reinitialization(
        &mut self,
        request: ResetRequest,
    ) -> Result<(), TaskStateMachineError> {
        if self.state != TaskState::FaultLocked
            || self.fault.map(|value| value.reset_request) != Some(request)
        {
            return Err(TaskStateMachineError::InvalidStateTransition);
        }
        if !self.mailbox.acknowledged_reset(request) {
            return Err(TaskStateMachineError::FallbackNotAcknowledged);
        }
        self.state = TaskState::Reinitializing;
        Ok(())
    }

    /// reset 在发布新 task epoch 前被拒绝时恢复 `FaultLocked`，保留原请求和统计。
    ///
    /// # Errors
    ///
    /// 仅允许从 `Reinitializing` 调用。
    pub fn reject_reinitialization(&mut self) -> Result<(), TaskStateMachineError> {
        if self.state != TaskState::Reinitializing {
            return Err(TaskStateMachineError::InvalidStateTransition);
        }
        self.state = TaskState::FaultLocked;
        Ok(())
    }

    /// 在事务成功发布新 epoch/commit 0 后清空旧 miss/预算历史与已确认 mailbox。
    ///
    /// # Errors
    ///
    /// 事务仍有故障、epoch 未严格递增或 commit 不是零时拒绝并保持 Reinitializing。
    pub fn complete_reinitialization(
        &mut self,
        task: &TaskTransaction,
    ) -> Result<TaskState, TaskStateMachineError> {
        if self.state != TaskState::Reinitializing
            || task.spec() != self.task_spec
            || task.engine_epoch() != self.engine_epoch
            || task.fault().is_some()
        {
            return Err(TaskStateMachineError::InvalidStateTransition);
        }
        let version = task.diagnostic().version();
        let expected_epoch = self
            .task_epoch
            .checked_add(1)
            .ok_or(TaskStateMachineError::CounterOverflow)?;
        if version.task_epoch.get() != expected_epoch || version.sequence != CommitSequence::ZERO {
            return Err(TaskStateMachineError::InvalidStateTransition);
        }
        self.task_epoch = expected_epoch;
        self.commit_sequence = CommitSequence::ZERO;
        self.miss_history.clear();
        self.fault = None;
        self.mailbox = FallbackMailbox::Empty;
        self.state = TaskState::Running;
        Ok(self.state)
    }

    /// 正常边界停止；不会生成 Fallback 请求。
    ///
    /// # Errors
    ///
    /// `FaultLocked` 或 `Reinitializing` 不能伪装为正常停止。
    pub fn stop(&mut self) -> Result<(), TaskStateMachineError> {
        if !matches!(self.state, TaskState::Running | TaskState::Degraded) {
            return Err(TaskStateMachineError::InvalidStateTransition);
        }
        self.state = TaskState::Stopped;
        Ok(())
    }

    fn lock_and_publish(
        &mut self,
        task: &mut TaskTransaction,
        reason: FaultReason,
        release_sequence: ReleaseSequence,
        observed_at: MonotonicTimestamp,
    ) -> Result<TaskState, TaskStateMachineError> {
        let fault = task.lock_fault(reason);
        self.publish_fault(fault, release_sequence, observed_at)
    }

    fn publish_fault(
        &mut self,
        fault: LatchedTaskFault,
        release_sequence: ReleaseSequence,
        observed_at: MonotonicTimestamp,
    ) -> Result<TaskState, TaskStateMachineError> {
        if let Some(existing) = self.fault {
            if existing == fault {
                if self.mailbox.request().is_some() {
                    self.state = TaskState::FaultLocked;
                    return Ok(self.state);
                }
                return self.fail_publication();
            }
            if self.state != TaskState::Reinitializing
                || self.mailbox.state() != FallbackMailboxState::Acknowledged
            {
                return self.fail_publication();
            }
            self.mailbox = FallbackMailbox::Empty;
        }

        // 先锁定状态，再尝试 publication；任何后续失败都不能恢复执行资格。
        self.state = TaskState::FaultLocked;
        self.fault = Some(fault);
        if self.mailbox.state() != FallbackMailboxState::Empty {
            return self.fail_publication();
        }
        let Some(request_sequence) = self.next_request_sequence else {
            return self.fail_publication();
        };
        let request = FallbackRequest::new(
            self.task_spec.version(),
            self.engine_epoch,
            self.task_spec.handle(),
            fault.reset_request.task_epoch,
            fault.reset_request.fault_generation,
            request_sequence,
            fault.reason,
            self.output_set,
            release_sequence,
            self.commit_sequence,
            observed_at,
        )
        .map_err(|_| {
            self.engine_faulted = true;
            TaskStateMachineError::FallbackPublicationFault
        })?;
        self.next_request_sequence = request_sequence.checked_next().ok();
        self.mailbox = FallbackMailbox::Pending(request);
        Ok(self.state)
    }

    fn fail_publication<T>(&mut self) -> Result<T, TaskStateMachineError> {
        self.engine_faulted = true;
        Err(TaskStateMachineError::FallbackPublicationFault)
    }

    fn ensure_active_task(&self, task: &TaskTransaction) -> Result<(), TaskStateMachineError> {
        self.ensure_task_identity(task)?;
        if let Some(fault) = self.fault.or_else(|| task.fault()) {
            return Err(TaskStateMachineError::FaultLocked(fault));
        }
        if !matches!(self.state, TaskState::Running | TaskState::Degraded) {
            return Err(TaskStateMachineError::InvalidStateTransition);
        }
        Ok(())
    }

    fn ensure_task_identity(&self, task: &TaskTransaction) -> Result<(), TaskStateMachineError> {
        let version = task.diagnostic().version();
        self.ensure_base_identity(task)?;
        if version.task_epoch.get() != self.task_epoch || version.sequence != self.commit_sequence {
            return Err(TaskStateMachineError::TaskIdentityMismatch);
        }
        Ok(())
    }

    fn ensure_base_identity(&self, task: &TaskTransaction) -> Result<(), TaskStateMachineError> {
        if task.spec() != self.task_spec || task.engine_epoch() != self.engine_epoch {
            return Err(TaskStateMachineError::TaskIdentityMismatch);
        }
        Ok(())
    }

    fn ensure_expected_release(
        &self,
        actual: ReleaseSequence,
    ) -> Result<(), TaskStateMachineError> {
        if self.miss_history.next_release_sequence != Some(actual) {
            return Err(TaskStateMachineError::UnexpectedReleaseSequence {
                expected: self.miss_history.next_release_sequence,
                actual,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum FallbackMailbox {
    Empty,
    Pending(FallbackRequest),
    Acknowledged(FallbackRequest),
}

impl FallbackMailbox {
    const fn state(self) -> FallbackMailboxState {
        match self {
            Self::Empty => FallbackMailboxState::Empty,
            Self::Pending(_) => FallbackMailboxState::Pending,
            Self::Acknowledged(_) => FallbackMailboxState::Acknowledged,
        }
    }

    const fn request(self) -> Option<FallbackRequest> {
        match self {
            Self::Empty => None,
            Self::Pending(request) | Self::Acknowledged(request) => Some(request),
        }
    }

    fn acknowledge(
        &mut self,
        engine_epoch: BootEpochId,
        task_epoch: aurora_control_contracts::TaskEpoch,
        request_sequence: FallbackRequestSequence,
    ) -> Result<FallbackRequest, TaskStateMachineError> {
        let Some(request) = self.request() else {
            return Err(TaskStateMachineError::FallbackAckMismatch);
        };
        if !request.matches_ack(engine_epoch, task_epoch, request_sequence) {
            return Err(TaskStateMachineError::FallbackAckMismatch);
        }
        *self = Self::Acknowledged(request);
        Ok(request)
    }

    fn acknowledged_reset(self, reset: ResetRequest) -> bool {
        let Self::Acknowledged(request) = self else {
            return false;
        };
        request.engine_epoch() == reset.engine_epoch
            && request.task_handle() == reset.task_handle
            && request.task_epoch() == reset.task_epoch
            && request.fault_generation() == reset.fault_generation
    }
}

#[derive(Debug)]
struct MissHistory {
    slots: Box<[u8]>,
    window_capacity: u32,
    write_index: usize,
    retained_releases: u32,
    window_misses: u32,
    consecutive_misses: u64,
    scheduled_releases: u64,
    deadline_misses: u64,
    next_release_sequence: Option<ReleaseSequence>,
    last_success_budget_exceeded: bool,
    saturated: bool,
    max_misses: u32,
    consecutive_threshold: u32,
}

impl MissHistory {
    fn new(policy: aurora_control_contracts::MissPolicy) -> Result<Self, TaskStateMachineError> {
        let window = policy.window().get();
        let mut slots = Vec::new();
        slots.try_reserve_exact(window as usize).map_err(|_| {
            TaskStateMachineError::MissWindowAllocationFailed {
                window_capacity: window,
            }
        })?;
        slots.resize(window as usize, 0);
        Ok(Self {
            slots: slots.into_boxed_slice(),
            window_capacity: window,
            write_index: 0,
            retained_releases: 0,
            window_misses: 0,
            consecutive_misses: 0,
            scheduled_releases: 0,
            deadline_misses: 0,
            next_release_sequence: Some(ReleaseSequence::ZERO),
            last_success_budget_exceeded: false,
            saturated: false,
            max_misses: policy.max_misses(),
            consecutive_threshold: policy.consecutive_misses(),
        })
    }

    fn statistics(&self) -> TaskHealthStatistics {
        TaskHealthStatistics {
            window_capacity: self.window_capacity,
            retained_releases: self.retained_releases,
            window_misses: self.window_misses,
            consecutive_misses: self.consecutive_misses,
            scheduled_releases: self.scheduled_releases,
            deadline_misses: self.deadline_misses,
            next_release_sequence: self.next_release_sequence,
            last_success_budget_exceeded: self.last_success_budget_exceeded,
            saturated: self.saturated,
        }
    }

    fn record_misses(&mut self, count: u64) -> MissBatchUpdate {
        saturating_add(&mut self.scheduled_releases, count, &mut self.saturated);
        saturating_add(&mut self.deadline_misses, count, &mut self.saturated);
        let fault = self.first_threshold_fault(count);
        self.append_misses(count);
        MissBatchUpdate { fault }
    }

    fn record_on_time(&mut self, budget_exceeded: bool) {
        saturating_add(&mut self.scheduled_releases, 1, &mut self.saturated);
        self.append(false);
        self.consecutive_misses = 0;
        self.last_success_budget_exceeded = budget_exceeded;
    }

    fn healthy_state(&self) -> TaskState {
        if self.window_misses == 0 && !self.last_success_budget_exceeded {
            TaskState::Running
        } else {
            TaskState::Degraded
        }
    }

    fn clear(&mut self) {
        self.slots.fill(0);
        self.write_index = 0;
        self.retained_releases = 0;
        self.window_misses = 0;
        self.consecutive_misses = 0;
        self.scheduled_releases = 0;
        self.deadline_misses = 0;
        self.next_release_sequence = Some(ReleaseSequence::ZERO);
        self.last_success_budget_exceeded = false;
        self.saturated = false;
    }

    fn first_threshold_fault(&self, count: u64) -> Option<MissThresholdFault> {
        let mut retained_releases = self.retained_releases;
        let mut window_misses = self.window_misses;
        let mut read_index = self.write_index;
        let bounded_count = count.min(u64::from(self.window_capacity));
        for offset in 0..bounded_count {
            if retained_releases == self.window_capacity {
                window_misses -= u32::from(self.slots[read_index]);
            } else {
                retained_releases += 1;
            }
            window_misses += 1;
            read_index += 1;
            if read_index == self.slots.len() {
                read_index = 0;
            }

            // 同一 release 同时越过阈值时，连续 miss 是更具体的直接触发原因。
            let consecutive = self.consecutive_misses.saturating_add(offset + 1);
            let reason = if consecutive >= u64::from(self.consecutive_threshold) {
                Some(FaultReason::ConsecutiveMissesReached)
            } else if window_misses > self.max_misses {
                Some(FaultReason::MissWindowExceeded)
            } else {
                None
            };
            if let Some(reason) = reason {
                return Some(MissThresholdFault { reason, offset });
            }
        }
        None
    }

    fn append(&mut self, missed: bool) {
        let old = self.slots[self.write_index];
        if self.retained_releases == self.window_capacity {
            self.window_misses -= u32::from(old);
        } else {
            self.retained_releases += 1;
        }
        let value = u8::from(missed);
        self.slots[self.write_index] = value;
        self.window_misses += u32::from(value);
        self.write_index += 1;
        if self.write_index == self.slots.len() {
            self.write_index = 0;
        }
    }

    fn append_misses(&mut self, count: u64) {
        if count == 0 {
            return;
        }
        let capacity = u64::from(self.window_capacity);
        if count >= capacity {
            self.slots.fill(1);
            self.retained_releases = self.window_capacity;
            self.window_misses = self.window_capacity;
            // 余数严格小于初始化期固定窗口；只推进索引，不重复改写窗口槽。
            for _ in 0..count % capacity {
                self.write_index += 1;
                if self.write_index == self.slots.len() {
                    self.write_index = 0;
                }
            }
        } else {
            for _ in 0..count {
                self.append(true);
            }
        }
        saturating_add(&mut self.consecutive_misses, count, &mut self.saturated);
    }
}

#[derive(Debug, Clone, Copy)]
struct MissBatchUpdate {
    fault: Option<MissThresholdFault>,
}

#[derive(Debug, Clone, Copy)]
struct MissThresholdFault {
    reason: FaultReason,
    offset: u64,
}

fn saturating_add(value: &mut u64, increment: u64, saturated: &mut bool) {
    if let Some(next) = value.checked_add(increment) {
        *value = next;
    } else {
        *value = u64::MAX;
        *saturated = true;
    }
}

#[cfg(test)]
#[path = "task_state_machine_tests.rs"]
mod tests;
