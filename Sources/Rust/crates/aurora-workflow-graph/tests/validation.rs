//! R2-01 golden Graph/Layout acceptance and exact diagnostic-cardinality tests.

use std::fs;
use std::path::Path;
use std::str;

use aurora_workflow_graph::{
    LayoutSource, WorkflowDiagnosticCode, WorkflowProjectInput, WorkflowSource,
    WorkflowValidationLimits, YamlSourceLimits, diagnostics_to_canonical_json, validate_project,
};

const MINIMAL: &[u8] =
    include_bytes!("../../../../Contracts/workflow/v1/examples/minimal.valid.aurora-workflow.yaml");
const LAYOUT: &[u8] = include_bytes!(
    "../../../../Contracts/workflow/v1/examples/minimal.valid.aurora-workflow-layout.yaml"
);
const CHILD: &[u8] =
    include_bytes!("../../../../Contracts/workflow/v1/examples/child.valid.aurora-workflow.yaml");
const PARENT: &[u8] =
    include_bytes!("../../../../Contracts/workflow/v1/examples/parent.valid.aurora-workflow.yaml");
const DECISION_MERGE: &[u8] = include_bytes!(
    "../../../../Contracts/workflow/v1/examples/decision-merge.valid.aurora-workflow.yaml"
);
const PARALLEL_WAIT: &[u8] = include_bytes!(
    "../../../../Contracts/workflow/v1/examples/parallel-wait.valid.aurora-workflow.yaml"
);
const JOIN_ANY: &[u8] = include_bytes!(
    "../../../../Contracts/workflow/v1/examples/join-any.valid.aurora-workflow.yaml"
);

fn limits(
    graph_source_bytes: usize,
    max_nodes: usize,
    layout_points: usize,
) -> Result<WorkflowValidationLimits, aurora_workflow_graph::WorkflowLimitError> {
    let graph_yaml = YamlSourceLimits::new(graph_source_bytes, 32, 32, 256, 65_536)?;
    let layout_yaml = YamlSourceLimits::new(65_536, 32, 32, 256, 65_536)?;
    WorkflowValidationLimits::new(
        graph_yaml,
        layout_yaml,
        8,
        max_nodes,
        64,
        8,
        64,
        64,
        16,
        layout_points,
    )
}

fn normal_limits() -> Result<WorkflowValidationLimits, aurora_workflow_graph::WorkflowLimitError> {
    limits(65_536, 64, 128)
}

fn validate_one_graph(bytes: &[u8]) -> Option<aurora_workflow_graph::WorkflowValidationOutput> {
    let limits = normal_limits().ok()?;
    Some(validate_project(
        WorkflowProjectInput {
            workflows: &[WorkflowSource {
                source_path: "graph.aurora-workflow.yaml",
                source_bytes: bytes,
            }],
            layouts: &[],
        },
        limits,
    ))
}

#[test]
fn valid_graph_layout_and_subworkflow_closure_publish_exact_models() {
    let result = normal_limits();
    assert!(result.is_ok(), "valid test limits must construct");
    let Ok(limits) = result else {
        return;
    };
    let workflows = [
        WorkflowSource {
            source_path: "workflows/parent.aurora-workflow.yaml",
            source_bytes: PARENT,
        },
        WorkflowSource {
            source_path: "workflows/child.aurora-workflow.yaml",
            source_bytes: CHILD,
        },
        WorkflowSource {
            source_path: "workflows/minimal.aurora-workflow.yaml",
            source_bytes: MINIMAL,
        },
        WorkflowSource {
            source_path: "workflows/decision-merge.aurora-workflow.yaml",
            source_bytes: DECISION_MERGE,
        },
        WorkflowSource {
            source_path: "workflows/parallel-wait.aurora-workflow.yaml",
            source_bytes: PARALLEL_WAIT,
        },
        WorkflowSource {
            source_path: "workflows/join-any.aurora-workflow.yaml",
            source_bytes: JOIN_ANY,
        },
    ];
    let output = validate_project(
        WorkflowProjectInput {
            workflows: &workflows,
            layouts: &[LayoutSource {
                source_path: "layouts/minimal.aurora-workflow-layout.yaml",
                source_bytes: LAYOUT,
            }],
        },
        limits,
    );
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    assert_eq!(output.workflows.as_ref().map(Vec::len), Some(6));
    assert_eq!(output.layouts.len(), 1);
}

