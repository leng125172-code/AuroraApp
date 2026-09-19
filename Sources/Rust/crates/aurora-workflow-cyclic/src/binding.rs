//! R2-05 固定容量 Action/condition 分派层。
//!
//! 构造期验证绑定表对所有会触发回调的结构化节点形成精确闭包。周期期仅按稠密句柄、连续
//! 区间和固定 staging 偏移分派；后端不能选择控制边，也无法获得物理 I/O、网络或 Hosted
//! Workflow 入口。

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::marker::PhantomData;

use aurora_control_contracts::FaultReason;
use aurora_control_engine::WorkSetIndex;
use aurora_types::LocalHandle;

use crate::{
    StructuredBranchDefinition, StructuredBranchMembership, StructuredCallHandle,
    StructuredEdgeDefinition, StructuredForkDefinition, StructuredInstanceDefinition,
    StructuredNodeDefinition, StructuredNodeExecutionError, StructuredNodeExecutor,
    StructuredNodeKind, StructuredNodeOutcome, StructuredOutputTrace, StructuredPlanError,
    StructuredStateCopy, StructuredSubworkflowDefinition, StructuredWorkflowDefinition,
    StructuredWorkflowRuntime, WorkflowEdgeHandle, WorkflowNodeContext, WorkflowNodeHandle,
};

/// 固定表连续区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindingRange {
    /// 第一项索引。
    pub start: u32,
    /// 项数。
    pub count: u32,
}

/// 一个 task image 内的固定字节区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeByteRange {
    /// 区间首字节。
    pub start: usize,
    /// 区间长度，可为零。
    pub length: usize,
}

/// Action 绑定稠密句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuntimeActionHandle(pub u32);

/// condition 绑定稠密句柄。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct RuntimeConditionHandle(pub u32);

/// Runtime binding reader/writer 版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeBindingVersion {
    /// 不兼容版本。
    pub major: u16,
    /// 向后兼容增量版本。
    pub minor: u16,
}

/// 与 host `plan_digest` 一一对应的固定计划身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeBindingPlanIdentity(pub [u8; 32]);

impl RuntimeBindingVersion {
    /// R2-05 Preview 1.0。
    pub const V1_0: Self = Self { major: 1, minor: 0 };
}

/// staging 存储区域。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeValueArea {
    /// application state staging bank。
    State,
    /// task output staging bank；不是物理 I/O。
    Output,
}

/// 固定宽度标量类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeValueType {
    /// 一字节 BOOL。
    Bool,
    /// 有符号 8 位整数。
    Sint,
    /// 有符号 16 位整数。
    Int,
    /// 有符号 32 位整数。
    Dint,
    /// 有符号 64 位整数。
    Lint,
    /// 无符号 8 位整数。
    Usint,
    /// 无符号 16 位整数。
    Uint,
    /// 无符号 32 位整数。
    Udint,
    /// 无符号 64 位整数。
    Ulint,
    /// IEEE-754 binary32。
    Real,
    /// IEEE-754 binary64。
    Lreal,
}

impl RuntimeValueType {
    const fn size(self) -> usize {
        match self {
            Self::Bool | Self::Sint | Self::Usint => 1,
            Self::Int | Self::Uint => 2,
            Self::Dint | Self::Udint | Self::Real => 4,
            Self::Lint | Self::Ulint | Self::Lreal => 8,
        }
    }
}

/// 一个固定 staging slot。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeValueSlot {
    /// 存储区域。
    pub area: RuntimeValueArea,
    /// 区域内字节偏移。
    pub offset_bytes: usize,
    /// 精确类型和宽度。
    pub value_type: RuntimeValueType,
}

/// Action 端口方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePortDirection {
    /// 只读。
    Input,
    /// 只写。
    Output,
    /// 读写。
    InOut,
}

/// 一个 writable Action port 的固定 Output Trace identity。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeOutputTraceDescriptor {
    /// plan-global、由 host `trace_values` catalog 分配的稠密 `ValueHandle`。
    pub value_handle: u32,
    /// 静态 canonical storage type handle。
    pub type_handle: u32,
}

/// 一个按声明次序排列的固定端口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeActionPort {
    /// 从零连续的端口索引。
    pub port: u32,
    /// 访问方向。
    pub direction: RuntimePortDirection,
    /// staging slot。
    pub slot: RuntimeValueSlot,
    /// Input 必须为 None；Output/InOut 必须恰有一个固定 Trace descriptor。
    pub output_trace: Option<RuntimeOutputTraceDescriptor>,
}

/// 周期安全 Action 类别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeActionKind {
    /// 静态链接 R1 ST POU 包装器。
    StPou,
    /// 只操作锁存/暂存 I/O image 的预验证动作。
    IoImage,
    /// 写入 task output staging 的固定类型命令。
    TypedCommand,
}

/// 一个稠密 Action 定义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeActionDefinition {
    /// 必须等于表索引。
    pub handle: RuntimeActionHandle,
    /// 必须为 Runtime 支持的精确版本。
    pub version: RuntimeBindingVersion,
    /// Action 类别。
    pub kind: RuntimeActionKind,
    /// 构建期解析的目标句柄；`u32::MAX` 保留。
    pub target_handle: u32,
    /// 此展开调用点独占的 transaction state 区间；不得由 backend 私有状态替代。
    pub invocation_state: RuntimeByteRange,
    /// 此 Action 独占的连续端口区间。
    pub ports: BindingRange,
}

/// 一个稠密 BOOL condition 定义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeConditionDefinition {
    /// 必须等于表索引。
    pub handle: RuntimeConditionHandle,
    /// BOOL staging slot。
    pub source: RuntimeValueSlot,
}

/// Decision 的一条静态 priority guard。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeGuardDefinition {
    /// 必须属于对应 Decision，且按 edge 区间顺序排列。
    pub edge: WorkflowEdgeHandle,
    /// BOOL condition。
    pub condition: RuntimeConditionHandle,
}

