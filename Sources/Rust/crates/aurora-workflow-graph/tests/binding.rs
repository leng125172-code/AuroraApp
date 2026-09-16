//! R2-05 exact typed Action and condition binding closure tests.

use aurora_workflow_cyclic::{
    RuntimeBindingLimits, StructuredEdgeDefinition, StructuredEdgeTarget, StructuredInstanceHandle,
    StructuredNodeDefinition, StructuredNodeKind, WorkflowEdgeHandle, WorkflowEdgeRange,
    WorkflowNodeHandle,
};
use aurora_workflow_graph::{
    ExpandedActionBindingInput, ExpandedNodeResourceInput, StableId, TaskBindingImageInput,
    TaskWorkflowPlanningInput, WorkflowActionKind, WorkflowActionPortBinding,
    WorkflowArtifactLimits, WorkflowBindingVersion, WorkflowConditionBindingInput,
    WorkflowPlanInputError, WorkflowPortDirection, WorkflowSource, WorkflowTargetLimitValues,
    WorkflowTargetLimits, WorkflowValidationLimits, WorkflowValueArea, WorkflowValueSlot,
    WorkflowValueType, WorkflowWriteRegion, YamlSourceLimits, build_runtime_binding_plan,
    compile_bound_workflow_plan,
};

const MINIMAL: &[u8] =
    include_bytes!("../../../../Contracts/workflow/v1/examples/minimal.valid.aurora-workflow.yaml");
const CONDITIONAL: &[u8] = br"kind: aurora.cyclic-workflow
schemaVersion: { major: 1, minor: 0, lifecycle: preview }
documentId: 018f0000-0000-7000-8000-000000000401
workflowId: 018f0000-0000-7000-8000-000000000402
canonicalName: conditional_action
permanent: false
nodes:
  - { nodeId: 018f0000-0000-7000-8000-000000000403, canonicalName: entry, kind: Entry }
  - { nodeId: 018f0000-0000-7000-8000-000000000404, canonicalName: wait, kind: Wait, executionOrder: 0, cancellationBoundary: false, mode: condition, conditionId: 018f0000-0000-7000-8000-000000000408, timeoutCycles: 5 }
  - { nodeId: 018f0000-0000-7000-8000-000000000405, canonicalName: end, kind: End }
edges:
  - { edgeId: 018f0000-0000-7000-8000-000000000406, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000403, targetNodeId: 018f0000-0000-7000-8000-000000000404, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000407, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000404, targetNodeId: 018f0000-0000-7000-8000-000000000405, backedge: false }
";
const CONDITION_CHILD: &[u8] = br"kind: aurora.cyclic-workflow
schemaVersion: { major: 1, minor: 0, lifecycle: preview }
documentId: 018f0000-0000-7000-8000-000000000411
workflowId: 018f0000-0000-7000-8000-000000000412
canonicalName: condition_child
permanent: false
nodes:
  - { nodeId: 018f0000-0000-7000-8000-000000000413, canonicalName: entry, kind: Entry }
  - { nodeId: 018f0000-0000-7000-8000-000000000414, canonicalName: wait, kind: Wait, executionOrder: 0, cancellationBoundary: false, mode: condition, conditionId: 018f0000-0000-7000-8000-000000000415, timeoutCycles: 5 }
  - { nodeId: 018f0000-0000-7000-8000-000000000416, canonicalName: end, kind: End }
edges:
  - { edgeId: 018f0000-0000-7000-8000-000000000417, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000413, targetNodeId: 018f0000-0000-7000-8000-000000000414, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000418, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000414, targetNodeId: 018f0000-0000-7000-8000-000000000416, backedge: false }
