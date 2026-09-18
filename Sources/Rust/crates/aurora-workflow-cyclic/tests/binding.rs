//! R2-05 runtime binding table closure and capacity tests.

use aurora_control_contracts::FaultReason;
use aurora_workflow_cyclic::{
    BindingRange, RuntimeActionBackend, RuntimeActionDefinition, RuntimeActionHandle,
    RuntimeActionKind, RuntimeActionPort, RuntimeBindingContext, RuntimeBindingExecutor,
    RuntimeBindingLimits, RuntimeBindingPlan, RuntimeBindingPlanError, RuntimeBindingPlanIdentity,
    RuntimeBindingVersion, RuntimeByteRange, RuntimeConditionDefinition, RuntimeConditionHandle,
    RuntimeGuardDefinition, RuntimeNodeBindingDefinition, RuntimeNodeBindingKind,
    RuntimeOutputTraceDescriptor, RuntimePortDirection, RuntimeValueArea, RuntimeValueSlot,
    RuntimeValueType, StructuredBranchRange, StructuredCallHandle, StructuredEdgeDefinition,
    StructuredEdgeTarget, StructuredInstanceHandle, StructuredNodeDefinition, StructuredNodeKind,
    StructuredSubworkflowDefinition, WorkflowEdgeHandle, WorkflowEdgeRange, WorkflowNodeHandle,
};

#[derive(Debug)]
struct NoIoBackend;

impl RuntimeActionBackend for NoIoBackend {
    fn invoke_st_pou(
        _invocation: RuntimeActionHandle,
        _target_handle: u32,
        _context: &mut RuntimeBindingContext<'_, '_, '_, '_>,
    ) -> Result<(), FaultReason> {
        Ok(())
    }

    fn invoke_io_image(
        _invocation: RuntimeActionHandle,
        _target_handle: u32,
        _context: &mut RuntimeBindingContext<'_, '_, '_, '_>,
    ) -> Result<(), FaultReason> {
        Ok(())
    }

    fn stage_typed_command(
        _invocation: RuntimeActionHandle,
        _target_handle: u32,
        _context: &mut RuntimeBindingContext<'_, '_, '_, '_>,
    ) -> Result<(), FaultReason> {
        Ok(())
    }
}

fn node(raw: u32, kind: StructuredNodeKind, start: u32, count: u32) -> StructuredNodeDefinition {
    StructuredNodeDefinition {
        handle: WorkflowNodeHandle::new(raw).unwrap_or_else(|_| unreachable!("valid handle")),
        instance: StructuredInstanceHandle(0),
        kind,
        outgoing: WorkflowEdgeRange { start, count },
        cancellation_boundary: false,
    }
}

fn edge(raw: u32, source: u32) -> StructuredEdgeDefinition {
    StructuredEdgeDefinition {
        handle: WorkflowEdgeHandle::new(raw).unwrap_or_else(|_| unreachable!("valid handle")),
        source: WorkflowNodeHandle::new(source).unwrap_or_else(|_| unreachable!("valid handle")),
        target: StructuredEdgeTarget::Complete,
        branch: None,
        maximum_traversals_per_run: None,
    }
}

fn slot(
    area: RuntimeValueArea,
    offset_bytes: usize,
    value_type: RuntimeValueType,
) -> RuntimeValueSlot {
    RuntimeValueSlot {
        area,
        offset_bytes,
        value_type,
    }
}

struct Fixture {
    nodes: Vec<StructuredNodeDefinition>,
    edges: Vec<StructuredEdgeDefinition>,
    node_bindings: Vec<RuntimeNodeBindingDefinition>,
    actions: Vec<RuntimeActionDefinition>,
    ports: Vec<RuntimeActionPort>,
    conditions: Vec<RuntimeConditionDefinition>,
    guards: Vec<RuntimeGuardDefinition>,
}

