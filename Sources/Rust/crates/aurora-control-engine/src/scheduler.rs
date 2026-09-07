//! 静态任务计划和无漂移的绝对单调调度决策。

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use aurora_control_contracts::{ReleaseSequence, TaskSpec};
use aurora_types::{BootEpochId, DurationNanos, LocalHandle, MonotonicTimestamp};

use crate::{
    FixedWorkSet, FixedWorkSetBuilder, MonotonicWait, StopSignal, WaitError, WaitOutcome, WaitStep,
    WorkSetCapacity, WorkSetError, WorkSetIndex, WorkSetLimits,
};

/// 可注入的单调时钟读取边界。
///
/// 实现必须返回同一进程 `BootEpochId` 下不回退的纳秒值，并且一次读取不得分配、
/// 阻塞等待或访问 UTC。未来 `aurora-platform-linux` 适配器负责用 Linux
/// `CLOCK_MONOTONIC` 实现读取；绝对等待由 [`ScheduleAction::WaitUntil`] 的调用方
/// 在 [`StaticTaskPlan::wait_once`] 的单次平台等待边界完成，不能改用 UTC deadline。
pub trait MonotonicClock {
    /// 返回当前进程启动 epoch 内的单调时间。
    fn now(&self) -> MonotonicTimestamp;
}

/// 调度边界上的继续或停止请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduleControl {
    /// 继续选择下一项 release。
    Continue,
    /// 在当前 task/release 边界停止，不消费 release。
    StopRequested,
}

/// 一次单调时钟观察产生的有界调度动作。
///
/// release 数据保持内联，避免周期路径为缩小枚举而执行 `Box` 堆分配。
#[derive(Debug, PartialEq, Eq)]
pub enum ScheduleAction<'plan> {
    /// 停止请求已在 release 边界观察到。
    Stopped {
        /// 观察停止请求时的单调时间。
        observed_at: MonotonicTimestamp,
    },
    /// 当前没有到期任务；Linux 平台层应等待到这个绝对单调时间。
    WaitUntil {
        /// 所有任务中最近的下一次绝对 release。
        release: MonotonicTimestamp,
    },
    /// 恰有一个任务 release 被消费；再次观察才会选择其他到期任务。
    Release(ReleaseDecision<'plan>),
}

/// 一个有界批次中被跳过的连续 release 范围。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkippedReleases {
    first: ReleaseSequence,
    last: ReleaseSequence,
    count: u64,
}

impl SkippedReleases {
    /// 返回首个被跳过的 release sequence。
    #[must_use]
    pub const fn first(self) -> ReleaseSequence {
        self.first
    }

    /// 返回最后一个被跳过的 release sequence。
    #[must_use]
    pub const fn last(self) -> ReleaseSequence {
        self.last
    }

    /// 返回跳过数量；调用方必须用有界批量算法更新 miss 窗口，不得逐项追赶。
    #[must_use]
    pub const fn count(self) -> u64 {
        self.count
    }
}

/// 当前 release 是否可以开始执行。
#[derive(Debug, PartialEq, Eq)]
pub enum ReleaseReadiness<'plan> {
    /// 开始检查点不晚于绝对 deadline；调用方完成 miss/Fault 准入后可进入任务体。
    Execute(ExecutionWindow<'plan>),
    /// 开始检查点晚于绝对 deadline；任务体不得执行。
    StartAfterDeadline,
    /// 在任务开始边界观察到停止；不进入任务体，计划保持停止。
    Stopped,
}

/// 一个已消费 release 的确定性调度结果。
///
/// 此时尚未开始任务体。调用方先处理跳过历史，再通过 [`Self::begin`] 检查实际开始；
/// 丢弃选择结果不会回退已经消费的 sequence，也不会执行任务体。
#[derive(Debug, PartialEq, Eq)]
pub struct ReleaseDecision<'plan> {
    task: TaskSpec,
    release_sequence: ReleaseSequence,
    scheduled_release: MonotonicTimestamp,
    absolute_deadline: MonotonicTimestamp,
    observed_at: MonotonicTimestamp,
    skipped_releases: Option<SkippedReleases>,
    progress: &'plan mut ScheduleProgress,
}

impl<'plan> ReleaseDecision<'plan> {
    /// 返回静态任务声明；其中包含优先级、周期、预算、HardLimit 和 miss 策略。
    #[must_use]
    pub const fn task(&self) -> TaskSpec {
        self.task
    }

    /// 返回当前 task epoch 内被消费的 release sequence。
    #[must_use]
    pub const fn release_sequence(&self) -> ReleaseSequence {
        self.release_sequence
    }

    /// 返回由 `engine_start + phase + k * period` 得到的绝对 release。
    #[must_use]
    pub const fn scheduled_release(&self) -> MonotonicTimestamp {
        self.scheduled_release
    }

    /// 返回由 `scheduled_release + relative_deadline` 得到的绝对 deadline。
    #[must_use]
    pub const fn absolute_deadline(&self) -> MonotonicTimestamp {
        self.absolute_deadline
    }

    /// 返回选择 release 时的单调时间；这不是任务实际开始时间。
    #[must_use]
    pub const fn observed_at(&self) -> MonotonicTimestamp {
        self.observed_at
    }

    /// 返回本次用常数次算术折叠的连续跳过范围。
    #[must_use]
    pub const fn skipped_releases(&self) -> Option<SkippedReleases> {
        self.skipped_releases
    }

    /// 在紧邻任务调用的开始边界重新读钟，消费选择结果并建立唯一执行窗口。
    ///
    /// 调用方必须先批量处理 `skipped_releases` 和任务准入；若 miss 阈值已触发
    /// Fault，应丢弃本选择结果，不得调用本方法。此处只判断时间和停止，不代替
    /// R0-06 的 miss/Fault 准入。返回 `Execute` 后应立即调用任务体。
    /// 选择结果和执行窗口独占借用计划，期间不能调度另一个任务或复制开始许可。
    ///
    /// # Errors
    ///
    /// 返回并锁存跨 epoch 或回退的时钟错误；不分配、不阻塞、不重试。
    pub fn begin<C: MonotonicClock + ?Sized>(
        self,
        clock: &C,
        control: ScheduleControl,
    ) -> Result<ReleaseReadiness<'plan>, SchedulerError> {
        let now = self.progress.read(clock)?;
        if matches!(control, ScheduleControl::StopRequested) {
            self.progress.stopped_at = Some(now);
            return Ok(ReleaseReadiness::Stopped);
        }
        if now.elapsed_nanos() > self.absolute_deadline.elapsed_nanos() {
            return Ok(ReleaseReadiness::StartAfterDeadline);
        }
        let timing = self.task.timing();
        Ok(ReleaseReadiness::Execute(ExecutionWindow {
            started_at: now,
            absolute_deadline: self.absolute_deadline,
            execution_budget_nanos: timing.execution_budget().get(),
            hard_limit_nanos: timing.hard_limit().get(),
            progress: self.progress,
        }))
    }
}

/// 已开始 release 的 deadline、预算和 `HardLimit` 检查窗口。
///
/// `started_at` 在 [`ReleaseDecision::begin`] 读取，不包含任务表扫描耗时。
/// R0 不使用异步信号抢占任务；
/// AOT 代码和 Runtime 必须在自身静态有界的调用/循环边界调用 [`Self::checkpoint`]。
/// 调用方在任务返回点执行最后一次检查，将结果交给后续 commit/discard 逻辑。
#[derive(Debug, PartialEq, Eq)]
pub struct ExecutionWindow<'plan> {
    started_at: MonotonicTimestamp,
    absolute_deadline: MonotonicTimestamp,
    execution_budget_nanos: u64,
    hard_limit_nanos: u64,
    progress: &'plan mut ScheduleProgress,
}

