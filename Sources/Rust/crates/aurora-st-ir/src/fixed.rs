use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::ast::{AstNode, AstNodeKind};
use crate::diagnostic::{make_diagnostic, sort_diagnostics};
use crate::fault::{
    IntegerArithmeticError, IntegerArithmeticMode, IntegerOperation, IntegerType,
    evaluate_integer_operation,
};
use crate::semantic::{AnalysisInputError, SemanticModel, SemanticSource, SemanticSymbolKind};
use crate::{Diagnostic, DiagnosticCode, SourceSpan, SymbolId, analyze};

const LENGTH_PREFIX_BYTES: u8 = 4;

/// Explicit non-zero Target Profile limits used by the R1-03 fixed-data pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedDataLimits {
    string_payload_bytes: u64,
    wstring_code_units: u64,
    array_element_count: u64,
    type_byte_ceiling: u64,
    fb_instances_per_program: u64,
    program_static_budget: u64,
    invocation_frame_budget: u64,
}

impl FixedDataLimits {
    /// Validates every mandatory fixed-data limit.
    ///
    /// # Errors
    ///
    /// Returns [`FixedDataLimitError`] when any limit is zero.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        max_string_payload_bytes: u64,
        max_wstring_code_units: u64,
        max_array_elements: u64,
        max_type_size_bytes: u64,
        max_static_fb_instances_per_program: u64,
        max_program_static_bytes: u64,
        max_invocation_frame_bytes: u64,
    ) -> Result<Self, FixedDataLimitError> {
        if max_string_payload_bytes == 0 {
            return Err(FixedDataLimitError::ZeroStringPayloadBytes);
        }
        if max_wstring_code_units == 0 {
            return Err(FixedDataLimitError::ZeroWstringCodeUnits);
        }
        if max_array_elements == 0 {
            return Err(FixedDataLimitError::ZeroArrayElements);
        }
        if max_type_size_bytes == 0 {
            return Err(FixedDataLimitError::ZeroTypeSizeBytes);
        }
        if max_static_fb_instances_per_program == 0 {
            return Err(FixedDataLimitError::ZeroStaticFunctionBlockInstances);
        }
        if max_program_static_bytes == 0 {
            return Err(FixedDataLimitError::ZeroProgramStaticBytes);
        }
        if max_invocation_frame_bytes == 0 {
            return Err(FixedDataLimitError::ZeroInvocationFrameBytes);
        }
        Ok(Self {
            string_payload_bytes: max_string_payload_bytes,
            wstring_code_units: max_wstring_code_units,
            array_element_count: max_array_elements,
            type_byte_ceiling: max_type_size_bytes,
            fb_instances_per_program: max_static_fb_instances_per_program,
            program_static_budget: max_program_static_bytes,
            invocation_frame_budget: max_invocation_frame_bytes,
        })
    }

    /// Maximum UTF-8 payload bytes in one STRING.
    #[must_use]
    pub const fn max_string_payload_bytes(self) -> u64 {
        self.string_payload_bytes
    }

    /// Maximum UTF-16 code units in one WSTRING.
    #[must_use]
    pub const fn max_wstring_code_units(self) -> u64 {
        self.wstring_code_units
    }

    /// Maximum number of elements in one ARRAY.
    #[must_use]
    pub const fn max_array_elements(self) -> u64 {
        self.array_element_count
    }

    /// Maximum canonical byte size of one type.
    #[must_use]
    pub const fn max_type_size_bytes(self) -> u64 {
        self.type_byte_ceiling
    }

    /// Maximum expanded static FB instances in one Program template.
    #[must_use]
    pub const fn max_static_fb_instances_per_program(self) -> u64 {
        self.fb_instances_per_program
    }

    /// Maximum canonical static bytes in one Program template.
    #[must_use]
    pub const fn max_program_static_bytes(self) -> u64 {
        self.program_static_budget
    }

    /// Maximum bytes in any Function, FB, or Program invocation frame.
    #[must_use]
    pub const fn max_invocation_frame_bytes(self) -> u64 {
        self.invocation_frame_budget
    }
}

/// Invalid mandatory R1-03 Target Profile limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FixedDataLimitError {
    /// STRING payload limit is zero.
    #[error("max_string_payload_bytes must be non-zero")]
    ZeroStringPayloadBytes,
    /// WSTRING code-unit limit is zero.
    #[error("max_wstring_code_units must be non-zero")]
    ZeroWstringCodeUnits,
    /// ARRAY element limit is zero.
    #[error("max_array_elements must be non-zero")]
    ZeroArrayElements,
    /// Per-type byte limit is zero.
    #[error("max_type_size_bytes must be non-zero")]
    ZeroTypeSizeBytes,
    /// Per-Program static FB-instance limit is zero.
    #[error("max_static_fb_instances_per_program must be non-zero")]
    ZeroStaticFunctionBlockInstances,
    /// Per-Program static byte limit is zero.
    #[error("max_program_static_bytes must be non-zero")]
    ZeroProgramStaticBytes,
    /// Invocation-frame byte limit is zero.
    #[error("max_invocation_frame_bytes must be non-zero")]
    ZeroInvocationFrameBytes,
}

/// Stable identifier for one canonical fixed type layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct FixedTypeId(
    /// Zero-based identifier value.
    pub u32,
);

/// Storage role of a canonical layout field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FixedFieldStorage {
    /// STRUCT member.
    Structure,
    /// POU input copied at invocation start.
    Input,
    /// FB/Program output retained in static storage.
    Output,
    /// FB/Program persistent local state.
    State,
    /// Per-invocation temporary or Function-local value.
    Temporary,
}

/// Reproducible initialization plan for a fixed value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FixedInitializer {
    /// All scalar bytes are zero.
    Zero,
    /// Length, payload, and padding are zero.
    EmptyString,
    /// First declared enum member is selected.
    FirstEnumerationMember {
        /// Member spelling.
        name: String,
        /// Canonical DINT representation.
        value: i32,
    },
    /// Child layouts recursively provide defaults and all padding is zero.
    Aggregate,
    /// A later arithmetic/IR pass lowers the already type-checked expression at this span; it must
    /// zero the complete destination first so unused payload and padding remain deterministic.
    ExplicitExpression {
        /// Normalized project-relative source containing the expression.
        source_path: String,
        /// Exact source expression span.
        span: SourceSpan,
    },
}

/// One field in a STRUCT, FB, Program, or invocation-frame layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixedFieldLayout {
    /// Original field or variable spelling.
    pub name: String,
    /// Declaration span.
    pub span: SourceSpan,
    /// Storage role.
    pub storage: FixedFieldStorage,
    /// Referenced fixed layout.
    pub value_type: FixedTypeId,
    /// Byte offset in the containing layout or frame.
    pub offset_bytes: u64,
    /// Declaration/default initialization plan.
    pub initializer: FixedInitializer,
}

/// One resolved enumeration item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixedEnumerationMember {
    /// Original member spelling.
    pub name: String,
    /// Declaration span.
    pub span: SourceSpan,
    /// Canonical DINT representation.
    pub value: i32,
}

/// Canonical fixed-layout category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FixedTypeKind {
    /// BOOL or fixed-width numeric scalar.
    Scalar {
        /// Canonical Aurora ST type spelling.
        name: String,
    },
    /// Fixed UTF-8/UTF-16 payload with a 4-byte little-endian length prefix.
    String {
        /// `false` for STRING and `true` for WSTRING.
        wide: bool,
        /// Payload bytes for STRING or UTF-16 code units for WSTRING.
        capacity: u64,
        /// Frozen length-prefix width; always 4 in Preview 1.0.
        length_prefix_bytes: u8,
    },
    /// Fixed-bound, fixed-stride array.
    Array {
        /// Inclusive lower bound.
        lower: i128,
        /// Inclusive upper bound.
        upper: i128,
        /// Exact element count.
        element_count: u64,
        /// Element layout.
        element_type: FixedTypeId,
        /// Canonical element stride.
        element_stride_bytes: u64,
    },
    /// Ordered, explicitly padded structure.
    Structure {
        /// Fields in declaration order.
        fields: Vec<FixedFieldLayout>,
    },
    /// DINT-backed nominal enumeration.
    Enumeration {
        /// Members in declaration order.
        members: Vec<FixedEnumerationMember>,
    },
    /// Named alias reusing the target layout exactly.
    Alias {
        /// Target fixed layout.
        target: FixedTypeId,
    },
    /// Static FB state/input/output layout plus a separate invocation frame.
    FunctionBlock {
        /// Persistent instance fields.
        fields: Vec<FixedFieldLayout>,
        /// Per-invocation fields.
        temporary_fields: Vec<FixedFieldLayout>,
        /// Canonical temporary-frame byte size.
        temporary_size_bytes: u64,
        /// Canonical temporary-frame alignment.
        temporary_alignment_bytes: u8,
    },
}