";
const TWO_CALL_PARENT: &[u8] = br"kind: aurora.cyclic-workflow
schemaVersion: { major: 1, minor: 0, lifecycle: preview }
documentId: 018f0000-0000-7000-8000-000000000421
workflowId: 018f0000-0000-7000-8000-000000000422
canonicalName: two_condition_calls
permanent: false
nodes:
  - { nodeId: 018f0000-0000-7000-8000-000000000423, canonicalName: entry, kind: Entry }
  - { nodeId: 018f0000-0000-7000-8000-000000000424, canonicalName: first_call, kind: Subworkflow, executionOrder: 0, cancellationBoundary: false, targetWorkflowId: 018f0000-0000-7000-8000-000000000412 }
  - { nodeId: 018f0000-0000-7000-8000-000000000425, canonicalName: second_call, kind: Subworkflow, executionOrder: 1, cancellationBoundary: false, targetWorkflowId: 018f0000-0000-7000-8000-000000000412 }
  - { nodeId: 018f0000-0000-7000-8000-000000000426, canonicalName: end, kind: End }
edges:
  - { edgeId: 018f0000-0000-7000-8000-000000000427, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000423, targetNodeId: 018f0000-0000-7000-8000-000000000424, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000428, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000424, targetNodeId: 018f0000-0000-7000-8000-000000000425, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000429, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000425, targetNodeId: 018f0000-0000-7000-8000-000000000426, backedge: false }
";

fn id(value: &str) -> StableId {
    StableId::parse(value).unwrap_or_else(|| unreachable!("test UUID is canonical v7"))
}

fn validation_limits() -> WorkflowValidationLimits {
    let yaml = YamlSourceLimits::new(128 * 1024, 32, 32, 512, 128 * 1024)
        .unwrap_or_else(|error| unreachable!("valid limits: {error}"));
    WorkflowValidationLimits::new(yaml, yaml, 16, 128, 256, 16, 128, 256, 32, 512)
        .unwrap_or_else(|error| unreachable!("valid limits: {error}"))
}

fn target_limits() -> WorkflowTargetLimits {
    WorkflowTargetLimits::new(WorkflowTargetLimitValues {
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
        max_action_ports_per_node: 1,
        max_condition_bindings_per_task: 1,
        max_trace_events_per_release: 4096,
        workflow_trace_ring_capacity: 4096,
    })
    .unwrap_or_else(|error| unreachable!("valid limits: {error}"))
}

fn artifact_limits() -> WorkflowArtifactLimits {
    WorkflowArtifactLimits::new(1024 * 1024, 1024 * 1024)
        .unwrap_or_else(|error| unreachable!("valid limits: {error}"))
}

fn task(root: StableId) -> TaskWorkflowPlanningInput {
    TaskWorkflowPlanningInput {
        task_handle: 7,
        root_workflow_ids: vec![root],
        watches: Vec::new(),
        trace_ring_capacity: 64,
    }
}

fn image() -> TaskBindingImageInput {
    TaskBindingImageInput {
        task_handle: 7,
        application_state_bytes: 64,
        output_bytes: 64,
    }
}

fn action_claim(root: StableId, action: StableId) -> ExpandedNodeResourceInput {
    let slot = WorkflowValueSlot {
        target_id: id("018f0000-0000-7000-8000-000000000431"),
        area: WorkflowValueArea::Output,
        offset_bytes: 4,
        image_offset_bytes: 4,
        value_type: WorkflowValueType::Dint,
    };
    ExpandedNodeResourceInput {
        task_handle: 7,
        instance_path: vec![root],
        node_id: action,
        committed_state_bytes: 3,
        staging_state_bytes: 5,
        trace_events_per_release: 1,
        writes: vec![WorkflowWriteRegion {
            target_id: slot.target_id,
            offset_bytes: slot.offset_bytes,
            size_bytes: 4,
        }],
        action_binding: Some(ExpandedActionBindingInput {
            version: WorkflowBindingVersion::V1_0,
            binding_id: id("018f0000-0000-7000-8000-000000000432"),
            kind: WorkflowActionKind::StPou,
            target_handle: 9,
            invocation_state_offset_bytes: 16,
            ports: vec![WorkflowActionPortBinding {
                port: 0,
                direction: WorkflowPortDirection::Output,
                slot,
            }],
            committed_state_bytes: 3,
            staging_state_bytes: 5,
            trace_events_per_release: 1,
        }),
    }
}

