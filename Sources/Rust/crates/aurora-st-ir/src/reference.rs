//! R1-07 仅用于验证的 Aurora ST Preview 1.0 规范执行器。
//!
//! 执行器消费已经验收的 Canonical IR 和 checkpoint plan，在 host/CI 中使用显式有界的
//! symbolic storage 逐周期执行。它不进入 Target Runtime，也不提供动态加载、Online Change
//! 或跨版本状态恢复能力。

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::checkpoint::CheckpointPlanner;
use crate::fault::decode_string;
use crate::{
    AstNodeKind, CanonicalIrVersion, CanonicalNode, CanonicalPou, CanonicalSourceMap,
    CanonicalSourceMapVersion, CanonicalStIr, CheckpointPlan, CheckpointPlanLimits,
    CheckpointPlanVersion, CheckpointSiteId, CheckpointSiteKind, DifferentialCycle,
    DifferentialDiagnostic, DifferentialFault, DifferentialStatus, DifferentialValue,
    FixedFieldLayout, FixedTypeId, FixedTypeKind, FixedTypeLayout, IntegerArithmeticError,
    IntegerArithmeticMode, IntegerOperation, IntegerType, RuntimeFaultCode, SemanticSymbol,
    SemanticSymbolKind, SemanticType, SymbolId, TaskHandle, evaluate_integer_operation,
    validate_array_index,
};

const INACTIVE_ACTIVATION: u32 = u32::MAX;

/// 规范执行器的强制 host-side 容量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceLimits {
    storage_entries: usize,
    storage_bytes: usize,
    cycles: u64,
    inputs_per_cycle: usize,
    checkpoints_per_cycle: usize,
}

impl ReferenceLimits {
    /// 验证并建立全部非零容量。
    ///
    /// # Errors
    ///
    /// 任一容量为零时返回 [`ReferenceLimitError`]。
    pub const fn new(
        max_storage_entries: usize,
        max_storage_bytes: usize,
        max_cycles: u64,
        max_inputs_per_cycle: usize,
        max_checkpoints_per_cycle: usize,
    ) -> Result<Self, ReferenceLimitError> {
        if max_storage_entries == 0 {
            return Err(ReferenceLimitError::ZeroStorageEntries);
        }
        if max_storage_bytes == 0 {
            return Err(ReferenceLimitError::ZeroStorageBytes);
        }
        if max_cycles == 0 {
            return Err(ReferenceLimitError::ZeroCycles);
        }
        if max_inputs_per_cycle == 0 {
            return Err(ReferenceLimitError::ZeroInputs);
        }
        if max_checkpoints_per_cycle == 0 {
            return Err(ReferenceLimitError::ZeroCheckpoints);
        }
        Ok(Self {
            storage_entries: max_storage_entries,
            storage_bytes: max_storage_bytes,
            cycles: max_cycles,
            inputs_per_cycle: max_inputs_per_cycle,
            checkpoints_per_cycle: max_checkpoints_per_cycle,
        })
    }

    /// 最大 symbolic storage entry 数量。
    #[must_use]
    pub const fn max_storage_entries(self) -> usize {
        self.storage_entries
    }

    /// 所有 committed storage byte 的最大总量。
    #[must_use]
    pub const fn max_storage_bytes(self) -> usize {
        self.storage_bytes
    }

    /// 一个 executor instance 最多执行的周期数。
    #[must_use]
    pub const fn max_cycles(self) -> u64 {
        self.cycles
    }

    /// 每周期最多接受的外部 scalar input 数量。
    #[must_use]
    pub const fn max_inputs_per_cycle(self) -> usize {
        self.inputs_per_cycle
    }

    /// 每周期最多记录的实际 checkpoint occurrence 数量。
    #[must_use]
    pub const fn max_checkpoints_per_cycle(self) -> usize {
        self.checkpoints_per_cycle
    }
}

/// 非法的零值规范执行容量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ReferenceLimitError {
    /// storage entry 容量为零。
    #[error("max_storage_entries must be non-zero")]
    ZeroStorageEntries,
    /// storage byte 容量为零。
    #[error("max_storage_bytes must be non-zero")]
    ZeroStorageBytes,
    /// 周期容量为零。
    #[error("max_cycles must be non-zero")]
    ZeroCycles,
    /// 输入容量为零。
    #[error("max_inputs_per_cycle must be non-zero")]
    ZeroInputs,
    /// checkpoint occurrence 容量为零。
    #[error("max_checkpoints_per_cycle must be non-zero")]
    ZeroCheckpoints,
}

/// 一个周期开始前写入 scalar global 的 canonical bits。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReferenceInput {
    /// Global declaration identity。
    pub symbol: SymbolId,
    /// Canonical little-endian scalar bit pattern；窄值只使用低位。
    pub value_bits: u64,
}

/// 一个规范执行周期的显式输入与 checkpoint stop 注入。
#[derive(Debug, Clone, Copy)]
pub struct ReferenceCycleRequest<'a> {
    /// 要执行的静态 Task。
    pub task: TaskHandle,
    /// 在 begin 边界原子应用的 global scalar inputs。
    pub inputs: &'a [ReferenceInput],
    /// 命中该非 Task-return checkpoint 时返回 `CheckpointStop`。
    pub stop_at_checkpoint: Option<CheckpointSiteId>,
}

