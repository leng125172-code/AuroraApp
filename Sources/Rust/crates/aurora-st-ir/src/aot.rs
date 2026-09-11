//! Deterministic Linux x64 AOT object generation for Canonical ST IR.
//!
//! This module is compiler-host-only. Generated functions use a fixed System V C ABI and only
//! reference bounded Runtime callbacks; they never perform discovery, allocation, I/O, logging,
//! or physical-device access.

use std::collections::{BTreeMap, BTreeSet};

use cranelift_codegen::Context;
use cranelift_codegen::ir::immediates::{Ieee32, Ieee64};
use cranelift_codegen::ir::{
    AbiParam, BlockArg, Function, InstBuilder, Signature, SourceLoc, StackSlot, StackSlotData,
    StackSlotKind, UserFuncName, types,
};
use cranelift_codegen::isa;
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module, default_libcall_names};
use cranelift_object::{ObjectBuilder, ObjectModule};
use object::{
    Architecture, BinaryFormat, Endianness, Object, ObjectKind, ObjectSection, ObjectSymbol,
    RelocationTarget,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use target_lexicon::Triple;
use thiserror::Error;

use crate::checkpoint::CheckpointPlanner;
use crate::{
    AstNodeKind, CanonicalIrVersion, CanonicalNode, CanonicalNodeId, CanonicalPou,
    CanonicalPouKind, CanonicalSourceMap, CanonicalSourceMapVersion, CanonicalStIr, CheckpointPlan,
    CheckpointPlanLimits, CheckpointPlanVersion, CheckpointSiteId, CheckpointSiteKind,
    FixedFieldLayout, FixedTypeId, FixedTypeKind, FixedTypeLayout, RuntimeFaultCode,
    SemanticSymbol, SemanticSymbolKind, SemanticType, SymbolId, TaskHandle,
};

/// Major version of the static AOT/Runtime ABI.
pub const AOT_ABI_MAJOR: u16 = 1;
/// Minor version of the static AOT/Runtime ABI.
pub const AOT_ABI_MINOR: u16 = 0;

const TARGET_TRIPLE: &str = "x86_64-unknown-linux-gnu";
const INACTIVE_ACTIVATION: i64 = u32::MAX as i64;
const STATUS_COMPLETED: i64 = 0;
const STATUS_CHECKPOINT_STOP: i64 = 1;
const STATUS_FAULTED: i64 = 2;

const READ_BITS: &str = "aurora_st_read_bits_v1";
const WRITE_BITS: &str = "aurora_st_write_bits_v1";
const CHECKPOINT: &str = "aurora_st_checkpoint_v1";
const REPORT_FAULT: &str = "aurora_st_report_fault_v1";
const RESET_FRAME: &str = "aurora_st_reset_frame_v1";
const CONCAT_STRING: &str = "aurora_st_concat_string_v1";

/// Exact supported AOT target and ABI selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AotTarget {
    /// Exact target triple; only `x86_64-unknown-linux-gnu` is accepted.
    pub target_triple: &'static str,
    /// Reader-incompatible Runtime ABI version.
    pub abi_major: u16,
    /// Backward-compatible Runtime ABI version.
    pub abi_minor: u16,
}

impl AotTarget {
    /// Returns the only R1-06 target profile accepted by this backend.
    #[must_use]
    pub const fn linux_x64_v1() -> Self {
        Self {
            target_triple: TARGET_TRIPLE,
            abi_major: AOT_ABI_MAJOR,
            abi_minor: AOT_ABI_MINOR,
        }
    }
}

/// Mandatory finite host-side AOT construction and publication capacities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AotLimits {
    functions: usize,
    function_bytes: usize,
    object_bytes: usize,
    relocations: usize,
    native_ranges: usize,
    transient_stack_bytes: usize,
}

impl AotLimits {
    /// Validates every mandatory AOT capacity.
    ///
    /// # Errors
    ///
    /// Returns [`AotLimitError`] when any capacity is zero.
    pub const fn new(
        max_functions: usize,
        max_function_bytes: usize,
        max_object_bytes: usize,
        max_relocations: usize,
        max_native_ranges: usize,
        max_transient_stack_bytes: usize,
    ) -> Result<Self, AotLimitError> {
        if max_functions == 0 {
            return Err(AotLimitError::ZeroFunctions);
        }
        if max_function_bytes == 0 {
            return Err(AotLimitError::ZeroFunctionBytes);
        }
        if max_object_bytes == 0 {
            return Err(AotLimitError::ZeroObjectBytes);
        }
        if max_relocations == 0 {
            return Err(AotLimitError::ZeroRelocations);
        }
        if max_native_ranges == 0 {
            return Err(AotLimitError::ZeroNativeRanges);
        }
        if max_transient_stack_bytes == 0 {
            return Err(AotLimitError::ZeroTransientStackBytes);
        }
        Ok(Self {
            functions: max_functions,
            function_bytes: max_function_bytes,
            object_bytes: max_object_bytes,
            relocations: max_relocations,
            native_ranges: max_native_ranges,
            transient_stack_bytes: max_transient_stack_bytes,
        })
    }

    /// Maximum POU plus Task entry functions.
    #[must_use]
    pub const fn max_functions(self) -> usize {
        self.functions
    }

    /// Maximum native bytes for one generated function.
    #[must_use]
    pub const fn max_function_bytes(self) -> usize {
        self.function_bytes
    }

    /// Maximum emitted ELF object bytes.
    #[must_use]
    pub const fn max_object_bytes(self) -> usize {
        self.object_bytes
    }

    /// Maximum relocations in the emitted object.
    #[must_use]
    pub const fn max_relocations(self) -> usize {
        self.relocations
    }

    /// Maximum native Source Map ranges.
    #[must_use]
    pub const fn max_native_ranges(self) -> usize {
        self.native_ranges
    }

    /// Maximum fixed native stack storage used for aggregate expression temporaries per POU.
    #[must_use]
    pub const fn max_transient_stack_bytes(self) -> usize {
        self.transient_stack_bytes
    }
}

/// Invalid zero-valued AOT capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AotLimitError {
    /// Function count is zero.
    #[error("max_functions must be non-zero")]
    ZeroFunctions,
    /// Per-function byte capacity is zero.
    #[error("max_function_bytes must be non-zero")]
    ZeroFunctionBytes,
    /// Object byte capacity is zero.
    #[error("max_object_bytes must be non-zero")]
    ZeroObjectBytes,
    /// Relocation capacity is zero.
    #[error("max_relocations must be non-zero")]
    ZeroRelocations,
    /// Native-range capacity is zero.
    #[error("max_native_ranges must be non-zero")]
    ZeroNativeRanges,
    /// Fixed aggregate-expression stack capacity is zero.
    #[error("max_transient_stack_bytes must be non-zero")]
    ZeroTransientStackBytes,
}

/// One statically linked Runtime callback required by an AOT object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeImport {
    /// Exact ABI symbol.
    pub symbol: String,
}

/// One exported Task entry point.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskExport {
    /// Scheduler identity bound at compile time.
    pub task: TaskHandle,
    /// Exact ELF symbol.
    pub symbol: String,
}

/// One half-open machine-code interval for a Canonical IR node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeCodeRange {
    /// Local POU function symbol containing the range.
    pub function_symbol: String,
    /// Canonical IR node owning the emitted instruction region.
    pub node: CanonicalNodeId,
    /// Section-relative byte offset in the function symbol's section.
    pub start: u32,
    /// Exclusive section-relative byte offset.
    pub end: u32,
}

/// Complete deterministic native range publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NativeSourceMap {
    /// Exact ABI major used by every range.
    pub abi_major: u16,
    /// Exact ABI minor used by every range.
    pub abi_minor: u16,
    /// Ranges sorted by function symbol, start, end, then node.
    pub ranges: Vec<NativeCodeRange>,
}

/// Atomic Linux x64 AOT publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AotArtifact {
    /// ELF64 relocatable object bytes.
    pub object: Vec<u8>,
    /// Lowercase SHA-256 of `object`.
    pub object_sha256: String,
    /// Sorted Runtime import whitelist actually referenced by the object.
    pub runtime_imports: Vec<RuntimeImport>,
    /// Task entry exports in `TaskHandle` order.
    pub task_exports: Vec<TaskExport>,
    /// Section-relative native ranges paired with their function symbols.
    pub native_source_map: NativeSourceMap,
}

/// Deterministic AOT rejection; no partial artifact is returned.
#[derive(Debug, Error)]
pub enum AotBuildError {
    /// Target or ABI differs from the only accepted R1 profile.
    #[error("unsupported AOT target or ABI")]
    UnsupportedTarget,
    /// Canonical inputs were not produced as one matching artifact set.
    #[error("Canonical IR, Source Map, and CheckpointPlan are inconsistent")]
    InconsistentInput,
    /// Accepted IR contains a runtime value form not representable by this ABI.
    #[error("unsupported executable node {kind:?} at Canonical node {node}")]
    UnsupportedNode {
        /// Node rejected by the backend.
        node: u32,
        /// Syntax category requiring unsupported runtime storage.
        kind: AstNodeKind,
    },
    /// A declared capacity would be crossed.
    #[error("AOT {resource} requires {actual}, exceeding limit {limit}")]
    CapacityExceeded {
        /// Bounded resource name.
        resource: &'static str,
        /// Required amount.
        actual: usize,
        /// Caller limit.
        limit: usize,
    },
    /// Cranelift rejected the deterministic module or function.
    #[error("Cranelift AOT generation failed: {0}")]
    Backend(String),
    /// Emitted bytes did not satisfy the frozen ELF contract.
    #[error("invalid emitted ELF object: {0}")]
    InvalidObject(String),
}

#[derive(Clone, Copy)]
struct RuntimeFunctions {
    read: FuncId,
    write: FuncId,
    checkpoint: FuncId,
    fault: FuncId,
    reset_frame: FuncId,
    concat_string: FuncId,
}

struct DeclaredPou {
    id: FuncId,
    symbol: String,
}

/// Compiles a complete Canonical IR artifact set into one deterministic Linux x64 ELF object.
///
/// The generated object uses fixed System V C signatures. Runtime storage access is always keyed
/// by Task, activation, stable symbol, byte offset, and scalar width; therefore generated code
/// neither receives nor retains raw image pointers. Callback failure cannot be silently ignored:
/// checkpoint stops and ST Faults return distinct status codes to the R0 transaction wrapper.
/// Task-return checkpoints are intentionally absent because `CycleTransaction::finish` owns that
/// exact boundary.
///
/// # Errors
///
/// Returns [`AotBuildError`] for mismatched artifact identities, unsupported target/value forms,
/// a capacity crossing, backend rejection, or an object that fails post-emission validation.
pub fn compile_linux_x64_aot(
    ir: &CanonicalStIr,
    source_map: &CanonicalSourceMap,
    checkpoints: &CheckpointPlan,
    target: AotTarget,
    limits: AotLimits,
) -> Result<AotArtifact, AotBuildError> {
    validate_inputs(ir, source_map, checkpoints, target)?;
    let required_functions = ir.pous.len().saturating_add(ir.tasks.len());
    enforce("functions", required_functions, limits.max_functions())?;

    let mut module = object_module()?;
    let runtime = declare_runtime(&mut module)?;
    let mut declared = BTreeMap::new();
    for pou in &ir.pous {
        let name = pou_symbol(pou.symbol);
        let id = module
            .declare_function(&name, Linkage::Local, &pou_signature())
            .map_err(backend)?;
        declared.insert(pou.symbol, DeclaredPou { id, symbol: name });
    }

    let checkpoint_sites = checkpoint_sites(checkpoints);
    let symbols = ir
        .symbols
        .iter()
        .map(|symbol| (symbol.id, symbol))
        .collect::<BTreeMap<_, _>>();
    let pous = ir
        .pous
        .iter()
        .map(|pou| (pou.symbol, pou))
        .collect::<BTreeMap<_, _>>();
    let mut ranges = Vec::new();
    for pou in &ir.pous {
        let declaration = declared
            .get(&pou.symbol)
            .ok_or(AotBuildError::InconsistentInput)?;
        define_pou(
            &mut module,
            pou,
            declaration,
            runtime,
            &declared,
            &symbols,
            &ir.types,
            &checkpoint_sites,
            limits,
            &mut ranges,
        )?;
    }

    let mut task_exports = Vec::with_capacity(ir.tasks.len());
    for task in &ir.tasks {
        let program = declared
            .get(&task.program)
            .ok_or(AotBuildError::InconsistentInput)?;
        let source = pous
            .get(&task.program)
            .ok_or(AotBuildError::InconsistentInput)?;
        if source.kind != CanonicalPouKind::Program {
            return Err(AotBuildError::InconsistentInput);
        }
        let name = task_symbol(task.task);
        let id = module
            .declare_function(&name, Linkage::Export, &task_signature())
            .map_err(backend)?;
        define_task(&mut module, id, task.task, program.id, limits)?;
        task_exports.push(TaskExport {
            task: task.task,
            symbol: name,
        });
    }

    let object = module.finish().emit().map_err(backend)?;
    enforce("object bytes", object.len(), limits.max_object_bytes())?;
    let runtime_imports = validate_object(&object, &task_exports, limits)?;
    translate_native_ranges(&object, &mut ranges)?;
    enforce("native ranges", ranges.len(), limits.max_native_ranges())?;
    ranges.sort_by(|left, right| {
        left.function_symbol
            .as_bytes()
            .cmp(right.function_symbol.as_bytes())
            .then(left.start.cmp(&right.start))
            .then(left.end.cmp(&right.end))
            .then(left.node.cmp(&right.node))
    });
    let object_sha256 = hex_digest(&Sha256::digest(&object));
    Ok(AotArtifact {
        object,
        object_sha256,
        runtime_imports,
        task_exports,
        native_source_map: NativeSourceMap {
            abi_major: AOT_ABI_MAJOR,
            abi_minor: AOT_ABI_MINOR,
            ranges,
        },
    })
}

