use std::collections::{BTreeMap, BTreeSet};
use std::str;

use crate::diagnostic::{make_diagnostic, sort_diagnostics};
use crate::model::{
    Backedge, Edge, JoinMode, JoinPolicy, LayoutEdge, LayoutGroup, LayoutNode, LayoutPoint, Node,
    NodeKind, StableId, WaitMode, WorkflowDocument, WorkflowLayoutDocument,
};
use crate::yaml::{MappingEntry, YamlNode, parse};
use crate::{SourceSpan, WorkflowDiagnostic, WorkflowDiagnosticCode, WorkflowValidationLimits};

/// One semantic Workflow source supplied by a project loader.
#[derive(Debug, Clone, Copy)]
pub struct WorkflowSource<'a> {
    /// Normalized project-relative path used for deterministic diagnostics.
    pub source_path: &'a str,
    /// Exact author bytes. The validator accepts only BOM-free UTF-8.
    pub source_bytes: &'a [u8],
}

/// One optional host-only Layout source supplied by a project loader.
#[derive(Debug, Clone, Copy)]
pub struct LayoutSource<'a> {
    /// Normalized project-relative path used for deterministic diagnostics.
    pub source_path: &'a str,
    /// Exact author bytes. The validator accepts only BOM-free UTF-8.
    pub source_bytes: &'a [u8],
}

/// Complete R2-01 project input. No filesystem or environment access occurs inside validation.
#[derive(Debug, Clone, Copy)]
pub struct WorkflowProjectInput<'a> {
    /// Semantic Workflow author documents.
    pub workflows: &'a [WorkflowSource<'a>],
    /// Independent host-only Layout documents.
    pub layouts: &'a [LayoutSource<'a>],
}

/// Atomic result of bounded Workflow Graph and independent Layout validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowValidationOutput {
    /// All semantic graphs, present only when every graph and project reference is valid.
    pub workflows: Option<Vec<WorkflowDocument>>,
    /// Valid Layout documents. Invalid Layouts never suppress valid semantic graphs.
    pub layouts: Vec<WorkflowLayoutDocument>,
    /// Deterministically ordered locale-neutral diagnostics.
    pub diagnostics: Vec<WorkflowDiagnostic>,
}

/// Parses and structurally validates an exact Preview 1.0 Workflow project.
///
/// The function is host-only and performs no I/O. YAML parsing, alias expansion, Graph collections,
/// and Layout collections are bounded by `limits`. Any semantic Graph error suppresses the entire
/// Graph model and all Layout publication. Layout-only errors omit only the affected Layout.
#[must_use]
pub fn validate_project(
    input: WorkflowProjectInput<'_>,
    limits: WorkflowValidationLimits,
) -> WorkflowValidationOutput {
    let mut diagnostics = Vec::new();
    if input.workflows.len() > limits.workflows() {
        let source = first_excess_workflow(input.workflows, limits.workflows());
        diagnostics.push(limit_diagnostic(
            source.map_or("", |value| value.source_path),
            source.and_then(utf8_or_empty),
            WorkflowDiagnosticCode::ResourceBudgetExceeded,
        ));
        return WorkflowValidationOutput {
            workflows: None,
            layouts: Vec::new(),
            diagnostics,
        };
    }
    let (decoded_graphs, mut graph_diagnostics) = validate_graph_sources(input.workflows, limits);
    diagnostics.append(&mut graph_diagnostics);
    if !diagnostics.is_empty() {
        sort_diagnostics(&mut diagnostics);
        return WorkflowValidationOutput {
            workflows: None,
            layouts: Vec::new(),
            diagnostics,
        };
    }

    let (mut layouts, mut layout_diagnostics) = if input.layouts.len() > limits.layouts() {
        let source = first_excess_layout(input.layouts, limits.layouts());
        (
            Vec::new(),
            vec![limit_diagnostic(
                source.map_or("", |value| value.source_path),
                source.and_then(layout_utf8_or_empty),
                WorkflowDiagnosticCode::LayoutLimitExceeded,
            )],
        )
    } else {
        validate_layout_sources(input.layouts, limits, &decoded_graphs)
    };
    diagnostics.append(&mut layout_diagnostics);
    validate_layout_identity_isolation(
        &decoded_graphs,
        &mut layouts,
        input.layouts,
        &mut diagnostics,
    );

    let mut workflows = decoded_graphs
        .into_iter()
        .map(|value| value.document)
        .collect::<Vec<_>>();
    workflows.sort_by_key(|document| document.workflow_id.network_bytes());
    layouts.sort_by_key(|document| document.document_id.network_bytes());
    sort_diagnostics(&mut diagnostics);
    WorkflowValidationOutput {
        workflows: Some(workflows),
        layouts,
        diagnostics,
    }
}

fn validate_graph_sources<'a>(
    sources: &'a [WorkflowSource<'a>],
    limits: WorkflowValidationLimits,
) -> (Vec<DecodedGraph<'a>>, Vec<WorkflowDiagnostic>) {
    let mut ordered = sources.to_vec();
    ordered.sort_by(|left, right| {
        left.source_path
            .as_bytes()
            .cmp(right.source_path.as_bytes())
    });
    let mut diagnostics = Vec::new();
    let mut graphs = Vec::with_capacity(ordered.len());
    for source in ordered {
        let parsed = parse(source.source_path, source.source_bytes, limits.graph_yaml());
        diagnostics.extend(parsed.diagnostics);
        let Some(root) = parsed.root else {
            continue;
        };
        let Ok(text) = str::from_utf8(source.source_bytes) else {
            continue;
        };
        let mut decoder = Decoder::new(source.source_path, text);
        let document = decoder.decode_graph(&root, limits);
        diagnostics.extend(decoder.diagnostics);
        if let Some(document) = document {
            graphs.push(DecodedGraph {
                document,
                source: text,
            });
        }
    }
    if diagnostics.is_empty() {
        validate_graph_collection(&graphs, &mut diagnostics);
    }
    sort_diagnostics(&mut diagnostics);
    (graphs, diagnostics)
}

fn validate_layout_sources(
    sources: &[LayoutSource<'_>],
    limits: WorkflowValidationLimits,
    graphs: &[DecodedGraph<'_>],
) -> (Vec<WorkflowLayoutDocument>, Vec<WorkflowDiagnostic>) {
    let mut ordered = sources.to_vec();
    ordered.sort_by(|left, right| {
        left.source_path
            .as_bytes()
            .cmp(right.source_path.as_bytes())
    });
    let mut diagnostics = Vec::new();
    let mut layouts = Vec::with_capacity(ordered.len());
    for source in ordered {
        let diagnostic_start = diagnostics.len();
        let parsed = parse(
            source.source_path,
            source.source_bytes,
            limits.layout_yaml(),
        );
        diagnostics.extend(parsed.diagnostics);
        let Some(root) = parsed.root else {
            continue;
        };
        let Ok(text) = str::from_utf8(source.source_bytes) else {
            continue;
        };
        let mut decoder = Decoder::new(source.source_path, text);
        let document = decoder.decode_layout(&root, limits);
        diagnostics.extend(decoder.diagnostics);
        if diagnostics.len() == diagnostic_start
            && let Some(document) = document
        {
            let mut reference_diagnostics = Vec::new();
            validate_layout_references(&document, text, graphs, &mut reference_diagnostics);
            if reference_diagnostics.is_empty() {
                layouts.push(document);
            } else {
                diagnostics.extend(reference_diagnostics);
            }
        }
    }
    sort_diagnostics(&mut diagnostics);
    (layouts, diagnostics)
}

fn utf8_or_empty(source: WorkflowSource<'_>) -> Option<&str> {
    str::from_utf8(source.source_bytes).ok()
}

fn layout_utf8_or_empty(source: LayoutSource<'_>) -> Option<&str> {
    str::from_utf8(source.source_bytes).ok()
}

fn first_excess_workflow<'a>(
    sources: &'a [WorkflowSource<'a>],
    limit: usize,
) -> Option<WorkflowSource<'a>> {
    let mut ordered = sources.to_vec();
    ordered.sort_by(|left, right| {
        left.source_path
            .as_bytes()
            .cmp(right.source_path.as_bytes())
    });
    ordered.get(limit).copied()
}