/// 规范执行器的输入、容量或 Canonical artifact 不一致。
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReferenceExecutionError {
    /// IR、Source Map 或 checkpoint plan 不是同一份 Preview 1.0 产物。
    #[error("inconsistent reference execution artifacts")]
    InconsistentArtifact,
    /// 请求了不存在的 Task。
    #[error("unknown reference task {0}")]
    UnknownTask(u32),
    /// 输入不是一个已声明的 scalar global。
    #[error("invalid scalar reference input symbol {0}")]
    InvalidInput(u32),
    /// 同一周期重复写入一个 input。
    #[error("duplicate scalar reference input symbol {0}")]
    DuplicateInput(u32),
    /// Canonical node shape 不是已接受模型可生成的形状。
    #[error("invalid executable Canonical node {node} ({kind:?})")]
    InvalidNode {
        /// Canonical node identity。
        node: u32,
        /// 节点类别。
        kind: AstNodeKind,
    },
    /// storage identity、layout 或范围不一致。
    #[error("invalid reference storage for symbol {0}")]
    InvalidStorage(u32),
    /// 规范执行容量超过调用方限制。
    #[error("reference {resource} requires {actual}, exceeding limit {limit}")]
    CapacityExceeded {
        /// 超限资源。
        resource: &'static str,
        /// 实际需求。
        actual: usize,
        /// 允许上限。
        limit: usize,
    },
    /// 周期计数无法继续且不会回绕。
    #[error("reference cycle sequence exhausted")]
    CycleSequenceExhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct StorageKey {
    task: Option<TaskHandle>,
    activation: u32,
    symbol: SymbolId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageRole {
    Global,
    State,
    Frame,
    Alias,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StorageEntry {
    role: StorageRole,
    bytes: Vec<u8>,
}

struct ReferenceModel {
    ir: CanonicalStIr,
    symbols: BTreeMap<SymbolId, SemanticSymbol>,
    layouts: BTreeMap<FixedTypeId, FixedTypeLayout>,
    pous: BTreeMap<SymbolId, CanonicalPou>,
    checkpoints: BTreeMap<crate::CanonicalNodeId, Vec<(CheckpointSiteId, CheckpointSiteKind)>>,
}

/// Host/CI 规范执行器。
///
/// `run_cycle` 会克隆一份有界 committed storage 作为 staging。只有 `Completed` 才整体提交；
/// Fault 或 checkpoint stop 均完整丢弃 staging。该类型会分配，且不是周期 Runtime API。
pub struct ReferenceExecutor {
    model: ReferenceModel,
    committed: BTreeMap<StorageKey, StorageEntry>,
    limits: ReferenceLimits,
    next_cycle: u64,
}

impl ReferenceExecutor {
    /// 从同批 Canonical IR、Source Map 和 checkpoint plan 建立规范执行器。
    ///
    /// 构造会重新生成 checkpoint plan 并逐字段比较，防止少生成或多生成执行点；同时验证初始
    /// symbolic storage 的 entry/byte 总量。
    ///
    /// # Errors
    ///
    /// artifact 不一致、初始化布局损坏或容量越界时返回 [`ReferenceExecutionError`]。
    pub fn new(
        ir: &CanonicalStIr,
        source_map: &CanonicalSourceMap,
        checkpoints: &CheckpointPlan,
        limits: ReferenceLimits,
    ) -> Result<Self, ReferenceExecutionError> {
        validate_artifacts(ir, source_map, checkpoints)?;
        let symbols = unique_map(&ir.symbols, |entry| entry.id)?;
        let layouts = unique_map(&ir.types, |entry| entry.id)?;
        let pous = unique_map(&ir.pous, |entry| entry.symbol)?;
        let mut model = ReferenceModel {
            ir: ir.clone(),
            symbols,
            layouts,
            pous,
            checkpoints: checkpoint_sites(checkpoints),
        };
        let committed = initial_storage(&mut model, limits)?;
        Ok(Self {
            model,
            committed,
            limits,
            next_cycle: 0,
        })
    }

    /// 执行一个周期，并返回提交或回滚后的完整可比较观察值。
    ///
    /// `inputs` 先在 begin 边界原子应用；重复或非法输入不会修改 committed storage。执行过程
    /// 单线程、无外部 I/O，所有循环次数来自 Canonical IR 的静态证明。
    ///
    /// # Errors
    ///
    /// Task/input 不存在、artifact 内部不一致或任何显式容量越界时返回错误。
    pub fn run_cycle(
        &mut self,
        request: ReferenceCycleRequest<'_>,
    ) -> Result<DifferentialCycle, ReferenceExecutionError> {
        if self.next_cycle >= self.limits.max_cycles() {
            return Err(ReferenceExecutionError::CycleSequenceExhausted);
        }
        let task = self
            .model
            .ir
            .tasks
            .iter()
            .find(|entry| entry.task == request.task)
            .copied()
            .ok_or(ReferenceExecutionError::UnknownTask(request.task.0))?;
        let changes = self.validate_inputs(request.inputs)?;
        for (key, bytes) in changes {
            let entry = self
                .committed
                .get_mut(&key)
                .ok_or(ReferenceExecutionError::InvalidStorage(key.symbol.0))?;
            entry.bytes = bytes;
        }
        let mut staging = self.committed.clone();
        reset_frame(
            &self.model,
            &mut staging,
            self.limits,
            request.task,
            task.program,
            INACTIVE_ACTIVATION,
        )?;
        let cycle = self.next_cycle;
        let mut execution = Execution {
            model: &self.model,
            storage: &mut staging,
            limits: self.limits,
            task: request.task,
            activation: INACTIVE_ACTIVATION,
            pou: task.program,
            stop_at_checkpoint: request.stop_at_checkpoint,
            checkpoints: Vec::new(),
        };
        let result = execution.execute_pou(task.program, INACTIVE_ACTIVATION);
        let reached = execution.checkpoints;
        let (status, fault) = match result {
            Ok(()) => {
                self.committed = staging;
                (DifferentialStatus::Completed, None)
            }
            Err(ExecutionAbort::CheckpointStop) => (DifferentialStatus::CheckpointStop, None),
            Err(ExecutionAbort::Fault(fault)) => (DifferentialStatus::Faulted, Some(fault)),
            Err(ExecutionAbort::Invalid(error)) => return Err(error),
        };
        let diagnostic = fault.map(|fault| DifferentialDiagnostic {
            cycle,
            site: fault.site,
            code: fault.code,
        });
        let observation = DifferentialCycle {
            cycle,
            task: request.task,
            status,
            state: snapshot_state(&self.committed, request.task),
            outputs: snapshot_outputs(&self.model, &self.committed, request.task)?,
            fault,
            diagnostics: diagnostic.into_iter().collect(),
            checkpoints: reached,
        };
        self.next_cycle = self
            .next_cycle
            .checked_add(1)
            .ok_or(ReferenceExecutionError::CycleSequenceExhausted)?;
        Ok(observation)
    }

    fn validate_inputs(
        &self,
        inputs: &[ReferenceInput],
    ) -> Result<Vec<(StorageKey, Vec<u8>)>, ReferenceExecutionError> {
        enforce(
            "inputs per cycle",
            inputs.len(),
            self.limits.max_inputs_per_cycle(),
        )?;
        let mut used = BTreeSet::new();
        let mut changes = Vec::with_capacity(inputs.len());
        for input in inputs {
            if !used.insert(input.symbol) {
                return Err(ReferenceExecutionError::DuplicateInput(input.symbol.0));
            }
            let symbol = self
                .model
                .symbols
                .get(&input.symbol)
                .filter(|entry| entry.kind == SemanticSymbolKind::GlobalVariable)
                .ok_or(ReferenceExecutionError::InvalidInput(input.symbol.0))?;
            let layout = self.model.layout_for_symbol(symbol.id)?;
            if !is_scalar_layout(layout) {
                return Err(ReferenceExecutionError::InvalidInput(input.symbol.0));
            }
            let size = usize::try_from(layout.size_bytes)
                .map_err(|_| ReferenceExecutionError::InvalidInput(input.symbol.0))?;
            let encoded = input.value_bits.to_le_bytes();
            let bytes = encoded
                .get(..size)
                .ok_or(ReferenceExecutionError::InvalidInput(input.symbol.0))?
                .to_vec();
            changes.push((global_key(input.symbol), bytes));
        }
        Ok(changes)
    }
}

impl ReferenceModel {
    fn symbol(&self, id: SymbolId) -> Result<&SemanticSymbol, ReferenceExecutionError> {
        self.symbols
            .get(&id)
            .ok_or(ReferenceExecutionError::InvalidStorage(id.0))
    }

    fn layout(&self, mut id: FixedTypeId) -> Result<&FixedTypeLayout, ReferenceExecutionError> {
        for _ in 0..=self.layouts.len() {
            let layout = self
                .layouts
                .get(&id)
                .ok_or(ReferenceExecutionError::InvalidStorage(id.0))?;
            if let FixedTypeKind::Alias { target } = layout.kind {
                id = target;
            } else {
                return Ok(layout);
            }
        }
        Err(ReferenceExecutionError::InvalidStorage(id.0))
    }

    fn layout_for_symbol(
        &self,
        symbol: SymbolId,
    ) -> Result<&FixedTypeLayout, ReferenceExecutionError> {
        let value_type = self
            .symbol(symbol)?
            .declared_type
            .as_ref()
            .ok_or(ReferenceExecutionError::InvalidStorage(symbol.0))?;
        let declaration = match value_type {
            SemanticType::Named { declaration }
            | SemanticType::Composite {
                declaration: Some(declaration),
            }
            | SemanticType::Enumeration {
                declaration: Some(declaration),
            }
            | SemanticType::FunctionBlock { declaration } => Some(*declaration),
            _ => None,
        };
        let layout = self.layouts.values().find(|layout| {
            if let Some(declaration) = declaration {
                layout.declaration == Some(declaration)
            } else {
                scalar_layout_matches(&layout.kind, value_type)
            }
        });
        layout.ok_or(ReferenceExecutionError::InvalidStorage(symbol.0))
    }

    fn field(
        &self,
        layout: FixedTypeId,
        name: &str,
    ) -> Result<FixedFieldLayout, ReferenceExecutionError> {
        let layout = self.layout(layout)?;
        let (FixedTypeKind::Structure { fields } | FixedTypeKind::FunctionBlock { fields, .. }) =
            &layout.kind
        else {
            return Err(ReferenceExecutionError::InvalidStorage(layout.id.0));
        };
        fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(name))
            .cloned()
            .ok_or(ReferenceExecutionError::InvalidStorage(layout.id.0))
    }
}

fn validate_artifacts(
    ir: &CanonicalStIr,
    source_map: &CanonicalSourceMap,
    checkpoints: &CheckpointPlan,
) -> Result<(), ReferenceExecutionError> {
    if ir.schema_version != CanonicalIrVersion::preview_v1_0()
        || source_map.schema_version != CanonicalSourceMapVersion::preview_v1_0()
        || checkpoints.schema_version != CheckpointPlanVersion::preview_v1_0()
    {
        return Err(ReferenceExecutionError::InconsistentArtifact);
    }
    let limits = CheckpointPlanLimits::new(usize::MAX, usize::MAX, 1)
        .map_err(|_| ReferenceExecutionError::InconsistentArtifact)?;
    let expected = CheckpointPlanner::new(ir, source_map, limits)
        .map_err(|_| ReferenceExecutionError::InconsistentArtifact)?
        .build()
        .map_err(|_| ReferenceExecutionError::InconsistentArtifact)?;
    if &expected != checkpoints {
        return Err(ReferenceExecutionError::InconsistentArtifact);
    }
    Ok(())
}

fn unique_map<T: Clone, K: Ord + Copy>(
    values: &[T],
    key: impl Fn(&T) -> K,
) -> Result<BTreeMap<K, T>, ReferenceExecutionError> {
    let mut result = BTreeMap::new();
    for value in values {
        if result.insert(key(value), value.clone()).is_some() {
            return Err(ReferenceExecutionError::InconsistentArtifact);
        }
    }
    Ok(result)
}

fn checkpoint_sites(
    plan: &CheckpointPlan,
) -> BTreeMap<crate::CanonicalNodeId, Vec<(CheckpointSiteId, CheckpointSiteKind)>> {
    let mut result = BTreeMap::new();
    for site in &plan.sites {
        if !matches!(site.site, CheckpointSiteKind::TaskReturn { .. }) {
            result
                .entry(site.node)
                .or_insert_with(Vec::new)
                .push((site.id, site.site));
        }
    }
    result
}

fn global_key(symbol: SymbolId) -> StorageKey {
    StorageKey {
        task: None,
        activation: INACTIVE_ACTIVATION,
        symbol,
    }
}

fn initial_storage(
    model: &mut ReferenceModel,
    limits: ReferenceLimits,
) -> Result<BTreeMap<StorageKey, StorageEntry>, ReferenceExecutionError> {
    let mut entries = Vec::new();
    for image in &model.ir.initialization.globals {
        entries.push((
            global_key(image.global),
            StorageEntry {
                role: StorageRole::Global,
                bytes: image.bytes.clone(),
            },
        ));
    }
    for image in &model.ir.initialization.tasks {
        let program = model
            .ir
            .programs
            .iter()
            .find(|entry| entry.program == image.program)
            .ok_or(ReferenceExecutionError::InvalidStorage(image.program.0))?;
        for field in &program.fields {
            let symbol = field_symbol(model, image.program, &field.name)?;
            let layout = model.layout(field.value_type)?;
            let bytes = slice_bytes(&image.bytes, field.offset_bytes, layout.size_bytes, symbol)?;
            entries.push((
                StorageKey {
                    task: Some(image.task),
                    activation: INACTIVE_ACTIVATION,
                    symbol,
                },
                StorageEntry {
                    role: StorageRole::State,
                    bytes,
                },
            ));
        }
    }
    let mut result = BTreeMap::new();
    for (key, entry) in entries {
        if result.insert(key, entry).is_some() {
            return Err(ReferenceExecutionError::InvalidStorage(key.symbol.0));
        }
    }
    enforce_storage(&result, limits)?;
    Ok(result)
}

fn field_symbol(
    model: &ReferenceModel,
    owner: SymbolId,
    name: &str,
) -> Result<SymbolId, ReferenceExecutionError> {
    model
        .symbols
        .values()
        .find(|symbol| {
            symbol.owner == Some(owner) && symbol.canonical_name.eq_ignore_ascii_case(name)
        })
        .map(|symbol| symbol.id)
        .ok_or(ReferenceExecutionError::InvalidStorage(owner.0))
}

fn slice_bytes(
    source: &[u8],
    offset: u64,
    size: u64,
    symbol: SymbolId,
) -> Result<Vec<u8>, ReferenceExecutionError> {
    let start =
        usize::try_from(offset).map_err(|_| ReferenceExecutionError::InvalidStorage(symbol.0))?;
    let length =
        usize::try_from(size).map_err(|_| ReferenceExecutionError::InvalidStorage(symbol.0))?;
    let end = start
        .checked_add(length)
        .ok_or(ReferenceExecutionError::InvalidStorage(symbol.0))?;
    source
        .get(start..end)
        .map(<[u8]>::to_vec)
        .ok_or(ReferenceExecutionError::InvalidStorage(symbol.0))
}

fn reset_frame(
    model: &ReferenceModel,
    storage: &mut BTreeMap<StorageKey, StorageEntry>,
    limits: ReferenceLimits,
    task: TaskHandle,
    pou: SymbolId,
    activation: u32,
) -> Result<(), ReferenceExecutionError> {
    let frame = model
        .ir
        .invocation_frames
        .iter()
        .find(|entry| entry.pou == pou)
        .ok_or(ReferenceExecutionError::InvalidStorage(pou.0))?;
    let image = model
        .ir
        .initialization
        .frames
        .iter()
        .find(|entry| entry.pou == pou)
        .ok_or(ReferenceExecutionError::InvalidStorage(pou.0))?;
    for field in &frame.fields {
        let symbol = field_symbol(model, pou, &field.name)?;
        let layout = model.layout(field.value_type)?;
        let bytes = slice_bytes(&image.bytes, field.offset_bytes, layout.size_bytes, symbol)?;
        storage.insert(
            StorageKey {
                task: Some(task),
                activation,
                symbol,
            },
            StorageEntry {
                role: StorageRole::Frame,
                bytes,
            },
        );
    }
    if model.symbol(pou)?.kind.eq(&SemanticSymbolKind::Function) {
        let layout = model.layout_for_symbol(pou)?;
        storage.insert(
            StorageKey {
                task: Some(task),
                activation,
                symbol: pou,
            },
            StorageEntry {
                role: StorageRole::Frame,
                bytes: vec![
                    0;
                    usize::try_from(layout.size_bytes)
                        .map_err(|_| ReferenceExecutionError::InvalidStorage(pou.0))?
                ],
            },
        );
    }
    enforce_storage(storage, limits)
}

fn snapshot_state(
    storage: &BTreeMap<StorageKey, StorageEntry>,
    task: TaskHandle,
) -> Vec<DifferentialValue> {
    storage
        .iter()
        .filter(|(key, entry)| key.task == Some(task) && entry.role == StorageRole::State)
        .map(|(key, entry)| DifferentialValue {
            task,
            activation: key.activation,
            symbol: key.symbol,
            bytes: entry.bytes.clone(),
        })
        .collect()
}

fn snapshot_outputs(
    model: &ReferenceModel,
    storage: &BTreeMap<StorageKey, StorageEntry>,
    task: TaskHandle,
) -> Result<Vec<DifferentialValue>, ReferenceExecutionError> {
    model
        .ir
        .tags
        .iter()
        .filter(|tag| tag.writer_tasks.binary_search(&task).is_ok())
        .map(|tag| {
            let entry = storage
                .get(&global_key(tag.global))
                .ok_or(ReferenceExecutionError::InvalidStorage(tag.global.0))?;
            Ok(DifferentialValue {
                task,
                activation: INACTIVE_ACTIVATION,
                symbol: tag.global,
                bytes: entry.bytes.clone(),
            })
        })
        .collect()
}

fn enforce(
    resource: &'static str,
    actual: usize,
    limit: usize,
) -> Result<(), ReferenceExecutionError> {
    if actual > limit {
        Err(ReferenceExecutionError::CapacityExceeded {
            resource,
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}

fn enforce_storage(
    storage: &BTreeMap<StorageKey, StorageEntry>,
    limits: ReferenceLimits,
) -> Result<(), ReferenceExecutionError> {
    enforce(
        "storage entries",
        storage.len(),
        limits.max_storage_entries(),
    )?;
    let bytes = storage.values().try_fold(0_usize, |sum, entry| {
        sum.checked_add(entry.bytes.len())
            .ok_or(ReferenceExecutionError::CapacityExceeded {
                resource: "storage bytes",
                actual: usize::MAX,
                limit: limits.max_storage_bytes(),
            })
    })?;
    enforce("storage bytes", bytes, limits.max_storage_bytes())
}

#[derive(Clone)]
enum Value {
    Scalar(u64),
    Aggregate { layout: FixedTypeId, bytes: Vec<u8> },
}

#[derive(Clone, Copy)]
struct Location {
    key: StorageKey,
    offset: usize,
    layout: FixedTypeId,
}

enum ExecutionAbort {
    Fault(DifferentialFault),
    CheckpointStop,
    Invalid(ReferenceExecutionError),
}

type ExecutionResult<T> = Result<T, ExecutionAbort>;

impl From<ReferenceExecutionError> for ExecutionAbort {
    fn from(value: ReferenceExecutionError) -> Self {
        Self::Invalid(value)
    }
}

struct Execution<'a> {
    model: &'a ReferenceModel,
    storage: &'a mut BTreeMap<StorageKey, StorageEntry>,
    limits: ReferenceLimits,
    task: TaskHandle,
    activation: u32,
    pou: SymbolId,
    stop_at_checkpoint: Option<CheckpointSiteId>,
    checkpoints: Vec<CheckpointSiteId>,
}

impl Execution<'_> {
    fn execute_pou(&mut self, pou: SymbolId, activation: u32) -> ExecutionResult<()> {
        let body = self
            .model
            .pous
            .get(&pou)
            .ok_or(ReferenceExecutionError::InvalidStorage(pou.0))?
            .body
            .clone();
        let previous_pou = self.pou;
        let previous_activation = self.activation;
        self.pou = pou;
        self.activation = activation;
        let result = self.statement(&body);
        self.pou = previous_pou;
        self.activation = previous_activation;
        match result {
            Ok(()) | Err(FlowAbort::Return) => Ok(()),
            Err(FlowAbort::Execution(error)) => Err(error),
        }
    }

    fn statement(&mut self, node: &CanonicalNode) -> Result<(), FlowAbort> {
        match node.kind {
            AstNodeKind::StatementList => {
                for child in &node.children {
                    self.statement(child)?;
                }
            }
            AstNodeKind::AssignmentStatement => {
                let [target, value] = node.children.as_slice() else {
                    return Err(self.invalid(node).into());
                };
                let location = self.location(target).map_err(FlowAbort::Execution)?;
                let value = self
                    .expression_for_layout(value, location.layout)
                    .map_err(FlowAbort::Execution)?;
                self.write(location, value).map_err(FlowAbort::Execution)?;
            }
            AstNodeKind::IfStatement => self.if_statement(node)?,
            AstNodeKind::ForStatement => self.for_statement(node)?,
            AstNodeKind::ReturnStatement => {
                if let Some(value) = node.children.first() {
                    let layout = self
                        .model
                        .layout_for_symbol(self.pou)
                        .map_err(ExecutionAbort::from)
                        .map_err(FlowAbort::Execution)?
                        .id;
                    let value = self
                        .expression_for_layout(value, layout)
                        .map_err(FlowAbort::Execution)?;
                    self.write(
                        Location {
                            key: StorageKey {
                                task: Some(self.task),
                                activation: self.activation,
                                symbol: self.pou,
                            },
                            offset: 0,
                            layout,
                        },
                        value,
                    )
                    .map_err(FlowAbort::Execution)?;
                }
                return Err(FlowAbort::Return);
            }
            AstNodeKind::FunctionBlockCallStatement => {
                self.function_block_call(node)
                    .map_err(FlowAbort::Execution)?;
            }
            _ => return Err(self.invalid(node).into()),
        }
        Ok(())
    }

    fn if_statement(&mut self, node: &CanonicalNode) -> Result<(), FlowAbort> {
        if node.children.len() < 2 {
            return Err(self.invalid(node).into());
        }
        if self
            .scalar(&node.children[0])
            .map_err(FlowAbort::Execution)?
            != 0
        {
            return self.statement(&node.children[1]);
        }
        for clause in node.children.iter().skip(2) {
            match clause.kind {
                AstNodeKind::ElsifClause => {
                    let [condition, body] = clause.children.as_slice() else {
                        return Err(self.invalid(clause).into());
                    };
                    if self.scalar(condition).map_err(FlowAbort::Execution)? != 0 {
                        return self.statement(body);
                    }
                }
                AstNodeKind::ElseClause => {
                    let [body] = clause.children.as_slice() else {
                        return Err(self.invalid(clause).into());
                    };
                    return self.statement(body);
                }
                _ => return Err(self.invalid(clause).into()),
            }
        }
        Ok(())
    }

    fn for_statement(&mut self, node: &CanonicalNode) -> Result<(), FlowAbort> {
        let iterations = node.loop_iterations.ok_or_else(|| self.invalid(node))?;
        let body_index = node
            .children
            .len()
            .checked_sub(1)
            .filter(|index| *index >= 3)
            .ok_or_else(|| self.invalid(node))?;
        let control_symbol = node.children[0]
            .symbol
            .ok_or_else(|| self.invalid(&node.children[0]))?;
        let control = Location {
            key: StorageKey {
                task: Some(self.task),
                activation: self.activation,
                symbol: control_symbol,
            },
            offset: 0,
            layout: self
                .model
                .layout_for_symbol(control_symbol)
                .map_err(ExecutionAbort::from)
                .map_err(FlowAbort::Execution)?
                .id,
        };
        let mut current = self
            .scalar(&node.children[1])
            .map_err(FlowAbort::Execution)?;
        self.write(control, Value::Scalar(current))
            .map_err(FlowAbort::Execution)?;
        let step = if body_index == 4 {
            self.scalar(&node.children[3])
                .map_err(FlowAbort::Execution)?
        } else {
            1
        };
        for _ in 0..iterations {
            self.statement(&node.children[body_index])?;
            self.checkpoint(node, |kind| {
                matches!(kind, CheckpointSiteKind::LoopBackEdge)
            })
            .map_err(FlowAbort::Execution)?;
            current = current.wrapping_add(step);
            self.write(control, Value::Scalar(current))
                .map_err(FlowAbort::Execution)?;
        }
        Ok(())
    }

    fn expression_for_layout(
        &mut self,
        node: &CanonicalNode,
        layout: FixedTypeId,
    ) -> ExecutionResult<Value> {
        if is_scalar_layout(self.model.layout(layout)?) {
            self.scalar(node).map(Value::Scalar)
        } else {
            self.aggregate(node, layout)
        }
    }

    fn scalar(&mut self, node: &CanonicalNode) -> ExecutionResult<u64> {
        match node.kind {
            AstNodeKind::Literal => scalar_literal(node, value_type(node)).map_err(Into::into),
            AstNodeKind::QualifiedLiteral => {
                let value = node.children.last().ok_or_else(|| self.invalid(node))?;
                if value.kind == AstNodeKind::Identifier {
                    self.enumeration_literal(value)
                } else {
                    self.scalar(value)
                }
            }
            AstNodeKind::ParenthesizedExpression => {
                self.scalar(node.children.last().ok_or_else(|| self.invalid(node))?)
            }
            AstNodeKind::Assignable => {
                let location = self.location(node)?;
                match self.read(location)? {
                    Value::Scalar(value) => Ok(normalize(value, value_type(node))),
                    Value::Aggregate { .. } => Err(self.invalid(node)),
                }
            }
            AstNodeKind::UnaryExpression => self.unary(node),
            AstNodeKind::BinaryExpression => self.binary(node),
            AstNodeKind::CallExpression => self.call_expression(node),
            _ => Err(self.invalid(node)),
        }
    }

    fn unary(&mut self, node: &CanonicalNode) -> ExecutionResult<u64> {
        let child = node.children.first().ok_or_else(|| self.invalid(node))?;
        let value = self.scalar(child)?;
        Ok(match node.text.as_deref() {
            Some("+") => value,
            Some("-") if matches!(value_type(node), Some(SemanticType::Real)) => {
                value ^ u64::from(1_u32 << 31)
            }
            Some("-") if matches!(value_type(node), Some(SemanticType::Lreal)) => {
                value ^ (1_u64 << 63)
            }
            Some("-") => normalize(value.wrapping_neg(), value_type(node)),
            Some("NOT") => normalize(!value, value_type(node)),
            _ => return Err(self.invalid(node)),
        })
    }

    fn binary(&mut self, node: &CanonicalNode) -> ExecutionResult<u64> {
        let [left_node, right_node] = node.children.as_slice() else {
            return Err(self.invalid(node));
        };
        let left = self.scalar(left_node)?;
        if node.text.as_deref() == Some("AND_THEN") && left == 0 {
            return Ok(0);
        }
        if node.text.as_deref() == Some("OR_ELSE") && left != 0 {
            return Ok(1);
        }
        let right = self.scalar(right_node)?;
        let operand_type = value_type(left_node);
        if is_float(operand_type) {
            return self.float_binary(node, left, right, operand_type);
        }
        let result = match node.text.as_deref() {
            Some("+") => integer_policy(
                operand_type,
                IntegerOperation::Add,
                Some(IntegerArithmeticMode::Checked),
                left,
                Some(right),
            )
            .map_err(|fault| self.abort_fault(node, fault))?,
            Some("-") => integer_policy(
                operand_type,
                IntegerOperation::Subtract,
                Some(IntegerArithmeticMode::Checked),
                left,
                Some(right),
            )
            .map_err(|fault| self.abort_fault(node, fault))?,
            Some("*") => integer_policy(
                operand_type,
                IntegerOperation::Multiply,
                Some(IntegerArithmeticMode::Checked),
                left,
                Some(right),
            )
            .map_err(|fault| self.abort_fault(node, fault))?,
            Some("/") => integer_policy(
                operand_type,
                IntegerOperation::Divide,
                None,
                left,
                Some(right),
            )
            .map_err(|fault| self.abort_fault(node, fault))?,
            Some("MOD") => integer_policy(
                operand_type,
                IntegerOperation::Modulo,
                None,
                left,
                Some(right),
            )
            .map_err(|fault| self.abort_fault(node, fault))?,
            Some("AND") => normalize(left & right, operand_type),
            Some("OR") => normalize(left | right, operand_type),
            Some("XOR") => normalize(left ^ right, operand_type),
            Some("AND_THEN") => u64::from(left != 0 && right != 0),
            Some("OR_ELSE") => u64::from(left != 0 || right != 0),
            Some("=" | "<>" | "<" | "<=" | ">" | ">=") => u64::from(integer_compare(
                node.text.as_deref().unwrap_or_default(),
                left,
                right,
                operand_type,
            )),
            _ => return Err(self.invalid(node)),
        };
        Ok(result)
    }

    #[allow(
        clippy::float_cmp,
        reason = "Preview 1.0 要求 IEEE ordered exact comparison，不使用近似比较"
    )]
    fn float_binary(
        &mut self,
        node: &CanonicalNode,
        left: u64,
        right: u64,
        value_type: Option<&SemanticType>,
    ) -> ExecutionResult<u64> {
        if matches!(value_type, Some(SemanticType::Real)) {
            let left = f32::from_bits(low_u32(left));
            let right = f32::from_bits(low_u32(right));
            let value = match node.text.as_deref() {
                Some("+") => return self.finite_f32(node, left + right),
                Some("-") => return self.finite_f32(node, left - right),
                Some("*") => return self.finite_f32(node, left * right),
                Some("/") => return self.finite_f32(node, left / right),
                Some("=") => left == right,
                Some("<>") => left != right,
                Some("<") => left < right,
                Some("<=") => left <= right,
                Some(">") => left > right,
                Some(">=") => left >= right,
                _ => return Err(self.invalid(node)),
            };
            Ok(u64::from(value))
        } else {
            let left = f64::from_bits(left);
            let right = f64::from_bits(right);
            let value = match node.text.as_deref() {
                Some("+") => return self.finite_f64(node, left + right),
                Some("-") => return self.finite_f64(node, left - right),
                Some("*") => return self.finite_f64(node, left * right),
                Some("/") => return self.finite_f64(node, left / right),
                Some("=") => left == right,
                Some("<>") => left != right,
                Some("<") => left < right,
                Some("<=") => left <= right,
                Some(">") => left > right,
                Some(">=") => left >= right,
                _ => return Err(self.invalid(node)),
            };
            Ok(u64::from(value))
        }
    }

    fn finite_f32(&self, node: &CanonicalNode, value: f32) -> ExecutionResult<u64> {
        if value.is_finite() {
            Ok(u64::from(value.to_bits()))
        } else {
            Err(self.abort_fault(node, RuntimeFaultCode::NonFiniteFloat))
        }
    }

    fn finite_f64(&self, node: &CanonicalNode, value: f64) -> ExecutionResult<u64> {
        if value.is_finite() {
            Ok(value.to_bits())
        } else {
            Err(self.abort_fault(node, RuntimeFaultCode::NonFiniteFloat))
        }
    }

    fn call_expression(&mut self, node: &CanonicalNode) -> ExecutionResult<u64> {
        let callee = node
            .children
            .first()
            .and_then(|name| name.children.first())
            .and_then(|identifier| identifier.symbol);
        if let Some(callee) = callee {
            let activation = self.invoke_function(node, callee)?;
            let location = Location {
                key: StorageKey {
                    task: Some(self.task),
                    activation,
                    symbol: callee,
                },
                offset: 0,
                layout: self.model.layout_for_symbol(callee)?.id,
            };
            return match self.read(location)? {
                Value::Scalar(value) => Ok(value),
                Value::Aggregate { .. } => Err(self.invalid(node)),
            };
        }
        self.standard_call(node)
    }

    fn invoke_function(&mut self, node: &CanonicalNode, callee: SymbolId) -> ExecutionResult<u32> {
        let activation = callee.0;
        reset_frame(
            self.model,
            self.storage,
            self.limits,
            self.task,
            callee,
            activation,
        )?;
        let inputs = self.input_symbols(callee);
        if inputs.len() != node.children.len().saturating_sub(1) {
            return Err(self.invalid(node));
        }
        for (argument, input) in node.children.iter().skip(1).zip(inputs) {
            let layout = self.model.layout_for_symbol(input)?.id;
            let value = self.expression_for_layout(argument, layout)?;
            self.write(
                Location {
                    key: StorageKey {
                        task: Some(self.task),
                        activation,
                        symbol: input,
                    },
                    offset: 0,
                    layout,
                },
                value,
            )?;
        }
        self.checkpoint(node, |kind| {
            matches!(kind, CheckpointSiteKind::BeforePouCall { callee: value } if value == callee)
        })?;
        self.execute_pou(callee, activation)?;
        self.checkpoint(node, |kind| {
            matches!(kind, CheckpointSiteKind::AfterPouCall { callee: value } if value == callee)
        })?;
        Ok(activation)
    }

    fn input_symbols(&self, owner: SymbolId) -> Vec<SymbolId> {
        self.model
            .symbols
            .values()
            .filter(|symbol| {
                symbol.owner == Some(owner) && symbol.kind == SemanticSymbolKind::InputVariable
            })
            .map(|symbol| symbol.id)
            .collect()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "把冻结的 Preview 1.0 标准函数表集中保留，便于逐项审查"
    )]
    fn standard_call(&mut self, node: &CanonicalNode) -> ExecutionResult<u64> {
        let name = call_name(node).ok_or_else(|| self.invalid(node))?;
        let mut args = Vec::with_capacity(node.children.len().saturating_sub(1));
        for child in node.children.iter().skip(1) {
            args.push(self.scalar(child)?);
        }
        let semantic_type = value_type(node);
        match (name.as_str(), args.as_slice()) {
            ("CHECKED_ADD", [left, right]) => self.integer_call(
                node,
                IntegerOperation::Add,
                Some(IntegerArithmeticMode::Checked),
                *left,
                Some(*right),
            ),
            ("CHECKED_SUB", [left, right]) => self.integer_call(
                node,
                IntegerOperation::Subtract,
                Some(IntegerArithmeticMode::Checked),
                *left,
                Some(*right),
            ),
            ("CHECKED_MUL", [left, right]) => self.integer_call(
                node,
                IntegerOperation::Multiply,
                Some(IntegerArithmeticMode::Checked),
                *left,
                Some(*right),
            ),
            ("SATURATING_ADD", [left, right]) => self.integer_call(
                node,
                IntegerOperation::Add,
                Some(IntegerArithmeticMode::Saturating),
                *left,
                Some(*right),
            ),
            ("SATURATING_SUB", [left, right]) => self.integer_call(
                node,
                IntegerOperation::Subtract,
                Some(IntegerArithmeticMode::Saturating),
                *left,
                Some(*right),
            ),
            ("SATURATING_MUL", [left, right]) => self.integer_call(
                node,
                IntegerOperation::Multiply,
                Some(IntegerArithmeticMode::Saturating),
                *left,
                Some(*right),
            ),
            ("WRAPPING_ADD", [left, right]) => self.integer_call(
                node,
                IntegerOperation::Add,
                Some(IntegerArithmeticMode::Wrapping),
                *left,
                Some(*right),
            ),
            ("WRAPPING_SUB", [left, right]) => self.integer_call(
                node,
                IntegerOperation::Subtract,
                Some(IntegerArithmeticMode::Wrapping),
                *left,
                Some(*right),
            ),
            ("WRAPPING_MUL", [left, right]) => self.integer_call(
                node,
                IntegerOperation::Multiply,
                Some(IntegerArithmeticMode::Wrapping),
                *left,
                Some(*right),
            ),
            ("CHECKED_NEG", [value]) => self.integer_call(
                node,
                IntegerOperation::Negate,
                Some(IntegerArithmeticMode::Checked),
                *value,
                None,
            ),
            ("SATURATING_NEG", [value]) => self.integer_call(
                node,
                IntegerOperation::Negate,
                Some(IntegerArithmeticMode::Saturating),
                *value,
                None,
            ),
            ("WRAPPING_NEG", [value]) => self.integer_call(
                node,
                IntegerOperation::Negate,
                Some(IntegerArithmeticMode::Wrapping),
                *value,
                None,
            ),
            ("ABS", [value]) if !is_float(semantic_type) => {
                self.integer_call(node, IntegerOperation::Absolute, None, *value, None)
            }
            ("ABS", [value]) if matches!(semantic_type, Some(SemanticType::Real)) => {
                self.finite_f32(node, f32::from_bits(low_u32(*value)).abs())
            }
            ("ABS", [value]) if matches!(semantic_type, Some(SemanticType::Lreal)) => {
                self.finite_f64(node, f64::from_bits(*value).abs())
            }
            ("SQRT", [value]) if matches!(semantic_type, Some(SemanticType::Real)) => {
                self.finite_f32(node, f32::from_bits(low_u32(*value)).sqrt())
            }
            ("SQRT", [value]) if matches!(semantic_type, Some(SemanticType::Lreal)) => {
                self.finite_f64(node, f64::from_bits(*value).sqrt())
            }
            ("MIN" | "MAX", [left, right]) => self.minimum_maximum(node, &name, *left, *right),
            ("LIMIT", [value, low, high]) => self.limit(node, *value, *low, *high),
            (name, [value]) if name.starts_with("TO_") => {
                self.convert(node, *value, value_type(&node.children[1]), semantic_type)
            }
            _ => Err(self.invalid(node)),
        }
    }

    fn integer_call(
        &self,
        node: &CanonicalNode,
        operation: IntegerOperation,
        mode: Option<IntegerArithmeticMode>,
        left: u64,
        right: Option<u64>,
    ) -> ExecutionResult<u64> {
        integer_policy(value_type(node), operation, mode, left, right)
            .map_err(|fault| self.abort_fault(node, fault))
    }

    fn minimum_maximum(
        &self,
        node: &CanonicalNode,
        name: &str,
        left: u64,
        right: u64,
    ) -> ExecutionResult<u64> {
        let minimum = name == "MIN";
        let value_type = value_type(node);
        if matches!(value_type, Some(SemanticType::Real)) {
            let left_value = f32::from_bits(low_u32(left));
            let right_value = f32::from_bits(low_u32(right));
            if !left_value.is_finite() || !right_value.is_finite() {
                return Err(self.abort_fault(node, RuntimeFaultCode::NonFiniteFloat));
            }
            return Ok(
                if (minimum && right_value < left_value) || (!minimum && right_value > left_value) {
                    right
                } else {
                    left
                },
            );
        }
        if matches!(value_type, Some(SemanticType::Lreal)) {
            let left_value = f64::from_bits(left);
            let right_value = f64::from_bits(right);
            if !left_value.is_finite() || !right_value.is_finite() {
                return Err(self.abort_fault(node, RuntimeFaultCode::NonFiniteFloat));
            }
            return Ok(
                if (minimum && right_value < left_value) || (!minimum && right_value > left_value) {
                    right
                } else {
                    left
                },
            );
        }
        let select_right = if is_unsigned(value_type) {
            (minimum && right < left) || (!minimum && right > left)
        } else {
            let left = signed_value(left, integer_bits(value_type).unwrap_or(64));
            let right = signed_value(right, integer_bits(value_type).unwrap_or(64));
            (minimum && right < left) || (!minimum && right > left)
        };
        Ok(if select_right { right } else { left })
    }

    fn limit(&self, node: &CanonicalNode, value: u64, low: u64, high: u64) -> ExecutionResult<u64> {
        let value_type = value_type(node);
        if matches!(value_type, Some(SemanticType::Real)) {
            let value = f32::from_bits(low_u32(value));
            let low = f32::from_bits(low_u32(low));
            let high = f32::from_bits(low_u32(high));
            if !low.is_finite() || !high.is_finite() || low > high {
                return Err(self.abort_fault(node, RuntimeFaultCode::InvalidRuntimeRange));
            }
            return self.finite_f32(node, value.max(low).min(high));
        }
        if matches!(value_type, Some(SemanticType::Lreal)) {
            let value = f64::from_bits(value);
            let low = f64::from_bits(low);
            let high = f64::from_bits(high);
            if !low.is_finite() || !high.is_finite() || low > high {
                return Err(self.abort_fault(node, RuntimeFaultCode::InvalidRuntimeRange));
            }
            return self.finite_f64(node, value.max(low).min(high));
        }
        let bits = integer_bits(value_type).unwrap_or(64);
        let (below, above, invalid) = if is_unsigned(value_type) {
            (value < low, value > high, low > high)
        } else {
            let value_signed = signed_value(value, bits);
            let low_signed = signed_value(low, bits);
            let high_signed = signed_value(high, bits);
            (
                value_signed < low_signed,
                value_signed > high_signed,
                low_signed > high_signed,
            )
        };
        if invalid {
            return Err(self.abort_fault(node, RuntimeFaultCode::InvalidRuntimeRange));
        }
        Ok(if below {
            low
        } else if above {
            high
        } else {
            value
        })
    }

    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "这些显式 cast 正是 Preview 1.0 固定宽度数值转换，范围在 cast 前验证"
    )]
    fn convert(
        &self,
        node: &CanonicalNode,
        value: u64,
        source: Option<&SemanticType>,
        target: Option<&SemanticType>,
    ) -> ExecutionResult<u64> {
        match (is_float(source), is_float(target)) {
            (false, false) => {
                let mathematical =
                    integer_value(value, source).ok_or_else(|| self.invalid(node))?;
                let target_type = integer_type(target).ok_or_else(|| self.invalid(node))?;
                let encoded = encode_integer(mathematical, target_type)
                    .ok_or_else(|| self.abort_fault(node, RuntimeFaultCode::InvalidRuntimeRange))?;
                Ok(encoded)
            }
            (false, true) => {
                let mathematical =
                    integer_value(value, source).ok_or_else(|| self.invalid(node))?;
                if matches!(target, Some(SemanticType::Real)) {
                    self.finite_f32(node, mathematical as f32)
                } else {
                    self.finite_f64(node, mathematical as f64)
                }
            }
            (true, false) => {
                let target_type = integer_type(target).ok_or_else(|| self.invalid(node))?;
                let mathematical = if matches!(source, Some(SemanticType::Real)) {
                    let value = f32::from_bits(low_u32(value));
                    if !value.is_finite() {
                        return Err(self.abort_fault(node, RuntimeFaultCode::InvalidRuntimeRange));
                    }
                    f64::from(value.trunc())
                } else {
                    let value = f64::from_bits(value);
                    if !value.is_finite() {
                        return Err(self.abort_fault(node, RuntimeFaultCode::InvalidRuntimeRange));
                    }
                    value.trunc()
                };
                float_to_integer(mathematical, target_type)
                    .ok_or_else(|| self.abort_fault(node, RuntimeFaultCode::InvalidRuntimeRange))
            }
            (true, true) => match (source, target) {
                (Some(SemanticType::Real), Some(SemanticType::Lreal)) => {
                    self.finite_f64(node, f64::from(f32::from_bits(low_u32(value))))
                }
                (Some(SemanticType::Lreal), Some(SemanticType::Real)) => {
                    self.finite_f32(node, f64::from_bits(value) as f32)
                }
                _ => Ok(value),
            },
        }
    }

    fn aggregate(&mut self, node: &CanonicalNode, expected: FixedTypeId) -> ExecutionResult<Value> {
        match node.kind {
            AstNodeKind::Assignable => {
                let location = self.location(node)?;
                if self.model.layout(location.layout)?.id != self.model.layout(expected)?.id {
                    return Err(self.invalid(node));
                }
                self.read(location)
            }
            AstNodeKind::Literal => {
                let layout = self.model.layout(expected)?;
                let FixedTypeKind::String { wide, capacity, .. } = layout.kind else {
                    return Err(self.invalid(node));
                };
                let text = node.text.as_deref().ok_or_else(|| self.invalid(node))?;
                let (decoded, units) =
                    decode_string(text, wide).ok_or_else(|| self.invalid(node))?;
                if units > capacity {
                    return Err(self.invalid(node));
                }
                let mut bytes =
                    vec![0; usize::try_from(layout.size_bytes).map_err(|_| self.invalid(node))?];
                bytes[..4].copy_from_slice(
                    &u32::try_from(units)
                        .map_err(|_| self.invalid(node))?
                        .to_le_bytes(),
                );
                let mut payload = Vec::new();
                if wide {
                    for unit in decoded.encode_utf16() {
                        payload.extend_from_slice(&unit.to_le_bytes());
                    }
                } else {
                    payload.extend_from_slice(decoded.as_bytes());
                }
                let end = 4_usize
                    .checked_add(payload.len())
                    .ok_or_else(|| self.invalid(node))?;
                bytes
                    .get_mut(4..end)
                    .ok_or_else(|| self.invalid(node))?
                    .copy_from_slice(&payload);
                Ok(Value::Aggregate {
                    layout: expected,
                    bytes,
                })
            }
            AstNodeKind::QualifiedLiteral | AstNodeKind::ParenthesizedExpression => self.aggregate(
                node.children.last().ok_or_else(|| self.invalid(node))?,
                expected,
            ),
            AstNodeKind::CallExpression => {
                let callee = node
                    .children
                    .first()
                    .and_then(|name| name.children.first())
                    .and_then(|identifier| identifier.symbol);
                if let Some(callee) = callee {
                    let activation = self.invoke_function(node, callee)?;
                    return self.read(Location {
                        key: StorageKey {
                            task: Some(self.task),
                            activation,
                            symbol: callee,
                        },
                        offset: 0,
                        layout: expected,
                    });
                }
                if call_name(node).as_deref() == Some("CONCAT") {
                    return self.concat(node, expected);
                }
                Err(self.invalid(node))
            }
            _ => Err(self.invalid(node)),
        }
    }

    fn concat(&mut self, node: &CanonicalNode, layout: FixedTypeId) -> ExecutionResult<Value> {
        let [_, left, right] = node.children.as_slice() else {
            return Err(self.invalid(node));
        };
        let Value::Aggregate {
            bytes: left_bytes, ..
        } = self.aggregate(left, layout)?
        else {
            return Err(self.invalid(node));
        };
        let Value::Aggregate {
            bytes: right_bytes, ..
        } = self.aggregate(right, layout)?
        else {
            return Err(self.invalid(node));
        };
        let definition = self.model.layout(layout)?;
        let FixedTypeKind::String { wide, capacity, .. } = definition.kind else {
            return Err(self.invalid(node));
        };
        let left_length = string_length(&left_bytes, node)?;
        let right_length = string_length(&right_bytes, node)?;
        let total = left_length
            .checked_add(right_length)
            .ok_or_else(|| self.abort_fault(node, RuntimeFaultCode::StringCapacityExceeded))?;
        if u64::from(total) > capacity {
            return Err(self.abort_fault(node, RuntimeFaultCode::StringCapacityExceeded));
        }
        let unit_width = if wide { 2_usize } else { 1_usize };
        let left_payload = left_length as usize * unit_width;
        let right_payload = right_length as usize * unit_width;
        let mut bytes =
            vec![0; usize::try_from(definition.size_bytes).map_err(|_| self.invalid(node))?];
        bytes[..4].copy_from_slice(&total.to_le_bytes());
        bytes[4..4 + left_payload].copy_from_slice(&left_bytes[4..4 + left_payload]);
        bytes[4 + left_payload..4 + left_payload + right_payload]
            .copy_from_slice(&right_bytes[4..4 + right_payload]);
        Ok(Value::Aggregate { layout, bytes })
    }

    fn enumeration_literal(&self, node: &CanonicalNode) -> ExecutionResult<u64> {
        let symbol = node.symbol.ok_or_else(|| self.invalid(node))?;
        let declaration = self.model.symbol(symbol)?;
        let layout = self.model.layout_for_symbol(symbol)?;
        let FixedTypeKind::Enumeration { members } = &layout.kind else {
            return Err(self.invalid(node));
        };
        members
            .iter()
            .find(|member| member.name.eq_ignore_ascii_case(&declaration.name))
            .map(|member| u64::from(member.value.cast_unsigned()))
            .ok_or_else(|| self.invalid(node))
    }

    fn function_block_call(&mut self, node: &CanonicalNode) -> ExecutionResult<()> {
        let target = node.children.first().ok_or_else(|| self.invalid(node))?;
        let instance = root_symbol(target).ok_or_else(|| self.invalid(node))?;
        let callee = match self.model.symbol(instance)?.declared_type.as_ref() {
            Some(SemanticType::FunctionBlock { declaration }) => *declaration,
            _ => return Err(self.invalid(node)),
        };
        let activation = instance.0;
        self.ensure_function_block_storage(instance, callee, activation)?;
        reset_frame(
            self.model,
            self.storage,
            self.limits,
            self.task,
            callee,
            activation,
        )?;
        for argument in node.children.iter().skip(1) {
            let [name, value] = argument.children.as_slice() else {
                return Err(self.invalid(argument));
            };
            if argument.kind != AstNodeKind::InputArgument {
                continue;
            }
            let parameter =
                self.parameter_symbol(callee, name, SemanticSymbolKind::InputVariable)?;
            let layout = self.model.layout_for_symbol(parameter)?.id;
            let value = self.expression_for_layout(value, layout)?;
            self.write(
                Location {
                    key: StorageKey {
                        task: Some(self.task),
                        activation,
                        symbol: parameter,
                    },
                    offset: 0,
                    layout,
                },
                value,
            )?;
        }
        self.checkpoint(node, |kind| {
            matches!(kind, CheckpointSiteKind::BeforePouCall { callee: value } if value == callee)
        })?;
        self.execute_pou(callee, activation)?;
        self.checkpoint(node, |kind| {
            matches!(kind, CheckpointSiteKind::AfterPouCall { callee: value } if value == callee)
        })?;
        self.sync_function_block_storage(instance, callee, activation)?;
        for argument in node.children.iter().skip(1) {
            if argument.kind != AstNodeKind::OutputArgument {
                continue;
            }
            let [name, target] = argument.children.as_slice() else {
                return Err(self.invalid(argument));
            };
            let parameter =
                self.parameter_symbol(callee, name, SemanticSymbolKind::OutputVariable)?;
            let source = Location {
                key: StorageKey {
                    task: Some(self.task),
                    activation,
                    symbol: parameter,
                },
                offset: 0,
                layout: self.model.layout_for_symbol(parameter)?.id,
            };
            let value = self.read(source)?;
            let destination = self.location(target)?;
            self.write(destination, value)?;
        }
        Ok(())
    }

    fn ensure_function_block_storage(
        &mut self,
        instance: SymbolId,
        callee: SymbolId,
        activation: u32,
    ) -> ExecutionResult<()> {
        let layout = self.model.layout_for_symbol(instance)?;
        let FixedTypeKind::FunctionBlock { fields, .. } = &layout.kind else {
            return Err(ReferenceExecutionError::InvalidStorage(instance.0).into());
        };
        let instance_location = Location {
            key: StorageKey {
                task: Some(self.task),
                activation: self.activation,
                symbol: instance,
            },
            offset: 0,
            layout: layout.id,
        };
        let Value::Aggregate { bytes, .. } = self.read(instance_location)? else {
            return Err(ReferenceExecutionError::InvalidStorage(instance.0).into());
        };
        for field in fields {
            let symbol = field_symbol(self.model, callee, &field.name)?;
            let value_layout = self.model.layout(field.value_type)?;
            let field_bytes =
                slice_bytes(&bytes, field.offset_bytes, value_layout.size_bytes, symbol)?;
            self.storage.insert(
                StorageKey {
                    task: Some(self.task),
                    activation,
                    symbol,
                },
                StorageEntry {
                    role: StorageRole::Alias,
                    bytes: field_bytes,
                },
            );
        }
        enforce_storage(self.storage, self.limits)?;
        Ok(())
    }

    fn sync_function_block_storage(
        &mut self,
        instance: SymbolId,
        callee: SymbolId,
        activation: u32,
    ) -> ExecutionResult<()> {
        let layout = self.model.layout_for_symbol(instance)?;
        let FixedTypeKind::FunctionBlock { fields, .. } = &layout.kind else {
            return Err(ReferenceExecutionError::InvalidStorage(instance.0).into());
        };
        let mut updates = Vec::with_capacity(fields.len());
        for field in fields {
            let symbol = field_symbol(self.model, callee, &field.name)?;
            let entry = self
                .storage
                .get(&StorageKey {
                    task: Some(self.task),
                    activation,
                    symbol,
                })
                .ok_or(ReferenceExecutionError::InvalidStorage(symbol.0))?;
            updates.push((field.offset_bytes, entry.bytes.clone(), symbol));
        }
        let key = StorageKey {
            task: Some(self.task),
            activation: self.activation,
            symbol: instance,
        };
        let destination = self
            .storage
            .get_mut(&key)
            .ok_or(ReferenceExecutionError::InvalidStorage(instance.0))?;
        for (offset, bytes, symbol) in updates {
            let start = usize::try_from(offset)
                .map_err(|_| ReferenceExecutionError::InvalidStorage(symbol.0))?;
            let end = start
                .checked_add(bytes.len())
                .ok_or(ReferenceExecutionError::InvalidStorage(symbol.0))?;
            destination
                .bytes
                .get_mut(start..end)
                .ok_or(ReferenceExecutionError::InvalidStorage(symbol.0))?
                .copy_from_slice(&bytes);
        }
        Ok(())
    }

    fn parameter_symbol(
        &self,
        owner: SymbolId,
        name: &CanonicalNode,
        kind: SemanticSymbolKind,
    ) -> ExecutionResult<SymbolId> {
        let name_node = name;
        let name = name_node
            .text
            .as_deref()
            .ok_or_else(|| self.invalid(name_node))?;
        self.model
            .symbols
            .values()
            .find(|symbol| {
                symbol.owner == Some(owner)
                    && symbol.kind == kind
                    && symbol.canonical_name.eq_ignore_ascii_case(name)
            })
            .map(|symbol| symbol.id)
            .ok_or_else(|| self.invalid(name_node))
    }

    fn location(&mut self, node: &CanonicalNode) -> ExecutionResult<Location> {
        if node.kind != AstNodeKind::Assignable {
            return Err(self.invalid(node));
        }
        let qualified = node.children.first().ok_or_else(|| self.invalid(node))?;
        let root = qualified
            .children
            .first()
            .and_then(|identifier| identifier.symbol)
            .or(qualified.symbol)
            .ok_or_else(|| self.invalid(node))?;
        let mut layout = self.model.layout_for_symbol(root)?.id;
        let key = if self.model.symbol(root)?.kind == SemanticSymbolKind::GlobalVariable {
            global_key(root)
        } else {
            StorageKey {
                task: Some(self.task),
                activation: self.activation,
                symbol: root,
            }
        };
        let mut offset = 0_usize;
        for field in qualified.children.iter().skip(1) {
            let name = field.text.as_deref().ok_or_else(|| self.invalid(field))?;
            let field = self.model.field(layout, name)?;
            offset = add_offset(offset, field.offset_bytes, root)?;
            layout = field.value_type;
        }
        for suffix in node.children.iter().skip(1) {
            match suffix.kind {
                AstNodeKind::FieldSuffix => {
                    let name = suffix
                        .children
                        .first()
                        .and_then(|field| field.text.as_deref())
                        .ok_or_else(|| self.invalid(suffix))?;
                    let field = self.model.field(layout, name)?;
                    offset = add_offset(offset, field.offset_bytes, root)?;
                    layout = field.value_type;
                }
                AstNodeKind::IndexSuffix => {
                    let definition = self.model.layout(layout)?;
                    let FixedTypeKind::Array {
                        lower,
                        upper,
                        element_type,
                        element_stride_bytes,
                        ..
                    } = definition.kind
                    else {
                        return Err(self.invalid(suffix));
                    };
                    let index_node = suffix
                        .children
                        .first()
                        .ok_or_else(|| self.invalid(suffix))?;
                    let index = integer_value(self.scalar(index_node)?, value_type(index_node))
                        .ok_or_else(|| self.invalid(index_node))?;
                    if validate_array_index(index, lower, upper).is_err() {
                        return Err(
                            self.abort_fault(suffix, RuntimeFaultCode::ArrayIndexOutOfBounds)
                        );
                    }
                    let relative = index
                        .checked_sub(lower)
                        .and_then(|value| u64::try_from(value).ok())
                        .ok_or_else(|| self.invalid(suffix))?;
                    let dynamic = relative
                        .checked_mul(element_stride_bytes)
                        .ok_or_else(|| self.invalid(suffix))?;
                    offset = add_offset(offset, dynamic, root)?;
                    layout = element_type;
                }
                _ => return Err(self.invalid(suffix)),
            }
        }
        Ok(Location {
            key,
            offset,
            layout,
        })
    }

    fn read(&self, location: Location) -> ExecutionResult<Value> {
        let entry =
            self.storage
                .get(&location.key)
                .ok_or(ReferenceExecutionError::InvalidStorage(
                    location.key.symbol.0,
                ))?;
        let layout = self.model.layout(location.layout)?;
        let size = usize::try_from(layout.size_bytes)
            .map_err(|_| ReferenceExecutionError::InvalidStorage(location.key.symbol.0))?;
        let end =
            location
                .offset
                .checked_add(size)
                .ok_or(ReferenceExecutionError::InvalidStorage(
                    location.key.symbol.0,
                ))?;
        let bytes = entry.bytes.get(location.offset..end).ok_or(
            ReferenceExecutionError::InvalidStorage(location.key.symbol.0),
        )?;
        if is_scalar_layout(layout) {
            let mut encoded = [0_u8; 8];
            encoded[..size].copy_from_slice(bytes);
            Ok(Value::Scalar(u64::from_le_bytes(encoded)))
        } else {
            Ok(Value::Aggregate {
                layout: layout.id,
                bytes: bytes.to_vec(),
            })
        }
    }

    fn write(&mut self, location: Location, value: Value) -> ExecutionResult<()> {
        let layout = self.model.layout(location.layout)?;
        let size = usize::try_from(layout.size_bytes)
            .map_err(|_| ReferenceExecutionError::InvalidStorage(location.key.symbol.0))?;
        let source = match value {
            Value::Scalar(value) if is_scalar_layout(layout) => {
                value.to_le_bytes()[..size].to_vec()
            }
            Value::Aggregate {
                layout: source_layout,
                bytes,
            } if self.model.layout(source_layout)?.id == layout.id && bytes.len() == size => bytes,
            _ => return Err(ReferenceExecutionError::InvalidStorage(location.key.symbol.0).into()),
        };
        let entry =
            self.storage
                .get_mut(&location.key)
                .ok_or(ReferenceExecutionError::InvalidStorage(
                    location.key.symbol.0,
                ))?;
        let end =
            location
                .offset
                .checked_add(size)
                .ok_or(ReferenceExecutionError::InvalidStorage(
                    location.key.symbol.0,
                ))?;
        entry
            .bytes
            .get_mut(location.offset..end)
            .ok_or(ReferenceExecutionError::InvalidStorage(
                location.key.symbol.0,
            ))?
            .copy_from_slice(&source);
        Ok(())
    }

    fn checkpoint(
        &mut self,
        node: &CanonicalNode,
        selected: impl Fn(CheckpointSiteKind) -> bool,
    ) -> ExecutionResult<()> {
        let matching = self
            .model
            .checkpoints
            .get(&node.id)
            .into_iter()
            .flatten()
            .filter(|(_, kind)| selected(*kind))
            .map(|(site, _)| *site)
            .collect::<Vec<_>>();
        let [site] = matching.as_slice() else {
            return Err(self.invalid(node));
        };
        let actual = self.checkpoints.len().saturating_add(1);
        enforce(
            "checkpoints per cycle",
            actual,
            self.limits.max_checkpoints_per_cycle(),
        )?;
        self.checkpoints.push(*site);
        if self.stop_at_checkpoint == Some(*site) {
            Err(ExecutionAbort::CheckpointStop)
        } else {
            Ok(())
        }
    }

    fn abort_fault(&self, node: &CanonicalNode, code: RuntimeFaultCode) -> ExecutionAbort {
        match node.fault_site {
            Some(site)
                if self.model.ir.fault_sites.iter().any(|entry| {
                    entry.id == site && entry.possible_faults.binary_search(&code).is_ok()
                }) =>
            {
                ExecutionAbort::Fault(DifferentialFault { site, code })
            }
            _ => self.invalid(node),
        }
    }

    #[allow(
        clippy::unused_self,
        reason = "保留为 Execution 方法，使所有递归错误路径只携带当前 Canonical node"
    )]
    fn invalid(&self, node: &CanonicalNode) -> ExecutionAbort {
        ExecutionAbort::Invalid(invalid_node(node))
    }
}

