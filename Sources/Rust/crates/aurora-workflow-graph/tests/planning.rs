//! R2-02 static-plan, exact expansion, topology, ownership, and resource-bound tests.

use aurora_workflow_graph::{
    ExpandedNodeResourceInput, StableId, TaskWorkflowPlanningInput, WorkflowArtifactLimits,
    WorkflowDiagnosticCode, WorkflowPlanInputError, WorkflowSource, WorkflowTargetLimitValues,
    WorkflowTargetLimits, WorkflowValidationLimits, WorkflowWriteRegion, YamlSourceLimits,
    compile_static_workflow_plan,
};

const MINIMAL: &[u8] =
    include_bytes!("../../../../Contracts/workflow/v1/examples/minimal.valid.aurora-workflow.yaml");
const CHILD: &[u8] =
    include_bytes!("../../../../Contracts/workflow/v1/examples/child.valid.aurora-workflow.yaml");
const PARENT: &[u8] =
    include_bytes!("../../../../Contracts/workflow/v1/examples/parent.valid.aurora-workflow.yaml");
const TWO_CALL_PARENT: &[u8] = br"kind: aurora.cyclic-workflow
schemaVersion: { major: 1, minor: 0, lifecycle: preview }
documentId: 018f0000-0000-7000-8000-000000000201
workflowId: 018f0000-0000-7000-8000-000000000202
canonicalName: two_call_parent
permanent: false
nodes:
  - { nodeId: 018f0000-0000-7000-8000-000000000203, canonicalName: entry, kind: Entry }
  - { nodeId: 018f0000-0000-7000-8000-000000000204, canonicalName: first_call, kind: Subworkflow, executionOrder: 0, cancellationBoundary: false, targetWorkflowId: 018f0000-0000-7000-8000-000000000011 }
  - { nodeId: 018f0000-0000-7000-8000-000000000205, canonicalName: second_call, kind: Subworkflow, executionOrder: 1, cancellationBoundary: false, targetWorkflowId: 018f0000-0000-7000-8000-000000000011 }
  - { nodeId: 018f0000-0000-7000-8000-000000000206, canonicalName: end, kind: End }
edges:
  - { edgeId: 018f0000-0000-7000-8000-000000000207, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000203, targetNodeId: 018f0000-0000-7000-8000-000000000204, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000208, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000204, targetNodeId: 018f0000-0000-7000-8000-000000000205, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000209, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000205, targetNodeId: 018f0000-0000-7000-8000-000000000206, backedge: false }
";
const BOUNDED_BACKEDGE: &[u8] = br"kind: aurora.cyclic-workflow
schemaVersion: { major: 1, minor: 0, lifecycle: preview }
documentId: 018f0000-0000-7000-8000-000000000211
workflowId: 018f0000-0000-7000-8000-000000000212
canonicalName: bounded_backedge
permanent: false
nodes:
  - { nodeId: 018f0000-0000-7000-8000-000000000213, canonicalName: entry, kind: Entry }
  - { nodeId: 018f0000-0000-7000-8000-000000000214, canonicalName: loop_join, kind: Join, executionOrder: 0, cancellationBoundary: false, mode: merge }
  - { nodeId: 018f0000-0000-7000-8000-000000000216, canonicalName: choose, kind: Decision, executionOrder: 1, cancellationBoundary: false }
  - { nodeId: 018f0000-0000-7000-8000-000000000215, canonicalName: step, kind: Action, executionOrder: 2, cancellationBoundary: false }
  - { nodeId: 018f0000-0000-7000-8000-000000000221, canonicalName: end, kind: End }
edges:
  - { edgeId: 018f0000-0000-7000-8000-000000000217, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000213, targetNodeId: 018f0000-0000-7000-8000-000000000214, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000218, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000214, targetNodeId: 018f0000-0000-7000-8000-000000000216, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000219, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000216, targetNodeId: 018f0000-0000-7000-8000-000000000215, conditionId: 018f0000-0000-7000-8000-000000000222, priority: 0, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000220, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000216, targetNodeId: 018f0000-0000-7000-8000-000000000214, conditionId: 018f0000-0000-7000-8000-000000000223, priority: 1, backedge: true, maxTraversalsPerRun: 3 }
  - { edgeId: 018f0000-0000-7000-8000-000000000224, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000215, targetNodeId: 018f0000-0000-7000-8000-000000000221, backedge: false }
