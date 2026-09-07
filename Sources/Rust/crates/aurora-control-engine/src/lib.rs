//! Aurora Control Engine 的可移植周期执行核心。
//!
//! 当前 crate 提供初始化期固定工作集，以及基于可注入单调时钟的静态绝对调度。
//! 同时提供固定字节 state/output 双 bank 事务和受 guard 约束的任务 reset。
//! 跨任务快照、SPSC、完整 miss/Fallback 状态机和 Linux 平台适配由后续工作项交付。

mod monotonic_wait;
mod scheduler;
mod transaction;
mod work_set;

pub use monotonic_wait::{MonotonicWait, StopSignal, WaitError, WaitOutcome, WaitStep};
pub use scheduler::{
    ExecutionCheckpoint, ExecutionWindow, MonotonicClock, ReleaseDecision, ReleaseReadiness,
    ScheduleAction, ScheduleControl, SchedulerError, SkippedReleases, StaticTaskPlan,
    StaticTaskPlanBuilder,
};
pub use transaction::{
    BankValues, BankView, CommitVersion, CycleCommit, CycleStart, CycleTransaction,
    InitializationRejected, LatchedTaskFault, ResetGuard, ResetGuardError, ResetRequest,
    TaskTransaction, TransactionError,
};
pub use work_set::{
    FixedWorkSet, FixedWorkSetBuilder, WorkSetCapacity, WorkSetError, WorkSetIndex, WorkSetIndices,
    WorkSetLimits,
};