enum FlowAbort {
    Return,
    Execution(ExecutionAbort),
}

impl From<ExecutionAbort> for FlowAbort {
    fn from(value: ExecutionAbort) -> Self {
        Self::Execution(value)
    }
}

fn scalar_literal(
    node: &CanonicalNode,
    value_type: Option<&SemanticType>,
) -> Result<u64, ReferenceExecutionError> {
    let text = node.text.as_deref().ok_or_else(|| invalid_node(node))?;
    if text.eq_ignore_ascii_case("TRUE") {
        return Ok(1);
    }
    if text.eq_ignore_ascii_case("FALSE") {
        return Ok(0);
    }
    if matches!(value_type, Some(SemanticType::Real)) {
        return text
            .parse::<f32>()
            .map(|value| u64::from(value.to_bits()))
            .map_err(|_| invalid_node(node));
    }
    if matches!(value_type, Some(SemanticType::Lreal)) {
        return text
            .parse::<f64>()
            .map(f64::to_bits)
            .map_err(|_| invalid_node(node));
    }
    let (radix, digits) = if let Some(value) = text.strip_prefix("16#") {
        (16, value)
    } else if let Some(value) = text.strip_prefix("2#") {
        (2, value)
    } else {
        (10, text)
    };
    if is_unsigned(value_type) {
        u64::from_str_radix(digits, radix).map_err(|_| invalid_node(node))
    } else {
        i64::from_str_radix(digits, radix)
            .map(i64::cast_unsigned)
            .map_err(|_| invalid_node(node))
    }
}

