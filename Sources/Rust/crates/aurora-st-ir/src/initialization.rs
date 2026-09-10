//! Canonical initialization byte-image construction for R1-06.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::diagnostic::make_diagnostic;
use crate::fault::{ConstantValue, evaluate_static_initializer};
use crate::{
    AddressSemanticModel, AnalysisInputError, CyclicWorkInputError, CyclicWorkLimits,
    CyclicWorkModel, Diagnostic, DiagnosticCode, FixedDataLimits, FixedFieldLayout,
    FixedInitializer, FixedSemanticModel, FixedTypeId, FixedTypeKind, SemanticSource, SourceSpan,
    SymbolId, TaskHandle, analyze_cyclic_work,
};

/// Mandatory final-instance initialization budgets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitializationLimits {
    per_task: u64,
    tasks_total: u64,
    globals_total: u64,
    frames_total: u64,
    all_total: u64,
}

impl InitializationLimits {
    /// Validates every mandatory finite initialization-image budget.
    ///
    /// # Errors
    ///
    /// Returns [`InitializationLimitError`] when any budget is zero.
    pub const fn new(
        max_state_bytes_per_task: u64,
        max_total_task_state_bytes: u64,
        max_total_global_bytes: u64,
        max_total_frame_bytes: u64,
        max_total_initialization_bytes: u64,
    ) -> Result<Self, InitializationLimitError> {
        if max_state_bytes_per_task == 0 {
            return Err(InitializationLimitError::ZeroTaskBytes);
        }
        if max_total_task_state_bytes == 0 {
            return Err(InitializationLimitError::ZeroTotalTaskBytes);
        }
        if max_total_global_bytes == 0 {
            return Err(InitializationLimitError::ZeroGlobalBytes);
        }
        if max_total_frame_bytes == 0 {
            return Err(InitializationLimitError::ZeroFrameBytes);
        }
        if max_total_initialization_bytes == 0 {
            return Err(InitializationLimitError::ZeroTotalBytes);
        }
        Ok(Self {
            per_task: max_state_bytes_per_task,
            tasks_total: max_total_task_state_bytes,
            globals_total: max_total_global_bytes,
            frames_total: max_total_frame_bytes,
            all_total: max_total_initialization_bytes,
        })
    }

    /// Maximum canonical persistent bytes for one task Program instance.
    #[must_use]
    pub const fn max_state_bytes_per_task(self) -> u64 {
        self.per_task
    }

    /// Maximum sum of all task Program instances.
    #[must_use]
    pub const fn max_total_task_state_bytes(self) -> u64 {
        self.tasks_total
    }

    /// Maximum sum of all global value templates.
    #[must_use]
    pub const fn max_total_global_bytes(self) -> u64 {
        self.globals_total
    }

    /// Maximum sum of all POU invocation-frame templates.
    #[must_use]
    pub const fn max_total_frame_bytes(self) -> u64 {
        self.frames_total
    }

    /// Maximum sum of every published initialization byte image.
    #[must_use]
    pub const fn max_total_initialization_bytes(self) -> u64 {
        self.all_total
    }
}

/// Invalid zero-valued initialization budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum InitializationLimitError {
    /// Per-task state budget is zero.
    #[error("max_state_bytes_per_task must be non-zero")]
    ZeroTaskBytes,
    /// Aggregate task-state budget is zero.
    #[error("max_total_task_state_bytes must be non-zero")]
    ZeroTotalTaskBytes,
    /// Aggregate global template budget is zero.
    #[error("max_total_global_bytes must be non-zero")]
    ZeroGlobalBytes,
    /// Aggregate invocation-frame template budget is zero.
    #[error("max_total_frame_bytes must be non-zero")]
    ZeroFrameBytes,
    /// Aggregate publication budget is zero.
    #[error("max_total_initialization_bytes must be non-zero")]
    ZeroTotalBytes,
}

/// Initial value bytes for one address-backed global.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GlobalInitializationImage {
    /// Global declaration identity.
    pub global: SymbolId,
    /// Exact fixed-layout bytes; padding and unused string payload are zero.
    pub bytes: Vec<u8>,
}

/// Initial persistent state/output bytes for one concrete task Program instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskInitializationImage {
    /// Scheduler task identity.
    pub task: TaskHandle,
    /// Instantiated Program declaration.
    pub program: SymbolId,
    /// Independent initial bytes owned by this task.
    pub bytes: Vec<u8>,
}