fn validate_inputs(
    ir: &CanonicalStIr,
    source_map: &CanonicalSourceMap,
    checkpoints: &CheckpointPlan,
    target: AotTarget,
) -> Result<(), AotBuildError> {
    if target != AotTarget::linux_x64_v1() {
        return Err(AotBuildError::UnsupportedTarget);
    }
    if ir.schema_version != CanonicalIrVersion::preview_v1_0()
        || source_map.schema_version != CanonicalSourceMapVersion::preview_v1_0()
        || checkpoints.schema_version != CheckpointPlanVersion::preview_v1_0()
    {
        return Err(AotBuildError::InconsistentInput);
    }
    let node_ids = ir
        .pous
        .iter()
        .flat_map(|pou| preorder(&pou.body))
        .map(|node| node.id)
        .collect::<BTreeSet<_>>();
    if node_ids.len() != source_map.nodes.len()
        || !source_map
            .nodes
            .iter()
            .all(|entry| node_ids.contains(&entry.node))
        || !checkpoints
            .sites
            .iter()
            .all(|site| node_ids.contains(&site.node))
    {
        return Err(AotBuildError::InconsistentInput);
    }
    let mut task_handles = BTreeSet::new();
    if !ir.tasks.iter().all(|task| task_handles.insert(task.task))
        || checkpoints.tasks.len() != ir.tasks.len()
        || !checkpoints.tasks.iter().all(|planned| {
            ir.tasks
                .iter()
                .any(|task| task.task == planned.task && task.program == planned.program)
        })
    {
        return Err(AotBuildError::InconsistentInput);
    }
    let plan_limits = CheckpointPlanLimits::new(usize::MAX, usize::MAX, 1)
        .map_err(|_| AotBuildError::InconsistentInput)?;
    let expected_checkpoints = CheckpointPlanner::new(ir, source_map, plan_limits)
        .map_err(|_| AotBuildError::InconsistentInput)?
        .build()
        .map_err(|_| AotBuildError::InconsistentInput)?;
    if &expected_checkpoints != checkpoints {
        return Err(AotBuildError::InconsistentInput);
    }
    Ok(())
}

fn object_module() -> Result<ObjectModule, AotBuildError> {
    let triple = TARGET_TRIPLE
        .parse::<Triple>()
        .map_err(|error| AotBuildError::Backend(error.to_string()))?;
    let mut flags = settings::builder();
    flags.set("opt_level", "speed_and_size").map_err(backend)?;
    flags.set("enable_verifier", "true").map_err(backend)?;
    flags.set("is_pic", "false").map_err(backend)?;
    flags
        .set("preserve_frame_pointers", "false")
        .map_err(backend)?;
    let isa = isa::lookup(triple)
        .map_err(backend)?
        .finish(settings::Flags::new(flags))
        .map_err(backend)?;
    let builder =
        ObjectBuilder::new(isa, "aurora-st-aot-v1", default_libcall_names()).map_err(backend)?;
    Ok(ObjectModule::new(builder))
}

fn declare_runtime(module: &mut ObjectModule) -> Result<RuntimeFunctions, AotBuildError> {
    let call_conv = module.isa().default_call_conv();
    let mut read = Signature::new(call_conv);
    read.params.extend([
        AbiParam::new(types::I64),
        AbiParam::new(types::I32),
        AbiParam::new(types::I32),
        AbiParam::new(types::I32),
        AbiParam::new(types::I64),
        AbiParam::new(types::I32),
    ]);
    read.returns.push(AbiParam::new(types::I64));
    let mut write = read.clone();
    write.returns.clear();
    write.params.push(AbiParam::new(types::I64));
    let mut checkpoint = Signature::new(call_conv);
    checkpoint
        .params
        .extend([AbiParam::new(types::I64), AbiParam::new(types::I32)]);
    checkpoint.returns.push(AbiParam::new(types::I32));
    let mut fault = Signature::new(call_conv);
    fault.params.extend([
        AbiParam::new(types::I64),
        AbiParam::new(types::I32),
        AbiParam::new(types::I32),
    ]);
    let mut reset = Signature::new(call_conv);
    reset.params.extend([
        AbiParam::new(types::I64),
        AbiParam::new(types::I32),
        AbiParam::new(types::I32),
        AbiParam::new(types::I32),
    ]);
    let mut concat_string = Signature::new(call_conv);
    concat_string.params.extend([
        AbiParam::new(types::I64),
        AbiParam::new(types::I64),
        AbiParam::new(types::I64),
        AbiParam::new(types::I32),
        AbiParam::new(types::I32),
    ]);
    concat_string.returns.push(AbiParam::new(types::I32));
    Ok(RuntimeFunctions {
        read: module
            .declare_function(READ_BITS, Linkage::Import, &read)
            .map_err(backend)?,
        write: module
            .declare_function(WRITE_BITS, Linkage::Import, &write)
            .map_err(backend)?,
        checkpoint: module
            .declare_function(CHECKPOINT, Linkage::Import, &checkpoint)
            .map_err(backend)?,
        fault: module
            .declare_function(REPORT_FAULT, Linkage::Import, &fault)
            .map_err(backend)?,
        reset_frame: module
            .declare_function(RESET_FRAME, Linkage::Import, &reset)
            .map_err(backend)?,
        concat_string: module
            .declare_function(CONCAT_STRING, Linkage::Import, &concat_string)
            .map_err(backend)?,
    })
}

fn pou_signature() -> Signature {
    let mut signature = Signature::new(cranelift_codegen::isa::CallConv::SystemV);
    signature.params.extend([
        AbiParam::new(types::I64),
        AbiParam::new(types::I32),
        AbiParam::new(types::I32),
    ]);
    signature.returns.push(AbiParam::new(types::I32));
    signature
}

fn task_signature() -> Signature {
    let mut signature = Signature::new(cranelift_codegen::isa::CallConv::SystemV);
    signature.params.push(AbiParam::new(types::I64));
    signature.returns.push(AbiParam::new(types::I32));
    signature
}

fn define_task(
    module: &mut ObjectModule,
    id: FuncId,
    task: TaskHandle,
    program: FuncId,
    limits: AotLimits,
) -> Result<(), AotBuildError> {
    let frontend_config = module.target_config();
    let mut context = Context::new();
    context.func =
        Function::with_name_signature(UserFuncName::user(1, id.as_u32()), task_signature());
    let mut function_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut function_context);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let context_value = builder.block_params(entry)[0];
        let task_value = builder.ins().iconst(types::I32, i64::from(task.0));
        let activation = builder.ins().iconst(types::I32, INACTIVE_ACTIVATION);
        let reference = module.declare_func_in_func(program, builder.func);
        let call = builder
            .ins()
            .call(reference, &[context_value, task_value, activation]);
        let status = builder.inst_results(call)[0];
        builder.ins().return_(&[status]);
        builder.seal_all_blocks();
        builder.finalize(frontend_config);
    }
    define_checked(module, id, &mut context, limits)
}

#[allow(clippy::too_many_arguments)]
fn define_pou(
    module: &mut ObjectModule,
    pou: &CanonicalPou,
    declaration: &DeclaredPou,
    runtime: RuntimeFunctions,
    declared: &BTreeMap<SymbolId, DeclaredPou>,
    symbols: &BTreeMap<SymbolId, &SemanticSymbol>,
    fixed_types: &[FixedTypeLayout],
    checkpoints: &BTreeMap<CanonicalNodeId, Vec<(CheckpointSiteId, CheckpointSiteKind)>>,
    limits: AotLimits,
    ranges: &mut Vec<NativeCodeRange>,
) -> Result<(), AotBuildError> {
    let frontend_config = module.target_config();
    let mut context = Context::new();
    context.func = Function::with_name_signature(
        UserFuncName::user(0, declaration.id.as_u32()),
        pou_signature(),
    );
    let mut function_context = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut context.func, &mut function_context);
        let entry = builder.create_block();
        let exit = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.append_block_param(exit, types::I32);
        builder.switch_to_block(entry);
        let params = builder.block_params(entry).to_vec();
        {
            let mut lowerer = FunctionLowerer {
                builder: &mut builder,
                module,
                runtime,
                declared,
                symbols,
                fixed_types,
                checkpoints,
                pou,
                context: params[0],
                task: params[1],
                activation: params[2],
                exit,
                transient_stack_limit: limits.max_transient_stack_bytes(),
                transient_stack_bytes: 0,
            };
            lowerer.statement(&pou.body)?;
            if !lowerer.builder.is_unreachable() {
                let completed = lowerer.builder.ins().iconst(types::I32, STATUS_COMPLETED);
                let args = [BlockArg::from(completed)];
                lowerer.builder.ins().jump(exit, &args);
            }
            lowerer.builder.switch_to_block(exit);
            let status = lowerer.builder.block_params(exit)[0];
            lowerer.builder.ins().return_(&[status]);
            lowerer.builder.seal_all_blocks();
        }
        builder.finalize(frontend_config);
    }
    module
        .define_function(declaration.id, &mut context)
        .map_err(backend)?;
    let compiled = context.compiled_code().ok_or_else(|| {
        AotBuildError::Backend("Cranelift did not retain compiled function bytes".to_owned())
    })?;
    enforce(
        "function bytes",
        compiled.buffer.data().len(),
        limits.max_function_bytes(),
    )?;
    for range in compiled.buffer.get_srclocs_sorted() {
        if range.start < range.end {
            let raw = range.loc.bits();
            if raw == u32::MAX {
                continue;
            }
            ranges.push(NativeCodeRange {
                function_symbol: declaration.symbol.clone(),
                node: CanonicalNodeId(raw),
                start: range.start,
                end: range.end,
            });
        }
    }
    Ok(())
}

fn define_checked(
    module: &mut ObjectModule,
    id: FuncId,
    context: &mut Context,
    limits: AotLimits,
) -> Result<(), AotBuildError> {
    module.define_function(id, context).map_err(backend)?;
    let compiled = context.compiled_code().ok_or_else(|| {
        AotBuildError::Backend("Cranelift did not retain compiled function bytes".to_owned())
    })?;
    enforce(
        "function bytes",
        compiled.buffer.data().len(),
        limits.max_function_bytes(),
    )
}

struct FunctionLowerer<'a, 'b> {
    builder: &'a mut FunctionBuilder<'b>,
    module: &'a mut ObjectModule,
    runtime: RuntimeFunctions,
    declared: &'a BTreeMap<SymbolId, DeclaredPou>,
    symbols: &'a BTreeMap<SymbolId, &'a SemanticSymbol>,
    fixed_types: &'a [FixedTypeLayout],
    checkpoints: &'a BTreeMap<CanonicalNodeId, Vec<(CheckpointSiteId, CheckpointSiteKind)>>,
    pou: &'a CanonicalPou,
    context: cranelift_codegen::ir::Value,
    task: cranelift_codegen::ir::Value,
    activation: cranelift_codegen::ir::Value,
    exit: cranelift_codegen::ir::Block,
    transient_stack_limit: usize,
    transient_stack_bytes: usize,
}

#[derive(Clone, Copy)]
struct StorageLocation {
    symbol: SymbolId,
    offset: cranelift_codegen::ir::Value,
    activation: cranelift_codegen::ir::Value,
    layout: FixedTypeId,
}

#[derive(Clone, Copy)]
struct AggregateSlot {
    slot: StackSlot,
    layout: FixedTypeId,
}

impl FunctionLowerer<'_, '_> {
    fn statement(&mut self, node: &CanonicalNode) -> Result<(), AotBuildError> {
        self.location(node);
        match node.kind {
            AstNodeKind::StatementList => {
                for child in &node.children {
                    if self.builder.is_unreachable() {
                        break;
                    }
                    self.statement(child)?;
                }
            }
            AstNodeKind::AssignmentStatement => {
                let [target, value] = node.children.as_slice() else {
                    return Err(AotBuildError::InconsistentInput);
                };
                let location = self.storage_location(target, self.activation)?;
                if self.is_scalar_layout(location.layout)? {
                    let value = self.expression(value)?;
                    let width = self.layout(location.layout)?.size_bytes;
                    self.store_at(
                        location.symbol,
                        location.offset,
                        value,
                        i64::try_from(width).map_err(|_| AotBuildError::InconsistentInput)?,
                        location.activation,
                    );
                } else {
                    let value = self.aggregate_expression(value, location.layout)?;
                    self.store_aggregate(location, value)?;
                }
            }
            AstNodeKind::IfStatement => self.if_statement(node)?,
            AstNodeKind::ForStatement => self.for_statement(node)?,
            AstNodeKind::ReturnStatement => {
                if let Some(value) = node.children.first() {
                    if self.pou.kind != CanonicalPouKind::Function {
                        return Err(AotBuildError::InconsistentInput);
                    }
                    let layout = self.fixed_type_for_symbol(self.pou.symbol)?;
                    let zero = self.builder.ins().iconst(types::I64, 0);
                    let destination = StorageLocation {
                        symbol: self.pou.symbol,
                        offset: zero,
                        activation: self.activation,
                        layout,
                    };
                    if self.is_scalar_layout(layout)? {
                        let result = self.expression(value)?;
                        self.store(self.pou.symbol, 0, result, value_width(value_type(value)))?;
                    } else {
                        let result = self.aggregate_expression(value, layout)?;
                        self.store_aggregate(destination, result)?;
                    }
                }
                self.jump_status(STATUS_COMPLETED);
            }
            AstNodeKind::FunctionBlockCallStatement => self.function_block_call(node)?,
            _ => {
                return Err(AotBuildError::UnsupportedNode {
                    node: node.id.0,
                    kind: node.kind,
                });
            }
        }
        Ok(())
    }

    fn if_statement(&mut self, node: &CanonicalNode) -> Result<(), AotBuildError> {
        if node.children.len() < 2 {
            return Err(AotBuildError::InconsistentInput);
        }
        let merge = self.builder.create_block();
        let mut next = self.builder.create_block();
        let condition = self.expression(&node.children[0])?;
        let truth = self.builder.ins().icmp_imm_s(
            cranelift_codegen::ir::condcodes::IntCC::NotEqual,
            condition,
            0,
        );
        let body = self.builder.create_block();
        self.builder.ins().brif(truth, body, &[], next, &[]);
        self.builder.switch_to_block(body);
        self.statement(&node.children[1])?;
        if !self.builder.is_unreachable() {
            self.builder.ins().jump(merge, &[]);
        }
        for clause in node.children.iter().skip(2) {
            self.builder.switch_to_block(next);
            match clause.kind {
                AstNodeKind::ElsifClause => {
                    let [condition, clause_body] = clause.children.as_slice() else {
                        return Err(AotBuildError::InconsistentInput);
                    };
                    let following = self.builder.create_block();
                    let selected = self.builder.create_block();
                    let value = self.expression(condition)?;
                    let truth = self.builder.ins().icmp_imm_s(
                        cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                        value,
                        0,
                    );
                    self.builder
                        .ins()
                        .brif(truth, selected, &[], following, &[]);
                    self.builder.switch_to_block(selected);
                    self.statement(clause_body)?;
                    if !self.builder.is_unreachable() {
                        self.builder.ins().jump(merge, &[]);
                    }
                    next = following;
                }
                AstNodeKind::ElseClause => {
                    let [clause_body] = clause.children.as_slice() else {
                        return Err(AotBuildError::InconsistentInput);
                    };
                    self.statement(clause_body)?;
                    if !self.builder.is_unreachable() {
                        self.builder.ins().jump(merge, &[]);
                    }
                    next = self.builder.create_block();
                }
                _ => return Err(AotBuildError::InconsistentInput),
            }
        }
        self.builder.switch_to_block(next);
        if !self.builder.is_unreachable() {
            self.builder.ins().jump(merge, &[]);
        }
        self.builder.switch_to_block(merge);
        Ok(())
    }