fn call_claim(root: StableId, call: StableId) -> ExpandedNodeResourceInput {
    ExpandedNodeResourceInput {
        task_handle: 7,
        instance_path: vec![root],
        node_id: call,
        committed_state_bytes: 0,
        staging_state_bytes: 0,
        trace_events_per_release: 0,
        writes: Vec::new(),
        action_binding: None,
    }
}

#[test]
fn bound_plan_publishes_one_exact_action_binding() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let claim = action_claim(root, id("018f0000-0000-7000-8000-000000000004"));
    let output = compile_bound_workflow_plan(
        &[WorkflowSource {
            source_path: "minimal.aurora-workflow.yaml",
            source_bytes: MINIMAL,
        }],
        validation_limits(),
        &[task(root)],
        &[claim],
        &[],
        &[image()],
        target_limits(),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("valid binding compiles: {error}"));
    let artifacts = output
        .artifacts
        .unwrap_or_else(|| unreachable!("valid binding publishes artifacts"));
    assert_eq!(artifacts.static_plan.schema_version.minor, 1);
    assert!(
        artifacts.static_plan.node_resources[0]
            .action_binding
            .is_some()
    );
    assert!(artifacts.static_plan.condition_bindings.is_empty());
}

#[test]
fn action_binding_rejects_missing_extra_and_resource_mismatch() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let action = id("018f0000-0000-7000-8000-000000000004");
    let sources = [WorkflowSource {
        source_path: "minimal.aurora-workflow.yaml",
        source_bytes: MINIMAL,
    }];
    let tasks = [task(root)];
    let compile = |claims: &[ExpandedNodeResourceInput]| {
        compile_bound_workflow_plan(
            &sources,
            validation_limits(),
            &tasks,
            claims,
            &[],
            &[image()],
            target_limits(),
            artifact_limits(),
        )
    };

    let mut missing = action_claim(root, action);
    missing.action_binding = None;
    assert_eq!(
        compile(&[missing]),
        Err(WorkflowPlanInputError::InvalidActionBinding)
    );

    let mut mismatch = action_claim(root, action);
    mismatch.writes.clear();
    assert_eq!(
        compile(&[mismatch]),
        Err(WorkflowPlanInputError::InvalidActionBinding)
    );

    let mut extra = action_claim(root, action);
    extra.node_id = id("018f0000-0000-7000-8000-000000000099");
    assert_eq!(
        compile(&[extra]),
        Err(WorkflowPlanInputError::InvalidResourceClaim)
    );

    let mut exceeded = action_claim(root, action);
    exceeded
        .action_binding
        .as_mut()
        .unwrap_or_else(|| unreachable!("fixture carries a binding"))
        .ports
        .push(WorkflowActionPortBinding {
            port: 1,
            direction: WorkflowPortDirection::Input,
            slot: WorkflowValueSlot {
                target_id: id("018f0000-0000-7000-8000-000000000434"),
                area: WorkflowValueArea::State,
                offset_bytes: 0,
                image_offset_bytes: 0,
                value_type: WorkflowValueType::Bool,
            },
        });
    assert_eq!(
        compile(&[exceeded]),
        Err(WorkflowPlanInputError::InvalidActionBinding)
    );
}

