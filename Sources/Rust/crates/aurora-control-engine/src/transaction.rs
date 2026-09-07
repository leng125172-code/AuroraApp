//! 单写者任务的固定容量 state/output 事务；不包含跨任务 publication slot 或物理 I/O。

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::atomic::{AtomicUsize, Ordering};

use aurora_control_contracts::{
    CommitSequence, ExecutionContractError, FaultGeneration, FaultReason, TaskEpoch, TaskSpec,
};
use aurora_types::{BootEpochId, LocalHandle};

use crate::{
    ExecutionCheckpoint, ExecutionWindow, FixedWorkSet, FixedWorkSetBuilder, MonotonicClock,
    ReleaseDecision, ReleaseReadiness, ScheduleControl, SchedulerError, StaticTaskPlan,
    WorkSetCapacity, WorkSetError, WorkSetIndex, WorkSetLimits,
};

/// 同一次整体提交的任务代际与版本；声明初值的 sequence 为零。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitVersion {
    /// 成功初始化的任务代际。
    pub task_epoch: TaskEpoch,
    /// 该代际内成功周期的计数。
    pub sequence: CommitSequence,
}

/// reset 的完整身份；授权和 Fallback guard 必须验证同一请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetRequest {
    /// 当前进程 epoch。
    pub engine_epoch: BootEpochId,
    /// 静态任务 owner。
    pub task_handle: LocalHandle,
    /// 被复位的任务代际。
    pub task_epoch: TaskEpoch,
    /// 被复位的锁存故障代际。
    pub fault_generation: FaultGeneration,
}

/// 保持到成功 reset 的任务故障；用于后续 R0-06 Fallback 请求构造，不是 mailbox ack。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatchedTaskFault {
    /// reset 必须完全匹配的身份。
    pub reset_request: ResetRequest,
    /// 首个导致锁定的原因。
    pub reason: FaultReason,
}

/// 外部 reset 准入失败；拒绝时不得修改 bank、epoch 或调度序列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetGuardError {
    /// 请求没有获得外部授权。
    Unauthorized,
    /// Fallback 尚未满足恢复前置条件。
    FallbackNotReady,
}

/// reset 授权与 Fallback guard 的外部边界。
///
/// 实现必须检查请求身份对应的授权及 Fallback 状态；不得用无条件成功实现生产准入。
/// 调用发生在任务锁定后的控制边界，必须有界、无分配、无 I/O；实际认证与 R3 IPC 在外部完成。
pub trait ResetGuard {
    /// 检查这一精确请求；拒绝必须返回显式原因。
    ///
    /// # Errors
    /// 返回授权或 Fallback 前置条件失败。
    fn check(&mut self, request: ResetRequest) -> Result<(), ResetGuardError>;
}

/// 声明初值检查失败，不允许发布新 epoch。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitializationRejected;

/// 事务构建、执行或复位的可穷举错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionError {
    /// 固定工作集容量、分配或索引错误。
    WorkSet(WorkSetError),
    /// 基础计数/代际校验失败。
    Contract(ExecutionContractError),
    /// 调度时钟或绝对时间错误。
    Scheduler(SchedulerError),
    /// 声明的 state + output 字节数量溢出。
    ImageSizeOverflow,
    /// 字节索引超出任务私有区域；不能寻址其他任务的 output。
    ImageOutOfRange,
    /// 任务声明、engine/task epoch 不匹配，或计划已经停止。
    PlanMismatch,
    /// 任务保持故障锁定，不能执行或发布。
    FaultLocked(LatchedTaskFault),
    /// 已存在未结束事务（包括被忘记的句柄），不能再次 begin。
    CycleAlreadyActive,
    /// deadline miss 丢弃本周期；是否转 Fault 由 R0-06 miss 准入决定。
    DeadlineMissed,
    /// 无待复位故障，或请求身份已经过期。
    StaleResetRequest,
    /// 外部 reset 准入拒绝。
    ResetDenied(ResetGuardError),
    /// 初始化校验失败；旧 committed 保留，任务继续锁定。
    ReinitializationFailed,
    /// 控制语义计数耗尽，禁止回绕。
    CounterOverflow,
}