";
const RECURSIVE_WORKFLOW: &[u8] = br"kind: aurora.cyclic-workflow
schemaVersion: { major: 1, minor: 0, lifecycle: preview }
documentId: 018f0000-0000-7000-8000-000000000301
workflowId: 018f0000-0000-7000-8000-000000000302
canonicalName: recursive_workflow
permanent: false
nodes:
  - { nodeId: 018f0000-0000-7000-8000-000000000303, canonicalName: entry, kind: Entry }
  - { nodeId: 018f0000-0000-7000-8000-000000000304, canonicalName: recurse, kind: Subworkflow, executionOrder: 0, cancellationBoundary: false, targetWorkflowId: 018f0000-0000-7000-8000-000000000302 }
  - { nodeId: 018f0000-0000-7000-8000-000000000305, canonicalName: end, kind: End }
edges:
  - { edgeId: 018f0000-0000-7000-8000-000000000306, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000303, targetNodeId: 018f0000-0000-7000-8000-000000000304, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000307, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000304, targetNodeId: 018f0000-0000-7000-8000-000000000305, backedge: false }
";
const REVERSED_FORWARD_ORDER: &[u8] = br"kind: aurora.cyclic-workflow
schemaVersion: { major: 1, minor: 0, lifecycle: preview }
documentId: 018f0000-0000-7000-8000-000000000311
workflowId: 018f0000-0000-7000-8000-000000000312
canonicalName: reversed_forward_order
permanent: false
nodes:
  - { nodeId: 018f0000-0000-7000-8000-000000000313, canonicalName: entry, kind: Entry }
  - { nodeId: 018f0000-0000-7000-8000-000000000314, canonicalName: late, kind: Action, executionOrder: 1, cancellationBoundary: false }
  - { nodeId: 018f0000-0000-7000-8000-000000000315, canonicalName: early, kind: Action, executionOrder: 0, cancellationBoundary: false }
  - { nodeId: 018f0000-0000-7000-8000-000000000316, canonicalName: end, kind: End }
edges:
  - { edgeId: 018f0000-0000-7000-8000-000000000317, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000313, targetNodeId: 018f0000-0000-7000-8000-000000000314, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000318, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000314, targetNodeId: 018f0000-0000-7000-8000-000000000315, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000319, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000315, targetNodeId: 018f0000-0000-7000-8000-000000000316, backedge: false }
";
const UNREACHABLE_CYCLE: &[u8] = br"kind: aurora.cyclic-workflow
schemaVersion: { major: 1, minor: 0, lifecycle: preview }
documentId: 018f0000-0000-7000-8000-000000000321
workflowId: 018f0000-0000-7000-8000-000000000322
canonicalName: unreachable_cycle
permanent: false
nodes:
  - { nodeId: 018f0000-0000-7000-8000-000000000323, canonicalName: entry, kind: Entry }
  - { nodeId: 018f0000-0000-7000-8000-000000000324, canonicalName: main, kind: Action, executionOrder: 0, cancellationBoundary: false }
  - { nodeId: 018f0000-0000-7000-8000-000000000325, canonicalName: end, kind: End }
  - { nodeId: 018f0000-0000-7000-8000-000000000326, canonicalName: orphan_a, kind: Action, executionOrder: 1, cancellationBoundary: false }
  - { nodeId: 018f0000-0000-7000-8000-000000000327, canonicalName: orphan_b, kind: Action, executionOrder: 2, cancellationBoundary: false }
edges:
  - { edgeId: 018f0000-0000-7000-8000-000000000328, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000323, targetNodeId: 018f0000-0000-7000-8000-000000000324, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000329, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000324, targetNodeId: 018f0000-0000-7000-8000-000000000325, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000330, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000326, targetNodeId: 018f0000-0000-7000-8000-000000000327, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000331, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000327, targetNodeId: 018f0000-0000-7000-8000-000000000326, backedge: false }
";

type LimitZeroer = fn(&mut WorkflowTargetLimitValues);

fn id(value: &str) -> StableId {
    StableId::parse(value).unwrap_or_else(|| unreachable!("test UUID is canonical v7"))
}

