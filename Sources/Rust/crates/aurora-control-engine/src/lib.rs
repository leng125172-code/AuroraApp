//! Aurora Control Engine 的可移植周期执行核心。
//!
//! 当前 crate 提供初始化期固定工作集、静态绝对调度、state/output 双 bank 事务，
//! 跨任务双槽快照、进程内有界 SPSC，以及 deadline/miss/Fallback 任务状态机。
//! Linux 平台适配由后续工作项交付。

mod bounded_spsc;
mod monotonic_wait;
mod scheduler;
mod snapshot_channel;
mod task_state_machine;
mod transaction;
mod work_set;

pub use bounded_spsc::{
    BoundedSpscConsumer, BoundedSpscProducer, SpscBuildError, SpscCapacity, SpscOverflowPolicy,
    SpscPopError, SpscPushError, SpscPushOutcome, SpscRead, SpscSequence, SpscStatistics,
    bounded_spsc,
};
pub use monotonic_wait::{MonotonicWait, StopSignal, WaitError, WaitOutcome, WaitStep};
pub use scheduler::{
    ExecutionCheckpoint, ExecutionWindow, MonotonicClock, ReleaseDecision, ReleaseReadiness,
    ScheduleAction, ScheduleControl, SchedulerError, SkippedReleases, StaticTaskPlan,
    StaticTaskPlanBuilder,
};
pub use snapshot_channel::{
    LatchedSnapshot, SnapshotChannelDefinition, SnapshotChannelError, SnapshotLatchProgress,
    SnapshotPayloadCapacity, SnapshotPublisher, SnapshotReader, SnapshotReaderStatistics,
};
pub use task_state_machine::{
    FallbackMailboxState, TaskHealthStatistics, TaskStateMachine, TaskStateMachineError,
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
