//! Audited host-only lowering from Static Workflow Plan 1.1 to one owned runtime binding plan.

use std::collections::BTreeMap;

use aurora_workflow_cyclic::{
    BindingRange, RuntimeActionDefinition, RuntimeActionHandle, RuntimeActionKind,
    RuntimeActionPort, RuntimeBindingLimits, RuntimeBindingPlan, RuntimeBindingPlanError,
    RuntimeBindingPlanIdentity, RuntimeBindingVersion, RuntimeByteRange,
    RuntimeConditionDefinition, RuntimeConditionHandle, RuntimeGuardDefinition,
    RuntimeNodeBindingDefinition, RuntimeNodeBindingKind, RuntimeOutputTraceDescriptor,
    RuntimePortDirection, RuntimeValueArea, RuntimeValueSlot, RuntimeValueType,
    StructuredEdgeDefinition, StructuredInstanceHandle, StructuredNodeDefinition,
    StructuredNodeKind, WorkflowTraceWatchArea, WorkflowTraceWatchBinding,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    CanonicalWorkflowNode, CanonicalWorkflowNodeKind, STATIC_WORKFLOW_PLAN_MINOR,
    STATIC_WORKFLOW_PLAN_TRACED_MINOR, StableId, TaskBindingImageInput, WorkflowActionKind,
    WorkflowActionPortBinding, WorkflowPlanArtifacts, WorkflowPortDirection, WorkflowStepHandle,
    WorkflowTraceValueSource, WorkflowValueArea, WorkflowValueSlot, WorkflowValueType,
};

/// One indivisible runtime binding plan plus its exact watch table.
pub struct RuntimeTracedBindingPlan {
    /// Audited Action/condition/guard plan.
    pub binding_plan: RuntimeBindingPlan,
    /// Watch bindings sorted by global value handle.
    pub watches: Vec<WorkflowTraceWatchBinding>,
}

/// Host/runtime bridge rejected a non-exact or non-representable input.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuntimeBindingBridgeError {
    /// Typed artifacts, retained canonical bytes, or their digests disagree.
    #[error("Workflow artifact integrity audit failed")]
    ArtifactIntegrity,
    /// Requested task or one of its exact plan tables is missing, duplicated, extra, or swapped.
    #[error("runtime binding bridge input does not exactly match the Static Workflow Plan")]
    GenerationAudit,
    /// A host fixed-width value cannot be represented on the supported target.
    #[error("runtime binding bridge value is not representable")]
    NotRepresentable,
    /// The digest is not the canonical `sha256:<64 lowercase hex>` identity.
    #[error("Static Workflow Plan digest is not canonical")]
    InvalidPlanIdentity,
    /// Cyclic runtime rejected the generated owned plan.
    #[error(transparent)]
    Runtime(#[from] RuntimeBindingPlanError),
}

