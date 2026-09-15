//! Cyclic Workflow 的固定容量运行期扫描内核。
//!
//! 静态计划在初始化期完成所有分配与边界验证。周期期只按固定步骤表扫描一次：本周期活动集
//! 锁存在预分配 scratch 中，所有转移（包括 forward/backedge）只写入下一周期活动集。
//! Workflow 控制状态、调用方状态和 output 共同使用 R0 [`CycleTransaction`]，本 crate 不提供
//! 第二套提交入口；只有外层最终调用 `finish` 才会整体发布。

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::mem::size_of;

use aurora_control_contracts::FaultReason;
use aurora_control_engine::{
    CycleIdentity, CycleTransaction, MonotonicClock, TransactionError, WorkSetIndex,
};
use aurora_types::LocalHandle;

mod structured;

pub use structured::*;

macro_rules! handle_type {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u32);

        impl $name {
            /// 创建句柄，拒绝保留的 `u32::MAX` 哨兵。
            ///
            /// # Errors
            /// 原始值为 `u32::MAX` 时返回 [`WorkflowPlanError::ReservedHandle`]。
            pub const fn new(raw: u32) -> Result<Self, WorkflowPlanError> {
                if raw == u32::MAX {
                    Err(WorkflowPlanError::ReservedHandle)
                } else {
                    Ok(Self(raw))
                }
            }

            /// 返回固定宽度原始值。
            #[must_use]
            pub const fn get(self) -> u32 {
                self.0
            }
        }
    };
}

handle_type!(WorkflowNodeHandle, "静态计划内的稠密可执行节点句柄。");
handle_type!(WorkflowEdgeHandle, "静态计划内的稠密控制边句柄。");

/// 一个节点在 edge 表中拥有的连续区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowEdgeRange {
    /// 第一条边的表索引；空区间等于前一区间末尾。
    pub start: u32,
    /// 区间内边数量。
    pub count: u32,
}

/// 一个静态执行步骤。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowNodeDefinition {
    /// 必须与数组索引完全相等的稠密句柄。
    pub handle: WorkflowNodeHandle,
    /// 此节点唯一拥有的连续 outgoing edge 区间。
    pub outgoing: WorkflowEdgeRange,
}

/// 一条转移的目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowEdgeTarget {
    /// 下一周期激活一个可执行节点。
    Node(WorkflowNodeHandle),
    /// 该控制分支完成，不再激活节点。
    Complete,
}

/// 一条已展开、无需运行期发现的控制边。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowEdgeDefinition {
    /// 必须与数组索引完全相等的稠密句柄。
    pub handle: WorkflowEdgeHandle,
    /// 唯一源节点。
    pub source: WorkflowNodeHandle,
    /// 下一周期目标或显式完成。
    pub target: WorkflowEdgeTarget,
    /// Backedge 的非零单次运行遍历上限；forward edge 为 `None`。
    pub maximum_traversals_per_run: Option<u64>,
}

/// 初始化期输入；切片会被精确复制为运行期固定表。
#[derive(Debug, Clone, Copy)]
pub struct CyclicWorkflowDefinition<'a> {
    /// 此计划唯一绑定的 R0 task。
    pub task_handle: LocalHandle,
    /// 按执行顺序排列、句柄稠密的节点表。
    pub nodes: &'a [WorkflowNodeDefinition],
    /// 按各节点 outgoing 区间排列、句柄稠密的边表。
    pub edges: &'a [WorkflowEdgeDefinition],
    /// 声明初值中唯一应置位的节点；不推断 root。
    pub initial_active: &'a [WorkflowNodeHandle],
    /// 任一周期活动集的固定上限。
    pub maximum_active_nodes: u32,
    /// 任一周期允许执行的固定节点上限。
    pub maximum_node_executions: u32,
    /// Workflow 控制前缀之后由任务逻辑拥有的 state 字节数。
    pub application_state_bytes: usize,
    /// 此 task 的精确 output 字节数。
    pub output_bytes: usize,
}