fn validation_limits() -> WorkflowValidationLimits {
    let yaml = YamlSourceLimits::new(128 * 1024, 32, 32, 512, 128 * 1024)
        .unwrap_or_else(|error| unreachable!("test YAML limits are valid: {error}"));
    WorkflowValidationLimits::new(yaml, yaml, 16, 128, 256, 16, 128, 256, 32, 512)
        .unwrap_or_else(|error| unreachable!("test validation limits are valid: {error}"))
}

fn target_values() -> WorkflowTargetLimitValues {
    WorkflowTargetLimitValues {
        max_workflows_per_task: 8,
        max_source_nodes_per_workflow: 128,
        max_source_edges_per_workflow: 256,
        max_expanded_workflow_instances: 32,
        max_expanded_nodes_per_task: 1024,
        max_expanded_edges_per_task: 2048,
        max_active_nodes_per_task: 512,
        max_node_executions_per_release: 512,
        max_fork_nesting_depth: 64,
        max_branches_per_fork: 64,
        max_pending_cancellations: 64,
        max_subworkflow_expansion_depth: 16,
        max_backedge_traversals_per_run: 1024,
        max_wait_cycles: 1024,
        max_workflow_state_bytes_per_task: 1024 * 1024,
        max_workflow_staging_bytes_per_task: 1024 * 1024,
        max_watch_handles_per_task: 128,
        max_trace_events_per_release: 4096,
        workflow_trace_ring_capacity: 4096,
    }
}

fn target_limits(values: WorkflowTargetLimitValues) -> WorkflowTargetLimits {
    WorkflowTargetLimits::new(values)
        .unwrap_or_else(|error| unreachable!("test Target limits are valid: {error}"))
}

fn artifact_limits() -> WorkflowArtifactLimits {
    WorkflowArtifactLimits::new(4 * 1024 * 1024, 4 * 1024 * 1024)
        .unwrap_or_else(|error| unreachable!("test artifact limits are valid: {error}"))
}

fn task(task_handle: u32, root: StableId) -> TaskWorkflowPlanningInput {
    TaskWorkflowPlanningInput {
        task_handle,
        root_workflow_ids: vec![root],
        watches: Vec::new(),
        trace_ring_capacity: 64,
    }
}

fn claim(task_handle: u32, path: Vec<StableId>, node_id: StableId) -> ExpandedNodeResourceInput {
    ExpandedNodeResourceInput {
        task_handle,
        instance_path: path,
        node_id,
        committed_state_bytes: 0,
        staging_state_bytes: 0,
        trace_events_per_release: 0,
        writes: Vec::new(),
    }
}

#[test]
fn one_call_site_generates_each_instance_node_edge_and_step_exactly_once() {
    let parent = id("018f0000-0000-7000-8000-000000000021");
    let call = id("018f0000-0000-7000-8000-000000000023");
    let child_action = id("018f0000-0000-7000-8000-000000000013");
    let sources = [
        WorkflowSource {
            source_path: "workflows/parent.aurora-workflow.yaml",
            source_bytes: PARENT,
        },
        WorkflowSource {
            source_path: "workflows/child.aurora-workflow.yaml",
            source_bytes: CHILD,
        },
    ];
    let tasks = [task(7, parent)];
    let claims = [
        claim(7, vec![parent], call),
        claim(7, vec![parent, call], child_action),
    ];
    let output = compile_static_workflow_plan(
        &sources,
        validation_limits(),
        &tasks,
        &claims,
        target_limits(target_values()),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("accepted inputs are consistent: {error}"));
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let artifacts = output
        .artifacts
        .unwrap_or_else(|| unreachable!("accepted plan publishes atomically"));
    assert_eq!(artifacts.canonical_ir.workflows.len(), 2);
    assert_eq!(artifacts.static_plan.instances.len(), 2);
    assert_eq!(artifacts.static_plan.steps.len(), 2);
    assert_eq!(artifacts.static_plan.edges.len(), 4);
    assert_eq!(
        artifacts
            .static_plan
            .steps
            .iter()
            .filter(|step| step.child_instance.is_some())
            .count(),
        1
    );
    let proof = &artifacts.static_plan.resources.tasks[0];
    assert_eq!(proof.root_workflows, 1);
    assert_eq!(proof.expanded_workflow_instances, 2);
    assert_eq!(proof.expanded_nodes, 6);
    assert_eq!(proof.expanded_edges, 4);
}

