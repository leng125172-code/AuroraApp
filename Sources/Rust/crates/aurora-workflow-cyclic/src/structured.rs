//! Fork/Join、Wait、取消和编译期展开子实例的固定容量语义层。
//!
//! 调用方必须从已认证的 R2-02 完整产物提供稠密表；构造期精确验证每张表，周期期不发现
//! 节点、不分配、不阻塞。所有可提交控制状态都存放在 R0 `CycleTransaction` 的 state 前缀。
//! `nodes` 仅包含展开后的可执行 steps；Entry/End 分别折叠为 initial 表和 Complete 边。
//! 持久布局为 active 位图、每实例一字节、每 Wait/backedge 八字节、每 Join token 位图及
//! `WaitAtBoundary` 的败方位图。R2-02 对含 Entry/End 的 expanded nodes 计数，其证明为准入
//! 上限；本模块实际 active 位图可更小，不能把 runtime 精确字节数宣称为上游证明逐字节相等。

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use aurora_control_contracts::FaultReason;
use aurora_control_engine::{
    CycleIdentity, CycleTransaction, MonotonicClock, TransactionError, WorkSetIndex,
};
use aurora_types::LocalHandle;

use crate::{WorkflowEdgeHandle, WorkflowEdgeRange, WorkflowNodeContext, WorkflowNodeHandle};

/// 稠密 Fork 句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredForkHandle(pub u32);

/// 稠密逻辑分支句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredBranchHandle(pub u32);

/// 稠密展开实例句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredInstanceHandle(pub u32);

/// 稠密子工作流调用句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredCallHandle(pub u32);

/// Join 模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredJoinMode {
    /// 互斥单 token 汇合。
    Merge,
    /// 等待全部分支 token。
    All,
    /// 首批到达中按 branchOrder 选择最小分支。
    Any(StructuredJoinPolicy),
}

/// `JoinAny` 败方策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredJoinPolicy {
    /// 提交点取消败方未来活动。
    CancelOthers,
    /// 败方继续，完成受所有活动约束。
    KeepRunning,
    /// 败方到成功取消边界才停止。
    WaitAtBoundary,
}

/// 周期节点种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredNodeKind {
    /// 每扫描调用一次周期安全执行器。
    Action,
    /// 有界执行器按静态 priority 求值并选择首条真边；全 false 返回 Retain。
    Decision,
    /// 激活配对 Fork 的全部有序分支。
    Fork(StructuredForkHandle),
    /// 显式汇合节点。
    Join {
        /// 配对 Fork；Merge 必须为 None。
        fork: Option<StructuredForkHandle>,
        /// 显式汇合模式。
        mode: StructuredJoinMode,
    },
    /// 按计划 release 序号等待。
    WaitCycles {
        /// 非零计划 release 周期数。
        wait_cycles: u64,
    },
    /// 每扫描求值条件；None 表示永久等待。
    WaitCondition {
        /// 非零超时周期数；None 为永久等待。
        timeout_cycles: Option<u64>,
    },
    /// 固定展开调用实例。
    Subworkflow(StructuredCallHandle),
}

/// 稠密执行节点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredNodeDefinition {
    /// 稠密表索引句柄。
    pub handle: WorkflowNodeHandle,
    /// 所属展开实例。
    pub instance: StructuredInstanceHandle,
    /// 静态节点语义。
    pub kind: StructuredNodeKind,
    /// 该节点唯一拥有的连续边区间。
    pub outgoing: WorkflowEdgeRange,
    /// 成功完成时可停止 pending-cancel 分支。
    pub cancellation_boundary: bool,
}

/// 控制边目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredEdgeTarget {
    /// 下一扫描可执行节点。
    Node(WorkflowNodeHandle),
    /// 请求此分支完成。
    Complete,
}

/// 一条控制边；`branch` 仅用于 Fork 激活或 paired Join 到达。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredEdgeDefinition {
    /// 稠密表索引句柄。
    pub handle: WorkflowEdgeHandle,
    /// 复制源字节索引或控制源节点。
    pub source: WorkflowNodeHandle,
    /// 复制目标字节索引或控制目标。
    pub target: StructuredEdgeTarget,
    /// 所属静态逻辑分支。
    pub branch: Option<StructuredBranchHandle>,
    /// Backedge 的每展开实例、每次顶层运行非零上限；普通边为 None。
    pub maximum_traversals_per_run: Option<u64>,
}

/// Fork 的 branch 表连续区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredBranchRange {
    /// 连续表区间首索引。
    pub start: u32,
    /// 连续表区间元素数。
    pub count: u32,
}

/// 一个 Fork。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredForkDefinition {
    /// 稠密表索引句柄。
    pub handle: StructuredForkHandle,
    /// 对应可执行节点。
    pub node: WorkflowNodeHandle,
    /// 按 branchOrder 排列的完整分支区间。
    pub branches: StructuredBranchRange,
}

/// branch 表必须按 `(fork, branch_order)` 排列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredBranchDefinition {
    /// 稠密表索引句柄。
    pub handle: StructuredBranchHandle,
    /// 配对 Fork；Merge 必须为 None。
    pub fork: StructuredForkHandle,
    /// 从零开始连续的确定性分支次序。
    pub branch_order: u32,
    /// Fork 到分支入口的唯一边。
    pub activation_edge: WorkflowEdgeHandle,
}

/// 节点可属于多个严格嵌套并行区域；重复 pair 被拒绝。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredBranchMembership {
    /// 对应可执行节点。
    pub node: WorkflowNodeHandle,
    /// 所属静态逻辑分支。
    pub branch: StructuredBranchHandle,
}

/// 展开实例。根实例没有 `parent_call`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredInstanceDefinition {
    /// 稠密表索引句柄。
    pub handle: StructuredInstanceHandle,
    /// 父调用；只有根实例为 None。
    pub parent_call: Option<StructuredCallHandle>,
}

/// state 字节复制项；索引相对 application state 起点。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredStateCopy {
    /// 复制源字节索引或控制源节点。
    pub source: usize,
    /// 复制目标字节索引或控制目标。
    pub target: usize,
}

/// 子工作流调用及其固定表区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredSubworkflowDefinition {
    /// 稠密表索引句柄。
    pub handle: StructuredCallHandle,
    /// 对应可执行节点。
    pub node: WorkflowNodeHandle,
    /// 独立展开子实例。
    pub child_instance: StructuredInstanceHandle,
    /// 子实例入口节点表区间。
    pub initial_nodes: StructuredBranchRange,
    /// 首次激活时按声明顺序复制输入。
    pub input_copies: StructuredBranchRange,
    /// 成功完成时按声明顺序复制输出。
    pub output_copies: StructuredBranchRange,
}