/// 一个回调节点的固定绑定类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeNodeBindingKind {
    /// Action 后端与唯一成功边。
    Action {
        /// 稠密 Action。
        action: RuntimeActionHandle,
        /// 可选成功 guard；false 时保留当前节点。
        guard: Option<RuntimeConditionHandle>,
        /// 后端成功且 guard 为真时采用的静态边。
        success_edge: WorkflowEdgeHandle,
    },
    /// 按 priority 顺序求值的完整 guard 区间。
    Decision {
        /// 此 Decision 独占的连续 guard 区间。
        guards: BindingRange,
    },
    /// condition Wait 的唯一 BOOL 来源。
    WaitCondition {
        /// condition 句柄。
        condition: RuntimeConditionHandle,
    },
}

/// 一个回调节点的唯一绑定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeNodeBindingDefinition {
    /// 结构化计划节点。
    pub node: WorkflowNodeHandle,
    /// 与节点种类严格一致的绑定。
    pub kind: RuntimeNodeBindingKind,
}

/// 构造期边界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeBindingLimits {
    /// Action 总数上限。
    pub maximum_actions: u32,
    /// condition 总数上限。
    pub maximum_conditions: u32,
    /// 单 Action 端口上限。
    pub maximum_ports_per_action: u32,
    /// 单 Decision guard 上限。
    pub maximum_guards_per_decision: u32,
}

/// 由签名 Static Workflow Plan resource proof 唯一导出的周期 Runtime 容量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeCyclicCapacities {
    task_handle: LocalHandle,
    maximum_active_nodes: u32,
    maximum_node_executions: u32,
    maximum_pending_cancellations: u32,
}

impl RuntimeCyclicCapacities {
    /// 构造不可替换的 task identity 与周期容量组合。
    ///
    /// # Errors
    /// task handle 无效时拒绝；零容量是否匹配空结构表由 owned plan 构造边界审计。
    pub fn new(
        task_handle: u32,
        maximum_active_nodes: u32,
        maximum_node_executions: u32,
        maximum_pending_cancellations: u32,
    ) -> Result<Self, RuntimeBindingPlanError> {
        Ok(Self {
            task_handle: LocalHandle::new(task_handle)
                .map_err(|_| RuntimeBindingPlanError::InvalidReference)?,
            maximum_active_nodes,
            maximum_node_executions,
            maximum_pending_cancellations,
        })
    }
}

/// 固定绑定表拒绝原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeBindingPlanError {
    /// 容量为零或表长超过容量。
    InvalidCapacity,
    /// 回调节点绑定缺失、重复或额外。
    MissingOrExtraNodeBinding,
    /// Action/condition 句柄不稠密或保留。
    NonDenseHandle,
    /// 连续区间有缺口、重叠、越界或端口序号不连续。
    InvalidRange,
    /// 引用不存在、节点类别不匹配或 edge 所有权错误。
    InvalidReference,
    /// slot 越界、condition 非 BOOL 或 Action 端口违反类别边界。
    InvalidSlot,
    /// 初始化分配失败。
    AllocationFailed,
    /// 装载的 binding plan 不属于调用方要求的 Static Workflow Plan。
    PlanIdentityMismatch,
}

impl Display for RuntimeBindingPlanError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "Workflow binding plan error: {self:?}")
    }
}

impl Error for RuntimeBindingPlanError {}

/// Action 后端可见的固定端口 staging 视图。
pub struct RuntimeBindingContext<'a, 'cycle, 'task, 'plan> {
    context: &'a mut WorkflowNodeContext<'cycle, 'task, 'plan>,
    ports: &'a [RuntimeActionPort],
    invocation_state: RuntimeByteRange,
}

impl RuntimeBindingContext<'_, '_, '_, '_> {
    /// 返回固定端口数。
    #[must_use]
    pub const fn port_count(&self) -> usize {
        self.ports.len()
    }

    /// 读取此调用点独占的 transaction state 字节。
    ///
    /// # Errors
    /// 偏移超出构建期固定区间时返回 `CapacityExceeded`。
    pub fn read_invocation_state(&mut self, offset: usize) -> Result<u8, FaultReason> {
        let index = self.invocation_state_index(offset)?;
        self.context
            .read_state(WorkSetIndex::new(index))
            .map_err(|_| FaultReason::CapacityExceeded)
    }

    /// 写入此调用点独占的 transaction state 字节。
    ///
    /// # Errors
    /// 偏移超出构建期固定区间时返回 `CapacityExceeded`。
    pub fn write_invocation_state(&mut self, offset: usize, value: u8) -> Result<(), FaultReason> {
        let index = self.invocation_state_index(offset)?;
        self.context
            .write_state(WorkSetIndex::new(index), value)
            .map_err(|_| FaultReason::CapacityExceeded)
    }

    fn invocation_state_index(&self, offset: usize) -> Result<usize, FaultReason> {
        (offset < self.invocation_state.length)
            .then(|| self.invocation_state.start.checked_add(offset))
            .flatten()
            .ok_or(FaultReason::CapacityExceeded)
    }

    /// 按端口和端口内偏移读取 staging 字节。
    ///
    /// # Errors
    /// 端口不存在、方向不允许或访问越界时返回 `CapacityExceeded`。
    pub fn read(&mut self, port: u32, offset: usize) -> Result<u8, FaultReason> {
        let binding = self.port(port, offset, true)?;
        self.read_slot(binding.slot, offset)
    }

    /// 按端口和端口内偏移写入 staging 字节。
    ///
    /// # Errors
    /// 端口不存在、方向不允许或访问越界时返回 `CapacityExceeded`。
    pub fn write(&mut self, port: u32, offset: usize, value: u8) -> Result<(), FaultReason> {
        let binding = self.port(port, offset, false)?;
        self.write_slot(binding.slot, offset, value)
    }

    fn port(&self, port: u32, offset: usize, read: bool) -> Result<RuntimeActionPort, FaultReason> {
        let index = usize::try_from(port).map_err(|_| FaultReason::CapacityExceeded)?;
        let binding = self
            .ports
            .get(index)
            .copied()
            .filter(|binding| binding.port == port && offset < binding.slot.value_type.size())
            .ok_or(FaultReason::CapacityExceeded)?;
        let allowed = if read {
            binding.direction != RuntimePortDirection::Output
        } else {
            binding.direction != RuntimePortDirection::Input
        };
        allowed
            .then_some(binding)
            .ok_or(FaultReason::CapacityExceeded)
    }