fn first_excess_layout<'a>(
    sources: &'a [LayoutSource<'a>],
    limit: usize,
) -> Option<LayoutSource<'a>> {
    let mut ordered = sources.to_vec();
    ordered.sort_by(|left, right| {
        left.source_path
            .as_bytes()
            .cmp(right.source_path.as_bytes())
    });
    ordered.get(limit).copied()
}

fn limit_diagnostic(
    source_path: &str,
    source: Option<&str>,
    code: WorkflowDiagnosticCode,
) -> WorkflowDiagnostic {
    make_diagnostic(
        source_path,
        source.unwrap_or(""),
        code,
        SourceSpan::empty(),
        "",
        None,
    )
}

struct DecodedGraph<'a> {
    document: WorkflowDocument,
    source: &'a str,
}

struct Decoder<'a> {
    source_path: &'a str,
    source: &'a str,
    diagnostics: Vec<WorkflowDiagnostic>,
}

impl<'a> Decoder<'a> {
    const fn new(source_path: &'a str, source: &'a str) -> Self {
        Self {
            source_path,
            source,
            diagnostics: Vec::new(),
        }
    }

    fn decode_graph(
        &mut self,
        root: &YamlNode,
        limits: WorkflowValidationLimits,
    ) -> Option<WorkflowDocument> {
        let Some(mapping) = root.mapping() else {
            self.push(WorkflowDiagnosticCode::InvalidField, root.span, "", None);
            return None;
        };
        self.unknown_fields(
            mapping,
            &[
                "kind",
                "schemaVersion",
                "documentId",
                "workflowId",
                "canonicalName",
                "permanent",
                "nodes",
                "edges",
            ],
            "",
        );
        self.exact_string(mapping, "kind", "aurora.cyclic-workflow", "/kind");
        self.version(mapping);
        let document_id = self.stable_id(mapping, "documentId", "/documentId");
        let workflow_id = self.stable_id(mapping, "workflowId", "/workflowId");
        let canonical_name = self.canonical_name(mapping, "canonicalName", "/canonicalName");
        let permanent = self.boolean(mapping, "permanent", "/permanent");
        let nodes = self.graph_nodes(mapping, limits);
        let edges = self.graph_edges(mapping, limits);
        if !self.diagnostics.is_empty() {
            return None;
        }
        let (
            Some(document_id),
            Some(workflow_id),
            Some(canonical_name),
            Some(permanent),
            Some(nodes),
            Some(edges),
        ) = (
            document_id,
            workflow_id,
            canonical_name,
            permanent,
            nodes,
            edges,
        )
        else {
            return None;
        };
        let document = WorkflowDocument {
            source_path: self.source_path.to_owned(),
            document_id,
            workflow_id,
            canonical_name,
            permanent,
            nodes,
            edges,
            span: root.span,
        };
        self.validate_graph_semantics(&document, root.span);
        self.diagnostics.is_empty().then_some(document)
    }

