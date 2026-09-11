//! Versioned Aurora ST frontend contracts.
//!
//! The parser is host-only compiler work: it may allocate within caller-supplied limits and is
//! never used on the cyclic execution path. A failed parse publishes diagnostics only; a partial
//! syntax tree cannot be mistaken for a deployable compiler artifact.

mod address;
mod aot;
mod ast;
mod bounds;
mod canonical;
mod checkpoint;
mod diagnostic;
mod differential;
mod fault;
mod fixed;
mod initialization;
mod lexer;
mod parser;
mod reference;
mod semantic;
mod source_map;

pub use address::{
    AddressAnalysisOutput, AddressBindingInputs, AddressBindingLimits, AddressLimitError,
    AddressSemanticModel, BitOrder, BoundTag, ByteOrder, DeviceBindingEntry, DeviceEndpoint,
    ExternalField, LocalHandle, LockedDevicePackage, LogicalAddress, LogicalArea, MappingDirection,
    MappingTransform, ProgramTaskBinding, ResolvedDeviceBinding, SnapshotDependency, StableId,
    TagCatalogEntry, TaskHandle, analyze_addresses,
};
pub use aot::{
    AOT_ABI_MAJOR, AOT_ABI_MINOR, AotArtifact, AotBuildError, AotLimitError, AotLimits, AotTarget,
    NativeCodeRange, NativeSourceMap, RuntimeImport, TaskExport, compile_linux_x64_aot,
};
pub use ast::{
    AST_SCHEMA_MAJOR, AST_SCHEMA_MINOR, AstNode, AstNodeKind, AstSerializationError, AstVersion,
    VersionedAst, to_canonical_json,
};
pub use bounds::{
    BoundedForLoop, CyclicWorkAnalysisOutput, CyclicWorkInputError, CyclicWorkLimitError,
    CyclicWorkLimits, CyclicWorkModel, TaskWorkBound, analyze_cyclic_work,
};
pub use canonical::{
    CANONICAL_ST_IR_MAJOR, CANONICAL_ST_IR_MINOR, CanonicalArtifactLimits, CanonicalFaultSite,
    CanonicalFaultSiteId, CanonicalIrInputError, CanonicalIrLimitError, CanonicalIrLimits,
    CanonicalIrOutput, CanonicalIrSerializationError, CanonicalIrVersion, CanonicalNode,
    CanonicalNodeId, CanonicalPou, CanonicalPouKind, CanonicalStIr, canonical_ir_to_json,
    lower_canonical_ir,
};
pub use checkpoint::{
    CHECKPOINT_PLAN_MAJOR, CHECKPOINT_PLAN_MINOR, CheckpointPlan, CheckpointPlanInputError,
    CheckpointPlanLimitError, CheckpointPlanLimits, CheckpointPlanSerializationError,
    CheckpointPlanVersion, CheckpointSite, CheckpointSiteId, CheckpointSiteKind, PouCheckpointPlan,
    TaskCheckpointPlan, checkpoint_plan_to_json,
};
pub use diagnostic::{
    Diagnostic, DiagnosticCode, DiagnosticSerializationError, SourcePosition, SourceSpan,
    diagnostics_to_canonical_json,
};
pub use differential::{
    DifferentialCycle, DifferentialDiagnostic, DifferentialFault, DifferentialKind,
    DifferentialMismatch, DifferentialStatus, DifferentialTrace, DifferentialValue,
    compare_differential_traces,
};
pub use fault::{
    FaultAnalysisOutput, FaultOperationKind, FaultSemanticModel, FaultSite, FaultSiteId,
    IntegerArithmeticError, IntegerArithmeticMode, IntegerOperation, IntegerType, RuntimeFaultCode,
    analyze_faults, evaluate_integer_operation, validate_array_index,
};
pub use fixed::{
    FixedAnalysisOutput, FixedDataLimitError, FixedDataLimits, FixedEnumerationMember,
    FixedFieldLayout, FixedFieldStorage, FixedGlobalLayout, FixedInitializer, FixedSemanticModel,
    FixedTypeId, FixedTypeKind, FixedTypeLayout, InvocationFrameLayout,
    StaticFunctionBlockInstance, StaticProgramLayout, analyze_fixed,
};
pub use initialization::{
    FrameInitializationImage, GlobalInitializationImage, InitializationInputError,
    InitializationLimitError, InitializationLimits, InitializationModel, InitializationOutput,
    TaskInitializationImage, build_initialization_images,
};
pub use parser::{LimitConfigurationError, ParseOutput, ParserLimits, parse};
pub use reference::{
    ReferenceCycleRequest, ReferenceExecutionError, ReferenceExecutor, ReferenceInput,
    ReferenceLimitError, ReferenceLimits,
};
pub use semantic::{
    AnalysisInputError, AnalysisOutput, ResolvedReference, SemanticModel, SemanticSource,
    SemanticSymbol, SemanticSymbolKind, SemanticType, SymbolId, TypedExpression, analyze,
};
pub use source_map::{
    CANONICAL_SOURCE_MAP_MAJOR, CANONICAL_SOURCE_MAP_MINOR, CanonicalSourceMap,
    CanonicalSourceMapInputError, CanonicalSourceMapLimitError, CanonicalSourceMapLimits,
    CanonicalSourceMapSerializationError, CanonicalSourceMapVersion, FaultSourceEntry,
    NodeSourceEntry, SourceFileEntry, SourceFileId, SymbolSourceEntry,
    canonical_source_map_to_json,
};