    fn read_slot(&mut self, slot: RuntimeValueSlot, offset: usize) -> Result<u8, FaultReason> {
        let index = slot
            .offset_bytes
            .checked_add(offset)
            .ok_or(FaultReason::CapacityExceeded)?;
        match slot.area {
            RuntimeValueArea::State => self.context.read_state(WorkSetIndex::new(index)),
            RuntimeValueArea::Output => self.context.read_output(WorkSetIndex::new(index)),
        }
        .map_err(|_| FaultReason::CapacityExceeded)
    }

    fn write_slot(
        &mut self,
        slot: RuntimeValueSlot,
        offset: usize,
        value: u8,
    ) -> Result<(), FaultReason> {
        let index = slot
            .offset_bytes
            .checked_add(offset)
            .ok_or(FaultReason::CapacityExceeded)?;
        match slot.area {
            RuntimeValueArea::State => self.context.write_state(WorkSetIndex::new(index), value),
            RuntimeValueArea::Output => self.context.write_output(WorkSetIndex::new(index), value),
        }
        .map_err(|_| FaultReason::CapacityExceeded)
    }
}

/// 周期安全 Action 后端。接口只接收固定 staging 端口，不能选择 Workflow 控制边。
pub trait RuntimeActionBackend {
    /// 调用静态链接的 R1 ST POU wrapper。
    ///
    /// # Errors
    /// 返回明确 `FaultReason` 会使整个 R0 task transaction 失去提交资格。
    fn invoke_st_pou(
        invocation: RuntimeActionHandle,
        target_handle: u32,
        context: &mut RuntimeBindingContext<'_, '_, '_, '_>,
    ) -> Result<(), FaultReason>;

    /// 操作锁存/暂存 I/O image；不得执行物理 I/O。
    ///
    /// # Errors
    /// 返回明确 `FaultReason` 会使整个 R0 task transaction 失去提交资格。
    fn invoke_io_image(
        invocation: RuntimeActionHandle,
        target_handle: u32,
        context: &mut RuntimeBindingContext<'_, '_, '_, '_>,
    ) -> Result<(), FaultReason>;

    /// 将固定类型命令写入 task output staging；不得发送设备或网络请求。
    ///
    /// # Errors
    /// 返回明确 `FaultReason` 会使整个 R0 task transaction 失去提交资格。
    fn stage_typed_command(
        invocation: RuntimeActionHandle,
        target_handle: u32,
        context: &mut RuntimeBindingContext<'_, '_, '_, '_>,
    ) -> Result<(), FaultReason>;
}

/// 已验证、固定容量的 R2-05 分派器。
pub struct RuntimeBindingExecutor<B> {
    backend: PhantomData<fn() -> B>,
    lookup: Box<[Option<RuntimeNodeBindingKind>]>,
    actions: Box<[RuntimeActionDefinition]>,
    ports: Box<[RuntimeActionPort]>,
    conditions: Box<[RuntimeConditionDefinition]>,
    guards: Box<[RuntimeGuardDefinition]>,
    /// 每个全局 port 一个最大标量宽度的调用前 snapshot；周期期不分配。
    trace_before: Box<[[u8; 8]]>,
}

/// 已完成 exact-closure 验证、不可拆分重排的 runtime binding plan。
pub struct RuntimeBindingPlan {
    identity: RuntimeBindingPlanIdentity,
    cyclic_capacities: RuntimeCyclicCapacities,
    application_state_bytes: usize,
    output_bytes: usize,
    nodes: Box<[StructuredNodeDefinition]>,
    edges: Box<[StructuredEdgeDefinition]>,
    initial_active: Box<[WorkflowNodeHandle]>,
    call_initial_ranges: Box<[BindingRange]>,
    call_initial_nodes: Box<[WorkflowNodeHandle]>,
    calls: Box<[StructuredSubworkflowDefinition]>,
    state_copies: Box<[StructuredStateCopy]>,
    lookup: Box<[Option<RuntimeNodeBindingKind>]>,
    actions: Box<[RuntimeActionDefinition]>,
    ports: Box<[RuntimeActionPort]>,
    conditions: Box<[RuntimeConditionDefinition]>,
    guards: Box<[RuntimeGuardDefinition]>,
}

impl RuntimeBindingPlan {
    /// 验证并一次性拥有一整份 plan；成功后表不能再被调用方交换、增删或拆分。
    ///
    /// # Errors
    /// 任一容量、引用、范围或完整闭包不变量失败时原子拒绝。
    #[allow(clippy::too_many_arguments)]
    #[doc(hidden)]
    pub fn from_generated_tables(
        identity: RuntimeBindingPlanIdentity,
        cyclic_capacities: RuntimeCyclicCapacities,
        nodes: &[StructuredNodeDefinition],
        edges: &[StructuredEdgeDefinition],
        initial_active: &[WorkflowNodeHandle],
        call_initial_ranges: &[BindingRange],
        call_initial_nodes: &[WorkflowNodeHandle],
        calls: &[StructuredSubworkflowDefinition],
        state_copies: &[StructuredStateCopy],
        node_bindings: &[RuntimeNodeBindingDefinition],
        actions: &[RuntimeActionDefinition],
        ports: &[RuntimeActionPort],
        conditions: &[RuntimeConditionDefinition],
        guards: &[RuntimeGuardDefinition],
        application_state_bytes: usize,
        output_bytes: usize,
        limits: RuntimeBindingLimits,
    ) -> Result<Self, RuntimeBindingPlanError> {
        if !nodes.is_empty()
            && (cyclic_capacities.maximum_active_nodes == 0
                || cyclic_capacities.maximum_node_executions == 0)
        {
            return Err(RuntimeBindingPlanError::InvalidCapacity);
        }
        validate_capacities(actions, conditions, limits)?;
        validate_actions(
            actions,
            ports,
            application_state_bytes,
            output_bytes,
            limits,
        )?;
        validate_conditions(conditions, application_state_bytes, output_bytes)?;
        validate_invocation_state_aliases(actions, ports, conditions)?;
        validate_initial_activation(
            nodes,
            initial_active,
            call_initial_ranges,
            call_initial_nodes,
        )?;
        validate_subworkflow_tables(
            nodes,
            call_initial_ranges,
            calls,
            state_copies,
            application_state_bytes,
        )?;
        let lookup = validate_node_closure(
            nodes,
            edges,
            node_bindings,
            actions,
            conditions,
            guards,
            limits,
        )?;
        Ok(Self {
            identity,
            cyclic_capacities,
            application_state_bytes,
            output_bytes,
            nodes: copy_box(nodes)?,
            edges: copy_box(edges)?,
            initial_active: copy_box(initial_active)?,
            call_initial_ranges: copy_box(call_initial_ranges)?,
            call_initial_nodes: copy_box(call_initial_nodes)?,
            calls: copy_box(calls)?,
            state_copies: copy_box(state_copies)?,
            lookup,
            actions: copy_box(actions)?,
            ports: copy_box(ports)?,
            conditions: copy_box(conditions)?,
            guards: copy_box(guards)?,
        })
    }