/// 初始化期静态计划拒绝原因；失败时不产生部分 Runtime。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowPlanError {
    /// 句柄使用了保留的 `u32::MAX`。
    ReservedHandle,
    /// 节点或边数量无法表示为非保留 `u32` handle。
    HandleCapacityExceeded,
    /// 节点或边 handle 与其表索引不一致。
    NonDenseHandle,
    /// outgoing 区间溢出、越界、重叠或留下缺口。
    InvalidEdgeRange,
    /// edge 不属于声明其区间的源节点。
    EdgeOwnerMismatch,
    /// edge 目标不存在于节点表。
    EdgeTargetOutOfRange,
    /// 非空计划缺少初始活动节点，或初始活动节点不存在/重复。
    InvalidInitialActiveNode,
    /// 活动集或执行次数上限为零，或小于初始活动集。
    InvalidExecutionCapacity,
    /// control state 与 application state 总大小溢出。
    StateSizeOverflow,
    /// Backedge 声明了零遍历上限。
    InvalidBackedgeLimit,
    /// 初始化期固定表或 bitmap 分配失败。
    AllocationFailed,
}

impl Display for WorkflowPlanError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "cyclic Workflow plan error: {self:?}")
    }
}

impl Error for WorkflowPlanError {}

/// 一个节点本周期完成后的唯一控制结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowNodeOutcome {
    /// 节点在下一周期继续保持活动。
    Retain,
    /// 采用一条属于当前节点的静态边；目标仅在下一周期生效。
    Take(WorkflowEdgeHandle),
}

/// 节点执行器；实现必须有界、无阻塞且不得在回调中执行 I/O。
pub trait WorkflowNodeExecutor {
    /// 执行一个本周期活动节点。
    ///
    /// `context` 只暴露 application state 与 task output，无法访问 Workflow control prefix。
    /// 返回 Fault 时整份 R0 staging bank 失去提交资格。
    ///
    /// # Errors
    /// 返回明确的 [`FaultReason`] 使 task 锁存相同 Fault。
    fn execute(
        &mut self,
        node: WorkflowNodeHandle,
        context: &mut WorkflowNodeContext<'_, '_, '_>,
    ) -> Result<WorkflowNodeOutcome, FaultReason>;
}

impl<F> WorkflowNodeExecutor for F
where
    F: for<'cycle, 'task, 'plan> FnMut(
        WorkflowNodeHandle,
        &mut WorkflowNodeContext<'cycle, 'task, 'plan>,
    ) -> Result<WorkflowNodeOutcome, FaultReason>,
{
    fn execute(
        &mut self,
        node: WorkflowNodeHandle,
        context: &mut WorkflowNodeContext<'_, '_, '_>,
    ) -> Result<WorkflowNodeOutcome, FaultReason> {
        self(node, context)
    }
}

/// 节点可见的受限 staging 视图。
pub struct WorkflowNodeContext<'cycle, 'task, 'plan> {
    cycle: &'cycle mut CycleTransaction<'task, 'plan>,
    state_offset: usize,
    state_len: usize,
    output_len: usize,
}