impl ExecutionWindow<'_> {
    /// 返回任务开始检查点的单调时间。
    #[must_use]
    pub const fn started_at(&self) -> MonotonicTimestamp {
        self.started_at
    }

    /// 返回本 release 的绝对 deadline。
    #[must_use]
    pub const fn absolute_deadline(&self) -> MonotonicTimestamp {
        self.absolute_deadline
    }

    /// 使用同一可注入时钟执行一次有界运行时间检查。
    ///
    /// 结果同时保留预算、HardLimit 和 deadline 三个独立边界，避免一个超限掩盖
    /// 另一个超限。等于边界仍在范围内，只有严格大于才报告超限。
    ///
    /// # Errors
    ///
    /// 时钟切换 boot epoch 或早于最近一次调度/开始/执行检查点时，返回并锁存
    /// [`SchedulerError::ClockEpochMismatch`] 或 [`SchedulerError::ClockMovedBackwards`]。
    pub fn checkpoint<C: MonotonicClock + ?Sized>(
        &mut self,
        clock: &C,
    ) -> Result<ExecutionCheckpoint, SchedulerError> {
        let observed_at = self.progress.read(clock)?;
        let elapsed_nanos = observed_at.elapsed_nanos() - self.started_at.elapsed_nanos();
        Ok(ExecutionCheckpoint {
            observed_at,
            elapsed: DurationNanos::new(elapsed_nanos),
            execution_budget_exceeded: elapsed_nanos > self.execution_budget_nanos,
            hard_limit_exceeded: elapsed_nanos > self.hard_limit_nanos,
            deadline_missed: observed_at.elapsed_nanos() > self.absolute_deadline.elapsed_nanos(),
        })
    }
}

/// 一次任务执行检查点的完整边界结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionCheckpoint {
    observed_at: MonotonicTimestamp,
    elapsed: DurationNanos,
    execution_budget_exceeded: bool,
    hard_limit_exceeded: bool,
    deadline_missed: bool,
}

impl ExecutionCheckpoint {
    /// 返回检查点的单调时间。
    #[must_use]
    pub const fn observed_at(self) -> MonotonicTimestamp {
        self.observed_at
    }

    /// 返回从开始检查点起的非负执行耗时。
    #[must_use]
    pub const fn elapsed(self) -> DurationNanos {
        self.elapsed
    }

    /// 返回执行耗时是否严格超过准入预算。
    #[must_use]
    pub const fn execution_budget_exceeded(self) -> bool {
        self.execution_budget_exceeded
    }

    /// 返回执行耗时是否严格超过 `HardLimit`。
    #[must_use]
    pub const fn hard_limit_exceeded(self) -> bool {
        self.hard_limit_exceeded
    }

    /// 返回检查点是否严格晚于绝对 deadline。
    #[must_use]
    pub const fn deadline_missed(self) -> bool {
        self.deadline_missed
    }
}

/// 静态任务计划的启动期构建器。
///
/// 构造时一次性分配精确容量。任务只能在启动路径加入；[`Self::seal`] 后不再暴露
/// 插入、删除、扩容或任务发现 API。
#[derive(Debug)]
pub struct StaticTaskPlanBuilder {
    engine_start: MonotonicTimestamp,
    tasks: FixedWorkSetBuilder<ScheduledTask>,
}

impl StaticTaskPlanBuilder {
    /// 以工程/Target Profile 验证过的容量和内存预算创建构建器。
    ///
    /// # Errors
    ///
    /// 固定工作集拒绝容量或无法完成预分配时返回 [`SchedulerError::WorkSet`]。
    pub fn new(
        engine_start: MonotonicTimestamp,
        capacity: WorkSetCapacity,
        limits: WorkSetLimits,
    ) -> Result<Self, SchedulerError> {
        Ok(Self {
            engine_start,
            tasks: FixedWorkSetBuilder::new(capacity, limits)?,
        })
    }

    /// 顺序加入一个已验证的静态任务声明。
    ///
    /// # Errors
    ///
    /// 加入数量超过声明容量时返回 [`SchedulerError::WorkSet`]。
    pub fn add_task(&mut self, task: TaskSpec) -> Result<WorkSetIndex, SchedulerError> {
        self.tasks
            .initialize_next(ScheduledTask {
                spec: task,
                next_release_sequence: ReleaseSequence::ZERO,
                next_ordinal: ScheduleOrdinal(0),
                schedule_error: None,
            })
            .map_err(SchedulerError::from)
    }

    /// 验证精确任务数和唯一 handle，冻结计划并进入周期使用阶段。
    ///
    /// 重复 handle 的检测至多比较 `capacity * (capacity - 1) / 2` 对；该工作只在
    /// 启动路径执行。成功后每次 [`StaticTaskPlan::observe`] 最多扫描 `capacity` 个槽。
    ///
    /// # Errors
    ///
    /// 未填满容量时返回 [`SchedulerError::WorkSet`]；同一计划内 handle 重复时返回
    /// [`SchedulerError::DuplicateTaskHandle`]。
    pub fn seal(self) -> Result<StaticTaskPlan, SchedulerError> {
        let tasks = self.tasks.seal()?;
        validate_unique_handles(&tasks)?;
        Ok(StaticTaskPlan {
            engine_start: self.engine_start,
            progress: ScheduleProgress {
                last_observed_at: self.engine_start,
                clock_error: None,
                stopped_at: None,
            },
            tasks,
        })
    }
}

/// 启动前冻结、周期期不扩容的确定性任务计划。
///
/// 每次观察通过一次固定容量扫描选择最小 `scheduled_release`；同刻按 priority
/// 降序、再按 `TaskHandle` 升序。选择和跳过折叠只用常数空间，不补跑旧 release。
#[derive(Debug)]
pub struct StaticTaskPlan {
    engine_start: MonotonicTimestamp,
    progress: ScheduleProgress,
    tasks: FixedWorkSet<ScheduledTask>,
}

impl StaticTaskPlan {
    /// 返回固定任务数量。
    #[must_use]
    pub const fn task_count(&self) -> usize {
        self.tasks.maximum_iteration_count()
    }

    /// 返回 engine epoch 的绝对调度原点。
    #[must_use]
    pub const fn engine_start(&self) -> MonotonicTimestamp {
        self.engine_start
    }

    /// 读取固定槽中保持的调度故障；故障任务不再参与选择，不随时间自动恢复。
    ///
    /// R0-06 应将报告的任务故障连接到 Fault/Fallback；读取不会确认或清除故障。
    ///
    /// # Errors
    ///
    /// 索引越界或工作集不变量损坏时返回 [`SchedulerError::WorkSet`]。
    pub fn task_schedule_error(
        &self,
        index: WorkSetIndex,
    ) -> Result<Option<SchedulerError>, SchedulerError> {
        Ok(self.tasks.get(index)?.schedule_error)
    }