    fn for_statement(&mut self, node: &CanonicalNode) -> Result<(), AotBuildError> {
        let iterations = node
            .loop_iterations
            .ok_or(AotBuildError::InconsistentInput)?;
        let body_index = node
            .children
            .len()
            .checked_sub(1)
            .ok_or(AotBuildError::InconsistentInput)?;
        if body_index < 3 {
            return Err(AotBuildError::InconsistentInput);
        }
        let control = node.children[0]
            .symbol
            .ok_or(AotBuildError::InconsistentInput)?;
        let control_width = self.symbol_width(control);
        let initial = self.expression(&node.children[1])?;
        self.store(control, 0, initial, control_width)?;
        if iterations == 0 {
            return Ok(());
        }
        let step = if body_index == 4 {
            self.expression(&node.children[3])?
        } else {
            self.builder.ins().iconst(types::I64, 1)
        };
        let header = self.builder.create_block();
        let body = self.builder.create_block();
        let done = self.builder.create_block();
        self.builder.append_block_param(header, types::I64);
        self.builder.append_block_param(header, types::I64);
        let remaining_initial = self.builder.ins().iconst(
            types::I64,
            i64::try_from(iterations).map_err(|_| AotBuildError::InconsistentInput)?,
        );
        let initial_args = [BlockArg::from(initial), BlockArg::from(remaining_initial)];
        self.builder.ins().jump(header, &initial_args);
        self.builder.switch_to_block(header);
        let current = self.builder.block_params(header)[0];
        let remaining = self.builder.block_params(header)[1];
        let has_work = self.builder.ins().icmp_imm_s(
            cranelift_codegen::ir::condcodes::IntCC::NotEqual,
            remaining,
            0,
        );
        self.builder.ins().brif(has_work, body, &[], done, &[]);
        self.builder.switch_to_block(body);
        self.statement(&node.children[body_index])?;
        if !self.builder.is_unreachable() {
            self.emit_checkpoint_kind(node, |kind| {
                matches!(kind, CheckpointSiteKind::LoopBackEdge)
            })?;
            let next = self.builder.ins().iadd(current, step);
            self.store(control, 0, next, control_width)?;
            let one = self.builder.ins().iconst(types::I64, 1);
            let left = self.builder.ins().isub(remaining, one);
            let next_args = [BlockArg::from(next), BlockArg::from(left)];
            self.builder.ins().jump(header, &next_args);
        }
        self.builder.switch_to_block(done);
        Ok(())
    }

    fn expression(
        &mut self,
        node: &CanonicalNode,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        self.location(node);
        match node.kind {
            AstNodeKind::Literal => literal(self.builder, node, value_type(node)),
            AstNodeKind::QualifiedLiteral => {
                let value = node
                    .children
                    .last()
                    .ok_or(AotBuildError::InconsistentInput)?;
                if value.kind == AstNodeKind::Literal {
                    literal(self.builder, value, value_type(node))
                } else if value.kind == AstNodeKind::Identifier {
                    self.enumeration_literal(value)
                } else {
                    self.expression(value)
                }
            }
            AstNodeKind::ParenthesizedExpression => node
                .children
                .last()
                .ok_or(AotBuildError::InconsistentInput)
                .and_then(|child| self.expression(child)),
            AstNodeKind::Assignable => {
                let (symbol, offset, width) = self.scalar_location(node)?;
                let value = self.load_at(symbol, offset, width, self.activation);
                Ok(self.normalize(value, value_type(node)))
            }
            AstNodeKind::UnaryExpression => {
                let child = node
                    .children
                    .first()
                    .ok_or(AotBuildError::InconsistentInput)?;
                let value = self.expression(child)?;
                match node.text.as_deref() {
                    Some("+") => Ok(value),
                    Some("-") if matches!(value_type(node), Some(SemanticType::Real)) => {
                        let sign = self.builder.ins().iconst(types::I64, i64::from(i32::MIN));
                        Ok(self.builder.ins().bxor(value, sign))
                    }
                    Some("-") if matches!(value_type(node), Some(SemanticType::Lreal)) => {
                        let sign = self.builder.ins().iconst(types::I64, i64::MIN);
                        Ok(self.builder.ins().bxor(value, sign))
                    }
                    Some("-") => Ok(self.builder.ins().ineg(value)),
                    Some("NOT") => Ok(self.builder.ins().bnot(value)),
                    _ => Err(AotBuildError::InconsistentInput),
                }
            }
            AstNodeKind::BinaryExpression => self.binary(node),
            AstNodeKind::CallExpression => self.call_expression(node),
            _ => Err(AotBuildError::UnsupportedNode {
                node: node.id.0,
                kind: node.kind,
            }),
        }
    }