    /// 返回 host plan identity。
    #[must_use]
    pub const fn identity(&self) -> RuntimeBindingPlanIdentity {
        self.identity
    }

    /// 返回由 host 审计并由本 plan 独占的完整结构节点表。
    #[must_use]
    pub fn structured_nodes(&self) -> &[StructuredNodeDefinition] {
        &self.nodes
    }

    /// 返回由 host 审计并由本 plan 独占的完整结构边表。
    #[must_use]
    pub fn structured_edges(&self) -> &[StructuredEdgeDefinition] {
        &self.edges
    }

    /// 返回由 host 从签名计划 Entry 目标导出的 task-local 初始活动节点。
    #[must_use]
    pub fn initial_active(&self) -> &[WorkflowNodeHandle] {
        &self.initial_active
    }

    /// 返回由签名计划中 child Entry 目标导出的指定调用初始节点。
    #[must_use]
    pub fn call_initial_nodes(&self, call: StructuredCallHandle) -> Option<&[WorkflowNodeHandle]> {
        let range = self
            .call_initial_ranges
            .get(usize::try_from(call.0).ok()?)?;
        let start = usize::try_from(range.start).ok()?;
        let end = usize::try_from(range.start.checked_add(range.count)?).ok()?;
        self.call_initial_nodes.get(start..end)
    }

    /// 返回由签名计划生成并由本 plan 独占的完整子工作流调用表。
    #[must_use]
    pub fn subworkflow_calls(&self) -> &[StructuredSubworkflowDefinition] {
        &self.calls
    }

    /// 返回由签名计划生成并由本 plan 独占的有序状态复制表。
    #[must_use]
    pub fn state_copies(&self) -> &[StructuredStateCopy] {
        &self.state_copies
    }

    /// 使用 plan 内绑定的 task identity、结构表、image 尺寸和签名资源容量构造 Runtime。
    ///
    /// 调用方只提供 R2-04 已降低并由 `StructuredWorkflowRuntime` 完整审计的 Fork、branch、
    /// membership 与 instance 表，不能替换资源证明中的任何容量。
    ///
    /// # Errors
    /// 结构表闭包、容量或状态布局不合法时原子拒绝，不返回部分 Runtime。
    pub fn build_structured_runtime(
        &self,
        forks: &[StructuredForkDefinition],
        branches: &[StructuredBranchDefinition],
        memberships: &[StructuredBranchMembership],
        instances: &[StructuredInstanceDefinition],
    ) -> Result<StructuredWorkflowRuntime, StructuredPlanError> {
        StructuredWorkflowRuntime::new(StructuredWorkflowDefinition {
            task_handle: self.cyclic_capacities.task_handle,
            nodes: &self.nodes,
            edges: &self.edges,
            initial_active: &self.initial_active,
            forks,
            branches,
            memberships,
            instances,
            calls: &self.calls,
            call_initial_nodes: &self.call_initial_nodes,
            state_copies: &self.state_copies,
            maximum_active_nodes: self.cyclic_capacities.maximum_active_nodes,
            maximum_node_executions: self.cyclic_capacities.maximum_node_executions,
            maximum_pending_cancellations: self.cyclic_capacities.maximum_pending_cancellations,
            application_state_bytes: self.application_state_bytes,
            output_bytes: self.output_bytes,
        })
    }
}

fn validate_subworkflow_tables(
    nodes: &[StructuredNodeDefinition],
    call_initial_ranges: &[BindingRange],
    calls: &[StructuredSubworkflowDefinition],
    state_copies: &[StructuredStateCopy],
    application_state_bytes: usize,
) -> Result<(), RuntimeBindingPlanError> {
    if calls.len() != call_initial_ranges.len() {
        return Err(RuntimeBindingPlanError::InvalidReference);
    }
    let mut copy_start = 0_u32;
    for (index, (call, initial)) in calls.iter().zip(call_initial_ranges).enumerate() {
        let expected = u32::try_from(index).map_err(|_| RuntimeBindingPlanError::InvalidRange)?;
        if call.handle.0 != expected
            || call.initial_nodes.start != initial.start
            || call.initial_nodes.count != initial.count
            || nodes
                .get(
                    usize::try_from(call.node.get())
                        .map_err(|_| RuntimeBindingPlanError::InvalidReference)?,
                )
                .is_none_or(|node| node.kind != StructuredNodeKind::Subworkflow(call.handle))
        {
            return Err(RuntimeBindingPlanError::InvalidReference);
        }
        for range in [call.input_copies, call.output_copies] {
            if range.start != copy_start {
                return Err(RuntimeBindingPlanError::InvalidRange);
            }
            copy_start = copy_start
                .checked_add(range.count)
                .ok_or(RuntimeBindingPlanError::InvalidRange)?;
            if usize::try_from(copy_start)
                .ok()
                .is_none_or(|end| end > state_copies.len())
            {
                return Err(RuntimeBindingPlanError::InvalidRange);
            }
        }
    }
    if usize::try_from(copy_start).ok() != Some(state_copies.len()) {
        return Err(RuntimeBindingPlanError::InvalidRange);
    }
    if state_copies.iter().any(|copy| {
        copy.source >= application_state_bytes || copy.target >= application_state_bytes
    }) {
        return Err(RuntimeBindingPlanError::InvalidSlot);
    }
    Ok(())
}