impl Display for TransactionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "transaction error: {self:?}")
    }
}

impl Error for TransactionError {}

impl From<WorkSetError> for TransactionError {
    fn from(value: WorkSetError) -> Self {
        Self::WorkSet(value)
    }
}

impl From<ExecutionContractError> for TransactionError {
    fn from(value: ExecutionContractError) -> Self {
        Self::Contract(value)
    }
}

impl From<SchedulerError> for TransactionError {
    fn from(value: SchedulerError) -> Self {
        Self::Scheduler(value)
    }
}

#[derive(Debug)]
struct BankByte {
    initial: u8,
    banks: [u8; 2],
}

/// 单一任务的两个组合 bank 和不可变声明初值。
///
/// 只接受字节值，不接受可能隐藏引用、析构或分配的泛型状态。state/output 共用一份
/// 固定工作集，每字节保存初值及两个 bank 的值。序列化/语言布局由调用方的已验证声明决定，
/// 这里不是共享内存 ABI。构建期分配并触及所有槽；周期期每次复制最多 state+output 字节。
///
/// 所有修改需要独占借用。descriptor 的 Release store 同时选中 bank 和其版本，读取
/// 用 Acquire；普通 bank 字节的安全性来自 Rust 借用，不声称支持并发无锁 reader。
/// R0-05 另行实现可并发的 publication slot。这里不提供物理输出或 Fallback mailbox。
#[derive(Debug)]
pub struct TaskTransaction {
    spec: TaskSpec,
    engine_epoch: BootEpochId,
    bytes: FixedWorkSet<BankByte>,
    state_bytes: usize,
    versions: [CommitVersion; 2],
    committed: AtomicUsize,
    output_valid: bool,
    active: bool,
    fault: Option<LatchedTaskFault>,
    fault_generation: FaultGeneration,
    has_faulted: bool,
}

impl TaskTransaction {
    /// 在启动前复制已验证的不可变声明初值，建立 epoch 1 / commit 0。
    ///
    /// 单个区域允许为空，总容量必须非零。`limits` 限制总字节槽数和实际分配字节数
    /// （含两个 bank、初值和槽标记）；结构本身的固定大小另计入任务计划预算。
    /// 初值只用于诊断，首次成功周期前不可作为有效 control output 发布。
    ///
    /// # Errors
    /// 拒绝零总容量、超预算、数量溢出或分配失败；不安装任何部分初始化对象。
    pub fn new(
        spec: TaskSpec,
        engine_epoch: BootEpochId,
        initial_state: &[u8],
        initial_output: &[u8],
        limits: WorkSetLimits,
    ) -> Result<Self, TransactionError> {
        let length = initial_state
            .len()
            .checked_add(initial_output.len())
            .ok_or(TransactionError::ImageSizeOverflow)?;
        let mut builder = FixedWorkSetBuilder::new(WorkSetCapacity::new(length)?, limits)?;
        for value in initial_state.iter().chain(initial_output) {
            builder.initialize_next(BankByte {
                initial: *value,
                banks: [*value; 2],
            })?;
        }
        let version = CommitVersion {
            task_epoch: TaskEpoch::new(1)?,
            sequence: CommitSequence::ZERO,
        };
        Ok(Self {
            spec,
            engine_epoch,
            bytes: builder.seal()?,
            state_bytes: initial_state.len(),
            versions: [version; 2],
            committed: AtomicUsize::new(0),
            output_valid: false,
            active: false,
            fault: None,
            fault_generation: FaultGeneration::new(1)?,
            has_faulted: false,
        })
    }