    fn enumeration_literal(
        &mut self,
        node: &CanonicalNode,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let symbol = node.symbol.ok_or(AotBuildError::InconsistentInput)?;
        let declaration = self
            .symbols
            .get(&symbol)
            .copied()
            .filter(|entry| entry.kind == SemanticSymbolKind::EnumerationMember)
            .ok_or(AotBuildError::InconsistentInput)?;
        let layout = self.layout(self.fixed_type_for_symbol(symbol)?)?;
        let FixedTypeKind::Enumeration { members } = &layout.kind else {
            return Err(AotBuildError::InconsistentInput);
        };
        let value = members
            .iter()
            .find(|member| member.name.eq_ignore_ascii_case(&declaration.name))
            .map(|member| member.value)
            .ok_or(AotBuildError::InconsistentInput)?;
        Ok(self.builder.ins().iconst(types::I64, i64::from(value)))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keeps the frozen ST operator table and its Fault branches reviewable together"
    )]
    fn binary(
        &mut self,
        node: &CanonicalNode,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let [left, right] = node.children.as_slice() else {
            return Err(AotBuildError::InconsistentInput);
        };
        let left = self.expression(left)?;
        if matches!(node.text.as_deref(), Some("AND_THEN" | "OR_ELSE")) {
            return self.short_circuit(node, left, right);
        }
        let right = self.expression(right)?;
        let operand_type = value_type(&node.children[0]);
        if is_float(operand_type) {
            return self.float_binary(node, left, right, operand_type);
        }
        let value = match node.text.as_deref() {
            Some("+") => self.checked_integer(node, "ADD", left, right, operand_type)?,
            Some("-") => self.checked_integer(node, "SUB", left, right, operand_type)?,
            Some("*") => self.checked_integer(node, "MUL", left, right, operand_type)?,
            Some("AND") => self.builder.ins().band(left, right),
            Some("OR") => self.builder.ins().bor(left, right),
            Some("XOR") => self.builder.ins().bxor(left, right),
            Some("=" | "<>" | "<" | "<=" | ">" | ">=") => {
                let unsigned = is_unsigned(operand_type);
                let condition = match (node.text.as_deref(), unsigned) {
                    (Some("="), _) => cranelift_codegen::ir::condcodes::IntCC::Equal,
                    (Some("<>"), _) => cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                    (Some("<"), true) => cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan,
                    (Some("<="), true) => {
                        cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThanOrEqual
                    }
                    (Some(">"), true) => {
                        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan
                    }
                    (Some(">="), true) => {
                        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThanOrEqual
                    }
                    (Some("<"), false) => cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                    (Some("<="), false) => {
                        cranelift_codegen::ir::condcodes::IntCC::SignedLessThanOrEqual
                    }
                    (Some(">"), false) => {
                        cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThan
                    }
                    (Some(">="), false) => {
                        cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThanOrEqual
                    }
                    _ => return Err(AotBuildError::InconsistentInput),
                };
                let compared = self.builder.ins().icmp(condition, left, right);
                self.builder.ins().uextend(types::I64, compared)
            }
            Some("/" | "MOD") => {
                let zero = self.builder.ins().icmp_imm_s(
                    cranelift_codegen::ir::condcodes::IntCC::Equal,
                    right,
                    0,
                );
                if node.fault_site.is_some() {
                    self.emit_fault_if(node, zero, RuntimeFaultCode::IntegerDivisionByZero)?;
                    if node.text.as_deref() == Some("/") && !is_unsigned(operand_type) {
                        let (minimum, _) = integer_bounds(
                            integer_bits(operand_type).ok_or(AotBuildError::InconsistentInput)?,
                            false,
                        );
                        let minimum_value = self.builder.ins().icmp_imm_s(
                            cranelift_codegen::ir::condcodes::IntCC::Equal,
                            left,
                            minimum,
                        );
                        let negative_one = self.builder.ins().icmp_imm_s(
                            cranelift_codegen::ir::condcodes::IntCC::Equal,
                            right,
                            -1,
                        );
                        let overflow = self.builder.ins().band(minimum_value, negative_one);
                        self.emit_fault_if(node, overflow, RuntimeFaultCode::IntegerOverflow)?;
                    }
                }
                let unsigned = is_unsigned(operand_type);
                if node.text.as_deref() == Some("/") && unsigned {
                    self.builder.ins().udiv(left, right)
                } else if node.text.as_deref() == Some("/") {
                    self.builder.ins().sdiv(left, right)
                } else if unsigned {
                    self.builder.ins().urem(left, right)
                } else {
                    let (minimum, _) = integer_bounds(
                        integer_bits(operand_type).ok_or(AotBuildError::InconsistentInput)?,
                        false,
                    );
                    let minimum_value = self.builder.ins().icmp_imm_s(
                        cranelift_codegen::ir::condcodes::IntCC::Equal,
                        left,
                        minimum,
                    );
                    let negative_one = self.builder.ins().icmp_imm_s(
                        cranelift_codegen::ir::condcodes::IntCC::Equal,
                        right,
                        -1,
                    );
                    let minimum_mod_negative_one =
                        self.builder.ins().band(minimum_value, negative_one);
                    let one = self.builder.ins().iconst(types::I64, 1);
                    let safe_divisor =
                        self.builder
                            .ins()
                            .select(minimum_mod_negative_one, one, right);
                    self.builder.ins().srem(left, safe_divisor)
                }
            }
            _ => return Err(AotBuildError::InconsistentInput),
        };
        Ok(self.normalize(value, value_type(node)))
    }

    fn short_circuit(
        &mut self,
        node: &CanonicalNode,
        left: cranelift_codegen::ir::Value,
        right: &CanonicalNode,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let evaluate_right = self.builder.create_block();
        let constant_path = self.builder.create_block();
        let merge = self.builder.create_block();
        self.builder.append_block_param(merge, types::I64);
        let truth = self.builder.ins().icmp_imm_s(
            cranelift_codegen::ir::condcodes::IntCC::NotEqual,
            left,
            0,
        );
        if node.text.as_deref() == Some("AND_THEN") {
            self.builder
                .ins()
                .brif(truth, evaluate_right, &[], constant_path, &[]);
        } else {
            self.builder
                .ins()
                .brif(truth, constant_path, &[], evaluate_right, &[]);
        }
        self.builder.switch_to_block(constant_path);
        let constant = self.builder.ins().iconst(
            types::I64,
            i64::from(node.text.as_deref() == Some("OR_ELSE")),
        );
        let constant_args = [BlockArg::from(constant)];
        self.builder.ins().jump(merge, &constant_args);
        self.builder.switch_to_block(evaluate_right);
        let right = self.expression(right)?;
        let normalized = self.builder.ins().icmp_imm_s(
            cranelift_codegen::ir::condcodes::IntCC::NotEqual,
            right,
            0,
        );
        let normalized = self.builder.ins().uextend(types::I64, normalized);
        let right_args = [BlockArg::from(normalized)];
        self.builder.ins().jump(merge, &right_args);
        self.builder.switch_to_block(merge);
        Ok(self.builder.block_params(merge)[0])
    }

    fn float_binary(
        &mut self,
        node: &CanonicalNode,
        left_bits: cranelift_codegen::ir::Value,
        right_bits: cranelift_codegen::ir::Value,
        value_type: Option<&SemanticType>,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let left = self.float_value(left_bits, value_type);
        let right = self.float_value(right_bits, value_type);
        if node.fault_site.is_some() {
            self.emit_non_finite_if(node, left, value_type)?;
            self.emit_non_finite_if(node, right, value_type)?;
        }
        let result = match node.text.as_deref() {
            Some("+") => self.builder.ins().fadd(left, right),
            Some("-") => self.builder.ins().fsub(left, right),
            Some("*") => self.builder.ins().fmul(left, right),
            Some("/") => self.builder.ins().fdiv(left, right),
            Some("=" | "<>" | "<" | "<=" | ">" | ">=") => {
                let condition = match node.text.as_deref() {
                    Some("=") => cranelift_codegen::ir::condcodes::FloatCC::Equal,
                    Some("<>") => cranelift_codegen::ir::condcodes::FloatCC::OrderedNotEqual,
                    Some("<") => cranelift_codegen::ir::condcodes::FloatCC::LessThan,
                    Some("<=") => cranelift_codegen::ir::condcodes::FloatCC::LessThanOrEqual,
                    Some(">") => cranelift_codegen::ir::condcodes::FloatCC::GreaterThan,
                    Some(">=") => cranelift_codegen::ir::condcodes::FloatCC::GreaterThanOrEqual,
                    _ => return Err(AotBuildError::InconsistentInput),
                };
                let compared = self.builder.ins().fcmp(condition, left, right);
                return Ok(self.builder.ins().uextend(types::I64, compared));
            }
            _ => return Err(AotBuildError::InconsistentInput),
        };
        if node.fault_site.is_some() {
            self.emit_non_finite_if(node, result, value_type)?;
        }
        Ok(self.float_bits(result, value_type))
    }

    fn call_expression(
        &mut self,
        node: &CanonicalNode,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let name = call_name(node).ok_or(AotBuildError::InconsistentInput)?;
        let callee = node.children[0]
            .children
            .first()
            .and_then(|value| value.symbol);
        if let Some(callee) = callee {
            let activation = self.invoke_user_call(node, callee)?;
            return self.load_with_activation(callee, 0, value_width(value_type(node)), activation);
        }
        self.standard_call(node, &name)
    }

    fn invoke_user_call(
        &mut self,
        node: &CanonicalNode,
        callee: SymbolId,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let declaration = self
            .declared
            .get(&callee)
            .ok_or(AotBuildError::InconsistentInput)?
            .id;
        let inputs = self.input_symbols(callee);
        if inputs.len() != node.children.len().saturating_sub(1) {
            return Err(AotBuildError::InconsistentInput);
        }
        let activation = self.builder.ins().iconst(types::I32, i64::from(callee.0));
        self.reset_frame(callee, activation);
        for (argument, (input, input_type)) in node.children.iter().skip(1).zip(inputs) {
            let layout = self.fixed_type_for_symbol(input)?;
            if self.is_scalar_layout(layout)? {
                let value = self.expression(argument)?;
                self.store_with_activation(
                    input,
                    0,
                    value,
                    value_width(input_type.as_ref()),
                    activation,
                )?;
            } else {
                let value = self.aggregate_expression(argument, layout)?;
                let zero = self.builder.ins().iconst(types::I64, 0);
                self.store_aggregate(
                    StorageLocation {
                        symbol: input,
                        offset: zero,
                        activation,
                        layout,
                    },
                    value,
                )?;
            }
        }
        self.emit_checkpoint_kind(node, |kind| {
            matches!(kind, CheckpointSiteKind::BeforePouCall { .. })
        })?;
        self.call_pou(declaration, activation)?;
        self.emit_checkpoint_kind(node, |kind| {
            matches!(kind, CheckpointSiteKind::AfterPouCall { .. })
        })?;
        Ok(activation)
    }

    #[allow(clippy::too_many_lines)]
    fn standard_call(
        &mut self,
        node: &CanonicalNode,
        name: &str,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let mut arguments = Vec::with_capacity(node.children.len().saturating_sub(1));
        for argument in node.children.iter().skip(1) {
            arguments.push(self.expression(argument)?);
        }
        let value = match (name, arguments.as_slice()) {
            ("SQRT", [value]) if is_float(value_type(node)) => {
                let float = self.float_value(*value, value_type(node));
                let result = self.builder.ins().sqrt(float);
                if node.fault_site.is_some() {
                    self.emit_non_finite_if(node, result, value_type(node))?;
                }
                self.float_bits(result, value_type(node))
            }
            ("CHECKED_ADD", [left, right]) => {
                self.checked_integer(node, "ADD", *left, *right, value_type(node))?
            }
            ("CHECKED_SUB", [left, right]) => {
                self.checked_integer(node, "SUB", *left, *right, value_type(node))?
            }
            ("CHECKED_MUL", [left, right]) => {
                self.checked_integer(node, "MUL", *left, *right, value_type(node))?
            }
            ("WRAPPING_ADD", [left, right]) => {
                let raw = self.builder.ins().iadd(*left, *right);
                self.normalize(raw, value_type(node))
            }
            ("WRAPPING_SUB", [left, right]) => {
                let raw = self.builder.ins().isub(*left, *right);
                self.normalize(raw, value_type(node))
            }
            ("WRAPPING_MUL", [left, right]) => {
                let raw = self.builder.ins().imul(*left, *right);
                self.normalize(raw, value_type(node))
            }
            ("SATURATING_ADD", [left, right]) => {
                self.saturating_integer("ADD", *left, *right, value_type(node))
            }
            ("SATURATING_SUB", [left, right]) => {
                self.saturating_integer("SUB", *left, *right, value_type(node))
            }
            ("SATURATING_MUL", [left, right]) => {
                self.saturating_integer("MUL", *left, *right, value_type(node))
            }
            ("CHECKED_NEG", [value]) => {
                let overflow = if is_unsigned(value_type(node)) {
                    self.builder.ins().icmp_imm_s(
                        cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                        *value,
                        0,
                    )
                } else {
                    let (minimum, _) = integer_bounds(
                        integer_bits(value_type(node)).ok_or(AotBuildError::InconsistentInput)?,
                        false,
                    );
                    self.builder.ins().icmp_imm_s(
                        cranelift_codegen::ir::condcodes::IntCC::Equal,
                        *value,
                        minimum,
                    )
                };
                self.emit_fault_if(node, overflow, RuntimeFaultCode::IntegerOverflow)?;
                let negated = self.builder.ins().ineg(*value);
                self.normalize(negated, value_type(node))
            }
            ("SATURATING_NEG", [_]) if is_unsigned(value_type(node)) => {
                self.builder.ins().iconst(types::I64, 0)
            }
            ("SATURATING_NEG", [value]) => {
                let (minimum, maximum) = integer_bounds(
                    integer_bits(value_type(node)).ok_or(AotBuildError::InconsistentInput)?,
                    is_unsigned(value_type(node)),
                );
                let at_minimum = self.builder.ins().icmp_imm_s(
                    cranelift_codegen::ir::condcodes::IntCC::Equal,
                    *value,
                    minimum,
                );
                let maximum = self.builder.ins().iconst(types::I64, maximum);
                let negated_raw = self.builder.ins().ineg(*value);
                let negated = self.normalize(negated_raw, value_type(node));
                self.builder.ins().select(at_minimum, maximum, negated)
            }
            ("WRAPPING_NEG", [value]) => {
                let negated = self.builder.ins().ineg(*value);
                self.normalize(negated, value_type(node))
            }
            ("ABS", [value]) if is_float(value_type(node)) => {
                let float = self.float_value(*value, value_type(node));
                let result = self.builder.ins().fabs(float);
                if node.fault_site.is_some() {
                    self.emit_non_finite_if(node, result, value_type(node))?;
                }
                self.float_bits(result, value_type(node))
            }
            ("ABS", [value]) => {
                let negative = self.builder.ins().icmp_imm_s(
                    cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                    *value,
                    0,
                );
                if node.fault_site.is_some() {
                    let (minimum, _) = integer_bounds(
                        integer_bits(value_type(node)).ok_or(AotBuildError::InconsistentInput)?,
                        false,
                    );
                    let overflow = self.builder.ins().icmp_imm_s(
                        cranelift_codegen::ir::condcodes::IntCC::Equal,
                        *value,
                        minimum,
                    );
                    self.emit_fault_if(node, overflow, RuntimeFaultCode::IntegerOverflow)?;
                }
                let negated = self.builder.ins().ineg(*value);
                self.builder.ins().select(negative, negated, *value)
            }
            ("MIN", [left, right]) if is_float(value_type(node)) => {
                self.float_min_max(node, *left, *right, true)?
            }
            ("MAX", [left, right]) if is_float(value_type(node)) => {
                self.float_min_max(node, *left, *right, false)?
            }
            ("MIN" | "MAX", [left, right]) => {
                let unsigned = is_unsigned(value_type(node));
                let minimum = name == "MIN";
                let condition = match (minimum, unsigned) {
                    (true, true) => cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan,
                    (true, false) => cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                    (false, true) => cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan,
                    (false, false) => cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThan,
                };
                let selected = self.builder.ins().icmp(condition, *right, *left);
                self.builder.ins().select(selected, *right, *left)
            }
            ("LIMIT", [value, low, high]) if is_float(value_type(node)) => {
                self.float_limit(node, *value, *low, *high)?
            }
            ("LIMIT", [value, low, high]) => {
                let unsigned = is_unsigned(value_type(node));
                let invalid = self.builder.ins().icmp(
                    if unsigned {
                        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan
                    } else {
                        cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThan
                    },
                    *low,
                    *high,
                );
                if node.fault_site.is_some() {
                    self.emit_fault_if(node, invalid, RuntimeFaultCode::InvalidRuntimeRange)?;
                }
                let below = self.builder.ins().icmp(
                    if unsigned {
                        cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan
                    } else {
                        cranelift_codegen::ir::condcodes::IntCC::SignedLessThan
                    },
                    *value,
                    *low,
                );
                let lower = self.builder.ins().select(below, *low, *value);
                let above = self.builder.ins().icmp(
                    if unsigned {
                        cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan
                    } else {
                        cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThan
                    },
                    lower,
                    *high,
                );
                self.builder.ins().select(above, *high, lower)
            }
            (value, [argument]) if value.starts_with("TO_") => self.explicit_conversion(
                node,
                *argument,
                value_type(&node.children[1]),
                value_type(node),
            )?,
            _ => {
                return Err(AotBuildError::UnsupportedNode {
                    node: node.id.0,
                    kind: node.kind,
                });
            }
        };
        Ok(value)
    }

    fn float_min_max(
        &mut self,
        node: &CanonicalNode,
        left_bits: cranelift_codegen::ir::Value,
        right_bits: cranelift_codegen::ir::Value,
        minimum: bool,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let value_type = value_type(node);
        let left = self.float_value(left_bits, value_type);
        let right = self.float_value(right_bits, value_type);
        self.emit_non_finite_if(node, left, value_type)?;
        self.emit_non_finite_if(node, right, value_type)?;
        let condition = if minimum {
            cranelift_codegen::ir::condcodes::FloatCC::LessThan
        } else {
            cranelift_codegen::ir::condcodes::FloatCC::GreaterThan
        };
        let choose_right = self.builder.ins().fcmp(condition, right, left);
        let result = self.builder.ins().select(choose_right, right, left);
        if node.fault_site.is_some() {
            self.emit_non_finite_if(node, result, value_type)?;
        }
        Ok(self.float_bits(result, value_type))
    }

    fn float_limit(
        &mut self,
        node: &CanonicalNode,
        value_bits: cranelift_codegen::ir::Value,
        low_bits: cranelift_codegen::ir::Value,
        high_bits: cranelift_codegen::ir::Value,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let value_type = value_type(node);
        let value = self.float_value(value_bits, value_type);
        let low = self.float_value(low_bits, value_type);
        let high = self.float_value(high_bits, value_type);
        let invalid = self.builder.ins().fcmp(
            cranelift_codegen::ir::condcodes::FloatCC::UnorderedOrGreaterThan,
            low,
            high,
        );
        self.emit_fault_if(node, invalid, RuntimeFaultCode::InvalidRuntimeRange)?;
        let below = self.builder.ins().fcmp(
            cranelift_codegen::ir::condcodes::FloatCC::LessThan,
            value,
            low,
        );
        let lower = self.builder.ins().select(below, low, value);
        let above = self.builder.ins().fcmp(
            cranelift_codegen::ir::condcodes::FloatCC::GreaterThan,
            lower,
            high,
        );
        let result = self.builder.ins().select(above, high, lower);
        self.emit_non_finite_if(node, result, value_type)?;
        Ok(self.float_bits(result, value_type))
    }

    fn explicit_conversion(
        &mut self,
        node: &CanonicalNode,
        value: cranelift_codegen::ir::Value,
        source_type: Option<&SemanticType>,
        target_type: Option<&SemanticType>,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        match (is_float(source_type), is_float(target_type)) {
            (false, false) => self.integer_conversion(node, value, source_type, target_type),
            (false, true) => self.integer_to_float(node, value, source_type, target_type),
            (true, false) => self.float_to_integer(node, value, source_type, target_type),
            (true, true) => self.float_conversion(node, value, source_type, target_type),
        }
    }

    fn integer_conversion(
        &mut self,
        node: &CanonicalNode,
        value: cranelift_codegen::ir::Value,
        source_type: Option<&SemanticType>,
        target_type: Option<&SemanticType>,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        integer_bits(source_type).ok_or(AotBuildError::InconsistentInput)?;
        let target_bits = integer_bits(target_type).ok_or(AotBuildError::InconsistentInput)?;
        let source_unsigned = is_unsigned(source_type);
        let target_unsigned = is_unsigned(target_type);
        let value = self.normalize(value, source_type);
        let negative = if source_unsigned {
            self.builder.ins().iconst(types::I8, 0)
        } else {
            self.builder.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                value,
                0,
            )
        };
        let above = if target_unsigned && target_bits == 64 {
            self.builder.ins().iconst(types::I8, 0)
        } else {
            let (_, target_maximum) = integer_bounds(target_bits, target_unsigned);
            let condition = if source_unsigned {
                cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan
            } else {
                cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThan
            };
            self.builder
                .ins()
                .icmp_imm_s(condition, value, target_maximum)
        };
        let below = if target_unsigned {
            negative
        } else if source_unsigned {
            self.builder.ins().iconst(types::I8, 0)
        } else {
            let (target_minimum, _) = integer_bounds(target_bits, false);
            self.builder.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                value,
                target_minimum,
            )
        };
        let invalid = self.builder.ins().bor(below, above);
        if node.fault_site.is_some() {
            self.emit_fault_if(node, invalid, RuntimeFaultCode::InvalidRuntimeRange)?;
        }
        Ok(self.normalize(value, target_type))
    }

    fn integer_to_float(
        &mut self,
        node: &CanonicalNode,
        value: cranelift_codegen::ir::Value,
        source_type: Option<&SemanticType>,
        target_type: Option<&SemanticType>,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        integer_bits(source_type).ok_or(AotBuildError::InconsistentInput)?;
        let value = self.normalize(value, source_type);
        let float_type = if matches!(target_type, Some(SemanticType::Real)) {
            types::F32
        } else {
            types::F64
        };
        let result = if is_unsigned(source_type) {
            self.builder.ins().fcvt_from_uint(float_type, value)
        } else {
            self.builder.ins().fcvt_from_sint(float_type, value)
        };
        if node.fault_site.is_some() {
            self.emit_non_finite_if(node, result, target_type)?;
        }
        Ok(self.float_bits(result, target_type))
    }

    fn float_to_integer(
        &mut self,
        node: &CanonicalNode,
        value_bits: cranelift_codegen::ir::Value,
        source_type: Option<&SemanticType>,
        target_type: Option<&SemanticType>,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let target_bits = integer_bits(target_type).ok_or(AotBuildError::InconsistentInput)?;
        let target_unsigned = is_unsigned(target_type);
        let value = self.float_value(value_bits, source_type);
        let lower = if target_unsigned {
            0.0
        } else {
            -(2_f64).powi(i32::from(target_bits) - 1)
        };
        let upper = if target_unsigned {
            (2_f64).powi(i32::from(target_bits))
        } else {
            (2_f64).powi(i32::from(target_bits) - 1)
        };
        let (lower_value, upper_value) = if matches!(source_type, Some(SemanticType::Real)) {
            let lower = if target_unsigned {
                0.0_f32
            } else {
                -(2_f32).powi(i32::from(target_bits) - 1)
            };
            let upper = if target_unsigned {
                (2_f32).powi(i32::from(target_bits))
            } else {
                (2_f32).powi(i32::from(target_bits) - 1)
            };
            (
                self.builder.ins().f32const(Ieee32::with_float(lower)),
                self.builder.ins().f32const(Ieee32::with_float(upper)),
            )
        } else {
            (
                self.builder.ins().f64const(Ieee64::with_float(lower)),
                self.builder.ins().f64const(Ieee64::with_float(upper)),
            )
        };
        let below = self.builder.ins().fcmp(
            cranelift_codegen::ir::condcodes::FloatCC::UnorderedOrLessThan,
            value,
            lower_value,
        );
        let at_or_above = self.builder.ins().fcmp(
            cranelift_codegen::ir::condcodes::FloatCC::GreaterThanOrEqual,
            value,
            upper_value,
        );
        let invalid = self.builder.ins().bor(below, at_or_above);
        self.emit_fault_if(node, invalid, RuntimeFaultCode::InvalidRuntimeRange)?;
        let converted = if target_unsigned {
            self.builder.ins().fcvt_to_uint_sat(types::I64, value)
        } else {
            self.builder.ins().fcvt_to_sint_sat(types::I64, value)
        };
        Ok(self.normalize(converted, target_type))
    }

    fn float_conversion(
        &mut self,
        node: &CanonicalNode,
        value_bits: cranelift_codegen::ir::Value,
        source_type: Option<&SemanticType>,
        target_type: Option<&SemanticType>,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let source = self.float_value(value_bits, source_type);
        let result = match (source_type, target_type) {
            (Some(SemanticType::Real), Some(SemanticType::Lreal)) => {
                self.builder.ins().fpromote(types::F64, source)
            }
            (Some(SemanticType::Lreal), Some(SemanticType::Real)) => {
                self.builder.ins().fdemote(types::F32, source)
            }
            (Some(SemanticType::Real), Some(SemanticType::Real))
            | (Some(SemanticType::Lreal), Some(SemanticType::Lreal)) => source,
            _ => return Err(AotBuildError::InconsistentInput),
        };
        if node.fault_site.is_some() {
            self.emit_non_finite_if(node, result, target_type)?;
        }
        Ok(self.float_bits(result, target_type))
    }

    fn checked_integer(
        &mut self,
        node: &CanonicalNode,
        operation: &str,
        left: cranelift_codegen::ir::Value,
        right: cranelift_codegen::ir::Value,
        value_type: Option<&SemanticType>,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let raw = match operation {
            "ADD" => self.builder.ins().iadd(left, right),
            "SUB" => self.builder.ins().isub(left, right),
            "MUL" => self.builder.ins().imul(left, right),
            _ => return Err(AotBuildError::InconsistentInput),
        };
        let bits = integer_bits(value_type).ok_or(AotBuildError::UnsupportedNode {
            node: node.id.0,
            kind: node.kind,
        })?;
        let overflow = if is_unsigned(value_type) {
            self.unsigned_overflow(operation, left, right, raw, bits)
        } else {
            self.signed_overflow(operation, left, right, raw, bits)
        };
        if node.fault_site.is_some() {
            self.emit_fault_if(node, overflow, RuntimeFaultCode::IntegerOverflow)?;
        }
        Ok(self.normalize(raw, value_type))
    }

    fn float_value(
        &mut self,
        bits: cranelift_codegen::ir::Value,
        value_type: Option<&SemanticType>,
    ) -> cranelift_codegen::ir::Value {
        if matches!(value_type, Some(SemanticType::Real)) {
            let reduced = self.builder.ins().ireduce(types::I32, bits);
            self.builder.ins().bitcast(
                types::F32,
                cranelift_codegen::ir::MemFlagsData::new(),
                reduced,
            )
        } else {
            self.builder
                .ins()
                .bitcast(types::F64, cranelift_codegen::ir::MemFlagsData::new(), bits)
        }
    }

    fn float_bits(
        &mut self,
        value: cranelift_codegen::ir::Value,
        value_type: Option<&SemanticType>,
    ) -> cranelift_codegen::ir::Value {
        if matches!(value_type, Some(SemanticType::Real)) {
            let bits = self.builder.ins().bitcast(
                types::I32,
                cranelift_codegen::ir::MemFlagsData::new(),
                value,
            );
            self.builder.ins().uextend(types::I64, bits)
        } else {
            self.builder.ins().bitcast(
                types::I64,
                cranelift_codegen::ir::MemFlagsData::new(),
                value,
            )
        }
    }

    fn emit_non_finite_if(
        &mut self,
        node: &CanonicalNode,
        value: cranelift_codegen::ir::Value,
        value_type: Option<&SemanticType>,
    ) -> Result<(), AotBuildError> {
        let unordered = self.builder.ins().fcmp(
            cranelift_codegen::ir::condcodes::FloatCC::Unordered,
            value,
            value,
        );
        let absolute = self.builder.ins().fabs(value);
        let maximum = if matches!(value_type, Some(SemanticType::Real)) {
            self.builder.ins().f32const(Ieee32::with_float(f32::MAX))
        } else {
            self.builder.ins().f64const(Ieee64::with_float(f64::MAX))
        };
        let infinite = self.builder.ins().fcmp(
            cranelift_codegen::ir::condcodes::FloatCC::GreaterThan,
            absolute,
            maximum,
        );
        let non_finite = self.builder.ins().bor(unordered, infinite);
        self.emit_fault_if(node, non_finite, RuntimeFaultCode::NonFiniteFloat)
    }

    fn saturating_integer(
        &mut self,
        operation: &str,
        left: cranelift_codegen::ir::Value,
        right: cranelift_codegen::ir::Value,
        value_type: Option<&SemanticType>,
    ) -> cranelift_codegen::ir::Value {
        let raw = match operation {
            "ADD" => self.builder.ins().iadd(left, right),
            "SUB" => self.builder.ins().isub(left, right),
            _ => self.builder.ins().imul(left, right),
        };
        let bits = integer_bits(value_type).unwrap_or(64);
        let overflow = if is_unsigned(value_type) {
            self.unsigned_overflow(operation, left, right, raw, bits)
        } else {
            self.signed_overflow(operation, left, right, raw, bits)
        };
        let (minimum, maximum) = integer_bounds(bits, is_unsigned(value_type));
        let minimum = self.builder.ins().iconst(types::I64, minimum);
        let maximum = self.builder.ins().iconst(types::I64, maximum);
        let normal = self.normalize(raw, value_type);
        let saturated = if is_unsigned(value_type) {
            if operation == "SUB" { minimum } else { maximum }
        } else if operation == "SUB" {
            let right_negative = self.builder.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                right,
                0,
            );
            self.builder.ins().select(right_negative, maximum, minimum)
        } else if operation == "MUL" {
            let left_negative = self.builder.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                left,
                0,
            );
            let right_negative = self.builder.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                right,
                0,
            );
            let result_negative = self.builder.ins().bxor(left_negative, right_negative);
            self.builder.ins().select(result_negative, minimum, maximum)
        } else {
            let left_negative = self.builder.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                left,
                0,
            );
            self.builder.ins().select(left_negative, minimum, maximum)
        };
        self.builder.ins().select(overflow, saturated, normal)
    }

    fn unsigned_overflow(
        &mut self,
        operation: &str,
        left: cranelift_codegen::ir::Value,
        right: cranelift_codegen::ir::Value,
        raw: cranelift_codegen::ir::Value,
        bits: u8,
    ) -> cranelift_codegen::ir::Value {
        if bits < 64 {
            let (_, maximum) = integer_bounds(bits, true);
            return match operation {
                "SUB" => self.builder.ins().icmp(
                    cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan,
                    left,
                    right,
                ),
                _ => self.builder.ins().icmp_imm_u(
                    cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThan,
                    raw,
                    maximum,
                ),
            };
        }
        match operation {
            "ADD" => self.builder.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan,
                raw,
                left,
            ),
            "SUB" => self.builder.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan,
                left,
                right,
            ),
            _ => {
                let high = self.builder.ins().umulhi(left, right);
                self.builder.ins().icmp_imm_u(
                    cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                    high,
                    0,
                )
            }
        }
    }

    fn signed_overflow(
        &mut self,
        operation: &str,
        left: cranelift_codegen::ir::Value,
        right: cranelift_codegen::ir::Value,
        raw: cranelift_codegen::ir::Value,
        bits: u8,
    ) -> cranelift_codegen::ir::Value {
        if bits < 64 {
            let (minimum, maximum) = integer_bounds(bits, false);
            let below = self.builder.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                raw,
                minimum,
            );
            let above = self.builder.ins().icmp_imm_s(
                cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThan,
                raw,
                maximum,
            );
            return self.builder.ins().bor(below, above);
        }
        if operation == "MUL" {
            let high = self.builder.ins().smulhi(left, right);
            let sign = self.builder.ins().sshr_imm_s(raw, 63);
            return self.builder.ins().icmp(
                cranelift_codegen::ir::condcodes::IntCC::NotEqual,
                high,
                sign,
            );
        }
        let first = self.builder.ins().bxor(left, raw);
        let second = if operation == "ADD" {
            self.builder.ins().bxor(right, raw)
        } else {
            self.builder.ins().bxor(left, right)
        };
        let combined = self.builder.ins().band(first, second);
        self.builder.ins().icmp_imm_s(
            cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
            combined,
            0,
        )
    }

    fn normalize(
        &mut self,
        value: cranelift_codegen::ir::Value,
        value_type: Option<&SemanticType>,
    ) -> cranelift_codegen::ir::Value {
        let Some(bits) = integer_bits(value_type) else {
            return value;
        };
        if bits == 64 {
            return value;
        }
        let shift = i64::from(64_u8.saturating_sub(bits));
        let shifted = self.builder.ins().ishl_imm_s(value, shift);
        if is_unsigned(value_type) || matches!(value_type, Some(SemanticType::Bool)) {
            self.builder.ins().ushr_imm_s(shifted, shift)
        } else {
            self.builder.ins().sshr_imm_s(shifted, shift)
        }
    }

    fn aggregate_expression(
        &mut self,
        node: &CanonicalNode,
        expected_layout: FixedTypeId,
    ) -> Result<AggregateSlot, AotBuildError> {
        match node.kind {
            AstNodeKind::Assignable => {
                let source = self.storage_location(node, self.activation)?;
                self.materialize_aggregate(source, expected_layout)
            }
            AstNodeKind::Literal => self.materialize_string_literal(node, expected_layout),
            AstNodeKind::QualifiedLiteral | AstNodeKind::ParenthesizedExpression => node
                .children
                .last()
                .ok_or(AotBuildError::InconsistentInput)
                .and_then(|child| self.aggregate_expression(child, expected_layout)),
            AstNodeKind::CallExpression => {
                let callee = node.children[0]
                    .children
                    .first()
                    .and_then(|value| value.symbol);
                if let Some(callee) = callee {
                    let activation = self.invoke_user_call(node, callee)?;
                    let source_layout = self.fixed_type_for_symbol(callee)?;
                    let zero = self.builder.ins().iconst(types::I64, 0);
                    self.materialize_aggregate(
                        StorageLocation {
                            symbol: callee,
                            offset: zero,
                            activation,
                            layout: source_layout,
                        },
                        expected_layout,
                    )
                } else if call_name(node).as_deref() == Some("CONCAT") {
                    self.materialize_concat(node, expected_layout)
                } else {
                    Err(AotBuildError::UnsupportedNode {
                        node: node.id.0,
                        kind: node.kind,
                    })
                }
            }
            _ => Err(AotBuildError::UnsupportedNode {
                node: node.id.0,
                kind: node.kind,
            }),
        }
    }

    fn materialize_aggregate(
        &mut self,
        source: StorageLocation,
        expected_layout: FixedTypeId,
    ) -> Result<AggregateSlot, AotBuildError> {
        let source_layout = self.layout(source.layout)?;
        let expected = self.layout(expected_layout)?;
        if source_layout.id != expected.id {
            return Err(AotBuildError::InconsistentInput);
        }
        let expected_size = expected.size_bytes;
        let result = self.create_aggregate_slot(expected_layout)?;
        let mut offset = 0_u64;
        while offset < expected_size {
            let width = aggregate_chunk_width(expected_size - offset);
            let dynamic_offset = self.builder.ins().iadd_imm_s(
                source.offset,
                i64::try_from(offset).map_err(|_| AotBuildError::InconsistentInput)?,
            );
            let value = self.load_at(source.symbol, dynamic_offset, width, source.activation);
            self.stack_store_bits(result.slot, offset, value, width)?;
            offset = offset
                .checked_add(u64::try_from(width).map_err(|_| AotBuildError::InconsistentInput)?)
                .ok_or(AotBuildError::InconsistentInput)?;
        }
        Ok(result)
    }

    fn materialize_string_literal(
        &mut self,
        node: &CanonicalNode,
        expected_layout: FixedTypeId,
    ) -> Result<AggregateSlot, AotBuildError> {
        let layout = self.layout(expected_layout)?.clone();
        let FixedTypeKind::String { wide, capacity, .. } = layout.kind else {
            return Err(AotBuildError::InconsistentInput);
        };
        let text = node
            .text
            .as_deref()
            .ok_or(AotBuildError::InconsistentInput)?;
        let (decoded, units) =
            crate::fault::decode_string(text, wide).ok_or(AotBuildError::InconsistentInput)?;
        if units > capacity {
            return Err(AotBuildError::InconsistentInput);
        }
        let size =
            usize::try_from(layout.size_bytes).map_err(|_| AotBuildError::InconsistentInput)?;
        let mut bytes = vec![0_u8; size];
        let length = u32::try_from(units).map_err(|_| AotBuildError::InconsistentInput)?;
        bytes
            .get_mut(..4)
            .ok_or(AotBuildError::InconsistentInput)?
            .copy_from_slice(&length.to_le_bytes());
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
            .ok_or(AotBuildError::InconsistentInput)?;
        bytes
            .get_mut(4..end)
            .ok_or(AotBuildError::InconsistentInput)?
            .copy_from_slice(&payload);
        let result = self.create_aggregate_slot(expected_layout)?;
        self.initialize_slot(result.slot, &bytes)?;
        Ok(result)
    }

    fn materialize_concat(
        &mut self,
        node: &CanonicalNode,
        expected_layout: FixedTypeId,
    ) -> Result<AggregateSlot, AotBuildError> {
        let [_, left, right] = node.children.as_slice() else {
            return Err(AotBuildError::InconsistentInput);
        };
        let left = self.aggregate_expression(left, expected_layout)?;
        let right = self.aggregate_expression(right, expected_layout)?;
        let result = self.create_aggregate_slot(expected_layout)?;
        let layout = self.layout(expected_layout)?;
        let FixedTypeKind::String { wide, capacity, .. } = layout.kind else {
            return Err(AotBuildError::InconsistentInput);
        };
        let destination = self.builder.ins().stack_addr(types::I64, result.slot, 0);
        let left = self.builder.ins().stack_addr(types::I64, left.slot, 0);
        let right = self.builder.ins().stack_addr(types::I64, right.slot, 0);
        let capacity = self.builder.ins().iconst(
            types::I32,
            i64::try_from(capacity).map_err(|_| AotBuildError::InconsistentInput)?,
        );
        let unit_width = self
            .builder
            .ins()
            .iconst(types::I32, if wide { 2 } else { 1 });
        let reference = self
            .module
            .declare_func_in_func(self.runtime.concat_string, self.builder.func);
        let call = self
            .builder
            .ins()
            .call(reference, &[destination, left, right, capacity, unit_width]);
        let status = self.builder.inst_results(call)[0];
        let failed = self.builder.ins().icmp_imm_s(
            cranelift_codegen::ir::condcodes::IntCC::NotEqual,
            status,
            0,
        );
        self.emit_fault_if(node, failed, RuntimeFaultCode::StringCapacityExceeded)?;
        Ok(result)
    }

    fn create_aggregate_slot(
        &mut self,
        layout: FixedTypeId,
    ) -> Result<AggregateSlot, AotBuildError> {
        let layout_value = self.layout(layout)?;
        let size = usize::try_from(layout_value.size_bytes)
            .map_err(|_| AotBuildError::InconsistentInput)?;
        let next = self.transient_stack_bytes.checked_add(size).ok_or(
            AotBuildError::CapacityExceeded {
                resource: "transient stack bytes",
                actual: usize::MAX,
                limit: self.transient_stack_limit,
            },
        )?;
        enforce("transient stack bytes", next, self.transient_stack_limit)?;
        let size_u32 = u32::try_from(size).map_err(|_| AotBuildError::CapacityExceeded {
            resource: "transient stack bytes",
            actual: size,
            limit: self.transient_stack_limit,
        })?;
        let align_shift = u8::try_from(layout_value.alignment_bytes.trailing_zeros())
            .map_err(|_| AotBuildError::InconsistentInput)?;
        let slot = self.builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size_u32,
            align_shift,
        ));
        self.transient_stack_bytes = next;
        Ok(AggregateSlot { slot, layout })
    }

    fn initialize_slot(&mut self, slot: StackSlot, bytes: &[u8]) -> Result<(), AotBuildError> {
        let mut offset = 0_usize;
        while offset < bytes.len() {
            let width = aggregate_chunk_width(
                u64::try_from(bytes.len() - offset)
                    .map_err(|_| AotBuildError::InconsistentInput)?,
            );
            let width_usize =
                usize::try_from(width).map_err(|_| AotBuildError::InconsistentInput)?;
            let mut encoded = [0_u8; 8];
            encoded[..width_usize].copy_from_slice(
                bytes
                    .get(offset..offset + width_usize)
                    .ok_or(AotBuildError::InconsistentInput)?,
            );
            let value = self
                .builder
                .ins()
                .iconst(types::I64, i64::from_le_bytes(encoded));
            self.stack_store_bits(
                slot,
                u64::try_from(offset).map_err(|_| AotBuildError::InconsistentInput)?,
                value,
                width,
            )?;
            offset += width_usize;
        }
        Ok(())
    }

    fn store_aggregate(
        &mut self,
        destination: StorageLocation,
        source: AggregateSlot,
    ) -> Result<(), AotBuildError> {
        let destination_layout = self.layout(destination.layout)?;
        let source_layout = self.layout(source.layout)?;
        if destination_layout.id != source_layout.id {
            return Err(AotBuildError::InconsistentInput);
        }
        let source_size = source_layout.size_bytes;
        let mut chunks = Vec::new();
        let mut offset = 0_u64;
        while offset < source_size {
            let width = aggregate_chunk_width(source_size - offset);
            chunks.push((
                offset,
                width,
                self.stack_load_bits(source.slot, offset, width)?,
            ));
            offset = offset
                .checked_add(u64::try_from(width).map_err(|_| AotBuildError::InconsistentInput)?)
                .ok_or(AotBuildError::InconsistentInput)?;
        }
        for (offset, width, value) in chunks {
            let dynamic_offset = self.builder.ins().iadd_imm_s(
                destination.offset,
                i64::try_from(offset).map_err(|_| AotBuildError::InconsistentInput)?,
            );
            self.store_at(
                destination.symbol,
                dynamic_offset,
                value,
                width,
                destination.activation,
            );
        }
        Ok(())
    }

    fn stack_store_bits(
        &mut self,
        slot: StackSlot,
        offset: u64,
        value: cranelift_codegen::ir::Value,
        width: i64,
    ) -> Result<(), AotBuildError> {
        let value = match width {
            1 => self.builder.ins().ireduce(types::I8, value),
            2 => self.builder.ins().ireduce(types::I16, value),
            4 => self.builder.ins().ireduce(types::I32, value),
            8 => value,
            _ => return Err(AotBuildError::InconsistentInput),
        };
        self.builder.ins().stack_store(
            types::I64,
            value,
            slot,
            i32::try_from(offset).map_err(|_| AotBuildError::InconsistentInput)?,
        );
        Ok(())
    }

    fn stack_load_bits(
        &mut self,
        slot: StackSlot,
        offset: u64,
        width: i64,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let value_type = match width {
            1 => types::I8,
            2 => types::I16,
            4 => types::I32,
            8 => types::I64,
            _ => return Err(AotBuildError::InconsistentInput),
        };
        let value = self.builder.ins().stack_load(
            types::I64,
            value_type,
            slot,
            i32::try_from(offset).map_err(|_| AotBuildError::InconsistentInput)?,
        );
        Ok(if width == 8 {
            value
        } else {
            self.builder.ins().uextend(types::I64, value)
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keeps ordered FB input/call/output semantics and aggregate handling together"
    )]
    fn function_block_call(&mut self, node: &CanonicalNode) -> Result<(), AotBuildError> {
        let instance = node
            .children
            .first()
            .and_then(root_symbol)
            .ok_or(AotBuildError::InconsistentInput)?;
        let instance_symbol = self
            .symbols
            .get(&instance)
            .ok_or(AotBuildError::InconsistentInput)?;
        let Some(SemanticType::FunctionBlock {
            declaration: callee,
        }) = instance_symbol.declared_type.as_ref()
        else {
            return Err(AotBuildError::InconsistentInput);
        };
        let declaration = self
            .declared
            .get(callee)
            .ok_or(AotBuildError::InconsistentInput)?;
        let activation = self.builder.ins().iconst(types::I32, i64::from(instance.0));
        self.reset_frame(*callee, activation);
        for argument in node.children.iter().skip(1) {
            let [name, value] = argument.children.as_slice() else {
                return Err(AotBuildError::InconsistentInput);
            };
            let parameter_kind = match argument.kind {
                AstNodeKind::InputArgument => SemanticSymbolKind::InputVariable,
                AstNodeKind::OutputArgument => SemanticSymbolKind::OutputVariable,
                _ => return Err(AotBuildError::InconsistentInput),
            };
            let parameter = self.parameter_symbol(*callee, name, parameter_kind)?;
            match argument.kind {
                AstNodeKind::InputArgument => {
                    let layout = self.fixed_type_for_symbol(parameter)?;
                    if self.is_scalar_layout(layout)? {
                        let evaluated = self.expression(value)?;
                        self.store_with_activation(
                            parameter,
                            0,
                            evaluated,
                            self.symbol_width(parameter),
                            activation,
                        )?;
                    } else {
                        let evaluated = self.aggregate_expression(value, layout)?;
                        let zero = self.builder.ins().iconst(types::I64, 0);
                        self.store_aggregate(
                            StorageLocation {
                                symbol: parameter,
                                offset: zero,
                                activation,
                                layout,
                            },
                            evaluated,
                        )?;
                    }
                }
                AstNodeKind::OutputArgument => {}
                _ => return Err(AotBuildError::InconsistentInput),
            }
        }
        self.emit_checkpoint_kind(node, |kind| {
            matches!(kind, CheckpointSiteKind::BeforePouCall { .. })
        })?;
        self.call_pou(declaration.id, activation)?;
        self.emit_checkpoint_kind(node, |kind| {
            matches!(kind, CheckpointSiteKind::AfterPouCall { .. })
        })?;
        for argument in node.children.iter().skip(1) {
            if argument.kind != AstNodeKind::OutputArgument {
                continue;
            }
            let [name, target] = argument.children.as_slice() else {
                return Err(AotBuildError::InconsistentInput);
            };
            let parameter =
                self.parameter_symbol(*callee, name, SemanticSymbolKind::OutputVariable)?;
            let parameter_layout = self.fixed_type_for_symbol(parameter)?;
            let target = self.storage_location(target, self.activation)?;
            if self.is_scalar_layout(parameter_layout)? {
                let output = self.load_with_activation(
                    parameter,
                    0,
                    self.symbol_width(parameter),
                    activation,
                )?;
                let width = self.layout(target.layout)?.size_bytes;
                self.store_at(
                    target.symbol,
                    target.offset,
                    output,
                    i64::try_from(width).map_err(|_| AotBuildError::InconsistentInput)?,
                    target.activation,
                );
            } else {
                let zero = self.builder.ins().iconst(types::I64, 0);
                let output = self.materialize_aggregate(
                    StorageLocation {
                        symbol: parameter,
                        offset: zero,
                        activation,
                        layout: parameter_layout,
                    },
                    target.layout,
                )?;
                self.store_aggregate(target, output)?;
            }
        }
        Ok(())
    }

    fn input_symbols(&self, owner: SymbolId) -> Vec<(SymbolId, Option<SemanticType>)> {
        self.symbols
            .values()
            .copied()
            .filter(|symbol| {
                symbol.owner == Some(owner) && symbol.kind == SemanticSymbolKind::InputVariable
            })
            .map(|symbol| (symbol.id, symbol.declared_type.clone()))
            .collect()
    }

    fn parameter_symbol(
        &self,
        owner: SymbolId,
        name: &CanonicalNode,
        kind: SemanticSymbolKind,
    ) -> Result<SymbolId, AotBuildError> {
        let canonical_name = name
            .text
            .as_deref()
            .ok_or(AotBuildError::InconsistentInput)?;
        self.symbols
            .values()
            .copied()
            .find(|symbol| {
                symbol.owner == Some(owner)
                    && symbol.kind == kind
                    && symbol.canonical_name == canonical_name
            })
            .map(|symbol| symbol.id)
            .ok_or(AotBuildError::InconsistentInput)
    }

    fn symbol_width(&self, symbol: SymbolId) -> i64 {
        self.symbols
            .get(&symbol)
            .and_then(|entry| entry.declared_type.as_ref())
            .map_or(8, |value_type| value_width(Some(value_type)))
    }

    fn scalar_location(
        &mut self,
        node: &CanonicalNode,
    ) -> Result<(SymbolId, cranelift_codegen::ir::Value, i64), AotBuildError> {
        let location = self.storage_location(node, self.activation)?;
        let layout = self.layout(location.layout)?;
        if !matches!(
            layout.kind,
            FixedTypeKind::Scalar { .. } | FixedTypeKind::Enumeration { .. }
        ) || !matches!(layout.size_bytes, 1 | 2 | 4 | 8)
        {
            return Err(AotBuildError::UnsupportedNode {
                node: node.id.0,
                kind: node.kind,
            });
        }
        Ok((
            location.symbol,
            location.offset,
            i64::try_from(layout.size_bytes).map_err(|_| AotBuildError::InconsistentInput)?,
        ))
    }

    fn storage_location(
        &mut self,
        node: &CanonicalNode,
        activation: cranelift_codegen::ir::Value,
    ) -> Result<StorageLocation, AotBuildError> {
        if node.kind != AstNodeKind::Assignable {
            return Err(AotBuildError::InconsistentInput);
        }
        let qualified = node
            .children
            .first()
            .ok_or(AotBuildError::InconsistentInput)?;
        let root = qualified
            .children
            .first()
            .and_then(|identifier| identifier.symbol)
            .or(qualified.symbol)
            .ok_or(AotBuildError::InconsistentInput)?;
        let mut value_type = self.fixed_type_for_symbol(root)?;
        let mut offset = self.builder.ins().iconst(types::I64, 0);
        for field in qualified.children.iter().skip(1) {
            let name = field
                .text
                .as_deref()
                .ok_or(AotBuildError::InconsistentInput)?;
            let layout = self.field_layout(value_type, name)?;
            offset = self.builder.ins().iadd_imm_s(
                offset,
                i64::try_from(layout.offset_bytes).map_err(|_| AotBuildError::InconsistentInput)?,
            );
            value_type = layout.value_type;
        }
        for suffix in node.children.iter().skip(1) {
            match suffix.kind {
                AstNodeKind::FieldSuffix => {
                    let name = suffix
                        .children
                        .first()
                        .and_then(|field| field.text.as_deref())
                        .ok_or(AotBuildError::InconsistentInput)?;
                    let layout = self.field_layout(value_type, name)?;
                    offset = self.builder.ins().iadd_imm_s(
                        offset,
                        i64::try_from(layout.offset_bytes)
                            .map_err(|_| AotBuildError::InconsistentInput)?,
                    );
                    value_type = layout.value_type;
                }
                AstNodeKind::IndexSuffix => {
                    let kind = self.resolved_kind(value_type)?;
                    let FixedTypeKind::Array {
                        lower,
                        upper,
                        element_type,
                        element_stride_bytes,
                        ..
                    } = kind
                    else {
                        return Err(AotBuildError::InconsistentInput);
                    };
                    let index_node = suffix
                        .children
                        .first()
                        .ok_or(AotBuildError::InconsistentInput)?;
                    let index = self.expression(index_node)?;
                    let lower =
                        i64::try_from(lower).map_err(|_| AotBuildError::InconsistentInput)?;
                    let upper =
                        i64::try_from(upper).map_err(|_| AotBuildError::InconsistentInput)?;
                    if suffix.fault_site.is_some() {
                        let below = self.builder.ins().icmp_imm_s(
                            cranelift_codegen::ir::condcodes::IntCC::SignedLessThan,
                            index,
                            lower,
                        );
                        let above = self.builder.ins().icmp_imm_s(
                            cranelift_codegen::ir::condcodes::IntCC::SignedGreaterThan,
                            index,
                            upper,
                        );
                        let outside = self.builder.ins().bor(below, above);
                        self.emit_fault_if(
                            suffix,
                            outside,
                            RuntimeFaultCode::ArrayIndexOutOfBounds,
                        )?;
                    }
                    let relative = self.builder.ins().iadd_imm_s(index, -lower);
                    let stride = i64::try_from(element_stride_bytes)
                        .map_err(|_| AotBuildError::InconsistentInput)?;
                    let byte_offset = self.builder.ins().imul_imm_s(relative, stride);
                    offset = self.builder.ins().iadd(offset, byte_offset);
                    value_type = element_type;
                }
                _ => return Err(AotBuildError::InconsistentInput),
            }
        }
        Ok(StorageLocation {
            symbol: root,
            offset,
            activation,
            layout: value_type,
        })
    }

    fn fixed_type_for_symbol(&self, symbol: SymbolId) -> Result<FixedTypeId, AotBuildError> {
        let symbol = self
            .symbols
            .get(&symbol)
            .copied()
            .ok_or(AotBuildError::InconsistentInput)?;
        let value_type = symbol
            .declared_type
            .as_ref()
            .ok_or(AotBuildError::InconsistentInput)?;
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
        self.fixed_types
            .iter()
            .find(|layout| {
                if let Some(declaration) = declaration {
                    layout.declaration == Some(declaration)
                } else {
                    scalar_layout_matches(&layout.kind, value_type)
                }
            })
            .map(|layout| layout.id)
            .ok_or(AotBuildError::InconsistentInput)
    }

    fn resolved_layout(&self, mut id: FixedTypeId) -> Result<&FixedTypeLayout, AotBuildError> {
        for _ in 0..=self.fixed_types.len() {
            let layout = self
                .fixed_types
                .iter()
                .find(|layout| layout.id == id)
                .ok_or(AotBuildError::InconsistentInput)?;
            if let FixedTypeKind::Alias { target } = layout.kind {
                id = target;
            } else {
                return Ok(layout);
            }
        }
        Err(AotBuildError::InconsistentInput)
    }

    fn layout(&self, id: FixedTypeId) -> Result<&FixedTypeLayout, AotBuildError> {
        self.resolved_layout(id)
    }

    fn is_scalar_layout(&self, id: FixedTypeId) -> Result<bool, AotBuildError> {
        Ok(matches!(
            self.layout(id)?.kind,
            FixedTypeKind::Scalar { .. } | FixedTypeKind::Enumeration { .. }
        ))
    }

    fn resolved_kind(&self, id: FixedTypeId) -> Result<FixedTypeKind, AotBuildError> {
        Ok(self.resolved_layout(id)?.kind.clone())
    }

    fn field_layout(
        &self,
        id: FixedTypeId,
        canonical_name: &str,
    ) -> Result<FixedFieldLayout, AotBuildError> {
        let (FixedTypeKind::Structure { fields } | FixedTypeKind::FunctionBlock { fields, .. }) =
            &self.resolved_layout(id)?.kind
        else {
            return Err(AotBuildError::InconsistentInput);
        };
        fields
            .iter()
            .find(|field| field.name.eq_ignore_ascii_case(canonical_name))
            .cloned()
            .ok_or(AotBuildError::InconsistentInput)
    }

    fn load_at(
        &mut self,
        symbol: SymbolId,
        offset: cranelift_codegen::ir::Value,
        width: i64,
        activation: cranelift_codegen::ir::Value,
    ) -> cranelift_codegen::ir::Value {
        let reference = self
            .module
            .declare_func_in_func(self.runtime.read, self.builder.func);
        let symbol = self.builder.ins().iconst(types::I32, i64::from(symbol.0));
        let width = self.builder.ins().iconst(types::I32, width);
        let call = self.builder.ins().call(
            reference,
            &[self.context, self.task, activation, symbol, offset, width],
        );
        self.builder.inst_results(call)[0]
    }

    fn store_at(
        &mut self,
        symbol: SymbolId,
        offset: cranelift_codegen::ir::Value,
        value: cranelift_codegen::ir::Value,
        width: i64,
        activation: cranelift_codegen::ir::Value,
    ) {
        let reference = self
            .module
            .declare_func_in_func(self.runtime.write, self.builder.func);
        let symbol = self.builder.ins().iconst(types::I32, i64::from(symbol.0));
        let width = self.builder.ins().iconst(types::I32, width);
        self.builder.ins().call(
            reference,
            &[
                self.context,
                self.task,
                activation,
                symbol,
                offset,
                width,
                value,
            ],
        );
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "keeps all Runtime callback emissions on one fallible lowering interface"
    )]
    fn load_with_activation(
        &mut self,
        symbol: SymbolId,
        offset: i64,
        width: i64,
        activation: cranelift_codegen::ir::Value,
    ) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
        let reference = self
            .module
            .declare_func_in_func(self.runtime.read, self.builder.func);
        let symbol = self.builder.ins().iconst(types::I32, i64::from(symbol.0));
        let offset = self.builder.ins().iconst(types::I64, offset);
        let width = self.builder.ins().iconst(types::I32, width);
        let call = self.builder.ins().call(
            reference,
            &[self.context, self.task, activation, symbol, offset, width],
        );
        Ok(self.builder.inst_results(call)[0])
    }

    fn store(
        &mut self,
        symbol: SymbolId,
        offset: i64,
        value: cranelift_codegen::ir::Value,
        width: i64,
    ) -> Result<(), AotBuildError> {
        self.store_with_activation(symbol, offset, value, width, self.activation)
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "keeps all Runtime callback emissions on one fallible lowering interface"
    )]
    fn store_with_activation(
        &mut self,
        symbol: SymbolId,
        offset: i64,
        value: cranelift_codegen::ir::Value,
        width: i64,
        activation: cranelift_codegen::ir::Value,
    ) -> Result<(), AotBuildError> {
        let reference = self
            .module
            .declare_func_in_func(self.runtime.write, self.builder.func);
        let symbol = self.builder.ins().iconst(types::I32, i64::from(symbol.0));
        let offset = self.builder.ins().iconst(types::I64, offset);
        let width = self.builder.ins().iconst(types::I32, width);
        self.builder.ins().call(
            reference,
            &[
                self.context,
                self.task,
                activation,
                symbol,
                offset,
                width,
                value,
            ],
        );
        Ok(())
    }

    fn reset_frame(&mut self, pou: SymbolId, activation: cranelift_codegen::ir::Value) {
        let reference = self
            .module
            .declare_func_in_func(self.runtime.reset_frame, self.builder.func);
        let pou = self.builder.ins().iconst(types::I32, i64::from(pou.0));
        self.builder
            .ins()
            .call(reference, &[self.context, self.task, activation, pou]);
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "keeps internal calls aligned with fallible checkpoint lowering"
    )]
    fn call_pou(
        &mut self,
        callee: FuncId,
        activation: cranelift_codegen::ir::Value,
    ) -> Result<(), AotBuildError> {
        let reference = self.module.declare_func_in_func(callee, self.builder.func);
        let call = self
            .builder
            .ins()
            .call(reference, &[self.context, self.task, activation]);
        let status = self.builder.inst_results(call)[0];
        let continue_block = self.builder.create_block();
        let stop = self.builder.create_block();
        let completed = self.builder.ins().icmp_imm_s(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            status,
            STATUS_COMPLETED,
        );
        self.builder
            .ins()
            .brif(completed, continue_block, &[], stop, &[]);
        self.builder.switch_to_block(stop);
        let args = [BlockArg::from(status)];
        self.builder.ins().jump(self.exit, &args);
        self.builder.switch_to_block(continue_block);
        Ok(())
    }

    fn emit_checkpoint_kind(
        &mut self,
        node: &CanonicalNode,
        selected: impl Fn(CheckpointSiteKind) -> bool,
    ) -> Result<(), AotBuildError> {
        let sites = self.checkpoints.get(&node.id).cloned().unwrap_or_default();
        let matching = sites
            .into_iter()
            .filter(|(_, kind)| selected(*kind))
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(AotBuildError::InconsistentInput);
        }
        self.emit_checkpoint(matching[0].0)
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "preserves a fallible insertion boundary for future ABI-minor validation"
    )]
    fn emit_checkpoint(&mut self, site: CheckpointSiteId) -> Result<(), AotBuildError> {
        let reference = self
            .module
            .declare_func_in_func(self.runtime.checkpoint, self.builder.func);
        let site = self.builder.ins().iconst(types::I32, i64::from(site.0));
        let call = self.builder.ins().call(reference, &[self.context, site]);
        let status = self.builder.inst_results(call)[0];
        let continue_block = self.builder.create_block();
        let stop = self.builder.create_block();
        let keep_running = self.builder.ins().icmp_imm_s(
            cranelift_codegen::ir::condcodes::IntCC::Equal,
            status,
            0,
        );
        self.builder
            .ins()
            .brif(keep_running, continue_block, &[], stop, &[]);
        self.builder.switch_to_block(stop);
        self.jump_status(STATUS_CHECKPOINT_STOP);
        self.builder.switch_to_block(continue_block);
        Ok(())
    }

    fn emit_fault_if(
        &mut self,
        node: &CanonicalNode,
        condition: cranelift_codegen::ir::Value,
        fault: RuntimeFaultCode,
    ) -> Result<(), AotBuildError> {
        let site = node.fault_site.ok_or(AotBuildError::InconsistentInput)?;
        let failed = self.builder.create_block();
        let continue_block = self.builder.create_block();
        self.builder
            .ins()
            .brif(condition, failed, &[], continue_block, &[]);
        self.builder.switch_to_block(failed);
        let reference = self
            .module
            .declare_func_in_func(self.runtime.fault, self.builder.func);
        let site = self.builder.ins().iconst(types::I32, i64::from(site.0));
        let code = self.builder.ins().iconst(types::I32, fault_number(fault));
        self.builder
            .ins()
            .call(reference, &[self.context, site, code]);
        self.jump_status(STATUS_FAULTED);
        self.builder.switch_to_block(continue_block);
        Ok(())
    }

    fn jump_status(&mut self, status: i64) {
        let status = self.builder.ins().iconst(types::I32, status);
        let args = [BlockArg::from(status)];
        self.builder.ins().jump(self.exit, &args);
        let unreachable = self.builder.create_block();
        self.builder.switch_to_block(unreachable);
    }

    fn location(&mut self, node: &CanonicalNode) {
        self.builder.set_srcloc(SourceLoc::new(node.id.0));
    }
}