#[test]
fn condition_catalog_rejects_both_missing_and_extra_entries() {
    let root = id("018f0000-0000-7000-8000-000000000402");
    let condition_id = id("018f0000-0000-7000-8000-000000000408");
    let condition = WorkflowConditionBindingInput {
        task_handle: 7,
        instance_path: vec![root],
        condition_id,
        source: WorkflowValueSlot {
            target_id: id("018f0000-0000-7000-8000-000000000433"),
            area: WorkflowValueArea::State,
            offset_bytes: 0,
            image_offset_bytes: 0,
            value_type: WorkflowValueType::Bool,
        },
    };
    let sources = [WorkflowSource {
        source_path: "conditional.aurora-workflow.yaml",
        source_bytes: CONDITIONAL,
    }];
    let tasks = [task(root)];
    let claims = [];
    let compile = |conditions: &[WorkflowConditionBindingInput]| {
        compile_bound_workflow_plan(
            &sources,
            validation_limits(),
            &tasks,
            &claims,
            conditions,
            &[image()],
            target_limits(),
            artifact_limits(),
        )
    };

    assert_eq!(
        compile(&[]),
        Err(WorkflowPlanInputError::InvalidConditionBinding)
    );
    let accepted = compile(std::slice::from_ref(&condition))
        .unwrap_or_else(|error| unreachable!("exact condition compiles: {error}"));
    assert_eq!(
        accepted
            .artifacts
            .unwrap_or_else(|| unreachable!("exact condition publishes artifacts"))
            .static_plan
            .condition_bindings
            .len(),
        1
    );

    let extra = WorkflowConditionBindingInput {
        condition_id: id("018f0000-0000-7000-8000-000000000409"),
        ..condition.clone()
    };
    assert_eq!(
        compile(&[condition, extra]),
        Err(WorkflowPlanInputError::InvalidConditionBinding)
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn repeated_subworkflow_calls_keep_condition_bindings_isolated() {
    let root = id("018f0000-0000-7000-8000-000000000422");
    let first_call = id("018f0000-0000-7000-8000-000000000424");
    let second_call = id("018f0000-0000-7000-8000-000000000425");
    let condition_id = id("018f0000-0000-7000-8000-000000000415");
    let sources = [
        WorkflowSource {
            source_path: "parent.aurora-workflow.yaml",
            source_bytes: TWO_CALL_PARENT,
        },
        WorkflowSource {
            source_path: "child.aurora-workflow.yaml",
            source_bytes: CONDITION_CHILD,
        },
    ];
    let tasks = [task(root)];
    let claims = [call_claim(root, first_call), call_claim(root, second_call)];
    let condition = |call, target| WorkflowConditionBindingInput {
        task_handle: 7,
        instance_path: vec![root, call],
        condition_id,
        source: WorkflowValueSlot {
            target_id: target,
            area: WorkflowValueArea::State,
            offset_bytes: 0,
            image_offset_bytes: u64::from(target != id("018f0000-0000-7000-8000-000000000435")),
            value_type: WorkflowValueType::Bool,
        },
    };
    let first = condition(first_call, id("018f0000-0000-7000-8000-000000000435"));
    let second = condition(second_call, id("018f0000-0000-7000-8000-000000000436"));

    let aliased = compile_bound_workflow_plan(
        &sources,
        validation_limits(),
        &tasks,
        &claims,
        &[
            first.clone(),
            WorkflowConditionBindingInput {
                source: WorkflowValueSlot {
                    image_offset_bytes: 0,
                    ..second.source
                },
                ..second.clone()
            },
        ],
        &[image()],
        WorkflowTargetLimits::new(WorkflowTargetLimitValues {
            max_condition_bindings_per_task: 2,
            ..target_limits().values()
        })
        .unwrap_or_else(|error| unreachable!("valid expanded condition capacity: {error}")),
        artifact_limits(),
    );
    assert_eq!(aliased, Err(WorkflowPlanInputError::InvalidBindingImage));

    let missing = compile_bound_workflow_plan(
        &sources,
        validation_limits(),
        &tasks,
        &claims,
        std::slice::from_ref(&first),
        &[image()],
        target_limits(),
        artifact_limits(),
    );
    assert_eq!(
        missing,
        Err(WorkflowPlanInputError::InvalidConditionBinding)
    );

    let output = compile_bound_workflow_plan(
        &sources,
        validation_limits(),
        &tasks,
        &claims,
        &[first, second],
        &[image()],
        WorkflowTargetLimits::new(WorkflowTargetLimitValues {
            max_condition_bindings_per_task: 2,
            ..target_limits().values()
        })
        .unwrap_or_else(|error| unreachable!("valid expanded condition capacity: {error}")),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("isolated conditions compile: {error}"));
    let conditions = &output
        .artifacts
        .unwrap_or_else(|| unreachable!("isolated conditions publish artifacts"))
        .static_plan
        .condition_bindings;
    assert_eq!(conditions.len(), 2);
    assert_ne!(conditions[0].instance, conditions[1].instance);
    assert_ne!(
        conditions[0].source.target_id,
        conditions[1].source.target_id
    );
    assert_ne!(
        conditions[0].source.image_offset_bytes,
        conditions[1].source.image_offset_bytes
    );
}

#[test]
fn resolved_image_capacity_rejects_first_byte_beyond_boundary() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let action = id("018f0000-0000-7000-8000-000000000004");
    let mut claim = action_claim(root, action);
    claim
        .action_binding
        .as_mut()
        .unwrap_or_else(|| unreachable!("fixture carries binding"))
        .ports[0]
        .slot
        .image_offset_bytes = 61;
    let result = compile_bound_workflow_plan(
        &[WorkflowSource {
            source_path: "minimal.aurora-workflow.yaml",
            source_bytes: MINIMAL,
        }],
        validation_limits(),
        &[task(root)],
        &[claim],
        &[],
        &[image()],
        target_limits(),
        artifact_limits(),
    );
    assert_eq!(result, Err(WorkflowPlanInputError::InvalidActionBinding));
}

#[test]
fn writable_ports_accept_adjacency_but_reject_logical_or_physical_overlap() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let action = id("018f0000-0000-7000-8000-000000000004");
    let mut adjacent = action_claim(root, action);
    let binding = adjacent
        .action_binding
        .as_mut()
        .unwrap_or_else(|| unreachable!("fixture carries binding"));
    binding.ports.push(WorkflowActionPortBinding {
        port: 1,
        direction: WorkflowPortDirection::Output,
        slot: WorkflowValueSlot {
            target_id: binding.ports[0].slot.target_id,
            area: WorkflowValueArea::Output,
            offset_bytes: 8,
            image_offset_bytes: 8,
            value_type: WorkflowValueType::Dint,
        },
    });
    adjacent.writes.push(WorkflowWriteRegion {
        target_id: binding.ports[0].slot.target_id,
        offset_bytes: 8,
        size_bytes: 4,
    });
    let sources = [WorkflowSource {
        source_path: "minimal.aurora-workflow.yaml",
        source_bytes: MINIMAL,
    }];
    let tasks = [task(root)];
    let limits = WorkflowTargetLimits::new(WorkflowTargetLimitValues {
        max_action_ports_per_node: 2,
        ..target_limits().values()
    })
    .unwrap_or_else(|error| unreachable!("valid two-port limit: {error}"));
    let compile = |claim: ExpandedNodeResourceInput| {
        compile_bound_workflow_plan(
            &sources,
            validation_limits(),
            &tasks,
            &[claim],
            &[],
            &[image()],
            limits,
            artifact_limits(),
        )
    };
    assert!(compile(adjacent.clone()).is_ok());

    let mut logical_overlap = adjacent.clone();
    logical_overlap
        .action_binding
        .as_mut()
        .unwrap_or_else(|| unreachable!("fixture carries binding"))
        .ports[1]
        .slot
        .offset_bytes = 7;
    logical_overlap.writes[1].offset_bytes = 7;
    assert_eq!(
        compile(logical_overlap),
        Err(WorkflowPlanInputError::InvalidActionBinding)
    );

    let mut physical_overlap = adjacent;
    physical_overlap
        .action_binding
        .as_mut()
        .unwrap_or_else(|| unreachable!("fixture carries binding"))
        .ports[1]
        .slot
        .image_offset_bytes = 7;
    assert_eq!(
        compile(physical_overlap),
        Err(WorkflowPlanInputError::InvalidActionBinding)
    );
}