    /// 在无活动任务的 release 边界执行至多一次绝对等待，并在前后检查停止。
    ///
    /// `release` 来自 `WaitUntil`；`maximum_stop_check_interval` 是工程/Target 声明的
    /// 非零纳秒间隔，启动前固定、运行期不得更改。单次等待目标为
    /// min(release, now + interval)，不会移动原始
    /// release 网格。Interrupted/CheckBoundary 必须交还外层有界控制步骤处理，
    /// 本方法不循环、不分配。系统调度延迟不在声明间隔保证内。
    ///
    /// # Errors
    ///
    /// 返回共享时钟历史的契约错误，或显式 Wait 配置/系统调用错误；不隐藏失败。
    pub fn wait_once<W: MonotonicWait + ?Sized, S: StopSignal + ?Sized>(
        &mut self,
        waiter: &mut W,
        release: MonotonicTimestamp,
        maximum_stop_check_interval: DurationNanos,
        stop: &S,
    ) -> Result<WaitStep, SchedulerError> {
        if self.progress.stopped_at.is_some() {
            return Ok(WaitStep::Stopped);
        }
        let now = self.progress.read(waiter)?;
        if stop.is_stop_requested() {
            self.progress.stopped_at = Some(now);
            return Ok(WaitStep::Stopped);
        }
        if release.boot_epoch() != now.boot_epoch() {
            return Err(SchedulerError::ClockEpochMismatch {
                expected: now.boot_epoch(),
                observed: release.boot_epoch(),
            });
        }
        if maximum_stop_check_interval.get() == 0 {
            return Err(SchedulerError::Wait(WaitError::InvalidCheckInterval));
        }
        if now.elapsed_nanos() >= release.elapsed_nanos() {
            return Ok(WaitStep::ReleaseReached);
        }
        // 先按 release-now 限界，避免 now + interval 在 u64 极值附近溢出。
        let interval = maximum_stop_check_interval
            .get()
            .min(release.elapsed_nanos() - now.elapsed_nanos());
        let target = MonotonicTimestamp::new(now.boot_epoch(), now.elapsed_nanos() + interval);
        let outcome = waiter.wait_until_once(target).map_err(SchedulerError::Wait);
        let after = self.progress.read(waiter)?;
        if stop.is_stop_requested() {
            self.progress.stopped_at = Some(after);
        }
        // 即使同时收到停止，也保留适配器失败供调用方诊断。
        let outcome = outcome?;
        if self.progress.stopped_at.is_some() {
            return Ok(WaitStep::Stopped);
        }
        match outcome {
            WaitOutcome::Interrupted => Ok(WaitStep::Interrupted),
            WaitOutcome::DeadlineReached if after.elapsed_nanos() < target.elapsed_nanos() => {
                Err(SchedulerError::Wait(WaitError::EarlyWake))
            }
            WaitOutcome::DeadlineReached if after.elapsed_nanos() >= release.elapsed_nanos() => {
                Ok(WaitStep::ReleaseReached)
            }
            WaitOutcome::DeadlineReached => Ok(WaitStep::CheckBoundary),
        }
    }

    /// 在一个 task/release 边界读取时钟并选择至多一个动作。
    ///
    /// 该方法最多扫描 `task_count` 个固定槽，不分配、不执行 I/O、不等待、不重试，
    /// 且每个任务在同一个 `now` 值下最多产生一个当前 release。停止请求先于任务
    /// 选择处理，因此不会消费新 release。停止在计划内锁存，后续 Continue 不能恢复。
    /// 每个任务的时间/计数故障首次以 Err 报告后保持在固定槽，后续观察跳过该任务；
    /// 同一 now 下至多 `task_count` 次故障报告后健康任务可继续得到服务。
    ///
    /// 选择/执行窗口存活期间不能再次借用同一计划，防止并行或重复启动：
    ///
    /// ```compile_fail,E0499
    /// use aurora_control_engine::{MonotonicClock, ScheduleControl, SchedulerError, StaticTaskPlan};
    /// fn overlap(plan: &mut StaticTaskPlan, clock: &impl MonotonicClock)
    ///     -> Result<(), SchedulerError> {
    ///     let selected = plan.observe(clock, ScheduleControl::Continue)?;
    ///     let next = plan.observe(clock, ScheduleControl::Continue)?;
    ///     drop(selected);
    ///     drop(next);
    ///     Ok(())
    /// }
    /// ```
    ///
    /// # Errors
    ///
    /// 时钟 epoch/顺序错误、绝对时间计算溢出、release counter 即将回绕或固定
    /// 工作集内部不变量损坏时，返回可穷举的 [`SchedulerError`]。
    pub fn observe<C: MonotonicClock + ?Sized>(
        &mut self,
        clock: &C,
        control: ScheduleControl,
    ) -> Result<ScheduleAction<'_>, SchedulerError> {
        if let Some(observed_at) = self.progress.stopped_at {
            return Ok(ScheduleAction::Stopped { observed_at });
        }
        let now = self.progress.read(clock)?;

        if matches!(control, ScheduleControl::StopRequested) {
            self.progress.stopped_at = Some(now);
            return Ok(ScheduleAction::Stopped { observed_at: now });
        }

        let mut selected: Option<DueCandidate> = None;
        let mut next_release: Option<MonotonicTimestamp> = None;
        for index in self.tasks.indices() {
            let task = self.tasks.get_mut(index)?;
            if task.schedule_error.is_some() {
                continue;
            }
            let candidate = match candidate_for(task, self.engine_start, now, index) {
                Ok(candidate) => candidate,
                Err(error) => {
                    task.schedule_error = Some(error);
                    return Err(error);
                }
            };
            match candidate {
                Candidate::Due(candidate) => {
                    if selected.is_none_or(|current| candidate.precedes(current)) {
                        selected = Some(candidate);
                    }
                }
                Candidate::Future(release) => {
                    if next_release
                        .is_none_or(|current| release.elapsed_nanos() < current.elapsed_nanos())
                    {
                        next_release = Some(release);
                    }
                }
            }
        }

        if let Some(candidate) = selected {
            return self.consume(candidate, now).map(ScheduleAction::Release);
        }

        match next_release {
            Some(release) => Ok(ScheduleAction::WaitUntil { release }),
            None => Err(SchedulerError::NoSchedulableTask),
        }
    }

    fn consume(
        &mut self,
        candidate: DueCandidate,
        now: MonotonicTimestamp,
    ) -> Result<ReleaseDecision<'_>, SchedulerError> {
        let task = self.tasks.get_mut(candidate.index)?;
        let first_unprocessed = task.next_release_sequence;
        let next = candidate.release_sequence.checked_next().map_err(|_| {
            SchedulerError::ReleaseSequenceOverflow {
                handle: candidate.task.handle(),
            }
        })?;
        task.next_release_sequence = next;
        // candidate_for 已拒绝最大 ordinal；此处不会回绕。
        task.next_ordinal = ScheduleOrdinal(candidate.ordinal.0 + 1);

        let skipped_releases = if candidate.release_sequence > first_unprocessed {
            let count = candidate.release_sequence.get() - first_unprocessed.get();
            Some(SkippedReleases {
                first: first_unprocessed,
                last: ReleaseSequence::new(candidate.release_sequence.get() - 1),
                count,
            })
        } else {
            None
        };

        Ok(ReleaseDecision {
            task: candidate.task,
            release_sequence: candidate.release_sequence,
            scheduled_release: candidate.scheduled_release,
            absolute_deadline: candidate.absolute_deadline,
            observed_at: now,
            skipped_releases,
            progress: &mut self.progress,
        })
    }
}