/// Initial bytes reconstructed for every invocation of one POU frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FrameInitializationImage {
    /// Function, Function Block, or Program declaration.
    pub pou: SymbolId,
    /// Canonical frame template bytes.
    pub bytes: Vec<u8>,
}

/// Complete deterministic initialization publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InitializationModel {
    /// One image per fixed global, in Global symbol order.
    pub globals: Vec<GlobalInitializationImage>,
    /// One independent image per Task binding, in Task-handle order.
    pub tasks: Vec<TaskInitializationImage>,
    /// One frame template per POU, in POU symbol order.
    pub frames: Vec<FrameInitializationImage>,
    /// Exact sum of task Program state bytes.
    pub total_task_state_bytes: u64,
    /// Exact sum of global template bytes.
    pub total_global_bytes: u64,
    /// Exact sum of invocation-frame template bytes.
    pub total_frame_bytes: u64,
    /// Exact sum of every byte vector above.
    pub total_initialization_bytes: u64,
}

/// Atomic initialization result. A budget failure publishes no partial byte image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitializationOutput {
    /// Complete images only when every model and byte boundary passes.
    pub model: Option<InitializationModel>,
    /// Exactly one `ST3005` for the first canonical budget crossing.
    pub diagnostics: Vec<Diagnostic>,
}

/// Caller/model corruption that cannot be represented as a new source diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InitializationInputError {
    /// Revalidation of the accepted work model failed.
    #[error(transparent)]
    WorkAnalysis(#[from] CyclicWorkInputError),
    /// Sources or upstream models do not reproduce the accepted work proof.
    #[error("sources do not match the accepted R1-06 work model")]
    WorkModelMismatch,
    /// Static initializer evaluation found an impossible accepted AST/model pair.
    #[error(transparent)]
    StaticEvaluation(#[from] AnalysisInputError),
    /// A fixed type ID is absent, duplicated, or not representable on the build host.
    #[error("invalid fixed layout {0}")]
    InvalidLayout(u32),
    /// A Program layout is absent or duplicated.
    #[error("invalid Program layout {0}")]
    InvalidProgram(u32),
    /// A global or frame entry is duplicated.
    #[error("duplicate initialization owner {kind} {id}")]
    DuplicateOwner {
        /// Owner category.
        kind: &'static str,
        /// Symbol/task value.
        id: u32,
    },
    /// An accepted initializer cannot be represented by its fixed destination layout.
    #[error("initializer in `{source_path}` bytes {start}..{end} does not match layout {layout}")]
    InvalidInitializer {
        /// Source containing the explicit initializer, or the owning layout for an implicit one.
        source_path: String,
        /// Inclusive byte offset.
        start: u32,
        /// Exclusive byte offset.
        end: u32,
        /// Destination fixed type.
        layout: u32,
    },
    /// Checked size/offset arithmetic exceeded `u64`.
    #[error("initialization size arithmetic overflow")]
    SizeOverflow,
}

/// Builds all reset/invocation initialization images after static work proof succeeds.
///
/// All byte vectors are preflighted with checked `u64` arithmetic before the first allocation.
/// Destinations start fully zeroed, then only declared values are written, so structure padding,
/// array stride padding, unused string payload, and tail padding remain zero. Each accepted Task
/// binding owns exactly one independent Program image; loops and call counts never multiply images.
///
/// # Errors
///
/// Returns [`InitializationInputError`] for inconsistent accepted models, layouts, or initializer
/// expressions. Resource limits produce one `ST3005` and no partial model.
pub fn build_initialization_images(
    sources: &[SemanticSource<'_>],
    address_model: &AddressSemanticModel,
    work_model: &CyclicWorkModel,
    fixed_limits: FixedDataLimits,
    work_limits: CyclicWorkLimits,
    limits: InitializationLimits,
) -> Result<InitializationOutput, InitializationInputError> {
    let revalidated = analyze_cyclic_work(sources, address_model, fixed_limits, work_limits)?;
    if revalidated.model.as_ref() != Some(work_model) || !revalidated.diagnostics.is_empty() {
        return Err(InitializationInputError::WorkModelMismatch);
    }
    Initializer::new(sources, address_model, work_model, limits)?.run()
}

#[derive(Clone, Copy)]
enum BudgetKind {
    Global,
    Task,
    Frame,
}

#[derive(Clone)]
struct BudgetItem {
    kind: BudgetKind,
    source_path: String,
    span: SourceSpan,
    bytes: u64,
}

#[derive(Default)]
struct Totals {
    global: u64,
    task: u64,
    frame: u64,
    all: u64,
}

struct Initializer<'a> {
    sources: &'a [SemanticSource<'a>],
    source_text: BTreeMap<String, &'a str>,
    fixed: &'a FixedSemanticModel,
    work: &'a CyclicWorkModel,
    limits: InitializationLimits,
    types: BTreeMap<FixedTypeId, &'a crate::FixedTypeLayout>,
    programs: BTreeMap<SymbolId, &'a crate::StaticProgramLayout>,
}