fn checkpoint_sites(
    plan: &CheckpointPlan,
) -> BTreeMap<CanonicalNodeId, Vec<(CheckpointSiteId, CheckpointSiteKind)>> {
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

fn literal(
    builder: &mut FunctionBuilder<'_>,
    node: &CanonicalNode,
    value_type: Option<&SemanticType>,
) -> Result<cranelift_codegen::ir::Value, AotBuildError> {
    let text = node
        .text
        .as_deref()
        .ok_or(AotBuildError::InconsistentInput)?;
    if text.eq_ignore_ascii_case("TRUE") {
        return Ok(builder.ins().iconst(types::I64, 1));
    }
    if text.eq_ignore_ascii_case("FALSE") {
        return Ok(builder.ins().iconst(types::I64, 0));
    }
    let normalized = text.replace('_', "");
    if matches!(value_type, Some(SemanticType::Real)) {
        let value = normalized
            .parse::<f32>()
            .map_err(|_| AotBuildError::UnsupportedNode {
                node: node.id.0,
                kind: node.kind,
            })?;
        return Ok(builder.ins().iconst(types::I64, i64::from(value.to_bits())));
    }
    if matches!(value_type, Some(SemanticType::Lreal)) {
        let value = normalized
            .parse::<f64>()
            .map_err(|_| AotBuildError::UnsupportedNode {
                node: node.id.0,
                kind: node.kind,
            })?;
        let bits = i64::from_ne_bytes(value.to_bits().to_ne_bytes());
        return Ok(builder.ins().iconst(types::I64, bits));
    }
    let (radix, digits) = if let Some(value) = normalized.strip_prefix("16#") {
        (16, value)
    } else if let Some(value) = normalized.strip_prefix("2#") {
        (2, value)
    } else {
        (10, normalized.as_str())
    };
    let value = if is_unsigned(value_type) {
        let value =
            u64::from_str_radix(digits, radix).map_err(|_| AotBuildError::UnsupportedNode {
                node: node.id.0,
                kind: node.kind,
            })?;
        i64::from_ne_bytes(value.to_ne_bytes())
    } else {
        i64::from_str_radix(digits, radix).map_err(|_| AotBuildError::UnsupportedNode {
            node: node.id.0,
            kind: node.kind,
        })?
    };
    Ok(builder.ins().iconst(types::I64, value))
}

fn root_symbol(node: &CanonicalNode) -> Option<SymbolId> {
    if let Some(symbol) = node.symbol {
        return Some(symbol);
    }
    node.children.iter().find_map(root_symbol)
}

fn value_type(node: &CanonicalNode) -> Option<&SemanticType> {
    node.value_type
        .as_ref()
        .or_else(|| node.children.iter().find_map(value_type))
}

fn value_width(value_type: Option<&SemanticType>) -> i64 {
    match value_type {
        Some(SemanticType::Bool | SemanticType::Sint | SemanticType::Usint) => 1,
        Some(SemanticType::Int | SemanticType::Uint) => 2,
        Some(
            SemanticType::Dint
            | SemanticType::Udint
            | SemanticType::Real
            | SemanticType::Enumeration { .. },
        ) => 4,
        _ => 8,
    }
}

fn integer_bits(value_type: Option<&SemanticType>) -> Option<u8> {
    match value_type {
        Some(SemanticType::Bool | SemanticType::Sint | SemanticType::Usint) => Some(8),
        Some(SemanticType::Int | SemanticType::Uint) => Some(16),
        Some(SemanticType::Dint | SemanticType::Udint | SemanticType::Enumeration { .. }) => {
            Some(32)
        }
        Some(SemanticType::Lint | SemanticType::Ulint) => Some(64),
        _ => None,
    }
}

fn is_unsigned(value_type: Option<&SemanticType>) -> bool {
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

fn is_float(value_type: Option<&SemanticType>) -> bool {
    matches!(value_type, Some(SemanticType::Real | SemanticType::Lreal))
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
        ) => expected.replace('_', "").parse::<u64>().ok() == Some(*capacity),
        _ => false,
    }
}