fn validate_initial_activation(
    nodes: &[StructuredNodeDefinition],
    initial_active: &[WorkflowNodeHandle],
    call_initial_ranges: &[BindingRange],
    call_initial_nodes: &[WorkflowNodeHandle],
) -> Result<(), RuntimeBindingPlanError> {
    let mut seen_initial = allocate_false(nodes.len())?;
    for handle in initial_active {
        let index =
            usize::try_from(handle.get()).map_err(|_| RuntimeBindingPlanError::InvalidReference)?;
        let seen = seen_initial
            .get_mut(index)
            .ok_or(RuntimeBindingPlanError::InvalidReference)?;
        if *seen {
            return Err(RuntimeBindingPlanError::InvalidReference);
        }
        *seen = true;
    }
    let call_count = nodes
        .iter()
        .filter(|node| matches!(node.kind, StructuredNodeKind::Subworkflow(_)))
        .count();
    if call_initial_ranges.len() != call_count {
        return Err(RuntimeBindingPlanError::InvalidReference);
    }
    let mut seen_calls = allocate_false(call_count)?;
    for node in nodes {
        if let StructuredNodeKind::Subworkflow(call) = node.kind {
            let seen = seen_calls
                .get_mut(
                    usize::try_from(call.0)
                        .map_err(|_| RuntimeBindingPlanError::InvalidReference)?,
                )
                .ok_or(RuntimeBindingPlanError::InvalidReference)?;
            if *seen {
                return Err(RuntimeBindingPlanError::InvalidReference);
            }
            *seen = true;
        }
    }
    if seen_calls.iter().any(|seen| !seen) {
        return Err(RuntimeBindingPlanError::InvalidReference);
    }
    let mut expected_start = 0_u32;
    let mut seen_call_initial = allocate_false(nodes.len())?;
    for range in call_initial_ranges {
        if range.start != expected_start {
            return Err(RuntimeBindingPlanError::InvalidRange);
        }
        let end = range
            .start
            .checked_add(range.count)
            .ok_or(RuntimeBindingPlanError::InvalidRange)?;
        let entries = call_initial_nodes
            .get(
                usize::try_from(range.start).map_err(|_| RuntimeBindingPlanError::InvalidRange)?
                    ..usize::try_from(end).map_err(|_| RuntimeBindingPlanError::InvalidRange)?,
            )
            .ok_or(RuntimeBindingPlanError::InvalidRange)?;
        for handle in entries {
            let index = usize::try_from(handle.get())
                .map_err(|_| RuntimeBindingPlanError::InvalidReference)?;
            let seen = seen_call_initial
                .get_mut(index)
                .ok_or(RuntimeBindingPlanError::InvalidReference)?;
            if *seen {
                return Err(RuntimeBindingPlanError::InvalidReference);
            }
            *seen = true;
        }
        expected_start = end;
    }
    if usize::try_from(expected_start).ok() != Some(call_initial_nodes.len()) {
        return Err(RuntimeBindingPlanError::InvalidRange);
    }
    Ok(())
}

impl<B> RuntimeBindingExecutor<B> {
    /// 验证完整节点/Action/condition/端口/guard 闭包并一次性复制固定表。
    ///
    /// # Errors
    /// 缺项、多项、重复项、悬空引用、非连续区间或 staging 越界都会原子拒绝。
    #[allow(clippy::too_many_arguments)]
    /// 低级测试/适配入口；生产装载必须使用 host bridge 生成
    /// [`RuntimeBindingPlan`] 后调用 [`Self::from_plan`]。
    #[doc(hidden)]
    pub fn from_untrusted_tables(
        nodes: &[StructuredNodeDefinition],
        edges: &[StructuredEdgeDefinition],
        node_bindings: &[RuntimeNodeBindingDefinition],
        actions: &[RuntimeActionDefinition],
        ports: &[RuntimeActionPort],
        conditions: &[RuntimeConditionDefinition],
        guards: &[RuntimeGuardDefinition],
        application_state_bytes: usize,
        output_bytes: usize,
        limits: RuntimeBindingLimits,
    ) -> Result<Self, RuntimeBindingPlanError> {
        let plan = RuntimeBindingPlan::from_generated_tables(
            RuntimeBindingPlanIdentity([0; 32]),
            RuntimeCyclicCapacities::new(
                LocalHandle::ZERO.get(),
                u32::try_from(nodes.len().max(1))
                    .map_err(|_| RuntimeBindingPlanError::InvalidCapacity)?,
                u32::try_from(nodes.len().max(1))
                    .map_err(|_| RuntimeBindingPlanError::InvalidCapacity)?,
                0,
            )?,
            nodes,
            edges,
            &[],
            &[],
            &[],
            &[],
            &[],
            node_bindings,
            actions,
            ports,
            conditions,
            guards,
            application_state_bytes,
            output_bytes,
            limits,
        )?;
        Self::from_plan(plan.identity(), plan)
    }

    /// 装载 host 生成的不可拆分 plan，并核对外层包声明的 identity。
    ///
    /// # Errors
    /// identity 不相等时原子拒绝，任何表都不会进入 executor。
    pub fn from_plan(
        expected_identity: RuntimeBindingPlanIdentity,
        plan: RuntimeBindingPlan,
    ) -> Result<Self, RuntimeBindingPlanError> {
        if plan.identity != expected_identity {
            return Err(RuntimeBindingPlanError::PlanIdentityMismatch);
        }
        let trace_before = zeroed_trace_scratch(plan.ports.len())?;
        Ok(Self {
            backend: PhantomData,
            lookup: plan.lookup,
            actions: plan.actions,
            ports: plan.ports,
            conditions: plan.conditions,
            guards: plan.guards,
            trace_before,
        })
    }

