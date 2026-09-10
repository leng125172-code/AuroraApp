//! R1-06 structured Canonical ST IR and source-map cardinality/publication boundaries.

use aurora_st_ir::{
    AddressBindingInputs, AddressBindingLimits, AddressSemanticModel, AstNode, AstNodeKind,
    CanonicalArtifactLimits, CanonicalIrInputError, CanonicalIrLimits,
    CanonicalIrSerializationError, CanonicalNode, CanonicalSourceMapLimits,
    CanonicalSourceMapSerializationError, CyclicWorkLimits, CyclicWorkModel, DiagnosticCode,
    ExternalField, FixedDataLimits, InitializationLimits, ParserLimits, ProgramTaskBinding,
    SemanticSource, SourceSpan, TagCatalogEntry, TaskHandle, VersionedAst, analyze_addresses,
    analyze_cyclic_work, canonical_ir_to_json, canonical_source_map_to_json, lower_canonical_ir,
    parse,
};

const TAG_ID: &str = "01890f3e-4c7b-7cc2-98c4-dc0c0c073901";
const EXTERNAL_SOURCE: &str = "x";

fn parser_limits() -> ParserLimits {
    ParserLimits::new(64 * 1024, 8 * 1024, 8 * 1024, 256)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn fixed_limits() -> FixedDataLimits {
    FixedDataLimits::new(1024, 1024, 1024, 64 * 1024, 1024, 64 * 1024, 64 * 1024)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn address_limits() -> AddressBindingLimits {
    AddressBindingLimits::new(128, 128, 128, 64, 64, 16, 64, 64)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn work_limits() -> CyclicWorkLimits {
    CyclicWorkLimits::new(16, 16, 64, 4096)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn ir_limits(max_nodes: usize, max_pous: usize, max_bytes: usize) -> CanonicalIrLimits {
    CanonicalIrLimits::new(max_nodes, max_pous, max_bytes)
        .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn initialization_limits() -> InitializationLimits {
    InitializationLimits::new(
        64 * 1024,
        1024 * 1024,
        1024 * 1024,
        1024 * 1024,
        4 * 1024 * 1024,
    )
    .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn source_map_limits(
    max_sources: usize,
    max_symbols: usize,
    max_nodes: usize,
    max_fault_sites: usize,
    max_bytes: usize,
) -> CanonicalSourceMapLimits {
    CanonicalSourceMapLimits::new(
        max_sources,
        max_symbols,
        max_nodes,
        max_fault_sites,
        max_bytes,
    )
    .unwrap_or_else(|error| unreachable!("test limits are valid: {error}"))
}

fn generous_source_map_limits() -> CanonicalSourceMapLimits {
    source_map_limits(16, 4096, 4096, 4096, 1024 * 1024)
}

fn source() -> &'static str {
    r"AURORA_ST VERSION 1.0;
VAR_GLOBAL
  Counter AT %MD0 : DINT := DINT#0;
END_VAR
FUNCTION Increment : DINT
VAR_INPUT
  Value : DINT;
END_VAR
RETURN CHECKED_ADD(CHECKED_ADD(Value, DINT#1), DINT#1);
END_FUNCTION
PROGRAM Main
VAR
  Index : DINT;
END_VAR
FOR Index := DINT#0 TO DINT#2 BY DINT#1 DO
  Counter := Increment(Counter);
END_FOR;
END_PROGRAM
"
}

fn accepted_models() -> (VersionedAst, AddressSemanticModel, CyclicWorkModel) {
    let parsed = parse("program/main.st", source().as_bytes(), parser_limits());
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let ast = parsed
        .ast
        .unwrap_or_else(|| unreachable!("diagnostic-free parse publishes an AST"));
    let (address_model, work_model) = accepted_models_for(&[SemanticSource::new(&ast, source())]);
    (ast, address_model, work_model)
}

fn accepted_models_for(sources: &[SemanticSource<'_>]) -> (AddressSemanticModel, CyclicWorkModel) {
    let catalog = [TagCatalogEntry {
        source_path: "project/tags.json",
        source: EXTERNAL_SOURCE,
        span: SourceSpan { start: 0, end: 1 },
        symbol: ExternalField {
            value: "counter",
            span: SourceSpan { start: 0, end: 1 },
        },
        tag_id: ExternalField {
            value: TAG_ID,
            span: SourceSpan { start: 0, end: 1 },
        },
    }];
    let semantic = aurora_st_ir::analyze_faults(sources, fixed_limits())
        .unwrap_or_else(|error| unreachable!("parser-produced source is valid: {error}"));
    let fault_model = semantic
        .model
        .unwrap_or_else(|| unreachable!("test source passes R1-04: {:?}", semantic.diagnostics));
    let program = fault_model
        .fixed
        .semantics
        .symbols
        .iter()
        .find(|symbol| symbol.name == "Main")
        .map_or_else(|| unreachable!("test declares Main"), |symbol| symbol.id);
    let tasks = [ProgramTaskBinding {
        program,
        task_handle: TaskHandle(7),
    }];
    let addresses = analyze_addresses(
        sources,
        fixed_limits(),
        address_limits(),
        AddressBindingInputs {
            tag_catalog: &catalog,
            device_bindings: &[],
            device_packages: &[],
            program_tasks: &tasks,
        },
    )
    .unwrap_or_else(|error| unreachable!("accepted inputs have valid shape: {error}"));
    let address_model = addresses
        .model
        .unwrap_or_else(|| unreachable!("test source passes R1-05: {:?}", addresses.diagnostics));
    let work = analyze_cyclic_work(sources, &address_model, fixed_limits(), work_limits())
        .unwrap_or_else(|error| unreachable!("accepted inputs have valid shape: {error}"));
    let work_model = work
        .model
        .unwrap_or_else(|| unreachable!("test source passes work proof: {:?}", work.diagnostics));
    (address_model, work_model)
}

fn lower(
    ast: &VersionedAst,
    address_model: &AddressSemanticModel,
    work_model: &CyclicWorkModel,
    limits: CanonicalIrLimits,
) -> aurora_st_ir::CanonicalIrOutput {
    lower_with_source_map(
        ast,
        address_model,
        work_model,
        generous_source_map_limits(),
        limits,
    )
}

fn lower_with_source_map(
    ast: &VersionedAst,
    address_model: &AddressSemanticModel,
    work_model: &CyclicWorkModel,
    map_limits: CanonicalSourceMapLimits,
    ir_limits: CanonicalIrLimits,
) -> aurora_st_ir::CanonicalIrOutput {
    lower_canonical_ir(
        &[SemanticSource::new(ast, source())],
        address_model,
        work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(ir_limits, map_limits),
    )
    .unwrap_or_else(|error| unreachable!("accepted models lower successfully: {error}"))
}

fn ir_node_count(node: &CanonicalNode) -> usize {
    1 + node.children.iter().map(ir_node_count).sum::<usize>()
}

fn ast_body_node_count(root: &AstNode) -> usize {
    root.children
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                AstNodeKind::FunctionDeclaration
                    | AstNodeKind::FunctionBlockDeclaration
                    | AstNodeKind::ProgramDeclaration
            )
        })
        .map(|pou| {
            let body = pou
                .children
                .iter()
                .find(|child| child.kind == AstNodeKind::StatementList)
                .unwrap_or_else(|| unreachable!("parser-produced POU has one body"));
            ast_node_count(body)
        })
        .sum()
}

fn ast_node_count(node: &AstNode) -> usize {
    1 + node.children.iter().map(ast_node_count).sum::<usize>()
}

fn nodes<'a>(node: &'a CanonicalNode, output: &mut Vec<&'a CanonicalNode>) {
    output.push(node);
    for child in &node.children {
        nodes(child, output);
    }
}

#[test]
fn every_executable_ast_node_has_one_dense_ir_node_without_loop_unrolling() {
    let (ast, address_model, work_model) = accepted_models();
    let output = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    );
    assert!(output.diagnostics.is_empty());
    let ir = output
        .ir
        .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    assert_eq!(ir.pous.len(), 2);
    let actual: usize = ir.pous.iter().map(|pou| ir_node_count(&pou.body)).sum();
    assert_eq!(actual, ast_body_node_count(&ast.root));
    let mut flat = Vec::new();
    for pou in &ir.pous {
        nodes(&pou.body, &mut flat);
    }
    assert_eq!(
        flat.iter().map(|node| node.id.0).collect::<Vec<_>>(),
        (0..u32::try_from(actual).unwrap_or(0)).collect::<Vec<_>>()
    );
    let loops = flat
        .iter()
        .filter(|node| node.kind == AstNodeKind::ForStatement)
        .collect::<Vec<_>>();
    assert_eq!(loops.len(), 1);
    assert_eq!(loops[0].loop_iterations, Some(3));
}

#[test]
fn source_map_has_exactly_one_entry_per_source_symbol_node_and_fault() {
    let (ast, address_model, work_model) = accepted_models();
    let output = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    );
    assert!(output.diagnostics.is_empty());
    let ir = output
        .ir
        .as_ref()
        .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    let source_map = output
        .source_map
        .as_ref()
        .unwrap_or_else(|| unreachable!("successful lowering publishes its source map"));

    assert_eq!(source_map.sources.len(), 1);
    assert_eq!(source_map.sources[0].id.0, 0);
    assert_eq!(source_map.sources[0].path, "program/main.st");
    assert_eq!(
        source_map.sources[0].byte_length,
        u32::try_from(source().len()).unwrap_or(0)
    );
    assert_eq!(source_map.symbols.len(), ir.symbols.len());
    for (entry, symbol) in source_map.symbols.iter().zip(&ir.symbols) {
        assert_eq!(entry.symbol, symbol.id);
        assert_eq!(entry.source.0, 0);
        assert_eq!(entry.span, symbol.span);
    }

    let mut flat = Vec::new();
    let mut owners = Vec::new();
    for pou in &ir.pous {
        let mut pou_nodes = Vec::new();
        nodes(&pou.body, &mut pou_nodes);
        owners.extend(pou_nodes.iter().map(|node| (node.id, pou.symbol)));
        flat.extend(pou_nodes);
    }
    assert_eq!(source_map.nodes.len(), flat.len());
    for ((entry, node), (node_id, pou)) in source_map.nodes.iter().zip(&flat).zip(&owners) {
        assert_eq!(entry.node, node.id);
        assert_eq!(entry.node, *node_id);
        assert_eq!(entry.pou, *pou);
        assert_eq!(entry.source.0, 0);
        assert!(entry.span.start <= entry.span.end);
        assert!(entry.span.end <= source_map.sources[0].byte_length);
    }

    assert_eq!(source_map.fault_sites.len(), ir.fault_sites.len());
    for entry in &source_map.fault_sites {
        let node = flat
            .iter()
            .find(|node| node.id == entry.node)
            .unwrap_or_else(|| unreachable!("every mapped node exists"));
        assert_eq!(node.fault_site, Some(entry.fault_site));
        assert_eq!(entry.source.0, 0);
    }
}

#[test]
fn fault_and_task_tables_are_neither_duplicated_nor_omitted() {
    let (ast, address_model, work_model) = accepted_models();
    let output = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    );
    let ir = output
        .ir
        .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    assert_eq!(ir.tasks.len(), address_model.program_tasks.len());
    assert_eq!(ir.tasks[0].task, TaskHandle(7));
    assert_eq!(ir.fault_sites.len(), address_model.faults.fault_sites.len());
    let mut flat = Vec::new();
    for pou in &ir.pous {
        nodes(&pou.body, &mut flat);
    }
    let references = flat
        .iter()
        .filter_map(|node| node.fault_site)
        .collect::<Vec<_>>();
    assert_eq!(references.len(), ir.fault_sites.len());
    assert_eq!(
        references
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        references.len()
    );
}

#[test]
fn node_capacity_accepts_exact_count_and_rejects_one_less_atomically() {
    let (ast, address_model, work_model) = accepted_models();
    let baseline = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    );
    let count = baseline.ir.as_ref().map_or(0, |ir| {
        ir.pous.iter().map(|pou| ir_node_count(&pou.body)).sum()
    });
    assert!(
        lower(
            &ast,
            &address_model,
            &work_model,
            ir_limits(count, 16, 1024 * 1024),
        )
        .diagnostics
        .is_empty()
    );
    let rejected = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(count.saturating_sub(1), 16, 1024 * 1024),
    );
    assert!(rejected.ir.is_none());
    assert!(rejected.source_map.is_none());
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(
        rejected.diagnostics[0].code,
        DiagnosticCode::ResourceBudgetExceeded
    );
    assert_eq!(rejected.diagnostics[0].source_path, "program/main.st");
}

