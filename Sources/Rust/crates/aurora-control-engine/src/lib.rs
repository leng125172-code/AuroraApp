//! Aurora Control Engine 的可移植周期执行核心。
//!
//! 当前 crate 只提供初始化期分配、周期期固定索引访问的工作集。调度、事务式
//! state/output bank、跨任务快照、SPSC 和平台适配由后续 R0 工作项交付。

mod work_set;

pub use work_set::{
    FixedWorkSet, FixedWorkSetBuilder, WorkSetCapacity, WorkSetError, WorkSetIndex, WorkSetIndices,
    WorkSetLimits,
};