/// 完整低级计划。
#[derive(Debug, Clone, Copy)]
pub struct StructuredWorkflowDefinition<'a> {
    /// 唯一绑定的 R0 task。
    pub task_handle: LocalHandle,
    /// 按全局执行顺序排列的完整可执行 step 表，不包含 Entry/End。
    pub nodes: &'a [StructuredNodeDefinition],
    /// 按节点 outgoing 区间排列的完整边表。
    pub edges: &'a [StructuredEdgeDefinition],
    /// 根实例初始活动节点。
    pub initial_active: &'a [WorkflowNodeHandle],
    /// 稠密 Fork 表。
    pub forks: &'a [StructuredForkDefinition],
    /// 按 branchOrder 排列的完整分支区间。
    pub branches: &'a [StructuredBranchDefinition],
    /// 完整的节点与嵌套分支归属关系。
    pub memberships: &'a [StructuredBranchMembership],
    /// 稠密展开实例表，根实例为零。
    pub instances: &'a [StructuredInstanceDefinition],
    /// 稠密子调用表。
    pub calls: &'a [StructuredSubworkflowDefinition],
    /// 所有子调用入口的连续表。
    pub call_initial_nodes: &'a [WorkflowNodeHandle],
    /// 所有调用的输入及输出复制表。
    pub state_copies: &'a [StructuredStateCopy],
    /// 每扫描未来活动节点上限。
    pub maximum_active_nodes: u32,
    /// 每扫描可执行节点上限。
    pub maximum_node_executions: u32,
    /// 同时 pending-cancel 分支上限。
    pub maximum_pending_cancellations: u32,
    /// 控制前缀之后 application state 精确字节数。
    pub application_state_bytes: usize,
    /// R0 output 精确字节数。
    pub output_bytes: usize,
}

/// 构造期完整性错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredPlanError {
    /// 句柄保留或不稠密。
    ReservedOrNonDense,
    /// 表条目缺失或多余。
    MissingOrExtraEntry,
    /// 唯一条目重复。
    DuplicateEntry,
    /// 引用不存在或所有权错误。
    InvalidReference,
    /// 区间没有精确覆盖固定表。
    InvalidRange,
    /// 节点出度或结构种类无效。
    InvalidNodeShape,
    /// 分支顺序不连续或重复。
    InvalidBranchOrder,
    /// 等待周期为零。
    InvalidWait,
    /// Backedge 上限为零，或普通边逆于静态执行顺序。
    InvalidBackedge,
    /// 取消边界定义不合法。
    InvalidCancellation,
    /// 实例关系或复制表无效。
    InvalidSubworkflow,
    /// 容量为零或无法表示。
    InvalidCapacity,
    /// 状态布局 checked 运算溢出。
    StateSizeOverflow,
    /// 初始化分配失败。
    AllocationFailed,
}

impl Display for StructuredPlanError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "structured Workflow plan error: {self:?}")
    }
}
impl Error for StructuredPlanError {}

/// Action/condition 回调结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredNodeOutcome {
    /// 下一周期继续执行当前节点。
    Retain,
    /// 选择属于当前节点的静态边。
    Take(WorkflowEdgeHandle),
    /// condition Wait 的 BOOL 结果。
    Condition(bool),
}

/// 仅 Action、Decision 与 condition Wait 会调用；结构节点由 Runtime 决定。
pub trait StructuredNodeExecutor {
    /// 执行 Action、按静态 priority 选择 Decision 或求值 condition；周期路径必须有界且无分配、无阻塞。
    ///
    /// # Errors
    /// 返回 `FaultReason` 会原样锁存到整个 R0 task。
    fn execute(
        &mut self,
        node: WorkflowNodeHandle,
        context: &mut WorkflowNodeContext<'_, '_, '_>,
    ) -> Result<StructuredNodeOutcome, FaultReason>;
}

impl<F> StructuredNodeExecutor for F
where
    F: for<'a, 'b, 'c> FnMut(
        WorkflowNodeHandle,
        &mut WorkflowNodeContext<'a, 'b, 'c>,
    ) -> Result<StructuredNodeOutcome, FaultReason>,
{
    fn execute(
        &mut self,
        node: WorkflowNodeHandle,
        context: &mut WorkflowNodeContext<'_, '_, '_>,
    ) -> Result<StructuredNodeOutcome, FaultReason> {
        self(node, context)
    }
}

/// 结构化扫描报告。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructuredScanReport {
    /// 本扫描实际执行节点数。
    pub executed_nodes: u32,
    /// 下次扫描活动节点数。
    pub next_active_nodes: u32,
    /// 没有活动节点或待取消分支。
    pub completed: bool,
}

/// 结构化扫描失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredScanError {
    /// 当前 R0 task 与计划不一致。
    TaskMismatch,
    /// 状态或输出精确长度不匹配。
    ImageLayoutMismatch,
    /// 同一 release 重复扫描。
    DuplicateRelease,
    /// 控制状态无效。
    InvalidControlState,
    /// 节点执行次数超过固定上限。
    ExecutionCapacityExceeded,
    /// 未来活动集超过固定容量。
    ActiveCapacityExceeded,
    /// 待取消分支超过固定容量。
    PendingCancellationExceeded,
    /// 执行器选择非法边或结果。
    InvalidTransition,
    /// WFF0002：backedge 尝试超过每次顶层运行上限。
    BackedgeTraversalExceeded {
        /// 触发失败的展开边。
        edge: WorkflowEdgeHandle,
    },
    /// 条件为 false 且已到超时。
    WaitTimeout {
        /// 对应可执行节点。
        node: WorkflowNodeHandle,
    },
    /// 取消没有在静态边界完成。
    CancellationBoundaryExceeded {
        /// 对应可执行节点。
        node: WorkflowNodeHandle,
    },
    /// 根实例节点原始 Fault。
    NodeFault {
        /// 对应可执行节点。
        node: WorkflowNodeHandle,
        /// 必须保留的原始 R0 `FaultReason`。
        reason: FaultReason,
    },
    /// 子实例节点原始 Fault。
    SubworkflowFault {
        /// 对应可执行节点。
        node: WorkflowNodeHandle,
        /// 必须保留的原始 R0 `FaultReason`。
        reason: FaultReason,
    },
    /// R0 原始 transaction 错误。
    Transaction(TransactionError),
    /// 内部结果不变量失败。
    InternalInvariant,
}

impl Display for StructuredScanError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "structured Workflow scan error: {self:?}")
    }
}
impl Error for StructuredScanError {}

/// 已验证的固定容量结构化 Runtime。
#[derive(Debug)]
pub struct StructuredWorkflowRuntime {
    task: LocalHandle,
    /// 按全局执行顺序排列的完整节点表。
    nodes: Box<[StructuredNodeDefinition]>,
    /// 按节点 outgoing 区间排列的完整边表。
    edges: Box<[StructuredEdgeDefinition]>,
    /// 稠密 Fork 表。
    forks: Box<[StructuredForkDefinition]>,
    /// 按 branchOrder 排列的完整分支区间。
    branches: Box<[StructuredBranchDefinition]>,
    /// 完整的节点与嵌套分支归属关系。
    memberships: Box<[StructuredBranchMembership]>,
    /// 稠密展开实例表，根实例为零。
    instances: Box<[StructuredInstanceDefinition]>,
    /// 稠密子调用表。
    calls: Box<[StructuredSubworkflowDefinition]>,
    call_initial: Box<[WorkflowNodeHandle]>,
    copies: Box<[StructuredStateCopy]>,
    initial: Box<[u8]>,
    current: Box<[u8]>,
    current_tokens: Box<[u8]>,
    canceled: Box<[u8]>,
    resolved: Box<[u8]>,
    scan_position: usize,
    completion_candidate: bool,
    layout: ControlLayout,
    max_active: u32,
    max_executions: u32,
    max_pending: u32,
    app_bytes: usize,
    /// R0 output 精确字节数。
    output_bytes: usize,
    last_scan: Option<CycleIdentity>,
}