/// One canonical fixed type layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixedTypeLayout {
    /// Deterministic layout identifier.
    pub id: FixedTypeId,
    /// Named type or FB declaration, absent for anonymous/built-in layouts.
    pub declaration: Option<SymbolId>,
    /// Source containing the declaration/use that created this layout.
    pub source_path: String,
    /// Declaration/type span.
    pub span: SourceSpan,
    /// Canonical byte size including tail padding.
    pub size_bytes: u64,
    /// Canonical alignment in bytes.
    pub alignment_bytes: u8,
    /// Default used when a containing declaration has no explicit initializer.
    pub default_initializer: FixedInitializer,
    /// Layout category and children.
    pub kind: FixedTypeKind,
}

/// Canonical static Program template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaticProgramLayout {
    /// Program declaration.
    pub program: SymbolId,
    /// Source path.
    pub source_path: String,
    /// Program name span.
    pub span: SourceSpan,
    /// Persistent fields.
    pub fields: Vec<FixedFieldLayout>,
    /// Canonical persistent byte size.
    pub size_bytes: u64,
    /// Canonical persistent alignment.
    pub alignment_bytes: u8,
    /// Per-invocation fields.
    pub temporary_fields: Vec<FixedFieldLayout>,
    /// Canonical per-invocation byte size.
    pub temporary_size_bytes: u64,
    /// Canonical per-invocation alignment.
    pub temporary_alignment_bytes: u8,
}

/// Fixed type and initialization plan retained for one address-backed global declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixedGlobalLayout {
    /// Global declaration symbol.
    pub global: SymbolId,
    /// Source path.
    pub source_path: String,
    /// Global name span.
    pub span: SourceSpan,
    /// Fixed value layout supplied to R1-05 address binding.
    pub value_type: FixedTypeId,
    /// Declaration/default initialization plan, including `%M` reset state.
    pub initializer: FixedInitializer,
}

/// Function or POU invocation-frame layout not embedded in static instance bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvocationFrameLayout {
    /// Owning Function, FB, or Program declaration.
    pub pou: SymbolId,
    /// Source path.
    pub source_path: String,
    /// POU name span.
    pub span: SourceSpan,
    /// Frame fields in declaration order.
    pub fields: Vec<FixedFieldLayout>,
    /// Canonical frame size.
    pub size_bytes: u64,
    /// Canonical frame alignment.
    pub alignment_bytes: u8,
}

/// Stable identity and offset of one expanded FB instance in a Program template.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StaticFunctionBlockInstance {
    /// Zero-based Program-local instance identifier.
    pub instance_id: u32,
    /// Owning Program declaration.
    pub program: SymbolId,
    /// Canonical declaration/index path from the Program root.
    pub path: String,
    /// Referenced FB declaration.
    pub function_block: SymbolId,
    /// Byte offset in Program static storage.
    pub offset_bytes: u64,
    /// Instance layout size.
    pub size_bytes: u64,
}

/// Complete R1-03 model layered over the successful R1-02 semantic model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixedSemanticModel {
    /// R1-02 declarations, references, and expression types.
    pub semantics: SemanticModel,
    /// Canonical layouts in deterministic ID order.
    pub types: Vec<FixedTypeLayout>,
    /// Static Program templates.
    pub programs: Vec<StaticProgramLayout>,
    /// Address-independent global type and initialization associations for R1-05.
    pub globals: Vec<FixedGlobalLayout>,
    /// Function/FB/Program invocation frames.
    pub invocation_frames: Vec<InvocationFrameLayout>,
    /// Expanded FB instances, grouped by Program declaration order.
    pub function_block_instances: Vec<StaticFunctionBlockInstance>,
}

/// Atomic R1-03 analysis result; diagnostics and a model are mutually exclusive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedAnalysisOutput {
    /// Complete model, present only when all R1-02/R1-03 checks pass.
    pub model: Option<FixedSemanticModel>,
    /// Stable diagnostics sorted by path/byte/code.
    pub diagnostics: Vec<Diagnostic>,
}