    fn graph_nodes(
        &mut self,
        mapping: &[MappingEntry],
        limits: WorkflowValidationLimits,
    ) -> Option<Vec<Node>> {
        let node = self.required(mapping, "nodes", "/nodes")?;
        let Some(items) = node.sequence() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                "/nodes",
                None,
            );
            return None;
        };
        if items.len() > limits.nodes_per_workflow() {
            self.push(
                WorkflowDiagnosticCode::ResourceBudgetExceeded,
                node.span,
                "/nodes",
                None,
            );
            return None;
        }
        let mut nodes = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let pointer = format!("/nodes/{index}");
            let diagnostic_start = self.diagnostics.len();
            if let Some(node) = self.graph_node(item, &pointer) {
                nodes.push(node);
            } else if self.diagnostics.len() == diagnostic_start {
                // 防止未来新增节点形状时静默裁剪无效项并发布部分 Graph。
                self.push(
                    WorkflowDiagnosticCode::InvalidField,
                    item.span,
                    &pointer,
                    None,
                );
            }
        }
        Some(nodes)
    }

    fn graph_node(&mut self, node: &YamlNode, pointer: &str) -> Option<Node> {
        let Some(mapping) = node.mapping() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                pointer,
                None,
            );
            return None;
        };
        let kind_pointer = format!("{pointer}/kind");
        let kind_text = self.string(mapping, "kind", &kind_pointer);
        let allowed = match kind_text.as_deref().and_then(node_allowed_fields) {
            Some(allowed) => allowed,
            None if kind_text.is_some() => {
                self.push(
                    WorkflowDiagnosticCode::InvalidField,
                    field_span(mapping, "kind", node.span),
                    &kind_pointer,
                    None,
                );
                return None;
            }
            None => return None,
        };
        self.unknown_fields(mapping, allowed, pointer);
        let node_id = self.stable_id(mapping, "nodeId", &format!("{pointer}/nodeId"));
        let canonical_name = self.canonical_name(
            mapping,
            "canonicalName",
            &format!("{pointer}/canonicalName"),
        );
        let structural = matches!(kind_text.as_deref(), Some("Entry" | "End"));
        let execution_order = if structural {
            None
        } else {
            self.u32(
                mapping,
                "executionOrder",
                &format!("{pointer}/executionOrder"),
                WorkflowDiagnosticCode::InvalidExecutionOrder,
            )
        };
        let cancellation_boundary = if structural {
            Some(false)
        } else {
            self.boolean(
                mapping,
                "cancellationBoundary",
                &format!("{pointer}/cancellationBoundary"),
            )
        };
        let kind = match kind_text.as_deref() {
            Some("Entry") => Some(NodeKind::Entry),
            Some("Action") => Some(NodeKind::Action),
            Some("Decision") => Some(NodeKind::Decision),
            Some("Fork") => Some(NodeKind::Fork),
            Some("Join") => self.join_kind(mapping, pointer),
            Some("Wait") => self.wait_kind(mapping, pointer),
            Some("Subworkflow") => self
                .stable_id(
                    mapping,
                    "targetWorkflowId",
                    &format!("{pointer}/targetWorkflowId"),
                )
                .map(|target_workflow_id| NodeKind::Subworkflow { target_workflow_id }),
            Some("End") => Some(NodeKind::End),
            _ => None,
        };
        let (Some(node_id), Some(canonical_name), Some(cancellation_boundary), Some(kind)) =
            (node_id, canonical_name, cancellation_boundary, kind)
        else {
            return None;
        };
        if !structural && execution_order.is_none() {
            return None;
        }
        Some(Node {
            node_id,
            canonical_name,
            execution_order,
            cancellation_boundary,
            kind,
            span: node.span,
        })
    }

    fn join_kind(&mut self, mapping: &[MappingEntry], pointer: &str) -> Option<NodeKind> {
        let mode_pointer = format!("{pointer}/mode");
        let mode = self.string(mapping, "mode", &mode_pointer);
        match mode.as_deref() {
            Some("merge") => {
                if has_field(mapping, "forkId") || has_field(mapping, "loserPolicy") {
                    self.push(
                        WorkflowDiagnosticCode::InvalidForkJoinPair,
                        field_span(
                            mapping,
                            "forkId",
                            field_span(mapping, "loserPolicy", mapping_span(mapping)),
                        ),
                        pointer,
                        None,
                    );
                    None
                } else {
                    Some(NodeKind::Join {
                        mode: JoinMode::Merge,
                        fork_id: None,
                        loser_policy: None,
                    })
                }
            }
            Some("join-all") => {
                let fork_id = self.stable_id(mapping, "forkId", &format!("{pointer}/forkId"));
                if has_field(mapping, "loserPolicy") {
                    self.push(
                        WorkflowDiagnosticCode::InvalidJoinMode,
                        field_span(mapping, "loserPolicy", mapping_span(mapping)),
                        pointer,
                        None,
                    );
                    None
                } else {
                    fork_id.map(|fork_id| NodeKind::Join {
                        mode: JoinMode::JoinAll,
                        fork_id: Some(fork_id),
                        loser_policy: None,
                    })
                }
            }
            Some("join-any") => {
                let fork_id = self.stable_id(mapping, "forkId", &format!("{pointer}/forkId"));
                let policy = self.string(mapping, "loserPolicy", &format!("{pointer}/loserPolicy"));
                let policy = match policy.as_deref() {
                    Some("cancel-others") => Some(JoinPolicy::CancelOthers),
                    Some("keep-running") => Some(JoinPolicy::KeepRunning),
                    Some("wait-at-boundary") => Some(JoinPolicy::WaitAtBoundary),
                    Some(_) => {
                        self.push(
                            WorkflowDiagnosticCode::InvalidJoinMode,
                            field_span(mapping, "loserPolicy", mapping_span(mapping)),
                            pointer,
                            None,
                        );
                        None
                    }
                    None => None,
                };
                fork_id
                    .zip(policy)
                    .map(|(fork_id, loser_policy)| NodeKind::Join {
                        mode: JoinMode::JoinAny,
                        fork_id: Some(fork_id),
                        loser_policy: Some(loser_policy),
                    })
            }
            Some(_) => {
                self.push(
                    WorkflowDiagnosticCode::InvalidJoinMode,
                    field_span(mapping, "mode", mapping_span(mapping)),
                    &mode_pointer,
                    None,
                );
                None
            }
            None => None,
        }
    }

    fn wait_kind(&mut self, mapping: &[MappingEntry], pointer: &str) -> Option<NodeKind> {
        let mode_pointer = format!("{pointer}/mode");
        let mode = self.string(mapping, "mode", &mode_pointer);
        match mode.as_deref() {
            Some("cycles") => {
                if has_any_field(mapping, &["conditionId", "timeoutCycles", "permanent"]) {
                    self.push(
                        WorkflowDiagnosticCode::InvalidWaitPolicy,
                        mapping_span(mapping),
                        pointer,
                        None,
                    );
                    return None;
                }
                self.positive_u64(
                    mapping,
                    "waitCycles",
                    &format!("{pointer}/waitCycles"),
                    WorkflowDiagnosticCode::InvalidWaitRange,
                )
                .map(|wait_cycles| NodeKind::Wait(WaitMode::Cycles { wait_cycles }))
            }
            Some("condition") => {
                if has_field(mapping, "waitCycles") {
                    self.push(
                        WorkflowDiagnosticCode::InvalidWaitPolicy,
                        field_span(mapping, "waitCycles", mapping_span(mapping)),
                        pointer,
                        None,
                    );
                    return None;
                }
                let condition_id =
                    self.stable_id(mapping, "conditionId", &format!("{pointer}/conditionId"));
                let has_timeout = has_field(mapping, "timeoutCycles");
                let has_permanent = has_field(mapping, "permanent");
                if has_timeout == has_permanent {
                    self.push(
                        WorkflowDiagnosticCode::InvalidWaitPolicy,
                        mapping_span(mapping),
                        pointer,
                        None,
                    );
                    return None;
                }
                let timeout_cycles = if has_timeout {
                    self.positive_u64(
                        mapping,
                        "timeoutCycles",
                        &format!("{pointer}/timeoutCycles"),
                        WorkflowDiagnosticCode::InvalidWaitRange,
                    )
                    .map(Some)
                } else {
                    match self.boolean(mapping, "permanent", &format!("{pointer}/permanent")) {
                        Some(true) => Some(None),
                        Some(false) => {
                            self.push(
                                WorkflowDiagnosticCode::InvalidWaitPolicy,
                                field_span(mapping, "permanent", mapping_span(mapping)),
                                pointer,
                                None,
                            );
                            None
                        }
                        None => None,
                    }
                };
                condition_id
                    .zip(timeout_cycles)
                    .map(|(condition_id, timeout_cycles)| {
                        NodeKind::Wait(WaitMode::Condition {
                            condition_id,
                            timeout_cycles,
                            permanent: !has_timeout,
                        })
                    })
            }
            Some(_) => {
                self.push(
                    WorkflowDiagnosticCode::InvalidWaitPolicy,
                    field_span(mapping, "mode", mapping_span(mapping)),
                    &mode_pointer,
                    None,
                );
                None
            }
            None => None,
        }
    }

    fn graph_edges(
        &mut self,
        mapping: &[MappingEntry],
        limits: WorkflowValidationLimits,
    ) -> Option<Vec<Edge>> {
        let node = self.required(mapping, "edges", "/edges")?;
        let Some(items) = node.sequence() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                "/edges",
                None,
            );
            return None;
        };
        if items.len() > limits.edges_per_workflow() {
            self.push(
                WorkflowDiagnosticCode::ResourceBudgetExceeded,
                node.span,
                "/edges",
                None,
            );
            return None;
        }
        let mut edges = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let pointer = format!("/edges/{index}");
            let diagnostic_start = self.diagnostics.len();
            if let Some(edge) = self.graph_edge(item, &pointer) {
                edges.push(edge);
            } else if self.diagnostics.len() == diagnostic_start {
                // 与节点相同，任何未解码 edge 都必须阻止部分 Graph 发布。
                self.push(
                    WorkflowDiagnosticCode::InvalidField,
                    item.span,
                    &pointer,
                    None,
                );
            }
        }
        Some(edges)
    }

    fn graph_edge(&mut self, node: &YamlNode, pointer: &str) -> Option<Edge> {
        let Some(mapping) = node.mapping() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                pointer,
                None,
            );
            return None;
        };
        self.unknown_fields(
            mapping,
            &[
                "edgeId",
                "kind",
                "sourceNodeId",
                "targetNodeId",
                "conditionId",
                "priority",
                "branchOrder",
                "backedge",
                "maxTraversalsPerRun",
            ],
            pointer,
        );
        let edge_id = self.stable_id(mapping, "edgeId", &format!("{pointer}/edgeId"));
        self.exact_string(mapping, "kind", "control", &format!("{pointer}/kind"));
        let source_node_id =
            self.stable_id(mapping, "sourceNodeId", &format!("{pointer}/sourceNodeId"));
        let target_node_id =
            self.stable_id(mapping, "targetNodeId", &format!("{pointer}/targetNodeId"));
        let condition_id =
            self.optional_stable_id(mapping, "conditionId", &format!("{pointer}/conditionId"));
        let priority = self.optional_u32(
            mapping,
            "priority",
            &format!("{pointer}/priority"),
            WorkflowDiagnosticCode::InvalidDecisionPriority,
        );
        let branch_order = self.optional_u32(
            mapping,
            "branchOrder",
            &format!("{pointer}/branchOrder"),
            WorkflowDiagnosticCode::InvalidBranchOrder,
        );
        let backedge_flag = self.boolean(mapping, "backedge", &format!("{pointer}/backedge"));
        let backedge = match backedge_flag {
            Some(true) => self
                .positive_u64(
                    mapping,
                    "maxTraversalsPerRun",
                    &format!("{pointer}/maxTraversalsPerRun"),
                    WorkflowDiagnosticCode::UnboundedBackedge,
                )
                .map(|max_traversals_per_run| {
                    Some(Backedge {
                        max_traversals_per_run,
                    })
                }),
            Some(false) if has_field(mapping, "maxTraversalsPerRun") => {
                self.push(
                    WorkflowDiagnosticCode::UnboundedBackedge,
                    field_span(mapping, "maxTraversalsPerRun", node.span),
                    pointer,
                    None,
                );
                Some(None)
            }
            Some(false) => Some(None),
            None => None,
        };
        let (Some(edge_id), Some(source_node_id), Some(target_node_id), Some(backedge)) =
            (edge_id, source_node_id, target_node_id, backedge)
        else {
            return None;
        };
        Some(Edge {
            edge_id,
            source_node_id,
            target_node_id,
            condition_id,
            priority,
            branch_order,
            backedge,
            span: node.span,
        })
    }

    fn validate_graph_semantics(&mut self, document: &WorkflowDocument, root_span: SourceSpan) {
        if !self.validate_graph_identities(document) || !self.validate_execution_order(document) {
            return;
        }

        let nodes = document
            .nodes
            .iter()
            .map(|node| (node.node_id, node))
            .collect::<BTreeMap<_, _>>();
        let mut references_valid = true;
        let mut invalid_order_sources = BTreeSet::new();
        let mut edge_keys = BTreeSet::new();
        for (index, edge) in document.edges.iter().enumerate() {
            let pointer = format!("/edges/{index}");
            let source = nodes.get(&edge.source_node_id).copied();
            let target = nodes.get(&edge.target_node_id).copied();
            if source.is_none() || target.is_none() {
                self.push(
                    WorkflowDiagnosticCode::DanglingEdge,
                    edge.span,
                    &pointer,
                    Some(edge.edge_id),
                );
                references_valid = false;
                continue;
            }
            let key = (edge.source_node_id, edge.target_node_id);
            if !edge_keys.insert(key) {
                self.push(
                    WorkflowDiagnosticCode::DuplicateEdge,
                    edge.span,
                    &pointer,
                    Some(edge.edge_id),
                );
                references_valid = false;
                continue;
            }
            if let Some(source) = source
                && !self.validate_edge_source_properties(edge, source, &pointer)
            {
                invalid_order_sources.insert(source.node_id);
            }
        }
        self.validate_join_fork_references(document, &nodes);
        if references_valid {
            self.validate_control_degrees(document, root_span);
            self.validate_ordered_edges(document, &invalid_order_sources);
        }
    }

    fn validate_graph_identities(&mut self, document: &WorkflowDocument) -> bool {
        let diagnostic_start = self.diagnostics.len();
        let mut identities = BTreeSet::new();
        identities.insert(document.document_id);
        if !identities.insert(document.workflow_id) {
            self.push(
                WorkflowDiagnosticCode::DuplicateStableIdentity,
                SourceSpan::empty(),
                "/workflowId",
                Some(document.workflow_id),
            );
        }
        let mut names = BTreeSet::new();
        for (index, node) in document.nodes.iter().enumerate() {
            if !identities.insert(node.node_id) {
                self.push(
                    WorkflowDiagnosticCode::DuplicateStableIdentity,
                    node.span,
                    &format!("/nodes/{index}/nodeId"),
                    Some(node.node_id),
                );
            }
            if !names.insert(node.canonical_name.as_str()) {
                self.push(
                    WorkflowDiagnosticCode::InvalidCanonicalName,
                    node.span,
                    &format!("/nodes/{index}/canonicalName"),
                    Some(node.node_id),
                );
            }
        }
        for (index, edge) in document.edges.iter().enumerate() {
            if !identities.insert(edge.edge_id) {
                self.push(
                    WorkflowDiagnosticCode::DuplicateStableIdentity,
                    edge.span,
                    &format!("/edges/{index}/edgeId"),
                    Some(edge.edge_id),
                );
            }
        }
        self.diagnostics.len() == diagnostic_start
    }

    fn validate_execution_order(&mut self, document: &WorkflowDocument) -> bool {
        let diagnostic_start = self.diagnostics.len();
        let mut ordered = document
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| node.execution_order.map(|order| (order, index, node)))
            .collect::<Vec<_>>();
        ordered.sort_by_key(|(order, _, _)| *order);
        let mut seen = BTreeSet::new();
        let mut duplicate = false;
        for (order, index, node) in &ordered {
            if !seen.insert(*order) {
                duplicate = true;
                self.push(
                    WorkflowDiagnosticCode::InvalidExecutionOrder,
                    node.span,
                    &format!("/nodes/{index}/executionOrder"),
                    Some(node.node_id),
                );
            }
        }
        if duplicate {
            return false;
        }
        for (expected, (actual, index, node)) in ordered.iter().enumerate() {
            if usize::try_from(*actual) != Ok(expected) {
                self.push(
                    WorkflowDiagnosticCode::InvalidExecutionOrder,
                    node.span,
                    &format!("/nodes/{index}/executionOrder"),
                    Some(node.node_id),
                );
                break;
            }
        }
        self.diagnostics.len() == diagnostic_start
    }

    fn validate_edge_source_properties(
        &mut self,
        edge: &Edge,
        source: &Node,
        pointer: &str,
    ) -> bool {
        let invalid_code = match source.kind {
            NodeKind::Decision
                if edge.priority.is_none()
                    || edge.condition_id.is_none()
                    || edge.branch_order.is_some() =>
            {
                Some(WorkflowDiagnosticCode::InvalidDecisionPriority)
            }
            NodeKind::Fork
                if edge.branch_order.is_none()
                    || edge.priority.is_some()
                    || edge.condition_id.is_some() =>
            {
                Some(WorkflowDiagnosticCode::InvalidBranchOrder)
            }
            NodeKind::Action if edge.priority.is_some() || edge.branch_order.is_some() => {
                Some(WorkflowDiagnosticCode::InvalidField)
            }
            NodeKind::Entry
            | NodeKind::Join { .. }
            | NodeKind::Wait(_)
            | NodeKind::Subworkflow { .. }
            | NodeKind::End
                if edge.priority.is_some()
                    || edge.branch_order.is_some()
                    || edge.condition_id.is_some() =>
            {
                Some(WorkflowDiagnosticCode::InvalidField)
            }
            _ => None,
        };
        if let Some(code) = invalid_code {
            self.push(code, edge.span, pointer, Some(edge.edge_id));
            false
        } else {
            true
        }
    }

    fn validate_join_fork_references(
        &mut self,
        document: &WorkflowDocument,
        nodes: &BTreeMap<StableId, &Node>,
    ) {
        for (index, node) in document.nodes.iter().enumerate() {
            let NodeKind::Join {
                mode,
                fork_id: Some(fork_id),
                ..
            } = node.kind
            else {
                continue;
            };
            if mode == JoinMode::Merge {
                continue;
            }
            if !nodes
                .get(&fork_id)
                .is_some_and(|fork| fork.kind == NodeKind::Fork)
            {
                self.push(
                    WorkflowDiagnosticCode::InvalidForkJoinPair,
                    node.span,
                    &format!("/nodes/{index}/forkId"),
                    Some(node.node_id),
                );
            }
        }
    }

    fn validate_control_degrees(&mut self, document: &WorkflowDocument, root_span: SourceSpan) {
        let mut incoming = BTreeMap::<StableId, usize>::new();
        let mut outgoing = BTreeMap::<StableId, usize>::new();
        for edge in &document.edges {
            *incoming.entry(edge.target_node_id).or_default() += 1;
            *outgoing.entry(edge.source_node_id).or_default() += 1;
        }
        if !self.validate_entry_count(document, root_span) {
            return;
        }
        for (index, node) in document.nodes.iter().enumerate() {
            let in_count = incoming.get(&node.node_id).copied().unwrap_or(0);
            let out_count = outgoing.get(&node.node_id).copied().unwrap_or(0);
            self.validate_node_degree(node, index, in_count, out_count);
        }
    }

    fn validate_entry_count(&mut self, document: &WorkflowDocument, root_span: SourceSpan) -> bool {
        let entries = document
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.kind == NodeKind::Entry)
            .collect::<Vec<_>>();
        if entries.is_empty() {
            self.push(
                WorkflowDiagnosticCode::MissingEntry,
                root_span,
                "/nodes",
                None,
            );
            false
        } else if entries.len() > 1 {
            for (index, node) in entries.iter().skip(1) {
                self.push(
                    WorkflowDiagnosticCode::MultipleEntries,
                    node.span,
                    &format!("/nodes/{index}"),
                    Some(node.node_id),
                );
            }
            false
        } else {
            true
        }
    }

    fn validate_node_degree(
        &mut self,
        node: &Node,
        index: usize,
        in_count: usize,
        out_count: usize,
    ) {
        let pointer = format!("/nodes/{index}");
        match node.kind {
            NodeKind::Entry => {
                if in_count != 0 || out_count != 1 {
                    self.push(
                        WorkflowDiagnosticCode::InvalidEntryEdge,
                        node.span,
                        &pointer,
                        Some(node.node_id),
                    );
                }
            }
            NodeKind::End => {
                if in_count != 1 || out_count != 0 {
                    self.push(
                        WorkflowDiagnosticCode::InvalidEndEdge,
                        node.span,
                        &pointer,
                        Some(node.node_id),
                    );
                }
            }
            NodeKind::Join { .. } => {
                if in_count < 2 || out_count != 1 {
                    self.push(
                        WorkflowDiagnosticCode::InvalidControlDegree,
                        node.span,
                        &pointer,
                        Some(node.node_id),
                    );
                }
            }
            NodeKind::Decision | NodeKind::Fork => {
                if in_count > 1 {
                    self.push(
                        WorkflowDiagnosticCode::ImplicitMerge,
                        node.span,
                        &pointer,
                        Some(node.node_id),
                    );
                } else if in_count != 1 || out_count < 2 {
                    self.push(
                        WorkflowDiagnosticCode::InvalidControlDegree,
                        node.span,
                        &pointer,
                        Some(node.node_id),
                    );
                }
            }
            _ => {
                if in_count > 1 {
                    self.push(
                        WorkflowDiagnosticCode::ImplicitMerge,
                        node.span,
                        &pointer,
                        Some(node.node_id),
                    );
                } else if in_count != 1 || out_count != 1 {
                    self.push(
                        WorkflowDiagnosticCode::InvalidControlDegree,
                        node.span,
                        &pointer,
                        Some(node.node_id),
                    );
                }
            }
        }
    }

    fn validate_ordered_edges(
        &mut self,
        document: &WorkflowDocument,
        invalid_sources: &BTreeSet<StableId>,
    ) {
        for node in &document.nodes {
            if invalid_sources.contains(&node.node_id) {
                continue;
            }
            let (code, value_of) = match node.kind {
                NodeKind::Decision => (
                    WorkflowDiagnosticCode::InvalidDecisionPriority,
                    edge_priority as fn(&Edge) -> Option<u32>,
                ),
                NodeKind::Fork => (
                    WorkflowDiagnosticCode::InvalidBranchOrder,
                    edge_branch_order as fn(&Edge) -> Option<u32>,
                ),
                _ => continue,
            };
            let mut ordered = document
                .edges
                .iter()
                .enumerate()
                .filter(|(_, edge)| edge.source_node_id == node.node_id)
                .filter_map(|(index, edge)| value_of(edge).map(|order| (order, index, edge)))
                .collect::<Vec<_>>();
            let mut seen = BTreeSet::new();
            let mut duplicate = false;
            for (order, index, edge) in &ordered {
                if !seen.insert(*order) {
                    duplicate = true;
                    self.push(
                        code,
                        edge.span,
                        &format!("/edges/{index}"),
                        Some(edge.edge_id),
                    );
                }
            }
            if duplicate {
                continue;
            }
            ordered.sort_by_key(|(order, _, _)| *order);
            if let Some((_, index, edge)) = ordered
                .iter()
                .enumerate()
                .find(|(expected, (actual, _, _))| usize::try_from(*actual) != Ok(*expected))
                .map(|(_, value)| value)
            {
                self.push(
                    code,
                    edge.span,
                    &format!("/edges/{index}"),
                    Some(edge.edge_id),
                );
            }
        }
    }

    fn decode_layout(
        &mut self,
        root: &YamlNode,
        limits: WorkflowValidationLimits,
    ) -> Option<WorkflowLayoutDocument> {
        let Some(mapping) = root.mapping() else {
            self.push(WorkflowDiagnosticCode::InvalidField, root.span, "", None);
            return None;
        };
        self.unknown_fields(
            mapping,
            &[
                "kind",
                "schemaVersion",
                "documentId",
                "workflowId",
                "nodes",
                "edges",
                "groups",
            ],
            "",
        );
        self.exact_string(mapping, "kind", "aurora.workflow-layout", "/kind");
        self.version(mapping);
        let document_id = self.stable_id(mapping, "documentId", "/documentId");
        let workflow_id = self.stable_id(mapping, "workflowId", "/workflowId");
        let nodes = self.layout_nodes(mapping, limits);
        let edges = self.layout_edges(mapping, limits);
        let groups = self.layout_groups(mapping, limits);
        if !self.diagnostics.is_empty() {
            return None;
        }
        let (Some(document_id), Some(workflow_id), Some(nodes), Some(edges), Some(groups)) =
            (document_id, workflow_id, nodes, edges, groups)
        else {
            return None;
        };
        let document = WorkflowLayoutDocument {
            source_path: self.source_path.to_owned(),
            document_id,
            workflow_id,
            nodes,
            edges,
            groups,
            span: root.span,
        };
        self.validate_layout_local(&document);
        self.diagnostics.is_empty().then_some(document)
    }

    fn layout_nodes(
        &mut self,
        mapping: &[MappingEntry],
        limits: WorkflowValidationLimits,
    ) -> Option<Vec<LayoutNode>> {
        let node = self.required(mapping, "nodes", "/nodes")?;
        let Some(items) = node.sequence() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                "/nodes",
                None,
            );
            return None;
        };
        if items.len() > limits.layout_nodes() {
            self.push(
                WorkflowDiagnosticCode::LayoutLimitExceeded,
                node.span,
                "/nodes",
                None,
            );
            return None;
        }
        let mut result = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let pointer = format!("/nodes/{index}");
            let Some(entry) = item.mapping() else {
                self.push(
                    WorkflowDiagnosticCode::InvalidField,
                    item.span,
                    &pointer,
                    None,
                );
                continue;
            };
            self.unknown_fields(
                entry,
                &[
                    "nodeId",
                    "xCanvasUnits",
                    "yCanvasUnits",
                    "widthCanvasUnits",
                    "heightCanvasUnits",
                ],
                &pointer,
            );
            let node_id = self.stable_id(entry, "nodeId", &format!("{pointer}/nodeId"));
            let x = self.i32(entry, "xCanvasUnits", &format!("{pointer}/xCanvasUnits"));
            let y = self.i32(entry, "yCanvasUnits", &format!("{pointer}/yCanvasUnits"));
            let width = self.positive_u32(
                entry,
                "widthCanvasUnits",
                &format!("{pointer}/widthCanvasUnits"),
            );
            let height = self.positive_u32(
                entry,
                "heightCanvasUnits",
                &format!("{pointer}/heightCanvasUnits"),
            );
            if let (
                Some(node_id),
                Some(x_canvas_units),
                Some(y_canvas_units),
                Some(width_canvas_units),
                Some(height_canvas_units),
            ) = (node_id, x, y, width, height)
            {
                result.push(LayoutNode {
                    node_id,
                    position: LayoutPoint {
                        x_canvas_units,
                        y_canvas_units,
                    },
                    width_canvas_units,
                    height_canvas_units,
                    span: item.span,
                });
            }
        }
        Some(result)
    }

    fn layout_edges(
        &mut self,
        mapping: &[MappingEntry],
        limits: WorkflowValidationLimits,
    ) -> Option<Vec<LayoutEdge>> {
        let node = self.required(mapping, "edges", "/edges")?;
        let Some(items) = node.sequence() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                "/edges",
                None,
            );
            return None;
        };
        if items.len() > limits.layout_edges() {
            self.push(
                WorkflowDiagnosticCode::LayoutLimitExceeded,
                node.span,
                "/edges",
                None,
            );
            return None;
        }
        let mut result = Vec::with_capacity(items.len());
        let mut point_count = 0_usize;
        for (index, item) in items.iter().enumerate() {
            let pointer = format!("/edges/{index}");
            let Some(entry) = item.mapping() else {
                self.push(
                    WorkflowDiagnosticCode::InvalidField,
                    item.span,
                    &pointer,
                    None,
                );
                continue;
            };
            self.unknown_fields(entry, &["edgeId", "points"], &pointer);
            let edge_id = self.stable_id(entry, "edgeId", &format!("{pointer}/edgeId"));
            let points_node = self.required(entry, "points", &format!("{pointer}/points"));
            let points = points_node.and_then(|points_node| {
                let Some(points) = points_node.sequence() else {
                    self.push(
                        WorkflowDiagnosticCode::InvalidField,
                        points_node.span,
                        &format!("{pointer}/points"),
                        None,
                    );
                    return None;
                };
                let Some(total) = point_count.checked_add(points.len()) else {
                    self.push(
                        WorkflowDiagnosticCode::LayoutLimitExceeded,
                        points_node.span,
                        &format!("{pointer}/points"),
                        None,
                    );
                    return None;
                };
                if total > limits.layout_points() {
                    self.push(
                        WorkflowDiagnosticCode::LayoutLimitExceeded,
                        points_node.span,
                        &format!("{pointer}/points"),
                        None,
                    );
                    return None;
                }
                point_count = total;
                let mut decoded = Vec::with_capacity(points.len());
                for (point_index, point) in points.iter().enumerate() {
                    let point_pointer = format!("{pointer}/points/{point_index}");
                    if let Some(point) = self.layout_point(point, &point_pointer) {
                        decoded.push(point);
                    }
                }
                Some(decoded)
            });
            if let (Some(edge_id), Some(points)) = (edge_id, points) {
                result.push(LayoutEdge {
                    edge_id,
                    points,
                    span: item.span,
                });
            }
        }
        Some(result)
    }

    fn layout_point(&mut self, node: &YamlNode, pointer: &str) -> Option<LayoutPoint> {
        let Some(mapping) = node.mapping() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                pointer,
                None,
            );
            return None;
        };
        self.unknown_fields(mapping, &["xCanvasUnits", "yCanvasUnits"], pointer);
        self.i32(mapping, "xCanvasUnits", &format!("{pointer}/xCanvasUnits"))
            .zip(self.i32(mapping, "yCanvasUnits", &format!("{pointer}/yCanvasUnits")))
            .map(|(x_canvas_units, y_canvas_units)| LayoutPoint {
                x_canvas_units,
                y_canvas_units,
            })
    }

    fn layout_groups(
        &mut self,
        mapping: &[MappingEntry],
        limits: WorkflowValidationLimits,
    ) -> Option<Vec<LayoutGroup>> {
        let node = self.required(mapping, "groups", "/groups")?;
        let Some(items) = node.sequence() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                "/groups",
                None,
            );
            return None;
        };
        if items.len() > limits.layout_groups() {
            self.push(
                WorkflowDiagnosticCode::LayoutLimitExceeded,
                node.span,
                "/groups",
                None,
            );
            return None;
        }
        let mut result = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let pointer = format!("/groups/{index}");
            let Some(entry) = item.mapping() else {
                self.push(
                    WorkflowDiagnosticCode::InvalidField,
                    item.span,
                    &pointer,
                    None,
                );
                continue;
            };
            self.unknown_fields(
                entry,
                &[
                    "groupId",
                    "canonicalName",
                    "nodeIds",
                    "xCanvasUnits",
                    "yCanvasUnits",
                    "widthCanvasUnits",
                    "heightCanvasUnits",
                    "annotation",
                ],
                &pointer,
            );
            let group_id = self.stable_id(entry, "groupId", &format!("{pointer}/groupId"));
            let canonical_name =
                self.canonical_name(entry, "canonicalName", &format!("{pointer}/canonicalName"));
            let node_ids = self.stable_id_sequence(entry, "nodeIds", &format!("{pointer}/nodeIds"));
            let x = self.i32(entry, "xCanvasUnits", &format!("{pointer}/xCanvasUnits"));
            let y = self.i32(entry, "yCanvasUnits", &format!("{pointer}/yCanvasUnits"));
            let width = self.positive_u32(
                entry,
                "widthCanvasUnits",
                &format!("{pointer}/widthCanvasUnits"),
            );
            let height = self.positive_u32(
                entry,
                "heightCanvasUnits",
                &format!("{pointer}/heightCanvasUnits"),
            );
            let (annotation, annotation_valid) =
                self.optional_string(entry, "annotation", &format!("{pointer}/annotation"));
            if let (
                Some(group_id),
                Some(canonical_name),
                Some(node_ids),
                Some(x_canvas_units),
                Some(y_canvas_units),
                Some(width_canvas_units),
                Some(height_canvas_units),
            ) = (group_id, canonical_name, node_ids, x, y, width, height)
                && annotation_valid
            {
                result.push(LayoutGroup {
                    group_id,
                    canonical_name,
                    node_ids,
                    position: LayoutPoint {
                        x_canvas_units,
                        y_canvas_units,
                    },
                    width_canvas_units,
                    height_canvas_units,
                    annotation,
                    span: item.span,
                });
            }
        }
        Some(result)
    }

    fn validate_layout_local(&mut self, document: &WorkflowLayoutDocument) {
        let mut identities = BTreeSet::from([document.document_id]);
        let mut node_refs = BTreeSet::new();
        for (index, node) in document.nodes.iter().enumerate() {
            if !node_refs.insert(node.node_id) {
                self.push(
                    WorkflowDiagnosticCode::DuplicateStableIdentity,
                    node.span,
                    &format!("/nodes/{index}/nodeId"),
                    Some(node.node_id),
                );
            }
        }
        let mut edge_refs = BTreeSet::new();
        for (index, edge) in document.edges.iter().enumerate() {
            if !edge_refs.insert(edge.edge_id) {
                self.push(
                    WorkflowDiagnosticCode::DuplicateStableIdentity,
                    edge.span,
                    &format!("/edges/{index}/edgeId"),
                    Some(edge.edge_id),
                );
            }
        }
        let mut names = BTreeSet::new();
        for (index, group) in document.groups.iter().enumerate() {
            if !identities.insert(group.group_id) {
                self.push(
                    WorkflowDiagnosticCode::DuplicateStableIdentity,
                    group.span,
                    &format!("/groups/{index}/groupId"),
                    Some(group.group_id),
                );
            }
            if !names.insert(group.canonical_name.as_str()) {
                self.push(
                    WorkflowDiagnosticCode::InvalidCanonicalName,
                    group.span,
                    &format!("/groups/{index}/canonicalName"),
                    Some(group.group_id),
                );
            }
            let mut node_ids = BTreeSet::new();
            for (node_index, node_id) in group.node_ids.iter().enumerate() {
                if !node_ids.insert(*node_id) {
                    self.push(
                        WorkflowDiagnosticCode::DuplicateStableIdentity,
                        group.span,
                        &format!("/groups/{index}/nodeIds/{node_index}"),
                        Some(*node_id),
                    );
                }
            }
        }
    }

    fn unknown_fields(&mut self, mapping: &[MappingEntry], allowed: &[&str], pointer: &str) {
        for entry in mapping {
            if !allowed.contains(&entry.key.as_str()) {
                self.push(
                    WorkflowDiagnosticCode::UnknownField,
                    entry.key_span,
                    &format!("{pointer}/{}", entry.key),
                    None,
                );
            }
        }
    }

    fn version(&mut self, mapping: &[MappingEntry]) {
        let Some(node) = field(mapping, "schemaVersion") else {
            self.push(
                WorkflowDiagnosticCode::UnsupportedSchemaVersion,
                mapping_span(mapping),
                "/schemaVersion",
                None,
            );
            return;
        };
        let Some(version) = node.mapping() else {
            self.push(
                WorkflowDiagnosticCode::UnsupportedSchemaVersion,
                node.span,
                "/schemaVersion",
                None,
            );
            return;
        };
        self.unknown_fields(version, &["major", "minor", "lifecycle"], "/schemaVersion");
        let valid = field(version, "major").and_then(YamlNode::integer) == Some(1)
            && field(version, "minor").and_then(YamlNode::integer) == Some(0)
            && field(version, "lifecycle").and_then(YamlNode::string) == Some("preview");
        if !valid {
            self.push(
                WorkflowDiagnosticCode::UnsupportedSchemaVersion,
                node.span,
                "/schemaVersion",
                None,
            );
        }
    }

    fn required<'b>(
        &mut self,
        mapping: &'b [MappingEntry],
        key: &str,
        pointer: &str,
    ) -> Option<&'b YamlNode> {
        let value = field(mapping, key);
        if value.is_none() {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                mapping_span(mapping),
                pointer,
                None,
            );
        }
        value
    }

    fn string(&mut self, mapping: &[MappingEntry], key: &str, pointer: &str) -> Option<String> {
        let node = self.required(mapping, key, pointer)?;
        let Some(value) = node.string() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                pointer,
                None,
            );
            return None;
        };
        Some(value.to_owned())
    }

    fn optional_string(
        &mut self,
        mapping: &[MappingEntry],
        key: &str,
        pointer: &str,
    ) -> (Option<String>, bool) {
        let Some(node) = field(mapping, key) else {
            return (None, true);
        };
        let Some(value) = node.string() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                pointer,
                None,
            );
            return (None, false);
        };
        (Some(value.to_owned()), true)
    }

    fn exact_string(&mut self, mapping: &[MappingEntry], key: &str, expected: &str, pointer: &str) {
        let Some(node) = self.required(mapping, key, pointer) else {
            return;
        };
        if node.string() != Some(expected) {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                pointer,
                None,
            );
        }
    }

    fn boolean(&mut self, mapping: &[MappingEntry], key: &str, pointer: &str) -> Option<bool> {
        let node = self.required(mapping, key, pointer)?;
        let Some(value) = node.boolean() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                pointer,
                None,
            );
            return None;
        };
        Some(value)
    }

    fn stable_id(
        &mut self,
        mapping: &[MappingEntry],
        key: &str,
        pointer: &str,
    ) -> Option<StableId> {
        let node = self.required(mapping, key, pointer)?;
        let Some(text) = node.string() else {
            self.push(
                WorkflowDiagnosticCode::InvalidStableIdentity,
                node.span,
                pointer,
                None,
            );
            return None;
        };
        let Some(value) = StableId::parse(text) else {
            self.push(
                WorkflowDiagnosticCode::InvalidStableIdentity,
                node.span,
                pointer,
                None,
            );
            return None;
        };
        Some(value)
    }

    fn optional_stable_id(
        &mut self,
        mapping: &[MappingEntry],
        key: &str,
        pointer: &str,
    ) -> Option<StableId> {
        let node = field(mapping, key)?;
        let Some(text) = node.string() else {
            self.push(
                WorkflowDiagnosticCode::InvalidStableIdentity,
                node.span,
                pointer,
                None,
            );
            return None;
        };
        let result = StableId::parse(text);
        if result.is_none() {
            self.push(
                WorkflowDiagnosticCode::InvalidStableIdentity,
                node.span,
                pointer,
                None,
            );
        }
        result
    }

    fn stable_id_sequence(
        &mut self,
        mapping: &[MappingEntry],
        key: &str,
        pointer: &str,
    ) -> Option<Vec<StableId>> {
        let node = self.required(mapping, key, pointer)?;
        let Some(items) = node.sequence() else {
            self.push(
                WorkflowDiagnosticCode::InvalidField,
                node.span,
                pointer,
                None,
            );
            return None;
        };
        let mut result = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let item_pointer = format!("{pointer}/{index}");
            let Some(text) = item.string() else {
                self.push(
                    WorkflowDiagnosticCode::InvalidStableIdentity,
                    item.span,
                    &item_pointer,
                    None,
                );
                continue;
            };
            if let Some(value) = StableId::parse(text) {
                result.push(value);
            } else {
                self.push(
                    WorkflowDiagnosticCode::InvalidStableIdentity,
                    item.span,
                    &item_pointer,
                    None,
                );
            }
        }
        Some(result)
    }

    fn canonical_name(
        &mut self,
        mapping: &[MappingEntry],
        key: &str,
        pointer: &str,
    ) -> Option<String> {
        let node = self.required(mapping, key, pointer)?;
        let Some(value) = node.string() else {
            self.push(
                WorkflowDiagnosticCode::InvalidCanonicalName,
                node.span,
                pointer,
                None,
            );
            return None;
        };
        if canonical_name_is_valid(value) {
            Some(value.to_owned())
        } else {
            self.push(
                WorkflowDiagnosticCode::InvalidCanonicalName,
                node.span,
                pointer,
                None,
            );
            None
        }
    }

    fn u32(
        &mut self,
        mapping: &[MappingEntry],
        key: &str,
        pointer: &str,
        code: WorkflowDiagnosticCode,
    ) -> Option<u32> {
        let node = self.required(mapping, key, pointer)?;
        let value = node.integer().and_then(|value| u32::try_from(value).ok());
        if value.is_none() {
            self.push(code, node.span, pointer, None);
        }
        value
    }

    fn optional_u32(
        &mut self,
        mapping: &[MappingEntry],
        key: &str,
        pointer: &str,
        code: WorkflowDiagnosticCode,
    ) -> Option<u32> {
        let node = field(mapping, key)?;
        let value = node.integer().and_then(|value| u32::try_from(value).ok());
        if value.is_none() {
            self.push(code, node.span, pointer, None);
        }
        value
    }

    fn positive_u64(
        &mut self,
        mapping: &[MappingEntry],
        key: &str,
        pointer: &str,
        code: WorkflowDiagnosticCode,
    ) -> Option<u64> {
        let node = self.required(mapping, key, pointer)?;
        let value = node
            .integer()
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0);
        if value.is_none() {
            self.push(code, node.span, pointer, None);
        }
        value
    }

    fn i32(&mut self, mapping: &[MappingEntry], key: &str, pointer: &str) -> Option<i32> {
        let node = self.required(mapping, key, pointer)?;
        let value = node.integer().and_then(|value| i32::try_from(value).ok());
        if value.is_none() {
            self.push(
                WorkflowDiagnosticCode::LayoutLimitExceeded,
                node.span,
                pointer,
                None,
            );
        }
        value
    }

    fn positive_u32(&mut self, mapping: &[MappingEntry], key: &str, pointer: &str) -> Option<u32> {
        let node = self.required(mapping, key, pointer)?;
        let value = node
            .integer()
            .and_then(|value| i32::try_from(value).ok())
            .filter(|value| *value > 0)
            .and_then(|value| u32::try_from(value).ok());
        if value.is_none() {
            self.push(
                WorkflowDiagnosticCode::LayoutLimitExceeded,
                node.span,
                pointer,
                None,
            );
        }
        value
    }

    fn push(
        &mut self,
        code: WorkflowDiagnosticCode,
        span: SourceSpan,
        pointer: &str,
        related_id: Option<StableId>,
    ) {
        self.diagnostics.push(make_diagnostic(
            self.source_path,
            self.source,
            code,
            span,
            pointer,
            related_id,
        ));
    }
}