#[test]
fn pou_capacity_accepts_exact_count_and_rejects_one_less_atomically() {
    let (ast, address_model, work_model) = accepted_models();
    assert!(
        lower(
            &ast,
            &address_model,
            &work_model,
            ir_limits(4096, 2, 1024 * 1024),
        )
        .diagnostics
        .is_empty()
    );
    let rejected = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 1, 1024 * 1024),
    );
    assert!(rejected.ir.is_none());
    assert!(rejected.source_map.is_none());
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(
        rejected.diagnostics[0].code,
        DiagnosticCode::ResourceBudgetExceeded
    );
}

#[test]
fn canonical_json_is_stable_and_has_an_exact_byte_boundary() {
    let (ast, address_model, work_model) = accepted_models();
    let ir = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, usize::MAX),
    )
    .ir
    .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    let unlimited = ir_limits(4096, 16, usize::MAX);
    let first = canonical_ir_to_json(&ir, unlimited)
        .unwrap_or_else(|error| unreachable!("valid IR serializes: {error}"));
    let second = canonical_ir_to_json(&ir, unlimited)
        .unwrap_or_else(|error| unreachable!("valid IR serializes: {error}"));
    assert_eq!(first, second);
    let exact = canonical_ir_to_json(&ir, ir_limits(4096, 16, first.len()))
        .unwrap_or_else(|error| unreachable!("exact byte boundary is accepted: {error}"));
    assert_eq!(exact, first);
    assert!(matches!(
        canonical_ir_to_json(&ir, ir_limits(4096, 16, first.len().saturating_sub(1))),
        Err(CanonicalIrSerializationError::EncodedSizeExceeded { .. })
    ));
}