impl WorkflowNodeContext<'_, '_, '_> {
    /// 返回节点可访问的 application state 固定字节数。
    #[must_use]
    pub const fn state_len(&self) -> usize {
        self.state_len
    }

    /// 返回节点可访问的 output 固定字节数。
    #[must_use]
    pub const fn output_len(&self) -> usize {
        self.output_len
    }

    /// 读取当前 staging 中的 application state，因此后序活动节点可见前序写入。
    ///
    /// # Errors
    /// 越界时锁存 R0 `CapacityExceeded` 并返回 transaction error。
    pub fn read_state(&mut self, index: WorkSetIndex) -> Result<u8, TransactionError> {
        let mapped = self.map_state(index)?;
        self.cycle.read_state(mapped)
    }

    /// 写入当前 staging 中的 application state。
    ///
    /// # Errors
    /// 越界时锁存 R0 `CapacityExceeded` 并返回 transaction error。
    pub fn write_state(&mut self, index: WorkSetIndex, value: u8) -> Result<(), TransactionError> {
        let mapped = self.map_state(index)?;
        self.cycle.write_state(mapped, value)
    }

    /// 读取当前 staging task output。
    ///
    /// # Errors
    /// 越界时锁存 R0 `CapacityExceeded` 并返回 transaction error。
    pub fn read_output(&mut self, index: WorkSetIndex) -> Result<u8, TransactionError> {
        if index.get() >= self.output_len {
            return self.cycle.read_output(WorkSetIndex::new(self.output_len));
        }
        self.cycle.read_output(index)
    }

    /// 写入当前 staging task output。
    ///
    /// # Errors
    /// 越界时锁存 R0 `CapacityExceeded` 并返回 transaction error。
    pub fn write_output(&mut self, index: WorkSetIndex, value: u8) -> Result<(), TransactionError> {
        if index.get() >= self.output_len {
            return self
                .cycle
                .write_output(WorkSetIndex::new(self.output_len), value);
        }
        self.cycle.write_output(index, value)
    }

    fn map_state(&mut self, index: WorkSetIndex) -> Result<WorkSetIndex, TransactionError> {
        if index.get() >= self.state_len {
            return self
                .cycle
                .read_state(WorkSetIndex::new(self.cycle.state_len()))
                .map(|_| WorkSetIndex::new(0));
        }
        match self.state_offset.checked_add(index.get()) {
            Some(mapped) => Ok(WorkSetIndex::new(mapped)),
            None => self
                .cycle
                .read_state(WorkSetIndex::new(self.cycle.state_len()))
                .map(|_| WorkSetIndex::new(0)),
        }
    }
}

/// 一次扫描的有界结果；不代表 R0 transaction 已提交。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowScanReport {
    /// 本周期实际执行的活动节点数。
    pub executed_nodes: u32,
    /// 下一周期去重后的活动节点数。
    pub next_active_nodes: u32,
    /// 下一周期活动集为空。
    pub completed: bool,
}

/// 周期扫描拒绝或执行失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowScanError {
    /// Cycle 所属 task 与静态计划不一致。
    TaskMismatch,
    /// state/output 精确布局与静态计划不一致。
    ImageLayoutMismatch,
    /// 同一 engine/task/task-epoch/release 已经尝试过扫描。
    DuplicateRelease,
    /// committed 活动 bitmap 含填充位，表明控制状态损坏。
    InvalidActiveBitmap,
    /// 当前活动数超过静态执行上限。
    ExecutionCapacityExceeded,
    /// 去重后的下一活动数超过静态活动上限。
    ActiveCapacityExceeded,
    /// Backedge 已达到静态声明的单次运行遍历上限。
    BackedgeTraversalExceeded,
    /// 执行器选择了不存在或不属于当前节点的 edge。
    InvalidTransition,
    /// 节点返回显式 Fault。
    NodeFault {
        /// 失败节点。
        node: WorkflowNodeHandle,
        /// 锁存到 R0 task 的原因。
        reason: FaultReason,
    },
    /// R0 transaction 的容量、deadline、时钟或锁存错误。
    Transaction(TransactionError),
    /// 内部结果槽与 transaction 执行结果不一致。
    InternalInvariant,
}

impl Display for WorkflowScanError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "cyclic Workflow scan error: {self:?}")
    }
}

impl Error for WorkflowScanError {}

/// 已验证、固定容量的单 task Workflow 扫描器。
#[derive(Debug)]
pub struct CyclicWorkflowRuntime {
    task_handle: LocalHandle,
    nodes: Box<[WorkflowNodeDefinition]>,
    edges: Box<[RuntimeEdge]>,
    initial_control: Box<[u8]>,
    current_active: Box<[u8]>,
    maximum_active_nodes: u32,
    maximum_node_executions: u32,
    application_state_bytes: usize,
    output_bytes: usize,
    total_state_bytes: usize,
    last_scan: Option<CycleIdentity>,
}