#[test]
fn two_call_sites_expand_two_isolated_children_without_inferred_roots() {
    let parent = id("018f0000-0000-7000-8000-000000000202");
    let first_call = id("018f0000-0000-7000-8000-000000000204");
    let second_call = id("018f0000-0000-7000-8000-000000000205");
    let child_action = id("018f0000-0000-7000-8000-000000000013");
    let sources = [
        WorkflowSource {
            source_path: "workflows/two-call-parent.aurora-workflow.yaml",
            source_bytes: TWO_CALL_PARENT,
        },
        WorkflowSource {
            source_path: "workflows/child.aurora-workflow.yaml",
            source_bytes: CHILD,
        },
        WorkflowSource {
            source_path: "workflows/unreferenced.aurora-workflow.yaml",
            source_bytes: MINIMAL,
        },
    ];
    let tasks = [task(7, parent)];
    let claims = [
        claim(7, vec![parent], first_call),
        claim(7, vec![parent, first_call], child_action),
        claim(7, vec![parent], second_call),
        claim(7, vec![parent, second_call], child_action),
    ];
    let artifacts = compile_static_workflow_plan(
        &sources,
        validation_limits(),
        &tasks,
        &claims,
        target_limits(target_values()),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("accepted inputs are consistent: {error}"))
    .artifacts
    .unwrap_or_else(|| unreachable!("accepted plan publishes atomically"));
    assert_eq!(artifacts.canonical_ir.workflows.len(), 2);
    assert_eq!(artifacts.source_digests.len(), 2);
    assert_eq!(artifacts.static_plan.instances.len(), 3);
    assert_eq!(artifacts.static_plan.steps.len(), 4);
    assert_eq!(artifacts.static_plan.edges.len(), 7);
    assert_eq!(
        artifacts
            .static_plan
            .steps
            .iter()
            .filter(|step| step.child_instance.is_some())
            .count(),
        2
    );
    let proof = &artifacts.static_plan.resources.tasks[0];
    assert_eq!(proof.root_workflows, 1);
    assert_eq!(proof.expanded_workflow_instances, 3);
    assert_eq!(proof.expanded_nodes, 10);
    assert_eq!(proof.expanded_edges, 7);
}

#[test]
fn bounded_backedge_is_retained_once_and_never_unrolled() {
    let root = id("018f0000-0000-7000-8000-000000000212");
    let action = id("018f0000-0000-7000-8000-000000000215");
    let sources = [WorkflowSource {
        source_path: "bounded-backedge.aurora-workflow.yaml",
        source_bytes: BOUNDED_BACKEDGE,
    }];
    let output = compile_static_workflow_plan(
        &sources,
        validation_limits(),
        &[task(1, root)],
        &[claim(1, vec![root], action)],
        target_limits(target_values()),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("bounded backedge is valid: {error}"));
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let artifacts = output
        .artifacts
        .unwrap_or_else(|| unreachable!("bounded backedge publishes a plan"));
    assert_eq!(artifacts.static_plan.instances.len(), 1);
    assert_eq!(artifacts.static_plan.steps.len(), 3);
    assert_eq!(artifacts.static_plan.edges.len(), 5);
    assert_eq!(
        artifacts.static_plan.resources.tasks[0].backedge_traversals_per_run,
        3
    );

    let mut too_small = target_values();
    too_small.max_backedge_traversals_per_run = 2;
    let rejected = compile_static_workflow_plan(
        &sources,
        validation_limits(),
        &[task(1, root)],
        &[claim(1, vec![root], action)],
        target_limits(too_small),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("bounded caller inputs remain valid: {error}"));
    assert!(rejected.artifacts.is_none());
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(
        rejected.diagnostics[0].code,
        WorkflowDiagnosticCode::ResourceBudgetExceeded
    );
}