#[test]
fn source_map_capacities_accept_exact_counts_and_reject_one_less_atomically() {
    let (ast, address_model, work_model) = accepted_models();
    let baseline = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    );
    let map = baseline
        .source_map
        .as_ref()
        .unwrap_or_else(|| unreachable!("baseline publishes a source map"));
    let counts = (
        map.sources.len(),
        map.symbols.len(),
        map.nodes.len(),
        map.fault_sites.len(),
    );
    assert!(counts.1 > 1 && counts.2 > 1 && counts.3 > 1);

    let exact = lower_with_source_map(
        &ast,
        &address_model,
        &work_model,
        source_map_limits(counts.0, counts.1, counts.2, counts.3, usize::MAX),
        ir_limits(4096, 16, 1024 * 1024),
    );
    assert!(exact.ir.is_some());
    assert!(exact.source_map.is_some());
    assert!(exact.diagnostics.is_empty());

    for limits in [
        source_map_limits(counts.0, counts.1 - 1, counts.2, counts.3, usize::MAX),
        source_map_limits(counts.0, counts.1, counts.2 - 1, counts.3, usize::MAX),
        source_map_limits(counts.0, counts.1, counts.2, counts.3 - 1, usize::MAX),
    ] {
        let rejected = lower_with_source_map(
            &ast,
            &address_model,
            &work_model,
            limits,
            ir_limits(4096, 16, 1024 * 1024),
        );
        assert!(rejected.ir.is_none());
        assert!(rejected.source_map.is_none());
        assert_eq!(rejected.diagnostics.len(), 1);
        assert_eq!(
            rejected.diagnostics[0].code,
            DiagnosticCode::ResourceBudgetExceeded
        );
    }
}