#[test]
fn negative_graph_goldens_emit_one_exact_primary() {
    let cases: &[(&[u8], WorkflowDiagnosticCode)] = &[
        (
            include_bytes!(
                "../../../../Contracts/workflow/v1/examples/unknown-node.invalid-WF0009.aurora-workflow.yaml"
            ),
            WorkflowDiagnosticCode::InvalidField,
        ),
        (
            include_bytes!(
                "../../../../Contracts/workflow/v1/examples/unknown-field.invalid-WF0008.aurora-workflow.yaml"
            ),
            WorkflowDiagnosticCode::UnknownField,
        ),
        (
            include_bytes!(
                "../../../../Contracts/workflow/v1/examples/version.invalid-WF0002.aurora-workflow.yaml"
            ),
            WorkflowDiagnosticCode::UnsupportedSchemaVersion,
        ),
        (
            include_bytes!(
                "../../../../Contracts/workflow/v1/examples/dangling-edge.invalid-WF1005.aurora-workflow.yaml"
            ),
            WorkflowDiagnosticCode::DanglingEdge,
        ),
        (
            include_bytes!(
                "../../../../Contracts/workflow/v1/examples/join-property.invalid-WF2004.aurora-workflow.yaml"
            ),
            WorkflowDiagnosticCode::InvalidForkJoinPair,
        ),
        (
            include_bytes!(
                "../../../../Contracts/workflow/v1/examples/edge-property.invalid-WF2002.aurora-workflow.yaml"
            ),
            WorkflowDiagnosticCode::InvalidDecisionPriority,
        ),
        (
            include_bytes!(
                "../../../../Contracts/workflow/v1/examples/duplicate-key.invalid-WF0004.aurora-workflow.yaml"
            ),
            WorkflowDiagnosticCode::DuplicateMappingKey,
        ),
        (
            include_bytes!(
                "../../../../Contracts/workflow/v1/examples/alias-cycle.invalid-WF0006.aurora-workflow.yaml"
            ),
            WorkflowDiagnosticCode::AliasCycle,
        ),
    ];
    for (source, expected) in cases {
        let output = validate_one_graph(source);
        assert!(output.is_some(), "valid test limits must construct");
        let Some(output) = output else {
            return;
        };
        assert!(output.workflows.is_none());
        assert_eq!(output.diagnostics.len(), 1, "{:?}", output.diagnostics);
        assert_eq!(output.diagnostics[0].code, *expected);
    }
}

#[test]
fn layout_errors_do_not_suppress_a_valid_graph() {
    let cases: &[(&[u8], WorkflowDiagnosticCode)] = &[
        (
            include_bytes!(
                "../../../../Contracts/workflow/v1/examples/dangling.invalid-WF3010.aurora-workflow-layout.yaml"
            ),
            WorkflowDiagnosticCode::InvalidLayoutReference,
        ),
        (
            include_bytes!(
                "../../../../Contracts/workflow/v1/examples/unknown-field.invalid-WF0008.aurora-workflow-layout.yaml"
            ),
            WorkflowDiagnosticCode::UnknownField,
        ),
    ];
    let result = normal_limits();
    assert!(result.is_ok(), "valid test limits must construct");
    let Ok(limits) = result else {
        return;
    };
    for (layout, expected) in cases {
        let output = validate_project(
            WorkflowProjectInput {
                workflows: &[WorkflowSource {
                    source_path: "minimal.aurora-workflow.yaml",
                    source_bytes: MINIMAL,
                }],
                layouts: &[LayoutSource {
                    source_path: "layout.aurora-workflow-layout.yaml",
                    source_bytes: layout,
                }],
            },
            limits,
        );
        assert_eq!(output.workflows.as_ref().map(Vec::len), Some(1));
        assert!(output.layouts.is_empty());
        assert_eq!(output.diagnostics.len(), 1, "{:?}", output.diagnostics);
        assert_eq!(output.diagnostics[0].code, *expected);
    }
}