fn integer_policy(
    value_type: Option<&SemanticType>,
    operation: IntegerOperation,
    mode: Option<IntegerArithmeticMode>,
    left: u64,
    right: Option<u64>,
) -> Result<u64, RuntimeFaultCode> {
    let integer_type = integer_type(value_type).ok_or(RuntimeFaultCode::IntegerOverflow)?;
    let left = integer_value(left, value_type).ok_or(RuntimeFaultCode::IntegerOverflow)?;
    let right = match right {
        Some(value) => {
            Some(integer_value(value, value_type).ok_or(RuntimeFaultCode::IntegerOverflow)?)
        }
        None => None,
    };
    evaluate_integer_operation(integer_type, operation, mode, left, right)
        .map_err(|error| match error {
            IntegerArithmeticError::RuntimeFault(code) => code,
            _ => RuntimeFaultCode::IntegerOverflow,
        })
        .and_then(|value| {
            encode_integer(value, integer_type).ok_or(RuntimeFaultCode::IntegerOverflow)
        })
}

fn integer_value(value: u64, value_type: Option<&SemanticType>) -> Option<i128> {
    let bits = integer_bits(value_type)?;
    if is_unsigned(value_type) {
        Some(i128::from(normalize(value, value_type)))
    } else {
        Some(i128::from(signed_value(value, bits)))
    }
}