#[test]
fn topology_and_recursion_failures_suppress_every_artifact() {
    let cases = [
        (
            "recursive.aurora-workflow.yaml",
            RECURSIVE_WORKFLOW,
            id("018f0000-0000-7000-8000-000000000302"),
            vec![WorkflowDiagnosticCode::RecursiveSubworkflow],
        ),
        (
            "reversed.aurora-workflow.yaml",
            REVERSED_FORWARD_ORDER,
            id("018f0000-0000-7000-8000-000000000312"),
            vec![WorkflowDiagnosticCode::InvalidForwardDependency],
        ),
        (
            "unreachable.aurora-workflow.yaml",
            UNREACHABLE_CYCLE,
            id("018f0000-0000-7000-8000-000000000322"),
            vec![
                WorkflowDiagnosticCode::UnmarkedCycleEdge,
                WorkflowDiagnosticCode::UnreachableNode,
                WorkflowDiagnosticCode::UnreachableNode,
                WorkflowDiagnosticCode::InvalidForwardDependency,
            ],
        ),
    ];
    for (path, source_bytes, root, mut expected) in cases {
        let output = compile_static_workflow_plan(
            &[WorkflowSource {
                source_path: path,
                source_bytes,
            }],
            validation_limits(),
            &[task(1, root)],
            &[],
            target_limits(target_values()),
            artifact_limits(),
        )
        .unwrap_or_else(|error| unreachable!("caller inputs are valid: {error}"));
        assert!(output.artifacts.is_none());
        let mut actual = output
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected, "{path}");
    }
}

#[test]
fn empty_task_set_is_rejected_before_any_artifact_is_built() {
    let source = [WorkflowSource {
        source_path: "minimal.aurora-workflow.yaml",
        source_bytes: MINIMAL,
    }];
    let result = compile_static_workflow_plan(
        &source,
        validation_limits(),
        &[],
        &[],
        target_limits(target_values()),
        artifact_limits(),
    );
    assert_eq!(result, Err(WorkflowPlanInputError::EmptyTaskSet));
}

#[test]
fn input_order_does_not_change_ir_plan_or_digests() {
    let parent = id("018f0000-0000-7000-8000-000000000021");
    let call = id("018f0000-0000-7000-8000-000000000023");
    let child_action = id("018f0000-0000-7000-8000-000000000013");
    let first_sources = [
        WorkflowSource {
            source_path: "z-parent.aurora-workflow.yaml",
            source_bytes: PARENT,
        },
        WorkflowSource {
            source_path: "a-child.aurora-workflow.yaml",
            source_bytes: CHILD,
        },
    ];
    let second_sources = [first_sources[1], first_sources[0]];
    let tasks = [task(7, parent)];
    let first_claims = [
        claim(7, vec![parent], call),
        claim(7, vec![parent, call], child_action),
    ];
    let second_claims = [first_claims[1].clone(), first_claims[0].clone()];
    let first = compile_static_workflow_plan(
        &first_sources,
        validation_limits(),
        &tasks,
        &first_claims,
        target_limits(target_values()),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("accepted inputs are consistent: {error}"));
    let second = compile_static_workflow_plan(
        &second_sources,
        validation_limits(),
        &tasks,
        &second_claims,
        target_limits(target_values()),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("accepted inputs are consistent: {error}"));
    let first = first
        .artifacts
        .unwrap_or_else(|| unreachable!("valid plan"));
    let second = second
        .artifacts
        .unwrap_or_else(|| unreachable!("valid plan"));
    assert_eq!(first.canonical_ir_json, second.canonical_ir_json);
    assert_eq!(first.static_plan_json, second.static_plan_json);
    assert_eq!(first.semantic_digest, second.semantic_digest);
    assert_eq!(first.plan_digest, second.plan_digest);
}

#[test]
fn missing_extra_and_duplicate_resource_claims_are_rejected_before_generation() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let action = id("018f0000-0000-7000-8000-000000000004");
    let source = [WorkflowSource {
        source_path: "minimal.aurora-workflow.yaml",
        source_bytes: MINIMAL,
    }];
    let tasks = [task(1, root)];
    let valid = claim(1, vec![root], action);
    let cases = [
        Vec::new(),
        vec![valid.clone(), valid.clone()],
        vec![claim(
            1,
            vec![root],
            id("018f0000-0000-7000-8000-000000000099"),
        )],
    ];
    for claims in cases {
        let result = compile_static_workflow_plan(
            &source,
            validation_limits(),
            &tasks,
            &claims,
            target_limits(target_values()),
            artifact_limits(),
        );
        assert_eq!(result, Err(WorkflowPlanInputError::InvalidResourceClaim));
    }
}

