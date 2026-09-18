//! R2-05 exact typed Action and condition binding closure tests.

use aurora_workflow_cyclic::{
    RuntimeBindingLimits, StructuredEdgeDefinition, StructuredEdgeTarget, StructuredInstanceHandle,
    StructuredJoinMode, StructuredNodeDefinition, StructuredNodeKind, WorkflowEdgeHandle,
    WorkflowEdgeRange, WorkflowNodeHandle,
};
use aurora_workflow_graph::{
    ExpandedActionBindingInput, ExpandedNodeResourceInput, StableId, TaskBindingImageInput,
    TaskWorkflowPlanningInput, WorkflowActionKind, WorkflowActionPortBinding,
    WorkflowArtifactLimits, WorkflowBindingVersion, WorkflowConditionBindingInput,
    WorkflowPlanArtifacts, WorkflowPlanInputError, WorkflowPortDirection, WorkflowSource,
    WorkflowTargetLimitValues, WorkflowTargetLimits, WorkflowValidationLimits, WorkflowValueArea,
    WorkflowValueSlot, WorkflowValueType, WorkflowWatchBindingInput, WorkflowWatchInput,
    WorkflowWriteRegion, YamlSourceLimits, build_runtime_binding_plan,
    build_runtime_traced_binding_plan, compile_bound_workflow_plan, compile_traced_workflow_plan,
};
use sha2::{Digest, Sha256};

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
const WAIT_CYCLES: &[u8] = br"kind: aurora.cyclic-workflow
schemaVersion: { major: 1, minor: 0, lifecycle: preview }
documentId: 018f0000-0000-7000-8000-000000000471
workflowId: 018f0000-0000-7000-8000-000000000472
canonicalName: wait_cycles
permanent: false
nodes:
  - { nodeId: 018f0000-0000-7000-8000-000000000473, canonicalName: entry, kind: Entry }
  - { nodeId: 018f0000-0000-7000-8000-000000000474, canonicalName: wait, kind: Wait, executionOrder: 0, cancellationBoundary: false, mode: cycles, waitCycles: 2 }
  - { nodeId: 018f0000-0000-7000-8000-000000000475, canonicalName: end, kind: End }
edges:
  - { edgeId: 018f0000-0000-7000-8000-000000000476, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000473, targetNodeId: 018f0000-0000-7000-8000-000000000474, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000477, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000474, targetNodeId: 018f0000-0000-7000-8000-000000000475, backedge: false }
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
const TWO_ACTION_CALL_PARENT: &[u8] = br"kind: aurora.cyclic-workflow
schemaVersion: { major: 1, minor: 0, lifecycle: preview }
documentId: 018f0000-0000-7000-8000-000000000461
workflowId: 018f0000-0000-7000-8000-000000000462
canonicalName: two_action_calls
permanent: false
nodes:
  - { nodeId: 018f0000-0000-7000-8000-000000000463, canonicalName: entry, kind: Entry }
  - { nodeId: 018f0000-0000-7000-8000-000000000464, canonicalName: first_call, kind: Subworkflow, executionOrder: 0, cancellationBoundary: false, targetWorkflowId: 018f0000-0000-7000-8000-000000000002 }
  - { nodeId: 018f0000-0000-7000-8000-000000000465, canonicalName: second_call, kind: Subworkflow, executionOrder: 1, cancellationBoundary: false, targetWorkflowId: 018f0000-0000-7000-8000-000000000002 }
  - { nodeId: 018f0000-0000-7000-8000-000000000466, canonicalName: end, kind: End }
edges:
  - { edgeId: 018f0000-0000-7000-8000-000000000467, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000463, targetNodeId: 018f0000-0000-7000-8000-000000000464, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000468, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000464, targetNodeId: 018f0000-0000-7000-8000-000000000465, backedge: false }
  - { edgeId: 018f0000-0000-7000-8000-000000000469, kind: control, sourceNodeId: 018f0000-0000-7000-8000-000000000465, targetNodeId: 018f0000-0000-7000-8000-000000000466, backedge: false }
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