fn encode_integer(value: i128, value_type: IntegerType) -> Option<u64> {
    match value_type {
        IntegerType::Sint => i8::try_from(value)
            .ok()
            .map(|value| u64::from(value.cast_unsigned())),
        IntegerType::Int => i16::try_from(value)
            .ok()
            .map(|value| u64::from(value.cast_unsigned())),
        IntegerType::Dint => i32::try_from(value)
            .ok()
            .map(|value| u64::from(value.cast_unsigned())),
        IntegerType::Lint => i64::try_from(value).ok().map(i64::cast_unsigned),
        IntegerType::Usint => u8::try_from(value).ok().map(u64::from),
        IntegerType::Uint => u16::try_from(value).ok().map(u64::from),
        IntegerType::Udint => u32::try_from(value).ok().map(u64::from),
        IntegerType::Ulint => u64::try_from(value).ok(),
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "边界先按目标宽度验证，随后 cast 实现规范要求的向零截断"
)]
fn float_to_integer(value: f64, value_type: IntegerType) -> Option<u64> {
    let (minimum, upper_exclusive) = match value_type {
        IntegerType::Sint => (f64::from(i8::MIN), f64::from(i8::MAX) + 1.0),
        IntegerType::Int => (f64::from(i16::MIN), f64::from(i16::MAX) + 1.0),
        IntegerType::Dint => (f64::from(i32::MIN), f64::from(i32::MAX) + 1.0),
        IntegerType::Lint => (i64::MIN as f64, 2_f64.powi(63)),
        IntegerType::Usint => (0.0, f64::from(u8::MAX) + 1.0),
        IntegerType::Uint => (0.0, f64::from(u16::MAX) + 1.0),
        IntegerType::Udint => (0.0, f64::from(u32::MAX) + 1.0),
        IntegerType::Ulint => (0.0, 2_f64.powi(64)),
    };
    if !value.is_finite() || value < minimum || value >= upper_exclusive {
        return None;
    }
    let mathematical = if value_type_signed(value_type) {
        i128::from(value as i64)
    } else {
        i128::from(value as u64)
    };
    encode_integer(mathematical, value_type)
}