impl CyclicWorkflowRuntime {
    /// 验证调用方所提供静态表的内部完整覆盖，并一次性分配所有运行期存储。
    ///
    /// 空节点计划必须使用空活动集；非空计划必须至少声明一个初始活动节点。容量字段仍必须
    /// 非零。edge 区间必须按节点顺序
    /// 精确覆盖所提供的 edge 表一次，禁止缺口、重叠、重复 owner 或悬空 target。本构造器不解析
    /// host artifact，也不推断源 Graph 节点；调用方必须从已认证且通过 R2-02 生成审计的完整产物
    /// 提供这些切片。
    ///
    /// # Errors
    /// 任一 handle、区间、初始活动集、容量、大小或分配检查失败时不返回部分对象。
    pub fn new(definition: CyclicWorkflowDefinition<'_>) -> Result<Self, WorkflowPlanError> {
        validate_count(definition.nodes.len())?;
        validate_count(definition.edges.len())?;
        if definition.maximum_active_nodes == 0 || definition.maximum_node_executions == 0 {
            return Err(WorkflowPlanError::InvalidExecutionCapacity);
        }
        validate_tables(definition.nodes, definition.edges)?;
        if !definition.nodes.is_empty() && definition.initial_active.is_empty() {
            return Err(WorkflowPlanError::InvalidInitialActiveNode);
        }
        let bitmap_bytes = bitmap_bytes(definition.nodes.len())?;
        let (edges, control_state_bytes) = build_runtime_edges(definition.edges, bitmap_bytes)?;
        let mut initial = allocate_zeroed(control_state_bytes)?;
        let mut initial_count = 0_u32;
        for handle in definition.initial_active {
            let index = handle_index(*handle, definition.nodes.len())
                .ok_or(WorkflowPlanError::InvalidInitialActiveNode)?;
            if set_bit(&mut initial, index) {
                initial_count = initial_count
                    .checked_add(1)
                    .ok_or(WorkflowPlanError::InvalidExecutionCapacity)?;
            } else {
                return Err(WorkflowPlanError::InvalidInitialActiveNode);
            }
        }
        if initial_count > definition.maximum_active_nodes
            || initial_count > definition.maximum_node_executions
        {
            return Err(WorkflowPlanError::InvalidExecutionCapacity);
        }
        let total_state_bytes = control_state_bytes
            .checked_add(definition.application_state_bytes)
            .ok_or(WorkflowPlanError::StateSizeOverflow)?;
        let current_active = allocate_zeroed(bitmap_bytes)?;
        Ok(Self {
            task_handle: definition.task_handle,
            nodes: clone_boxed_slice(definition.nodes)?,
            edges,
            initial_control: initial,
            current_active,
            maximum_active_nodes: definition.maximum_active_nodes,
            maximum_node_executions: definition.maximum_node_executions,
            application_state_bytes: definition.application_state_bytes,
            output_bytes: definition.output_bytes,
            total_state_bytes,
            last_scan: None,
        })
    }

    /// 返回应置于 R0 task initial state 最前方的精确控制 bitmap。
    #[must_use]
    pub fn initial_control_state(&self) -> &[u8] {
        &self.initial_control
    }

    /// 返回 Workflow control prefix 的固定字节数。
    #[must_use]
    pub fn control_state_bytes(&self) -> usize {
        self.initial_control.len()
    }

