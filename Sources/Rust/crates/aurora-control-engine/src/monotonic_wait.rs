//! Linux 单次绝对等待及有界停止检查的适配边界。

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::atomic::{AtomicBool, Ordering};

use aurora_types::MonotonicTimestamp;

use crate::MonotonicClock;

/// 无分配、非阻塞的停止标志；不承载需要发布的其他数据。
pub trait StopSignal {
    /// 在 task/release/等待检查边界读取停止请求。
    fn is_stop_requested(&self) -> bool;
}

impl StopSignal for AtomicBool {
    fn is_stop_requested(&self) -> bool {
        // 标志只表达停止，不用于同步其他内存；无需发布/获取 payload。
        self.load(Ordering::Relaxed)
    }
}

/// Linux 时钟适配器的单次绝对等待边界。
///
/// 必须使用与 now 相同原点的 `CLOCK_MONOTONIC` 绝对纳秒值；进程相对时间需要由
/// 适配器做 checked 原点转换。实现只执行一次有界等待，不分配或重试 EINTR。
/// EINTR 返回 Interrupted，其他失败返回 WaitError；禁止静默改成相对 sleep。
/// 阻塞只允许发生在没有正在执行任务的 release 等待边界。
///
/// [`crate::StaticTaskPlan::wait_once`] 将等待截为调用方声明的最大停止检查间隔；
/// OS 调度延迟仍可能推迟唤醒，此接口不承诺硬实时响应。
pub trait MonotonicWait: MonotonicClock {
    /// 等待一次绝对单调目标；不得内部循环重试。
    ///
    /// # Errors
    ///
    /// clock/timespec 无法表示时返回 `InvalidDeadline`；系统调用失败时返回 `PlatformFailure`。
    fn wait_until_once(&mut self, deadline: MonotonicTimestamp) -> Result<WaitOutcome, WaitError>;
}

/// 适配器单次等待的返回原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// 已到达本次绝对等待目标。
    DeadlineReached,
    /// EINTR 或显式唤醒；交还调用方检查停止，不内部重试。
    Interrupted,
}

/// 调度器一次有界等待步骤的结果；调用方每一步重新观察控制状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitStep {
    /// 已达到原始 release；下一步重新 observe，不直接执行旧候选。
    ReleaseReached,
    /// 到达停止检查间隔，但原始 release 尚未到达。
    CheckBoundary,
    /// 等待被中断；不得在无界循环中自动重试。
    Interrupted,
    /// 停止已锁存，后续 Continue 不能恢复。
    Stopped,
}

/// 绝对等待的可穷举配置/适配错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitError {
    /// 最大停止检查间隔为零。
    InvalidCheckInterval,
    /// 适配器无法表示绝对 deadline 或其时钟原点。
    InvalidDeadline,
    /// OS 等待失败；EINTR 应返回 `WaitOutcome::Interrupted`。
    PlatformFailure,
    /// 适配器报告到期，但读取的时间仍早于等待目标。
    EarlyWake,
}

impl Display for WaitError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidCheckInterval => "stop check interval must be non-zero nanoseconds",
            Self::InvalidDeadline => "absolute wait deadline cannot be represented",
            Self::PlatformFailure => "absolute monotonic wait failed",
            Self::EarlyWake => "absolute wait reported completion before its deadline",
        })
    }
}