fn integer_bounds(bits: u8, unsigned: bool) -> (i64, i64) {
    if unsigned {
        if bits == 64 {
            (0, -1)
        } else {
            (0, (1_i64 << bits) - 1)
        }
    } else if bits == 64 {
        (i64::MIN, i64::MAX)
    } else {
        let maximum = (1_i64 << (bits - 1)) - 1;
        (-maximum - 1, maximum)
    }
}

const fn aggregate_chunk_width(remaining: u64) -> i64 {
    if remaining >= 8 {
        8
    } else if remaining >= 4 {
        4
    } else if remaining >= 2 {
        2
    } else {
        1
    }
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

fn fault_number(code: RuntimeFaultCode) -> i64 {
    match code {
        RuntimeFaultCode::IntegerOverflow => 1,
        RuntimeFaultCode::IntegerDivisionByZero => 2,
        RuntimeFaultCode::NonFiniteFloat => 3,
        RuntimeFaultCode::InvalidRuntimeRange => 4,
        RuntimeFaultCode::ArrayIndexOutOfBounds => 5,
        RuntimeFaultCode::StringCapacityExceeded => 6,
    }
}

fn preorder(root: &CanonicalNode) -> Vec<&CanonicalNode> {
    fn visit<'a>(node: &'a CanonicalNode, result: &mut Vec<&'a CanonicalNode>) {
        result.push(node);
        for child in &node.children {
            visit(child, result);
        }
    }
    let mut result = Vec::new();
    visit(root, &mut result);
    result
}