    /// 返回静态可执行节点数。
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// 返回静态控制边数。
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// 在当前 R0 staging bank 中执行一次静态扫描，但不提交 transaction。
    ///
    /// 本周期活动集先完整锁存到固定 scratch，再清空 control prefix 并生成下一周期活动集；
    /// 所以 forward/backedge 均不会同周期追加执行。节点按静态顺序至多执行一次，每个实际节点后
    /// 调用一次 R0 checkpoint。调用方可在本方法返回后继续执行 ST，并最终只调用一次 `finish`。
    ///
    /// # Errors
    /// task/layout/release/活动集/转移/节点或 R0 错误都会先使 transaction 不可提交，再返回原因。
    pub fn stage_scan<C: MonotonicClock + ?Sized, E: WorkflowNodeExecutor + ?Sized>(
        &mut self,
        cycle: &mut CycleTransaction<'_, '_>,
        clock: &C,
        executor: &mut E,
    ) -> Result<WorkflowScanReport, WorkflowScanError> {
        let identity = cycle.identity();
        if identity.task_handle != self.task_handle {
            poison(cycle, FaultReason::TaskExecutionFault);
            return Err(WorkflowScanError::TaskMismatch);
        }
        if cycle.state_len() != self.total_state_bytes || cycle.output_len() != self.output_bytes {
            poison(cycle, FaultReason::CapacityExceeded);
            return Err(WorkflowScanError::ImageLayoutMismatch);
        }
        if self.last_scan == Some(identity) {
            poison(cycle, FaultReason::TaskExecutionFault);
            return Err(WorkflowScanError::DuplicateRelease);
        }
        self.last_scan = Some(identity);

        let nodes = &self.nodes;
        let edges = &self.edges;
        let current_active = &mut self.current_active;
        let control_bytes = self.initial_control.len();
        let maximum_active_nodes = self.maximum_active_nodes;
        let maximum_node_executions = self.maximum_node_executions;
        let application_state_bytes = self.application_state_bytes;
        let output_bytes = self.output_bytes;
        let mut report = None;
        let mut failure = None;
        let transaction_result = cycle.execute(|cycle| {
            match scan_inner(
                cycle,
                clock,
                executor,
                nodes,
                edges,
                current_active,
                control_bytes,
                maximum_active_nodes,
                maximum_node_executions,
                application_state_bytes,
                output_bytes,
            ) {
                Ok(value) => {
                    report = Some(value);
                    Ok(())
                }
                Err(error) => {
                    failure = Some(error);
                    if matches!(error, WorkflowScanError::Transaction(_)) {
                        Ok(())
                    } else {
                        Err(scan_fault_reason(error))
                    }
                }
            }
        });
        match (transaction_result, failure, report) {
            (_, Some(error), _) => Err(error),
            (Err(error), None, _) => Err(WorkflowScanError::Transaction(error)),
            (Ok(()), None, Some(value)) => Ok(value),
            (Ok(()), None, None) => Err(WorkflowScanError::InternalInvariant),
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "all fixed plan and image bounds are passed explicitly into the allocation-free scan"
)]
fn scan_inner<C: MonotonicClock + ?Sized, E: WorkflowNodeExecutor + ?Sized>(
    cycle: &mut CycleTransaction<'_, '_>,
    clock: &C,
    executor: &mut E,
    nodes: &[WorkflowNodeDefinition],
    edges: &[RuntimeEdge],
    current_active: &mut [u8],
    control_bytes: usize,
    maximum_active_nodes: u32,
    maximum_node_executions: u32,
    application_state_bytes: usize,
    output_bytes: usize,
) -> Result<WorkflowScanReport, WorkflowScanError> {
    let current_active_count = latch_current_active(
        cycle,
        current_active,
        nodes.len(),
        maximum_active_nodes,
        maximum_node_executions,
    )?;
    let mut executed = 0_u32;
    let mut next_active = 0_u32;
    for (index, node) in nodes.iter().enumerate() {
        if !bit_is_set(current_active, index) {
            continue;
        }
        executed = executed
            .checked_add(1)
            .ok_or(WorkflowScanError::ExecutionCapacityExceeded)?;
        if executed > maximum_node_executions {
            return Err(WorkflowScanError::ExecutionCapacityExceeded);
        }
        let outcome = {
            let mut context = WorkflowNodeContext {
                cycle,
                state_offset: control_bytes,
                state_len: application_state_bytes,
                output_len: output_bytes,
            };
            executor
                .execute(node.handle, &mut context)
                .map_err(|reason| WorkflowScanError::NodeFault {
                    node: node.handle,
                    reason,
                })?
        };
        cycle
            .checkpoint(clock)
            .map_err(WorkflowScanError::Transaction)?;
        let target = resolve_outcome(cycle, node, edges, outcome)?;
        if let Some(target) = target {
            let target_index =
                handle_index(target, nodes.len()).ok_or(WorkflowScanError::InvalidTransition)?;
            if set_cycle_bit(cycle, target_index)? {
                next_active = next_active
                    .checked_add(1)
                    .ok_or(WorkflowScanError::ActiveCapacityExceeded)?;
                if next_active > maximum_active_nodes {
                    return Err(WorkflowScanError::ActiveCapacityExceeded);
                }
            }
        }
    }
    if executed != current_active_count {
        return Err(WorkflowScanError::InternalInvariant);
    }
    Ok(WorkflowScanReport {
        executed_nodes: executed,
        next_active_nodes: next_active,
        completed: next_active == 0,
    })
}

fn latch_current_active(
    cycle: &mut CycleTransaction<'_, '_>,
    current_active: &mut [u8],
    node_count: usize,
    maximum_active_nodes: u32,
    maximum_node_executions: u32,
) -> Result<u32, WorkflowScanError> {
    for (index, byte) in current_active.iter_mut().enumerate() {
        *byte = cycle
            .read_state(WorkSetIndex::new(index))
            .map_err(WorkflowScanError::Transaction)?;
        cycle
            .write_state(WorkSetIndex::new(index), 0)
            .map_err(WorkflowScanError::Transaction)?;
    }
    if has_padding_bits(current_active, node_count) {
        return Err(WorkflowScanError::InvalidActiveBitmap);
    }
    let count = current_active
        .iter()
        .try_fold(0_u32, |total, byte| total.checked_add(byte.count_ones()))
        .ok_or(WorkflowScanError::ActiveCapacityExceeded)?;
    if count > maximum_active_nodes {
        return Err(WorkflowScanError::ActiveCapacityExceeded);
    }
    if count > maximum_node_executions {
        return Err(WorkflowScanError::ExecutionCapacityExceeded);
    }
    Ok(count)
}

fn resolve_outcome(
    cycle: &mut CycleTransaction<'_, '_>,
    node: &WorkflowNodeDefinition,
    edges: &[RuntimeEdge],
    outcome: WorkflowNodeOutcome,
) -> Result<Option<WorkflowNodeHandle>, WorkflowScanError> {
    let WorkflowNodeOutcome::Take(handle) = outcome else {
        return Ok(Some(node.handle));
    };
    let edge_index =
        handle_index(handle, edges.len()).ok_or(WorkflowScanError::InvalidTransition)?;
    let edge = edges
        .get(edge_index)
        .ok_or(WorkflowScanError::InvalidTransition)?;
    let start =
        usize::try_from(node.outgoing.start).map_err(|_| WorkflowScanError::InvalidTransition)?;
    let count =
        usize::try_from(node.outgoing.count).map_err(|_| WorkflowScanError::InvalidTransition)?;
    let end = start
        .checked_add(count)
        .ok_or(WorkflowScanError::InvalidTransition)?;
    if edge.definition.source != node.handle || edge_index < start || edge_index >= end {
        return Err(WorkflowScanError::InvalidTransition);
    }
    if let (Some(counter_offset), Some(maximum)) = (
        edge.backedge_counter_offset,
        edge.definition.maximum_traversals_per_run,
    ) {
        increment_backedge(cycle, counter_offset, maximum)?;
    }
    Ok(match edge.definition.target {
        WorkflowEdgeTarget::Node(target) => Some(target),
        WorkflowEdgeTarget::Complete => None,
    })
}

fn validate_count(count: usize) -> Result<(), WorkflowPlanError> {
    u32::try_from(count)
        .ok()
        .filter(|value| *value != u32::MAX)
        .map(|_| ())
        .ok_or(WorkflowPlanError::HandleCapacityExceeded)
}

fn validate_tables(
    nodes: &[WorkflowNodeDefinition],
    edges: &[WorkflowEdgeDefinition],
) -> Result<(), WorkflowPlanError> {
    let mut expected_edge_start = 0_usize;
    for (index, node) in nodes.iter().enumerate() {
        if usize::try_from(node.handle.get()) != Ok(index) {
            return Err(WorkflowPlanError::NonDenseHandle);
        }
        let start = usize::try_from(node.outgoing.start)
            .map_err(|_| WorkflowPlanError::InvalidEdgeRange)?;
        let count = usize::try_from(node.outgoing.count)
            .map_err(|_| WorkflowPlanError::InvalidEdgeRange)?;
        let end = start
            .checked_add(count)
            .ok_or(WorkflowPlanError::InvalidEdgeRange)?;
        if start != expected_edge_start || end > edges.len() {
            return Err(WorkflowPlanError::InvalidEdgeRange);
        }
        for edge in &edges[start..end] {
            if edge.source != node.handle {
                return Err(WorkflowPlanError::EdgeOwnerMismatch);
            }
        }
        expected_edge_start = end;
    }
    if expected_edge_start != edges.len() {
        return Err(WorkflowPlanError::InvalidEdgeRange);
    }
    for (index, edge) in edges.iter().enumerate() {
        if usize::try_from(edge.handle.get()) != Ok(index) {
            return Err(WorkflowPlanError::NonDenseHandle);
        }
        if let WorkflowEdgeTarget::Node(target) = edge.target
            && handle_index(target, nodes.len()).is_none()
        {
            return Err(WorkflowPlanError::EdgeTargetOutOfRange);
        }
        if edge.maximum_traversals_per_run == Some(0) {
            return Err(WorkflowPlanError::InvalidBackedgeLimit);
        }
    }
    Ok(())
}

fn bitmap_bytes(node_count: usize) -> Result<usize, WorkflowPlanError> {
    node_count
        .checked_add(7)
        .map(|value| value / 8)
        .ok_or(WorkflowPlanError::StateSizeOverflow)
}

fn allocate_zeroed(length: usize) -> Result<Box<[u8]>, WorkflowPlanError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| WorkflowPlanError::AllocationFailed)?;
    bytes.resize(length, 0);
    Ok(bytes.into_boxed_slice())
}