impl Error for WaitError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ScheduleAction, ScheduleControl, SchedulerError, StaticTaskPlan, StaticTaskPlanBuilder,
        WorkSetCapacity, WorkSetLimits,
    };
    use aurora_control_contracts::{
        ExecutionBudgetNanos, ExecutionContractVersion, HardLimitNanos, MissPolicy, MissWindow,
        RelativeDeadlineNanos, TaskPeriodNanos, TaskPhaseNanos, TaskPriority, TaskSpec, TaskTiming,
    };
    use aurora_types::{BootEpochId, DurationNanos, LocalHandle};

    #[derive(Clone, Copy)]
    enum Wake {
        Complete,
        Interrupt,
        Stop,
        Fail,
        Early,
        Regress,
    }

    struct TestWaiter<'a> {
        now: MonotonicTimestamp,
        wake: Wake,
        stop: &'a AtomicBool,
        targets: Vec<MonotonicTimestamp>,
    }

    impl MonotonicClock for TestWaiter<'_> {
        fn now(&self) -> MonotonicTimestamp {
            self.now
        }
    }

    impl MonotonicWait for TestWaiter<'_> {
        fn wait_until_once(
            &mut self,
            deadline: MonotonicTimestamp,
        ) -> Result<WaitOutcome, WaitError> {
            self.targets.push(deadline);
            match self.wake {
                Wake::Fail => return Err(WaitError::PlatformFailure),
                Wake::Interrupt => return Ok(WaitOutcome::Interrupted),
                Wake::Early => return Ok(WaitOutcome::DeadlineReached),
                Wake::Regress => {
                    self.now = MonotonicTimestamp::new(
                        self.now.boot_epoch(),
                        self.now.elapsed_nanos() - 1,
                    );
                    return Ok(WaitOutcome::Interrupted);
                }
                Wake::Stop => self.stop.store(true, Ordering::Relaxed),
                Wake::Complete => {}
            }
            self.now = deadline;
            Ok(WaitOutcome::DeadlineReached)
        }
    }

    #[test]
    fn wait_uses_absolute_slices_without_moving_release() -> Result<(), Box<dyn Error>> {
        let stop = AtomicBool::new(false);
        let mut waiter = waiter(&stop, Wake::Complete)?;
        let mut plan = plan(waiter.now)?;
        let ScheduleAction::WaitUntil { release } =
            plan.observe(&waiter, ScheduleControl::Continue)?
        else {
            return Err("expected future release".into());
        };
        assert_eq!(
            plan.wait_once(&mut waiter, release, DurationNanos::new(4), &stop)?,
            WaitStep::CheckBoundary
        );
        assert_eq!(
            plan.wait_once(&mut waiter, release, DurationNanos::new(4), &stop)?,
            WaitStep::CheckBoundary
        );
        assert_eq!(
            plan.wait_once(&mut waiter, release, DurationNanos::new(4), &stop)?,
            WaitStep::ReleaseReached
        );
        assert_eq!(
            waiter
                .targets
                .iter()
                .map(|t| t.elapsed_nanos())
                .collect::<Vec<_>>(),
            vec![4, 8, 10]
        );
        assert_eq!(
            plan.wait_once(&mut waiter, release, DurationNanos::new(4), &stop)?,
            WaitStep::ReleaseReached
        );
        assert_eq!(waiter.targets.len(), 3);
        match plan.observe(&waiter, ScheduleControl::Continue)? {
            ScheduleAction::Release(selected) => assert_eq!(selected.scheduled_release(), release),
            _ => return Err("expected release after waiting".into()),
        }
        Ok(())
    }

    #[test]
    fn stop_before_or_during_wait_is_latched() -> Result<(), Box<dyn Error>> {
        for already_requested in [false, true] {
            let stop = AtomicBool::new(already_requested);
            let mut waiter = waiter(&stop, Wake::Stop)?;
            let mut plan = plan(waiter.now)?;
            let release = MonotonicTimestamp::new(waiter.now.boot_epoch(), 100);
            assert_eq!(
                plan.wait_once(&mut waiter, release, DurationNanos::new(5), &stop)?,
                WaitStep::Stopped
            );
            assert_eq!(waiter.targets.len(), usize::from(!already_requested));
            stop.store(false, Ordering::Relaxed);
            assert_eq!(
                plan.wait_once(&mut waiter, release, DurationNanos::new(5), &stop)?,
                WaitStep::Stopped
            );
            assert!(matches!(
                plan.observe(&waiter, ScheduleControl::Continue)?,
                ScheduleAction::Stopped { .. }
            ));
        }
        Ok(())
    }

    #[test]
    fn interrupted_and_failed_waits_are_not_retried() -> Result<(), Box<dyn Error>> {
        let stop = AtomicBool::new(false);
        for (wake, expected) in [
            (Wake::Interrupt, Ok(WaitStep::Interrupted)),
            (
                Wake::Fail,
                Err(SchedulerError::Wait(WaitError::PlatformFailure)),
            ),
            (Wake::Early, Err(SchedulerError::Wait(WaitError::EarlyWake))),
        ] {
            let mut waiter = waiter(&stop, wake)?;
            let mut plan = plan(waiter.now)?;
            let release = MonotonicTimestamp::new(waiter.now.boot_epoch(), 100);
            assert_eq!(
                plan.wait_once(&mut waiter, release, DurationNanos::new(5), &stop),
                expected
            );
            assert_eq!(waiter.targets.len(), 1);
        }
        Ok(())
    }

    #[test]
    fn wait_validation_and_clock_history_cover_extremes() -> Result<(), Box<dyn Error>> {
        let stop = AtomicBool::new(false);
        let mut waiter = waiter(&stop, Wake::Complete)?;
        let epoch = waiter.now.boot_epoch();
        let mut plan = plan(waiter.now)?;
        let release = MonotonicTimestamp::new(epoch, 10);
        assert_eq!(
            plan.wait_once(&mut waiter, release, DurationNanos::new(0), &stop),
            Err(SchedulerError::Wait(WaitError::InvalidCheckInterval))
        );
        let mut bytes = epoch.to_bytes();
        bytes[15] = 2;
        let other = MonotonicTimestamp::new(BootEpochId::from_bytes(bytes)?, 10);
        assert!(matches!(
            plan.wait_once(&mut waiter, other, DurationNanos::new(5), &stop),
            Err(SchedulerError::ClockEpochMismatch { .. })
        ));
        assert!(waiter.targets.is_empty());
        waiter.now = MonotonicTimestamp::new(epoch, u64::MAX - 2);
        assert_eq!(
            plan.wait_once(
                &mut waiter,
                MonotonicTimestamp::new(epoch, u64::MAX),
                DurationNanos::new(u64::MAX),
                &stop
            )?,
            WaitStep::ReleaseReached
        );
        assert_eq!(
            waiter.targets.last().map(|t| t.elapsed_nanos()),
            Some(u64::MAX)
        );
        waiter.now = MonotonicTimestamp::new(epoch, u64::MAX - 1);
        assert!(matches!(
            plan.observe(&waiter, ScheduleControl::Continue),
            Err(SchedulerError::ClockMovedBackwards { .. })
        ));
        Ok(())
    }

    #[test]
    fn interrupted_wait_clock_regression_is_not_hidden() -> Result<(), Box<dyn Error>> {
        let stop = AtomicBool::new(false);
        let mut waiter = waiter(&stop, Wake::Regress)?;
        waiter.now = MonotonicTimestamp::new(waiter.now.boot_epoch(), 2);
        let mut plan = plan(waiter.now)?;
        let release = MonotonicTimestamp::new(waiter.now.boot_epoch(), 10);
        assert!(matches!(
            plan.wait_once(&mut waiter, release, DurationNanos::new(5), &stop),
            Err(SchedulerError::ClockMovedBackwards { .. })
        ));
        assert_eq!(waiter.targets.len(), 1);
        Ok(())
    }

    fn waiter(stop: &AtomicBool, wake: Wake) -> Result<TestWaiter<'_>, Box<dyn Error>> {
        let epoch = BootEpochId::from_bytes([
            1, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 7, 0x39, 1,
        ])?;
        Ok(TestWaiter {
            now: MonotonicTimestamp::new(epoch, 0),
            wake,
            stop,
            targets: Vec::with_capacity(3),
        })
    }

    fn plan(start: MonotonicTimestamp) -> Result<StaticTaskPlan, Box<dyn Error>> {
        let capacity = WorkSetCapacity::new(1)?;
        let mut builder =
            StaticTaskPlanBuilder::new(start, capacity, WorkSetLimits::new(capacity, usize::MAX))?;
        let timing = TaskTiming::new(
            TaskPeriodNanos::new(100)?,
            TaskPhaseNanos::new(10),
            RelativeDeadlineNanos::new(50)?,
            ExecutionBudgetNanos::new(10)?,
            HardLimitNanos::new(20)?,
        )?;
        builder.add_task(TaskSpec::new(
            ExecutionContractVersion::V1_0,
            LocalHandle::ZERO,
            TaskPriority::new(0),
            timing,
            MissPolicy::new(MissWindow::new(4, 4)?, 2, 3)?,
        ))?;
        Ok(builder.seal()?)
    }
}