    fn condition(
        &self,
        handle: RuntimeConditionHandle,
        context: &mut WorkflowNodeContext<'_, '_, '_>,
    ) -> Result<bool, FaultReason> {
        let index = usize::try_from(handle.0).map_err(|_| FaultReason::CapacityExceeded)?;
        let definition = self
            .conditions
            .get(index)
            .filter(|definition| definition.handle == handle)
            .ok_or(FaultReason::CapacityExceeded)?;
        let value = match definition.source.area {
            RuntimeValueArea::State => {
                context.read_state(WorkSetIndex::new(definition.source.offset_bytes))
            }
            RuntimeValueArea::Output => {
                context.read_output(WorkSetIndex::new(definition.source.offset_bytes))
            }
        }
        .map_err(|_| FaultReason::CapacityExceeded)?;
        Ok(value != 0)
    }
}

impl<B: RuntimeActionBackend> StructuredNodeExecutor for RuntimeBindingExecutor<B> {
    fn execute(
        &mut self,
        node: WorkflowNodeHandle,
        context: &mut WorkflowNodeContext<'_, '_, '_>,
    ) -> Result<StructuredNodeOutcome, FaultReason> {
        self.execute_inner(node, context, None)
            .map_err(|error| match error {
                StructuredNodeExecutionError::Fault(reason) => reason,
                StructuredNodeExecutionError::Trace(_) => FaultReason::TaskExecutionFault,
            })
    }

    fn execute_traced(
        &mut self,
        node: WorkflowNodeHandle,
        context: &mut WorkflowNodeContext<'_, '_, '_>,
        trace: &mut dyn StructuredOutputTrace,
    ) -> Result<StructuredNodeOutcome, StructuredNodeExecutionError> {
        self.execute_inner(node, context, Some(trace))
    }
}

impl<B: RuntimeActionBackend> RuntimeBindingExecutor<B> {
    fn execute_inner(
        &mut self,
        node: WorkflowNodeHandle,
        context: &mut WorkflowNodeContext<'_, '_, '_>,
        mut trace: Option<&mut dyn StructuredOutputTrace>,
    ) -> Result<StructuredNodeOutcome, StructuredNodeExecutionError> {
        let index = usize::try_from(node.get()).map_err(|_| FaultReason::TaskExecutionFault)?;
        let binding = self
            .lookup
            .get(index)
            .copied()
            .flatten()
            .ok_or(FaultReason::TaskExecutionFault)?;
        match binding {
            RuntimeNodeBindingKind::Action {
                action,
                guard,
                success_edge,
            } => {
                let action = self.actions[action.0 as usize];
                let start = action.ports.start as usize;
                let end = start + action.ports.count as usize;
                let trace_enabled = trace.as_ref().is_some_and(|sink| sink.is_enabled());
                if trace_enabled {
                    for port_index in start..end {
                        let port = self.ports[port_index];
                        if port.direction != RuntimePortDirection::Input {
                            read_slot(context, port.slot, &mut self.trace_before[port_index])?;
                        }
                    }
                }
                {
                    let mut binding_context = RuntimeBindingContext {
                        context,
                        ports: &self.ports[start..end],
                        invocation_state: action.invocation_state,
                    };
                    match action.kind {
                        RuntimeActionKind::StPou => B::invoke_st_pou(
                            action.handle,
                            action.target_handle,
                            &mut binding_context,
                        )?,
                        RuntimeActionKind::IoImage => B::invoke_io_image(
                            action.handle,
                            action.target_handle,
                            &mut binding_context,
                        )?,
                        RuntimeActionKind::TypedCommand => B::stage_typed_command(
                            action.handle,
                            action.target_handle,
                            &mut binding_context,
                        )?,
                    }
                }
                if trace_enabled {
                    for port_index in start..end {
                        let port = self.ports[port_index];
                        if port.direction == RuntimePortDirection::Input {
                            continue;
                        }
                        let descriptor =
                            port.output_trace.ok_or(FaultReason::TaskExecutionFault)?;
                        let mut after = [0_u8; 8];
                        read_slot(context, port.slot, &mut after)?;
                        let size = port.slot.value_type.size();
                        if let Some(sink) = trace.as_mut() {
                            sink.stage_output(
                                action.handle.0,
                                descriptor.value_handle,
                                descriptor.type_handle,
                                &self.trace_before[port_index][..size],
                                &after[..size],
                            )
                            .map_err(StructuredNodeExecutionError::Trace)?;
                        }
                    }
                }
                if let Some(condition) = guard
                    && !self.condition(condition, context)?
                {
                    return Ok(StructuredNodeOutcome::Retain);
                }
                Ok(StructuredNodeOutcome::Take(success_edge))
            }
            RuntimeNodeBindingKind::Decision { guards } => {
                let start = guards.start as usize;
                let end = start + guards.count as usize;
                for guard in &self.guards[start..end] {
                    if self.condition(guard.condition, context)? {
                        return Ok(StructuredNodeOutcome::Take(guard.edge));
                    }
                }
                Ok(StructuredNodeOutcome::Retain)
            }
            RuntimeNodeBindingKind::WaitCondition { condition } => Ok(
                StructuredNodeOutcome::Condition(self.condition(condition, context)?),
            ),
        }
    }
}

fn read_slot(
    context: &mut WorkflowNodeContext<'_, '_, '_>,
    slot: RuntimeValueSlot,
    target: &mut [u8; 8],
) -> Result<(), FaultReason> {
    target.fill(0);
    for (relative, byte) in target[..slot.value_type.size()].iter_mut().enumerate() {
        let index = slot
            .offset_bytes
            .checked_add(relative)
            .ok_or(FaultReason::CapacityExceeded)?;
        *byte = match slot.area {
            RuntimeValueArea::State => context.read_state(WorkSetIndex::new(index)),
            RuntimeValueArea::Output => context.read_output(WorkSetIndex::new(index)),
        }
        .map_err(|_| FaultReason::CapacityExceeded)?;
    }
    Ok(())
}

fn validate_capacities(
    actions: &[RuntimeActionDefinition],
    conditions: &[RuntimeConditionDefinition],
    limits: RuntimeBindingLimits,
) -> Result<(), RuntimeBindingPlanError> {
    if limits.maximum_actions == 0
        || limits.maximum_conditions == 0
        || limits.maximum_ports_per_action == 0
        || limits.maximum_guards_per_decision == 0
        || actions.len() > limits.maximum_actions as usize
        || conditions.len() > limits.maximum_conditions as usize
    {
        return Err(RuntimeBindingPlanError::InvalidCapacity);
    }
    Ok(())
}