const fn value_type_signed(value_type: IntegerType) -> bool {
    matches!(
        value_type,
        IntegerType::Sint | IntegerType::Int | IntegerType::Dint | IntegerType::Lint
    )
}

fn integer_compare(
    operation: &str,
    left: u64,
    right: u64,
    value_type: Option<&SemanticType>,
) -> bool {
    if is_unsigned(value_type) || matches!(value_type, Some(SemanticType::Bool)) {
        match operation {
            "=" => left == right,
            "<>" => left != right,
            "<" => left < right,
            "<=" => left <= right,
            ">" => left > right,
            ">=" => left >= right,
            _ => false,
        }
    } else {
        let bits = integer_bits(value_type).unwrap_or(64);
        let left = signed_value(left, bits);
        let right = signed_value(right, bits);
        match operation {
            "=" => left == right,
            "<>" => left != right,
            "<" => left < right,
            "<=" => left <= right,
            ">" => left > right,
            ">=" => left >= right,
            _ => false,
        }
    }
}

fn signed_value(value: u64, bits: u8) -> i64 {
    if bits == 64 {
        value.cast_signed()
    } else {
        let shift = 64_u32 - u32::from(bits);
        (value << shift).cast_signed() >> shift
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "REAL 的 canonical representation 明确定义为 u64 低 32 位"
)]
const fn low_u32(value: u64) -> u32 {
    value as u32
}