fn fixture() -> Fixture {
    let nodes = vec![
        node(0, StructuredNodeKind::Action, 0, 1),
        node(1, StructuredNodeKind::Decision, 1, 2),
        node(
            2,
            StructuredNodeKind::WaitCondition {
                timeout_cycles: Some(3),
            },
            3,
            1,
        ),
    ];
    let edges = vec![edge(0, 0), edge(1, 1), edge(2, 1), edge(3, 2)];
    let node_bindings = vec![
        RuntimeNodeBindingDefinition {
            node: nodes[0].handle,
            kind: RuntimeNodeBindingKind::Action {
                action: RuntimeActionHandle(0),
                guard: Some(RuntimeConditionHandle(0)),
                success_edge: edges[0].handle,
            },
        },
        RuntimeNodeBindingDefinition {
            node: nodes[1].handle,
            kind: RuntimeNodeBindingKind::Decision {
                guards: BindingRange { start: 0, count: 2 },
            },
        },
        RuntimeNodeBindingDefinition {
            node: nodes[2].handle,
            kind: RuntimeNodeBindingKind::WaitCondition {
                condition: RuntimeConditionHandle(3),
            },
        },
    ];
    let actions = vec![RuntimeActionDefinition {
        handle: RuntimeActionHandle(0),
        version: RuntimeBindingVersion::V1_0,
        kind: RuntimeActionKind::StPou,
        target_handle: 7,
        invocation_state: RuntimeByteRange {
            start: 0,
            length: 0,
        },
        ports: BindingRange { start: 0, count: 1 },
    }];
    let ports = vec![RuntimeActionPort {
        port: 0,
        direction: RuntimePortDirection::Output,
        slot: slot(RuntimeValueArea::Output, 0, RuntimeValueType::Dint),
        output_trace: Some(RuntimeOutputTraceDescriptor {
            value_handle: 0,
            type_handle: 0,
        }),
    }];
    let conditions = (0..4)
        .map(|handle| RuntimeConditionDefinition {
            handle: RuntimeConditionHandle(handle),
            source: slot(
                RuntimeValueArea::State,
                handle as usize,
                RuntimeValueType::Bool,
            ),
        })
        .collect();
    let guards = vec![
        RuntimeGuardDefinition {
            edge: edges[1].handle,
            condition: RuntimeConditionHandle(1),
        },
        RuntimeGuardDefinition {
            edge: edges[2].handle,
            condition: RuntimeConditionHandle(2),
        },
    ];
    Fixture {
        nodes,
        edges,
        node_bindings,
        actions,
        ports,
        conditions,
        guards,
    }
}

fn limits() -> RuntimeBindingLimits {
    RuntimeBindingLimits {
        maximum_actions: 1,
        maximum_conditions: 4,
        maximum_ports_per_action: 1,
        maximum_guards_per_decision: 2,
    }
}

#[test]
fn exact_callback_closure_and_capacity_equality_are_accepted() {
    let fixture = fixture();
    let result = RuntimeBindingExecutor::<NoIoBackend>::from_untrusted_tables(
        &fixture.nodes,
        &fixture.edges,
        &fixture.node_bindings,
        &fixture.actions,
        &fixture.ports,
        &fixture.conditions,
        &fixture.guards,
        4,
        4,
        limits(),
    );
    assert!(result.is_ok());
}

#[test]
fn missing_duplicate_and_extra_entries_are_rejected() {
    let fixture = fixture();
    let build = |bindings: &[RuntimeNodeBindingDefinition], actions: &[RuntimeActionDefinition]| {
        RuntimeBindingExecutor::<NoIoBackend>::from_untrusted_tables(
            &fixture.nodes,
            &fixture.edges,
            bindings,
            actions,
            &fixture.ports,
            &fixture.conditions,
            &fixture.guards,
            4,
            4,
            limits(),
        )
        .map(|_| ())
    };
    assert_eq!(
        build(&fixture.node_bindings[..2], &fixture.actions),
        Err(RuntimeBindingPlanError::MissingOrExtraNodeBinding)
    );
    let original_bindings = fixture.node_bindings.clone();
    let mut duplicate_bindings = fixture.node_bindings.clone();
    duplicate_bindings[2].node = duplicate_bindings[1].node;
    assert_eq!(
        build(&duplicate_bindings, &fixture.actions),
        Err(RuntimeBindingPlanError::MissingOrExtraNodeBinding)
    );
    let mut extra_actions = fixture.actions.clone();
    extra_actions.push(RuntimeActionDefinition {
        handle: RuntimeActionHandle(1),
        ports: BindingRange { start: 1, count: 0 },
        ..fixture.actions[0]
    });
    assert_eq!(
        build(&original_bindings, &extra_actions),
        Err(RuntimeBindingPlanError::InvalidCapacity)
    );
}