fn validate_actions(
    actions: &[RuntimeActionDefinition],
    ports: &[RuntimeActionPort],
    state_bytes: usize,
    output_bytes: usize,
    limits: RuntimeBindingLimits,
) -> Result<(), RuntimeBindingPlanError> {
    let mut next = 0_usize;
    let mut trace_handles = Vec::new();
    trace_handles
        .try_reserve_exact(ports.len())
        .map_err(|_| RuntimeBindingPlanError::AllocationFailed)?;
    let mut invocation_ranges = Vec::new();
    invocation_ranges
        .try_reserve_exact(actions.len())
        .map_err(|_| RuntimeBindingPlanError::AllocationFailed)?;
    for (index, action) in actions.iter().enumerate() {
        if usize::try_from(action.handle.0) != Ok(index) || action.target_handle == u32::MAX {
            return Err(RuntimeBindingPlanError::NonDenseHandle);
        }
        if action.version != RuntimeBindingVersion::V1_0 {
            return Err(RuntimeBindingPlanError::InvalidReference);
        }
        let state_end = action
            .invocation_state
            .start
            .checked_add(action.invocation_state.length)
            .filter(|end| *end <= state_bytes)
            .ok_or(RuntimeBindingPlanError::InvalidSlot)?;
        if action.invocation_state.length != 0 {
            invocation_ranges.push((action.invocation_state.start, state_end));
        }
        let range = checked_range(action.ports, ports.len())?;
        if range.start != next || action.ports.count > limits.maximum_ports_per_action {
            return Err(RuntimeBindingPlanError::InvalidRange);
        }
        for (port_index, port) in ports[range.clone()].iter().enumerate() {
            let trace_valid = match (port.direction, port.output_trace) {
                (RuntimePortDirection::Input, None) => true,
                (RuntimePortDirection::Output | RuntimePortDirection::InOut, Some(descriptor)) => {
                    let valid = descriptor.value_handle != u32::MAX
                        && descriptor.type_handle != u32::MAX
                        && !trace_handles.contains(&descriptor.value_handle);
                    if valid {
                        trace_handles.push(descriptor.value_handle);
                    }
                    valid
                }
                _ => false,
            };
            if usize::try_from(port.port) != Ok(port_index)
                || !slot_fits(port.slot, state_bytes, output_bytes)
                || !valid_action_port(action.kind, *port)
                || !trace_valid
            {
                return Err(RuntimeBindingPlanError::InvalidSlot);
            }
        }
        let action_ports = &ports[range.clone()];
        for (port_index, left) in action_ports.iter().enumerate() {
            if left.direction == RuntimePortDirection::Input {
                continue;
            }
            for right in action_ports
                .iter()
                .skip(port_index + 1)
                .filter(|port| port.direction != RuntimePortDirection::Input)
            {
                if left.slot.area == right.slot.area
                    && usize_ranges_overlap(
                        left.slot.offset_bytes,
                        left.slot.value_type.size(),
                        right.slot.offset_bytes,
                        right.slot.value_type.size(),
                    )
                {
                    return Err(RuntimeBindingPlanError::InvalidSlot);
                }
            }
        }
        if action.kind == RuntimeActionKind::TypedCommand
            && ports[range.clone()]
                .iter()
                .filter(|port| port.direction == RuntimePortDirection::Output)
                .count()
                != 1
        {
            return Err(RuntimeBindingPlanError::InvalidSlot);
        }
        next = range.end;
    }
    if next != ports.len() {
        return Err(RuntimeBindingPlanError::InvalidRange);
    }
    invocation_ranges.sort_unstable();
    if invocation_ranges
        .windows(2)
        .any(|pair| pair[1].0 < pair[0].1)
    {
        return Err(RuntimeBindingPlanError::InvalidSlot);
    }
    Ok(())
}

fn validate_invocation_state_aliases(
    actions: &[RuntimeActionDefinition],
    ports: &[RuntimeActionPort],
    conditions: &[RuntimeConditionDefinition],
) -> Result<(), RuntimeBindingPlanError> {
    for action in actions {
        if action.invocation_state.length == 0 {
            continue;
        }
        for slot in ports
            .iter()
            .map(|port| port.slot)
            .chain(conditions.iter().map(|condition| condition.source))
            .filter(|slot| slot.area == RuntimeValueArea::State)
        {
            if usize_ranges_overlap(
                action.invocation_state.start,
                action.invocation_state.length,
                slot.offset_bytes,
                slot.value_type.size(),
            ) {
                return Err(RuntimeBindingPlanError::InvalidSlot);
            }
        }
    }
    Ok(())
}

fn usize_ranges_overlap(
    left_start: usize,
    left_size: usize,
    right_start: usize,
    right_size: usize,
) -> bool {
    left_start < right_start + right_size && right_start < left_start + left_size
}