    /// 返回上一完整版本供诊断；Fault 后读取不等于有权发布旧 control output。
    ///
    /// 读取视图仍在使用时不能可变访问事务：
    /// ```compile_fail,E0502
    /// use aurora_control_engine::TaskTransaction;
    /// use aurora_control_contracts::FaultReason;
    /// fn overlapping(task: &mut TaskTransaction) {
    ///     let old = task.diagnostic();
    ///     task.lock_fault(FaultReason::TaskExecutionFault);
    ///     let _version = old.version();
    /// }
    /// ```
    #[must_use]
    pub fn diagnostic(&self) -> BankView<'_> {
        BankView {
            values: BankValues {
                task: self,
                bank: self.committed.load(Ordering::Acquire),
            },
        }
    }

    /// 只返回最近成功周期的有效输出视图；初值、Fault 或 discard 后返回 None。
    /// 借用存活期间不能开始下次事务；不得把副本当作永久输出发布许可。
    #[must_use]
    pub fn publishable(&self) -> Option<BankView<'_>> {
        if self.output_valid && self.fault.is_none() && !self.active {
            Some(self.diagnostic())
        } else {
            None
        }
    }

    /// 读取持久故障证据，不确认或清除它。
    #[must_use]
    pub const fn fault(&self) -> Option<LatchedTaskFault> {
        self.fault
    }

    /// 在 release 边界锁定显式 Fault；重复调用幂等，保留首个原因。
    /// 后续 R0-06 用此入口处理 miss 阈值并构造不可丢失的 Fallback 请求。
    pub fn lock_fault(&mut self, reason: FaultReason) -> LatchedTaskFault {
        self.output_valid = false;
        if let Some(fault) = self.fault {
            return fault;
        }
        let mut reason = reason;
        if self.has_faulted {
            match self.fault_generation.checked_next() {
                Ok(next) => self.fault_generation = next,
                Err(_) => reason = FaultReason::CounterOverflow,
            }
        }
        self.has_faulted = true;
        let fault = LatchedTaskFault {
            reset_request: ResetRequest {
                engine_epoch: self.engine_epoch,
                task_handle: self.spec.handle(),
                task_epoch: self.diagnostic().version().task_epoch,
                fault_generation: self.fault_generation,
            },
            reason,
        };
        self.fault = Some(fault);
        fault
    }

    /// begin 完整复制 committed 到 staging，然后紧邻任务执行重新读钟。
    ///
    /// 调用前必须完成 R0-06 skip/miss 准入；锁定任务绝不进入 Execute。
    /// 返回句柄唯一拥有 staging 和调度窗口；未 finish/discard 的句柄析构会锁定任务。
    /// 复制有固定容量上界，不分配、不阻塞；输入/跨任务快照锁存由 R0-05 接入。
    ///
    /// # Errors
    /// 拒绝活动事务、Fault、错误任务/epoch 和时钟错误。
    pub fn begin<'task, 'plan, C: MonotonicClock + ?Sized>(
        &'task mut self,
        selected: ReleaseDecision<'plan>,
        clock: &C,
        control: ScheduleControl,
    ) -> Result<CycleStart<'task, 'plan>, TransactionError> {
        if let Some(fault) = self.fault {
            return Err(TransactionError::FaultLocked(fault));
        }
        if self.active {
            self.lock_fault(FaultReason::TaskExecutionFault);
            return Err(TransactionError::CycleAlreadyActive);
        }
        if !selected.matches_transaction(
            self.spec,
            self.engine_epoch,
            self.diagnostic().version().task_epoch,
        ) {
            return Err(TransactionError::PlanMismatch);
        }
        let staging = 1 - self.committed.load(Ordering::Acquire);
        self.copy_bank(staging, false)?;
        match selected.begin(clock, control) {
            Ok(ReleaseReadiness::Execute(window)) => {
                self.active = true;
                Ok(CycleStart::Execute(CycleTransaction {
                    task: self,
                    window,
                    staging,
                    failure: None,
                    resolved: false,
                }))
            }
            Ok(ReleaseReadiness::StartAfterDeadline) => {
                self.output_valid = false;
                Ok(CycleStart::StartAfterDeadline)
            }
            Ok(ReleaseReadiness::Stopped) => {
                self.output_valid = false;
                Ok(CycleStart::Stopped)
            }
            Err(error) => {
                self.lock_fault(FaultReason::ClockContractViolation);
                Err(error.into())
            }
        }
    }

    /// 匹配请求并通过 guard 后，从不可变声明初值重建 staging，再验证并整体恢复。
    ///
    /// `validate_initial` 只能读取初值，用于任务初始化有效性检查；必须有界且无 I/O/分配。
    /// 验证返回后读钟，准备原网格中严格晚于该时刻的 release；所有可失败步骤都在
    /// 新 descriptor 发布前完成。成功时 epoch+1、commit 0，输出仍无发布资格，
    /// 直到恢复任务成功执行。此 API 不清除 engine 时钟故障或全局 Stop。
    ///
    /// # Errors
    /// 旧身份/guard 拒绝不改变 committed；初始化或调度失败保持锁定并保留旧 bank。
    /// epoch/Fault generation 耗尽拒绝恢复，不能回绕。
    /// 初始化回调 unwind 时同样锁存新初始化故障，但异常继续向外传播，不伪造成功。
    pub fn reset<C: MonotonicClock + ?Sized, G: ResetGuard + ?Sized>(
        &mut self,
        request: ResetRequest,
        guard: &mut G,
        plan: &mut StaticTaskPlan,
        clock: &C,
        validate_initial: impl FnOnce(BankValues<'_>) -> Result<(), InitializationRejected>,
    ) -> Result<CommitVersion, TransactionError> {
        let fault = self.fault.ok_or(TransactionError::StaleResetRequest)?;
        if request != fault.reset_request {
            return Err(TransactionError::StaleResetRequest);
        }
        guard
            .check(request)
            .map_err(TransactionError::ResetDenied)?;
        let next_epoch = request
            .task_epoch
            .checked_next()
            .map_err(|_| TransactionError::CounterOverflow)?;
        if self.fault_generation.get() == u64::MAX {
            return Err(TransactionError::CounterOverflow);
        }
        let staging = 1 - self.committed.load(Ordering::Acquire);
        self.copy_bank(staging, true)?;
        let initialized = {
            let mut guard = InitializationGuard {
                task: self,
                succeeded: false,
            };
            let result = validate_initial(BankValues {
                task: guard.task,
                bank: staging,
            });
            guard.succeeded = result.is_ok();
            result
        };
        if initialized.is_err() {
            return Err(TransactionError::ReinitializationFailed);
        }
        let prepared =
            plan.prepare_task_reset(self.spec, self.engine_epoch, request.task_epoch, clock)?;
        let version = CommitVersion {
            task_epoch: next_epoch,
            sequence: CommitSequence::ZERO,
        };
        self.versions[staging] = version;
        prepared.commit(next_epoch);
        self.active = false;
        self.fault = None;
        self.output_valid = false;
        self.committed.store(staging, Ordering::Release);
        Ok(version)
    }

    fn copy_bank(&mut self, staging: usize, initial: bool) -> Result<(), TransactionError> {
        let committed = self.committed.load(Ordering::Acquire);
        for index in self.bytes.indices() {
            let byte = self.bytes.get_mut(index)?;
            byte.banks[staging] = if initial {
                byte.initial
            } else {
                byte.banks[committed]
            };
        }
        Ok(())
    }

    fn image_index(
        &self,
        output: bool,
        index: WorkSetIndex,
    ) -> Result<WorkSetIndex, TransactionError> {
        let (offset, length) = if output {
            (
                self.state_bytes,
                self.bytes.maximum_iteration_count() - self.state_bytes,
            )
        } else {
            (0, self.state_bytes)
        };
        if index.get() >= length {
            return Err(TransactionError::ImageOutOfRange);
        }
        Ok(WorkSetIndex::new(offset + index.get()))
    }
}