impl<'a> Initializer<'a> {
    fn new(
        sources: &'a [SemanticSource<'a>],
        address: &'a AddressSemanticModel,
        work: &'a CyclicWorkModel,
        limits: InitializationLimits,
    ) -> Result<Self, InitializationInputError> {
        let fixed = &address.faults.fixed;
        let mut types = BTreeMap::new();
        for layout in &fixed.types {
            if types.insert(layout.id, layout).is_some() {
                return Err(InitializationInputError::InvalidLayout(layout.id.0));
            }
        }
        let mut programs = BTreeMap::new();
        for program in &fixed.programs {
            if programs.insert(program.program, program).is_some() {
                return Err(InitializationInputError::InvalidProgram(program.program.0));
            }
        }
        Ok(Self {
            sources,
            source_text: sources
                .iter()
                .map(|source| (source.ast.source_path.clone(), source.source))
                .collect(),
            fixed,
            work,
            limits,
            types,
            programs,
        })
    }

    fn run(&self) -> Result<InitializationOutput, InitializationInputError> {
        let (items, totals) = self.preflight()?;
        if let Some(item) = self.first_budget_crossing(&items)? {
            let source = self
                .source_text
                .get(&item.source_path)
                .copied()
                .unwrap_or("");
            return Ok(InitializationOutput {
                model: None,
                diagnostics: vec![make_diagnostic(
                    &item.source_path,
                    source,
                    DiagnosticCode::ResourceBudgetExceeded,
                    item.span,
                )],
            });
        }

        let mut globals = Vec::with_capacity(self.fixed.globals.len());
        let mut global_owners = BTreeSet::new();
        for global in &self.fixed.globals {
            if !global_owners.insert(global.global) {
                return Err(InitializationInputError::DuplicateOwner {
                    kind: "global",
                    id: global.global.0,
                });
            }
            globals.push(GlobalInitializationImage {
                global: global.global,
                bytes: self.image(
                    global.value_type,
                    &global.initializer,
                    &global.source_path,
                    global.span,
                )?,
            });
        }

        let mut tasks = Vec::with_capacity(self.work.tasks.len());
        let mut task_owners = BTreeSet::new();
        for task in &self.work.tasks {
            if !task_owners.insert(task.task) {
                return Err(InitializationInputError::DuplicateOwner {
                    kind: "task",
                    id: task.task.0,
                });
            }
            let program = self
                .programs
                .get(&task.program)
                .copied()
                .ok_or(InitializationInputError::InvalidProgram(task.program.0))?;
            let mut bytes = zeroed(program.size_bytes, task.program.0)?;
            self.write_fields(
                &program.fields,
                &program.source_path,
                &mut bytes,
                program.span,
            )?;
            tasks.push(TaskInitializationImage {
                task: task.task,
                program: task.program,
                bytes,
            });
        }

        let mut frames = Vec::with_capacity(self.fixed.invocation_frames.len());
        let mut frame_owners = BTreeSet::new();
        for frame in &self.fixed.invocation_frames {
            if !frame_owners.insert(frame.pou) {
                return Err(InitializationInputError::DuplicateOwner {
                    kind: "frame",
                    id: frame.pou.0,
                });
            }
            let mut bytes = zeroed(frame.size_bytes, frame.pou.0)?;
            self.write_fields(&frame.fields, &frame.source_path, &mut bytes, frame.span)?;
            frames.push(FrameInitializationImage {
                pou: frame.pou,
                bytes,
            });
        }

        Ok(InitializationOutput {
            model: Some(InitializationModel {
                globals,
                tasks,
                frames,
                total_task_state_bytes: totals.task,
                total_global_bytes: totals.global,
                total_frame_bytes: totals.frame,
                total_initialization_bytes: totals.all,
            }),
            diagnostics: Vec::new(),
        })
    }

    fn preflight(&self) -> Result<(Vec<BudgetItem>, Totals), InitializationInputError> {
        let mut items = Vec::new();
        for global in &self.fixed.globals {
            items.push(BudgetItem {
                kind: BudgetKind::Global,
                source_path: global.source_path.clone(),
                span: global.span,
                bytes: self.layout(global.value_type)?.size_bytes,
            });
        }
        for task in &self.work.tasks {
            let program = self
                .programs
                .get(&task.program)
                .copied()
                .ok_or(InitializationInputError::InvalidProgram(task.program.0))?;
            items.push(BudgetItem {
                kind: BudgetKind::Task,
                source_path: program.source_path.clone(),
                span: program.span,
                bytes: program.size_bytes,
            });
        }
        for frame in &self.fixed.invocation_frames {
            items.push(BudgetItem {
                kind: BudgetKind::Frame,
                source_path: frame.source_path.clone(),
                span: frame.span,
                bytes: frame.size_bytes,
            });
        }
        let mut totals = Totals::default();
        for item in &items {
            totals.all = checked_add(totals.all, item.bytes)?;
            match item.kind {
                BudgetKind::Global => totals.global = checked_add(totals.global, item.bytes)?,
                BudgetKind::Task => totals.task = checked_add(totals.task, item.bytes)?,
                BudgetKind::Frame => totals.frame = checked_add(totals.frame, item.bytes)?,
            }
        }
        Ok((items, totals))
    }

    fn first_budget_crossing(
        &self,
        items: &[BudgetItem],
    ) -> Result<Option<BudgetItem>, InitializationInputError> {
        let mut totals = Totals::default();
        for item in items {
            totals.all = checked_add(totals.all, item.bytes)?;
            let category_exceeded = match item.kind {
                BudgetKind::Global => {
                    totals.global = checked_add(totals.global, item.bytes)?;
                    totals.global > self.limits.max_total_global_bytes()
                }
                BudgetKind::Task => {
                    totals.task = checked_add(totals.task, item.bytes)?;
                    item.bytes > self.limits.max_state_bytes_per_task()
                        || totals.task > self.limits.max_total_task_state_bytes()
                }
                BudgetKind::Frame => {
                    totals.frame = checked_add(totals.frame, item.bytes)?;
                    totals.frame > self.limits.max_total_frame_bytes()
                }
            };
            if category_exceeded || totals.all > self.limits.max_total_initialization_bytes() {
                return Ok(Some(item.clone()));
            }
        }
        Ok(None)
    }

    fn image(
        &self,
        value_type: FixedTypeId,
        initializer: &FixedInitializer,
        source_path: &str,
        span: SourceSpan,
    ) -> Result<Vec<u8>, InitializationInputError> {
        let mut bytes = zeroed(self.layout(value_type)?.size_bytes, value_type.0)?;
        self.write_initializer(value_type, initializer, source_path, span, &mut bytes, 0)?;
        Ok(bytes)
    }

    fn write_fields(
        &self,
        fields: &[FixedFieldLayout],
        source_path: &str,
        destination: &mut [u8],
        owner_span: SourceSpan,
    ) -> Result<(), InitializationInputError> {
        for field in fields {
            let size = self.layout(field.value_type)?.size_bytes;
            let end = checked_add(field.offset_bytes, size)?;
            let start = host_offset(field.offset_bytes, field.value_type)?;
            let end = host_offset(end, field.value_type)?;
            let target = destination.get_mut(start..end).ok_or_else(|| {
                Self::invalid_initializer(source_path, owner_span, field.value_type)
            })?;
            self.write_initializer(
                field.value_type,
                &field.initializer,
                source_path,
                field.span,
                target,
                0,
            )?;
        }
        Ok(())
    }

    fn write_initializer(
        &self,
        value_type: FixedTypeId,
        initializer: &FixedInitializer,
        source_path: &str,
        span: SourceSpan,
        destination: &mut [u8],
        depth: usize,
    ) -> Result<(), InitializationInputError> {
        if depth > self.types.len() {
            return Err(InitializationInputError::InvalidLayout(value_type.0));
        }
        match initializer {
            FixedInitializer::Zero => {
                let layout = self.resolved_layout(value_type, depth)?;
                if matches!(layout.kind, FixedTypeKind::Scalar { .. }) {
                    Ok(())
                } else {
                    Err(Self::invalid_initializer(source_path, span, value_type))
                }
            }
            FixedInitializer::EmptyString => {
                let layout = self.resolved_layout(value_type, depth)?;
                if matches!(layout.kind, FixedTypeKind::String { .. }) {
                    Ok(())
                } else {
                    Err(Self::invalid_initializer(source_path, span, value_type))
                }
            }
            FixedInitializer::FirstEnumerationMember { name, value } => {
                let layout = self.resolved_layout(value_type, depth)?;
                let FixedTypeKind::Enumeration { members } = &layout.kind else {
                    return Err(Self::invalid_initializer(source_path, span, value_type));
                };
                if !members
                    .first()
                    .is_some_and(|member| member.name == *name && member.value == *value)
                {
                    return Err(Self::invalid_initializer(source_path, span, value_type));
                }
                Self::write_bytes(
                    destination,
                    &value.to_le_bytes(),
                    source_path,
                    span,
                    value_type,
                )
            }
            FixedInitializer::ExplicitExpression {
                source_path: expression_path,
                span: expression_span,
            } => {
                let value = evaluate_static_initializer(
                    self.sources,
                    self.fixed,
                    expression_path,
                    *expression_span,
                )?;
                self.write_constant(
                    value_type,
                    &value,
                    expression_path,
                    *expression_span,
                    destination,
                )
            }
            FixedInitializer::Aggregate => {
                self.write_aggregate(value_type, source_path, span, destination, depth)
            }
        }
    }

    fn write_aggregate(
        &self,
        value_type: FixedTypeId,
        source_path: &str,
        span: SourceSpan,
        destination: &mut [u8],
        depth: usize,
    ) -> Result<(), InitializationInputError> {
        let layout = self.resolved_layout(value_type, depth)?;
        match &layout.kind {
            FixedTypeKind::Array {
                element_count,
                element_type,
                element_stride_bytes,
                ..
            } => {
                for index in 0..*element_count {
                    let offset = index
                        .checked_mul(*element_stride_bytes)
                        .ok_or(InitializationInputError::SizeOverflow)?;
                    let element = self.layout(*element_type)?;
                    let end = checked_add(offset, element.size_bytes)?;
                    let start = host_offset(offset, *element_type)?;
                    let end = host_offset(end, *element_type)?;
                    let target = destination
                        .get_mut(start..end)
                        .ok_or_else(|| Self::invalid_initializer(source_path, span, value_type))?;
                    self.write_initializer(
                        *element_type,
                        &element.default_initializer,
                        &element.source_path,
                        element.span,
                        target,
                        depth + 1,
                    )?;
                }
                Ok(())
            }
            FixedTypeKind::Structure { fields } | FixedTypeKind::FunctionBlock { fields, .. } => {
                self.write_fields(fields, &layout.source_path, destination, layout.span)
            }
            FixedTypeKind::Alias { target } => {
                let target_layout = self.layout(*target)?;
                self.write_initializer(
                    *target,
                    &target_layout.default_initializer,
                    &target_layout.source_path,
                    target_layout.span,
                    destination,
                    depth + 1,
                )
            }
            FixedTypeKind::Scalar { .. }
            | FixedTypeKind::String { .. }
            | FixedTypeKind::Enumeration { .. } => {
                Err(Self::invalid_initializer(source_path, span, value_type))
            }
        }
    }

    fn write_constant(
        &self,
        value_type: FixedTypeId,
        value: &ConstantValue,
        source_path: &str,
        span: SourceSpan,
        destination: &mut [u8],
    ) -> Result<(), InitializationInputError> {
        let layout = self.resolved_layout(value_type, 0)?;
        match (&layout.kind, value) {
            (FixedTypeKind::Scalar { name }, ConstantValue::Bool(value)) if name == "BOOL" => {
                Self::write_bytes(
                    destination,
                    &[u8::from(*value)],
                    source_path,
                    span,
                    value_type,
                )
            }
            (FixedTypeKind::Scalar { name }, ConstantValue::Integer(value)) => {
                let bytes = integer_bytes(name, *value)
                    .ok_or_else(|| Self::invalid_initializer(source_path, span, value_type))?;
                Self::write_bytes(destination, &bytes, source_path, span, value_type)
            }
            (FixedTypeKind::Scalar { name }, ConstantValue::Real(bits)) if name == "REAL" => {
                Self::write_bytes(
                    destination,
                    &bits.to_le_bytes(),
                    source_path,
                    span,
                    value_type,
                )
            }
            (FixedTypeKind::Scalar { name }, ConstantValue::Lreal(bits)) if name == "LREAL" => {
                Self::write_bytes(
                    destination,
                    &bits.to_le_bytes(),
                    source_path,
                    span,
                    value_type,
                )
            }
            (
                FixedTypeKind::String { wide, capacity, .. },
                ConstantValue::String {
                    wide: value_wide,
                    value,
                    units,
                },
            ) if wide == value_wide && units <= capacity => {
                let length = u32::try_from(*units)
                    .map_err(|_| Self::invalid_initializer(source_path, span, value_type))?;
                let prefix = destination
                    .get_mut(..4)
                    .ok_or_else(|| Self::invalid_initializer(source_path, span, value_type))?;
                prefix.copy_from_slice(&length.to_le_bytes());
                let mut payload = Vec::new();
                if *wide {
                    for unit in value.encode_utf16() {
                        payload.extend_from_slice(&unit.to_le_bytes());
                    }
                } else {
                    payload.extend_from_slice(value.as_bytes());
                }
                let end = 4_usize
                    .checked_add(payload.len())
                    .ok_or(InitializationInputError::SizeOverflow)?;
                let target = destination
                    .get_mut(4..end)
                    .ok_or_else(|| Self::invalid_initializer(source_path, span, value_type))?;
                target.copy_from_slice(&payload);
                Ok(())
            }
            (FixedTypeKind::Enumeration { .. }, ConstantValue::Integer(value)) => {
                let value = i32::try_from(*value)
                    .map_err(|_| Self::invalid_initializer(source_path, span, value_type))?;
                Self::write_bytes(
                    destination,
                    &value.to_le_bytes(),
                    source_path,
                    span,
                    value_type,
                )
            }
            _ => Err(Self::invalid_initializer(source_path, span, value_type)),
        }
    }

    fn write_bytes(
        destination: &mut [u8],
        bytes: &[u8],
        source_path: &str,
        span: SourceSpan,
        value_type: FixedTypeId,
    ) -> Result<(), InitializationInputError> {
        if destination.len() != bytes.len() {
            return Err(Self::invalid_initializer(source_path, span, value_type));
        }
        destination.copy_from_slice(bytes);
        Ok(())
    }

    fn resolved_layout(
        &self,
        mut value_type: FixedTypeId,
        depth: usize,
    ) -> Result<&'a crate::FixedTypeLayout, InitializationInputError> {
        for _ in depth..=self.types.len() {
            let layout = self.layout(value_type)?;
            if let FixedTypeKind::Alias { target } = layout.kind {
                value_type = target;
            } else {
                return Ok(layout);
            }
        }
        Err(InitializationInputError::InvalidLayout(value_type.0))
    }

    fn layout(
        &self,
        value_type: FixedTypeId,
    ) -> Result<&'a crate::FixedTypeLayout, InitializationInputError> {
        self.types
            .get(&value_type)
            .copied()
            .ok_or(InitializationInputError::InvalidLayout(value_type.0))
    }

    fn invalid_initializer(
        source_path: &str,
        span: SourceSpan,
        layout: FixedTypeId,
    ) -> InitializationInputError {
        InitializationInputError::InvalidInitializer {
            source_path: source_path.to_owned(),
            start: span.start,
            end: span.end,
            layout: layout.0,
        }
    }
}