#[test]
fn layout_collection_limit_does_not_suppress_a_valid_graph() {
    let graph_yaml = YamlSourceLimits::new(65_536, 32, 32, 256, 65_536);
    let layout_yaml = YamlSourceLimits::new(65_536, 32, 32, 256, 65_536);
    assert!(graph_yaml.is_ok() && layout_yaml.is_ok());
    let (Ok(graph_yaml), Ok(layout_yaml)) = (graph_yaml, layout_yaml) else {
        return;
    };
    let result =
        WorkflowValidationLimits::new(graph_yaml, layout_yaml, 8, 64, 64, 1, 64, 64, 16, 128);
    assert!(result.is_ok(), "valid test limits must construct");
    let Ok(limits) = result else {
        return;
    };
    let layouts = [
        LayoutSource {
            source_path: "a.layout.yaml",
            source_bytes: LAYOUT,
        },
        LayoutSource {
            source_path: "b.layout.yaml",
            source_bytes: LAYOUT,
        },
    ];
    let output = validate_project(
        WorkflowProjectInput {
            workflows: &[WorkflowSource {
                source_path: "graph.yaml",
                source_bytes: MINIMAL,
            }],
            layouts: &layouts,
        },
        limits,
    );
    assert_eq!(output.workflows.as_ref().map(Vec::len), Some(1));
    assert!(output.layouts.is_empty());
    assert_eq!(output.diagnostics.len(), 1);
    assert_eq!(
        output.diagnostics[0].code,
        WorkflowDiagnosticCode::LayoutLimitExceeded
    );
}

#[test]
fn decision_order_and_property_errors_have_exact_cardinality() {
    let source = str::from_utf8(DECISION_MERGE);
    assert!(source.is_ok());
    let Ok(source) = source else {
        return;
    };
    let cases = [
        (
            source.replace("priority: 1", "priority: 0"),
            WorkflowDiagnosticCode::InvalidDecisionPriority,
        ),
        (
            source.replace("priority: 1", "priority: 2"),
            WorkflowDiagnosticCode::InvalidDecisionPriority,
        ),
        (
            source.replace(", priority: 1", ""),
            WorkflowDiagnosticCode::InvalidDecisionPriority,
        ),
    ];
    for (source, expected) in cases {
        let output = validate_one_graph(source.as_bytes());
        assert!(output.is_some(), "valid test limits must construct");
        let Some(output) = output else {
            return;
        };
        assert!(output.workflows.is_none());
        assert_eq!(output.diagnostics.len(), 1, "{:?}", output.diagnostics);
        assert_eq!(output.diagnostics[0].code, expected);
    }
}

#[test]
fn exact_capacity_passes_and_first_item_above_fails_once() {
    let result = limits(MINIMAL.len(), 3, 1);
    assert!(result.is_ok(), "valid exact limits must construct");
    let Ok(exact) = result else {
        return;
    };
    let accepted = validate_project(
        WorkflowProjectInput {
            workflows: &[WorkflowSource {
                source_path: "minimal.aurora-workflow.yaml",
                source_bytes: MINIMAL,
            }],
            layouts: &[LayoutSource {
                source_path: "minimal.aurora-workflow-layout.yaml",
                source_bytes: LAYOUT,
            }],
        },
        exact,
    );
    assert!(
        accepted.diagnostics.is_empty(),
        "{:?}",
        accepted.diagnostics
    );

    let result = limits(MINIMAL.len() - 1, 3, 1);
    assert!(result.is_ok(), "valid short limit must construct");
    let Ok(one_byte_short) = result else {
        return;
    };
    let rejected = validate_project(
        WorkflowProjectInput {
            workflows: &[WorkflowSource {
                source_path: "minimal.aurora-workflow.yaml",
                source_bytes: MINIMAL,
            }],
            layouts: &[],
        },
        one_byte_short,
    );
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(
        rejected.diagnostics[0].code,
        WorkflowDiagnosticCode::SourceLimitExceeded
    );

    let result = limits(MINIMAL.len(), 2, 1);
    assert!(result.is_ok(), "valid node limit must construct");
    let Ok(two_nodes) = result else {
        return;
    };
    let rejected = validate_project(
        WorkflowProjectInput {
            workflows: &[WorkflowSource {
                source_path: "minimal.aurora-workflow.yaml",
                source_bytes: MINIMAL,
            }],
            layouts: &[],
        },
        two_nodes,
    );
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(
        rejected.diagnostics[0].code,
        WorkflowDiagnosticCode::ResourceBudgetExceeded
    );
}