impl StructuredWorkflowRuntime {
    /// 初始化期验证完整表并精确分配状态；失败不返回部分计划。
    ///
    /// # Errors
    /// 表缺失、重复、越界、容量或分配错误时拒绝构造。
    pub fn new(d: StructuredWorkflowDefinition<'_>) -> Result<Self, StructuredPlanError> {
        validate(&d)?;
        let layout = ControlLayout::new(&d)?;
        if layout.pending_bits > d.maximum_pending_cancellations as usize {
            return Err(StructuredPlanError::InvalidCapacity);
        }
        let bytes = layout.bytes;
        bytes
            .checked_add(d.application_state_bytes)
            .ok_or(StructuredPlanError::StateSizeOverflow)?;
        let mut initial = zeroed(bytes)?;
        for n in d.initial_active {
            let i = n.get() as usize;
            initial[i / 8] |= 1 << (i % 8);
        }
        initial[layout.instance_offset] = if d.initial_active.is_empty() { 2 } else { 1 };
        Ok(Self {
            task: d.task_handle,
            nodes: copied(d.nodes)?,
            edges: copied(d.edges)?,
            forks: copied(d.forks)?,
            branches: copied(d.branches)?,
            memberships: copied(d.memberships)?,
            instances: copied(d.instances)?,
            calls: copied(d.calls)?,
            call_initial: copied(d.call_initial_nodes)?,
            copies: copied(d.state_copies)?,
            initial,
            current: zeroed(d.nodes.len())?,
            current_tokens: zeroed(d.branches.len())?,
            canceled: zeroed(d.branches.len())?,
            resolved: zeroed(d.forks.len())?,
            scan_position: 0,
            completion_candidate: false,
            layout,
            max_active: d.maximum_active_nodes,
            max_executions: d.maximum_node_executions,
            max_pending: d.maximum_pending_cancellations,
            app_bytes: d.application_state_bytes,
            output_bytes: d.output_bytes,
            last_scan: None,
        })
    }

    /// R0 初始 state 的控制前缀；其后追加 application state。
    #[must_use]
    pub fn initial_control_state(&self) -> &[u8] {
        &self.initial
    }
    /// 精确控制前缀字节数。
    #[must_use]
    pub fn control_state_bytes(&self) -> usize {
        self.initial.len()
    }

    /// 在同一 R0 staging bank 扫描；只有外层 `finish` 发布状态和输出。
    ///
    /// 所有循环由固定表长度界定；本方法及内部路径无分配、锁和 I/O。
    /// # Errors
    /// 任意节点 Fault、布局、容量或 deadline 错误使整个 transaction 不可提交。
    pub fn stage_scan<C: MonotonicClock + ?Sized, E: StructuredNodeExecutor + ?Sized>(
        &mut self,
        cycle: &mut CycleTransaction<'_, '_>,
        clock: &C,
        executor: &mut E,
    ) -> Result<StructuredScanReport, StructuredScanError> {
        let identity = cycle.identity();
        let invalid = if identity.task_handle != self.task {
            Some(StructuredScanError::TaskMismatch)
        } else if cycle.state_len() != self.initial.len() + self.app_bytes
            || cycle.output_len() != self.output_bytes
        {
            Some(StructuredScanError::ImageLayoutMismatch)
        } else if self.last_scan == Some(identity) {
            Some(StructuredScanError::DuplicateRelease)
        } else {
            None
        };
        if let Some(error) = invalid {
            crate::poison(cycle, reason(error));
            return Err(error);
        }
        self.last_scan = Some(identity);
        let mut result = Err(StructuredScanError::InternalInvariant);
        let transaction = cycle.execute(|cycle| {
            result = self.scan(cycle, clock, executor);
            match result {
                Ok(_) | Err(StructuredScanError::Transaction(_)) => Ok(()),
                Err(error) => Err(reason(error)),
            }
        });
        match (result, transaction) {
            (Err(error), _) => Err(error),
            (_, Err(error)) => Err(StructuredScanError::Transaction(error)),
            (Ok(report), Ok(())) => Ok(report),
        }
    }