/// 静态计划构建或周期调度失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerError {
    /// 单次绝对等待配置或平台适配错误。
    Wait(WaitError),
    /// 固定工作集容量、分配、初始化或访问失败。
    WorkSet(WorkSetError),
    /// 同一静态计划出现重复任务 handle。
    DuplicateTaskHandle {
        /// 重复的 payload-local handle。
        handle: LocalHandle,
    },
    /// 单调时钟切换到了另一个 boot epoch。
    ClockEpochMismatch {
        /// 计划或执行窗口要求的 epoch。
        expected: BootEpochId,
        /// 时钟实际返回的 epoch。
        observed: BootEpochId,
    },
    /// 同一 epoch 内的单调时钟发生回退。
    ClockMovedBackwards {
        /// 前一次有效观察的纳秒值。
        previous_elapsed_nanos: u64,
        /// 本次回退后的纳秒值。
        observed_elapsed_nanos: u64,
    },
    /// `engine_start + phase + k * period` 或绝对 deadline 无法用 `u64` 表示。
    ScheduleTimeOverflow {
        /// 发生溢出的任务。
        handle: LocalHandle,
        /// 无法计算的 release sequence。
        release_sequence: ReleaseSequence,
    },
    /// 当前 release 的下一 sequence 将回绕。
    ReleaseSequenceOverflow {
        /// sequence 已耗尽的任务。
        handle: LocalHandle,
    },
    /// 冻结计划没有产生到期项或可表示的未来 release。
    NoSchedulableTask,
}

impl From<WorkSetError> for SchedulerError {
    fn from(value: WorkSetError) -> Self {
        Self::WorkSet(value)
    }
}

impl Display for SchedulerError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wait(error) => Display::fmt(error, formatter),
            Self::WorkSet(error) => Display::fmt(error, formatter),
            Self::DuplicateTaskHandle { handle } => {
                write!(formatter, "duplicate task handle {}", handle.get())
            }
            Self::ClockEpochMismatch { .. } => {
                formatter.write_str("monotonic clock changed boot epoch")
            }
            Self::ClockMovedBackwards {
                previous_elapsed_nanos,
                observed_elapsed_nanos,
            } => write!(
                formatter,
                "monotonic clock moved backwards from {previous_elapsed_nanos} to {observed_elapsed_nanos} nanoseconds"
            ),
            Self::ScheduleTimeOverflow {
                handle,
                release_sequence,
            } => write!(
                formatter,
                "task {} release {} absolute time cannot be represented",
                handle.get(),
                release_sequence.get()
            ),
            Self::ReleaseSequenceOverflow { handle } => write!(
                formatter,
                "task {} release sequence cannot advance without wraparound",
                handle.get()
            ),
            Self::NoSchedulableTask => {
                formatter.write_str("static task plan has no schedulable task")
            }
        }
    }
}

impl Error for SchedulerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Wait(error) => Some(error),
            Self::WorkSet(error) => Some(error),
            Self::DuplicateTaskHandle { .. }
            | Self::ClockEpochMismatch { .. }
            | Self::ClockMovedBackwards { .. }
            | Self::ScheduleTimeOverflow { .. }
            | Self::ReleaseSequenceOverflow { .. }
            | Self::NoSchedulableTask => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ScheduledTask {
    spec: TaskSpec,
    next_release_sequence: ReleaseSequence,
    next_ordinal: ScheduleOrdinal,
    schedule_error: Option<SchedulerError>,
}

// engine 时间网格位置与 task epoch 内序列分开；本工作项没有 reset/reinitialize API。
#[derive(Debug, Clone, Copy)]
struct ScheduleOrdinal(u64);

#[derive(Debug, PartialEq, Eq)]
struct ScheduleProgress {
    last_observed_at: MonotonicTimestamp,
    clock_error: Option<SchedulerError>,
    stopped_at: Option<MonotonicTimestamp>,
}

impl ScheduleProgress {
    fn read<C: MonotonicClock + ?Sized>(
        &mut self,
        clock: &C,
    ) -> Result<MonotonicTimestamp, SchedulerError> {
        if let Some(error) = self.clock_error {
            return Err(error);
        }
        let now = clock.now();
        if let Err(error) = validate_clock_order(self.last_observed_at, now) {
            self.clock_error = Some(error);
            return Err(error);
        }
        self.last_observed_at = now;
        Ok(now)
    }
}

#[derive(Debug, Clone, Copy)]
enum Candidate {
    Due(DueCandidate),
    Future(MonotonicTimestamp),
}

#[derive(Debug, Clone, Copy)]
struct DueCandidate {
    index: WorkSetIndex,
    task: TaskSpec,
    release_sequence: ReleaseSequence,
    ordinal: ScheduleOrdinal,
    scheduled_release: MonotonicTimestamp,
    absolute_deadline: MonotonicTimestamp,
}

impl DueCandidate {
    fn precedes(self, other: Self) -> bool {
        let self_release = self.scheduled_release.elapsed_nanos();
        let other_release = other.scheduled_release.elapsed_nanos();
        self_release < other_release
            || (self_release == other_release
                && (self.task.priority() > other.task.priority()
                    || (self.task.priority() == other.task.priority()
                        && self.task.handle() < other.task.handle())))
    }
}

fn validate_unique_handles(tasks: &FixedWorkSet<ScheduledTask>) -> Result<(), SchedulerError> {
    for left in tasks.indices() {
        let left_handle = tasks.get(left)?.spec.handle();
        for right_value in (left.get() + 1)..tasks.maximum_iteration_count() {
            let right = WorkSetIndex::new(right_value);
            if tasks.get(right)?.spec.handle() == left_handle {
                return Err(SchedulerError::DuplicateTaskHandle {
                    handle: left_handle,
                });
            }
        }
    }
    Ok(())
}

fn candidate_for(
    task: &ScheduledTask,
    engine_start: MonotonicTimestamp,
    now: MonotonicTimestamp,
    index: WorkSetIndex,
) -> Result<Candidate, SchedulerError> {
    let spec = task.spec;
    let timing = spec.timing();
    let first_release_nanos = engine_start
        .elapsed_nanos()
        .checked_add(timing.phase().get())
        .ok_or(SchedulerError::ScheduleTimeOverflow {
            handle: spec.handle(),
            release_sequence: ReleaseSequence::ZERO,
        })?;

    if now.elapsed_nanos() < first_release_nanos {
        let (release, _) = release_timing(
            engine_start,
            spec,
            task.next_ordinal,
            task.next_release_sequence,
        )?;
        return Ok(Candidate::Future(release));
    }

    let latest_due_value = (now.elapsed_nanos() - first_release_nanos) / timing.period().get();
    if latest_due_value < task.next_ordinal.0 {
        let (release, _) = release_timing(
            engine_start,
            spec,
            task.next_ordinal,
            task.next_release_sequence,
        )?;
        return Ok(Candidate::Future(release));
    }
    if latest_due_value == u64::MAX {
        return Err(SchedulerError::ReleaseSequenceOverflow {
            handle: spec.handle(),
        });
    }

    let skipped_count = latest_due_value - task.next_ordinal.0;
    let sequence_value = task
        .next_release_sequence
        .get()
        .checked_add(skipped_count)
        .filter(|value| *value < u64::MAX)
        .ok_or(SchedulerError::ReleaseSequenceOverflow {
            handle: spec.handle(),
        })?;
    let release_sequence = ReleaseSequence::new(sequence_value);
    let ordinal = ScheduleOrdinal(latest_due_value);
    let (scheduled_release, absolute_deadline) =
        release_timing(engine_start, spec, ordinal, release_sequence)?;
    Ok(Candidate::Due(DueCandidate {
        index,
        task: spec,
        release_sequence,
        ordinal,
        scheduled_release,
        absolute_deadline,
    }))
}