#[test]
fn overlapping_static_writers_are_rejected_even_across_tasks_roots() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let action = id("018f0000-0000-7000-8000-000000000004");
    let target = id("018f0000-0000-7000-8000-000000000090");
    let source = [WorkflowSource {
        source_path: "minimal.aurora-workflow.yaml",
        source_bytes: MINIMAL,
    }];
    let tasks = [task(1, root), task(2, root)];
    let mut first = claim(1, vec![root], action);
    first.writes.push(WorkflowWriteRegion {
        target_id: target,
        offset_bytes: 0,
        size_bytes: 4,
    });
    let mut second = claim(2, vec![root], action);
    second.writes.push(WorkflowWriteRegion {
        target_id: target,
        offset_bytes: 2,
        size_bytes: 2,
    });
    let output = compile_static_workflow_plan(
        &source,
        validation_limits(),
        &tasks,
        &[first, second],
        target_limits(target_values()),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("claims are structurally valid: {error}"));
    assert!(output.artifacts.is_none());
    assert_eq!(output.diagnostics.len(), 1);
    assert_eq!(
        output.diagnostics[0].code,
        WorkflowDiagnosticCode::WriteConflict
    );
}

#[test]
fn exact_resource_boundary_passes_and_one_less_suppresses_all_artifacts() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let action = id("018f0000-0000-7000-8000-000000000004");
    let source = [WorkflowSource {
        source_path: "minimal.aurora-workflow.yaml",
        source_bytes: MINIMAL,
    }];
    let tasks = [task(1, root)];
    let claims = [claim(1, vec![root], action)];
    let baseline = compile_static_workflow_plan(
        &source,
        validation_limits(),
        &tasks,
        &claims,
        target_limits(target_values()),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("baseline inputs are valid: {error}"));
    let proof = baseline
        .artifacts
        .as_ref()
        .unwrap_or_else(|| unreachable!("baseline plan"))
        .static_plan
        .resources
        .tasks[0]
        .clone();
    let mut exact = target_values();
    exact.max_workflows_per_task = proof.root_workflows;
    exact.max_source_nodes_per_workflow = 3;
    exact.max_source_edges_per_workflow = 2;
    exact.max_expanded_workflow_instances = proof.expanded_workflow_instances;
    exact.max_expanded_nodes_per_task = proof.expanded_nodes;
    exact.max_expanded_edges_per_task = proof.expanded_edges;
    exact.max_active_nodes_per_task = proof.active_nodes;
    exact.max_node_executions_per_release = proof.node_executions_per_release;
    exact.max_fork_nesting_depth = 1;
    exact.max_branches_per_fork = 1;
    exact.max_pending_cancellations = 1;
    exact.max_subworkflow_expansion_depth = proof.subworkflow_expansion_depth;
    exact.max_workflow_state_bytes_per_task = proof.workflow_state_bytes;
    exact.max_workflow_staging_bytes_per_task = proof.workflow_staging_bytes;
    exact.max_watch_handles_per_task = 1;
    exact.max_trace_events_per_release = proof.trace_events_per_release;
    exact.workflow_trace_ring_capacity = proof.trace_ring_capacity;
    let accepted = compile_static_workflow_plan(
        &source,
        validation_limits(),
        &tasks,
        &claims,
        target_limits(exact),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("exact limits are valid: {error}"));
    assert!(accepted.artifacts.is_some());

    exact.max_expanded_nodes_per_task = proof.expanded_nodes - 1;
    let rejected = compile_static_workflow_plan(
        &source,
        validation_limits(),
        &tasks,
        &claims,
        target_limits(exact),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("limit is non-zero: {error}"));
    assert!(rejected.artifacts.is_none());
    assert_eq!(rejected.diagnostics.len(), 1);
    assert_eq!(
        rejected.diagnostics[0].code,
        WorkflowDiagnosticCode::ResourceBudgetExceeded
    );
}