fn normalize(value: u64, value_type: Option<&SemanticType>) -> u64 {
    let Some(bits) = integer_bits(value_type) else {
        return value;
    };
    if bits == 64 {
        return value;
    }
    let mask = (1_u64 << bits) - 1;
    value & mask
}

fn integer_type(value_type: Option<&SemanticType>) -> Option<IntegerType> {
    Some(match value_type? {
        SemanticType::Sint => IntegerType::Sint,
        SemanticType::Int => IntegerType::Int,
        SemanticType::Dint => IntegerType::Dint,
        SemanticType::Lint => IntegerType::Lint,
        SemanticType::Usint => IntegerType::Usint,
        SemanticType::Uint => IntegerType::Uint,
        SemanticType::Udint => IntegerType::Udint,
        SemanticType::Ulint => IntegerType::Ulint,
        _ => return None,
    })
}

fn integer_bits(value_type: Option<&SemanticType>) -> Option<u8> {
    Some(match value_type? {
        SemanticType::Bool | SemanticType::Sint | SemanticType::Usint => 8,
        SemanticType::Int | SemanticType::Uint => 16,
        SemanticType::Dint | SemanticType::Udint | SemanticType::Enumeration { .. } => 32,
        SemanticType::Lint | SemanticType::Ulint => 64,
        _ => return None,
    })
}