fn validate_graph_collection(
    graphs: &[DecodedGraph<'_>],
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) {
    let mut identities = BTreeMap::<StableId, (&str, SourceSpan)>::new();
    let mut workflow_names = BTreeSet::new();
    let workflow_ids = graphs
        .iter()
        .map(|graph| graph.document.workflow_id)
        .collect::<BTreeSet<_>>();
    for graph in graphs {
        let document = &graph.document;
        register_project_identity(
            &mut identities,
            document.document_id,
            &document.source_path,
            document.span,
            "/documentId",
            graph.source,
            diagnostics,
        );
        register_project_identity(
            &mut identities,
            document.workflow_id,
            &document.source_path,
            document.span,
            "/workflowId",
            graph.source,
            diagnostics,
        );
        if !workflow_names.insert(document.canonical_name.as_str()) {
            diagnostics.push(make_diagnostic(
                &document.source_path,
                graph.source,
                WorkflowDiagnosticCode::InvalidCanonicalName,
                document.span,
                "/canonicalName",
                Some(document.workflow_id),
            ));
        }
        for (index, node) in document.nodes.iter().enumerate() {
            register_project_identity(
                &mut identities,
                node.node_id,
                &document.source_path,
                node.span,
                &format!("/nodes/{index}/nodeId"),
                graph.source,
                diagnostics,
            );
            if let NodeKind::Subworkflow { target_workflow_id } = node.kind
                && !workflow_ids.contains(&target_workflow_id)
            {
                diagnostics.push(make_diagnostic(
                    &document.source_path,
                    graph.source,
                    WorkflowDiagnosticCode::MissingSubworkflow,
                    node.span,
                    &format!("/nodes/{index}/targetWorkflowId"),
                    Some(node.node_id),
                ));
            }
        }
        for (index, edge) in document.edges.iter().enumerate() {
            register_project_identity(
                &mut identities,
                edge.edge_id,
                &document.source_path,
                edge.span,
                &format!("/edges/{index}/edgeId"),
                graph.source,
                diagnostics,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn register_project_identity<'a>(
    identities: &mut BTreeMap<StableId, (&'a str, SourceSpan)>,
    identity: StableId,
    source_path: &'a str,
    span: SourceSpan,
    pointer: &str,
    source: &str,
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) {
    if identities.insert(identity, (source_path, span)).is_some() {
        diagnostics.push(make_diagnostic(
            source_path,
            source,
            WorkflowDiagnosticCode::DuplicateStableIdentity,
            span,
            pointer,
            Some(identity),
        ));
    }
}

fn validate_layout_references(
    layout: &WorkflowLayoutDocument,
    source: &str,
    graphs: &[DecodedGraph<'_>],
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) {
    let Some(graph) = graphs
        .iter()
        .find(|graph| graph.document.workflow_id == layout.workflow_id)
    else {
        diagnostics.push(make_diagnostic(
            &layout.source_path,
            source,
            WorkflowDiagnosticCode::InvalidLayoutReference,
            layout.span,
            "/workflowId",
            Some(layout.workflow_id),
        ));
        return;
    };
    let node_ids = graph
        .document
        .nodes
        .iter()
        .map(|node| node.node_id)
        .collect::<BTreeSet<_>>();
    let edge_ids = graph
        .document
        .edges
        .iter()
        .map(|edge| edge.edge_id)
        .collect::<BTreeSet<_>>();
    for (index, node) in layout.nodes.iter().enumerate() {
        if !node_ids.contains(&node.node_id) {
            diagnostics.push(make_diagnostic(
                &layout.source_path,
                source,
                WorkflowDiagnosticCode::InvalidLayoutReference,
                node.span,
                &format!("/nodes/{index}/nodeId"),
                Some(node.node_id),
            ));
        }
    }
    for (index, edge) in layout.edges.iter().enumerate() {
        if !edge_ids.contains(&edge.edge_id) {
            diagnostics.push(make_diagnostic(
                &layout.source_path,
                source,
                WorkflowDiagnosticCode::InvalidLayoutReference,
                edge.span,
                &format!("/edges/{index}/edgeId"),
                Some(edge.edge_id),
            ));
        }
    }
    for (index, group) in layout.groups.iter().enumerate() {
        if group
            .node_ids
            .iter()
            .any(|node_id| !node_ids.contains(node_id))
        {
            diagnostics.push(make_diagnostic(
                &layout.source_path,
                source,
                WorkflowDiagnosticCode::InvalidLayoutReference,
                group.span,
                &format!("/groups/{index}/nodeIds"),
                Some(group.group_id),
            ));
        }
    }
}

fn validate_layout_identity_isolation(
    graphs: &[DecodedGraph<'_>],
    layouts: &mut Vec<WorkflowLayoutDocument>,
    sources: &[LayoutSource<'_>],
    diagnostics: &mut Vec<WorkflowDiagnostic>,
) {
    let mut identities = BTreeSet::new();
    for graph in graphs {
        identities.insert(graph.document.document_id);
        identities.insert(graph.document.workflow_id);
        identities.extend(graph.document.nodes.iter().map(|node| node.node_id));
        identities.extend(graph.document.edges.iter().map(|edge| edge.edge_id));
    }
    let mut accepted = Vec::with_capacity(layouts.len());
    for layout in layouts.drain(..) {
        let source = sources
            .iter()
            .find(|source| source.source_path == layout.source_path)
            .and_then(|source| str::from_utf8(source.source_bytes).ok())
            .unwrap_or("");
        let mut valid = true;
        if !identities.insert(layout.document_id) {
            diagnostics.push(make_diagnostic(
                &layout.source_path,
                source,
                WorkflowDiagnosticCode::DuplicateStableIdentity,
                layout.span,
                "/documentId",
                Some(layout.document_id),
            ));
            valid = false;
        }
        for (index, group) in layout.groups.iter().enumerate() {
            if !identities.insert(group.group_id) {
                diagnostics.push(make_diagnostic(
                    &layout.source_path,
                    source,
                    WorkflowDiagnosticCode::DuplicateStableIdentity,
                    group.span,
                    &format!("/groups/{index}/groupId"),
                    Some(group.group_id),
                ));
                valid = false;
            }
        }
        if valid {
            accepted.push(layout);
        }
    }
    *layouts = accepted;
}

fn field<'a>(mapping: &'a [MappingEntry], key: &str) -> Option<&'a YamlNode> {
    mapping
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| &entry.value)
}

fn has_field(mapping: &[MappingEntry], key: &str) -> bool {
    field(mapping, key).is_some()
}

fn has_any_field(mapping: &[MappingEntry], keys: &[&str]) -> bool {
    keys.iter().any(|key| has_field(mapping, key))
}

fn node_allowed_fields(kind: &str) -> Option<&'static [&'static str]> {
    const STRUCTURAL: &[&str] = &["nodeId", "canonicalName", "kind"];
    const EXECUTABLE: &[&str] = &[
        "nodeId",
        "canonicalName",
        "kind",
        "executionOrder",
        "cancellationBoundary",
    ];
    const JOIN: &[&str] = &[
        "nodeId",
        "canonicalName",
        "kind",
        "executionOrder",
        "cancellationBoundary",
        "mode",
        "forkId",
        "loserPolicy",
    ];
    const WAIT: &[&str] = &[
        "nodeId",
        "canonicalName",
        "kind",
        "executionOrder",
        "cancellationBoundary",
        "mode",
        "waitCycles",
        "conditionId",
        "timeoutCycles",
        "permanent",
    ];
    const SUBWORKFLOW: &[&str] = &[
        "nodeId",
        "canonicalName",
        "kind",
        "executionOrder",
        "cancellationBoundary",
        "targetWorkflowId",
    ];
    match kind {
        "Entry" | "End" => Some(STRUCTURAL),
        "Action" | "Decision" | "Fork" => Some(EXECUTABLE),
        "Join" => Some(JOIN),
        "Wait" => Some(WAIT),
        "Subworkflow" => Some(SUBWORKFLOW),
        _ => None,
    }
}

const fn edge_priority(edge: &Edge) -> Option<u32> {
    edge.priority
}

const fn edge_branch_order(edge: &Edge) -> Option<u32> {
    edge.branch_order
}

fn field_span(mapping: &[MappingEntry], key: &str, fallback: SourceSpan) -> SourceSpan {
    field(mapping, key).map_or(fallback, |node| node.span)
}

fn mapping_span(mapping: &[MappingEntry]) -> SourceSpan {
    let Some(first) = mapping.first() else {
        return SourceSpan::empty();
    };
    let end = mapping
        .last()
        .map_or(first.value.span.end, |entry| entry.value.span.end);
    SourceSpan {
        start: first.key_span.start,
        end,
    }
}

fn canonical_name_is_valid(value: &str) -> bool {
    if value.is_empty() || value.len() > 256 {
        return false;
    }
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    matches!(first, b'a'..=b'z' | b'_')
        && bytes.all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}
