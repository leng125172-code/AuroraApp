//! Aurora Control Engine 的可移植周期执行核心。
//!
//! 当前 crate 提供初始化期固定工作集，以及基于可注入单调时钟的静态绝对调度。
//! 事务式 state/output bank、跨任务快照、SPSC 和具体 Linux 平台适配由后续
//! R0 工作项交付。

mod scheduler;
mod work_set;

pub use scheduler::{
    ExecutionCheckpoint, ExecutionWindow, MonotonicClock, ReleaseDecision, ReleaseReadiness,
    ScheduleAction, ScheduleControl, SchedulerError, SkippedReleases, StaticTaskPlan,
    StaticTaskPlanBuilder,
};
pub use work_set::{
    FixedWorkSet, FixedWorkSetBuilder, WorkSetCapacity, WorkSetError, WorkSetIndex, WorkSetIndices,
    WorkSetLimits,
};