    fn scan<C: MonotonicClock + ?Sized, E: StructuredNodeExecutor + ?Sized>(
        &mut self,
        cycle: &mut CycleTransaction<'_, '_>,
        clock: &C,
        executor: &mut E,
    ) -> Result<StructuredScanReport, StructuredScanError> {
        self.canceled.fill(0);
        self.validate_state(cycle)?;
        for i in 0..self.nodes.len() {
            self.current[i] = u8::from(bit(cycle, 0, i)?);
        }
        // token 与 active set 一同锁存，当前扫描的新到达不改变已选中的首批赢家。
        for b in 0..self.branches.len() {
            self.current_tokens[b] = u8::from(self.token(cycle, b)?);
        }
        for f in 0..self.forks.len() {
            // Fork 执行后空闲的 active 位与非空 token 组合表示 resolved；该位不是活动节点。
            // 新一轮 Fork 激活会清 token，因此不会与真正的 Fork active 混淆。
            let node = self.forks[f].node.get() as usize;
            self.resolved[f] = u8::from(
                self.current[node] != 0
                    && range(self.forks[f].branches).any(|b| self.current_tokens[b] != 0),
            );
            if self.resolved[f] != 0 {
                self.current[node] = 0;
            }
        }
        for i in 0..self.nodes.len() {
            if self.current[i] != 0 {
                set_bit(cycle, 0, i, false)?;
            }
        }
        let mut executed = 0_u32;
        for i in 0..self.nodes.len() {
            if self.current[i] == 0 {
                continue;
            }
            executed += 1;
            if executed > self.max_executions {
                return Err(StructuredScanError::ExecutionCapacityExceeded);
            }
            self.scan_position = i;
            self.completion_candidate = false;
            self.step(cycle, executor, i)?;
            cycle
                .checkpoint(clock)
                .map_err(StructuredScanError::Transaction)?;
            // 子实例的最后一个当前节点可能早于同一扫描中的其他分支节点。必须在该
            // executionOrder 边界传播输出，后序节点才能读取本周期 staging 值；尚未扫描的
            // 同实例 current 节点仍计为运行中，避免提前完成或多复制输出。
            if self.completion_candidate {
                self.finish_calls(cycle)?;
            }
        }
        self.scan_position = self.nodes.len();
        // 先取消再传播调用完成，避免把已取消的子实例误认为成功完成而复制输出。
        self.apply_cancellations(cycle)?;
        self.finish_calls(cycle)?;
        self.apply_cancellations(cycle)?;
        let mut pending = 0_u32;
        for b in 0..self.branches.len() {
            if self.is_pending(cycle, b)? {
                pending += 1;
            }
        }
        if pending > self.max_pending {
            return Err(StructuredScanError::PendingCancellationExceeded);
        }
        let mut active = 0_u32;
        for i in 0..self.nodes.len() {
            active += u32::from(self.is_active(cycle, i)?);
        }
        if active > self.max_active {
            return Err(StructuredScanError::ActiveCapacityExceeded);
        }
        let mut running_call = false;
        for c in 0..self.calls.len() {
            running_call |= read(cycle, self.call_offset(c))? != 0;
        }
        let completed = active == 0 && pending == 0 && !running_call;
        write(
            cycle,
            self.layout.instance_offset,
            if completed { 2 } else { 1 },
        )?;
        Ok(StructuredScanReport {
            executed_nodes: executed,
            next_active_nodes: active,
            completed,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "节点目录集中匹配以便逐项审计扫描语义"
    )]
    fn step<E: StructuredNodeExecutor + ?Sized>(
        &mut self,
        cycle: &mut CycleTransaction<'_, '_>,
        executor: &mut E,
        i: usize,
    ) -> Result<(), StructuredScanError> {
        let node = self.nodes[i];
        let first = node.outgoing.start as usize;
        match node.kind {
            StructuredNodeKind::Action | StructuredNodeKind::Decision => match self
                .callback(cycle, executor, node)?
            {
                StructuredNodeOutcome::Retain => set_bit(cycle, 0, i, true),
                StructuredNodeOutcome::Take(edge) => self.take(cycle, i, edge.get() as usize),
                StructuredNodeOutcome::Condition(_) => Err(StructuredScanError::InvalidTransition),
            },
            StructuredNodeKind::Fork(f) => {
                let branch_range = self.forks[f.0 as usize].branches;
                self.resolved[f.0 as usize] = 0;
                for b in range(branch_range) {
                    self.set_token(cycle, b, false)?;
                    self.canceled[b] = 0;
                }
                self.clear_pending(cycle, f.0 as usize)?;
                for b in range(branch_range) {
                    self.take(cycle, i, self.branches[b].activation_edge.get() as usize)?;
                }
                Ok(())
            }
            StructuredNodeKind::Join {
                fork: None,
                mode: StructuredJoinMode::Merge,
            } => self.take(cycle, i, first),
            StructuredNodeKind::Join {
                fork: Some(f),
                mode,
            } => {
                let f = f.0 as usize;
                if self.resolved[f] != 0 {
                    return Ok(());
                }
                let branch_range = self.forks[f].branches;
                let mut winner = None;
                let mut arrived = 0;
                for b in range(branch_range) {
                    if self.current_tokens[b] != 0 {
                        arrived += 1;
                        if winner.is_none() {
                            winner = Some(b);
                        }
                    }
                }
                if arrived == 0
                    || (mode == StructuredJoinMode::All && arrived != branch_range.count)
                {
                    return set_bit(cycle, 0, i, true);
                }
                if let StructuredJoinMode::Any(policy) = mode {
                    for b in range(branch_range) {
                        if Some(b) != winner {
                            match policy {
                                StructuredJoinPolicy::CancelOthers => self.canceled[b] = 1,
                                StructuredJoinPolicy::KeepRunning => {}
                                StructuredJoinPolicy::WaitAtBoundary => {
                                    if self.current_tokens[b] == 0 && !self.token(cycle, b)? {
                                        let winner =
                                            winner.ok_or(StructuredScanError::InternalInvariant)?;
                                        let slot = self
                                            .pending_slot_for_winner(b, winner)
                                            .ok_or(StructuredScanError::InternalInvariant)?;
                                        set_bit(cycle, self.layout.pending_offset, slot, true)?;
                                    } else {
                                        self.canceled[b] = 1;
                                    }
                                }
                            }
                        }
                    }
                    // one-hot 赢家同时为下一扫描的 pending loser 位映射提供稳定锚点。
                    for b in range(branch_range) {
                        self.set_token(cycle, b, Some(b) == winner)?;
                    }
                }
                self.resolved[f] = 1;
                set_bit(cycle, 0, self.forks[f].node.get() as usize, true)?;
                set_bit(cycle, 0, i, false)?;
                self.take(cycle, i, first)
            }
            StructuredNodeKind::Join { .. } => Err(StructuredScanError::InvalidControlState),
            StructuredNodeKind::WaitCycles { wait_cycles } => {
                let elapsed = self.elapsed(cycle, i)?;
                if elapsed >= wait_cycles {
                    self.take(cycle, i, first)
                } else {
                    set_bit(cycle, 0, i, true)
                }
            }
            StructuredNodeKind::WaitCondition { timeout_cycles } => {
                let StructuredNodeOutcome::Condition(satisfied) =
                    self.callback(cycle, executor, node)?
                else {
                    return Err(StructuredScanError::InvalidTransition);
                };
                if satisfied {
                    return self.take(cycle, i, first);
                }
                if let Some(timeout) = timeout_cycles
                    && self.elapsed(cycle, i)? >= timeout
                {
                    return Err(StructuredScanError::WaitTimeout { node: node.handle });
                }
                set_bit(cycle, 0, i, true)
            }
            StructuredNodeKind::Subworkflow(c) => {
                let c = c.0 as usize;
                let call = self.calls[c];
                if read(cycle, self.call_offset(c))? == 0 {
                    self.copy(cycle, call.input_copies)?;
                    write(cycle, self.call_offset(c), 1)?;
                    for entry in range(call.initial_nodes) {
                        self.activate(cycle, self.call_initial[entry].get() as usize)?;
                    }
                    self.completion_candidate = call.initial_nodes.count == 0;
                    return Ok(());
                }
                Err(StructuredScanError::InvalidControlState)
            }
        }
    }

    fn apply_cancellations(
        &mut self,
        cycle: &mut CycleTransaction<'_, '_>,
    ) -> Result<(), StructuredScanError> {
        // 所有循环受固定 membership/instance 表约束，不递归、不分配。
        for index in 0..self.memberships.len() {
            let m = self.memberships[index];
            let b = m.branch.0 as usize;
            if self.canceled[b] == 0 {
                continue;
            }
            set_bit(cycle, 0, m.node.get() as usize, false)?;
            if let StructuredNodeKind::Fork(f) = self.nodes[m.node.get() as usize].kind {
                for b in range(self.forks[f.0 as usize].branches) {
                    self.set_token(cycle, b, false)?;
                }
                self.clear_pending(cycle, f.0 as usize)?;
                self.resolved[f.0 as usize] = 0;
            }
            if let StructuredNodeKind::Subworkflow(call) = self.nodes[m.node.get() as usize].kind {
                self.cancel_call(cycle, call.0 as usize)?;
            }
        }
        Ok(())
    }

    fn is_descendant(
        &self,
        mut instance: StructuredInstanceHandle,
        ancestor: StructuredInstanceHandle,
    ) -> bool {
        for _ in 0..self.instances.len() {
            if instance == ancestor {
                return true;
            }
            let Some(call) = self.instances[instance.0 as usize].parent_call else {
                return false;
            };
            instance = self.nodes[self.calls[call.0 as usize].node.get() as usize].instance;
        }
        false
    }

    fn cancel_call(
        &mut self,
        cycle: &mut CycleTransaction<'_, '_>,
        call: usize,
    ) -> Result<(), StructuredScanError> {
        let root = self.calls[call].child_instance;
        for (i, node) in self.nodes.iter().enumerate() {
            if self.is_descendant(node.instance, root) {
                set_bit(cycle, 0, i, false)?;
            }
        }
        for (i, c) in self.calls.iter().enumerate() {
            if self.is_descendant(c.child_instance, root) {
                write(cycle, self.call_offset(i), 0)?;
            }
        }
        for f in 0..self.forks.len() {
            let fork = self.forks[f];
            if self.is_descendant(self.nodes[fork.node.get() as usize].instance, root) {
                for b in range(fork.branches) {
                    self.set_token(cycle, b, false)?;
                }
                self.clear_pending(cycle, f)?;
                self.resolved[f] = 0;
            }
        }
        Ok(())
    }

    fn finish_calls(
        &mut self,
        cycle: &mut CycleTransaction<'_, '_>,
    ) -> Result<(), StructuredScanError> {
        // 实例拓扑由构造器保证父先子后；逆序结算允许嵌套完成在一个提交点传播。
        for instance in (1..self.instances.len()).rev() {
            let Some(handle) = self.instances[instance].parent_call else {
                continue;
            };
            let c = handle.0 as usize;
            if read(cycle, self.call_offset(c))? == 0 {
                continue;
            }
            let call = self.calls[c];
            let mut running = false;
            for (i, node) in self.nodes.iter().enumerate() {
                if node.instance == call.child_instance
                    && (self.is_active(cycle, i)?
                        || (i > self.scan_position && self.current[i] != 0))
                {
                    running = true;
                }
            }
            for (nested, child) in self.calls.iter().enumerate() {
                if self.nodes[child.node.get() as usize].instance == call.child_instance
                    && read(cycle, self.call_offset(nested))? != 0
                {
                    running = true;
                }
            }
            if !running {
                if self.cancel_at_boundary(cycle, self.nodes[call.node.get() as usize])? {
                    self.cancel_call(cycle, c)?;
                    continue;
                }
                self.copy(cycle, call.output_copies)?;
                write(cycle, self.call_offset(c), 0)?;
                self.take(
                    cycle,
                    call.node.get() as usize,
                    self.nodes[call.node.get() as usize].outgoing.start as usize,
                )?;
            }
        }
        Ok(())
    }

    fn callback<E: StructuredNodeExecutor + ?Sized>(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
        executor: &mut E,
        node: StructuredNodeDefinition,
    ) -> Result<StructuredNodeOutcome, StructuredScanError> {
        let mut context = WorkflowNodeContext {
            cycle,
            state_offset: self.initial.len(),
            state_len: self.app_bytes,
            output_len: self.output_bytes,
        };
        executor
            .execute(node.handle, &mut context)
            .map_err(|reason| {
                if self.instances[node.instance.0 as usize]
                    .parent_call
                    .is_some()
                {
                    StructuredScanError::SubworkflowFault {
                        node: node.handle,
                        reason,
                    }
                } else {
                    StructuredScanError::NodeFault {
                        node: node.handle,
                        reason,
                    }
                }
            })
    }

    fn take(
        &mut self,
        cycle: &mut CycleTransaction<'_, '_>,
        i: usize,
        e: usize,
    ) -> Result<(), StructuredScanError> {
        let node = self.nodes[i];
        let edge = *self
            .edges
            .get(e)
            .ok_or(StructuredScanError::InvalidTransition)?;
        if edge.source != node.handle {
            return Err(StructuredScanError::InvalidTransition);
        }
        if self.cancel_at_boundary(cycle, node)? {
            return Ok(());
        }
        if let Some(offset) = self.layout.backedge_offsets[e] {
            let maximum = edge
                .maximum_traversals_per_run
                .ok_or(StructuredScanError::InternalInvariant)?;
            let old = read_u64(cycle, offset)?;
            if old >= maximum {
                return Err(StructuredScanError::BackedgeTraversalExceeded { edge: edge.handle });
            }
            let next = old
                .checked_add(1)
                .ok_or(StructuredScanError::BackedgeTraversalExceeded { edge: edge.handle })?;
            write_u64(cycle, offset, next)?;
        }
        if let StructuredEdgeTarget::Node(target) = edge.target {
            if let StructuredNodeKind::Fork(f) = self.nodes[target.get() as usize].kind {
                self.ensure_fork_quiescent(cycle, f.0 as usize)?;
            }
            if let Some(branch) = edge.branch
                && matches!(
                    self.nodes[target.get() as usize].kind,
                    StructuredNodeKind::Join { fork: Some(_), .. }
                )
            {
                let b = branch.0 as usize;
                if self.resolved[self.branches[b].fork.0 as usize] != 0 {
                    if self.is_pending(cycle, b)? {
                        return Err(StructuredScanError::CancellationBoundaryExceeded {
                            node: node.handle,
                        });
                    }
                    return Ok(());
                }
                if self.token(cycle, b)? {
                    return Err(StructuredScanError::InvalidControlState);
                }
                self.set_token(cycle, b, true)?;
            }
            self.activate(cycle, target.get() as usize)?;
        } else {
            self.completion_candidate = true;
            for m in &self.memberships {
                if m.node == node.handle && self.is_pending(cycle, m.branch.0 as usize)? {
                    return Err(StructuredScanError::CancellationBoundaryExceeded {
                        node: node.handle,
                    });
                }
            }
        }
        Ok(())
    }

    fn cancel_at_boundary(
        &mut self,
        cycle: &mut CycleTransaction<'_, '_>,
        node: StructuredNodeDefinition,
    ) -> Result<bool, StructuredScanError> {
        if !node.cancellation_boundary {
            return Ok(false);
        }
        let mut canceled = false;
        for index in 0..self.memberships.len() {
            let m = self.memberships[index];
            let b = m.branch.0 as usize;
            if m.node == node.handle && self.is_pending(cycle, b)? {
                let slot = self
                    .pending_slot(cycle, b)?
                    .ok_or(StructuredScanError::InternalInvariant)?;
                set_bit(cycle, self.layout.pending_offset, slot, false)?;
                self.canceled[b] = 1;
                canceled = true;
            }
        }
        Ok(canceled)
    }

    fn activate(
        &mut self,
        cycle: &mut CycleTransaction<'_, '_>,
        i: usize,
    ) -> Result<(), StructuredScanError> {
        if let StructuredNodeKind::Fork(f) = self.nodes[i].kind {
            self.clear_pending(cycle, f.0 as usize)?;
            for b in range(self.forks[f.0 as usize].branches) {
                self.set_token(cycle, b, false)?;
            }
            self.resolved[f.0 as usize] = 0;
        }
        if !bit(cycle, 0, i)? {
            set_bit(cycle, 0, i, true)?;
            if let Some(offset) = self.layout.wait_offsets[i] {
                write_u64(cycle, offset, cycle.identity().release_sequence.get())?;
            }
        }
        Ok(())
    }

    fn ensure_fork_quiescent(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
        f: usize,
    ) -> Result<(), StructuredScanError> {
        // 同一展开区域不能并存两代分支。新 Fork 清 token 前必须确认旧败方已经静止。
        // 当前扫描尚未执行的节点也必须计入；已取消分支的未来状态将在提交前统一清除。
        for membership in &self.memberships {
            let b = membership.branch.0 as usize;
            if self.branches[b].fork.0 as usize != f || self.canceled[b] != 0 {
                continue;
            }
            let i = membership.node.get() as usize;
            if self.is_pending(cycle, b)?
                || self.is_active(cycle, i)?
                || (i > self.scan_position && self.current[i] != 0)
            {
                return Err(StructuredScanError::InvalidControlState);
            }
            if let StructuredNodeKind::Subworkflow(c) = self.nodes[i].kind
                && read(cycle, self.call_offset(c.0 as usize))? != 0
            {
                return Err(StructuredScanError::InvalidControlState);
            }
        }
        Ok(())
    }

    fn elapsed(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
        i: usize,
    ) -> Result<u64, StructuredScanError> {
        cycle
            .identity()
            .release_sequence
            .get()
            .checked_sub(read_u64(
                cycle,
                self.layout.wait_offsets[i].ok_or(StructuredScanError::InternalInvariant)?,
            )?)
            .ok_or(StructuredScanError::InvalidControlState)
    }

    fn copy(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
        r: StructuredBranchRange,
    ) -> Result<(), StructuredScanError> {
        for c in &self.copies[range(r)] {
            let value = read(cycle, self.initial.len() + c.source)?;
            write(cycle, self.initial.len() + c.target, value)?;
        }
        Ok(())
    }
    fn call_offset(&self, c: usize) -> usize {
        self.layout.instance_offset + self.calls[c].child_instance.0 as usize
    }

    fn token(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
        b: usize,
    ) -> Result<bool, StructuredScanError> {
        let branch = self.branches[b];
        bit(
            cycle,
            self.layout.token_offsets[branch.fork.0 as usize],
            branch.branch_order as usize,
        )
    }

    fn set_token(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
        b: usize,
        value: bool,
    ) -> Result<(), StructuredScanError> {
        let branch = self.branches[b];
        set_bit(
            cycle,
            self.layout.token_offsets[branch.fork.0 as usize],
            branch.branch_order as usize,
            value,
        )
    }

    fn is_active(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
        i: usize,
    ) -> Result<bool, StructuredScanError> {
        if let StructuredNodeKind::Fork(f) = self.nodes[i].kind
            && self.resolved[f.0 as usize] != 0
        {
            return Ok(false);
        }
        bit(cycle, 0, i)
    }

    fn pending_slot_for_winner(&self, b: usize, winner: usize) -> Option<usize> {
        let branch = self.branches[b];
        let start = self.layout.pending_starts[branch.fork.0 as usize]?;
        if b == winner {
            return None;
        }
        Some(start + branch.branch_order as usize - usize::from(b > winner))
    }

    fn pending_slot(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
        b: usize,
    ) -> Result<Option<usize>, StructuredScanError> {
        let f = self.branches[b].fork.0 as usize;
        if self.resolved[f] == 0 || self.layout.pending_starts[f].is_none() {
            return Ok(None);
        }
        for winner in range(self.forks[f].branches) {
            if self.token(cycle, winner)? {
                return Ok(self.pending_slot_for_winner(b, winner));
            }
        }
        Err(StructuredScanError::InvalidControlState)
    }

    fn is_pending(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
        b: usize,
    ) -> Result<bool, StructuredScanError> {
        match self.pending_slot(cycle, b)? {
            Some(slot) => bit(cycle, self.layout.pending_offset, slot),
            None => Ok(false),
        }
    }

    fn clear_pending(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
        f: usize,
    ) -> Result<(), StructuredScanError> {
        if let Some(start) = self.layout.pending_starts[f] {
            for slot in start..start + self.forks[f].branches.count as usize - 1 {
                set_bit(cycle, self.layout.pending_offset, slot, false)?;
            }
        }
        Ok(())
    }

    fn validate_state(
        &self,
        cycle: &mut CycleTransaction<'_, '_>,
    ) -> Result<(), StructuredScanError> {
        validate_padding(cycle, 0, self.nodes.len())?;
        validate_padding(cycle, self.layout.pending_offset, self.layout.pending_bits)?;
        let root_state = read(cycle, self.layout.instance_offset)?;
        if !matches!(root_state, 1 | 2) {
            return Err(StructuredScanError::InvalidControlState);
        }
        for instance in 1..self.instances.len() {
            if read(cycle, self.layout.instance_offset + instance)? > 1 {
                return Err(StructuredScanError::InvalidControlState);
            }
        }
        for (i, n) in self.nodes.iter().enumerate() {
            if let Some(offset) = self.layout.wait_offsets[i]
                && read_u64(cycle, offset)? > cycle.identity().release_sequence.get()
            {
                return Err(StructuredScanError::InvalidControlState);
            }
            let is_marker = if let StructuredNodeKind::Fork(f) = n.kind {
                let mut count = 0;
                for b in range(self.forks[f.0 as usize].branches) {
                    count += u32::from(self.token(cycle, b)?);
                }
                count > 0
            } else {
                false
            };
            if bit(cycle, 0, i)?
                && !is_marker
                && (root_state == 2
                    || read(cycle, self.layout.instance_offset + n.instance.0 as usize)? == 0)
            {
                return Err(StructuredScanError::InvalidControlState);
            }
        }
        for (e, offset) in self.layout.backedge_offsets.iter().enumerate() {
            if let Some(offset) = offset
                && read_u64(cycle, *offset)?
                    > self.edges[e]
                        .maximum_traversals_per_run
                        .ok_or(StructuredScanError::InternalInvariant)?
            {
                return Err(StructuredScanError::InvalidControlState);
            }
        }
        for (f, fork) in self.forks.iter().enumerate() {
            validate_padding(
                cycle,
                self.layout.token_offsets[f],
                fork.branches.count as usize,
            )?;
            let mut count = 0;
            for b in range(fork.branches) {
                count += u32::from(self.token(cycle, b)?);
            }
            let resolved = bit(cycle, 0, fork.node.get() as usize)? && count != 0;
            if resolved
                && match self.layout.join_modes[f] {
                    StructuredJoinMode::All => count != fork.branches.count,
                    StructuredJoinMode::Any(_) => count != 1,
                    StructuredJoinMode::Merge => true,
                }
            {
                return Err(StructuredScanError::InvalidControlState);
            }
            if let Some(start) = self.layout.pending_starts[f] {
                for slot in start..start + fork.branches.count as usize - 1 {
                    if bit(cycle, self.layout.pending_offset, slot)? && !resolved {
                        return Err(StructuredScanError::InvalidControlState);
                    }
                }
            }
        }
        Ok(())
    }
}

/// 与 R2-02 证明一致的紧凑持久布局。所有索引表仅在构造期分配。
#[derive(Debug)]
struct ControlLayout {
    bytes: usize,
    instance_offset: usize,
    wait_offsets: Box<[Option<usize>]>,
    backedge_offsets: Box<[Option<usize>]>,
    token_offsets: Box<[usize]>,
    pending_offset: usize,
    pending_bits: usize,
    pending_starts: Box<[Option<usize>]>,
    join_modes: Box<[StructuredJoinMode]>,
}

impl ControlLayout {
    fn new(d: &StructuredWorkflowDefinition<'_>) -> Result<Self, StructuredPlanError> {
        let mut bytes = d.nodes.len().div_ceil(8);
        let instance_offset = add_bytes(&mut bytes, d.instances.len())?;
        let mut wait_offsets = filled(d.nodes.len(), None)?;
        for (i, node) in d.nodes.iter().enumerate() {
            if matches!(
                node.kind,
                StructuredNodeKind::WaitCycles { .. } | StructuredNodeKind::WaitCondition { .. }
            ) {
                wait_offsets[i] = Some(add_bytes(&mut bytes, 8)?);
            }
        }
        let mut backedge_offsets = filled(d.edges.len(), None)?;
        for (i, edge) in d.edges.iter().enumerate() {
            if edge.maximum_traversals_per_run.is_some() {
                backedge_offsets[i] = Some(add_bytes(&mut bytes, 8)?);
            }
        }
        let mut token_offsets = filled(d.forks.len(), 0)?;
        let mut pending_starts = filled(d.forks.len(), None)?;
        let mut join_modes = filled(d.forks.len(), StructuredJoinMode::Merge)?;
        let mut pending_bits = 0_usize;
        for (f, fork) in d.forks.iter().enumerate() {
            token_offsets[f] = add_bytes(&mut bytes, (fork.branches.count as usize).div_ceil(8))?;
            for node in d.nodes {
                if let StructuredNodeKind::Join {
                    fork: Some(handle),
                    mode,
                } = node.kind
                    && handle == fork.handle
                {
                    join_modes[f] = mode;
                }
            }
            if join_modes[f] == StructuredJoinMode::Any(StructuredJoinPolicy::WaitAtBoundary) {
                pending_starts[f] = Some(pending_bits);
                pending_bits = pending_bits
                    .checked_add(fork.branches.count as usize - 1)
                    .ok_or(StructuredPlanError::StateSizeOverflow)?;
            }
        }
        let pending_offset = add_bytes(&mut bytes, pending_bits.div_ceil(8))?;
        Ok(Self {
            bytes,
            instance_offset,
            wait_offsets,
            backedge_offsets,
            token_offsets,
            pending_offset,
            pending_bits,
            pending_starts,
            join_modes,
        })
    }
}

fn add_bytes(bytes: &mut usize, count: usize) -> Result<usize, StructuredPlanError> {
    let offset = *bytes;
    *bytes = bytes
        .checked_add(count)
        .ok_or(StructuredPlanError::StateSizeOverflow)?;
    Ok(offset)
}

fn filled<T: Clone>(n: usize, value: T) -> Result<Box<[T]>, StructuredPlanError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(n)
        .map_err(|_| StructuredPlanError::AllocationFailed)?;
    result.resize(n, value);
    Ok(result.into_boxed_slice())
}