fn release_timing(
    engine_start: MonotonicTimestamp,
    task: TaskSpec,
    ordinal: ScheduleOrdinal,
    release_sequence: ReleaseSequence,
) -> Result<(MonotonicTimestamp, MonotonicTimestamp), SchedulerError> {
    let timing = task.timing();
    let offset = ordinal
        .0
        .checked_mul(timing.period().get())
        .and_then(|periods| timing.phase().get().checked_add(periods))
        .and_then(|offset| engine_start.elapsed_nanos().checked_add(offset))
        .ok_or(SchedulerError::ScheduleTimeOverflow {
            handle: task.handle(),
            release_sequence,
        })?;
    let deadline = offset.checked_add(timing.relative_deadline().get()).ok_or(
        SchedulerError::ScheduleTimeOverflow {
            handle: task.handle(),
            release_sequence,
        },
    )?;
    let epoch = engine_start.boot_epoch();
    Ok((
        MonotonicTimestamp::new(epoch, offset),
        MonotonicTimestamp::new(epoch, deadline),
    ))
}

fn validate_clock_order(
    previous: MonotonicTimestamp,
    observed: MonotonicTimestamp,
) -> Result<(), SchedulerError> {
    if observed.boot_epoch() != previous.boot_epoch() {
        return Err(SchedulerError::ClockEpochMismatch {
            expected: previous.boot_epoch(),
            observed: observed.boot_epoch(),
        });
    }
    if observed.elapsed_nanos() < previous.elapsed_nanos() {
        return Err(SchedulerError::ClockMovedBackwards {
            previous_elapsed_nanos: previous.elapsed_nanos(),
            observed_elapsed_nanos: observed.elapsed_nanos(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use aurora_control_contracts::{
        ExecutionBudgetNanos, ExecutionContractError, ExecutionContractVersion, HardLimitNanos,
        MissPolicy, MissWindow, RelativeDeadlineNanos, TaskPeriodNanos, TaskPhaseNanos,
        TaskPriority, TaskSpec, TaskTiming,
    };
    use aurora_test_support::ManualClock;
    use aurora_types::{BootEpochId, DurationNanos, LocalHandle, MonotonicTimestamp, UtcTimestamp};

    use super::{
        ExecutionWindow, MonotonicClock, ReleaseReadiness, ScheduleAction, ScheduleControl,
        SchedulerError, StaticTaskPlan, StaticTaskPlanBuilder,
    };
    use crate::{WorkSetCapacity, WorkSetLimits};

    impl MonotonicClock for ManualClock {
        fn now(&self) -> MonotonicTimestamp {
            self.monotonic()
        }
    }

    #[test]
    fn absolute_schedule_honors_phase_and_does_not_drift_after_execution()
    -> Result<(), Box<dyn Error>> {
        let epoch = epoch(1)?;
        let mut clock = ManualClock::new(epoch, UtcTimestamp::UNIX_EPOCH);
        let mut plan = plan(&[task(0, 0, 10, 3, 10, 2, 5)?], clock.monotonic())?;

        assert_eq!(
            plan.observe(&clock, ScheduleControl::Continue)?,
            ScheduleAction::WaitUntil {
                release: MonotonicTimestamp::new(epoch, 3)
            }
        );
        clock.advance(DurationNanos::new(3))?;
        let first = release(plan.observe(&clock, ScheduleControl::Continue)?)?;
        assert_eq!(first.release_sequence().get(), 0);
        assert_eq!(first.scheduled_release().elapsed_nanos(), 3);
        assert_eq!(first.absolute_deadline().elapsed_nanos(), 13);

        let mut window = execution_window(first.begin(&clock, ScheduleControl::Continue)?)?;
        clock.advance(DurationNanos::new(5))?;
        let checkpoint = window.checkpoint(&clock)?;
        assert!(checkpoint.execution_budget_exceeded());
        assert!(!checkpoint.hard_limit_exceeded());
        assert!(!checkpoint.deadline_missed());
        assert_eq!(
            plan.observe(&clock, ScheduleControl::Continue)?,
            ScheduleAction::WaitUntil {
                release: MonotonicTimestamp::new(epoch, 13)
            }
        );

        clock.advance(DurationNanos::new(5))?;
        let second = release(plan.observe(&clock, ScheduleControl::Continue)?)?;
        assert_eq!(second.release_sequence().get(), 1);
        assert_eq!(second.scheduled_release().elapsed_nanos(), 13);
        Ok(())
    }

    #[test]
    fn same_release_uses_priority_then_handle_and_is_replayable() -> Result<(), Box<dyn Error>> {
        let epoch = epoch(2)?;
        let clock = ManualClock::new(epoch, UtcTimestamp::UNIX_EPOCH);
        let tasks = [
            task(9, -1, 10, 0, 10, 2, 5)?,
            task(7, 4, 10, 0, 10, 2, 5)?,
            task(3, 4, 10, 0, 10, 2, 5)?,
        ];
        let mut left = plan(&tasks, clock.monotonic())?;
        let mut right = plan(&tasks, clock.monotonic())?;

        let left_order = take_handles(&mut left, &clock, tasks.len())?;
        let right_order = take_handles(&mut right, &clock, tasks.len())?;
        assert_eq!(left_order, vec![3, 7, 9]);
        assert_eq!(left_order, right_order);
        Ok(())
    }

    #[test]
    fn different_periods_choose_earliest_absolute_release_before_priority()
    -> Result<(), Box<dyn Error>> {
        let epoch = epoch(10)?;
        let mut clock = ManualClock::new(epoch, UtcTimestamp::UNIX_EPOCH);
        let tasks = [task(1, 10, 10, 0, 9, 2, 5)?, task(2, -10, 6, 0, 5, 2, 4)?];
        let mut plan = plan(&tasks, clock.monotonic())?;

        assert_eq!(
            release(plan.observe(&clock, ScheduleControl::Continue)?)?
                .task()
                .handle()
                .get(),
            1
        );
        assert_eq!(
            release(plan.observe(&clock, ScheduleControl::Continue)?)?
                .task()
                .handle()
                .get(),
            2
        );

        clock.advance(DurationNanos::new(10))?;
        let earlier = release(plan.observe(&clock, ScheduleControl::Continue)?)?;
        assert_eq!(earlier.task().handle().get(), 2);
        assert_eq!(earlier.scheduled_release().elapsed_nanos(), 6);
        let later = release(plan.observe(&clock, ScheduleControl::Continue)?)?;
        assert_eq!(later.task().handle().get(), 1);
        assert_eq!(later.scheduled_release().elapsed_nanos(), 10);
        Ok(())
    }

    #[test]
    fn late_observation_folds_skips_and_never_catches_up() -> Result<(), Box<dyn Error>> {
        let epoch = epoch(3)?;
        let mut clock = ManualClock::new(epoch, UtcTimestamp::UNIX_EPOCH);
        let tasks = [task(0, 0, 10, 0, 5, 2, 4)?];
        let mut on_time = plan(&tasks, clock.monotonic())?;
        clock.advance(DurationNanos::new(32))?;

        let current = release(on_time.observe(&clock, ScheduleControl::Continue)?)?;
        let skipped = current.skipped_releases().ok_or("missing skipped range")?;
        assert_eq!(current.release_sequence().get(), 3);
        assert_eq!(current.scheduled_release().elapsed_nanos(), 30);
        assert_eq!(skipped.first().get(), 0);
        assert_eq!(skipped.last().get(), 2);
        assert_eq!(skipped.count(), 3);
        assert!(matches!(
            current.begin(&clock, ScheduleControl::Continue)?,
            ReleaseReadiness::Execute(_)
        ));
        assert_eq!(
            on_time.observe(&clock, ScheduleControl::Continue)?,
            ScheduleAction::WaitUntil {
                release: MonotonicTimestamp::new(epoch, 40)
            }
        );

        let mut late_clock = ManualClock::new(epoch, UtcTimestamp::UNIX_EPOCH);
        late_clock.advance(DurationNanos::new(36))?;
        let mut late = plan(&tasks, MonotonicTimestamp::new(epoch, 0))?;
        let missed = release(late.observe(&late_clock, ScheduleControl::Continue)?)?;
        assert_eq!(missed.release_sequence().get(), 3);
        assert_eq!(missed.observed_at().elapsed_nanos(), 36);
        assert_eq!(
            missed.begin(&late_clock, ScheduleControl::Continue)?,
            ReleaseReadiness::StartAfterDeadline
        );
        Ok(())
    }

    #[test]
    fn stop_boundary_does_not_consume_release_and_utc_adjustment_changes_nothing()
    -> Result<(), Box<dyn Error>> {
        let epoch = epoch(4)?;
        let mut clock = ManualClock::new(epoch, UtcTimestamp::UNIX_EPOCH);
        let mut plan = plan(&[task(0, 0, 10, 0, 10, 2, 5)?], clock.monotonic())?;

        assert_eq!(
            plan.observe(&clock, ScheduleControl::StopRequested)?,
            ScheduleAction::Stopped {
                observed_at: MonotonicTimestamp::new(epoch, 0)
            }
        );
        let adjusted = UtcTimestamp::new(-1_000, 123)?;
        // 停止后的计划不可直接恢复；UTC 不变性用另一个独立计划验证。
        let capacity = WorkSetCapacity::new(1)?;
        let mut builder = StaticTaskPlanBuilder::new(
            clock.now(),
            capacity,
            WorkSetLimits::new(capacity, usize::MAX),
        )?;
        builder.add_task(task(0, 0, 10, 0, 10, 2, 5)?)?;
        let mut plan = builder.seal()?;
        clock.set_utc(adjusted);
        let decision = release(plan.observe(&clock, ScheduleControl::Continue)?)?;
        assert_eq!(clock.utc(), adjusted);
        assert_eq!(decision.release_sequence().get(), 0);
        assert_eq!(decision.scheduled_release().elapsed_nanos(), 0);
        assert_eq!(decision.absolute_deadline().elapsed_nanos(), 10);
        Ok(())
    }

    #[test]
    fn execution_checkpoint_preserves_equal_boundaries_and_all_overruns()
    -> Result<(), Box<dyn Error>> {
        let epoch = epoch(5)?;
        let mut clock = ManualClock::new(epoch, UtcTimestamp::UNIX_EPOCH);
        let mut plan = plan(&[task(0, 0, 10, 0, 10, 3, 6)?], clock.monotonic())?;
        let decision = release(plan.observe(&clock, ScheduleControl::Continue)?)?;
        let mut window = execution_window(decision.begin(&clock, ScheduleControl::Continue)?)?;

        clock.advance(DurationNanos::new(3))?;
        let at_budget = window.checkpoint(&clock)?;
        assert!(!at_budget.execution_budget_exceeded());
        assert!(!at_budget.hard_limit_exceeded());
        assert!(!at_budget.deadline_missed());

        clock.advance(DurationNanos::new(3))?;
        let at_hard_limit = window.checkpoint(&clock)?;
        assert!(at_hard_limit.execution_budget_exceeded());
        assert!(!at_hard_limit.hard_limit_exceeded());
        assert!(!at_hard_limit.deadline_missed());

        clock.advance(DurationNanos::new(1))?;
        let over_limit = window.checkpoint(&clock)?;
        assert!(over_limit.execution_budget_exceeded());
        assert!(over_limit.hard_limit_exceeded());
        assert!(!over_limit.deadline_missed());
        assert_eq!(over_limit.elapsed().get(), 7);

        clock.advance(DurationNanos::new(3))?;
        let at_deadline = window.checkpoint(&clock)?;
        assert!(at_deadline.hard_limit_exceeded());
        assert!(!at_deadline.deadline_missed());

        clock.advance(DurationNanos::new(1))?;
        let after_deadline = window.checkpoint(&clock)?;
        assert!(after_deadline.execution_budget_exceeded());
        assert!(after_deadline.hard_limit_exceeded());
        assert!(after_deadline.deadline_missed());
        assert_eq!(after_deadline.observed_at().elapsed_nanos(), 11);
        Ok(())
    }

    #[test]
    fn clock_contract_and_checked_time_math_are_enforced() -> Result<(), Box<dyn Error>> {
        let epoch = epoch(6)?;
        let other_epoch = epoch_id(7)?;
        let task = task(0, 0, 4, 2, 4, 1, 2)?;
        let start = MonotonicTimestamp::new(epoch, u64::MAX - 1);
        let mut overflow_plan = plan(&[task], start)?;
        let mut overflow_clock = ManualClock::new(epoch, UtcTimestamp::UNIX_EPOCH);
        overflow_clock.advance(DurationNanos::new(u64::MAX))?;
        assert_eq!(
            overflow_plan.observe(&overflow_clock, ScheduleControl::Continue),
            Err(SchedulerError::ScheduleTimeOverflow {
                handle: LocalHandle::ZERO,
                release_sequence: aurora_control_contracts::ReleaseSequence::ZERO,
            })
        );

        let start = MonotonicTimestamp::new(epoch, 10);
        let mut regressing_plan = plan(&[task], start)?;
        let earlier = FixedClock(MonotonicTimestamp::new(epoch, 9));
        assert_eq!(
            regressing_plan.observe(&earlier, ScheduleControl::Continue),
            Err(SchedulerError::ClockMovedBackwards {
                previous_elapsed_nanos: 10,
                observed_elapsed_nanos: 9,
            })
        );
        let mut plan = plan(&[task], start)?;
        let wrong_epoch = FixedClock(MonotonicTimestamp::new(other_epoch, 10));
        assert!(matches!(
            plan.observe(&wrong_epoch, ScheduleControl::Continue),
            Err(SchedulerError::ClockEpochMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn release_sequence_refuses_wraparound() -> Result<(), Box<dyn Error>> {
        let epoch = epoch(8)?;
        let mut clock = ManualClock::new(epoch, UtcTimestamp::UNIX_EPOCH);
        clock.advance(DurationNanos::new(u64::MAX))?;
        let mut plan = plan(
            &[task(0, 0, 1, 0, 1, 1, 1)?],
            MonotonicTimestamp::new(epoch, 0),
        )?;
        assert_eq!(
            plan.observe(&clock, ScheduleControl::Continue),
            Err(SchedulerError::ReleaseSequenceOverflow {
                handle: LocalHandle::ZERO,
            })
        );
        Ok(())
    }

    #[test]
    fn plan_rejects_duplicate_handles_and_unfilled_capacity() -> Result<(), Box<dyn Error>> {
        let epoch = epoch(9)?;
        let start = MonotonicTimestamp::new(epoch, 0);
        let capacity = WorkSetCapacity::new(2)?;
        let limits = WorkSetLimits::new(capacity, usize::MAX);
        let mut duplicate = StaticTaskPlanBuilder::new(start, capacity, limits)?;
        duplicate.add_task(task(1, 0, 10, 0, 10, 2, 5)?)?;
        duplicate.add_task(task(1, 1, 20, 0, 20, 2, 5)?)?;
        assert_eq!(
            duplicate.seal().err(),
            Some(SchedulerError::DuplicateTaskHandle {
                handle: LocalHandle::new(1)?,
            })
        );

        let mut unfilled = StaticTaskPlanBuilder::new(start, capacity, limits)?;
        unfilled.add_task(task(0, 0, 10, 0, 10, 2, 5)?)?;
        assert!(matches!(
            unfilled.seal(),
            Err(SchedulerError::WorkSet(
                crate::WorkSetError::Uninitialized { .. }
            ))
        ));
        Ok(())
    }

    #[derive(Debug, Clone, Copy)]
    struct FixedClock(MonotonicTimestamp);

    #[test]
    fn checkpoint_regression_is_rejected() -> Result<(), Box<dyn Error>> {
        let epoch = epoch(11)?;
        let clock = FixedClock(MonotonicTimestamp::new(epoch, 0));
        let mut plan = plan(&[task(0, 0, 100, 0, 100, 20, 30)?], clock.now())?;
        let decision = release(plan.observe(&clock, ScheduleControl::Continue)?)?;
        let mut window = execution_window(decision.begin(&clock, ScheduleControl::Continue)?)?;
        assert!(
            window
                .checkpoint(&FixedClock(MonotonicTimestamp::new(epoch, 40)))?
                .hard_limit_exceeded()
        );
        assert!(matches!(
            window.checkpoint(&FixedClock(MonotonicTimestamp::new(epoch, 10))),
            Err(SchedulerError::ClockMovedBackwards { .. })
        ));
        assert!(matches!(
            plan.observe(
                &FixedClock(MonotonicTimestamp::new(epoch, 50)),
                ScheduleControl::Continue
            ),
            Err(SchedulerError::ClockMovedBackwards { .. })
        ));
        Ok(())
    }

    #[test]
    fn task_overflow_does_not_block_a_healthy_task() -> Result<(), Box<dyn Error>> {
        let epoch = epoch(12)?;
        let clock = FixedClock(MonotonicTimestamp::new(epoch, u64::MAX - 10));
        let mut plan = plan(
            &[task(0, 0, 20, 0, 20, 1, 1)?, task(1, 0, 5, 0, 5, 1, 1)?],
            clock.now(),
        )?;
        assert!(matches!(
            plan.observe(&clock, ScheduleControl::Continue),
            Err(SchedulerError::ScheduleTimeOverflow { .. })
        ));
        let healthy = release(plan.observe(&clock, ScheduleControl::Continue)?)?;
        assert_eq!(healthy.task().handle().get(), 1);
        let handle = healthy.task().handle();
        assert!(matches!(
            plan.task_schedule_error(crate::WorkSetIndex::new(0))?,
            Some(SchedulerError::ScheduleTimeOverflow { .. })
        ));
        let later = FixedClock(MonotonicTimestamp::new(epoch, u64::MAX - 5));
        assert_eq!(
            release(plan.observe(&later, ScheduleControl::Continue)?)?
                .task()
                .handle(),
            handle
        );
        Ok(())
    }

    #[test]
    fn begin_rechecks_deadline_and_excludes_selection_time() -> Result<(), Box<dyn Error>> {
        let epoch = epoch(14)?;
        let start = MonotonicTimestamp::new(epoch, 0);
        let specs = [task(0, 0, 100, 0, 50, 10, 20)?];
        for (begin_at, executable) in [(49, true), (50, true), (51, false)] {
            let mut plan = plan(&specs, start)?;
            let selected = release(plan.observe(&FixedClock(start), ScheduleControl::Continue)?)?;
            assert_eq!(selected.observed_at(), start);
            let result = selected.begin(
                &FixedClock(MonotonicTimestamp::new(epoch, begin_at)),
                ScheduleControl::Continue,
            )?;
            if executable {
                let mut window = execution_window(result)?;
                assert_eq!(window.started_at().elapsed_nanos(), begin_at);
                assert_eq!(window.absolute_deadline().elapsed_nanos(), 50);
                let checkpoint =
                    window.checkpoint(&FixedClock(MonotonicTimestamp::new(epoch, begin_at + 1)))?;
                assert_eq!(checkpoint.elapsed().get(), 1);
                assert!(!checkpoint.execution_budget_exceeded());
            } else {
                assert_eq!(result, ReleaseReadiness::StartAfterDeadline);
            }
        }
        Ok(())
    }

    #[test]
    fn execution_time_history_survives_window_end_and_rejects_epoch_change()
    -> Result<(), Box<dyn Error>> {
        let epoch = epoch(15)?;
        let start = MonotonicTimestamp::new(epoch, 0);
        let specs = [task(0, 0, 100, 0, 100, 10, 50)?];
        let mut history = plan(&specs, start)?;
        {
            let selected =
                release(history.observe(&FixedClock(start), ScheduleControl::Continue)?)?;
            let mut window =
                execution_window(selected.begin(&FixedClock(start), ScheduleControl::Continue)?)?;
            window.checkpoint(&FixedClock(MonotonicTimestamp::new(epoch, 30)))?;
        }
        assert!(matches!(
            history.observe(
                &FixedClock(MonotonicTimestamp::new(epoch, 20)),
                ScheduleControl::Continue
            ),
            Err(SchedulerError::ClockMovedBackwards { .. })
        ));

        let mut changed_epoch = plan(&specs, start)?;
        let selected =
            release(changed_epoch.observe(&FixedClock(start), ScheduleControl::Continue)?)?;
        let mut window =
            execution_window(selected.begin(&FixedClock(start), ScheduleControl::Continue)?)?;
        assert!(matches!(
            window.checkpoint(&FixedClock(MonotonicTimestamp::new(epoch_id(16)?, 10))),
            Err(SchedulerError::ClockEpochMismatch { .. })
        ));
        assert!(matches!(
            window.checkpoint(&FixedClock(MonotonicTimestamp::new(epoch, 20))),
            Err(SchedulerError::ClockEpochMismatch { .. })
        ));
        Ok(())
    }

    #[test]
    fn begin_stop_prevents_execution_and_remains_stopped() -> Result<(), Box<dyn Error>> {
        let epoch = epoch(17)?;
        let clock = FixedClock(MonotonicTimestamp::new(epoch, 0));
        let mut plan = plan(&[task(0, 0, 10, 0, 10, 1, 1)?], clock.now())?;
        let selected = release(plan.observe(&clock, ScheduleControl::Continue)?)?;
        assert_eq!(
            selected.begin(&clock, ScheduleControl::StopRequested)?,
            ReleaseReadiness::Stopped
        );
        assert!(matches!(
            plan.observe(&clock, ScheduleControl::Continue)?,
            ScheduleAction::Stopped { .. }
        ));
        Ok(())
    }

    #[test]
    fn every_absolute_time_arithmetic_stage_rejects_overflow() -> Result<(), Box<dyn Error>> {
        use super::{ScheduleOrdinal, release_timing};
        use aurora_control_contracts::ReleaseSequence;
        let epoch = epoch(18)?;
        // 乘法、phase 累加、engine 原点累加以及 deadline 累加各自溢出。
        for (engine_start, spec, ordinal) in [
            (0, task(0, 0, 2, 0, 1, 1, 1)?, u64::MAX),
            (0, task(0, 0, 3, 2, 1, 1, 1)?, u64::MAX / 3),
            (2, task(0, 0, 2, 0, 1, 1, 1)?, u64::MAX / 2),
            (u64::MAX, task(0, 0, 1, 0, 1, 1, 1)?, 0),
        ] {
            assert!(matches!(
                release_timing(
                    MonotonicTimestamp::new(epoch, engine_start),
                    spec,
                    ScheduleOrdinal(ordinal),
                    ReleaseSequence::ZERO
                ),
                Err(SchedulerError::ScheduleTimeOverflow { .. })
            ));
        }
        let (release, deadline) = release_timing(
            MonotonicTimestamp::new(epoch, u64::MAX - 1),
            task(0, 0, 1, 0, 1, 1, 1)?,
            ScheduleOrdinal(0),
            ReleaseSequence::ZERO,
        )?;
        assert_eq!(release.elapsed_nanos(), u64::MAX - 1);
        assert_eq!(deadline.elapsed_nanos(), u64::MAX);
        Ok(())
    }

    #[test]
    fn future_release_overflow_is_reported_once_and_retained() -> Result<(), Box<dyn Error>> {
        let epoch = epoch(19)?;
        let period = u64::MAX / 2 + 1;
        let clock = FixedClock(MonotonicTimestamp::new(epoch, period));
        let mut plan = plan(
            &[task(0, 0, period, 0, 1, 1, 1)?],
            MonotonicTimestamp::new(epoch, 0),
        )?;
        assert_eq!(
            release(plan.observe(&clock, ScheduleControl::Continue)?)?
                .release_sequence()
                .get(),
            1
        );
        assert!(matches!(
            plan.observe(&clock, ScheduleControl::Continue),
            Err(SchedulerError::ScheduleTimeOverflow { .. })
        ));
        assert_eq!(
            plan.observe(&clock, ScheduleControl::Continue),
            Err(SchedulerError::NoSchedulableTask)
        );
        Ok(())
    }

    #[test]
    fn ordinal_and_task_sequence_are_independent_and_both_checked() -> Result<(), Box<dyn Error>> {
        use super::ScheduleOrdinal;
        use aurora_control_contracts::ReleaseSequence;
        let epoch = epoch(20)?;
        let start = MonotonicTimestamp::new(epoch, 0);
        let specs = [task(0, 0, 10, 0, 10, 1, 1)?];
        let mut independent = plan(&specs, start)?;
        // 仅注入内部算术状态，验证两种计数的独立性；这不是 reset 策略实现。
        independent
            .tasks
            .get_mut(crate::WorkSetIndex::new(0))?
            .next_ordinal = ScheduleOrdinal(7);
        let first = release(independent.observe(
            &FixedClock(MonotonicTimestamp::new(epoch, 70)),
            ScheduleControl::Continue,
        )?)?;
        assert_eq!(first.scheduled_release().elapsed_nanos(), 70);
        assert_eq!(first.release_sequence().get(), 0);
        let later = release(independent.observe(
            &FixedClock(MonotonicTimestamp::new(epoch, 110)),
            ScheduleControl::Continue,
        )?)?;
        assert_eq!(later.release_sequence().get(), 4);
        assert_eq!(
            later.skipped_releases().map(super::SkippedReleases::count),
            Some(3)
        );

        for (next_sequence, now) in [(u64::MAX - 2, 30), (u64::MAX - 1, 10)] {
            let mut exhausted = plan(&specs, start)?;
            exhausted
                .tasks
                .get_mut(crate::WorkSetIndex::new(0))?
                .next_release_sequence = ReleaseSequence::new(next_sequence);
            assert!(matches!(
                exhausted.observe(
                    &FixedClock(MonotonicTimestamp::new(epoch, now)),
                    ScheduleControl::Continue
                ),
                Err(SchedulerError::ReleaseSequenceOverflow { .. })
            ));
        }
        Ok(())
    }

    #[test]
    fn stop_is_latched_at_the_boundary() -> Result<(), Box<dyn Error>> {
        let epoch = epoch(13)?;
        let clock = FixedClock(MonotonicTimestamp::new(epoch, 0));
        let mut plan = plan(&[task(0, 0, 10, 0, 10, 1, 1)?], clock.now())?;
        assert!(matches!(
            plan.observe(&clock, ScheduleControl::StopRequested)?,
            ScheduleAction::Stopped { .. }
        ));
        assert!(matches!(
            plan.observe(&clock, ScheduleControl::Continue)?,
            ScheduleAction::Stopped { .. }
        ));
        Ok(())
    }

    impl MonotonicClock for FixedClock {
        fn now(&self) -> MonotonicTimestamp {
            self.0
        }
    }

    fn plan(
        tasks: &[TaskSpec],
        engine_start: MonotonicTimestamp,
    ) -> Result<StaticTaskPlan, SchedulerError> {
        let capacity = WorkSetCapacity::new(tasks.len())?;
        let limits = WorkSetLimits::new(capacity, usize::MAX);
        let mut builder = StaticTaskPlanBuilder::new(engine_start, capacity, limits)?;
        for task in tasks {
            builder.add_task(*task)?;
        }
        builder.seal()
    }

    fn task(
        handle: u32,
        priority: i16,
        period: u64,
        phase: u64,
        deadline: u64,
        budget: u64,
        hard_limit: u64,
    ) -> Result<TaskSpec, ExecutionContractError> {
        let handle =
            LocalHandle::new(handle).map_err(|_| ExecutionContractError::InvalidCapacity)?;
        let timing = TaskTiming::new(
            TaskPeriodNanos::new(period)?,
            TaskPhaseNanos::new(phase),
            RelativeDeadlineNanos::new(deadline)?,
            ExecutionBudgetNanos::new(budget)?,
            HardLimitNanos::new(hard_limit)?,
        )?;
        let miss_policy = MissPolicy::new(MissWindow::new(4, 4)?, 2, 3)?;
        Ok(TaskSpec::new(
            ExecutionContractVersion::V1_0,
            handle,
            TaskPriority::new(priority),
            timing,
            miss_policy,
        ))
    }

    fn epoch(discriminator: u8) -> Result<BootEpochId, aurora_types::IdentifierError> {
        epoch_id(discriminator)
    }

    fn epoch_id(discriminator: u8) -> Result<BootEpochId, aurora_types::IdentifierError> {
        BootEpochId::from_bytes([
            0x01,
            0x89,
            0x0f,
            0x3e,
            0x4c,
            0x7b,
            0x7c,
            0xc2,
            0x98,
            0xc4,
            0xdc,
            0x0c,
            0x0c,
            0x07,
            0x39,
            discriminator,
        ])
    }

    fn release(action: ScheduleAction<'_>) -> Result<super::ReleaseDecision<'_>, &'static str> {
        match action {
            ScheduleAction::Release(decision) => Ok(decision),
            ScheduleAction::Stopped { .. } | ScheduleAction::WaitUntil { .. } => {
                Err("expected a release action")
            }
        }
    }

    fn execution_window(
        readiness: ReleaseReadiness<'_>,
    ) -> Result<ExecutionWindow<'_>, &'static str> {
        match readiness {
            ReleaseReadiness::Execute(window) => Ok(window),
            ReleaseReadiness::StartAfterDeadline => Err("expected an executable release"),
            ReleaseReadiness::Stopped => Err("release was stopped"),
        }
    }

    fn take_handles<C: MonotonicClock + ?Sized>(
        plan: &mut StaticTaskPlan,
        clock: &C,
        count: usize,
    ) -> Result<Vec<u32>, SchedulerError> {
        let mut handles = Vec::with_capacity(count);
        for _ in 0..count {
            match plan.observe(clock, ScheduleControl::Continue)? {
                ScheduleAction::Release(decision) => handles.push(decision.task().handle().get()),
                ScheduleAction::Stopped { .. } | ScheduleAction::WaitUntil { .. } => break,
            }
        }
        Ok(handles)
    }
}