// guard 只覆盖已通过授权的初始化检查，不改变拒绝请求的幂等语义。
// Err 和 unwind 均使旧 reset 身份失效；析构只做常数次本地状态更新，不分配/阻塞。
struct InitializationGuard<'task> {
    task: &'task mut TaskTransaction,
    succeeded: bool,
}

impl Drop for InitializationGuard<'_> {
    fn drop(&mut self) {
        if !self.succeeded {
            self.task.fault = None;
            self.task.lock_fault(FaultReason::ReinitializationFailed);
        }
    }
}

/// 一次锁存的完整组合 bank 的只读借用；不得跨后续可变操作保留。
#[derive(Debug, Clone, Copy)]
pub struct BankView<'task> {
    values: BankValues<'task>,
}

impl<'task> BankView<'task> {
    /// 返回 state 和 output 共用的版本。
    #[must_use]
    pub fn version(self) -> CommitVersion {
        self.values.task.versions[self.values.bank]
    }

    /// 返回同一完整版本的只读字节区域；没有修改权限。
    #[must_use]
    pub const fn values(self) -> BankValues<'task> {
        self.values
    }
}

/// 无独立提交资格/版本的只读字节视图，用于完整 bank 读取和声明初值验证。
/// 只有 [`BankView`] 才标识已提交版本，不得把 staging 视作已发布输出。
#[derive(Debug, Clone, Copy)]
pub struct BankValues<'task> {
    task: &'task TaskTransaction,
    bank: usize,
}