fn clone_boxed_slice<T: Copy>(values: &[T]) -> Result<Box<[T]>, WorkflowPlanError> {
    let mut copied = Vec::new();
    copied
        .try_reserve_exact(values.len())
        .map_err(|_| WorkflowPlanError::AllocationFailed)?;
    copied.extend_from_slice(values);
    Ok(copied.into_boxed_slice())
}

#[derive(Debug, Clone, Copy)]
struct RuntimeEdge {
    definition: WorkflowEdgeDefinition,
    backedge_counter_offset: Option<usize>,
}

fn build_runtime_edges(
    definitions: &[WorkflowEdgeDefinition],
    bitmap_bytes: usize,
) -> Result<(Box<[RuntimeEdge]>, usize), WorkflowPlanError> {
    let mut edges = Vec::new();
    edges
        .try_reserve_exact(definitions.len())
        .map_err(|_| WorkflowPlanError::AllocationFailed)?;
    let mut next_counter = bitmap_bytes;
    for definition in definitions {
        let backedge_counter_offset = if definition.maximum_traversals_per_run.is_some() {
            let offset = next_counter;
            next_counter = next_counter
                .checked_add(size_of::<u64>())
                .ok_or(WorkflowPlanError::StateSizeOverflow)?;
            Some(offset)
        } else {
            None
        };
        edges.push(RuntimeEdge {
            definition: *definition,
            backedge_counter_offset,
        });
    }
    Ok((edges.into_boxed_slice(), next_counter))
}