fn validate_object(
    bytes: &[u8],
    exports: &[TaskExport],
    limits: AotLimits,
) -> Result<Vec<RuntimeImport>, AotBuildError> {
    let file = object::File::parse(bytes).map_err(invalid_object)?;
    if file.format() != BinaryFormat::Elf
        || file.architecture() != Architecture::X86_64
        || file.endianness() != Endianness::Little
        || file.kind() != ObjectKind::Relocatable
        || !file.is_64()
    {
        return Err(AotBuildError::InvalidObject(
            "expected little-endian ELF64 x86-64 relocatable object".to_owned(),
        ));
    }
    let allowed = [
        READ_BITS,
        WRITE_BITS,
        CHECKPOINT,
        REPORT_FAULT,
        RESET_FRAME,
        CONCAT_STRING,
    ]
    .into_iter()
    .collect::<BTreeSet<_>>();
    let referenced = file
        .sections()
        .flat_map(|section| section.relocations())
        .filter_map(|(_, relocation)| match relocation.target() {
            RelocationTarget::Symbol(symbol) => Some(symbol),
            _ => None,
        })
        .collect::<Vec<_>>();
    let expected_exports = exports
        .iter()
        .map(|entry| entry.symbol.clone())
        .collect::<BTreeSet<_>>();
    let mut imports = BTreeSet::new();
    let mut actual_exports = BTreeSet::new();
    for symbol in file.symbols() {
        let name = symbol.name().map_err(invalid_object)?;
        if symbol.is_undefined() && !name.is_empty() {
            if !allowed.contains(name) {
                return Err(AotBuildError::InvalidObject(format!(
                    "unexpected undefined symbol `{name}`"
                )));
            }
            if referenced.contains(&symbol.index()) {
                imports.insert(name.to_owned());
            }
        } else if symbol.is_global() && !name.is_empty() {
            if !expected_exports.contains(name) {
                return Err(AotBuildError::InvalidObject(format!(
                    "unexpected defined global symbol `{name}`"
                )));
            }
            actual_exports.insert(name.to_owned());
        }
    }
    if actual_exports != expected_exports {
        return Err(AotBuildError::InvalidObject(
            "Task export set differs from the declared artifact".to_owned(),
        ));
    }
    let relocations = file
        .sections()
        .map(|section| section.relocations().count())
        .sum::<usize>();
    enforce("relocations", relocations, limits.max_relocations())?;
    Ok(imports
        .into_iter()
        .map(|symbol| RuntimeImport { symbol })
        .collect())
}