impl BankValues<'_> {
    /// 返回 task 私有 state 区域的固定字节数。
    #[must_use]
    pub const fn state_len(self) -> usize {
        self.task.state_bytes
    }

    /// 返回 task 私有 output 区域的固定字节数。
    #[must_use]
    pub const fn output_len(self) -> usize {
        self.task.bytes.maximum_iteration_count() - self.task.state_bytes
    }

    /// 读取 state 字节；一次索引检查，不分配、不阻塞。
    /// # Errors
    /// 索引不属于该区域时返回 `ImageOutOfRange`。
    pub fn state(self, index: WorkSetIndex) -> Result<u8, TransactionError> {
        self.read(false, index)
    }

    /// 读取 output 字节；只能访问当前任务拥有的本地 output 区域。
    /// # Errors
    /// 索引不属于该区域时返回 `ImageOutOfRange`。
    pub fn output(self, index: WorkSetIndex) -> Result<u8, TransactionError> {
        self.read(true, index)
    }

    fn read(self, output: bool, index: WorkSetIndex) -> Result<u8, TransactionError> {
        let index = self.task.image_index(output, index)?;
        Ok(self.task.bytes.get(index)?.banks[self.bank])
    }
}

/// begin 后只有 Execute 含有可修改 staging 的唯一句柄。
#[derive(Debug)]
pub enum CycleStart<'task, 'plan> {
    /// 调用任务体，最后必须 finish 或 discard。
    Execute(CycleTransaction<'task, 'plan>),
    /// 记录 miss，不执行任务体。
    StartAfterDeadline,
    /// 边界停止，不生成输出。
    Stopped,
}

/// 活动周期的唯一 staging 访问权；没有裸可变 slice 或 committed 写入口。
///
/// 方法不分配/阻塞；调用方任务步骤及检查点必须静态有界。忽略方法返回的错误也
/// 不能恢复写入/提交；Fault 在任务内锁存。忘记句柄会留下 active 标记，后续 begin
/// 拒绝并锁定，不会复用半更新 bank。
///
/// finish 消耗句柄，不能再次写入或提交：
/// ```compile_fail,E0382
/// use aurora_control_engine::{CycleTransaction, MonotonicClock, WorkSetIndex};
/// fn reuse(mut cycle: CycleTransaction<'_, '_>, clock: &impl MonotonicClock) {
///     let _result = cycle.finish(clock);
///     let _write = cycle.write_output(WorkSetIndex::new(0), 1);
/// }
/// ```
#[derive(Debug)]
pub struct CycleTransaction<'task, 'plan> {
    task: &'task mut TaskTransaction,
    window: ExecutionWindow<'plan>,
    staging: usize,
    failure: Option<TransactionError>,
    resolved: bool,
}