fn handle_index<H: HandleValue>(handle: H, length: usize) -> Option<usize> {
    usize::try_from(handle.raw())
        .ok()
        .filter(|index| *index < length)
}

trait HandleValue {
    fn raw(self) -> u32;
}

impl HandleValue for WorkflowNodeHandle {
    fn raw(self) -> u32 {
        self.get()
    }
}

impl HandleValue for WorkflowEdgeHandle {
    fn raw(self) -> u32 {
        self.get()
    }
}

fn set_bit(bitmap: &mut [u8], index: usize) -> bool {
    let byte = index / 8;
    let mask = 1_u8 << (index % 8);
    let was_clear = bitmap[byte] & mask == 0;
    bitmap[byte] |= mask;
    was_clear
}

fn bit_is_set(bitmap: &[u8], index: usize) -> bool {
    bitmap[index / 8] & (1_u8 << (index % 8)) != 0
}

fn has_padding_bits(bitmap: &[u8], node_count: usize) -> bool {
    let remainder = node_count % 8;
    remainder != 0
        && bitmap
            .last()
            .is_some_and(|last| *last & !((1_u8 << remainder) - 1) != 0)
}

fn set_cycle_bit(
    cycle: &mut CycleTransaction<'_, '_>,
    index: usize,
) -> Result<bool, WorkflowScanError> {
    let byte_index = WorkSetIndex::new(index / 8);
    let mask = 1_u8 << (index % 8);
    let byte = cycle
        .read_state(byte_index)
        .map_err(WorkflowScanError::Transaction)?;
    if byte & mask != 0 {
        return Ok(false);
    }
    cycle
        .write_state(byte_index, byte | mask)
        .map_err(WorkflowScanError::Transaction)?;
    Ok(true)
}