/// Runs R1-02 semantic checks followed by fixed-capacity/layout/instance validation.
///
/// Explicit limits are mandatory. This pass is host-only and does not generate R1-04 Fault sites,
/// R1-05 address bindings, Canonical IR, AOT code, or runtime allocations.
///
/// # Errors
///
/// Returns [`AnalysisInputError`] for corrupt or mismatched AST/source inputs.
pub fn analyze_fixed(
    sources: &[SemanticSource<'_>],
    limits: FixedDataLimits,
) -> Result<FixedAnalysisOutput, AnalysisInputError> {
    let semantic_output = analyze(sources)?;
    let Some(semantics) = semantic_output.model else {
        return Ok(FixedAnalysisOutput {
            model: None,
            diagnostics: semantic_output.diagnostics,
        });
    };
    FixedAnalyzer::new(sources, semantics, limits).run()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DefinitionKind {
    Type,
    FunctionBlock,
}

#[derive(Debug, Clone, Copy)]
struct Definition<'a> {
    source_index: usize,
    node: &'a AstNode,
    name: &'a AstNode,
    symbol: SymbolId,
    kind: DefinitionKind,
    layout_id: FixedTypeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildState {
    Unvisited,
    Visiting,
    Done,
    Failed,
}

struct LayoutSlot {
    state: BuildState,
    layout: Option<FixedTypeLayout>,
    source_index: usize,
    span: SourceSpan,
}

struct BuiltFields {
    fields: Vec<FixedFieldLayout>,
    size_bytes: u64,
    alignment_bytes: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntegerKind {
    Signed(u8),
    Unsigned(u8),
    Untyped,
}

#[derive(Debug, Clone, Copy)]
struct ConstantInteger {
    value: i128,
    kind: IntegerKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConstantError {
    Dynamic,
    Invalid,
    InvalidCall,
    InvalidConversion,
    Overflow,
    DivisionByZero,
}

struct FixedAnalyzer<'a> {
    sources: Vec<SemanticSource<'a>>,
    semantics: SemanticModel,
    limits: FixedDataLimits,
    diagnostics: Vec<Diagnostic>,
    diagnostic_keys: BTreeSet<(usize, u32, u32, &'static str)>,
    definitions: Vec<Definition<'a>>,
    definitions_by_name: BTreeMap<String, usize>,
    layouts: Vec<LayoutSlot>,
    anonymous_layouts: BTreeMap<(usize, u32, u32), FixedTypeId>,
    builtin_layouts: BTreeMap<String, FixedTypeId>,
    programs: Vec<StaticProgramLayout>,
    globals: Vec<FixedGlobalLayout>,
    invocation_frames: Vec<InvocationFrameLayout>,
    instances: Vec<StaticFunctionBlockInstance>,
}

impl<'a> FixedAnalyzer<'a> {
    fn new(
        sources: &[SemanticSource<'a>],
        semantics: SemanticModel,
        limits: FixedDataLimits,
    ) -> Self {
        let mut ordered = sources.to_vec();
        ordered.sort_by(|left, right| {
            left.ast
                .source_path
                .as_bytes()
                .cmp(right.ast.source_path.as_bytes())
        });
        Self {
            sources: ordered,
            semantics,
            limits,
            diagnostics: Vec::new(),
            diagnostic_keys: BTreeSet::new(),
            definitions: Vec::new(),
            definitions_by_name: BTreeMap::new(),
            layouts: Vec::new(),
            anonymous_layouts: BTreeMap::new(),
            builtin_layouts: BTreeMap::new(),
            programs: Vec::new(),
            globals: Vec::new(),
            invocation_frames: Vec::new(),
            instances: Vec::new(),
        }
    }

    fn run(mut self) -> Result<FixedAnalysisOutput, AnalysisInputError> {
        self.collect_definitions()?;
        for index in 0..self.definitions.len() {
            let definition = self.definitions[index];
            self.ensure_definition(index, definition.source_index, definition.name.span)?;
        }
        self.validate_non_layout_types()?;
        self.build_pou_layouts()?;
        self.expand_program_instances()?;
        sort_diagnostics(&mut self.diagnostics);
        if self.diagnostics.is_empty() {
            let types = self
                .layouts
                .into_iter()
                .map(|slot| {
                    slot.layout
                        .ok_or_else(|| AnalysisInputError::InvalidAstShape {
                            source_path: self.sources[slot.source_index].ast.source_path.clone(),
                            span_start: slot.span.start,
                            span_end: slot.span.end,
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(FixedAnalysisOutput {
                model: Some(FixedSemanticModel {
                    semantics: self.semantics,
                    types,
                    programs: self.programs,
                    globals: self.globals,
                    invocation_frames: self.invocation_frames,
                    function_block_instances: self.instances,
                }),
                diagnostics: Vec::new(),
            })
        } else {
            Ok(FixedAnalysisOutput {
                model: None,
                diagnostics: self.diagnostics,
            })
        }
    }

    fn collect_definitions(&mut self) -> Result<(), AnalysisInputError> {
        for source_index in 0..self.sources.len() {
            let root = &self.sources[source_index].ast.root;
            for node in &root.children {
                match node.kind {
                    AstNodeKind::TypeBlock => {
                        for declaration in &node.children {
                            self.collect_definition(
                                source_index,
                                declaration,
                                DefinitionKind::Type,
                                SemanticSymbolKind::Type,
                            )?;
                        }
                    }
                    AstNodeKind::FunctionBlockDeclaration => self.collect_definition(
                        source_index,
                        node,
                        DefinitionKind::FunctionBlock,
                        SemanticSymbolKind::FunctionBlock,
                    )?,
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn collect_definition(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        kind: DefinitionKind,
        symbol_kind: SemanticSymbolKind,
    ) -> Result<(), AnalysisInputError> {
        let name = self.child(source_index, node, 0)?;
        let spelling = self.text(source_index, name)?;
        let symbol = self
            .symbol_at(source_index, name.span, symbol_kind)
            .ok_or_else(|| self.invalid_shape(source_index, name.span))?;
        let layout_id = self.allocate_slot(source_index, node.span)?;
        let index = self.definitions.len();
        self.definitions.push(Definition {
            source_index,
            node,
            name,
            symbol,
            kind,
            layout_id,
        });
        self.definitions_by_name
            .insert(spelling.to_ascii_lowercase(), index);
        Ok(())
    }

    fn allocate_slot(
        &mut self,
        source_index: usize,
        span: SourceSpan,
    ) -> Result<FixedTypeId, AnalysisInputError> {
        let id = FixedTypeId(
            u32::try_from(self.layouts.len()).map_err(|_| AnalysisInputError::TooManySymbols)?,
        );
        self.layouts.push(LayoutSlot {
            state: BuildState::Unvisited,
            layout: None,
            source_index,
            span,
        });
        Ok(id)
    }

    fn ensure_definition(
        &mut self,
        definition_index: usize,
        reference_source_index: usize,
        reference_span: SourceSpan,
    ) -> Result<Option<FixedTypeId>, AnalysisInputError> {
        let definition = self.definitions[definition_index];
        let slot_index = definition.layout_id.0 as usize;
        match self.layouts[slot_index].state {
            BuildState::Done => return Ok(Some(definition.layout_id)),
            BuildState::Failed => return Ok(None),
            BuildState::Visiting => {
                self.emit(
                    reference_source_index,
                    DiagnosticCode::RecursiveInstance,
                    reference_span,
                );
                return Ok(None);
            }
            BuildState::Unvisited => {}
        }
        self.layouts[slot_index].state = BuildState::Visiting;
        let result = match definition.kind {
            DefinitionKind::Type => {
                let type_node = self.child(definition.source_index, definition.node, 1)?;
                self.build_named_type(definition, type_node)
            }
            DefinitionKind::FunctionBlock => self.build_function_block(definition),
        }?;
        if let Some(layout) = result {
            self.layouts[slot_index].layout = Some(layout);
            self.layouts[slot_index].state = BuildState::Done;
            Ok(Some(definition.layout_id))
        } else {
            self.layouts[slot_index].state = BuildState::Failed;
            Ok(None)
        }
    }

    fn build_named_type(
        &mut self,
        definition: Definition<'a>,
        type_node: &'a AstNode,
    ) -> Result<Option<FixedTypeLayout>, AnalysisInputError> {
        let parts = match type_node.kind {
            AstNodeKind::StringType => self.build_string(definition.source_index, type_node)?,
            AstNodeKind::ArrayType => self.build_array(definition.source_index, type_node)?,
            AstNodeKind::StructureType => {
                self.build_structure(definition.source_index, type_node)?
            }
            AstNodeKind::EnumerationType => {
                self.build_enumeration(definition.source_index, type_node)?
            }
            AstNodeKind::ElementaryType | AstNodeKind::NamedType => {
                let Some(target) = self.resolve_type(definition.source_index, type_node)? else {
                    return Ok(None);
                };
                let target_layout = self.layout(target)?;
                Some((
                    target_layout.size_bytes,
                    target_layout.alignment_bytes,
                    FixedTypeKind::Alias { target },
                ))
            }
            _ => return Err(self.invalid_shape(definition.source_index, type_node.span)),
        };
        let Some((size_bytes, alignment_bytes, kind)) = parts else {
            return Ok(None);
        };
        if !matches!(kind, FixedTypeKind::Alias { .. }) {
            self.check_type_size(definition.source_index, definition.name.span, size_bytes);
        }
        let default_initializer = if let Some(expression) = definition.node.children.get(2) {
            self.explicit_initializer(definition.source_index, expression)
        } else {
            self.implicit_initializer(&kind)?
        };
        Ok(Some(FixedTypeLayout {
            id: definition.layout_id,
            declaration: Some(definition.symbol),
            source_path: self.sources[definition.source_index]
                .ast
                .source_path
                .clone(),
            span: definition.node.span,
            size_bytes,
            alignment_bytes,
            default_initializer,
            kind,
        }))
    }

    fn build_function_block(
        &mut self,
        definition: Definition<'a>,
    ) -> Result<Option<FixedTypeLayout>, AnalysisInputError> {
        let Some(fields) =
            self.build_pou_fields(definition.source_index, definition.node, false)?
        else {
            return Ok(None);
        };
        let Some(temporary_fields) =
            self.build_pou_fields(definition.source_index, definition.node, true)?
        else {
            return Ok(None);
        };
        self.check_type_size(
            definition.source_index,
            definition.name.span,
            fields.size_bytes,
        );
        self.check_frame_size(
            definition.source_index,
            definition.name.span,
            temporary_fields.size_bytes,
        );
        Ok(Some(FixedTypeLayout {
            id: definition.layout_id,
            declaration: Some(definition.symbol),
            source_path: self.sources[definition.source_index]
                .ast
                .source_path
                .clone(),
            span: definition.node.span,
            size_bytes: fields.size_bytes,
            alignment_bytes: fields.alignment_bytes,
            default_initializer: FixedInitializer::Aggregate,
            kind: FixedTypeKind::FunctionBlock {
                fields: fields.fields,
                temporary_fields: temporary_fields.fields,
                temporary_size_bytes: temporary_fields.size_bytes,
                temporary_alignment_bytes: temporary_fields.alignment_bytes,
            },
        }))
    }

    fn resolve_type(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
    ) -> Result<Option<FixedTypeId>, AnalysisInputError> {
        match node.kind {
            AstNodeKind::ElementaryType => {
                let name = self.text(source_index, node)?.to_ascii_uppercase();
                self.builtin(source_index, node.span, &name).map(Some)
            }
            AstNodeKind::NamedType => {
                let canonical = self.text(source_index, node)?.to_ascii_lowercase();
                let Some(index) = self.definitions_by_name.get(&canonical).copied() else {
                    return Ok(None);
                };
                self.ensure_definition(index, source_index, node.span)
            }
            AstNodeKind::StringType
            | AstNodeKind::ArrayType
            | AstNodeKind::StructureType
            | AstNodeKind::EnumerationType => self.anonymous_type(source_index, node),
            _ => Err(self.invalid_shape(source_index, node.span)),
        }
    }

    fn builtin(
        &mut self,
        source_index: usize,
        span: SourceSpan,
        name: &str,
    ) -> Result<FixedTypeId, AnalysisInputError> {
        if let Some(id) = self.builtin_layouts.get(name).copied() {
            return Ok(id);
        }
        let (size, alignment) = match name {
            "BOOL" | "SINT" | "USINT" => (1, 1),
            "INT" | "UINT" => (2, 2),
            "DINT" | "UDINT" | "REAL" => (4, 4),
            "LINT" | "ULINT" | "LREAL" => (8, 8),
            _ => return Err(self.invalid_shape(source_index, span)),
        };
        let id = self.allocate_slot(source_index, span)?;
        self.layouts[id.0 as usize] = LayoutSlot {
            state: BuildState::Done,
            layout: Some(FixedTypeLayout {
                id,
                declaration: None,
                source_path: self.sources[source_index].ast.source_path.clone(),
                span,
                size_bytes: size,
                alignment_bytes: alignment,
                default_initializer: FixedInitializer::Zero,
                kind: FixedTypeKind::Scalar {
                    name: name.to_owned(),
                },
            }),
            source_index,
            span,
        };
        self.check_type_size(source_index, span, size);
        self.builtin_layouts.insert(name.to_owned(), id);
        Ok(id)
    }

    fn anonymous_type(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
    ) -> Result<Option<FixedTypeId>, AnalysisInputError> {
        let key = (source_index, node.span.start, node.span.end);
        if let Some(id) = self.anonymous_layouts.get(&key).copied() {
            return Ok(Some(id));
        }
        let id = self.allocate_slot(source_index, node.span)?;
        self.anonymous_layouts.insert(key, id);
        self.layouts[id.0 as usize].state = BuildState::Visiting;
        let parts = match node.kind {
            AstNodeKind::StringType => self.build_string(source_index, node)?,
            AstNodeKind::ArrayType => self.build_array(source_index, node)?,
            AstNodeKind::StructureType => self.build_structure(source_index, node)?,
            AstNodeKind::EnumerationType => self.build_enumeration(source_index, node)?,
            _ => return Err(self.invalid_shape(source_index, node.span)),
        };
        let Some((size, alignment, kind)) = parts else {
            self.layouts[id.0 as usize].state = BuildState::Failed;
            return Ok(None);
        };
        self.check_type_size(source_index, node.span, size);
        self.layouts[id.0 as usize] = LayoutSlot {
            state: BuildState::Done,
            layout: Some(FixedTypeLayout {
                id,
                declaration: None,
                source_path: self.sources[source_index].ast.source_path.clone(),
                span: node.span,
                size_bytes: size,
                alignment_bytes: alignment,
                default_initializer: self.implicit_initializer(&kind)?,
                kind,
            }),
            source_index,
            span: node.span,
        };
        Ok(Some(id))
    }

    fn build_string(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
    ) -> Result<Option<(u64, u8, FixedTypeKind)>, AnalysisInputError> {
        let wide = self
            .text(source_index, node)?
            .eq_ignore_ascii_case("WSTRING");
        let capacity_node = self.child(source_index, node, 0)?;
        let capacity = self.text(source_index, capacity_node)?.parse::<u64>().ok();
        let Some(capacity) = capacity else {
            self.emit(
                source_index,
                DiagnosticCode::InvalidTypeCapacity,
                capacity_node.span,
            );
            return Ok(None);
        };
        let limit = if wide {
            self.limits.max_wstring_code_units()
        } else {
            self.limits.max_string_payload_bytes()
        };
        if capacity == 0 || capacity > limit || capacity > u64::from(u32::MAX) {
            self.emit(
                source_index,
                DiagnosticCode::InvalidTypeCapacity,
                capacity_node.span,
            );
            return Ok(None);
        }
        let payload = if wide {
            capacity.checked_mul(2)
        } else {
            Some(capacity)
        };
        let Some(size) = payload
            .and_then(|value| value.checked_add(u64::from(LENGTH_PREFIX_BYTES)))
            .and_then(|value| align_up(value, 4))
        else {
            self.emit(source_index, DiagnosticCode::InvalidTypeCapacity, node.span);
            return Ok(None);
        };
        Ok(Some((
            size,
            4,
            FixedTypeKind::String {
                wide,
                capacity,
                length_prefix_bytes: LENGTH_PREFIX_BYTES,
            },
        )))
    }

    fn build_array(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
    ) -> Result<Option<(u64, u8, FixedTypeKind)>, AnalysisInputError> {
        let lower_node = self.child(source_index, node, 0)?;
        let upper_node = self.child(source_index, node, 1)?;
        let element_node = self.child(source_index, node, 2)?;
        let lower = self.constant_integer(source_index, lower_node);
        let upper = self.constant_integer(source_index, upper_node);
        let (lower, upper) = match (lower, upper) {
            (Ok(lower), Ok(upper)) => (lower, upper),
            (Err(error), _) => {
                self.emit(
                    source_index,
                    constant_diagnostic(error, DiagnosticCode::InvalidTypeCapacity),
                    lower_node.span,
                );
                return Ok(None);
            }
            (_, Err(error)) => {
                self.emit(
                    source_index,
                    constant_diagnostic(error, DiagnosticCode::InvalidTypeCapacity),
                    upper_node.span,
                );
                return Ok(None);
            }
        };
        let bound_kind = match (lower.kind, upper.kind) {
            (IntegerKind::Untyped, IntegerKind::Untyped) => Some(IntegerKind::Signed(32)),
            (IntegerKind::Untyped, kind) | (kind, IntegerKind::Untyped) => Some(kind),
            (left, right) if left == right => Some(left),
            _ => None,
        };
        let Some(bound_kind) = bound_kind else {
            self.emit(source_index, DiagnosticCode::InvalidTypeCapacity, node.span);
            return Ok(None);
        };
        if !integer_fits(lower.value, bound_kind)
            || !integer_fits(upper.value, bound_kind)
            || lower.value > upper.value
        {
            self.emit(source_index, DiagnosticCode::InvalidTypeCapacity, node.span);
            return Ok(None);
        }
        let Some(count_i128) = upper
            .value
            .checked_sub(lower.value)
            .and_then(|value| value.checked_add(1))
        else {
            self.emit(source_index, DiagnosticCode::InvalidTypeCapacity, node.span);
            return Ok(None);
        };
        let Ok(element_count) = u64::try_from(count_i128) else {
            self.emit(source_index, DiagnosticCode::InvalidTypeCapacity, node.span);
            return Ok(None);
        };
        if element_count == 0 || element_count > self.limits.max_array_elements() {
            self.emit(source_index, DiagnosticCode::InvalidTypeCapacity, node.span);
            return Ok(None);
        }
        let Some(element_type) = self.resolve_type(source_index, element_node)? else {
            return Ok(None);
        };
        let element = self.layout(element_type)?.clone();
        let Some(stride) = align_up(element.size_bytes, u64::from(element.alignment_bytes)) else {
            self.emit(source_index, DiagnosticCode::InvalidTypeCapacity, node.span);
            return Ok(None);
        };
        let Some(size) = stride.checked_mul(element_count) else {
            self.emit(source_index, DiagnosticCode::InvalidTypeCapacity, node.span);
            return Ok(None);
        };
        Ok(Some((
            size,
            element.alignment_bytes,
            FixedTypeKind::Array {
                lower: lower.value,
                upper: upper.value,
                element_count,
                element_type,
                element_stride_bytes: stride,
            },
        )))
    }

    fn build_structure(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
    ) -> Result<Option<(u64, u8, FixedTypeKind)>, AnalysisInputError> {
        let mut fields = Vec::new();
        let mut offset = 0_u64;
        let mut alignment = 1_u8;
        for field in &node.children {
            let name = self.child(source_index, field, 0)?;
            let type_node = self.child(source_index, field, 1)?;
            let Some(value_type) = self.resolve_type(source_index, type_node)? else {
                return Ok(None);
            };
            let value_layout = self.layout(value_type)?.clone();
            let Some(field_offset) = align_up(offset, u64::from(value_layout.alignment_bytes))
            else {
                self.emit(
                    source_index,
                    DiagnosticCode::InvalidTypeCapacity,
                    field.span,
                );
                return Ok(None);
            };
            let Some(next) = field_offset.checked_add(value_layout.size_bytes) else {
                self.emit(
                    source_index,
                    DiagnosticCode::InvalidTypeCapacity,
                    field.span,
                );
                return Ok(None);
            };
            fields.push(FixedFieldLayout {
                name: self.text(source_index, name)?.to_owned(),
                span: name.span,
                storage: FixedFieldStorage::Structure,
                value_type,
                offset_bytes: field_offset,
                initializer: self.initializer(source_index, value_type, field.children.get(2))?,
            });
            offset = next;
            alignment = alignment.max(value_layout.alignment_bytes);
        }
        let Some(size) = align_up(offset, u64::from(alignment)) else {
            self.emit(source_index, DiagnosticCode::InvalidTypeCapacity, node.span);
            return Ok(None);
        };
        Ok(Some((size, alignment, FixedTypeKind::Structure { fields })))
    }

    fn build_enumeration(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
    ) -> Result<Option<(u64, u8, FixedTypeKind)>, AnalysisInputError> {
        let mut members = Vec::new();
        let mut used = BTreeSet::new();
        let mut previous: Option<i32> = None;
        let mut valid = true;
        for item in &node.children {
            let name = self.child(source_index, item, 0)?;
            let value = if let Some(expression) = item.children.get(1) {
                match self.constant_integer(source_index, expression) {
                    Ok(value) => i32::try_from(value.value).ok(),
                    Err(
                        error @ (ConstantError::Overflow
                        | ConstantError::DivisionByZero
                        | ConstantError::InvalidCall
                        | ConstantError::InvalidConversion),
                    ) => {
                        self.emit(
                            source_index,
                            constant_diagnostic(error, DiagnosticCode::InvalidInitializer),
                            expression.span,
                        );
                        valid = false;
                        continue;
                    }
                    Err(ConstantError::Dynamic | ConstantError::Invalid) => None,
                }
            } else {
                previous.map_or(Some(0), |value| value.checked_add(1))
            };
            let Some(value) = value else {
                self.emit(source_index, DiagnosticCode::InvalidInitializer, item.span);
                valid = false;
                continue;
            };
            if !used.insert(value) {
                self.emit(source_index, DiagnosticCode::InvalidInitializer, item.span);
                valid = false;
                continue;
            }
            members.push(FixedEnumerationMember {
                name: self.text(source_index, name)?.to_owned(),
                span: name.span,
                value,
            });
            previous = Some(value);
        }
        if !valid || members.is_empty() {
            return Ok(None);
        }
        Ok(Some((4, 4, FixedTypeKind::Enumeration { members })))
    }

    fn build_pou_layouts(&mut self) -> Result<(), AnalysisInputError> {
        for source_index in 0..self.sources.len() {
            let ast = self.sources[source_index].ast;
            let top_nodes = ast.root.children.iter().collect::<Vec<_>>();
            for node in top_nodes {
                if !matches!(
                    node.kind,
                    AstNodeKind::FunctionDeclaration
                        | AstNodeKind::FunctionBlockDeclaration
                        | AstNodeKind::ProgramDeclaration
                ) {
                    continue;
                }
                self.build_pou_layout(source_index, node)?;
            }
        }
        Ok(())
    }

    fn validate_non_layout_types(&mut self) -> Result<(), AnalysisInputError> {
        for source_index in 0..self.sources.len() {
            let ast = self.sources[source_index].ast;
            let top_nodes = ast.root.children.iter().collect::<Vec<_>>();
            for node in top_nodes {
                match node.kind {
                    AstNodeKind::GlobalVariableBlock => {
                        for declaration in &node.children {
                            let name = self.child(source_index, declaration, 0)?;
                            let type_node = self.child(source_index, declaration, 2)?;
                            let Some(value_type) = self.resolve_type(source_index, type_node)?
                            else {
                                continue;
                            };
                            let global = self
                                .symbol_at(
                                    source_index,
                                    name.span,
                                    SemanticSymbolKind::GlobalVariable,
                                )
                                .ok_or_else(|| self.invalid_shape(source_index, name.span))?;
                            let initializer = self.initializer(
                                source_index,
                                value_type,
                                declaration.children.get(3),
                            )?;
                            self.globals.push(FixedGlobalLayout {
                                global,
                                source_path: self.sources[source_index].ast.source_path.clone(),
                                span: name.span,
                                value_type,
                                initializer,
                            });
                        }
                    }
                    AstNodeKind::FunctionDeclaration => {
                        let return_type = self.child(source_index, node, 1)?;
                        self.resolve_type(source_index, return_type)?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn build_pou_layout(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
    ) -> Result<(), AnalysisInputError> {
        let name = self.child(source_index, node, 0)?;
        let symbol_kind = match node.kind {
            AstNodeKind::FunctionDeclaration => SemanticSymbolKind::Function,
            AstNodeKind::FunctionBlockDeclaration => SemanticSymbolKind::FunctionBlock,
            AstNodeKind::ProgramDeclaration => SemanticSymbolKind::Program,
            _ => return Err(self.invalid_shape(source_index, node.span)),
        };
        let symbol = self
            .symbol_at(source_index, name.span, symbol_kind)
            .ok_or_else(|| self.invalid_shape(source_index, name.span))?;
        match node.kind {
            AstNodeKind::FunctionBlockDeclaration => {
                self.build_function_block_frame(source_index, node, name, symbol)
            }
            AstNodeKind::ProgramDeclaration => {
                self.build_program_layout(source_index, node, name, symbol)
            }
            AstNodeKind::FunctionDeclaration => {
                self.build_function_frame(source_index, node, name, symbol)
            }
            _ => Err(self.invalid_shape(source_index, node.span)),
        }
    }

    fn build_function_block_frame(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        name: &AstNode,
        symbol: SymbolId,
    ) -> Result<(), AnalysisInputError> {
        let Some(definition_index) = self
            .definitions_by_name
            .get(&self.text(source_index, name)?.to_ascii_lowercase())
            .copied()
        else {
            return Err(self.invalid_shape(source_index, name.span));
        };
        let Some(type_id) = self.ensure_definition(definition_index, source_index, name.span)?
        else {
            return Ok(());
        };
        let layout = self.layout(type_id)?.clone();
        let FixedTypeKind::FunctionBlock {
            temporary_fields,
            temporary_size_bytes,
            temporary_alignment_bytes,
            ..
        } = layout.kind
        else {
            return Err(self.invalid_shape(source_index, node.span));
        };
        self.invocation_frames.push(InvocationFrameLayout {
            pou: symbol,
            source_path: self.sources[source_index].ast.source_path.clone(),
            span: name.span,
            fields: temporary_fields,
            size_bytes: temporary_size_bytes,
            alignment_bytes: temporary_alignment_bytes,
        });
        Ok(())
    }

    fn build_program_layout(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        name: &AstNode,
        symbol: SymbolId,
    ) -> Result<(), AnalysisInputError> {
        let Some(fields) = self.build_pou_fields(source_index, node, false)? else {
            return Ok(());
        };
        let Some(temporary) = self.build_pou_fields(source_index, node, true)? else {
            return Ok(());
        };
        if fields.size_bytes > self.limits.max_program_static_bytes() {
            self.emit(
                source_index,
                DiagnosticCode::ResourceBudgetExceeded,
                name.span,
            );
        }
        self.check_frame_size(source_index, name.span, temporary.size_bytes);
        self.programs.push(StaticProgramLayout {
            program: symbol,
            source_path: self.sources[source_index].ast.source_path.clone(),
            span: name.span,
            fields: fields.fields,
            size_bytes: fields.size_bytes,
            alignment_bytes: fields.alignment_bytes,
            temporary_fields: temporary.fields.clone(),
            temporary_size_bytes: temporary.size_bytes,
            temporary_alignment_bytes: temporary.alignment_bytes,
        });
        self.invocation_frames.push(InvocationFrameLayout {
            pou: symbol,
            source_path: self.sources[source_index].ast.source_path.clone(),
            span: name.span,
            fields: temporary.fields,
            size_bytes: temporary.size_bytes,
            alignment_bytes: temporary.alignment_bytes,
        });
        Ok(())
    }

    fn build_function_frame(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        name: &AstNode,
        symbol: SymbolId,
    ) -> Result<(), AnalysisInputError> {
        let Some(fields) = self.build_pou_fields(source_index, node, true)? else {
            return Ok(());
        };
        self.check_frame_size(source_index, name.span, fields.size_bytes);
        self.invocation_frames.push(InvocationFrameLayout {
            pou: symbol,
            source_path: self.sources[source_index].ast.source_path.clone(),
            span: name.span,
            fields: fields.fields,
            size_bytes: fields.size_bytes,
            alignment_bytes: fields.alignment_bytes,
        });
        Ok(())
    }

    fn build_pou_fields(
        &mut self,
        source_index: usize,
        node: &'a AstNode,
        temporary: bool,
    ) -> Result<Option<BuiltFields>, AnalysisInputError> {
        let mut fields = Vec::new();
        let mut offset = 0_u64;
        let mut alignment = 1_u8;
        let mut valid = true;
        for block in &node.children {
            let selected = if temporary {
                node.kind == AstNodeKind::FunctionDeclaration
                    && matches!(
                        block.kind,
                        AstNodeKind::InputVariableBlock
                            | AstNodeKind::OutputVariableBlock
                            | AstNodeKind::LocalVariableBlock
                            | AstNodeKind::TemporaryVariableBlock
                    )
                    || node.kind != AstNodeKind::FunctionDeclaration
                        && block.kind == AstNodeKind::TemporaryVariableBlock
            } else {
                matches!(
                    block.kind,
                    AstNodeKind::InputVariableBlock
                        | AstNodeKind::OutputVariableBlock
                        | AstNodeKind::LocalVariableBlock
                )
            };
            if !selected {
                continue;
            }
            let storage = match block.kind {
                AstNodeKind::InputVariableBlock => FixedFieldStorage::Input,
                AstNodeKind::OutputVariableBlock => FixedFieldStorage::Output,
                AstNodeKind::LocalVariableBlock if temporary => FixedFieldStorage::Temporary,
                AstNodeKind::LocalVariableBlock => FixedFieldStorage::State,
                AstNodeKind::TemporaryVariableBlock => FixedFieldStorage::Temporary,
                _ => return Err(self.invalid_shape(source_index, block.span)),
            };
            for declaration in &block.children {
                let names = self.child(source_index, declaration, 0)?;
                let type_node = self.child(source_index, declaration, 1)?;
                let Some(value_type) = self.resolve_type(source_index, type_node)? else {
                    valid = false;
                    continue;
                };
                let value_layout = self.layout(value_type)?.clone();
                for name in &names.children {
                    let Some(field_offset) =
                        align_up(offset, u64::from(value_layout.alignment_bytes))
                    else {
                        self.emit(
                            source_index,
                            DiagnosticCode::InvalidTypeCapacity,
                            declaration.span,
                        );
                        valid = false;
                        continue;
                    };
                    let Some(next) = field_offset.checked_add(value_layout.size_bytes) else {
                        self.emit(
                            source_index,
                            DiagnosticCode::InvalidTypeCapacity,
                            declaration.span,
                        );
                        valid = false;
                        continue;
                    };
                    fields.push(FixedFieldLayout {
                        name: self.text(source_index, name)?.to_owned(),
                        span: name.span,
                        storage,
                        value_type,
                        offset_bytes: field_offset,
                        initializer: self.initializer(
                            source_index,
                            value_type,
                            declaration.children.get(2),
                        )?,
                    });
                    offset = next;
                    alignment = alignment.max(value_layout.alignment_bytes);
                }
            }
        }
        let Some(size) = align_up(offset, u64::from(alignment)) else {
            self.emit(source_index, DiagnosticCode::InvalidTypeCapacity, node.span);
            return Ok(None);
        };
        if valid {
            Ok(Some(BuiltFields {
                fields,
                size_bytes: size,
                alignment_bytes: alignment,
            }))
        } else {
            Ok(None)
        }
    }

    fn initializer(
        &mut self,
        source_index: usize,
        value_type: FixedTypeId,
        explicit: Option<&AstNode>,
    ) -> Result<FixedInitializer, AnalysisInputError> {
        if let Some(expression) = explicit {
            return Ok(self.explicit_initializer(source_index, expression));
        }
        Ok(self.layout(value_type)?.default_initializer.clone())
    }

    fn explicit_initializer(
        &mut self,
        source_index: usize,
        expression: &AstNode,
    ) -> FixedInitializer {
        if !is_static_initializer(expression) {
            self.emit(
                source_index,
                DiagnosticCode::InvalidInitializer,
                expression.span,
            );
        }
        FixedInitializer::ExplicitExpression {
            source_path: self.sources[source_index].ast.source_path.clone(),
            span: expression.span,
        }
    }

    fn implicit_initializer(
        &self,
        kind: &FixedTypeKind,
    ) -> Result<FixedInitializer, AnalysisInputError> {
        Ok(match kind {
            FixedTypeKind::Scalar { .. } => FixedInitializer::Zero,
            FixedTypeKind::String { .. } => FixedInitializer::EmptyString,
            FixedTypeKind::Enumeration { members } => {
                let Some(first) = members.first() else {
                    return Err(AnalysisInputError::TooManySymbols);
                };
                FixedInitializer::FirstEnumerationMember {
                    name: first.name.clone(),
                    value: first.value,
                }
            }
            FixedTypeKind::Alias { target } => self.layout(*target)?.default_initializer.clone(),
            FixedTypeKind::Array { .. }
            | FixedTypeKind::Structure { .. }
            | FixedTypeKind::FunctionBlock { .. } => FixedInitializer::Aggregate,
        })
    }

    fn expand_program_instances(&mut self) -> Result<(), AnalysisInputError> {
        for program_index in 0..self.programs.len() {
            let program = self.programs[program_index].clone();
            let count = program.fields.iter().try_fold(0_u64, |total, field| {
                self.fb_count(field.value_type)
                    .and_then(|count| total.checked_add(count))
            });
            let Some(count) = count else {
                self.emit_for_path(
                    &program.source_path,
                    DiagnosticCode::ResourceBudgetExceeded,
                    program.span,
                );
                continue;
            };
            if count > self.limits.max_static_fb_instances_per_program()
                || count > u64::from(u32::MAX)
            {
                self.emit_for_path(
                    &program.source_path,
                    DiagnosticCode::ResourceBudgetExceeded,
                    program.span,
                );
                continue;
            }
            let first_instance = self.instances.len();
            for field in &program.fields {
                self.expand_type(
                    program.program,
                    &field.name,
                    field.value_type,
                    field.offset_bytes,
                );
            }
            for (index, instance) in self.instances[first_instance..].iter_mut().enumerate() {
                instance.instance_id =
                    u32::try_from(index).map_err(|_| AnalysisInputError::TooManySymbols)?;
            }
        }
        Ok(())
    }

    fn fb_count(&self, value_type: FixedTypeId) -> Option<u64> {
        match &self.layout(value_type).ok()?.kind {
            FixedTypeKind::Scalar { .. }
            | FixedTypeKind::String { .. }
            | FixedTypeKind::Enumeration { .. } => Some(0),
            FixedTypeKind::Alias { target } => self.fb_count(*target),
            FixedTypeKind::Array {
                element_count,
                element_type,
                ..
            } => self.fb_count(*element_type)?.checked_mul(*element_count),
            FixedTypeKind::Structure { fields } => fields.iter().try_fold(0_u64, |total, field| {
                self.fb_count(field.value_type)
                    .and_then(|count| total.checked_add(count))
            }),
            FixedTypeKind::FunctionBlock { fields, .. } => {
                fields.iter().try_fold(1_u64, |total, field| {
                    self.fb_count(field.value_type)
                        .and_then(|count| total.checked_add(count))
                })
            }
        }
    }

    fn expand_type(
        &mut self,
        program: SymbolId,
        path: &str,
        value_type: FixedTypeId,
        base_offset: u64,
    ) {
        let Ok(layout) = self.layout(value_type).cloned() else {
            return;
        };
        match layout.kind {
            FixedTypeKind::Alias { target } => {
                self.expand_type(program, path, target, base_offset);
            }
            FixedTypeKind::Array {
                lower,
                element_count,
                element_type,
                element_stride_bytes,
                ..
            } => {
                for index in 0..element_count {
                    let Some(offset) = index
                        .checked_mul(element_stride_bytes)
                        .and_then(|value| base_offset.checked_add(value))
                    else {
                        return;
                    };
                    let Some(element_index) = lower.checked_add(i128::from(index)) else {
                        return;
                    };
                    self.expand_type(
                        program,
                        &format!("{path}[{element_index}]"),
                        element_type,
                        offset,
                    );
                }
            }
            FixedTypeKind::Structure { fields } => {
                for field in fields {
                    let Some(offset) = base_offset.checked_add(field.offset_bytes) else {
                        return;
                    };
                    self.expand_type(
                        program,
                        &format!("{path}.{}", field.name),
                        field.value_type,
                        offset,
                    );
                }
            }
            FixedTypeKind::FunctionBlock { fields, .. } => {
                let Some(function_block) = layout.declaration else {
                    return;
                };
                self.instances.push(StaticFunctionBlockInstance {
                    instance_id: 0,
                    program,
                    path: path.to_owned(),
                    function_block,
                    offset_bytes: base_offset,
                    size_bytes: layout.size_bytes,
                });
                for field in fields {
                    let Some(offset) = base_offset.checked_add(field.offset_bytes) else {
                        return;
                    };
                    self.expand_type(
                        program,
                        &format!("{path}.{}", field.name),
                        field.value_type,
                        offset,
                    );
                }
            }
            FixedTypeKind::Scalar { .. }
            | FixedTypeKind::String { .. }
            | FixedTypeKind::Enumeration { .. } => {}
        }
    }

    fn constant_integer(
        &self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<ConstantInteger, ConstantError> {
        match node.kind {
            AstNodeKind::Literal => {
                let text = self
                    .text(source_index, node)
                    .map_err(|_| ConstantError::Invalid)?;
                parse_integer(text)
                    .map(|value| ConstantInteger {
                        value,
                        kind: IntegerKind::Untyped,
                    })
                    .ok_or(ConstantError::Invalid)
            }
            AstNodeKind::QualifiedLiteral => {
                let qualifier = self
                    .child(source_index, node, 0)
                    .map_err(|_| ConstantError::Invalid)?;
                let value_node = self
                    .child(source_index, node, 1)
                    .map_err(|_| ConstantError::Invalid)?;
                let qualifier = self
                    .text(source_index, qualifier)
                    .map_err(|_| ConstantError::Invalid)?;
                let kind = integer_kind(qualifier).ok_or(ConstantError::Invalid)?;
                let mut value = self.constant_integer(source_index, value_node)?;
                if !integer_fits(value.value, kind) {
                    return Err(ConstantError::Invalid);
                }
                value.kind = kind;
                Ok(value)
            }
            AstNodeKind::ParenthesizedExpression => self.constant_integer(
                source_index,
                self.child(source_index, node, 0)
                    .map_err(|_| ConstantError::Invalid)?,
            ),
            AstNodeKind::UnaryExpression => self.constant_integer_unary(source_index, node),
            AstNodeKind::BinaryExpression => self.constant_integer_binary(source_index, node),
            AstNodeKind::CallExpression => self.constant_integer_call(source_index, node),
            AstNodeKind::Assignable => Err(ConstantError::Dynamic),
            _ => Err(ConstantError::Invalid),
        }
    }

    fn constant_integer_unary(
        &self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<ConstantInteger, ConstantError> {
        let operand = self.constant_integer(
            source_index,
            self.child(source_index, node, 0)
                .map_err(|_| ConstantError::Invalid)?,
        )?;
        let operator = self
            .text(source_index, node)
            .map_err(|_| ConstantError::Invalid)?
            .to_ascii_uppercase();
        let value = match operator.as_str() {
            "+" => Some(operand.value),
            "-" => operand.value.checked_neg(),
            "NOT" => constant_bitwise(
                IntegerOperation::BitwiseNot,
                operand.kind,
                operand.value,
                None,
            )
            .ok(),
            _ => None,
        }
        .ok_or(if operator == "-" {
            ConstantError::Overflow
        } else {
            ConstantError::Invalid
        })?;
        if !integer_fits(value, operand.kind) {
            return Err(ConstantError::Overflow);
        }
        Ok(ConstantInteger {
            value,
            kind: operand.kind,
        })
    }

    fn constant_integer_binary(
        &self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<ConstantInteger, ConstantError> {
        let left = self.constant_integer(
            source_index,
            self.child(source_index, node, 0)
                .map_err(|_| ConstantError::Invalid)?,
        )?;
        let right = self.constant_integer(
            source_index,
            self.child(source_index, node, 1)
                .map_err(|_| ConstantError::Invalid)?,
        )?;
        let kind = common_integer_kind(left.kind, right.kind).ok_or(ConstantError::Invalid)?;
        let operator = self
            .text(source_index, node)
            .map_err(|_| ConstantError::Invalid)?
            .to_ascii_uppercase();
        if matches!(operator.as_str(), "/" | "MOD") && right.value == 0 {
            return Err(ConstantError::DivisionByZero);
        }
        let value = match operator.as_str() {
            "+" => left.value.checked_add(right.value),
            "-" => left.value.checked_sub(right.value),
            "*" => left.value.checked_mul(right.value),
            "/" => left.value.checked_div(right.value),
            "MOD" => left.value.checked_rem(right.value),
            "AND" => constant_bitwise(
                IntegerOperation::BitwiseAnd,
                kind,
                left.value,
                Some(right.value),
            )
            .ok(),
            "OR" => constant_bitwise(
                IntegerOperation::BitwiseOr,
                kind,
                left.value,
                Some(right.value),
            )
            .ok(),
            "XOR" => constant_bitwise(
                IntegerOperation::BitwiseXor,
                kind,
                left.value,
                Some(right.value),
            )
            .ok(),
            _ => return Err(ConstantError::Invalid),
        }
        .ok_or(ConstantError::Overflow)?;
        if !integer_fits(value, kind) {
            return Err(ConstantError::Overflow);
        }
        Ok(ConstantInteger { value, kind })
    }

    fn constant_integer_call(
        &self,
        source_index: usize,
        node: &AstNode,
    ) -> Result<ConstantInteger, ConstantError> {
        let name = self
            .child(source_index, node, 0)
            .map_err(|_| ConstantError::Invalid)?;
        if name.children.len() != 1 {
            return Err(ConstantError::InvalidCall);
        }
        let identifier = name.children.first().ok_or(ConstantError::Invalid)?;
        let name = self
            .text(source_index, identifier)
            .map_err(|_| ConstantError::Invalid)?
            .to_ascii_uppercase();
        let mut arguments = Vec::with_capacity(node.children.len().saturating_sub(1));
        for argument in node.children.iter().skip(1) {
            arguments.push(self.constant_integer(source_index, argument)?);
        }
        if let Some(target) = name.strip_prefix("TO_").and_then(integer_kind) {
            let [argument] = arguments.as_slice() else {
                return Err(ConstantError::InvalidCall);
            };
            if !integer_fits(argument.value, target) {
                return Err(ConstantError::InvalidConversion);
            }
            return Ok(ConstantInteger {
                value: argument.value,
                kind: target,
            });
        }
        if matches!(name.as_str(), "MIN" | "MAX" | "LIMIT") {
            return constant_min_max_limit(&name, &arguments);
        }
        let (operation, mode, arity) =
            constant_integer_standard_operation(&name).ok_or(ConstantError::InvalidCall)?;
        if arguments.len() != arity {
            return Err(ConstantError::InvalidCall);
        }
        let kind = arguments
            .iter()
            .try_fold(IntegerKind::Untyped, |current, argument| {
                common_integer_kind(current, argument.kind).ok_or(ConstantError::InvalidCall)
            })?;
        let kind = if kind == IntegerKind::Untyped {
            IntegerKind::Signed(32)
        } else {
            kind
        };
        let value_type = fixed_integer_type(kind).ok_or(ConstantError::InvalidCall)?;
        let left = arguments.first().ok_or(ConstantError::InvalidCall)?.value;
        let right = arguments.get(1).map(|argument| argument.value);
        match evaluate_integer_operation(value_type, operation, mode, left, right) {
            Ok(value) => Ok(ConstantInteger { value, kind }),
            Err(IntegerArithmeticError::RuntimeFault(_)) => Err(ConstantError::Overflow),
            Err(_) => Err(ConstantError::InvalidCall),
        }
    }

    fn layout(&self, id: FixedTypeId) -> Result<&FixedTypeLayout, AnalysisInputError> {
        let Some(slot) = self.layouts.get(id.0 as usize) else {
            return Err(AnalysisInputError::TooManySymbols);
        };
        slot.layout
            .as_ref()
            .ok_or_else(|| AnalysisInputError::InvalidAstShape {
                source_path: self.sources[slot.source_index].ast.source_path.clone(),
                span_start: slot.span.start,
                span_end: slot.span.end,
            })
    }

    fn child<'n>(
        &self,
        source_index: usize,
        node: &'n AstNode,
        index: usize,
    ) -> Result<&'n AstNode, AnalysisInputError> {
        node.children
            .get(index)
            .ok_or_else(|| self.invalid_shape(source_index, node.span))
    }

    fn text<'n>(
        &self,
        source_index: usize,
        node: &'n AstNode,
    ) -> Result<&'n str, AnalysisInputError> {
        node.text
            .as_deref()
            .ok_or_else(|| self.invalid_shape(source_index, node.span))
    }

    fn invalid_shape(&self, source_index: usize, span: SourceSpan) -> AnalysisInputError {
        AnalysisInputError::InvalidAstShape {
            source_path: self.sources[source_index].ast.source_path.clone(),
            span_start: span.start,
            span_end: span.end,
        }
    }

    fn symbol_at(
        &self,
        source_index: usize,
        span: SourceSpan,
        kind: SemanticSymbolKind,
    ) -> Option<SymbolId> {
        let path = &self.sources[source_index].ast.source_path;
        self.semantics
            .symbols
            .iter()
            .find(|symbol| {
                symbol.source_path == *path && symbol.span == span && symbol.kind == kind
            })
            .map(|symbol| symbol.id)
    }

    fn check_type_size(&mut self, source_index: usize, span: SourceSpan, size: u64) {
        if size > self.limits.max_type_size_bytes() {
            self.emit(source_index, DiagnosticCode::InvalidTypeCapacity, span);
        }
    }

    fn check_frame_size(&mut self, source_index: usize, span: SourceSpan, size: u64) {
        if size > self.limits.max_invocation_frame_bytes() {
            self.emit(source_index, DiagnosticCode::ResourceBudgetExceeded, span);
        }
    }

    fn emit(&mut self, source_index: usize, code: DiagnosticCode, span: SourceSpan) {
        let key = (source_index, span.start, span.end, code.as_str());
        if self.diagnostic_keys.insert(key) {
            self.diagnostics.push(make_diagnostic(
                &self.sources[source_index].ast.source_path,
                self.sources[source_index].source,
                code,
                span,
            ));
        }
    }

    fn emit_for_path(&mut self, path: &str, code: DiagnosticCode, span: SourceSpan) {
        if let Some(source_index) = self
            .sources
            .iter()
            .position(|source| source.ast.source_path == path)
        {
            self.emit(source_index, code, span);
        }
    }
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    let remainder = value % alignment;
    if remainder == 0 {
        Some(value)
    } else {
        value.checked_add(alignment - remainder)
    }
}

const fn constant_diagnostic(error: ConstantError, invalid: DiagnosticCode) -> DiagnosticCode {
    match error {
        ConstantError::Dynamic => DiagnosticCode::DynamicCyclicStorage,
        ConstantError::Invalid => invalid,
        ConstantError::InvalidCall => DiagnosticCode::InvalidCall,
        ConstantError::InvalidConversion => DiagnosticCode::InvalidExplicitConversion,
        ConstantError::Overflow => DiagnosticCode::ConstantOverflow,
        ConstantError::DivisionByZero => DiagnosticCode::ConstantDivisionByZero,
    }
}

fn constant_min_max_limit(
    name: &str,
    arguments: &[ConstantInteger],
) -> Result<ConstantInteger, ConstantError> {
    let expected = if name == "LIMIT" { 3 } else { 2 };
    if arguments.len() != expected {
        return Err(ConstantError::InvalidCall);
    }
    let kind = arguments
        .iter()
        .try_fold(IntegerKind::Untyped, |current, argument| {
            common_integer_kind(current, argument.kind).ok_or(ConstantError::InvalidCall)
        })?;
    let kind = if kind == IntegerKind::Untyped {
        IntegerKind::Signed(32)
    } else {
        kind
    };
    if arguments
        .iter()
        .any(|argument| !integer_fits(argument.value, kind))
    {
        return Err(ConstantError::InvalidConversion);
    }
    let value = if name == "MIN" {
        arguments[0].value.min(arguments[1].value)
    } else if name == "MAX" {
        arguments[0].value.max(arguments[1].value)
    } else {
        let value = arguments[0].value;
        let low = arguments[1].value;
        let high = arguments[2].value;
        if low > high {
            return Err(ConstantError::InvalidConversion);
        }
        value.clamp(low, high)
    };
    Ok(ConstantInteger { value, kind })
}

fn constant_integer_standard_operation(
    name: &str,
) -> Option<(IntegerOperation, Option<IntegerArithmeticMode>, usize)> {
    if name == "ABS" {
        return Some((IntegerOperation::Absolute, None, 1));
    }
    let (prefix, suffix) = name.rsplit_once('_')?;
    let operation = match suffix {
        "ADD" => IntegerOperation::Add,
        "SUB" => IntegerOperation::Subtract,
        "MUL" => IntegerOperation::Multiply,
        "NEG" => IntegerOperation::Negate,
        _ => return None,
    };
    let mode = match prefix {
        "CHECKED" => IntegerArithmeticMode::Checked,
        "SATURATING" => IntegerArithmeticMode::Saturating,
        "WRAPPING" => IntegerArithmeticMode::Wrapping,
        _ => return None,
    };
    let arity = if operation == IntegerOperation::Negate {
        1
    } else {
        2
    };
    Some((operation, Some(mode), arity))
}

const fn fixed_integer_type(kind: IntegerKind) -> Option<IntegerType> {
    Some(match kind {
        IntegerKind::Signed(8) => IntegerType::Sint,
        IntegerKind::Signed(16) => IntegerType::Int,
        IntegerKind::Signed(32) => IntegerType::Dint,
        IntegerKind::Signed(64) => IntegerType::Lint,
        IntegerKind::Unsigned(8) => IntegerType::Usint,
        IntegerKind::Unsigned(16) => IntegerType::Uint,
        IntegerKind::Unsigned(32) => IntegerType::Udint,
        IntegerKind::Unsigned(64) => IntegerType::Ulint,
        IntegerKind::Untyped | IntegerKind::Signed(_) | IntegerKind::Unsigned(_) => return None,
    })
}

fn constant_bitwise(
    operation: IntegerOperation,
    kind: IntegerKind,
    left: i128,
    right: Option<i128>,
) -> Result<i128, ConstantError> {
    let resolved = if kind == IntegerKind::Untyped {
        IntegerKind::Signed(32)
    } else {
        kind
    };
    if !integer_fits(left, resolved) || right.is_some_and(|value| !integer_fits(value, resolved)) {
        return Err(ConstantError::InvalidConversion);
    }
    let value_type = fixed_integer_type(resolved).ok_or(ConstantError::Invalid)?;
    evaluate_integer_operation(value_type, operation, None, left, right)
        .map_err(|_| ConstantError::Invalid)
}

fn parse_integer(text: &str) -> Option<i128> {
    let (radix, digits) = if let Some(value) = text.strip_prefix("16#") {
        (16, value)
    } else if let Some(value) = text.strip_prefix("2#") {
        (2, value)
    } else {
        (10, text)
    };
    i128::from_str_radix(digits, radix).ok()
}

fn integer_kind(name: &str) -> Option<IntegerKind> {
    Some(match name.to_ascii_uppercase().as_str() {
        "SINT" => IntegerKind::Signed(8),
        "INT" => IntegerKind::Signed(16),
        "DINT" => IntegerKind::Signed(32),
        "LINT" => IntegerKind::Signed(64),
        "USINT" => IntegerKind::Unsigned(8),
        "UINT" => IntegerKind::Unsigned(16),
        "UDINT" => IntegerKind::Unsigned(32),
        "ULINT" => IntegerKind::Unsigned(64),
        _ => return None,
    })
}

fn common_integer_kind(left: IntegerKind, right: IntegerKind) -> Option<IntegerKind> {
    match (left, right) {
        (IntegerKind::Untyped, value) | (value, IntegerKind::Untyped) => Some(value),
        (left, right) if left == right => Some(left),
        (IntegerKind::Signed(left), IntegerKind::Signed(right)) => {
            Some(IntegerKind::Signed(left.max(right)))
        }
        (IntegerKind::Unsigned(left), IntegerKind::Unsigned(right)) => {
            Some(IntegerKind::Unsigned(left.max(right)))
        }
        (IntegerKind::Signed(signed), IntegerKind::Unsigned(unsigned))
        | (IntegerKind::Unsigned(unsigned), IntegerKind::Signed(signed)) => [8_u8, 16, 32, 64]
            .into_iter()
            .find(|candidate| *candidate >= signed && *candidate > unsigned)
            .map(IntegerKind::Signed),
    }
}

fn is_initializer_standard_function(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper.strip_prefix("TO_").is_some_and(|target| {
        matches!(
            target,
            "SINT"
                | "INT"
                | "DINT"
                | "LINT"
                | "USINT"
                | "UINT"
                | "UDINT"
                | "ULINT"
                | "REAL"
                | "LREAL"
        )
    }) || matches!(
        upper.as_str(),
        "CHECKED_ADD"
            | "CHECKED_SUB"
            | "CHECKED_MUL"
            | "CHECKED_NEG"
            | "SATURATING_ADD"
            | "SATURATING_SUB"
            | "SATURATING_MUL"
            | "SATURATING_NEG"
            | "WRAPPING_ADD"
            | "WRAPPING_SUB"
            | "WRAPPING_MUL"
            | "WRAPPING_NEG"
            | "MIN"
            | "MAX"
            | "LIMIT"
            | "ABS"
            | "SQRT"
            | "CONCAT"
    )
}

fn is_static_initializer(node: &AstNode) -> bool {
    match node.kind {
        AstNodeKind::Literal => true,
        AstNodeKind::QualifiedLiteral => node.children.get(1).is_some_and(|value| {
            matches!(
                value.kind,
                AstNodeKind::Literal | AstNodeKind::Identifier | AstNodeKind::UnaryExpression
            )
        }),
        AstNodeKind::ParenthesizedExpression | AstNodeKind::UnaryExpression => {
            node.children.first().is_some_and(is_static_initializer)
        }
        AstNodeKind::BinaryExpression => {
            node.children.len() == 2 && node.children.iter().all(is_static_initializer)
        }
        AstNodeKind::CallExpression => {
            let Some(name) = node
                .children
                .first()
                .and_then(|qualified| qualified.children.first())
                .and_then(|identifier| identifier.text.as_deref())
            else {
                return false;
            };
            is_initializer_standard_function(name)
                && node.children.iter().skip(1).all(is_static_initializer)
        }
        _ => false,
    }
}

fn integer_fits(value: i128, kind: IntegerKind) -> bool {
    match kind {
        IntegerKind::Untyped => true,
        IntegerKind::Signed(bits) => {
            let limit = 1_i128 << (u32::from(bits) - 1);
            (-limit..limit).contains(&value)
        }
        IntegerKind::Unsigned(bits) => {
            value >= 0
                && u128::try_from(value).is_ok_and(|value| {
                    value
                        < if bits == 64 {
                            u128::from(u64::MAX) + 1
                        } else {
                            1_u128 << u32::from(bits)
                        }
                })
        }
    }
}