fn translate_native_ranges(
    bytes: &[u8],
    ranges: &mut [NativeCodeRange],
) -> Result<(), AotBuildError> {
    let file = object::File::parse(bytes).map_err(invalid_object)?;
    let offsets = file
        .symbols()
        .filter_map(|symbol| {
            let name = symbol.name().ok()?;
            if name.starts_with("aurora_st_pou_") && !symbol.is_undefined() {
                Some((name.to_owned(), (symbol.address(), symbol.size())))
            } else {
                None
            }
        })
        .collect::<BTreeMap<_, _>>();
    for range in ranges {
        let (base, size) = offsets
            .get(&range.function_symbol)
            .copied()
            .ok_or_else(|| {
                AotBuildError::InvalidObject(format!(
                    "native Source Map function `{}` is absent",
                    range.function_symbol
                ))
            })?;
        let function_end = base.checked_add(size).ok_or_else(|| {
            AotBuildError::InvalidObject("native function extent overflows u64".to_owned())
        })?;
        let start = base.checked_add(u64::from(range.start)).ok_or_else(|| {
            AotBuildError::InvalidObject("native Source Map start overflows u64".to_owned())
        })?;
        let end = base.checked_add(u64::from(range.end)).ok_or_else(|| {
            AotBuildError::InvalidObject("native Source Map end overflows u64".to_owned())
        })?;
        if start >= end || start < base || end > function_end {
            return Err(AotBuildError::InvalidObject(format!(
                "native Source Map range for `{}` lies outside its function",
                range.function_symbol
            )));
        }
        range.start = u32::try_from(start).map_err(|_| {
            AotBuildError::InvalidObject("native Source Map start exceeds u32".to_owned())
        })?;
        range.end = u32::try_from(end).map_err(|_| {
            AotBuildError::InvalidObject("native Source Map end exceeds u32".to_owned())
        })?;
    }
    Ok(())
}

fn enforce(resource: &'static str, actual: usize, limit: usize) -> Result<(), AotBuildError> {
    if actual > limit {
        Err(AotBuildError::CapacityExceeded {
            resource,
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}

fn task_symbol(task: TaskHandle) -> String {
    format!("aurora_st_task_{:08x}_v1", task.0)
}

fn pou_symbol(pou: SymbolId) -> String {
    format!("aurora_st_pou_{:08x}_v1", pou.0)
}

fn backend(error: impl std::fmt::Display) -> AotBuildError {
    AotBuildError::Backend(error.to_string())
}

fn invalid_object(error: impl std::fmt::Display) -> AotBuildError {
    AotBuildError::InvalidObject(error.to_string())
}

fn hex_digest(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        result.push(char::from(DIGITS[usize::from(byte >> 4)]));
        result.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CanonicalIrVersion, CanonicalSourceMapVersion, CheckpointPlanVersion, InitializationModel,
        NodeSourceEntry, SourceFileEntry, SourceFileId, SourceSpan, SymbolSourceEntry,
        TaskCheckpointPlan, TaskWorkBound,
    };

    fn limits(
        functions: usize,
        function_bytes: usize,
        object_bytes: usize,
        relocations: usize,
        native_ranges: usize,
    ) -> AotLimits {
        AotLimits::new(
            functions,
            function_bytes,
            object_bytes,
            relocations,
            native_ranges,
            1024 * 1024,
        )
        .unwrap_or_else(|error| unreachable!("test limits are non-zero: {error}"))
    }

    #[allow(clippy::too_many_lines)]
    fn artifact_set() -> (CanonicalStIr, CanonicalSourceMap, CheckpointPlan) {
        let program = SymbolId(0);
        let task = TaskHandle(7);
        let body = CanonicalNode {
            id: CanonicalNodeId(0),
            kind: AstNodeKind::StatementList,
            text: None,
            symbol: None,
            value_type: None,
            fault_site: None,
            loop_iterations: None,
            children: vec![CanonicalNode {
                id: CanonicalNodeId(1),
                kind: AstNodeKind::ReturnStatement,
                text: None,
                symbol: None,
                value_type: None,
                fault_site: None,
                loop_iterations: None,
                children: Vec::new(),
            }],
        };
        let ir = CanonicalStIr {
            schema_version: CanonicalIrVersion::preview_v1_0(),
            symbols: vec![SemanticSymbol {
                id: program,
                name: "Main".to_owned(),
                canonical_name: "main".to_owned(),
                kind: SemanticSymbolKind::Program,
                source_path: "main.st".to_owned(),
                span: SourceSpan { start: 0, end: 4 },
                owner: None,
                declared_type: None,
            }],
            types: Vec::new(),
            programs: Vec::new(),
            globals: Vec::new(),
            invocation_frames: Vec::new(),
            function_block_instances: Vec::new(),
            tags: Vec::new(),
            snapshot_dependencies: Vec::new(),
            fault_sites: Vec::new(),
            initialization: InitializationModel {
                globals: Vec::new(),
                tasks: Vec::new(),
                frames: Vec::new(),
                total_task_state_bytes: 0,
                total_global_bytes: 0,
                total_frame_bytes: 0,
                total_initialization_bytes: 0,
            },
            tasks: vec![TaskWorkBound {
                task,
                program,
                source_operations: 1,
            }],
            pous: vec![CanonicalPou {
                symbol: program,
                kind: CanonicalPouKind::Program,
                body,
            }],
        };
        let source_map = CanonicalSourceMap {
            schema_version: CanonicalSourceMapVersion::preview_v1_0(),
            sources: vec![SourceFileEntry {
                id: SourceFileId(0),
                path: "main.st".to_owned(),
                byte_length: 8,
            }],
            symbols: vec![SymbolSourceEntry {
                symbol: program,
                source: SourceFileId(0),
                span: SourceSpan { start: 0, end: 4 },
            }],
            nodes: vec![
                NodeSourceEntry {
                    node: CanonicalNodeId(0),
                    pou: program,
                    source: SourceFileId(0),
                    span: SourceSpan { start: 0, end: 8 },
                },
                NodeSourceEntry {
                    node: CanonicalNodeId(1),
                    pou: program,
                    source: SourceFileId(0),
                    span: SourceSpan { start: 4, end: 8 },
                },
            ],
            fault_sites: Vec::new(),
        };
        let plan = CheckpointPlan {
            schema_version: CheckpointPlanVersion::preview_v1_0(),
            pous: vec![crate::PouCheckpointPlan {
                pou: program,
                checkpoints: Vec::new(),
            }],
            tasks: vec![TaskCheckpointPlan {
                task,
                program,
                return_checkpoint: CheckpointSiteId(0),
            }],
            sites: vec![crate::CheckpointSite {
                id: CheckpointSiteId(0),
                pou: program,
                node: CanonicalNodeId(0),
                site: CheckpointSiteKind::TaskReturn { task },
            }],
        };
        (ir, source_map, plan)
    }

    #[test]
    fn emits_deterministic_linux_x64_object_without_task_return_callback() {
        let (ir, map, plan) = artifact_set();
        let generous = limits(8, 64 * 1024, 1024 * 1024, 128, 128);
        let first = compile_linux_x64_aot(&ir, &map, &plan, AotTarget::linux_x64_v1(), generous)
            .unwrap_or_else(|error| unreachable!("minimal IR compiles: {error}"));
        let second = compile_linux_x64_aot(&ir, &map, &plan, AotTarget::linux_x64_v1(), generous)
            .unwrap_or_else(|error| unreachable!("same IR compiles: {error}"));
        assert_eq!(first.object, second.object);
        assert_eq!(first.object_sha256, second.object_sha256);
        assert_eq!(first.native_source_map, second.native_source_map);
        assert_eq!(first.task_exports.len(), 1);

        let object = object::File::parse(&*first.object)
            .unwrap_or_else(|error| unreachable!("backend validates emitted object: {error}"));
        let checkpoint_relocations = object
            .symbols()
            .filter_map(|symbol| symbol.name().ok())
            .filter(|name| *name == CHECKPOINT)
            .count();
        assert_eq!(
            checkpoint_relocations, 1,
            "ABI declaration exists exactly once"
        );
        assert!(
            !first
                .native_source_map
                .ranges
                .iter()
                .any(|range| range.node == CanonicalNodeId(u32::MAX))
        );
    }

    #[test]
    fn every_aot_capacity_rejects_zero_and_function_boundary_is_exact() {
        assert!(AotLimits::new(0, 1, 1, 1, 1, 1).is_err());
        assert!(AotLimits::new(1, 0, 1, 1, 1, 1).is_err());
        assert!(AotLimits::new(1, 1, 0, 1, 1, 1).is_err());
        assert!(AotLimits::new(1, 1, 1, 0, 1, 1).is_err());
        assert!(AotLimits::new(1, 1, 1, 1, 0, 1).is_err());
        assert!(AotLimits::new(1, 1, 1, 1, 1, 0).is_err());

        let (ir, map, plan) = artifact_set();
        let exact = compile_linux_x64_aot(
            &ir,
            &map,
            &plan,
            AotTarget::linux_x64_v1(),
            limits(2, 64 * 1024, 1024 * 1024, 128, 128),
        );
        assert!(exact.is_ok());
        assert!(matches!(
            compile_linux_x64_aot(
                &ir,
                &map,
                &plan,
                AotTarget::linux_x64_v1(),
                limits(1, 64 * 1024, 1024 * 1024, 128, 128),
            ),
            Err(AotBuildError::CapacityExceeded {
                resource: "functions",
                actual: 2,
                limit: 1,
            })
        ));
    }

    #[test]
    fn rejects_unknown_target_and_mismatched_artifacts_atomically() {
        let (ir, mut map, plan) = artifact_set();
        let generous = limits(8, 64 * 1024, 1024 * 1024, 128, 128);
        assert!(matches!(
            compile_linux_x64_aot(
                &ir,
                &map,
                &plan,
                AotTarget {
                    target_triple: "x86_64-unknown-linux-gnu",
                    abi_major: 2,
                    abi_minor: 0,
                },
                generous,
            ),
            Err(AotBuildError::UnsupportedTarget)
        ));
        map.nodes.pop();
        assert!(matches!(
            compile_linux_x64_aot(&ir, &map, &plan, AotTarget::linux_x64_v1(), generous,),
            Err(AotBuildError::InconsistentInput)
        ));

        let (ir, mut map, plan) = artifact_set();
        map.nodes[1] = map.nodes[0];
        assert!(matches!(
            compile_linux_x64_aot(&ir, &map, &plan, AotTarget::linux_x64_v1(), generous,),
            Err(AotBuildError::InconsistentInput)
        ));

        let (ir, map, mut plan) = artifact_set();
        plan.sites.push(plan.sites[0]);
        assert!(matches!(
            compile_linux_x64_aot(&ir, &map, &plan, AotTarget::linux_x64_v1(), generous,),
            Err(AotBuildError::InconsistentInput)
        ));
    }
}