#[test]
fn source_file_capacity_counts_non_executable_inputs_without_omission() {
    const TYPES_SOURCE: &str = "AURORA_ST VERSION 1.0;\nTYPE Auxiliary : DINT; END_TYPE\n";
    let main_ast = parse("program/main.st", source().as_bytes(), parser_limits())
        .ast
        .unwrap_or_else(|| unreachable!("main source parses"));
    let types_ast = parse(
        "types/auxiliary.st",
        TYPES_SOURCE.as_bytes(),
        parser_limits(),
    )
    .ast
    .unwrap_or_else(|| unreachable!("type source parses"));
    let sources = [
        SemanticSource::new(&main_ast, source()),
        SemanticSource::new(&types_ast, TYPES_SOURCE),
    ];
    let (address_model, work_model) = accepted_models_for(&sources);

    let accepted = lower_canonical_ir(
        &sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            source_map_limits(2, 4096, 4096, 4096, 1024 * 1024),
        ),
    )
    .unwrap_or_else(|error| unreachable!("accepted models lower: {error}"));
    let map = accepted
        .source_map
        .unwrap_or_else(|| unreachable!("accepted build publishes a source map"));
    assert_eq!(
        map.sources
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        ["program/main.st", "types/auxiliary.st"]
    );

    let reversed_sources = [sources[1], sources[0]];
    let reordered = lower_canonical_ir(
        &reversed_sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            source_map_limits(2, 4096, 4096, 4096, 1024 * 1024),
        ),
    )
    .unwrap_or_else(|error| unreachable!("source order does not change lowering: {error}"));
    assert_eq!(reordered.ir, accepted.ir);
    assert_eq!(reordered.source_map.as_ref(), Some(&map));

    let rejected = lower_canonical_ir(
        &sources,
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            source_map_limits(1, 4096, 4096, 4096, 1024 * 1024),
        ),
    )
    .unwrap_or_else(|error| unreachable!("capacity crossing is a diagnostic: {error}"));
    assert!(rejected.ir.is_none());
    assert!(rejected.source_map.is_none());
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(
        rejected.diagnostics[0].code,
        DiagnosticCode::ResourceBudgetExceeded
    );
    assert_eq!(rejected.diagnostics[0].source_path, "types/auxiliary.st");
}