impl CycleTransaction<'_, '_> {
    /// 读取当前 staging 的 state 字节；只在事务尚未失败时可见。
    /// # Errors
    /// 越界锁定容量 Fault；已经发生的 Fault/miss 原样返回。
    pub fn read_state(&mut self, index: WorkSetIndex) -> Result<u8, TransactionError> {
        self.read(false, index)
    }

    /// 读取当前 staging 的 output 字节，不返回可发布 bank 视图。
    /// # Errors
    /// 越界锁定容量 Fault；已经发生的 Fault/miss 原样返回。
    pub fn read_output(&mut self, index: WorkSetIndex) -> Result<u8, TransactionError> {
        self.read(true, index)
    }

    /// 写入 task 私有 state 字节。
    /// # Errors
    /// 越界锁定 CapacityExceeded；已失败事务不可继续修改。
    pub fn write_state(&mut self, index: WorkSetIndex, value: u8) -> Result<(), TransactionError> {
        self.write(false, index, value)
    }

    /// 写入 task 私有 output 字节；未写值延续上一完整 bank。
    /// # Errors
    /// 越界锁定 CapacityExceeded；无其他 task/物理地址写入口。
    pub fn write_output(&mut self, index: WorkSetIndex, value: u8) -> Result<(), TransactionError> {
        self.write(true, index, value)
    }

    /// 执行一个静态有界任务步骤；显式错误立即锁定，下一步骤不能再运行。
    /// 回调 unwind 会先锁定，再向宿主传播；宿主捕获异常也不能继续执行或提交。
    /// # Errors
    /// 保留任务 Fault 或步骤中已锁存的容量/时间失败，不吞掉错误。
    pub fn execute(
        &mut self,
        step: impl FnOnce(&mut Self) -> Result<(), FaultReason>,
    ) -> Result<(), TransactionError> {
        self.ensure_open()?;
        if let Err(reason) = self.guarded_call(FaultReason::TaskExecutionFault, step) {
            self.fail_fault(reason);
        }
        self.ensure_open()
    }

    /// 执行有界时钟检查；HardLimit/时钟错误锁定，deadline miss 使 staging 无效。
    /// miss 后仍读钟，防止任务返回时的 `HardLimit` 超限/时钟异常被早期 miss 掩盖。
    /// # Errors
    /// 返回锁存失败；预算超限但未越 HardLimit/deadline 时仍可提交。
    pub fn checkpoint<C: MonotonicClock + ?Sized>(
        &mut self,
        clock: &C,
    ) -> Result<ExecutionCheckpoint, TransactionError> {
        if !matches!(self.failure, Some(TransactionError::DeadlineMissed)) {
            self.ensure_open()?;
        }
        let checkpoint = match self.guarded_call(FaultReason::ClockContractViolation, |cycle| {
            cycle.window.checkpoint(clock)
        }) {
            Ok(value) => value,
            Err(error) => {
                self.fail_fault(FaultReason::ClockContractViolation);
                return Err(error.into());
            }
        };
        if checkpoint.hard_limit_exceeded() {
            self.fail_fault(FaultReason::HardLimitExceeded);
        } else if checkpoint.deadline_missed() {
            self.failure = Some(TransactionError::DeadlineMissed);
            self.task.output_valid = false;
        }
        self.ensure_open()?;
        Ok(checkpoint)
    }