fn bit(
    cycle: &mut CycleTransaction<'_, '_>,
    offset: usize,
    i: usize,
) -> Result<bool, StructuredScanError> {
    Ok(read(cycle, offset + i / 8)? & (1 << (i % 8)) != 0)
}

fn set_bit(
    cycle: &mut CycleTransaction<'_, '_>,
    offset: usize,
    i: usize,
    value: bool,
) -> Result<(), StructuredScanError> {
    let old = read(cycle, offset + i / 8)?;
    let mask = 1 << (i % 8);
    write(
        cycle,
        offset + i / 8,
        if value { old | mask } else { old & !mask },
    )
}

fn validate_padding(
    cycle: &mut CycleTransaction<'_, '_>,
    offset: usize,
    bits: usize,
) -> Result<(), StructuredScanError> {
    if !bits.is_multiple_of(8) && read(cycle, offset + bits / 8)? >> (bits % 8) != 0 {
        return Err(StructuredScanError::InvalidControlState);
    }
    Ok(())
}

fn range(r: StructuredBranchRange) -> std::ops::Range<usize> {
    r.start as usize..r.start as usize + r.count as usize
}
fn read(c: &mut CycleTransaction<'_, '_>, i: usize) -> Result<u8, StructuredScanError> {
    c.read_state(WorkSetIndex::new(i))
        .map_err(StructuredScanError::Transaction)
}
fn write(c: &mut CycleTransaction<'_, '_>, i: usize, v: u8) -> Result<(), StructuredScanError> {
    c.write_state(WorkSetIndex::new(i), v)
        .map_err(StructuredScanError::Transaction)
}
fn read_u64(c: &mut CycleTransaction<'_, '_>, i: usize) -> Result<u64, StructuredScanError> {
    let mut bytes = [0; 8];
    for (j, b) in bytes.iter_mut().enumerate() {
        *b = read(c, i + j)?;
    }
    Ok(u64::from_le_bytes(bytes))
}
fn write_u64(
    c: &mut CycleTransaction<'_, '_>,
    i: usize,
    v: u64,
) -> Result<(), StructuredScanError> {
    for (j, b) in v.to_le_bytes().iter().enumerate() {
        write(c, i + j, *b)?;
    }
    Ok(())
}
fn zeroed(n: usize) -> Result<Box<[u8]>, StructuredPlanError> {
    crate::allocate_zeroed(n).map_err(|_| StructuredPlanError::AllocationFailed)
}
fn copied<T: Copy>(s: &[T]) -> Result<Box<[T]>, StructuredPlanError> {
    crate::clone_boxed_slice(s).map_err(|_| StructuredPlanError::AllocationFailed)
}
fn reason(e: StructuredScanError) -> FaultReason {
    match e {
        StructuredScanError::NodeFault { reason, .. }
        | StructuredScanError::SubworkflowFault { reason, .. } => reason,
        StructuredScanError::ActiveCapacityExceeded
        | StructuredScanError::ExecutionCapacityExceeded
        | StructuredScanError::BackedgeTraversalExceeded { .. }
        | StructuredScanError::PendingCancellationExceeded
        | StructuredScanError::ImageLayoutMismatch => FaultReason::CapacityExceeded,
        _ => FaultReason::TaskExecutionFault,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "完整静态表在构造边界集中审计，周期路径不调用"
)]
fn validate(d: &StructuredWorkflowDefinition<'_>) -> Result<(), StructuredPlanError> {
    for n in [
        d.nodes.len(),
        d.edges.len(),
        d.forks.len(),
        d.branches.len(),
        d.instances.len(),
        d.calls.len(),
        d.memberships.len(),
        d.call_initial_nodes.len(),
        d.state_copies.len(),
    ] {
        if n >= u32::MAX as usize {
            return Err(StructuredPlanError::InvalidCapacity);
        }
    }
    if d.maximum_active_nodes == 0
        || d.maximum_node_executions == 0
        || d.maximum_pending_cancellations == 0
        || (!d.nodes.is_empty() && d.initial_active.is_empty())
        || d.initial_active.len() > d.maximum_active_nodes as usize
        || d.initial_active.len() > d.maximum_node_executions as usize
    {
        return Err(StructuredPlanError::InvalidCapacity);
    }
    if d.instances.is_empty() || d.instances[0].parent_call.is_some() {
        return Err(StructuredPlanError::InvalidSubworkflow);
    }
    let mut edge_start = 0;
    for (i, n) in d.nodes.iter().enumerate() {
        if n.handle.get() as usize != i {
            return Err(StructuredPlanError::ReservedOrNonDense);
        }
        if n.instance.0 as usize >= d.instances.len() {
            return Err(StructuredPlanError::InvalidReference);
        }
        if n.outgoing.start as usize != edge_start {
            return Err(StructuredPlanError::InvalidRange);
        }
        edge_start = edge_start
            .checked_add(n.outgoing.count as usize)
            .ok_or(StructuredPlanError::InvalidRange)?;
        if edge_start > d.edges.len() {
            return Err(StructuredPlanError::InvalidRange);
        }
        for e in &d.edges[n.outgoing.start as usize..edge_start] {
            if e.source != n.handle {
                return Err(StructuredPlanError::InvalidReference);
            }
        }
        match n.kind {
            StructuredNodeKind::Decision => {
                if n.outgoing.count == 0 {
                    return Err(StructuredPlanError::InvalidNodeShape);
                }
            }
            StructuredNodeKind::Fork(f) => {
                if n.outgoing.count < 2
                    || d.forks.get(f.0 as usize).is_none_or(|f| f.node != n.handle)
                {
                    return Err(StructuredPlanError::InvalidNodeShape);
                }
            }
            StructuredNodeKind::Join { fork, mode } => {
                if n.outgoing.count != 1
                    || (mode == StructuredJoinMode::Merge) != fork.is_none()
                    || fork.is_some_and(|f| f.0 as usize >= d.forks.len())
                {
                    return Err(StructuredPlanError::InvalidNodeShape);
                }
            }
            StructuredNodeKind::WaitCycles { wait_cycles: 0 }
            | StructuredNodeKind::WaitCondition {
                timeout_cycles: Some(0),
            } => return Err(StructuredPlanError::InvalidWait),
            StructuredNodeKind::Subworkflow(c) => {
                if n.outgoing.count != 1
                    || d.calls.get(c.0 as usize).is_none_or(|c| c.node != n.handle)
                {
                    return Err(StructuredPlanError::InvalidSubworkflow);
                }
            }
            _ => {
                if n.outgoing.count != 1 {
                    return Err(StructuredPlanError::InvalidNodeShape);
                }
            }
        }
    }
    if edge_start != d.edges.len() {
        return Err(StructuredPlanError::MissingOrExtraEntry);
    }
    for (i, e) in d.edges.iter().enumerate() {
        if e.handle.get() as usize != i {
            return Err(StructuredPlanError::ReservedOrNonDense);
        }
        if e.source.get() as usize >= d.nodes.len()
            || e.branch.is_some_and(|b| b.0 as usize >= d.branches.len())
        {
            return Err(StructuredPlanError::InvalidReference);
        }
        if e.maximum_traversals_per_run == Some(0)
            || (e.maximum_traversals_per_run.is_some()
                && matches!(e.target, StructuredEdgeTarget::Complete))
        {
            return Err(StructuredPlanError::InvalidBackedge);
        }
        if e.branch.is_some() && matches!(e.target, StructuredEdgeTarget::Complete) {
            return Err(StructuredPlanError::InvalidReference);
        }
        if let StructuredEdgeTarget::Node(t) = e.target {
            if e.maximum_traversals_per_run.is_none() && t.get() <= e.source.get() {
                return Err(StructuredPlanError::InvalidBackedge);
            }
            let target = d
                .nodes
                .get(t.get() as usize)
                .ok_or(StructuredPlanError::InvalidReference)?;
            if target.instance != d.nodes[e.source.get() as usize].instance {
                return Err(StructuredPlanError::InvalidSubworkflow);
            }
            if let StructuredNodeKind::Join { fork: Some(f), .. } = target.kind {
                let b = e.branch.ok_or(StructuredPlanError::MissingOrExtraEntry)?;
                if d.branches[b.0 as usize].fork != f
                    || !d.memberships.contains(&StructuredBranchMembership {
                        node: e.source,
                        branch: b,
                    })
                {
                    return Err(StructuredPlanError::InvalidReference);
                }
            } else if e.branch.is_some()
                && !matches!(
                    d.nodes[e.source.get() as usize].kind,
                    StructuredNodeKind::Fork(_)
                )
            {
                return Err(StructuredPlanError::InvalidReference);
            }
        }
    }
    for (i, n) in d.initial_active.iter().enumerate() {
        if n.get() as usize >= d.nodes.len() || d.nodes[n.get() as usize].instance.0 != 0 {
            return Err(StructuredPlanError::InvalidReference);
        }
        if d.initial_active[..i].contains(n) {
            return Err(StructuredPlanError::DuplicateEntry);
        }
    }
    let mut branch_start = 0;
    for (i, f) in d.forks.iter().enumerate() {
        if f.handle.0 as usize != i
            || d.nodes
                .get(f.node.get() as usize)
                .is_none_or(|n| n.kind != StructuredNodeKind::Fork(f.handle))
        {
            return Err(StructuredPlanError::MissingOrExtraEntry);
        }
        if f.branches.start as usize != branch_start || f.branches.count < 2 {
            return Err(StructuredPlanError::InvalidRange);
        }
        branch_start = branch_start
            .checked_add(f.branches.count as usize)
            .ok_or(StructuredPlanError::InvalidRange)?;
        if branch_start > d.branches.len()
            || d.nodes[f.node.get() as usize].outgoing.count != f.branches.count
        {
            return Err(StructuredPlanError::InvalidRange);
        }
        if d.nodes.iter().filter(|n| matches!(n.kind, StructuredNodeKind::Join { fork: Some(h), .. } if h == f.handle)).count() != 1 { return Err(StructuredPlanError::MissingOrExtraEntry); }
        for (order, b) in d.branches[range(f.branches)].iter().enumerate() {
            if b.fork != f.handle
                || b.branch_order as usize != order
                || b.handle.0 as usize != f.branches.start as usize + order
            {
                return Err(StructuredPlanError::InvalidBranchOrder);
            }
            let e = d
                .edges
                .get(b.activation_edge.get() as usize)
                .ok_or(StructuredPlanError::InvalidReference)?;
            if e.source != f.node || e.branch != Some(b.handle) {
                return Err(StructuredPlanError::InvalidReference);
            }
            if let StructuredEdgeTarget::Node(target) = e.target {
                if !d.memberships.contains(&StructuredBranchMembership {
                    node: target,
                    branch: b.handle,
                }) {
                    return Err(StructuredPlanError::MissingOrExtraEntry);
                }
            } else {
                return Err(StructuredPlanError::InvalidReference);
            }
        }
    }
    if branch_start != d.branches.len() {
        return Err(StructuredPlanError::MissingOrExtraEntry);
    }
    for (i, m) in d.memberships.iter().enumerate() {
        if m.node.get() as usize >= d.nodes.len() || m.branch.0 as usize >= d.branches.len() {
            return Err(StructuredPlanError::InvalidReference);
        }
        if d.memberships[..i].contains(m) {
            return Err(StructuredPlanError::DuplicateEntry);
        }
    }
    for (i, instance) in d.instances.iter().enumerate() {
        if instance.handle.0 as usize != i {
            return Err(StructuredPlanError::ReservedOrNonDense);
        }
        if i != 0
            && instance
                .parent_call
                .and_then(|c| d.calls.get(c.0 as usize))
                .is_none_or(|c| c.child_instance != instance.handle)
        {
            return Err(StructuredPlanError::InvalidSubworkflow);
        }
    }
    let mut initial_start = 0;
    let mut copy_start = 0;
    for (i, c) in d.calls.iter().enumerate() {
        if c.handle.0 as usize != i
            || d.nodes
                .get(c.node.get() as usize)
                .is_none_or(|n| n.kind != StructuredNodeKind::Subworkflow(c.handle))
            || c.child_instance.0 as usize >= d.instances.len()
            || d.instances[c.child_instance.0 as usize].parent_call != Some(c.handle)
            || d.nodes[c.node.get() as usize].instance.0 >= c.child_instance.0
        {
            return Err(StructuredPlanError::InvalidSubworkflow);
        }
        if c.initial_nodes.start as usize != initial_start {
            return Err(StructuredPlanError::InvalidRange);
        }
        if c.initial_nodes.count == 0 && d.nodes.iter().any(|n| n.instance == c.child_instance) {
            return Err(StructuredPlanError::MissingOrExtraEntry);
        }
        initial_start = initial_start
            .checked_add(c.initial_nodes.count as usize)
            .ok_or(StructuredPlanError::InvalidRange)?;
        if initial_start > d.call_initial_nodes.len() {
            return Err(StructuredPlanError::InvalidRange);
        }
        for (j, n) in d.call_initial_nodes[range(c.initial_nodes)]
            .iter()
            .enumerate()
        {
            if d.nodes
                .get(n.get() as usize)
                .is_none_or(|n| n.instance != c.child_instance)
                || d.call_initial_nodes[range(c.initial_nodes)]
                    .iter()
                    .take(j)
                    .any(|p| p == n)
            {
                return Err(StructuredPlanError::InvalidSubworkflow);
            }
        }
        for r in [c.input_copies, c.output_copies] {
            if r.start as usize != copy_start {
                return Err(StructuredPlanError::InvalidRange);
            }
            copy_start = copy_start
                .checked_add(r.count as usize)
                .ok_or(StructuredPlanError::InvalidRange)?;
            if copy_start > d.state_copies.len() {
                return Err(StructuredPlanError::InvalidRange);
            }
        }
    }
    if initial_start != d.call_initial_nodes.len() || copy_start != d.state_copies.len() {
        return Err(StructuredPlanError::MissingOrExtraEntry);
    }
    for c in d.state_copies {
        if c.source >= d.application_state_bytes || c.target >= d.application_state_bytes {
            return Err(StructuredPlanError::InvalidReference);
        }
    }
    Ok(())
}