#[test]
fn source_order_does_not_change_models_or_diagnostic_json() {
    let result = normal_limits();
    assert!(result.is_ok(), "valid test limits must construct");
    let Ok(limits) = result else {
        return;
    };
    let left = validate_project(
        WorkflowProjectInput {
            workflows: &[
                WorkflowSource {
                    source_path: "parent.yaml",
                    source_bytes: PARENT,
                },
                WorkflowSource {
                    source_path: "child.yaml",
                    source_bytes: CHILD,
                },
            ],
            layouts: &[],
        },
        limits,
    );
    let right = validate_project(
        WorkflowProjectInput {
            workflows: &[
                WorkflowSource {
                    source_path: "child.yaml",
                    source_bytes: CHILD,
                },
                WorkflowSource {
                    source_path: "parent.yaml",
                    source_bytes: PARENT,
                },
            ],
            layouts: &[],
        },
        limits,
    );
    assert_eq!(left, right);
    let left_json = diagnostics_to_canonical_json(&left.diagnostics);
    let right_json = diagnostics_to_canonical_json(&right.diagnostics);
    assert!(left_json.is_ok());
    assert!(right_json.is_ok());
    if let (Ok(left_json), Ok(right_json)) = (left_json, right_json) {
        assert_eq!(left_json, right_json);
    }
}

#[test]
fn fixture_inventory_is_exact_and_reviewable() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../Contracts/workflow/v1/examples");
    let result = fs::read_dir(root);
    assert!(result.is_ok(), "fixture directory must be readable");
    let Ok(entries) = result else {
        return;
    };
    let mut actual = Vec::new();
    for entry in entries {
        assert!(entry.is_ok(), "every fixture entry must be readable");
        let Ok(entry) = entry else {
            return;
        };
        let file_name = entry.file_name();
        let name = file_name.to_str().map(str::to_owned);
        assert!(name.is_some(), "fixture names must be UTF-8");
        let Some(name) = name else {
            return;
        };
        actual.push(name);
    }
    actual.sort();
    let mut expected = vec![
        "alias-cycle.invalid-WF0006.aurora-workflow.yaml",
        "child.valid.aurora-workflow.yaml",
        "dangling-edge.invalid-WF1005.aurora-workflow.yaml",
        "dangling.invalid-WF3010.aurora-workflow-layout.yaml",
        "decision-merge.valid.aurora-workflow.yaml",
        "duplicate-key.invalid-WF0004.aurora-workflow.yaml",
        "edge-property.invalid-WF2002.aurora-workflow.yaml",
        "join-property.invalid-WF2004.aurora-workflow.yaml",
        "join-any.valid.aurora-workflow.yaml",
        "minimal.valid.aurora-workflow-layout.yaml",
        "minimal.valid.aurora-workflow.yaml",
        "parent.valid.aurora-workflow.yaml",
        "parallel-wait.valid.aurora-workflow.yaml",
        "unknown-field.invalid-WF0008.aurora-workflow-layout.yaml",
        "unknown-field.invalid-WF0008.aurora-workflow.yaml",
        "unknown-node.invalid-WF0009.aurora-workflow.yaml",
        "version.invalid-WF0002.aurora-workflow.yaml",
    ];
    expected.sort_unstable();
    assert_eq!(actual, expected);
}