fn checked_add(left: u64, right: u64) -> Result<u64, InitializationInputError> {
    left.checked_add(right)
        .ok_or(InitializationInputError::SizeOverflow)
}

fn host_offset(value: u64, layout: FixedTypeId) -> Result<usize, InitializationInputError> {
    usize::try_from(value).map_err(|_| InitializationInputError::InvalidLayout(layout.0))
}

fn zeroed(size: u64, layout: u32) -> Result<Vec<u8>, InitializationInputError> {
    let size =
        usize::try_from(size).map_err(|_| InitializationInputError::InvalidLayout(layout))?;
    Ok(vec![0; size])
}

fn integer_bytes(name: &str, value: i128) -> Option<Vec<u8>> {
    Some(match name {
        "SINT" => i8::try_from(value).ok()?.to_le_bytes().to_vec(),
        "INT" => i16::try_from(value).ok()?.to_le_bytes().to_vec(),
        "DINT" => i32::try_from(value).ok()?.to_le_bytes().to_vec(),
        "LINT" => i64::try_from(value).ok()?.to_le_bytes().to_vec(),
        "USINT" => u8::try_from(value).ok()?.to_le_bytes().to_vec(),
        "UINT" => u16::try_from(value).ok()?.to_le_bytes().to_vec(),
        "UDINT" => u32::try_from(value).ok()?.to_le_bytes().to_vec(),
        "ULINT" => u64::try_from(value).ok()?.to_le_bytes().to_vec(),
        _ => return None,
    })
}
