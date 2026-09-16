//! Host/offline simulation 的薄适配层。
//!
//! 本模块不解释 Workflow，也不维护第二份状态。调用方仍使用真实 `TaskTransaction::begin`、
//! 显式 `MonotonicClock`、[`StructuredWorkflowRuntime`] 与相同 Trace recorder；因此离线结果与
//! Target 周期路径共享同一 transaction、checkpoint、Fault 和容量语义。

use aurora_control_engine::{CycleTransaction, MonotonicClock};

use crate::{
    StructuredNodeExecutor, StructuredScanError, StructuredScanReport, StructuredWorkflowRuntime,
    WorkflowTraceRecorder,
};

/// 在 host/manual clock 下调用真实结构化 runtime 的 traced scan。
///
/// 调用方必须以真实 `TaskTransaction` 取得 `cycle`，并在返回后执行同一 `finish`/`discard`、
/// recorder finalize 与 flush 流程。本函数不生成额外节点、事件或提交。
///
/// # Errors
/// 原样返回 [`StructuredWorkflowRuntime::stage_scan_traced`] 的错误。
pub fn stage_simulated_release<C: MonotonicClock + ?Sized, E: StructuredNodeExecutor + ?Sized>(
    runtime: &mut StructuredWorkflowRuntime,
    cycle: &mut CycleTransaction<'_, '_>,
    clock: &C,
    executor: &mut E,
    recorder: &mut WorkflowTraceRecorder,
) -> Result<StructuredScanReport, StructuredScanError> {
    runtime.stage_scan_traced(cycle, clock, executor, recorder)
}