fn validate_conditions(
    conditions: &[RuntimeConditionDefinition],
    state_bytes: usize,
    output_bytes: usize,
) -> Result<(), RuntimeBindingPlanError> {
    for (index, condition) in conditions.iter().enumerate() {
        if usize::try_from(condition.handle.0) != Ok(index) {
            return Err(RuntimeBindingPlanError::NonDenseHandle);
        }
        if condition.source.value_type != RuntimeValueType::Bool
            || !slot_fits(condition.source, state_bytes, output_bytes)
        {
            return Err(RuntimeBindingPlanError::InvalidSlot);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn validate_node_closure(
    nodes: &[StructuredNodeDefinition],
    edges: &[StructuredEdgeDefinition],
    bindings: &[RuntimeNodeBindingDefinition],
    actions: &[RuntimeActionDefinition],
    conditions: &[RuntimeConditionDefinition],
    guards: &[RuntimeGuardDefinition],
    limits: RuntimeBindingLimits,
) -> Result<Box<[Option<RuntimeNodeBindingKind>]>, RuntimeBindingPlanError> {
    let expected = nodes
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                StructuredNodeKind::Action
                    | StructuredNodeKind::Decision
                    | StructuredNodeKind::WaitCondition { .. }
            )
        })
        .count();
    if bindings.len() != expected {
        return Err(RuntimeBindingPlanError::MissingOrExtraNodeBinding);
    }
    let mut lookup = Vec::new();
    lookup
        .try_reserve_exact(nodes.len())
        .map_err(|_| RuntimeBindingPlanError::AllocationFailed)?;
    lookup.resize(nodes.len(), None);
    let mut used_actions = allocate_false(actions.len())?;
    let mut used_conditions = allocate_false(conditions.len())?;
    let mut next_guard = 0_usize;
    for binding in bindings {
        let node_index = usize::try_from(binding.node.get())
            .ok()
            .filter(|index| *index < nodes.len())
            .ok_or(RuntimeBindingPlanError::InvalidReference)?;
        if lookup[node_index].replace(binding.kind).is_some() {
            return Err(RuntimeBindingPlanError::MissingOrExtraNodeBinding);
        }
        let node = nodes[node_index];
        let outgoing = checked_range(node.outgoing.into(), edges.len())?;
        match (node.kind, binding.kind) {
            (
                StructuredNodeKind::Action,
                RuntimeNodeBindingKind::Action {
                    action,
                    guard,
                    success_edge,
                },
            ) => {
                let action_index = reference(action.0, actions.len())?;
                if used_actions[action_index]
                    || outgoing.len() != 1
                    || edges[outgoing.start].handle != success_edge
                {
                    return Err(RuntimeBindingPlanError::InvalidReference);
                }
                used_actions[action_index] = true;
                if let Some(condition) = guard {
                    used_conditions[reference(condition.0, conditions.len())?] = true;
                }
            }
            (StructuredNodeKind::Decision, RuntimeNodeBindingKind::Decision { guards: range }) => {
                let guard_range = checked_range(range, guards.len())?;
                if guard_range.start != next_guard
                    || range.count > limits.maximum_guards_per_decision
                    || guard_range.len() != outgoing.len()
                {
                    return Err(RuntimeBindingPlanError::InvalidRange);
                }
                for (edge, guard) in edges[outgoing.clone()]
                    .iter()
                    .zip(&guards[guard_range.clone()])
                {
                    if edge.handle != guard.edge {
                        return Err(RuntimeBindingPlanError::InvalidReference);
                    }
                    used_conditions[reference(guard.condition.0, conditions.len())?] = true;
                }
                next_guard = guard_range.end;
            }
            (
                StructuredNodeKind::WaitCondition { .. },
                RuntimeNodeBindingKind::WaitCondition { condition },
            ) => used_conditions[reference(condition.0, conditions.len())?] = true,
            _ => return Err(RuntimeBindingPlanError::InvalidReference),
        }
    }
    if lookup
        .iter()
        .zip(nodes)
        .any(|(binding, node)| binding.is_some() != callback_node(node.kind))
        || used_actions.iter().any(|used| !used)
        || used_conditions.iter().any(|used| !used)
        || next_guard != guards.len()
    {
        return Err(RuntimeBindingPlanError::MissingOrExtraNodeBinding);
    }
    Ok(lookup.into_boxed_slice())
}

const fn callback_node(kind: StructuredNodeKind) -> bool {
    matches!(
        kind,
        StructuredNodeKind::Action
            | StructuredNodeKind::Decision
            | StructuredNodeKind::WaitCondition { .. }
    )
}

fn valid_action_port(kind: RuntimeActionKind, port: RuntimeActionPort) -> bool {
    match kind {
        RuntimeActionKind::StPou => true,
        RuntimeActionKind::IoImage | RuntimeActionKind::TypedCommand => match port.direction {
            RuntimePortDirection::Input => port.slot.area == RuntimeValueArea::State,
            RuntimePortDirection::Output => port.slot.area == RuntimeValueArea::Output,
            RuntimePortDirection::InOut => false,
        },
    }
}

fn slot_fits(slot: RuntimeValueSlot, state_bytes: usize, output_bytes: usize) -> bool {
    let Some(end) = slot.offset_bytes.checked_add(slot.value_type.size()) else {
        return false;
    };
    match slot.area {
        RuntimeValueArea::State => end <= state_bytes,
        RuntimeValueArea::Output => end <= output_bytes,
    }
}

fn reference(raw: u32, len: usize) -> Result<usize, RuntimeBindingPlanError> {
    usize::try_from(raw)
        .ok()
        .filter(|index| *index < len)
        .ok_or(RuntimeBindingPlanError::InvalidReference)
}

fn checked_range(
    range: BindingRange,
    len: usize,
) -> Result<std::ops::Range<usize>, RuntimeBindingPlanError> {
    let start = usize::try_from(range.start).map_err(|_| RuntimeBindingPlanError::InvalidRange)?;
    let count = usize::try_from(range.count).map_err(|_| RuntimeBindingPlanError::InvalidRange)?;
    let end = start
        .checked_add(count)
        .filter(|end| *end <= len)
        .ok_or(RuntimeBindingPlanError::InvalidRange)?;
    Ok(start..end)
}

impl From<crate::WorkflowEdgeRange> for BindingRange {
    fn from(range: crate::WorkflowEdgeRange) -> Self {
        Self {
            start: range.start,
            count: range.count,
        }
    }
}

fn copy_box<T: Copy>(values: &[T]) -> Result<Box<[T]>, RuntimeBindingPlanError> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(values.len())
        .map_err(|_| RuntimeBindingPlanError::AllocationFailed)?;
    result.extend_from_slice(values);
    Ok(result.into_boxed_slice())
}

fn zeroed_trace_scratch(length: usize) -> Result<Box<[[u8; 8]]>, RuntimeBindingPlanError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| RuntimeBindingPlanError::AllocationFailed)?;
    values.resize(length, [0; 8]);
    Ok(values.into_boxed_slice())
}

fn allocate_false(length: usize) -> Result<Box<[bool]>, RuntimeBindingPlanError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| RuntimeBindingPlanError::AllocationFailed)?;
    values.resize(length, false);
    Ok(values.into_boxed_slice())
}