fn traced_action_claim(root: StableId, action: StableId) -> ExpandedNodeResourceInput {
    let mut claim = action_claim(root, action);
    claim.trace_events_per_release = 0;
    claim
        .action_binding
        .as_mut()
        .unwrap_or_else(|| unreachable!("fixture carries binding"))
        .trace_events_per_release = 0;
    claim
}

fn resign_static_plan(artifacts: &mut WorkflowPlanArtifacts) {
    artifacts.static_plan_json = serde_jcs::to_vec(&artifacts.static_plan)
        .unwrap_or_else(|error| unreachable!("test plan serializes: {error}"));
    let hash = Sha256::digest(&artifacts.static_plan_json);
    let mut digest = String::from("sha256:");
    for byte in hash {
        use std::fmt::Write;
        write!(&mut digest, "{byte:02x}")
            .unwrap_or_else(|error| unreachable!("string formatting succeeds: {error}"));
    }
    artifacts.plan_digest = digest;
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

    let mut redirected = edges;
    redirected[0].target = StructuredEdgeTarget::Node(node_handle);
    redirected[0].maximum_traversals_per_run = Some(1);
    assert!(
        build_runtime_binding_plan(&artifacts, 7, &nodes, &redirected, image(), limits).is_err()
    );
}

#[test]
fn runtime_bridge_rejects_a_structural_kind_mismatch() {
    let root = id("018f0000-0000-7000-8000-000000000472");
    let artifacts = compile_bound_workflow_plan(
        &[WorkflowSource {
            source_path: "wait-cycles.aurora-workflow.yaml",
            source_bytes: WAIT_CYCLES,
        }],
        validation_limits(),
        &[task(root)],
        &[],
        &[],
        &[image()],
        target_limits(),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("valid wait plan: {error}"))
    .artifacts
    .unwrap_or_else(|| unreachable!("valid wait plan publishes artifacts"));
    let node = WorkflowNodeHandle::new(0).unwrap_or_else(|_| unreachable!("valid handle"));
    let edge = WorkflowEdgeHandle::new(0).unwrap_or_else(|_| unreachable!("valid handle"));
    let mut nodes = [StructuredNodeDefinition {
        handle: node,
        instance: StructuredInstanceHandle(0),
        kind: StructuredNodeKind::WaitCycles { wait_cycles: 2 },
        outgoing: WorkflowEdgeRange { start: 0, count: 1 },
        cancellation_boundary: false,
    }];
    let edges = [StructuredEdgeDefinition {
        handle: edge,
        source: node,
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
    build_runtime_binding_plan(&artifacts, 7, &nodes, &edges, image(), limits)
        .unwrap_or_else(|error| unreachable!("exact structural bridge succeeds: {error}"));

    nodes[0].kind = StructuredNodeKind::Join {
        fork: None,
        mode: StructuredJoinMode::Merge,
    };
    assert!(build_runtime_binding_plan(&artifacts, 7, &nodes, &edges, image(), limits).is_err());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one end-to-end fixture covers catalog closure, fragment boundaries, bridge, and mutation rejection"
)]
fn traced_plan_closes_output_and_31_32_33_byte_watch_catalog_exactly() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let action = id("018f0000-0000-7000-8000-000000000004");
    let watch_ids = [
        id("018f0000-0000-7000-8000-000000000441"),
        id("018f0000-0000-7000-8000-000000000442"),
        id("018f0000-0000-7000-8000-000000000443"),
    ];
    let mut traced_task = task(root);
    traced_task.watches = watch_ids
        .into_iter()
        .zip([31, 32, 33])
        .map(|(value_id, encoded_bytes)| WorkflowWatchInput {
            value_id,
            encoded_bytes,
        })
        .collect();
    let watch_bindings = watch_ids
        .into_iter()
        .zip([31, 32, 33])
        .zip([0, 31, 0])
        .enumerate()
        .map(
            |(index, ((value_id, encoded_bytes), image_offset_bytes))| WorkflowWatchBindingInput {
                task_handle: 7,
                instance_path: vec![root],
                value_id,
                type_handle: 100
                    + u32::try_from(index).unwrap_or_else(|_| unreachable!("small fixture")),
                area: if index == 2 {
                    WorkflowValueArea::State
                } else {
                    WorkflowValueArea::Output
                },
                image_offset_bytes,
                encoded_bytes,
            },
        )
        .collect::<Vec<_>>();
    let output = compile_traced_workflow_plan(
        &[WorkflowSource {
            source_path: "minimal.aurora-workflow.yaml",
            source_bytes: MINIMAL,
        }],
        validation_limits(),
        &[traced_task.clone()],
        &[traced_action_claim(root, action)],
        &[],
        &[image()],
        &watch_bindings,
        target_limits(),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("valid traced plan: {error}"));
    let artifacts = output
        .artifacts
        .unwrap_or_else(|| unreachable!("valid traced plan publishes artifacts"));
    assert_eq!(artifacts.static_plan.schema_version.minor, 3);
    assert!(artifacts.static_plan.trace_structure.is_some());
    assert_eq!(artifacts.static_plan.trace_values.len(), 4);
    assert_eq!(
        artifacts
            .static_plan
            .trace_values
            .iter()
            .map(|value| value.handle.0)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    assert_eq!(
        artifacts
            .static_plan
            .trace_values
            .iter()
            .skip(1)
            .map(|value| value.fragment_count)
            .collect::<Vec<_>>(),
        vec![1, 1, 2]
    );

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
    let runtime_limits = RuntimeBindingLimits {
        maximum_actions: 1,
        maximum_conditions: 1,
        maximum_ports_per_action: 1,
        maximum_guards_per_decision: 1,
    };
    let runtime =
        build_runtime_traced_binding_plan(&artifacts, 7, &nodes, &edges, image(), runtime_limits)
            .unwrap_or_else(|error| unreachable!("exact traced bridge succeeds: {error}"));
    assert_eq!(runtime.watches.len(), 3);
    assert_eq!(
        runtime
            .watches
            .iter()
            .map(|watch| (watch.value_handle, watch.type_handle, watch.byte_count))
            .collect::<Vec<_>>(),
        vec![(1, 100, 31), (2, 101, 32), (3, 102, 33)]
    );

    let mut missing = artifacts.clone();
    missing.static_plan.trace_values.remove(0);
    resign_static_plan(&mut missing);
    assert!(
        build_runtime_traced_binding_plan(&missing, 7, &nodes, &edges, image(), runtime_limits)
            .is_err()
    );
    let mut extra = artifacts.clone();
    let mut duplicate = extra.static_plan.trace_values[0];
    duplicate.handle.0 = 4;
    extra.static_plan.trace_values.push(duplicate);
    resign_static_plan(&mut extra);
    assert!(
        build_runtime_traced_binding_plan(&extra, 7, &nodes, &edges, image(), runtime_limits)
            .is_err()
    );
    let mut swapped = artifacts.clone();
    let watch_value_id = swapped.static_plan.trace_values[1].value_id;
    swapped.static_plan.trace_values[1].value_id = swapped.static_plan.trace_values[0].value_id;
    swapped.static_plan.trace_values[0].value_id = watch_value_id;
    resign_static_plan(&mut swapped);
    assert!(
        build_runtime_traced_binding_plan(&swapped, 7, &nodes, &edges, image(), runtime_limits)
            .is_err()
    );

    let mut reordered_task = traced_task;
    reordered_task.watches.reverse();
    let mut reordered_bindings = watch_bindings;
    reordered_bindings.reverse();
    let reordered = compile_traced_workflow_plan(
        &[WorkflowSource {
            source_path: "minimal.aurora-workflow.yaml",
            source_bytes: MINIMAL,
        }],
        validation_limits(),
        &[reordered_task],
        &[traced_action_claim(root, action)],
        &[],
        &[image()],
        &reordered_bindings,
        target_limits(),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("reordered traced plan: {error}"))
    .artifacts
    .unwrap_or_else(|| unreachable!("valid reordered plan publishes artifacts"));
    assert_eq!(artifacts.static_plan_json, reordered.static_plan_json);
    assert_eq!(artifacts.plan_digest, reordered.plan_digest);
}

#[test]
fn traced_compile_rejects_implicit_reserve_and_non_exact_watch_bindings() {
    let root = id("018f0000-0000-7000-8000-000000000002");
    let action = id("018f0000-0000-7000-8000-000000000004");
    let value_id = id("018f0000-0000-7000-8000-000000000451");
    let mut traced_task = task(root);
    traced_task.watches = vec![WorkflowWatchInput {
        value_id,
        encoded_bytes: 4,
    }];
    let binding = WorkflowWatchBindingInput {
        task_handle: 7,
        instance_path: vec![root],
        value_id,
        type_handle: 77,
        area: WorkflowValueArea::State,
        image_offset_bytes: 60,
        encoded_bytes: 4,
    };
    let compile = |claim: ExpandedNodeResourceInput, watches: &[WorkflowWatchBindingInput]| {
        compile_traced_workflow_plan(
            &[WorkflowSource {
                source_path: "minimal.aurora-workflow.yaml",
                source_bytes: MINIMAL,
            }],
            validation_limits(),
            &[traced_task.clone()],
            &[claim],
            &[],
            &[image()],
            watches,
            target_limits(),
            artifact_limits(),
        )
    };
    assert_eq!(
        compile(action_claim(root, action), std::slice::from_ref(&binding)),
        Err(WorkflowPlanInputError::InvalidTraceBinding)
    );
    assert_eq!(
        compile(traced_action_claim(root, action), &[]),
        Err(WorkflowPlanInputError::InvalidTraceBinding)
    );
    assert_eq!(
        compile(
            traced_action_claim(root, action),
            &[binding.clone(), binding.clone()]
        ),
        Err(WorkflowPlanInputError::InvalidTraceBinding)
    );
    let mut out_of_bounds = binding;
    out_of_bounds.image_offset_bytes = 61;
    assert_eq!(
        compile(traced_action_claim(root, action), &[out_of_bounds]),
        Err(WorkflowPlanInputError::InvalidTraceBinding)
    );
}

#[test]
fn traced_outputs_keep_repeated_action_call_sites_physically_distinct() {
    let child = id("018f0000-0000-7000-8000-000000000002");
    let action = id("018f0000-0000-7000-8000-000000000004");
    let parent = id("018f0000-0000-7000-8000-000000000462");
    let first_call = id("018f0000-0000-7000-8000-000000000464");
    let second_call = id("018f0000-0000-7000-8000-000000000465");
    let mut first_action = traced_action_claim(child, action);
    first_action.instance_path = vec![parent, first_call];
    let mut second_action = traced_action_claim(child, action);
    second_action.instance_path = vec![parent, second_call];
    let second_binding = second_action
        .action_binding
        .as_mut()
        .unwrap_or_else(|| unreachable!("fixture carries binding"));
    second_binding.binding_id = id("018f0000-0000-7000-8000-000000000471");
    second_binding.invocation_state_offset_bytes = 24;
    second_binding.ports[0].slot.target_id = id("018f0000-0000-7000-8000-000000000472");
    second_binding.ports[0].slot.image_offset_bytes = 8;
    second_action.writes[0].target_id = second_binding.ports[0].slot.target_id;

    let output = compile_traced_workflow_plan(
        &[
            WorkflowSource {
                source_path: "child.aurora-workflow.yaml",
                source_bytes: MINIMAL,
            },
            WorkflowSource {
                source_path: "parent.aurora-workflow.yaml",
                source_bytes: TWO_ACTION_CALL_PARENT,
            },
        ],
        validation_limits(),
        &[task(parent)],
        &[
            call_claim(parent, first_call),
            call_claim(parent, second_call),
            first_action,
            second_action,
        ],
        &[],
        &[image()],
        &[],
        target_limits(),
        artifact_limits(),
    )
    .unwrap_or_else(|error| unreachable!("isolated call sites compile: {error}"));
    let artifacts = output
        .artifacts
        .unwrap_or_else(|| unreachable!("valid traced plan publishes artifacts"));
    let outputs = &artifacts.static_plan.trace_values;
    assert_eq!(outputs.len(), 2);
    assert_ne!(outputs[0].instance, outputs[1].instance);
    assert_ne!(outputs[0].image_offset_bytes, outputs[1].image_offset_bytes);
    assert_ne!(outputs[0].source, outputs[1].source);
}