    /// 最后一次读钟并检查成功后，一次 Release 发布 state/output 与共同的新版本。
    ///
    /// 返回预算观测供 R0-06 更新 Degraded；本层不维护 miss 窗口。
    /// # Errors
    /// Fault、miss、时钟或 commit counter 溢出均不移动 committed descriptor。
    pub fn finish<C: MonotonicClock + ?Sized>(
        mut self,
        clock: &C,
    ) -> Result<CycleCommit, TransactionError> {
        let checkpoint = self.checkpoint(clock)?;
        let previous = self.task.diagnostic().version();
        let Ok(sequence) = previous.sequence.checked_next() else {
            self.fail_fault(FaultReason::CounterOverflow);
            return Err(TransactionError::CounterOverflow);
        };
        let version = CommitVersion {
            task_epoch: previous.task_epoch,
            sequence,
        };
        self.task.versions[self.staging] = version;
        self.task.output_valid = true;
        self.task.active = false;
        self.task.committed.store(self.staging, Ordering::Release);
        self.resolved = true;
        Ok(CycleCommit {
            version,
            checkpoint,
        })
    }

    /// 显式 Fault 丢弃，保留上一完整版本用于诊断；消耗句柄，不允许再次执行。
    #[must_use]
    pub fn discard(mut self, reason: FaultReason) -> LatchedTaskFault {
        let fault = self.task.lock_fault(reason);
        self.task.active = false;
        self.resolved = true;
        fault
    }

    fn ensure_open(&self) -> Result<(), TransactionError> {
        match self.failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn guarded_call<T>(&mut self, reason: FaultReason, call: impl FnOnce(&mut Self) -> T) -> T {
        let mut guard = CycleCallGuard {
            cycle: self,
            reason,
            returned: false,
        };
        let result = call(guard.cycle);
        guard.returned = true;
        result
    }

    fn fail_fault(&mut self, reason: FaultReason) {
        let fault = self.task.lock_fault(reason);
        self.failure = Some(TransactionError::FaultLocked(fault));
    }

    fn write(
        &mut self,
        output: bool,
        index: WorkSetIndex,
        value: u8,
    ) -> Result<(), TransactionError> {
        self.ensure_open()?;
        let result = self.task.image_index(output, index).and_then(|index| {
            self.task
                .bytes
                .get_mut(index)
                .map_err(TransactionError::from)
        });
        match result {
            Ok(byte) => {
                byte.banks[self.staging] = value;
                Ok(())
            }
            Err(error) => {
                self.fail_fault(FaultReason::CapacityExceeded);
                Err(error)
            }
        }
    }

    fn read(&mut self, output: bool, index: WorkSetIndex) -> Result<u8, TransactionError> {
        self.ensure_open()?;
        let result = self.task.image_index(output, index).and_then(|index| {
            self.task
                .bytes
                .get(index)
                .map(|byte| byte.banks[self.staging])
                .map_err(TransactionError::from)
        });
        if result.is_err() {
            self.fail_fault(FaultReason::CapacityExceeded);
        }
        result
    }
}

// 不捕获/吞掉 panic；只在回调未正常返回时撤销活动事务的执行和提交资格。
// 持有整个事务的独占借用，避免宿主 catch_unwind 后绕过 Fault 锁存。
struct CycleCallGuard<'call, 'task, 'plan> {
    cycle: &'call mut CycleTransaction<'task, 'plan>,
    reason: FaultReason,
    returned: bool,
}

impl Drop for CycleCallGuard<'_, '_, '_> {
    fn drop(&mut self) {
        if !self.returned {
            self.cycle.fail_fault(self.reason);
        }
    }
}

impl Drop for CycleTransaction<'_, '_> {
    fn drop(&mut self) {
        if !self.resolved {
            self.task.output_valid = false;
            if self.failure.is_none() {
                self.task.lock_fault(FaultReason::TaskExecutionFault);
            }
            self.task.active = false;
        }
    }
}

/// 成功提交及其最终时间边界观测；不代替跨任务快照发布或 Fallback ack。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleCommit {
    /// state/output 共用的版本。
    pub version: CommitVersion,
    /// 最终时间及预算检查结果。
    pub checkpoint: ExecutionCheckpoint,
}

#[cfg(test)]
#[path = "transaction_tests.rs"]
mod transaction_tests;