#[test]
fn canonical_source_map_json_is_stable_versioned_and_exactly_bounded() {
    let (ast, address_model, work_model) = accepted_models();
    let mut map = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, 1024 * 1024),
    )
    .source_map
    .unwrap_or_else(|| unreachable!("successful lowering publishes a source map"));
    let unlimited = source_map_limits(16, 4096, 4096, 4096, usize::MAX);
    let first = canonical_source_map_to_json(&map, unlimited)
        .unwrap_or_else(|error| unreachable!("valid source map serializes: {error}"));
    let second = canonical_source_map_to_json(&map, unlimited)
        .unwrap_or_else(|error| unreachable!("valid source map serializes: {error}"));
    assert_eq!(first, second);
    let exact =
        canonical_source_map_to_json(&map, source_map_limits(16, 4096, 4096, 4096, first.len()))
            .unwrap_or_else(|error| unreachable!("exact byte boundary is accepted: {error}"));
    assert_eq!(exact, first);
    assert!(matches!(
        canonical_source_map_to_json(
            &map,
            source_map_limits(16, 4096, 4096, 4096, first.len().saturating_sub(1)),
        ),
        Err(CanonicalSourceMapSerializationError::EncodedSizeExceeded { .. })
    ));

    map.schema_version.minor = map.schema_version.minor.saturating_add(1);
    assert!(matches!(
        canonical_source_map_to_json(&map, unlimited),
        Err(CanonicalSourceMapSerializationError::UnsupportedVersion { .. })
    ));
}

#[test]
fn serializer_rejects_an_unknown_writer_version_without_bytes() {
    let (ast, address_model, work_model) = accepted_models();
    let mut ir = lower(
        &ast,
        &address_model,
        &work_model,
        ir_limits(4096, 16, usize::MAX),
    )
    .ir
    .unwrap_or_else(|| unreachable!("successful lowering publishes complete IR"));
    ir.schema_version.minor = ir.schema_version.minor.saturating_add(1);
    assert!(matches!(
        canonical_ir_to_json(&ir, ir_limits(4096, 16, usize::MAX)),
        Err(CanonicalIrSerializationError::UnsupportedVersion { .. })
    ));
}

#[test]
fn mismatched_work_model_is_rejected_before_any_ir_is_published() {
    let (ast, address_model, mut work_model) = accepted_models();
    work_model.tasks.clear();
    let result = lower_canonical_ir(
        &[SemanticSource::new(&ast, source())],
        &address_model,
        &work_model,
        fixed_limits(),
        work_limits(),
        initialization_limits(),
        CanonicalArtifactLimits::new(
            ir_limits(4096, 16, 1024 * 1024),
            generous_source_map_limits(),
        ),
    );
    assert_eq!(result.err(), Some(CanonicalIrInputError::WorkModelMismatch));
}

#[test]
fn every_zero_limit_is_rejected() {
    assert!(CanonicalIrLimits::new(0, 1, 1).is_err());
    assert!(CanonicalIrLimits::new(1, 0, 1).is_err());
    assert!(CanonicalIrLimits::new(1, 1, 0).is_err());
    assert!(CanonicalSourceMapLimits::new(0, 1, 1, 1, 1).is_err());
    assert!(CanonicalSourceMapLimits::new(1, 0, 1, 1, 1).is_err());
    assert!(CanonicalSourceMapLimits::new(1, 1, 0, 1, 1).is_err());
    assert!(CanonicalSourceMapLimits::new(1, 1, 1, 0, 1).is_err());
    assert!(CanonicalSourceMapLimits::new(1, 1, 1, 1, 0).is_err());
}