#[test]
fn first_port_or_image_byte_beyond_the_boundary_is_rejected() {
    let mut fixture = fixture();
    fixture.ports[0].slot.offset_bytes = 1;
    let result = RuntimeBindingExecutor::<NoIoBackend>::from_untrusted_tables(
        &fixture.nodes,
        &fixture.edges,
        &fixture.node_bindings,
        &fixture.actions,
        &fixture.ports,
        &fixture.conditions,
        &fixture.guards,
        4,
        4,
        limits(),
    );
    assert_eq!(
        result.map(|_| ()),
        Err(RuntimeBindingPlanError::InvalidSlot)
    );
}

#[test]
fn output_trace_descriptors_reject_missing_extra_and_duplicate_handles() {
    let mut fixture = fixture();
    {
        let build = |ports: &[RuntimeActionPort]| {
            RuntimeBindingExecutor::<NoIoBackend>::from_untrusted_tables(
                &fixture.nodes,
                &fixture.edges,
                &fixture.node_bindings,
                &fixture.actions,
                ports,
                &fixture.conditions,
                &fixture.guards,
                4,
                8,
                limits(),
            )
            .map(|_| ())
        };

        let mut missing = fixture.ports.clone();
        missing[0].output_trace = None;
        assert_eq!(build(&missing), Err(RuntimeBindingPlanError::InvalidSlot));

        let mut extra = fixture.ports.clone();
        extra[0].direction = RuntimePortDirection::Input;
        assert_eq!(build(&extra), Err(RuntimeBindingPlanError::InvalidSlot));
    }

    fixture.actions[0].ports.count = 2;
    let mut duplicate = fixture.ports.clone();
    duplicate.push(RuntimeActionPort {
        port: 1,
        direction: RuntimePortDirection::Output,
        slot: slot(RuntimeValueArea::Output, 4, RuntimeValueType::Dint),
        output_trace: duplicate[0].output_trace,
    });
    let result = RuntimeBindingExecutor::<NoIoBackend>::from_untrusted_tables(
        &fixture.nodes,
        &fixture.edges,
        &fixture.node_bindings,
        &fixture.actions,
        &duplicate,
        &fixture.conditions,
        &fixture.guards,
        4,
        8,
        RuntimeBindingLimits {
            maximum_ports_per_action: 2,
            ..limits()
        },
    )
    .map(|_| ());
    assert_eq!(result, Err(RuntimeBindingPlanError::InvalidSlot));
}

#[test]
fn writable_port_ranges_accept_adjacency_and_reject_first_overlap() {
    let mut fixture = fixture();
    fixture.actions[0].ports.count = 2;
    fixture.ports.push(RuntimeActionPort {
        port: 1,
        direction: RuntimePortDirection::Output,
        slot: slot(RuntimeValueArea::Output, 4, RuntimeValueType::Dint),
        output_trace: Some(RuntimeOutputTraceDescriptor {
            value_handle: 1,
            type_handle: 0,
        }),
    });
    let limits = RuntimeBindingLimits {
        maximum_ports_per_action: 2,
        ..limits()
    };
    let build = |ports: &[RuntimeActionPort]| {
        RuntimeBindingExecutor::<NoIoBackend>::from_untrusted_tables(
            &fixture.nodes,
            &fixture.edges,
            &fixture.node_bindings,
            &fixture.actions,
            ports,
            &fixture.conditions,
            &fixture.guards,
            4,
            8,
            limits,
        )
    };
    assert_eq!(build(&fixture.ports).map(|_| ()), Ok(()));
    let mut overlapping = fixture.ports.clone();
    overlapping[1].slot.offset_bytes = 3;
    assert_eq!(
        build(&overlapping).map(|_| ()),
        Err(RuntimeBindingPlanError::InvalidSlot)
    );
}