fn increment_backedge(
    cycle: &mut CycleTransaction<'_, '_>,
    offset: usize,
    maximum: u64,
) -> Result<(), WorkflowScanError> {
    let mut bytes = [0_u8; size_of::<u64>()];
    for (relative, byte) in bytes.iter_mut().enumerate() {
        let index = offset
            .checked_add(relative)
            .ok_or(WorkflowScanError::ActiveCapacityExceeded)?;
        *byte = cycle
            .read_state(WorkSetIndex::new(index))
            .map_err(WorkflowScanError::Transaction)?;
    }
    let traversals = u64::from_le_bytes(bytes);
    if traversals >= maximum {
        return Err(WorkflowScanError::BackedgeTraversalExceeded);
    }
    let next = traversals
        .checked_add(1)
        .ok_or(WorkflowScanError::BackedgeTraversalExceeded)?
        .to_le_bytes();
    for (relative, byte) in next.iter().enumerate() {
        let index = offset
            .checked_add(relative)
            .ok_or(WorkflowScanError::ActiveCapacityExceeded)?;
        cycle
            .write_state(WorkSetIndex::new(index), *byte)
            .map_err(WorkflowScanError::Transaction)?;
    }
    Ok(())
}

fn poison(cycle: &mut CycleTransaction<'_, '_>, reason: FaultReason) {
    let _result = cycle.execute(|_| Err(reason));
}

const fn scan_fault_reason(error: WorkflowScanError) -> FaultReason {
    match error {
        WorkflowScanError::Transaction(
            TransactionError::ImageOutOfRange | TransactionError::WorkSet(_),
        )
        | WorkflowScanError::ActiveCapacityExceeded
        | WorkflowScanError::ImageLayoutMismatch => FaultReason::CapacityExceeded,
        WorkflowScanError::Transaction(TransactionError::FaultLocked(fault)) => fault.reason,
        WorkflowScanError::NodeFault { reason, .. } => reason,
        _ => FaultReason::TaskExecutionFault,
    }
}