/// Generates and validates the only supported runtime binding input for one task.
///
/// `nodes` and `edges` are the already-lowered R2-04 structured tables for the same task. This
/// bridge verifies their callback-node shape against the Static Workflow Plan, derives every
/// Action/condition/guard entry from stable plan identities, and returns one owned plan whose
/// tables cannot subsequently be swapped or resized.
///
/// # Errors
/// Missing, extra, reordered, cross-task, capacity-invalid, or identity-invalid inputs are
/// rejected before a [`RuntimeBindingPlan`] is returned.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub fn build_runtime_binding_plan(
    artifacts: &WorkflowPlanArtifacts,
    task_handle: u32,
    nodes: &[StructuredNodeDefinition],
    edges: &[StructuredEdgeDefinition],
    image: TaskBindingImageInput,
    limits: RuntimeBindingLimits,
) -> Result<RuntimeBindingPlan, RuntimeBindingBridgeError> {
    if artifacts.static_plan.schema_version.minor != STATIC_WORKFLOW_PLAN_MINOR {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    Ok(
        build_runtime_binding_bundle(artifacts, task_handle, nodes, edges, image, limits)?
            .binding_plan,
    )
}

/// Builds the Static Plan 1.2 runtime binding plan and exact watch table.
///
/// # Errors
/// Rejects non-1.2 plans and any missing, extra, swapped, or inconsistent Trace descriptor.
#[allow(clippy::too_many_arguments)]
pub fn build_runtime_traced_binding_plan(
    artifacts: &WorkflowPlanArtifacts,
    task_handle: u32,
    nodes: &[StructuredNodeDefinition],
    edges: &[StructuredEdgeDefinition],
    image: TaskBindingImageInput,
    limits: RuntimeBindingLimits,
) -> Result<RuntimeTracedBindingPlan, RuntimeBindingBridgeError> {
    if artifacts.static_plan.schema_version.minor != STATIC_WORKFLOW_PLAN_TRACED_MINOR {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    build_runtime_binding_bundle(artifacts, task_handle, nodes, edges, image, limits)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn build_runtime_binding_bundle(
    artifacts: &WorkflowPlanArtifacts,
    task_handle: u32,
    nodes: &[StructuredNodeDefinition],
    edges: &[StructuredEdgeDefinition],
    image: TaskBindingImageInput,
    limits: RuntimeBindingLimits,
) -> Result<RuntimeTracedBindingPlan, RuntimeBindingBridgeError> {
    audit_artifact_integrity(artifacts)?;
    let plan_minor = artifacts.static_plan.schema_version.minor;
    if !matches!(
        plan_minor,
        STATIC_WORKFLOW_PLAN_MINOR | STATIC_WORKFLOW_PLAN_TRACED_MINOR
    ) {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    if image.task_handle != task_handle {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    let identity = RuntimeBindingPlanIdentity(parse_digest(&artifacts.plan_digest)?);
    let mut steps = artifacts
        .static_plan
        .steps
        .iter()
        .filter(|step| step.task_handle == task_handle)
        .collect::<Vec<_>>();
    steps.sort_by_key(|step| step.task_execution_order);
    if steps.len() != nodes.len() {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    let canonical_nodes = artifacts
        .canonical_ir
        .workflows
        .iter()
        .flat_map(|workflow| workflow.nodes.iter().map(|node| (node.handle, node)))
        .collect::<BTreeMap<_, _>>();
    let canonical_edges = artifacts
        .canonical_ir
        .workflows
        .iter()
        .flat_map(|workflow| workflow.edges.iter())
        .fold(BTreeMap::<_, Vec<_>>::new(), |mut map, edge| {
            map.entry(edge.source_node).or_default().push(edge);
            map
        });
    let resources = artifacts
        .static_plan
        .node_resources
        .iter()
        .map(|resource| (resource.step, resource))
        .collect::<BTreeMap<_, _>>();
    let traced = plan_minor == STATIC_WORKFLOW_PLAN_TRACED_MINOR;
    let mut trace_outputs = BTreeMap::new();
    let mut trace_watches = BTreeMap::new();
    if traced {
        for (index, descriptor) in artifacts.static_plan.trace_values.iter().enumerate() {
            if descriptor.handle.0 != to_u32(index)? || descriptor.type_handle == u32::MAX {
                return Err(RuntimeBindingBridgeError::GenerationAudit);
            }
            match descriptor.source {
                WorkflowTraceValueSource::Output { step, port } => {
                    if trace_outputs.insert((step, port), descriptor).is_some() {
                        return Err(RuntimeBindingBridgeError::GenerationAudit);
                    }
                }
                WorkflowTraceValueSource::Watch { watch } => {
                    if trace_watches.insert(watch, descriptor).is_some() {
                        return Err(RuntimeBindingBridgeError::GenerationAudit);
                    }
                }
            }
        }
    } else if !artifacts.static_plan.trace_values.is_empty() {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    let legacy_output_handles = legacy_output_handles(&artifacts.static_plan.node_resources)?;

    let task_conditions = artifacts
        .static_plan
        .condition_bindings
        .iter()
        .filter(|condition| condition.task_handle == task_handle)
        .collect::<Vec<_>>();
    let mut condition_handles = BTreeMap::new();
    let mut conditions = Vec::new();
    for (index, condition) in task_conditions.iter().enumerate() {
        let handle = RuntimeConditionHandle(to_u32(index)?);
        if condition_handles
            .insert((condition.instance, condition.condition_id), handle)
            .is_some()
        {
            return Err(RuntimeBindingBridgeError::GenerationAudit);
        }
        conditions.push(RuntimeConditionDefinition {
            handle,
            source: runtime_slot(condition.source)?,
        });
    }

    let mut actions = Vec::new();
    let mut ports = Vec::new();
    let mut guards = Vec::new();
    let mut node_bindings = Vec::new();
    let runtime_instances = artifacts
        .static_plan
        .instances
        .iter()
        .filter(|instance| instance.task_handle == task_handle)
        .enumerate()
        .map(|(local, instance)| Ok((instance.handle, StructuredInstanceHandle(to_u32(local)?))))
        .collect::<Result<BTreeMap<_, _>, RuntimeBindingBridgeError>>()?;
    let mut consumed_outputs = 0_usize;
    for (local_index, (step, node)) in steps.iter().zip(nodes).enumerate() {
        if step.task_execution_order != to_u32(local_index)?
            || node.handle.get() != to_u32(local_index)?
        {
            return Err(RuntimeBindingBridgeError::GenerationAudit);
        }
        if runtime_instances.get(&step.instance).copied() != Some(node.instance) {
            return Err(RuntimeBindingBridgeError::GenerationAudit);
        }
        let canonical = canonical_nodes
            .get(&step.node)
            .copied()
            .ok_or(RuntimeBindingBridgeError::GenerationAudit)?;
        validate_node_kind(canonical, node.kind)?;
        match canonical.node_kind {
            CanonicalWorkflowNodeKind::Action => {
                let resource = resources
                    .get(&step.handle)
                    .copied()
                    .ok_or(RuntimeBindingBridgeError::GenerationAudit)?;
                let binding = resource
                    .action_binding
                    .as_ref()
                    .ok_or(RuntimeBindingBridgeError::GenerationAudit)?;
                let action = RuntimeActionHandle(to_u32(actions.len())?);
                let port_start = to_u32(ports.len())?;
                for port in &binding.ports {
                    let output_trace = if port.direction == WorkflowPortDirection::Input {
                        if traced && trace_outputs.contains_key(&(step.handle, port.port)) {
                            return Err(RuntimeBindingBridgeError::GenerationAudit);
                        }
                        None
                    } else if traced {
                        let descriptor = trace_outputs
                            .get(&(step.handle, port.port))
                            .copied()
                            .ok_or(RuntimeBindingBridgeError::GenerationAudit)?;
                        validate_output_descriptor(descriptor, step, port)?;
                        consumed_outputs += 1;
                        Some(RuntimeOutputTraceDescriptor {
                            value_handle: descriptor.handle.0,
                            type_handle: descriptor.type_handle,
                        })
                    } else {
                        let value_handle = legacy_output_handles
                            .get(&(step.handle, port.port))
                            .copied()
                            .ok_or(RuntimeBindingBridgeError::GenerationAudit)?;
                        Some(RuntimeOutputTraceDescriptor {
                            value_handle,
                            type_handle: workflow_value_type_handle(port.slot.value_type),
                        })
                    };
                    ports.push(runtime_port(port, output_trace)?);
                }
                actions.push(RuntimeActionDefinition {
                    handle: action,
                    version: RuntimeBindingVersion {
                        major: binding.version.major,
                        minor: binding.version.minor,
                    },
                    kind: runtime_action_kind(binding.kind),
                    target_handle: binding.target_handle,
                    invocation_state: RuntimeByteRange {
                        start: to_usize(binding.invocation_state_offset_bytes)?,
                        length: to_usize(binding.committed_state_bytes)?,
                    },
                    ports: BindingRange {
                        start: port_start,
                        count: to_u32(binding.ports.len())?,
                    },
                });
                let outgoing = one_action_edge(canonical, &canonical_edges)?;
                let runtime_edge = one_runtime_edge(node, edges)?;
                let guard = outgoing
                    .condition_id
                    .map(|condition_id| {
                        condition_handle(&condition_handles, step.instance, condition_id)
                    })
                    .transpose()?;
                node_bindings.push(RuntimeNodeBindingDefinition {
                    node: node.handle,
                    kind: RuntimeNodeBindingKind::Action {
                        action,
                        guard,
                        success_edge: runtime_edge.handle,
                    },
                });
            }
            CanonicalWorkflowNodeKind::Decision => {
                let mut outgoing = canonical_edges
                    .get(&canonical.handle)
                    .cloned()
                    .unwrap_or_default();
                outgoing.sort_by_key(|edge| edge.priority);
                let range = runtime_edge_range(node, edges)?;
                if outgoing.len() != range.len() {
                    return Err(RuntimeBindingBridgeError::GenerationAudit);
                }
                let start = to_u32(guards.len())?;
                for (host_edge, runtime_edge) in outgoing.into_iter().zip(&edges[range]) {
                    let condition_id = host_edge
                        .condition_id
                        .ok_or(RuntimeBindingBridgeError::GenerationAudit)?;
                    guards.push(RuntimeGuardDefinition {
                        edge: runtime_edge.handle,
                        condition: condition_handle(
                            &condition_handles,
                            step.instance,
                            condition_id,
                        )?,
                    });
                }
                node_bindings.push(RuntimeNodeBindingDefinition {
                    node: node.handle,
                    kind: RuntimeNodeBindingKind::Decision {
                        guards: BindingRange {
                            start,
                            count: to_u32(guards.len())? - start,
                        },
                    },
                });
            }
            CanonicalWorkflowNodeKind::WaitCondition { condition_id, .. } => {
                node_bindings.push(RuntimeNodeBindingDefinition {
                    node: node.handle,
                    kind: RuntimeNodeBindingKind::WaitCondition {
                        condition: condition_handle(
                            &condition_handles,
                            step.instance,
                            condition_id,
                        )?,
                    },
                });
            }
            _ => {}
        }
    }
    let expected_resources = steps
        .iter()
        .filter(|step| {
            canonical_nodes.get(&step.node).is_some_and(|node| {
                matches!(
                    node.node_kind,
                    CanonicalWorkflowNodeKind::Action
                        | CanonicalWorkflowNodeKind::Subworkflow { .. }
                )
            })
        })
        .count();
    let actual_resources = artifacts
        .static_plan
        .node_resources
        .iter()
        .filter(|resource| steps.iter().any(|step| step.handle == resource.step))
        .count();
    if actual_resources != expected_resources {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    if traced {
        let task_outputs = trace_outputs
            .iter()
            .filter(|(_, descriptor)| descriptor.task_handle == task_handle)
            .collect::<Vec<_>>();
        if consumed_outputs != task_outputs.len()
            || task_outputs
                .iter()
                .any(|((step, _), _)| steps.iter().all(|candidate| candidate.handle != *step))
        {
            return Err(RuntimeBindingBridgeError::GenerationAudit);
        }
    }
    let watches = if traced {
        build_runtime_watches(
            artifacts,
            task_handle,
            image,
            &runtime_instances,
            &trace_watches,
        )?
    } else {
        Vec::new()
    };
    let binding_plan = RuntimeBindingPlan::from_generated_tables(
        identity,
        nodes,
        edges,
        &node_bindings,
        &actions,
        &ports,
        &conditions,
        &guards,
        to_usize(image.application_state_bytes)?,
        to_usize(image.output_bytes)?,
        limits,
    )
    .map_err(RuntimeBindingBridgeError::from)?;
    Ok(RuntimeTracedBindingPlan {
        binding_plan,
        watches,
    })
}

fn legacy_output_handles(
    resources: &[crate::PlannedNodeResources],
) -> Result<BTreeMap<(WorkflowStepHandle, u32), u32>, RuntimeBindingBridgeError> {
    let mut keys = resources
        .iter()
        .filter_map(|resource| {
            resource
                .action_binding
                .as_ref()
                .map(|binding| (resource.step, binding))
        })
        .flat_map(|(step, binding)| {
            binding
                .ports
                .iter()
                .filter(|port| port.direction != WorkflowPortDirection::Input)
                .map(move |port| (step, port.port))
        })
        .collect::<Vec<_>>();
    keys.sort_unstable();
    let mut handles = BTreeMap::new();
    for (index, key) in keys.into_iter().enumerate() {
        if handles.insert(key, to_u32(index)?).is_some() {
            return Err(RuntimeBindingBridgeError::GenerationAudit);
        }
    }
    Ok(handles)
}

fn validate_output_descriptor(
    descriptor: &crate::PlannedTraceValue,
    step: &crate::WorkflowPlanStep,
    port: &WorkflowActionPortBinding,
) -> Result<(), RuntimeBindingBridgeError> {
    let expected_fragments = port.slot.value_type.size_bytes().div_ceil(32);
    if descriptor.task_handle != step.task_handle
        || descriptor.instance != step.instance
        || descriptor.value_id != port.slot.target_id
        || descriptor.type_handle != workflow_value_type_handle(port.slot.value_type)
        || descriptor.area != port.slot.area
        || descriptor.image_offset_bytes != port.slot.image_offset_bytes
        || descriptor.encoded_bytes != port.slot.value_type.size_bytes()
        || u64::from(descriptor.fragment_count) != expected_fragments
    {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    Ok(())
}

fn build_runtime_watches(
    artifacts: &WorkflowPlanArtifacts,
    task_handle: u32,
    image: TaskBindingImageInput,
    runtime_instances: &BTreeMap<crate::WorkflowInstanceHandle, StructuredInstanceHandle>,
    trace_watches: &BTreeMap<crate::WorkflowWatchHandle, &crate::PlannedTraceValue>,
) -> Result<Vec<WorkflowTraceWatchBinding>, RuntimeBindingBridgeError> {
    let planned = artifacts
        .static_plan
        .watches
        .iter()
        .filter(|watch| watch.task_handle == task_handle)
        .map(|watch| (watch.handle, watch))
        .collect::<BTreeMap<_, _>>();
    let actual = trace_watches
        .iter()
        .filter(|(_, descriptor)| descriptor.task_handle == task_handle)
        .map(|(handle, descriptor)| (*handle, *descriptor))
        .collect::<BTreeMap<_, _>>();
    if planned.len() != actual.len()
        || planned.keys().copied().collect::<Vec<_>>() != actual.keys().copied().collect::<Vec<_>>()
    {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    let mut bindings = Vec::with_capacity(planned.len());
    for (handle, watch) in planned {
        let descriptor = actual[&handle];
        let end = descriptor
            .image_offset_bytes
            .checked_add(descriptor.encoded_bytes)
            .ok_or(RuntimeBindingBridgeError::GenerationAudit)?;
        let capacity = match descriptor.area {
            WorkflowValueArea::State => image.application_state_bytes,
            WorkflowValueArea::Output => image.output_bytes,
        };
        if descriptor.value_id != watch.value_id
            || descriptor.encoded_bytes != watch.encoded_bytes
            || descriptor.fragment_count != watch.fragment_count
            || descriptor.type_handle == u32::MAX
            || descriptor.encoded_bytes == 0
            || end > capacity
        {
            return Err(RuntimeBindingBridgeError::GenerationAudit);
        }
        let workflow_instance = runtime_instances
            .get(&descriptor.instance)
            .copied()
            .ok_or(RuntimeBindingBridgeError::GenerationAudit)?;
        bindings.push(WorkflowTraceWatchBinding {
            workflow_instance,
            value_handle: descriptor.handle.0,
            type_handle: descriptor.type_handle,
            area: match descriptor.area {
                WorkflowValueArea::State => WorkflowTraceWatchArea::State,
                WorkflowValueArea::Output => WorkflowTraceWatchArea::Output,
            },
            offset: to_usize(descriptor.image_offset_bytes)?,
            byte_count: to_usize(descriptor.encoded_bytes)?,
        });
    }
    bindings.sort_by_key(|binding| binding.value_handle);
    Ok(bindings)
}

fn audit_artifact_integrity(
    artifacts: &WorkflowPlanArtifacts,
) -> Result<(), RuntimeBindingBridgeError> {
    let canonical_ir_json = serde_jcs::to_vec(&artifacts.canonical_ir)
        .map_err(|_| RuntimeBindingBridgeError::ArtifactIntegrity)?;
    let static_plan_json = serde_jcs::to_vec(&artifacts.static_plan)
        .map_err(|_| RuntimeBindingBridgeError::ArtifactIntegrity)?;
    if canonical_ir_json != artifacts.canonical_ir_json
        || static_plan_json != artifacts.static_plan_json
        || digest(&canonical_ir_json) != artifacts.semantic_digest
        || digest(&static_plan_json) != artifacts.plan_digest
        || artifacts.static_plan.semantic_digest != artifacts.semantic_digest
    {
        return Err(RuntimeBindingBridgeError::ArtifactIntegrity);
    }
    Ok(())
}

fn digest(bytes: &[u8]) -> String {
    let value = Sha256::digest(bytes);
    let mut output = String::with_capacity(7 + value.len() * 2);
    output.push_str("sha256:");
    for byte in value {
        use std::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn validate_node_kind(
    host: &CanonicalWorkflowNode,
    runtime: StructuredNodeKind,
) -> Result<(), RuntimeBindingBridgeError> {
    let matches = matches!(
        (&host.node_kind, runtime),
        (
            CanonicalWorkflowNodeKind::Action,
            StructuredNodeKind::Action
        ) | (
            CanonicalWorkflowNodeKind::Decision,
            StructuredNodeKind::Decision
        ) | (
            CanonicalWorkflowNodeKind::WaitCondition { .. },
            StructuredNodeKind::WaitCondition { .. }
        )
    );
    let host_callback = matches!(
        host.node_kind,
        CanonicalWorkflowNodeKind::Action
            | CanonicalWorkflowNodeKind::Decision
            | CanonicalWorkflowNodeKind::WaitCondition { .. }
    );
    let runtime_callback = matches!(
        runtime,
        StructuredNodeKind::Action
            | StructuredNodeKind::Decision
            | StructuredNodeKind::WaitCondition { .. }
    );
    if (host_callback || runtime_callback) && !matches {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    Ok(())
}

fn one_action_edge<'a>(
    node: &CanonicalWorkflowNode,
    edges: &'a BTreeMap<crate::WorkflowNodeHandle, Vec<&'a crate::CanonicalWorkflowEdge>>,
) -> Result<&'a crate::CanonicalWorkflowEdge, RuntimeBindingBridgeError> {
    let outgoing = edges
        .get(&node.handle)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if outgoing.len() != 1 {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    Ok(outgoing[0])
}

fn one_runtime_edge<'a>(
    node: &StructuredNodeDefinition,
    edges: &'a [StructuredEdgeDefinition],
) -> Result<&'a StructuredEdgeDefinition, RuntimeBindingBridgeError> {
    let range = runtime_edge_range(node, edges)?;
    if range.len() != 1 {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    Ok(&edges[range.start])
}

fn runtime_edge_range(
    node: &StructuredNodeDefinition,
    edges: &[StructuredEdgeDefinition],
) -> Result<std::ops::Range<usize>, RuntimeBindingBridgeError> {
    let start = usize::try_from(node.outgoing.start)
        .map_err(|_| RuntimeBindingBridgeError::NotRepresentable)?;
    let count = usize::try_from(node.outgoing.count)
        .map_err(|_| RuntimeBindingBridgeError::NotRepresentable)?;
    let end = start
        .checked_add(count)
        .filter(|end| *end <= edges.len())
        .ok_or(RuntimeBindingBridgeError::GenerationAudit)?;
    if edges[start..end]
        .iter()
        .any(|edge| edge.source != node.handle)
    {
        return Err(RuntimeBindingBridgeError::GenerationAudit);
    }
    Ok(start..end)
}

fn condition_handle(
    handles: &BTreeMap<(crate::WorkflowInstanceHandle, StableId), RuntimeConditionHandle>,
    instance: crate::WorkflowInstanceHandle,
    condition: StableId,
) -> Result<RuntimeConditionHandle, RuntimeBindingBridgeError> {
    handles
        .get(&(instance, condition))
        .copied()
        .ok_or(RuntimeBindingBridgeError::GenerationAudit)
}

const fn runtime_action_kind(kind: WorkflowActionKind) -> RuntimeActionKind {
    match kind {
        WorkflowActionKind::StPou => RuntimeActionKind::StPou,
        WorkflowActionKind::IoImage => RuntimeActionKind::IoImage,
        WorkflowActionKind::TypedCommand => RuntimeActionKind::TypedCommand,
    }
}

fn runtime_port(
    port: &WorkflowActionPortBinding,
    output_trace: Option<RuntimeOutputTraceDescriptor>,
) -> Result<RuntimeActionPort, RuntimeBindingBridgeError> {
    Ok(RuntimeActionPort {
        port: port.port,
        direction: match port.direction {
            WorkflowPortDirection::Input => RuntimePortDirection::Input,
            WorkflowPortDirection::Output => RuntimePortDirection::Output,
            WorkflowPortDirection::InOut => RuntimePortDirection::InOut,
        },
        slot: runtime_slot(port.slot)?,
        output_trace,
    })
}

const fn workflow_value_type_handle(value_type: WorkflowValueType) -> u32 {
    match value_type {
        WorkflowValueType::Bool => 1,
        WorkflowValueType::Sint => 2,
        WorkflowValueType::Int => 3,
        WorkflowValueType::Dint => 4,
        WorkflowValueType::Lint => 5,
        WorkflowValueType::Usint => 6,
        WorkflowValueType::Uint => 7,
        WorkflowValueType::Udint => 8,
        WorkflowValueType::Ulint => 9,
        WorkflowValueType::Real => 10,
        WorkflowValueType::Lreal => 11,
    }
}

fn runtime_slot(slot: WorkflowValueSlot) -> Result<RuntimeValueSlot, RuntimeBindingBridgeError> {
    Ok(RuntimeValueSlot {
        area: match slot.area {
            WorkflowValueArea::State => RuntimeValueArea::State,
            WorkflowValueArea::Output => RuntimeValueArea::Output,
        },
        offset_bytes: to_usize(slot.image_offset_bytes)?,
        value_type: match slot.value_type {
            WorkflowValueType::Bool => RuntimeValueType::Bool,
            WorkflowValueType::Sint => RuntimeValueType::Sint,
            WorkflowValueType::Int => RuntimeValueType::Int,
            WorkflowValueType::Dint => RuntimeValueType::Dint,
            WorkflowValueType::Lint => RuntimeValueType::Lint,
            WorkflowValueType::Usint => RuntimeValueType::Usint,
            WorkflowValueType::Uint => RuntimeValueType::Uint,
            WorkflowValueType::Udint => RuntimeValueType::Udint,
            WorkflowValueType::Ulint => RuntimeValueType::Ulint,
            WorkflowValueType::Real => RuntimeValueType::Real,
            WorkflowValueType::Lreal => RuntimeValueType::Lreal,
        },
    })
}

fn parse_digest(value: &str) -> Result<[u8; 32], RuntimeBindingBridgeError> {
    let hex = value
        .strip_prefix("sha256:")
        .filter(|hex| hex.len() == 64)
        .ok_or(RuntimeBindingBridgeError::InvalidPlanIdentity)?;
    let mut bytes = [0_u8; 32];
    let (pairs, remainder) = hex.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return Err(RuntimeBindingBridgeError::InvalidPlanIdentity);
    }
    for (index, pair) in pairs.iter().enumerate() {
        let high = hex_digit(pair[0]).ok_or(RuntimeBindingBridgeError::InvalidPlanIdentity)?;
        let low = hex_digit(pair[1]).ok_or(RuntimeBindingBridgeError::InvalidPlanIdentity)?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

const fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn to_u32(value: usize) -> Result<u32, RuntimeBindingBridgeError> {
    u32::try_from(value).map_err(|_| RuntimeBindingBridgeError::NotRepresentable)
}

fn to_usize(value: u64) -> Result<usize, RuntimeBindingBridgeError> {
    usize::try_from(value).map_err(|_| RuntimeBindingBridgeError::NotRepresentable)
}