const fn is_unsigned(value_type: Option<&SemanticType>) -> bool {
    matches!(
        value_type,
        Some(
            SemanticType::Bool
                | SemanticType::Usint
                | SemanticType::Uint
                | SemanticType::Udint
                | SemanticType::Ulint
        )
    )
}

const fn is_float(value_type: Option<&SemanticType>) -> bool {
    matches!(value_type, Some(SemanticType::Real | SemanticType::Lreal))
}

fn is_scalar_layout(layout: &FixedTypeLayout) -> bool {
    matches!(
        layout.kind,
        FixedTypeKind::Scalar { .. } | FixedTypeKind::Enumeration { .. }
    )
}

fn scalar_layout_matches(kind: &FixedTypeKind, value_type: &SemanticType) -> bool {
    match (kind, value_type) {
        (FixedTypeKind::Scalar { name }, SemanticType::Bool) => name == "BOOL",
        (FixedTypeKind::Scalar { name }, SemanticType::Sint) => name == "SINT",
        (FixedTypeKind::Scalar { name }, SemanticType::Int) => name == "INT",
        (FixedTypeKind::Scalar { name }, SemanticType::Dint) => name == "DINT",
        (FixedTypeKind::Scalar { name }, SemanticType::Lint) => name == "LINT",
        (FixedTypeKind::Scalar { name }, SemanticType::Usint) => name == "USINT",
        (FixedTypeKind::Scalar { name }, SemanticType::Uint) => name == "UINT",
        (FixedTypeKind::Scalar { name }, SemanticType::Udint) => name == "UDINT",
        (FixedTypeKind::Scalar { name }, SemanticType::Ulint) => name == "ULINT",
        (FixedTypeKind::Scalar { name }, SemanticType::Real) => name == "REAL",
        (FixedTypeKind::Scalar { name }, SemanticType::Lreal) => name == "LREAL",
        (
            FixedTypeKind::String {
                wide: false,
                capacity,
                ..
            },
            SemanticType::String { capacity: expected },
        )
        | (
            FixedTypeKind::String {
                wide: true,
                capacity,
                ..
            },
            SemanticType::Wstring { capacity: expected },
        ) => expected.parse::<u64>().ok() == Some(*capacity),
        _ => false,
    }
}

fn value_type(node: &CanonicalNode) -> Option<&SemanticType> {
    node.value_type
        .as_ref()
        .or_else(|| node.children.iter().find_map(value_type))
}

fn root_symbol(node: &CanonicalNode) -> Option<SymbolId> {
    node.symbol
        .or_else(|| node.children.iter().find_map(root_symbol))
}

fn call_name(node: &CanonicalNode) -> Option<String> {
    node.children
        .first()?
        .children
        .first()?
        .text
        .as_ref()
        .map(|value| value.to_ascii_uppercase())
}

fn add_offset(current: usize, extra: u64, symbol: SymbolId) -> Result<usize, ExecutionAbort> {
    let extra =
        usize::try_from(extra).map_err(|_| ReferenceExecutionError::InvalidStorage(symbol.0))?;
    current
        .checked_add(extra)
        .ok_or_else(|| ReferenceExecutionError::InvalidStorage(symbol.0).into())
}

fn string_length(bytes: &[u8], node: &CanonicalNode) -> ExecutionResult<u32> {
    let raw: [u8; 4] = bytes
        .get(..4)
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| invalid_node(node))?;
    Ok(u32::from_le_bytes(raw))
}

const fn invalid_node(node: &CanonicalNode) -> ReferenceExecutionError {
    ReferenceExecutionError::InvalidNode {
        node: node.id.0,
        kind: node.kind,
    }
}