#[test]
fn artifact_byte_limits_accept_equality_and_reject_first_excess_atomically() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let action = id("018f0000-0000-7000-8000-000000000004");
    let source = [WorkflowSource {
        source_path: "minimal.aurora-workflow.yaml",
        source_bytes: MINIMAL,
    }];
    let tasks = [task(1, root)];
    let claims = [claim(1, vec![root], action)];
    let baseline = compile_static_workflow_plan(
        &source,
        validation_limits(),
        &tasks,
        &claims,
        target_limits(target_values()),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("baseline inputs are valid: {error}"))
    .artifacts
    .unwrap_or_else(|| unreachable!("baseline artifacts"));
    let canonical_bytes = baseline.canonical_ir_json.len();
    let plan_bytes = baseline.static_plan_json.len();
    let exact_limits = WorkflowArtifactLimits::new(canonical_bytes, plan_bytes)
        .unwrap_or_else(|error| unreachable!("serialized artifacts are non-empty: {error}"));
    let exact = compile_static_workflow_plan(
        &source,
        validation_limits(),
        &tasks,
        &claims,
        target_limits(target_values()),
        exact_limits,
    )
    .unwrap_or_else(|error| unreachable!("exact byte limits are valid: {error}"));
    assert!(exact.artifacts.is_some());

    let cases = [
        WorkflowArtifactLimits::new(canonical_bytes - 1, plan_bytes)
            .unwrap_or_else(|error| unreachable!("lower canonical limit is non-zero: {error}")),
        WorkflowArtifactLimits::new(canonical_bytes, plan_bytes - 1)
            .unwrap_or_else(|error| unreachable!("lower plan limit is non-zero: {error}")),
    ];
    for limits in cases {
        let rejected = compile_static_workflow_plan(
            &source,
            validation_limits(),
            &tasks,
            &claims,
            target_limits(target_values()),
            limits,
        )
        .unwrap_or_else(|error| unreachable!("lower byte limit is a diagnostic: {error}"));
        assert!(rejected.artifacts.is_none());
        assert_eq!(rejected.diagnostics.len(), 1);
        assert_eq!(
            rejected.diagnostics[0].code,
            WorkflowDiagnosticCode::ResourceBudgetExceeded
        );
    }
}

#[test]
fn every_target_limit_is_mandatory_and_nonzero() {
    let names_and_zeroers: &[(&str, LimitZeroer)] = &[
        ("max_workflows_per_task", |v| v.max_workflows_per_task = 0),
        ("max_source_nodes_per_workflow", |v| {
            v.max_source_nodes_per_workflow = 0;
        }),
        ("max_source_edges_per_workflow", |v| {
            v.max_source_edges_per_workflow = 0;
        }),
        ("max_expanded_workflow_instances", |v| {
            v.max_expanded_workflow_instances = 0;
        }),
        ("max_expanded_nodes_per_task", |v| {
            v.max_expanded_nodes_per_task = 0;
        }),
        ("max_expanded_edges_per_task", |v| {
            v.max_expanded_edges_per_task = 0;
        }),
        ("max_active_nodes_per_task", |v| {
            v.max_active_nodes_per_task = 0;
        }),
        ("max_node_executions_per_release", |v| {
            v.max_node_executions_per_release = 0;
        }),
        ("max_fork_nesting_depth", |v| v.max_fork_nesting_depth = 0),
        ("max_branches_per_fork", |v| v.max_branches_per_fork = 0),
        ("max_pending_cancellations", |v| {
            v.max_pending_cancellations = 0;
        }),
        ("max_subworkflow_expansion_depth", |v| {
            v.max_subworkflow_expansion_depth = 0;
        }),
        ("max_backedge_traversals_per_run", |v| {
            v.max_backedge_traversals_per_run = 0;
        }),
        ("max_wait_cycles", |v| v.max_wait_cycles = 0),
        ("max_workflow_state_bytes_per_task", |v| {
            v.max_workflow_state_bytes_per_task = 0;
        }),
        ("max_workflow_staging_bytes_per_task", |v| {
            v.max_workflow_staging_bytes_per_task = 0;
        }),
        ("max_watch_handles_per_task", |v| {
            v.max_watch_handles_per_task = 0;
        }),
        ("max_trace_events_per_release", |v| {
            v.max_trace_events_per_release = 0;
        }),
        ("workflow_trace_ring_capacity", |v| {
            v.workflow_trace_ring_capacity = 0;
        }),
    ];
    for (name, zero) in names_and_zeroers {
        let mut values = target_values();
        zero(&mut values);
        let error = WorkflowTargetLimits::new(values);
        assert!(error.is_err());
        assert_eq!(
            error.err().map(|value| value.to_string()),
            Some(format!("Workflow planning limit `{name}` must be non-zero"))
        );
    }
}