#[test]
fn invocation_state_cannot_alias_any_resolved_state_slot() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let mut claim = action_claim(root, id("018f0000-0000-7000-8000-000000000004"));
    claim
        .action_binding
        .as_mut()
        .unwrap_or_else(|| unreachable!("fixture carries binding"))
        .ports[0]
        .slot
        .area = WorkflowValueArea::State;
    claim
        .action_binding
        .as_mut()
        .unwrap_or_else(|| unreachable!("fixture carries binding"))
        .ports[0]
        .slot
        .image_offset_bytes = 16;
    let result = compile_bound_workflow_plan(
        &[WorkflowSource {
            source_path: "minimal.aurora-workflow.yaml",
            source_bytes: MINIMAL,
        }],
        validation_limits(),
        &[task(root)],
        &[claim],
        &[],
        &[image()],
        target_limits(),
        artifact_limits(),
    );
    assert_eq!(result, Err(WorkflowPlanInputError::InvalidActionBinding));
}

#[test]
fn runtime_bridge_binds_plan_identity_and_rejects_missing_or_wrong_kind() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let output = compile_bound_workflow_plan(
        &[WorkflowSource {
            source_path: "minimal.aurora-workflow.yaml",
            source_bytes: MINIMAL,
        }],
        validation_limits(),
        &[task(root)],
        &[action_claim(
            root,
            id("018f0000-0000-7000-8000-000000000004"),
        )],
        &[],
        &[image()],
        target_limits(),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("valid bound plan: {error}"));
    let artifacts = output
        .artifacts
        .unwrap_or_else(|| unreachable!("valid plan publishes artifacts"));
    let node_handle = WorkflowNodeHandle::new(0).unwrap_or_else(|_| unreachable!("valid handle"));
    let edge_handle = WorkflowEdgeHandle::new(0).unwrap_or_else(|_| unreachable!("valid handle"));
    let nodes = [StructuredNodeDefinition {
        handle: node_handle,
        instance: StructuredInstanceHandle(0),
        kind: StructuredNodeKind::Action,
        outgoing: WorkflowEdgeRange { start: 0, count: 1 },
        cancellation_boundary: false,
    }];
    let edges = [StructuredEdgeDefinition {
        handle: edge_handle,
        source: node_handle,
        target: StructuredEdgeTarget::Complete,
        branch: None,
        maximum_traversals_per_run: None,
    }];
    let limits = RuntimeBindingLimits {
        maximum_actions: 1,
        maximum_conditions: 1,
        maximum_ports_per_action: 1,
        maximum_guards_per_decision: 1,
    };
    let plan = build_runtime_binding_plan(&artifacts, 7, &nodes, &edges, image(), limits)
        .unwrap_or_else(|error| unreachable!("exact bridge succeeds: {error}"));
    assert_ne!(plan.identity().0, [0; 32]);

    let mut swapped = artifacts.clone();
    swapped.static_plan.node_resources[0]
        .action_binding
        .as_mut()
        .unwrap_or_else(|| unreachable!("fixture carries binding"))
        .target_handle = 10;
    assert!(build_runtime_binding_plan(&swapped, 7, &nodes, &edges, image(), limits).is_err());

    assert!(build_runtime_binding_plan(&artifacts, 7, &nodes, &[], image(), limits).is_err());
    let mut wrong_nodes = nodes;
    wrong_nodes[0].kind = StructuredNodeKind::Decision;
    assert!(
        build_runtime_binding_plan(&artifacts, 7, &wrong_nodes, &edges, image(), limits).is_err()
    );
}