#[test]
fn invocation_state_ranges_must_be_disjoint_and_in_bounds() {
    let mut fixture = fixture();
    fixture.actions[0].invocation_state = RuntimeByteRange {
        start: 4,
        length: 1,
    };
    fixture
        .nodes
        .push(node(3, StructuredNodeKind::Action, 4, 1));
    fixture.edges.push(edge(4, 3));
    fixture.node_bindings.push(RuntimeNodeBindingDefinition {
        node: fixture.nodes[3].handle,
        kind: RuntimeNodeBindingKind::Action {
            action: RuntimeActionHandle(1),
            guard: None,
            success_edge: fixture.edges[4].handle,
        },
    });
    fixture.actions.push(RuntimeActionDefinition {
        handle: RuntimeActionHandle(1),
        invocation_state: RuntimeByteRange {
            start: 5,
            length: 1,
        },
        ports: BindingRange { start: 1, count: 0 },
        ..fixture.actions[0]
    });
    let build = |actions: &[RuntimeActionDefinition]| {
        RuntimeBindingExecutor::<NoIoBackend>::from_untrusted_tables(
            &fixture.nodes,
            &fixture.edges,
            &fixture.node_bindings,
            actions,
            &fixture.ports,
            &fixture.conditions,
            &fixture.guards,
            6,
            4,
            RuntimeBindingLimits {
                maximum_actions: 2,
                ..limits()
            },
        )
    };
    assert_eq!(build(&fixture.actions).map(|_| ()), Ok(()));
    let mut overlapping = fixture.actions.clone();
    overlapping[1].invocation_state.start = 4;
    let result = build(&overlapping);
    assert_eq!(
        result.map(|_| ()),
        Err(RuntimeBindingPlanError::InvalidSlot)
    );
}

#[test]
fn invocation_state_rejects_alias_with_a_condition_or_port_slot() {
    let mut fixture = fixture();
    fixture.actions[0].invocation_state = RuntimeByteRange {
        start: 0,
        length: 1,
    };
    let result = RuntimeBindingExecutor::<NoIoBackend>::from_untrusted_tables(
        &fixture.nodes,
        &fixture.edges,
        &fixture.node_bindings,
        &fixture.actions,
        &fixture.ports,
        &fixture.conditions,
        &fixture.guards,
        4,
        4,
        limits(),
    );
    assert_eq!(
        result.map(|_| ()),
        Err(RuntimeBindingPlanError::InvalidSlot)
    );
}

#[test]
fn owned_plan_rejects_a_different_static_plan_identity() {
    let fixture = fixture();
    let mut nodes = fixture.nodes.clone();
    nodes.push(node(
        3,
        StructuredNodeKind::Subworkflow(StructuredCallHandle(0)),
        4,
        0,
    ));
    let identity = RuntimeBindingPlanIdentity([7; 32]);
    let calls = [StructuredSubworkflowDefinition {
        handle: StructuredCallHandle(0),
        node: nodes[3].handle,
        child_instance: StructuredInstanceHandle(1),
        initial_nodes: StructuredBranchRange { start: 0, count: 1 },
        input_copies: StructuredBranchRange { start: 0, count: 0 },
        output_copies: StructuredBranchRange { start: 0, count: 0 },
    }];
    let invalid = RuntimeBindingPlan::from_generated_tables(
        identity,
        &nodes,
        &fixture.edges,
        &[fixture.nodes[0].handle],
        &[BindingRange { start: 1, count: 1 }],
        &[fixture.nodes[1].handle],
        &calls,
        &[],
        &fixture.node_bindings,
        &fixture.actions,
        &fixture.ports,
        &fixture.conditions,
        &fixture.guards,
        4,
        4,
        limits(),
    );
    assert!(matches!(
        invalid,
        Err(RuntimeBindingPlanError::InvalidRange)
    ));
    let plan = RuntimeBindingPlan::from_generated_tables(
        identity,
        &nodes,
        &fixture.edges,
        &[fixture.nodes[0].handle],
        &[BindingRange { start: 0, count: 1 }],
        &[fixture.nodes[1].handle],
        &calls,
        &[],
        &fixture.node_bindings,
        &fixture.actions,
        &fixture.ports,
        &fixture.conditions,
        &fixture.guards,
        4,
        4,
        limits(),
    )
    .unwrap_or_else(|error| unreachable!("valid owned plan: {error}"));
    assert_eq!(plan.initial_active(), &[fixture.nodes[0].handle]);
    assert_eq!(
        plan.call_initial_nodes(StructuredCallHandle(0)),
        Some(&[fixture.nodes[1].handle][..])
    );
    assert_eq!(plan.call_initial_nodes(StructuredCallHandle(1)), None);
    let result =
        RuntimeBindingExecutor::<NoIoBackend>::from_plan(RuntimeBindingPlanIdentity([8; 32]), plan);
    assert_eq!(
        result.map(|_| ()),
        Err(RuntimeBindingPlanError::PlanIdentityMismatch)
    );
}
