//! Host-only R2 Workflow Trace decode、compare 与 replay。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use aurora_control_contracts::{
    MissOutcome, WORKFLOW_TRACE_FILE_HEADER_SIZE, WORKFLOW_TRACE_RECORD_SIZE,
    WorkflowTraceEventKind, WorkflowTraceFileView, WorkflowTraceRecord,
};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{BuildError, BuildResult};

/// 解码并验证一个 Workflow Trace 文件。
pub(crate) fn decode_file(path: &Path) -> BuildResult<String> {
    let bytes = read(path)?;
    decode_bytes(path, &bytes)
}

/// 完整验证两份 Workflow Trace 后按 header 与 record bytes 精确比较。
pub(crate) fn compare_files(expected_path: &Path, actual_path: &Path) -> BuildResult<String> {
    let expected = read(expected_path)?;
    let actual = read(actual_path)?;
    compare_bytes(expected_path, &expected, actual_path, &actual)
}

/// 验证 Trace 与配对静态计划并输出离线 replay 时间线；gap/drop 不补造事件。
pub(crate) fn replay_file(path: &Path, plan_path: &Path) -> BuildResult<String> {
    let bytes = read(path)?;
    let plan_bytes = read_plan(plan_path)?;
    replay_bytes(path, &bytes, plan_path, &plan_bytes)
}

fn decode_bytes(path: &Path, bytes: &[u8]) -> BuildResult<String> {
    let view = parse(path, bytes)?;
    let header = view.header();
    let completeness = view.completeness();
    let mut output = format!(
        "engine_epoch={} plan_digest={} records={} dropped={} gaps={} status={}",
        hex(&header.engine_epoch().to_bytes()),
        hex(&header.plan_digest()),
        header.record_count(),
        header.dropped_records(),
        completeness.observed_sequence_gaps,
        status(completeness.is_complete()),
    );
    for (index, record) in view.records().enumerate() {
        let record = record.map_err(|source| workflow_error(path, source))?;
        write!(output, "\n{index}: {}", format_record(record)).map_err(|_| {
            BuildError::Validation("cannot format decoded Workflow Trace record".to_owned())
        })?;
    }
    Ok(output)
}

fn compare_bytes(
    expected_path: &Path,
    expected_bytes: &[u8],
    actual_path: &Path,
    actual_bytes: &[u8],
) -> BuildResult<String> {
    let expected = parse(expected_path, expected_bytes)?;
    let actual = parse(actual_path, actual_bytes)?;
    if expected.header() != actual.header() {
        return Err(BuildError::Validation(format!(
            "Workflow Trace header mismatch: expected {:?}, actual {:?}",
            expected.header(),
            actual.header()
        )));
    }
    let count = usize::try_from(expected.header().record_count()).map_err(|_| {
        BuildError::Validation("Workflow Trace record count is not representable".to_owned())
    })?;
    for index in 0..count {
        let start = WORKFLOW_TRACE_FILE_HEADER_SIZE + index * WORKFLOW_TRACE_RECORD_SIZE;
        let end = start + WORKFLOW_TRACE_RECORD_SIZE;
        if expected_bytes[start..end] != actual_bytes[start..end] {
            return Err(BuildError::Validation(format!(
                "Workflow Trace record {index} bytes differ"
            )));
        }
    }
    Ok(format!(
        "Workflow Trace files match for {count} records ({})",
        status(expected.completeness().is_complete())
    ))
}

fn replay_bytes(
    path: &Path,
    bytes: &[u8],
    plan_path: &Path,
    plan_bytes: &[u8],
) -> BuildResult<String> {
    let view = parse(path, bytes)?;
    let plan = StaticPlanIndex::parse(plan_path, plan_bytes)?;
    let actual_digest: [u8; 32] = Sha256::digest(plan_bytes).into();
    if view.header().plan_digest() != actual_digest {
        return Err(BuildError::Validation(format!(
            "Workflow Trace PlanDigest does not match `{}`",
            plan_path.display()
        )));
    }
    let mut releases = 0_u64;
    let mut committed = 0_u64;
    let mut discarded = 0_u64;
    let mut output_events = 0_u64;
    let mut timeline = String::new();
    let records = view
        .records()
        .map(|record| record.map_err(|source| workflow_error(path, source)))
        .collect::<BuildResult<Vec<_>>>()?;
    for record in &records {
        let record = *record;
        plan.validate_record(record)?;
        match record.kind() {
            WorkflowTraceEventKind::ScanCommitted => {
                releases = releases.saturating_add(1);
                committed = committed.saturating_add(1);
            }
            WorkflowTraceEventKind::ScanDiscarded => {
                releases = releases.saturating_add(1);
                discarded = discarded.saturating_add(1);
            }
            WorkflowTraceEventKind::OutputStaged => {
                output_events = output_events.saturating_add(1);
            }
            _ => {}
        }
        write!(timeline, "\n{}", format_replay_record(record)).map_err(|_| {
            BuildError::Validation("cannot format Workflow Trace replay".to_owned())
        })?;
    }
    let completeness = view.completeness();
    if completeness.is_complete() {
        plan.validate_complete_release_closure(&records)?;
    }
    let traceability = if !completeness.is_complete() {
        "incomplete"
    } else if plan.minor == 3 {
        "traceable"
    } else {
        "unverified"
    };
    Ok(format!(
        "status={} traceability={} records={} releases={} committed={} discarded={} output_events={} dropped={} gaps={}{}",
        status(completeness.is_complete()),
        traceability,
        view.header().record_count(),
        releases,
        committed,
        discarded,
        output_events,
        completeness.dropped_records,
        completeness.observed_sequence_gaps,
        timeline,
    ))
}

fn format_replay_record(record: WorkflowTraceRecord) -> String {
    match record.kind() {
        WorkflowTraceEventKind::NodeExecuted => format!(
            "release task={} epoch={} release={} node={}:{} execution={}",
            record.task_handle().get(),
            record.task_epoch().get(),
            record.release_sequence().get(),
            record.workflow_instance_handle(),
            record.node_handle().unwrap_or(u32::MAX),
            record.execution_order().unwrap_or(u32::MAX),
        ),
        WorkflowTraceEventKind::TransitionTaken | WorkflowTraceEventKind::ForkActivated => format!(
            "control task={} release={} node={}:{} edge={} branch={:?}",
            record.task_handle().get(),
            record.release_sequence().get(),
            record.workflow_instance_handle(),
            record.node_handle().unwrap_or(u32::MAX),
            record.edge_handle().unwrap_or(u32::MAX),
            record.branch_order(),
        ),
        WorkflowTraceEventKind::OutputStaged => format!(
            "output task={} release={} node={}:{} source={} value={} type={} fragment={}/{}",
            record.task_handle().get(),
            record.release_sequence().get(),
            record.workflow_instance_handle(),
            record.node_handle().unwrap_or(u32::MAX),
            record.source_handle().unwrap_or(u32::MAX),
            record.value_handle().unwrap_or(u32::MAX),
            record.type_handle().unwrap_or(u32::MAX),
            record.fragment().index,
            record.fragment().count,
        ),
        WorkflowTraceEventKind::ScanCommitted | WorkflowTraceEventKind::ScanDiscarded => format!(
            "terminal task={} release={} kind={:?} commit={}->{}",
            record.task_handle().get(),
            record.release_sequence().get(),
            record.kind(),
            record.commit_before().get(),
            record.commit_after().get(),
        ),
        _ => format_record(record),
    }
}

#[derive(Debug, Default)]
struct TaskPlanIndex {
    instances: u32,
    instance_parents: Vec<Option<u32>>,
    node_instances: Vec<u32>,
    node_metadata: Vec<Option<TraceNodeIndex>>,
    edge_instances: Vec<u32>,
    edge_sources: Vec<u32>,
    edge_metadata: Vec<Option<TraceEdgeIndex>>,
    initial_active: BTreeSet<u32>,
    instance_initial_active: BTreeMap<u32, u32>,
    root_instances: BTreeSet<u32>,
    sources: BTreeSet<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraceJoinPolicyIndex {
    CancelOthers,
    KeepRunning,
    WaitAtBoundary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TraceNodeKindIndex {
    Action,
    Decision,
    Fork {
        branch_orders: BTreeSet<u32>,
    },
    Merge,
    JoinAll {
        branch_orders: BTreeSet<u32>,
    },
    JoinAny {
        loser_policy: TraceJoinPolicyIndex,
        branch_orders: BTreeSet<u32>,
    },
    WaitCycles,
    WaitCondition {
        has_timeout: bool,
    },
    Subworkflow {
        call_handle: u32,
        child_instance: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TraceNodeIndex {
    kind: TraceNodeKindIndex,
    cancellation_boundary: bool,
    cancellation_branch_orders: BTreeSet<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TraceEdgeIndex {
    source: u32,
    instance: u32,
    target_complete: bool,
    target_node: Option<u32>,
    branch_order: Option<u32>,
}

#[derive(Debug)]
enum TraceValueSourceIndex {
    Output { node: u32, action: u32, port: u32 },
    Watch,
}

#[derive(Debug)]
struct TraceValueIndex {
    task: u32,
    instance: u32,
    type_handle: u32,
    encoded_bytes: u64,
    fragment_count: u16,
    source: TraceValueSourceIndex,
}

#[derive(Debug)]
struct ExpectedOutput {
    task: u32,
    instance: u32,
    node: u32,
    action: u32,
    value_id: String,
    type_handle: u32,
    area: String,
    image_offset_bytes: u64,
    encoded_bytes: u64,
}

#[derive(Debug)]
struct ExpectedWatch {
    task: u32,
    value_id: String,
    encoded_bytes: u64,
    fragment_count: u16,
}

#[derive(Debug)]
struct StaticPlanIndex {
    minor: u32,
    tasks: BTreeMap<u32, TaskPlanIndex>,
    trace_values: Vec<TraceValueIndex>,
    output_values: BTreeMap<(u32, u32), BTreeSet<u32>>,
    watch_values: BTreeMap<u32, BTreeSet<u32>>,
}

#[derive(Debug)]
struct ReleaseTraceClosure {
    key: (u32, u64, u64),
    executed_nodes: BTreeSet<u32>,
    faulted_nodes: BTreeSet<u32>,
    transitions: BTreeSet<(u32, u32)>,
    fork_activations: BTreeSet<(u32, u32, u32)>,
    joins: BTreeMap<u32, (u16, Option<u32>)>,
    waits: BTreeMap<u32, u16>,
    cancel_requests: BTreeSet<(u32, u32, u16)>,
    cancel_applications: BTreeSet<(u32, u32, u16)>,
    subworkflow_activations: BTreeMap<u32, (u32, u32)>,
    subworkflow_completions: BTreeMap<u32, (u32, u32)>,
    completion_requests: BTreeSet<u32>,
    initialized_roots: BTreeSet<u32>,
    completed_roots: BTreeSet<u32>,
    committed: bool,
    faulted: bool,
    deadline_discarded: bool,
    output_values: BTreeSet<u32>,
    watch_values: BTreeSet<u32>,
}

#[derive(Debug, Clone, Default)]
struct ActiveSetExpectation {
    required: BTreeSet<u32>,
    allowed: BTreeSet<u32>,
}

impl ReleaseTraceClosure {
    fn new(record: WorkflowTraceRecord) -> Self {
        Self {
            key: release_key(record),
            executed_nodes: BTreeSet::new(),
            faulted_nodes: BTreeSet::new(),
            transitions: BTreeSet::new(),
            fork_activations: BTreeSet::new(),
            joins: BTreeMap::new(),
            waits: BTreeMap::new(),
            cancel_requests: BTreeSet::new(),
            cancel_applications: BTreeSet::new(),
            subworkflow_activations: BTreeMap::new(),
            subworkflow_completions: BTreeMap::new(),
            completion_requests: BTreeSet::new(),
            initialized_roots: BTreeSet::new(),
            completed_roots: BTreeSet::new(),
            committed: false,
            faulted: false,
            deadline_discarded: false,
            output_values: BTreeSet::new(),
            watch_values: BTreeSet::new(),
        }
    }
}

impl StaticPlanIndex {
    #[allow(
        clippy::too_many_lines,
        reason = "单次按引用顺序审计 plan 稠密表，避免先接受部分或交叉任务 handle"
    )]
    fn parse(path: &Path, bytes: &[u8]) -> BuildResult<Self> {
        let value: Value = serde_json::from_slice(bytes).map_err(|source| BuildError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        let root = object(&value, "Static Workflow Plan root")?;
        let version = object(
            root.get("schema_version").ok_or_else(|| {
                BuildError::Validation(
                    "Static Workflow Plan `schema_version` is required".to_owned(),
                )
            })?,
            "Static Workflow Plan schema_version",
        )?;
        let minor = u32_field(version, "minor")?;
        if u32_field(version, "major")? != 1 || !matches!(minor, 1..=3) {
            return validation("Static Workflow Plan schema version must be 1.1, 1.2, or 1.3");
        }
        let instances = array_field(root, "instances")?;
        let steps = array_field(root, "steps")?;
        let edges = array_field(root, "edges")?;
        let resources = array_field(root, "node_resources")?;
        let watches = array_field(root, "watches")?;

        let mut tasks = BTreeMap::<u32, TaskPlanIndex>::new();
        let mut instance_tasks = Vec::with_capacity(instances.len());
        let mut instance_locals = Vec::with_capacity(instances.len());
        for (expected, value) in instances.iter().enumerate() {
            let item = object(value, "instances[]")?;
            require_dense_handle(item, expected, "instances")?;
            let task = u32_field(item, "task_handle")?;
            let entry = tasks.entry(task).or_default();
            instance_locals.push(entry.instances);
            entry.instances = entry.instances.checked_add(1).ok_or_else(|| {
                BuildError::Validation("Static Workflow Plan instance count overflow".to_owned())
            })?;
            instance_tasks.push(task);
        }
        for task in tasks.values_mut() {
            task.instance_parents.resize(
                usize::try_from(task.instances).map_err(|_| {
                    BuildError::Validation(
                        "Static Workflow Plan instance count is not representable".to_owned(),
                    )
                })?,
                None,
            );
        }

        let mut step_tasks = Vec::with_capacity(steps.len());
        let mut step_instances = Vec::with_capacity(steps.len());
        let mut step_nodes = Vec::with_capacity(steps.len());
        let mut step_children = Vec::with_capacity(steps.len());
        let mut child_instances = BTreeSet::new();
        let mut next_execution = BTreeMap::<u32, u32>::new();
        for (expected, value) in steps.iter().enumerate() {
            let item = object(value, "steps[]")?;
            require_dense_handle(item, expected, "steps")?;
            let task = u32_field(item, "task_handle")?;
            let instance = index_field(item, "instance", instance_tasks.len())?;
            if instance_tasks[instance] != task {
                return validation("Static Workflow Plan step crosses task ownership");
            }
            let execution = u32_field(item, "task_execution_order")?;
            let next = next_execution.entry(task).or_default();
            if execution != *next {
                return validation("Static Workflow Plan task execution order is not dense");
            }
            *next = next.checked_add(1).ok_or_else(|| {
                BuildError::Validation("Static Workflow Plan node count overflow".to_owned())
            })?;
            let task_index = tasks.get_mut(&task).ok_or_else(|| {
                BuildError::Validation("Static Workflow Plan step has no task instance".to_owned())
            })?;
            let local_instance = *instance_locals.get(instance).ok_or_else(|| {
                BuildError::Validation(
                    "Static Workflow Plan step instance is not representable".to_owned(),
                )
            })?;
            if task_index.node_instances.len() != usize::try_from(execution).unwrap_or(usize::MAX) {
                return validation("Static Workflow Plan task node table is not dense");
            }
            task_index.node_instances.push(local_instance);
            let child = item
                .get("child_instance")
                .map(|_| index_field(item, "child_instance", instance_tasks.len()))
                .transpose()?;
            if child.is_some_and(|child| instance_tasks[child] != task)
                || child.is_some_and(|child| !child_instances.insert(child))
            {
                return validation("Static Workflow Plan child instance ownership is invalid");
            }
            if let Some(child) = child {
                let child_local = *instance_locals.get(child).ok_or_else(|| {
                    BuildError::Validation(
                        "Static Workflow Plan child instance is not representable".to_owned(),
                    )
                })?;
                let parent = task_index
                    .instance_parents
                    .get_mut(usize::try_from(child_local).map_err(|_| {
                        BuildError::Validation(
                            "Static Workflow Plan child handle is not representable".to_owned(),
                        )
                    })?)
                    .ok_or_else(|| {
                        BuildError::Validation(
                            "Static Workflow Plan child handle is outside its task".to_owned(),
                        )
                    })?;
                if parent.replace(local_instance).is_some() {
                    return validation(
                        "Static Workflow Plan child instance has more than one parent",
                    );
                }
            }
            step_tasks.push(task);
            step_instances.push(u32::try_from(instance).map_err(|_| {
                BuildError::Validation("Static Workflow Plan instance is outside u32".to_owned())
            })?);
            step_nodes.push(execution);
            step_children.push(child);
        }

        for index in tasks.values_mut() {
            index
                .node_metadata
                .resize_with(index.node_instances.len(), || None);
        }

        let mut runtime_edges = BTreeMap::<u32, Vec<(u32, u32, usize)>>::new();
        let mut expanded_edge_facts = Vec::with_capacity(edges.len());
        for (expected, value) in edges.iter().enumerate() {
            let item = object(value, "edges[]")?;
            require_dense_handle(item, expected, "edges")?;
            let instance = index_field(item, "instance", instance_tasks.len())?;
            let task = instance_tasks[instance];
            let source = item
                .get("source_step")
                .map(|_| index_field(item, "source_step", step_tasks.len()))
                .transpose()?;
            if source.is_some_and(|source| {
                step_tasks[source] != task
                    || usize::try_from(step_instances[source]) != Ok(instance)
            }) {
                return validation("Static Workflow Plan edge source crosses instance ownership");
            }
            let source = source.map(|source| step_nodes[source]);
            expanded_edge_facts.push((task, instance_locals[instance], source));
            if let Some(source) = source {
                let local_instance = *instance_locals.get(instance).ok_or_else(|| {
                    BuildError::Validation(
                        "Static Workflow Plan edge instance is not representable".to_owned(),
                    )
                })?;
                runtime_edges
                    .entry(task)
                    .or_default()
                    .push((source, local_instance, expected));
            }
        }
        for (task, edges) in &mut runtime_edges {
            // Structured runtime 按 task 执行序拼接各节点 outgoing 区间；canonical StableId
            // 顺序刻意与运行时 handle 无关。
            edges.sort_by_key(|(source, _, _)| *source);
            let task_index = tasks.get_mut(task).ok_or_else(|| {
                BuildError::Validation("Static Workflow Plan edge has no owning task".to_owned())
            })?;
            task_index
                .edge_sources
                .extend(edges.iter().map(|(source, _, _)| *source));
            task_index
                .edge_instances
                .extend(edges.iter().map(|(_, instance, _)| *instance));
            task_index.edge_metadata.resize_with(edges.len(), || None);
        }

        for index in tasks.values_mut() {
            let node_count = u32::try_from(index.node_instances.len()).map_err(|_| {
                BuildError::Validation("Static Workflow Plan node count is outside u32".to_owned())
            })?;
            index.sources.extend(0..node_count);
        }
        let mut expected_outputs = BTreeMap::new();
        let mut next_action = BTreeMap::<u32, u32>::new();
        let mut resource_steps = BTreeSet::new();
        for value in resources {
            let item = object(value, "node_resources[]")?;
            let step = index_field(item, "step", step_tasks.len())?;
            if !resource_steps.insert(step) {
                return validation("Static Workflow Plan resource step is duplicated");
            }
            let task = step_tasks[step];
            if let Some(binding) = item.get("action_binding") {
                let binding = object(binding, "node_resources[].action_binding")?;
                let action = *next_action.entry(task).or_default();
                *next_action.get_mut(&task).ok_or_else(|| {
                    BuildError::Validation("Static Workflow Plan action is missing".to_owned())
                })? = action.checked_add(1).ok_or_else(|| {
                    BuildError::Validation("Static Workflow Plan action count overflow".to_owned())
                })?;
                let task_index = tasks.get_mut(&task).ok_or_else(|| {
                    BuildError::Validation("Static Workflow Plan resource has no task".to_owned())
                })?;
                task_index.sources.insert(action);
                for port in array_field(binding, "ports")? {
                    let port = object(port, "ports[]")?;
                    if port.get("direction").and_then(Value::as_str) == Some("output")
                        || port.get("direction").and_then(Value::as_str) == Some("in_out")
                    {
                        let port_handle = u32_field(port, "port")?;
                        let slot = object(
                            port.get("slot").ok_or_else(|| {
                                BuildError::Validation(
                                    "Static Workflow Plan output slot is required".to_owned(),
                                )
                            })?,
                            "ports[].slot",
                        )?;
                        let value_type = string_field(slot, "value_type")?;
                        let encoded_bytes = value_type_bytes(value_type)?;
                        let expected = ExpectedOutput {
                            task,
                            instance: step_instances[step],
                            node: step_nodes[step],
                            action,
                            value_id: string_field(slot, "target_id")?.to_owned(),
                            type_handle: value_type_handle(value_type)?,
                            area: string_field(slot, "area")?.to_owned(),
                            image_offset_bytes: decimal_u64_field(slot, "image_offset_bytes")?,
                            encoded_bytes,
                        };
                        if expected_outputs
                            .insert(
                                (
                                    u32::try_from(step).map_err(|_| {
                                        BuildError::Validation(
                                            "Static Workflow Plan step is outside u32".to_owned(),
                                        )
                                    })?,
                                    port_handle,
                                ),
                                expected,
                            )
                            .is_some()
                        {
                            return validation("Static Workflow Plan output source is duplicated");
                        }
                    }
                }
            }
        }

        let mut expected_watches = BTreeMap::new();
        for (expected, value) in watches.iter().enumerate() {
            let item = object(value, "watches[]")?;
            require_dense_handle(item, expected, "watches")?;
            let task = u32_field(item, "task_handle")?;
            tasks.get(&task).ok_or_else(|| {
                BuildError::Validation("Static Workflow Plan watch has no task".to_owned())
            })?;
            expected_watches.insert(
                u32::try_from(expected).map_err(|_| {
                    BuildError::Validation("Static Workflow Plan watch is outside u32".to_owned())
                })?,
                ExpectedWatch {
                    task,
                    value_id: string_field(item, "value_id")?.to_owned(),
                    encoded_bytes: decimal_u64_field(item, "encoded_bytes")?,
                    fragment_count: u16_field(item, "fragment_count")?,
                },
            );
        }
        let trace_values = parse_trace_values(
            root,
            minor,
            &instance_tasks,
            &instance_locals,
            &expected_outputs,
            &expected_watches,
        )?;
        parse_trace_structure(
            root,
            minor,
            &instance_tasks,
            &instance_locals,
            &child_instances,
            &step_tasks,
            &step_instances,
            &step_nodes,
            &step_children,
            &expanded_edge_facts,
            &mut tasks,
        )?;
        let mut output_values = BTreeMap::<(u32, u32), BTreeSet<u32>>::new();
        let mut watch_values = BTreeMap::<u32, BTreeSet<u32>>::new();
        for (index, descriptor) in trace_values.iter().enumerate() {
            let handle = u32::try_from(index).map_err(|_| {
                BuildError::Validation(
                    "Static Workflow Plan Trace value count is outside u32".to_owned(),
                )
            })?;
            match &descriptor.source {
                TraceValueSourceIndex::Output { node, .. } => {
                    output_values
                        .entry((descriptor.task, *node))
                        .or_default()
                        .insert(handle);
                }
                TraceValueSourceIndex::Watch => {
                    watch_values
                        .entry(descriptor.task)
                        .or_default()
                        .insert(handle);
                }
            }
        }
        Ok(Self {
            minor,
            tasks,
            trace_values,
            output_values,
            watch_values,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "每种固定 Trace 事件在一个穷尽 match 中与 Plan 1.3 结构证据逐项对应"
    )]
    fn validate_record(&self, record: WorkflowTraceRecord) -> BuildResult<()> {
        let task = self.tasks.get(&record.task_handle().get()).ok_or_else(|| {
            BuildError::Validation(format!(
                "Workflow Trace task {} is absent from the Static Workflow Plan",
                record.task_handle().get()
            ))
        })?;
        let instance = record.workflow_instance_handle();
        let basic_instance = instance < task.instances;
        let owned_node = || match (record.node_handle(), record.execution_order()) {
            (Some(node), Some(order)) if node == order => {
                owned_by_instance(&task.node_instances, node, instance)
            }
            _ => false,
        };
        let edge = || {
            record.edge_handle().and_then(|handle| {
                usize::try_from(handle)
                    .ok()
                    .filter(|index| {
                        owned_by_instance(&task.edge_instances, handle, instance)
                            && task.edge_sources.get(*index) == record.node_handle().as_ref()
                    })
                    .and_then(|index| task.edge_metadata.get(index).and_then(Option::as_ref))
            })
        };
        let metadata = || {
            record.node_handle().and_then(|handle| {
                usize::try_from(handle)
                    .ok()
                    .and_then(|index| task.node_metadata.get(index).and_then(Option::as_ref))
            })
        };
        let legacy_source_valid = record
            .source_handle()
            .is_none_or(|handle| task.sources.contains(&handle));
        let structurally_valid = if self.minor < 3 {
            legacy_source_valid
                && record
                    .node_handle()
                    .is_none_or(|handle| owned_by_instance(&task.node_instances, handle, instance))
                && record
                    .execution_order()
                    .is_none_or(|order| owned_by_instance(&task.node_instances, order, instance))
                && match (record.node_handle(), record.execution_order()) {
                    (Some(node), Some(order)) => node == order,
                    _ => true,
                }
                && match record.kind() {
                    WorkflowTraceEventKind::TransitionTaken
                    | WorkflowTraceEventKind::ForkActivated => {
                        match (record.node_handle(), record.edge_handle()) {
                            (Some(node), Some(edge)) => usize::try_from(edge)
                                .ok()
                                .and_then(|edge| task.edge_sources.get(edge))
                                .is_some_and(|source| *source == node),
                            _ => false,
                        }
                    }
                    _ => true,
                }
        } else {
            match record.kind() {
                WorkflowTraceEventKind::WorkflowInitialized
                | WorkflowTraceEventKind::WorkflowCompleted => {
                    record.node_handle().is_none()
                        && record.execution_order().is_none()
                        && task.root_instances.contains(&instance)
                }
                WorkflowTraceEventKind::NodeExecuted => owned_node() && metadata().is_some(),
                WorkflowTraceEventKind::TransitionTaken => owned_node() && edge().is_some(),
                WorkflowTraceEventKind::ForkActivated => {
                    owned_node()
                        && matches!(
                            metadata().map(|node| &node.kind),
                            Some(TraceNodeKindIndex::Fork { .. })
                        )
                        && edge().is_some_and(|edge| edge.branch_order == record.branch_order())
                }
                WorkflowTraceEventKind::JoinSatisfied => {
                    owned_node() && metadata().is_some_and(|node| join_event_matches(node, record))
                }
                WorkflowTraceEventKind::WaitObserved => {
                    owned_node()
                        && metadata().is_some_and(|node| wait_event_matches(node, record.detail()))
                }
                WorkflowTraceEventKind::CancelRequested => {
                    owned_node()
                        && metadata().is_some_and(|node| cancel_requested_matches(node, record))
                }
                WorkflowTraceEventKind::CancelApplied => {
                    owned_node()
                        && metadata().is_some_and(|node| cancel_applied_matches(node, record))
                }
                WorkflowTraceEventKind::SubworkflowActivated
                | WorkflowTraceEventKind::SubworkflowCompleted => {
                    match (record.node_handle(), record.execution_order(), metadata()) {
                        (Some(node), Some(order), Some(metadata)) if node == order => {
                            matches!(
                                metadata.kind,
                                TraceNodeKindIndex::Subworkflow {
                                    call_handle,
                                    child_instance,
                                } if child_instance == instance
                                    && record.source_handle() == Some(call_handle)
                            )
                        }
                        _ => false,
                    }
                }
                WorkflowTraceEventKind::CompletionRequested => {
                    owned_node()
                        && task.edge_metadata.iter().flatten().any(|edge| {
                            edge.source == record.node_handle().unwrap_or(u32::MAX)
                                && edge.instance == instance
                                && edge.target_complete
                        })
                }
                WorkflowTraceEventKind::WorkflowFaulted => {
                    owned_node() && record.source_handle() == record.node_handle()
                }
                WorkflowTraceEventKind::OutputStaged
                | WorkflowTraceEventKind::WatchedValue
                | WorkflowTraceEventKind::ForceObserved
                | WorkflowTraceEventKind::FallbackObserved
                | WorkflowTraceEventKind::DeadlineObserved
                | WorkflowTraceEventKind::ScanCommitted
                | WorkflowTraceEventKind::ScanDiscarded => true,
            }
        };
        let valid = basic_instance
            && structurally_valid
            && match record.kind() {
                WorkflowTraceEventKind::WatchedValue | WorkflowTraceEventKind::OutputStaged => {
                    self.validate_trace_value(record).is_ok()
                }
                _ => true,
            };
        if valid {
            Ok(())
        } else {
            Err(BuildError::Validation(format!(
                "Workflow Trace event {} contains a handle outside task {} Static Workflow Plan tables",
                record.event_sequence().get(),
                record.task_handle().get()
            )))
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "每个 Trace 事件只收集一次 release 闭包事实，重复分派会弱化完整性审计"
    )]
    fn validate_complete_release_closure(
        &self,
        records: &[WorkflowTraceRecord],
    ) -> BuildResult<()> {
        let mut release = None::<ReleaseTraceClosure>;
        let mut active_sets = BTreeMap::<(u32, u64), ActiveSetExpectation>::new();
        for record in records {
            if release
                .as_ref()
                .is_some_and(|current| current.key != release_key(*record))
            {
                let completed = release
                    .take()
                    .ok_or_else(|| BuildError::Validation("release audit missing".to_owned()))?;
                self.audit_release_closure(&completed)?;
                self.audit_active_set(&completed, &mut active_sets)?;
            }
            let current = release.get_or_insert_with(|| ReleaseTraceClosure::new(*record));
            match record.kind() {
                WorkflowTraceEventKind::WorkflowInitialized => {
                    if !current
                        .initialized_roots
                        .insert(record.workflow_instance_handle())
                    {
                        return validation(
                            "complete Workflow Trace initializes one root more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::NodeExecuted => {
                    let node = record.node_handle().ok_or_else(|| {
                        BuildError::Validation("NodeExecuted is missing its node".to_owned())
                    })?;
                    if !current.executed_nodes.insert(node) {
                        return validation(
                            "complete Workflow Trace executes one node more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::WorkflowFaulted => {
                    current.faulted = true;
                    let node = record.node_handle().ok_or_else(|| {
                        BuildError::Validation("WorkflowFaulted is missing its node".to_owned())
                    })?;
                    if !current.faulted_nodes.insert(node) {
                        return validation(
                            "complete Workflow Trace faults one node more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::TransitionTaken => {
                    let event = required_node_edge(*record, "TransitionTaken")?;
                    if !current.transitions.insert(event) {
                        return validation(
                            "complete Workflow Trace takes one transition more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::ForkActivated => {
                    let (node, edge) = required_node_edge(*record, "ForkActivated")?;
                    let branch = record.branch_order().ok_or_else(|| {
                        BuildError::Validation("ForkActivated is missing its branch".to_owned())
                    })?;
                    if !current.fork_activations.insert((node, edge, branch)) {
                        return validation(
                            "complete Workflow Trace activates one Fork branch more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::JoinSatisfied => {
                    let node = required_node(*record, "JoinSatisfied")?;
                    if current
                        .joins
                        .insert(node, (record.detail(), record.branch_order()))
                        .is_some()
                    {
                        return validation(
                            "complete Workflow Trace satisfies one Join more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::WaitObserved => {
                    let node = required_node(*record, "WaitObserved")?;
                    if current.waits.insert(node, record.detail()).is_some() {
                        return validation(
                            "complete Workflow Trace observes one Wait more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::CancelRequested => {
                    let node = required_node(*record, "CancelRequested")?;
                    let branch = required_branch(*record, "CancelRequested")?;
                    if !current
                        .cancel_requests
                        .insert((node, branch, record.detail()))
                    {
                        return validation(
                            "complete Workflow Trace requests one branch cancellation more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::CancelApplied => {
                    let node = required_node(*record, "CancelApplied")?;
                    let branch = required_branch(*record, "CancelApplied")?;
                    if !current
                        .cancel_applications
                        .insert((node, branch, record.detail()))
                    {
                        return validation(
                            "complete Workflow Trace applies one branch cancellation more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::SubworkflowActivated => {
                    let node = required_node(*record, "SubworkflowActivated")?;
                    let source = required_source(*record, "SubworkflowActivated")?;
                    if current
                        .subworkflow_activations
                        .insert(node, (record.workflow_instance_handle(), source))
                        .is_some()
                    {
                        return validation(
                            "complete Workflow Trace activates one Subworkflow more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::SubworkflowCompleted => {
                    let node = required_node(*record, "SubworkflowCompleted")?;
                    let source = required_source(*record, "SubworkflowCompleted")?;
                    if current
                        .subworkflow_completions
                        .insert(node, (record.workflow_instance_handle(), source))
                        .is_some()
                    {
                        return validation(
                            "complete Workflow Trace completes one Subworkflow more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::CompletionRequested => {
                    let node = required_node(*record, "CompletionRequested")?;
                    if !current.completion_requests.insert(node) {
                        return validation(
                            "complete Workflow Trace requests one completion more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::WorkflowCompleted => {
                    if !current
                        .completed_roots
                        .insert(record.workflow_instance_handle())
                    {
                        return validation(
                            "complete Workflow Trace completes one root more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::ScanCommitted => current.committed = true,
                WorkflowTraceEventKind::DeadlineObserved
                    if record.detail() == MissOutcome::FinishAfterDeadline as u16 =>
                {
                    current.deadline_discarded = true;
                }
                WorkflowTraceEventKind::OutputStaged
                    if !record.fragment().is_present() || record.fragment().index == 0 =>
                {
                    let value = record.value_handle().ok_or_else(|| {
                        BuildError::Validation("OutputStaged is missing its value".to_owned())
                    })?;
                    if !current.output_values.insert(value) {
                        return validation(
                            "complete Workflow Trace generates one Output value more than once",
                        );
                    }
                }
                WorkflowTraceEventKind::WatchedValue if record.fragment().index == 0 => {
                    let value = record.value_handle().ok_or_else(|| {
                        BuildError::Validation("WatchedValue is missing its value".to_owned())
                    })?;
                    if !current.watch_values.insert(value) {
                        return validation(
                            "complete Workflow Trace generates one Watch value more than once",
                        );
                    }
                }
                _ => {}
            }
        }
        if let Some(current) = release {
            self.audit_release_closure(&current)?;
            self.audit_active_set(&current, &mut active_sets)?;
        }
        Ok(())
    }

    fn audit_release_closure(&self, release: &ReleaseTraceClosure) -> BuildResult<()> {
        if self.minor >= 3 {
            self.audit_structural_events(release)?;
        }
        let task = release.key.0;
        let mut required_outputs = BTreeSet::new();
        let mut allowed_outputs = BTreeSet::new();
        for node in &release.executed_nodes {
            if let Some(values) = self.output_values.get(&(task, *node)) {
                allowed_outputs.extend(values);
                if !release.faulted_nodes.contains(node) {
                    required_outputs.extend(values);
                }
            }
        }
        if !required_outputs.is_subset(&release.output_values)
            || !release.output_values.is_subset(&allowed_outputs)
        {
            return validation(
                "complete Workflow Trace Output producers do not exactly match executed Actions",
            );
        }
        let empty = BTreeSet::new();
        let expected_watches = if release.faulted || release.deadline_discarded {
            &empty
        } else {
            self.watch_values.get(&task).unwrap_or(&empty)
        };
        if &release.watch_values != expected_watches {
            return validation(
                "complete Workflow Trace Watch producers do not match the planned catalog",
            );
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Plan 1.3 每类结构节点的事件基数在同一处逐项闭合，避免漏掉成对事件"
    )]
    fn audit_structural_events(&self, release: &ReleaseTraceClosure) -> BuildResult<()> {
        let task = self.tasks.get(&release.key.0).ok_or_else(|| {
            BuildError::Validation(
                "release task is absent from the Static Workflow Plan".to_owned(),
            )
        })?;
        for (node, _) in &release.transitions {
            if !release.executed_nodes.contains(node)
                && !release.subworkflow_completions.contains_key(node)
            {
                return validation(
                    "complete Workflow Trace transition has no executed or completed producer",
                );
            }
        }
        for node in release
            .fork_activations
            .iter()
            .map(|event| event.0)
            .chain(release.joins.keys().copied())
            .chain(release.waits.keys().copied())
            .chain(release.cancel_requests.iter().map(|event| event.0))
            .chain(release.subworkflow_activations.keys().copied())
        {
            if !release.executed_nodes.contains(&node) {
                return validation(
                    "complete Workflow Trace structural event has no NodeExecuted producer",
                );
            }
        }

        let mut complete_transitions = BTreeSet::new();
        for (node, edge) in &release.transitions {
            let metadata = task
                .edge_metadata
                .get(usize::try_from(*edge).map_err(|_| {
                    BuildError::Validation("Runtime edge handle is not representable".to_owned())
                })?)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    BuildError::Validation("Runtime edge is absent from trace_structure".to_owned())
                })?;
            if metadata.target_complete {
                complete_transitions.insert(*node);
            }
        }
        if complete_transitions != release.completion_requests {
            return validation(
                "complete Workflow Trace completion requests do not match complete transitions",
            );
        }
        for node in release.subworkflow_completions.keys() {
            if release
                .transitions
                .iter()
                .filter(|event| event.0 == *node)
                .count()
                != 1
            {
                return validation(
                    "complete Workflow Trace Subworkflow completion does not have one transition",
                );
            }
        }
        for (node, branch, detail) in &release.cancel_applications {
            if *detail == 1
                && !release
                    .cancel_requests
                    .iter()
                    .any(|request| request.0 == *node && request.1 == *branch)
            {
                return validation(
                    "complete Workflow Trace commit-boundary cancellation has no request",
                );
            }
        }

        for node in &release.executed_nodes {
            let metadata = task
                .node_metadata
                .get(usize::try_from(*node).map_err(|_| {
                    BuildError::Validation("Runtime node handle is not representable".to_owned())
                })?)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    BuildError::Validation("Runtime node is absent from trace_structure".to_owned())
                })?;
            let transitions = release
                .transitions
                .iter()
                .filter(|event| event.0 == *node)
                .map(|event| event.1)
                .collect::<BTreeSet<_>>();
            let faulted = release.faulted_nodes.contains(node);
            match &metadata.kind {
                TraceNodeKindIndex::Action | TraceNodeKindIndex::Decision => {
                    if transitions.len() > 1 {
                        return validation(
                            "complete Workflow Trace takes more than one scalar node transition",
                        );
                    }
                }
                TraceNodeKindIndex::Fork { branch_orders } => {
                    let expected = task
                        .edge_metadata
                        .iter()
                        .enumerate()
                        .filter_map(|(edge, metadata)| {
                            metadata.as_ref().and_then(|edge_metadata| {
                                (edge_metadata.source == *node).then(|| {
                                    edge_metadata.branch_order.map(|branch| (edge, branch))
                                })?
                            })
                        })
                        .map(|(edge, branch)| {
                            u32::try_from(edge)
                                .map(|edge| (*node, edge, branch))
                                .map_err(|_| {
                                    BuildError::Validation(
                                        "Runtime edge handle exceeds u32".to_owned(),
                                    )
                                })
                        })
                        .collect::<BuildResult<BTreeSet<_>>>()?;
                    let expected_branches = expected
                        .iter()
                        .map(|event| event.2)
                        .collect::<BTreeSet<_>>();
                    let actual = release
                        .fork_activations
                        .iter()
                        .filter(|event| event.0 == *node)
                        .copied()
                        .collect::<BTreeSet<_>>();
                    let expected_transitions = expected
                        .iter()
                        .map(|event| event.1)
                        .collect::<BTreeSet<_>>();
                    let activated_transitions =
                        actual.iter().map(|event| event.1).collect::<BTreeSet<_>>();
                    if expected_branches != *branch_orders
                        || activated_transitions != transitions
                        || if faulted {
                            !actual.is_subset(&expected)
                                || !transitions.is_subset(&expected_transitions)
                        } else {
                            actual != expected || transitions != expected_transitions
                        }
                    {
                        return validation(
                            "complete Workflow Trace Fork event multiplicity does not match its branches",
                        );
                    }
                }
                TraceNodeKindIndex::Merge => {
                    let joined = release.joins.contains_key(node);
                    if transitions.len() > 1
                        || joined != (transitions.len() == 1)
                        || (!faulted && !joined)
                    {
                        return validation(
                            "complete Workflow Trace Merge events do not form one closed transition",
                        );
                    }
                }
                TraceNodeKindIndex::JoinAll { .. } | TraceNodeKindIndex::JoinAny { .. } => {
                    let joined = release.joins.contains_key(node);
                    if transitions.len() > 1 || joined != (transitions.len() == 1) {
                        return validation(
                            "complete Workflow Trace JoinSatisfied does not match its transition",
                        );
                    }
                    let requests = release
                        .cancel_requests
                        .iter()
                        .filter(|event| event.0 == *node)
                        .map(|event| event.1)
                        .collect::<BTreeSet<_>>();
                    if let TraceNodeKindIndex::JoinAny {
                        loser_policy,
                        branch_orders,
                    } = &metadata.kind
                        && let Some((_, Some(winner))) = release.joins.get(node)
                    {
                        let losers = branch_orders
                            .iter()
                            .filter(|branch| **branch != *winner)
                            .copied()
                            .collect::<BTreeSet<_>>();
                        let expected_requests =
                            if *loser_policy == TraceJoinPolicyIndex::KeepRunning {
                                BTreeSet::new()
                            } else {
                                losers.clone()
                            };
                        if requests != expected_requests {
                            return validation(
                                "complete Workflow Trace JoinAny cancellation requests do not match its losers",
                            );
                        }
                        let commit_applied = release
                            .cancel_applications
                            .iter()
                            .filter(|event| event.0 == *node && event.2 == 1)
                            .map(|event| event.1)
                            .collect::<BTreeSet<_>>();
                        if (*loser_policy == TraceJoinPolicyIndex::CancelOthers
                            && commit_applied != losers)
                            || (*loser_policy == TraceJoinPolicyIndex::WaitAtBoundary
                                && !commit_applied.is_subset(&losers))
                            || (*loser_policy == TraceJoinPolicyIndex::KeepRunning
                                && !commit_applied.is_empty())
                        {
                            return validation(
                                "complete Workflow Trace JoinAny applied cancellations do not match its policy",
                            );
                        }
                    } else if !requests.is_empty() {
                        return validation(
                            "complete Workflow Trace cancellation request has no satisfied JoinAny",
                        );
                    }
                }
                TraceNodeKindIndex::WaitCycles | TraceNodeKindIndex::WaitCondition { .. } => {
                    let detail = release.waits.get(node).copied();
                    if !faulted && detail.is_none() {
                        return validation(
                            "complete Workflow Trace is missing the executed Wait observation",
                        );
                    }
                    if let Some(detail) = detail {
                        let took_transition = matches!(detail, 2 | 4);
                        if transitions.len() > 1
                            || (!faulted && took_transition != (transitions.len() == 1))
                            || (!took_transition && !transitions.is_empty())
                        {
                            return validation(
                                "complete Workflow Trace Wait observation does not match its transition",
                            );
                        }
                    }
                }
                TraceNodeKindIndex::Subworkflow { .. } => {
                    if !faulted && !release.subworkflow_activations.contains_key(node) {
                        return validation(
                            "complete Workflow Trace is missing the executed Subworkflow activation",
                        );
                    }
                    let completed = release.subworkflow_completions.contains_key(node);
                    if transitions.len() > 1 || completed != (transitions.len() == 1) {
                        return validation(
                            "complete Workflow Trace Subworkflow completion does not match its transition",
                        );
                    }
                }
            }
        }
        Self::audit_root_completion(task, release)?;
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "root 的 candidate、running 与 cancellation ambiguity 必须在一个闭包中比较"
    )]
    fn audit_root_completion(
        task: &TaskPlanIndex,
        release: &ReleaseTraceClosure,
    ) -> BuildResult<()> {
        if !release.committed {
            if release.completed_roots.is_empty() {
                return Ok(());
            }
            return validation("discarded Workflow Trace release completes a root");
        }

        let mut candidates = BTreeSet::new();
        for node in &release.completion_requests {
            candidates.insert(root_for_node(task, *node)?);
        }
        for (node, _, _) in &release.cancel_applications {
            candidates.insert(root_for_node(task, *node)?);
        }
        if !release.initialized_roots.is_empty() {
            let roots_with_nodes = (0..task.node_instances.len())
                .map(|node| {
                    u32::try_from(node)
                        .map_err(|_| {
                            BuildError::Validation("Runtime node handle exceeds u32".to_owned())
                        })
                        .and_then(|node| root_for_node(task, node))
                })
                .collect::<BuildResult<BTreeSet<_>>>()?;
            candidates.extend(task.root_instances.difference(&roots_with_nodes).copied());
        }

        let mut running = BTreeSet::new();
        for (_, edge) in &release.transitions {
            let metadata = task
                .edge_metadata
                .get(usize::try_from(*edge).map_err(|_| {
                    BuildError::Validation("Runtime edge handle is not representable".to_owned())
                })?)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    BuildError::Validation("Runtime edge is absent from trace_structure".to_owned())
                })?;
            if let Some(target) = metadata.target_node {
                running.insert(root_for_node(task, target)?);
            }
        }
        for node in &release.executed_nodes {
            let metadata = task
                .node_metadata
                .get(usize::try_from(*node).map_err(|_| {
                    BuildError::Validation("Runtime node handle is not representable".to_owned())
                })?)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    BuildError::Validation("Runtime node is absent from trace_structure".to_owned())
                })?;
            let transitioned = release.transitions.iter().any(|event| event.0 == *node);
            let canceled_at_boundary = release
                .cancel_applications
                .iter()
                .any(|event| event.0 == *node && event.2 == 2);
            let remains_running = match &metadata.kind {
                TraceNodeKindIndex::Action
                | TraceNodeKindIndex::Decision
                | TraceNodeKindIndex::Merge
                | TraceNodeKindIndex::JoinAll { .. }
                | TraceNodeKindIndex::JoinAny { .. } => !transitioned && !canceled_at_boundary,
                TraceNodeKindIndex::WaitCycles | TraceNodeKindIndex::WaitCondition { .. } => {
                    release
                        .waits
                        .get(node)
                        .is_some_and(|detail| matches!(detail, 1 | 3 | 6))
                        && !canceled_at_boundary
                }
                TraceNodeKindIndex::Subworkflow { .. } => {
                    release.subworkflow_activations.contains_key(node)
                        && !release.subworkflow_completions.contains_key(node)
                        && !canceled_at_boundary
                }
                TraceNodeKindIndex::Fork { .. } => false,
            };
            if remains_running {
                running.insert(root_for_node(task, *node)?);
            }
        }

        let mut ambiguous_cancellation = BTreeSet::new();
        for (node, _, _) in release.cancel_requests.iter().chain(
            release
                .cancel_applications
                .iter()
                .filter(|event| event.2 == 1),
        ) {
            ambiguous_cancellation.insert(root_for_node(task, *node)?);
        }
        for root in &release.completed_roots {
            if !candidates.contains(root)
                || (running.contains(root) && !ambiguous_cancellation.contains(root))
            {
                return validation(
                    "complete Workflow Trace root completion contradicts its release lifecycle",
                );
            }
        }
        for root in candidates
            .difference(&running)
            .filter(|root| !ambiguous_cancellation.contains(root))
        {
            if !release.completed_roots.contains(root) {
                return validation(
                    "complete Workflow Trace omits a root completion required by its release lifecycle",
                );
            }
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "跨 release active-set 的 expected、retained、transition 与取消歧义必须原子更新"
    )]
    fn audit_active_set(
        &self,
        release: &ReleaseTraceClosure,
        active_sets: &mut BTreeMap<(u32, u64), ActiveSetExpectation>,
    ) -> BuildResult<()> {
        if self.minor < 3 {
            return Ok(());
        }
        let task = self.tasks.get(&release.key.0).ok_or_else(|| {
            BuildError::Validation(
                "release task is absent from the Static Workflow Plan".to_owned(),
            )
        })?;
        let node_roots = (0..task.node_instances.len())
            .map(|node| {
                u32::try_from(node)
                    .map_err(|_| {
                        BuildError::Validation("Runtime node handle exceeds u32".to_owned())
                    })
                    .and_then(|node| root_for_node(task, node).map(|root| (node, root)))
            })
            .collect::<BuildResult<BTreeMap<_, _>>>()?;
        let epoch = (release.key.0, release.key.1);
        if !release.initialized_roots.is_empty() {
            let initial = ActiveSetExpectation {
                required: task.initial_active.clone(),
                allowed: task.initial_active.clone(),
            };
            active_sets.insert(epoch, initial);
        }
        let expected = active_sets.get(&epoch).ok_or_else(|| {
            BuildError::Validation(
                "complete Workflow Trace task epoch starts without WorkflowInitialized".to_owned(),
            )
        })?;
        if !release.executed_nodes.is_subset(&expected.allowed)
            || (release.committed && !expected.required.is_subset(&release.executed_nodes))
        {
            return validation(
                "complete Workflow Trace NodeExecuted set does not match the prior committed active set",
            );
        }
        if !release.committed {
            return Ok(());
        }

        let mut next = ActiveSetExpectation::default();
        for (_, edge) in &release.transitions {
            let metadata = task
                .edge_metadata
                .get(usize::try_from(*edge).map_err(|_| {
                    BuildError::Validation("Runtime edge handle is not representable".to_owned())
                })?)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    BuildError::Validation("Runtime edge is absent from trace_structure".to_owned())
                })?;
            if let Some(target) = metadata.target_node {
                next.required.insert(target);
                next.allowed.insert(target);
            }
        }
        for node in &release.executed_nodes {
            let metadata = task
                .node_metadata
                .get(usize::try_from(*node).map_err(|_| {
                    BuildError::Validation("Runtime node handle is not representable".to_owned())
                })?)
                .and_then(Option::as_ref)
                .ok_or_else(|| {
                    BuildError::Validation("Runtime node is absent from trace_structure".to_owned())
                })?;
            let transitioned = release.transitions.iter().any(|event| event.0 == *node);
            let canceled_at_boundary = release
                .cancel_applications
                .iter()
                .any(|event| event.0 == *node && event.2 == 2);
            let retained = match &metadata.kind {
                TraceNodeKindIndex::Action
                | TraceNodeKindIndex::Decision
                | TraceNodeKindIndex::Merge
                | TraceNodeKindIndex::JoinAll { .. }
                | TraceNodeKindIndex::JoinAny { .. } => !transitioned && !canceled_at_boundary,
                TraceNodeKindIndex::WaitCycles | TraceNodeKindIndex::WaitCondition { .. } => {
                    release
                        .waits
                        .get(node)
                        .is_some_and(|detail| matches!(detail, 1 | 3 | 6))
                        && !canceled_at_boundary
                }
                TraceNodeKindIndex::Fork { .. } | TraceNodeKindIndex::Subworkflow { .. } => false,
            };
            if retained {
                next.required.insert(*node);
                next.allowed.insert(*node);
            }
            if let TraceNodeKindIndex::Subworkflow { child_instance, .. } = &metadata.kind
                && release.subworkflow_activations.contains_key(node)
                && !release.subworkflow_completions.contains_key(node)
                && let Some(initial) = task.instance_initial_active.get(child_instance).copied()
            {
                next.required.insert(initial);
                next.allowed.insert(initial);
            }
        }

        let ambiguous_roots = release
            .cancel_requests
            .iter()
            .map(|event| event.0)
            .chain(
                release
                    .cancel_applications
                    .iter()
                    .filter(|event| event.2 == 1)
                    .map(|event| event.0),
            )
            .map(|node| {
                node_roots.get(&node).copied().ok_or_else(|| {
                    BuildError::Validation(
                        "Runtime node is outside its task instance table".to_owned(),
                    )
                })
            })
            .collect::<BuildResult<BTreeSet<_>>>()?;
        if !ambiguous_roots.is_empty() {
            let ambiguous_nodes = node_roots
                .iter()
                .filter_map(|(node, root)| ambiguous_roots.contains(root).then_some(*node))
                .collect::<BTreeSet<_>>();
            next.required.retain(|node| !ambiguous_nodes.contains(node));
            next.allowed.extend(ambiguous_nodes);
        }
        for root in &release.completed_roots {
            let completed_nodes = node_roots
                .iter()
                .filter_map(|(node, candidate)| (*candidate == *root).then_some(*node))
                .collect::<BTreeSet<_>>();
            next.required.retain(|node| !completed_nodes.contains(node));
            next.allowed.retain(|node| !completed_nodes.contains(node));
        }
        active_sets.insert(epoch, next);
        Ok(())
    }

    fn validate_trace_value(&self, record: WorkflowTraceRecord) -> BuildResult<()> {
        if self.minor < 2 {
            return validation("Static Workflow Plan 1.1 cannot validate value Trace events");
        }
        let value_handle = record
            .value_handle()
            .ok_or_else(|| BuildError::Validation("Trace value handle is required".to_owned()))?;
        let descriptor = self
            .trace_values
            .get(usize::try_from(value_handle).map_err(|_| {
                BuildError::Validation("Trace value handle is not representable".to_owned())
            })?)
            .ok_or_else(|| {
                BuildError::Validation("Trace value handle is outside the catalog".to_owned())
            })?;
        if descriptor.task != record.task_handle().get()
            || descriptor.instance != record.workflow_instance_handle()
            || record.type_handle() != Some(descriptor.type_handle)
            || !fragment_matches(record, descriptor)
        {
            return validation("Trace value event does not match its catalog descriptor");
        }
        match (&descriptor.source, record.kind()) {
            (
                TraceValueSourceIndex::Output { node, action, port },
                WorkflowTraceEventKind::OutputStaged,
            ) if record.node_handle() == Some(*node)
                && record.execution_order() == Some(*node)
                && record.source_handle() == Some(*action)
                && *port != u32::MAX =>
            {
                Ok(())
            }
            (TraceValueSourceIndex::Watch, WorkflowTraceEventKind::WatchedValue)
                if record.node_handle().is_none()
                    && record.execution_order().is_none()
                    && record.source_handle().is_none() =>
            {
                Ok(())
            }
            _ => validation("Trace value event source does not match its catalog descriptor"),
        }
    }
}

fn release_key(record: WorkflowTraceRecord) -> (u32, u64, u64) {
    (
        record.task_handle().get(),
        record.task_epoch().get(),
        record.release_sequence().get(),
    )
}

fn required_node(record: WorkflowTraceRecord, event: &str) -> BuildResult<u32> {
    record
        .node_handle()
        .ok_or_else(|| BuildError::Validation(format!("{event} is missing its node handle")))
}

fn required_node_edge(record: WorkflowTraceRecord, event: &str) -> BuildResult<(u32, u32)> {
    Ok((
        required_node(record, event)?,
        record
            .edge_handle()
            .ok_or_else(|| BuildError::Validation(format!("{event} is missing its edge handle")))?,
    ))
}

fn required_branch(record: WorkflowTraceRecord, event: &str) -> BuildResult<u32> {
    record
        .branch_order()
        .ok_or_else(|| BuildError::Validation(format!("{event} is missing its branch order")))
}

fn required_source(record: WorkflowTraceRecord, event: &str) -> BuildResult<u32> {
    record
        .source_handle()
        .ok_or_else(|| BuildError::Validation(format!("{event} is missing its source handle")))
}

fn root_for_node(task: &TaskPlanIndex, node: u32) -> BuildResult<u32> {
    let instance = task
        .node_instances
        .get(usize::try_from(node).map_err(|_| {
            BuildError::Validation("Runtime node handle is not representable".to_owned())
        })?)
        .copied()
        .ok_or_else(|| {
            BuildError::Validation("Runtime node is outside its task instance table".to_owned())
        })?;
    root_for_instance(task, instance)
}

fn root_for_instance(task: &TaskPlanIndex, mut instance: u32) -> BuildResult<u32> {
    for _ in 0..task.instance_parents.len() {
        let parent = task
            .instance_parents
            .get(usize::try_from(instance).map_err(|_| {
                BuildError::Validation("Runtime instance handle is not representable".to_owned())
            })?)
            .ok_or_else(|| {
                BuildError::Validation("Runtime instance is outside its task table".to_owned())
            })?;
        match parent {
            Some(parent) => instance = *parent,
            None if task.root_instances.contains(&instance) => return Ok(instance),
            None => {
                return validation(
                    "Runtime instance ancestry does not terminate at a planned root",
                );
            }
        }
    }
    validation("Runtime instance ancestry contains a cycle")
}

fn owned_by_instance(values: &[u32], handle: u32, instance: u32) -> bool {
    usize::try_from(handle)
        .ok()
        .and_then(|index| values.get(index))
        .is_some_and(|owner| *owner == instance)
}

fn join_event_matches(node: &TraceNodeIndex, record: WorkflowTraceRecord) -> bool {
    match (&node.kind, record.detail(), record.branch_order()) {
        (TraceNodeKindIndex::Merge, 3, None) | (TraceNodeKindIndex::JoinAll { .. }, 1, None) => {
            true
        }
        (TraceNodeKindIndex::JoinAny { branch_orders, .. }, 2, Some(branch_order)) => {
            branch_orders.contains(&branch_order)
        }
        _ => false,
    }
}

fn wait_event_matches(node: &TraceNodeIndex, detail: u16) -> bool {
    match node.kind {
        TraceNodeKindIndex::WaitCycles => matches!(detail, 1 | 2),
        TraceNodeKindIndex::WaitCondition { has_timeout: true } => matches!(detail, 3..=5),
        TraceNodeKindIndex::WaitCondition { has_timeout: false } => matches!(detail, 4 | 6),
        _ => false,
    }
}

fn cancel_requested_matches(node: &TraceNodeIndex, record: WorkflowTraceRecord) -> bool {
    let TraceNodeKindIndex::JoinAny {
        loser_policy,
        branch_orders,
    } = &node.kind
    else {
        return false;
    };
    let policy_matches = matches!(
        (*loser_policy, record.detail()),
        (TraceJoinPolicyIndex::CancelOthers, 1) | (TraceJoinPolicyIndex::WaitAtBoundary, 2)
    );
    policy_matches
        && record
            .branch_order()
            .is_some_and(|branch| branch_orders.contains(&branch))
}

fn cancel_applied_matches(node: &TraceNodeIndex, record: WorkflowTraceRecord) -> bool {
    let Some(branch) = record.branch_order() else {
        return false;
    };
    match (&node.kind, record.detail()) {
        (
            TraceNodeKindIndex::JoinAny {
                loser_policy,
                branch_orders,
            },
            1,
        ) => *loser_policy != TraceJoinPolicyIndex::KeepRunning && branch_orders.contains(&branch),
        (_, 2) => node.cancellation_boundary && node.cancellation_branch_orders.contains(&branch),
        _ => false,
    }
}

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Plan 1.3 provenance tables are admitted atomically against every referenced core table"
)]
fn parse_trace_structure(
    root: &Map<String, Value>,
    minor: u32,
    instance_tasks: &[u32],
    instance_locals: &[u32],
    child_instances: &BTreeSet<usize>,
    step_tasks: &[u32],
    step_instances: &[u32],
    step_nodes: &[u32],
    step_children: &[Option<usize>],
    expanded_edge_facts: &[(u32, u32, Option<u32>)],
    tasks: &mut BTreeMap<u32, TaskPlanIndex>,
) -> BuildResult<()> {
    let expected_roots = (0..instance_tasks.len())
        .filter(|instance| !child_instances.contains(instance))
        .collect::<BTreeSet<_>>();
    for instance in &expected_roots {
        let task = instance_tasks[*instance];
        tasks
            .get_mut(&task)
            .ok_or_else(|| {
                BuildError::Validation("Static Workflow Plan root has no task".to_owned())
            })?
            .root_instances
            .insert(instance_locals[*instance]);
    }
    let Some(value) = root.get("trace_structure") else {
        return if minor < 3 {
            Ok(())
        } else {
            validation("Static Workflow Plan 1.3 `trace_structure` is required")
        };
    };
    if minor < 3 {
        return validation("Static Workflow Plan 1.1/1.2 cannot contain `trace_structure`");
    }
    let structure = object(value, "trace_structure")?;
    let roots = array_field(structure, "root_instances")?;
    let mut actual_roots = BTreeSet::new();
    for value in roots {
        let instance = usize::try_from(value.as_u64().ok_or_else(|| {
            BuildError::Validation("Static Workflow Plan root instance must be a u32".to_owned())
        })?)
        .map_err(|_| {
            BuildError::Validation(
                "Static Workflow Plan root instance is not representable".to_owned(),
            )
        })?;
        if instance >= instance_tasks.len() || !actual_roots.insert(instance) {
            return validation("Static Workflow Plan root instance table is invalid");
        }
    }
    if actual_roots != expected_roots {
        return validation("Static Workflow Plan root instance table is not closed");
    }

    let initial_active = array_field(structure, "initial_active")?;
    let mut initial_steps = BTreeSet::new();
    let mut initial_instances = BTreeSet::new();
    for value in initial_active {
        let step = usize::try_from(value.as_u64().ok_or_else(|| {
            BuildError::Validation(
                "Static Workflow Plan initial active step must be a u32".to_owned(),
            )
        })?)
        .map_err(|_| {
            BuildError::Validation(
                "Static Workflow Plan initial active step is not representable".to_owned(),
            )
        })?;
        if step >= step_tasks.len() || !initial_steps.insert(step) {
            return validation("Static Workflow Plan initial active table is invalid");
        }
        let instance = usize::try_from(step_instances[step]).map_err(|_| {
            BuildError::Validation(
                "Static Workflow Plan initial active instance is not representable".to_owned(),
            )
        })?;
        if !initial_instances.insert(instance) {
            return validation(
                "Static Workflow Plan initial active step is not unique to an instance",
            );
        }
        let task = step_tasks[step];
        let task_index = tasks.get_mut(&task).ok_or_else(|| {
            BuildError::Validation(
                "Static Workflow Plan initial active step has no task".to_owned(),
            )
        })?;
        let local_instance = instance_locals[instance];
        if task_index
            .instance_initial_active
            .insert(local_instance, step_nodes[step])
            .is_some()
        {
            return validation("Static Workflow Plan initial active Runtime node is duplicated");
        }
        if expected_roots.contains(&instance) && !task_index.initial_active.insert(step_nodes[step])
        {
            return validation(
                "Static Workflow Plan root initial active Runtime node is duplicated",
            );
        }
    }

    let nodes = array_field(structure, "nodes")?;
    if nodes.len() != step_tasks.len() {
        return validation("Static Workflow Plan Trace node table is not closed");
    }
    for (expected, value) in nodes.iter().enumerate() {
        let item = object(value, "trace_structure.nodes[]")?;
        let step = index_field(item, "step", step_tasks.len())?;
        if step != expected {
            return validation("Static Workflow Plan Trace nodes are not in step order");
        }
        let task = step_tasks[step];
        let node = usize::try_from(step_nodes[step]).map_err(|_| {
            BuildError::Validation(
                "Static Workflow Plan Trace node is not representable".to_owned(),
            )
        })?;
        let kind = object(
            item.get("node_kind").ok_or_else(|| {
                BuildError::Validation(
                    "Static Workflow Plan Trace node kind is required".to_owned(),
                )
            })?,
            "trace_structure.nodes[].node_kind",
        )?;
        let parsed_kind = match string_field(kind, "kind")? {
            "action" => TraceNodeKindIndex::Action,
            "decision" => TraceNodeKindIndex::Decision,
            "fork" => TraceNodeKindIndex::Fork {
                branch_orders: sorted_u32_set(kind, "branch_orders")?,
            },
            "merge" => TraceNodeKindIndex::Merge,
            "join_all" => TraceNodeKindIndex::JoinAll {
                branch_orders: sorted_u32_set(kind, "branch_orders")?,
            },
            "join_any" => TraceNodeKindIndex::JoinAny {
                loser_policy: match string_field(kind, "loser_policy")? {
                    "cancel_others" => TraceJoinPolicyIndex::CancelOthers,
                    "keep_running" => TraceJoinPolicyIndex::KeepRunning,
                    "wait_at_boundary" => TraceJoinPolicyIndex::WaitAtBoundary,
                    _ => return validation("Static Workflow Plan JoinAny policy is invalid"),
                },
                branch_orders: sorted_u32_set(kind, "branch_orders")?,
            },
            "wait_cycles" => TraceNodeKindIndex::WaitCycles,
            "wait_condition" => TraceNodeKindIndex::WaitCondition {
                has_timeout: bool_field(kind, "has_timeout")?,
            },
            "subworkflow" => {
                let child = index_field(kind, "child_instance", instance_tasks.len())?;
                if step_children[step] != Some(child)
                    || instance_tasks[child] != task
                    || child == usize::try_from(step_instances[step]).unwrap_or(usize::MAX)
                {
                    return validation("Static Workflow Plan Subworkflow child is invalid");
                }
                TraceNodeKindIndex::Subworkflow {
                    call_handle: u32_field(kind, "call_handle")?,
                    child_instance: instance_locals[child],
                }
            }
            _ => return validation("Static Workflow Plan Trace node kind is invalid"),
        };
        if !matches!(parsed_kind, TraceNodeKindIndex::Subworkflow { .. })
            && step_children[step].is_some()
        {
            return validation("Static Workflow Plan non-Subworkflow step has a child");
        }
        let cancellation_boundary = bool_field(item, "cancellation_boundary")?;
        let cancellation_branch_orders = sorted_u32_set(item, "cancellation_branch_orders")?;
        if !cancellation_boundary && !cancellation_branch_orders.is_empty() {
            return validation("Static Workflow Plan cancellation membership lacks a boundary");
        }
        let task_index = tasks.get_mut(&task).ok_or_else(|| {
            BuildError::Validation("Static Workflow Plan Trace node has no task".to_owned())
        })?;
        let slot = task_index.node_metadata.get_mut(node).ok_or_else(|| {
            BuildError::Validation("Static Workflow Plan Trace node is outside task".to_owned())
        })?;
        if slot.is_some() {
            return validation("Static Workflow Plan Trace node is duplicated");
        }
        *slot = Some(TraceNodeIndex {
            kind: parsed_kind,
            cancellation_boundary,
            cancellation_branch_orders,
        });
    }
    if tasks
        .values()
        .any(|task| task.node_metadata.iter().any(Option::is_none))
    {
        return validation("Static Workflow Plan Trace node table is missing a step");
    }
    if tasks.values().any(|task| {
        task.node_instances
            .iter()
            .any(|instance| !task.instance_initial_active.contains_key(instance))
    }) {
        return validation(
            "Static Workflow Plan initial active table is missing an executable instance",
        );
    }

    for task in tasks.values_mut() {
        task.edge_sources.clear();
        task.edge_instances.clear();
        task.edge_metadata.clear();
    }
    let edges = array_field(structure, "edges")?;
    let expected_edges = expanded_edge_facts
        .iter()
        .filter(|(_, _, source)| source.is_some())
        .count();
    if edges.len() != expected_edges {
        return validation("Static Workflow Plan Trace edge table is not closed");
    }
    let mut seen_expanded = BTreeSet::new();
    let mut next_runtime = BTreeMap::<u32, u32>::new();
    for value in edges {
        let item = object(value, "trace_structure.edges[]")?;
        let task = u32_field(item, "task_handle")?;
        let runtime = u32_field(item, "runtime_edge_handle")?;
        let next = next_runtime.entry(task).or_default();
        if runtime != *next {
            return validation("Static Workflow Plan Runtime Trace edge handles are not dense");
        }
        *next = next.checked_add(1).ok_or_else(|| {
            BuildError::Validation("Static Workflow Plan Runtime edge count overflow".to_owned())
        })?;
        let expanded = index_field(item, "expanded_edge", expanded_edge_facts.len())?;
        if !seen_expanded.insert(expanded) {
            return validation("Static Workflow Plan Trace edge is duplicated");
        }
        let source_step = index_field(item, "source_step", step_tasks.len())?;
        let (core_task, core_instance, core_source) = expanded_edge_facts[expanded];
        if task != core_task
            || step_tasks[source_step] != task
            || core_source != Some(step_nodes[source_step])
            || instance_locals[usize::try_from(step_instances[source_step]).map_err(|_| {
                BuildError::Validation(
                    "Static Workflow Plan Trace source instance is invalid".to_owned(),
                )
            })?] != core_instance
        {
            return validation("Static Workflow Plan Trace edge does not match its core edge");
        }
        let target = object(
            item.get("target").ok_or_else(|| {
                BuildError::Validation(
                    "Static Workflow Plan Trace edge target is required".to_owned(),
                )
            })?,
            "trace_structure.edges[].target",
        )?;
        let (target_complete, target_node) = match string_field(target, "kind")? {
            "complete" => (true, None),
            "step" => {
                let target_step = index_field(target, "step", step_tasks.len())?;
                if step_tasks[target_step] != task
                    || step_instances[target_step] != step_instances[source_step]
                {
                    return validation("Static Workflow Plan Trace edge target crosses ownership");
                }
                (false, Some(step_nodes[target_step]))
            }
            _ => return validation("Static Workflow Plan Trace edge target kind is invalid"),
        };
        let branch_order = item
            .get("branch_order")
            .map(|_| u32_field(item, "branch_order"))
            .transpose()?;
        let task_index = tasks.get_mut(&task).ok_or_else(|| {
            BuildError::Validation("Static Workflow Plan Trace edge has no task".to_owned())
        })?;
        task_index.edge_sources.push(step_nodes[source_step]);
        task_index.edge_instances.push(core_instance);
        task_index.edge_metadata.push(Some(TraceEdgeIndex {
            source: step_nodes[source_step],
            instance: core_instance,
            target_complete,
            target_node,
            branch_order,
        }));
    }
    if seen_expanded.len() != expected_edges {
        return validation("Static Workflow Plan Trace edge table is missing an edge");
    }
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "1.2/1.3 catalog parser performs one atomic expected-versus-actual producer audit"
)]
fn parse_trace_values(
    root: &Map<String, Value>,
    minor: u32,
    instance_tasks: &[u32],
    instance_locals: &[u32],
    expected_outputs: &BTreeMap<(u32, u32), ExpectedOutput>,
    expected_watches: &BTreeMap<u32, ExpectedWatch>,
) -> BuildResult<Vec<TraceValueIndex>> {
    let Some(value) = root.get("trace_values") else {
        return if minor == 1 || (expected_outputs.is_empty() && expected_watches.is_empty()) {
            Ok(Vec::new())
        } else {
            validation("Static Workflow Plan 1.2/1.3 `trace_values` is required")
        };
    };
    let values = value.as_array().ok_or_else(|| {
        BuildError::Validation("Static Workflow Plan `trace_values` must be an array".to_owned())
    })?;
    if minor == 1 {
        return values.is_empty().then(Vec::new).ok_or_else(|| {
            BuildError::Validation(
                "Static Workflow Plan 1.1 cannot contain `trace_values`".to_owned(),
            )
        });
    }
    if values.len() != expected_outputs.len() + expected_watches.len() {
        return validation("Static Workflow Plan Trace catalog is not closed");
    }
    let mut seen_outputs = BTreeSet::new();
    let mut seen_watches = BTreeSet::new();
    let mut catalog = Vec::with_capacity(values.len());
    for (expected_handle, value) in values.iter().enumerate() {
        let item = object(value, "trace_values[]")?;
        require_dense_handle(item, expected_handle, "trace_values")?;
        let task = u32_field(item, "task_handle")?;
        let instance = u32_field(item, "instance")?;
        if instance_tasks.get(instance as usize).copied() != Some(task) {
            return validation("Static Workflow Plan Trace value crosses task ownership");
        }
        let type_handle = u32_field(item, "type_handle")?;
        let encoded_bytes = decimal_u64_field(item, "encoded_bytes")?;
        let fragment_count = u16_field(item, "fragment_count")?;
        if type_handle == u32::MAX
            || encoded_bytes == 0
            || u64::from(fragment_count) != encoded_bytes.div_ceil(32)
        {
            return validation("Static Workflow Plan Trace value shape is invalid");
        }
        let area = string_field(item, "area")?;
        let image_offset_bytes = decimal_u64_field(item, "image_offset_bytes")?;
        image_offset_bytes
            .checked_add(encoded_bytes)
            .ok_or_else(|| {
                BuildError::Validation(
                    "Static Workflow Plan Trace value range overflows".to_owned(),
                )
            })?;
        let value_id = string_field(item, "value_id")?;
        let source = object(
            item.get("source").ok_or_else(|| {
                BuildError::Validation("Static Workflow Plan Trace source is required".to_owned())
            })?,
            "trace_values[].source",
        )?;
        let source_index = match string_field(source, "kind")? {
            "output" => {
                let step = u32_field(source, "step")?;
                let port = u32_field(source, "port")?;
                let expected = expected_outputs.get(&(step, port)).ok_or_else(|| {
                    BuildError::Validation("Trace Output source is not planned".to_owned())
                })?;
                if !seen_outputs.insert((step, port))
                    || task != expected.task
                    || instance != expected.instance
                    || value_id != expected.value_id
                    || type_handle != expected.type_handle
                    || area != expected.area
                    || image_offset_bytes != expected.image_offset_bytes
                    || encoded_bytes != expected.encoded_bytes
                {
                    return validation("Trace Output descriptor does not match its Action port");
                }
                TraceValueSourceIndex::Output {
                    node: expected.node,
                    action: expected.action,
                    port,
                }
            }
            "watch" => {
                let watch = u32_field(source, "watch")?;
                let expected = expected_watches.get(&watch).ok_or_else(|| {
                    BuildError::Validation("Trace Watch source is not planned".to_owned())
                })?;
                if !seen_watches.insert(watch)
                    || task != expected.task
                    || value_id != expected.value_id
                    || encoded_bytes != expected.encoded_bytes
                    || fragment_count != expected.fragment_count
                {
                    return validation("Trace Watch descriptor does not match its planned watch");
                }
                TraceValueSourceIndex::Watch
            }
            _ => return validation("Static Workflow Plan Trace source kind is invalid"),
        };
        catalog.push(TraceValueIndex {
            task,
            instance: *instance_locals.get(instance as usize).ok_or_else(|| {
                BuildError::Validation(
                    "Static Workflow Plan Trace instance is outside its task-local table"
                        .to_owned(),
                )
            })?,
            type_handle,
            encoded_bytes,
            fragment_count,
            source: source_index,
        });
    }
    if seen_outputs.len() != expected_outputs.len() || seen_watches.len() != expected_watches.len()
    {
        return validation("Static Workflow Plan Trace catalog is missing a producer");
    }
    Ok(catalog)
}

fn fragment_matches(record: WorkflowTraceRecord, descriptor: &TraceValueIndex) -> bool {
    let fragment = record.fragment();
    if record.kind() == WorkflowTraceEventKind::OutputStaged && record.detail() == 2 {
        return fragment.count == 0 && fragment.index == 0 && fragment.bytes == 0;
    }
    let index = u64::from(fragment.index);
    let count = u64::from(fragment.count);
    let expected_count = u64::from(descriptor.fragment_count);
    let expected_bytes = descriptor
        .encoded_bytes
        .saturating_sub(index.saturating_mul(32))
        .min(32);
    count == expected_count
        && index < count
        && u64::from(fragment.bytes) == expected_bytes
        && fragment.digest.is_some()
}

fn object<'value>(value: &'value Value, context: &str) -> BuildResult<&'value Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| BuildError::Validation(format!("{context} must be a JSON object")))
}

fn array_field<'value>(
    object: &'value Map<String, Value>,
    field: &str,
) -> BuildResult<&'value [Value]> {
    object
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| {
            BuildError::Validation(format!("Static Workflow Plan `{field}` must be an array"))
        })
}

fn u32_field(object: &Map<String, Value>, field: &str) -> BuildResult<u32> {
    let value = object.get(field).and_then(Value::as_u64).ok_or_else(|| {
        BuildError::Validation(format!("Static Workflow Plan `{field}` must be a u32"))
    })?;
    u32::try_from(value).map_err(|_| {
        BuildError::Validation(format!("Static Workflow Plan `{field}` is outside u32"))
    })
}

fn u16_field(object: &Map<String, Value>, field: &str) -> BuildResult<u16> {
    u16::try_from(u32_field(object, field)?).map_err(|_| {
        BuildError::Validation(format!("Static Workflow Plan `{field}` is outside u16"))
    })
}

fn bool_field(object: &Map<String, Value>, field: &str) -> BuildResult<bool> {
    object.get(field).and_then(Value::as_bool).ok_or_else(|| {
        BuildError::Validation(format!("Static Workflow Plan `{field}` must be a boolean"))
    })
}

fn sorted_u32_set(object: &Map<String, Value>, field: &str) -> BuildResult<BTreeSet<u32>> {
    let values = array_field(object, field)?;
    let mut result = BTreeSet::new();
    let mut previous = None;
    for value in values {
        let raw = value.as_u64().ok_or_else(|| {
            BuildError::Validation(format!(
                "Static Workflow Plan `{field}` entries must be u32"
            ))
        })?;
        let current = u32::try_from(raw).map_err(|_| {
            BuildError::Validation(format!(
                "Static Workflow Plan `{field}` entry is outside u32"
            ))
        })?;
        if previous.is_some_and(|previous| previous >= current) || !result.insert(current) {
            return validation(&format!(
                "Static Workflow Plan `{field}` must be strictly sorted and unique"
            ));
        }
        previous = Some(current);
    }
    Ok(result)
}

fn string_field<'value>(
    object: &'value Map<String, Value>,
    field: &str,
) -> BuildResult<&'value str> {
    object.get(field).and_then(Value::as_str).ok_or_else(|| {
        BuildError::Validation(format!("Static Workflow Plan `{field}` must be a string"))
    })
}

fn decimal_u64_field(object: &Map<String, Value>, field: &str) -> BuildResult<u64> {
    string_field(object, field)?.parse::<u64>().map_err(|_| {
        BuildError::Validation(format!(
            "Static Workflow Plan `{field}` must be a decimal u64 string"
        ))
    })
}

fn value_type_bytes(value_type: &str) -> BuildResult<u64> {
    match value_type {
        "bool" | "sint" | "usint" => Ok(1),
        "int" | "uint" => Ok(2),
        "dint" | "udint" | "real" => Ok(4),
        "lint" | "ulint" | "lreal" => Ok(8),
        _ => validation("Static Workflow Plan Action port type is invalid"),
    }
}

fn value_type_handle(value_type: &str) -> BuildResult<u32> {
    match value_type {
        "bool" => Ok(1),
        "sint" => Ok(2),
        "int" => Ok(3),
        "dint" => Ok(4),
        "lint" => Ok(5),
        "usint" => Ok(6),
        "uint" => Ok(7),
        "udint" => Ok(8),
        "ulint" => Ok(9),
        "real" => Ok(10),
        "lreal" => Ok(11),
        _ => validation("Static Workflow Plan Action port type is invalid"),
    }
}

fn index_field(object: &Map<String, Value>, field: &str, length: usize) -> BuildResult<usize> {
    let value = usize::try_from(u32_field(object, field)?).map_err(|_| {
        BuildError::Validation(format!(
            "Static Workflow Plan `{field}` is not representable"
        ))
    })?;
    if value < length {
        Ok(value)
    } else {
        validation("Static Workflow Plan handle is outside its referenced table")
    }
}

fn require_dense_handle(
    object: &Map<String, Value>,
    expected: usize,
    table: &str,
) -> BuildResult<()> {
    if usize::try_from(u32_field(object, "handle")?).ok() == Some(expected) {
        Ok(())
    } else {
        validation(&format!(
            "Static Workflow Plan `{table}` handles are not dense"
        ))
    }
}

fn validation<T>(message: &str) -> BuildResult<T> {
    Err(BuildError::Validation(message.to_owned()))
}

fn format_record(record: WorkflowTraceRecord) -> String {
    format!(
        "event={} task={} task_epoch={} release={} commit={}->{} instance={} kind={:?} detail={} node={:?} edge={:?} source={:?} value={:?} type={:?} branch={:?} execution={:?} fault={:?} fragment={}/{} bytes={}",
        record.event_sequence().get(),
        record.task_handle().get(),
        record.task_epoch().get(),
        record.release_sequence().get(),
        record.commit_before().get(),
        record.commit_after().get(),
        record.workflow_instance_handle(),
        record.kind(),
        record.detail(),
        record.node_handle(),
        record.edge_handle(),
        record.source_handle(),
        record.value_handle(),
        record.type_handle(),
        record.branch_order(),
        record.execution_order(),
        record.fault(),
        record.fragment().index,
        record.fragment().count,
        record.fragment().bytes,
    )
}

fn read(path: &Path) -> BuildResult<Vec<u8>> {
    fs::read(path).map_err(|source| BuildError::Io {
        operation: "read Workflow Trace",
        path: path.to_path_buf(),
        source,
    })
}

fn read_plan(path: &Path) -> BuildResult<Vec<u8>> {
    fs::read(path).map_err(|source| BuildError::Io {
        operation: "read Static Workflow Plan",
        path: path.to_path_buf(),
        source,
    })
}

fn parse<'bytes>(path: &Path, bytes: &'bytes [u8]) -> BuildResult<WorkflowTraceFileView<'bytes>> {
    WorkflowTraceFileView::parse(bytes).map_err(|source| workflow_error(path, source))
}

fn workflow_error(
    path: &Path,
    source: aurora_control_contracts::WorkflowTraceCodecError,
) -> BuildError {
    BuildError::WorkflowTrace {
        path: path.to_path_buf(),
        source,
    }
}

const fn status(complete: bool) -> &'static str {
    if complete { "complete" } else { "incomplete" }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _result = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use aurora_control_contracts::{
        CommitSequence, EventSequence, MissOutcome, ReleaseSequence, TaskEpoch,
        WorkflowTraceEventKind, WorkflowTraceFileHeader, WorkflowTraceRecord,
        WorkflowTraceRecordBytes, WorkflowTraceValueFragment, WorkflowTraceVersion,
    };
    use aurora_types::{BootEpochId, LocalHandle};
    use sha2::{Digest, Sha256};

    use super::{compare_bytes, decode_bytes, replay_bytes};

    #[test]
    fn tools_validate_compare_and_mark_drop_incomplete() -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":1},"instances":[{"handle":0,"task_handle":0}],"steps":[],"edges":[],"node_resources":[],"watches":[]}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        let complete = one_terminal_file(0, 0, digest)?;
        assert!(decode_bytes(path, &complete)?.contains("status=complete"));
        let replay = replay_bytes(path, &complete, plan_path, plan)?;
        assert!(replay.contains("traceability=unverified"));
        assert!(replay.contains("releases=1 committed=1"));
        assert!(compare_bytes(path, &complete, path, &complete)?.contains("match"));

        let plan_1_3 = br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[],"edges":[],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[],"root_instances":[0],"nodes":[],"edges":[]}}"#;
        let digest_1_3: [u8; 32] = Sha256::digest(plan_1_3).into();
        let complete_1_3 = empty_root_file(digest_1_3)?;
        assert!(
            replay_bytes(path, &complete_1_3, plan_path, plan_1_3)?
                .contains("traceability=traceable")
        );

        let incomplete = one_terminal_file(0, 1, digest)?;
        assert!(
            replay_bytes(path, &incomplete, plan_path, plan)?.contains("traceability=incomplete")
        );
        assert!(compare_bytes(path, &complete, path, &incomplete).is_err());

        let different = one_terminal_file(1, 0, digest)?;
        assert!(compare_bytes(path, &complete, path, &different).is_err());
        assert!(replay_bytes(path, &complete, plan_path, b"{}").is_err());
        Ok(())
    }

    #[test]
    fn replay_uses_static_plan_1_2_value_catalog_exactly() -> Result<(), Box<dyn std::error::Error>>
    {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":2},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0}],"edges":[],"node_resources":[{"step":0,"action_binding":{"ports":[{"port":0,"direction":"output","slot":{"target_id":"018f0000-0000-7000-8000-000000000001","value_type":"dint","area":"output","image_offset_bytes":"4"}}]}}],"watches":[{"handle":0,"task_handle":0,"value_id":"018f0000-0000-7000-8000-000000000002","encoded_bytes":"4","fragment_count":1}],"trace_values":[{"handle":0,"task_handle":0,"instance":0,"value_id":"018f0000-0000-7000-8000-000000000001","source":{"kind":"output","step":0,"port":0},"type_handle":4,"area":"output","image_offset_bytes":"4","encoded_bytes":"4","fragment_count":1},{"handle":1,"task_handle":0,"instance":0,"value_id":"018f0000-0000-7000-8000-000000000002","source":{"kind":"watch","watch":0},"type_handle":77,"area":"state","image_offset_bytes":"8","encoded_bytes":"4","fragment_count":1}]}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        let valid = value_file(digest, 4)?;
        let replay = replay_bytes(path, &valid, plan_path, plan)?;
        assert!(replay.contains("output_events=1"));

        let missing_output = remove_record(&valid, 3)?;
        assert!(replay_bytes(path, &missing_output, plan_path, plan).is_err());
        let missing_watch = remove_record(&valid, 4)?;
        assert!(replay_bytes(path, &missing_watch, plan_path, plan).is_err());

        let wrong_type = value_file(digest, 5)?;
        assert!(replay_bytes(path, &wrong_type, plan_path, plan).is_err());

        let missing_catalog = br#"{"schema_version":{"major":1,"minor":2},"instances":[{"handle":0,"task_handle":0}],"steps":[],"edges":[],"node_resources":[],"watches":[]}"#;
        let missing_digest: [u8; 32] = Sha256::digest(missing_catalog).into();
        let terminal = one_terminal_file(0, 0, missing_digest)?;
        assert!(replay_bytes(path, &terminal, plan_path, missing_catalog).is_ok());
        Ok(())
    }

    #[test]
    fn replay_rejects_cross_instance_edge_ownership() -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":1},"instances":[{"handle":0,"task_handle":0},{"handle":1,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0},{"handle":1,"task_handle":0,"instance":1,"task_execution_order":1}],"edges":[{"handle":0,"instance":1,"edge":0,"source_step":1}],"node_resources":[],"watches":[]}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        let trace = cross_instance_edge_file(digest)?;
        assert!(replay_bytes(path, &trace, plan_path, plan).is_err());
        Ok(())
    }

    #[test]
    fn replay_rejects_edge_emitted_from_the_wrong_node() -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":1},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0},{"handle":1,"task_handle":0,"instance":0,"task_execution_order":1}],"edges":[{"handle":0,"instance":0,"edge":0,"source_step":1}],"node_resources":[],"watches":[]}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        let trace = cross_instance_edge_file(digest)?;

        assert!(replay_bytes(path, &trace, plan_path, plan).is_err());
        Ok(())
    }

    #[test]
    fn replay_indexes_edge_sources_in_runtime_node_order() -> Result<(), Box<dyn std::error::Error>>
    {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":1},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0},{"handle":1,"task_handle":0,"instance":0,"task_execution_order":1}],"edges":[{"handle":0,"instance":0,"edge":0,"source_step":1},{"handle":1,"instance":0,"edge":1,"source_step":0}],"node_resources":[],"watches":[]}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        let trace = cross_instance_edge_file(digest)?;

        let replay = replay_bytes(path, &trace, plan_path, plan)?;
        assert!(replay.contains("control task=0 release=0 node=0:0 edge=0"));
        Ok(())
    }

    #[test]
    fn replay_maps_global_plan_instances_to_task_local_trace_handles()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":2},"instances":[{"handle":0,"task_handle":0},{"handle":1,"task_handle":1}],"steps":[{"handle":0,"task_handle":1,"instance":1,"task_execution_order":0}],"edges":[],"node_resources":[{"step":0,"action_binding":{"ports":[{"port":0,"direction":"output","slot":{"target_id":"018f0000-0000-7000-8000-000000000001","value_type":"dint","area":"output","image_offset_bytes":"0"}}]}}],"watches":[],"trace_values":[{"handle":0,"task_handle":1,"instance":1,"value_id":"018f0000-0000-7000-8000-000000000001","source":{"kind":"output","step":0,"port":0},"type_handle":4,"area":"output","image_offset_bytes":"0","encoded_bytes":"4","fragment_count":1}]}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        let trace = task_one_value_file(digest)?;
        let replay = replay_bytes(path, &trace, plan_path, plan)?;
        assert!(replay.contains("output task=1 release=0 node=0:0"));
        Ok(())
    }

    #[test]
    fn replay_accepts_deadline_discard_without_watch_capture()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":2},"instances":[{"handle":0,"task_handle":0}],"steps":[],"edges":[],"node_resources":[],"watches":[{"handle":0,"task_handle":0,"value_id":"018f0000-0000-7000-8000-000000000002","encoded_bytes":"4","fragment_count":1}],"trace_values":[{"handle":0,"task_handle":0,"instance":0,"value_id":"018f0000-0000-7000-8000-000000000002","source":{"kind":"watch","watch":0},"type_handle":77,"area":"state","image_offset_bytes":"8","encoded_bytes":"4","fragment_count":1}]}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        let trace = deadline_discard_file(digest)?;

        let replay = replay_bytes(path, &trace, plan_path, plan)?;
        assert!(replay.contains("discarded=1"));
        Ok(())
    }

    #[test]
    fn replay_rejects_shape_compatible_structural_event_swap()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"node":0,"task_execution_order":0}],"edges":[],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"wait_cycles"},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[]}}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        let valid = structural_file(digest, WorkflowTraceEventKind::WaitObserved, 1)?;
        assert!(replay_bytes(path, &valid, plan_path, plan)?.contains("traceability=traceable"));

        let swapped = structural_file(digest, WorkflowTraceEventKind::JoinSatisfied, 1)?;
        assert!(replay_bytes(path, &swapped, plan_path, plan).is_err());
        Ok(())
    }

    #[test]
    fn replay_rejects_missing_fork_activation_after_sequence_repair()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0},{"handle":1,"task_handle":0,"instance":0,"task_execution_order":1},{"handle":2,"task_handle":0,"instance":0,"task_execution_order":2}],"edges":[{"handle":0,"instance":0,"edge":0,"source_step":0},{"handle":1,"instance":0,"edge":1,"source_step":0}],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"fork","branch_orders":[0,1]},"cancellation_boundary":false,"cancellation_branch_orders":[]},{"step":1,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]},{"step":2,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[{"task_handle":0,"runtime_edge_handle":0,"expanded_edge":0,"source_step":0,"target":{"kind":"step","step":1},"branch_order":0},{"task_handle":0,"runtime_edge_handle":1,"expanded_edge":1,"source_step":0,"target":{"kind":"step","step":2},"branch_order":1}]}}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        let valid = fork_file(digest, true)?;
        assert!(replay_bytes(path, &valid, plan_path, plan)?.contains("traceability=traceable"));

        let repaired_sequences = fork_file(digest, false)?;
        assert!(replay_bytes(path, &repaired_sequences, plan_path, plan).is_err());
        Ok(())
    }

    #[test]
    fn replay_rejects_missing_or_duplicate_join_closure() -> Result<(), Box<dyn std::error::Error>>
    {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0},{"handle":1,"task_handle":0,"instance":0,"task_execution_order":1}],"edges":[{"handle":0,"instance":0,"edge":0,"source_step":0}],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"merge"},"cancellation_boundary":false,"cancellation_branch_orders":[]},{"step":1,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[{"task_handle":0,"runtime_edge_handle":0,"expanded_edge":0,"source_step":0,"target":{"kind":"step","step":1}}]}}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        assert!(
            replay_bytes(path, &merge_file(digest, 1)?, plan_path, plan)?
                .contains("traceability=traceable")
        );
        assert!(replay_bytes(path, &merge_file(digest, 0)?, plan_path, plan).is_err());
        assert!(replay_bytes(path, &merge_file(digest, 2)?, plan_path, plan).is_err());
        Ok(())
    }

    #[test]
    fn replay_binds_root_completion_to_release_lifecycle() -> Result<(), Box<dyn std::error::Error>>
    {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0}],"edges":[{"handle":0,"instance":0,"edge":0,"source_step":0}],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[{"task_handle":0,"runtime_edge_handle":0,"expanded_edge":0,"source_step":0,"target":{"kind":"complete"}}]}}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        let valid = root_lifecycle_file(digest, true, true)?;
        assert!(replay_bytes(path, &valid, plan_path, plan)?.contains("traceability=traceable"));
        let omitted = root_lifecycle_file(digest, true, false)?;
        assert!(replay_bytes(path, &omitted, plan_path, plan).is_err());
        let forged_after_retain = root_lifecycle_file(digest, false, true)?;
        assert!(replay_bytes(path, &forged_after_retain, plan_path, plan).is_err());
        Ok(())
    }

    #[test]
    fn replay_carries_the_active_set_across_committed_releases()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0},{"handle":1,"task_handle":0,"instance":0,"task_execution_order":1}],"edges":[{"handle":0,"instance":0,"edge":0,"source_step":0},{"handle":1,"instance":0,"edge":1,"source_step":1}],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]},{"step":1,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[{"task_handle":0,"runtime_edge_handle":0,"expanded_edge":0,"source_step":0,"target":{"kind":"step","step":1}},{"task_handle":0,"runtime_edge_handle":1,"expanded_edge":1,"source_step":1,"target":{"kind":"complete"}}]}}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();
        let valid = two_release_active_set_file(digest, true)?;
        assert!(replay_bytes(path, &valid, plan_path, plan)?.contains("traceability=traceable"));

        let repaired_sequences = two_release_active_set_file(digest, false)?;
        assert!(replay_bytes(path, &repaired_sequences, plan_path, plan).is_err());
        Ok(())
    }

    #[test]
    fn replay_seeds_the_first_release_from_the_signed_initial_active_set()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0},{"handle":1,"task_handle":0,"instance":0,"task_execution_order":1}],"edges":[{"handle":0,"instance":0,"edge":0,"source_step":0},{"handle":1,"instance":0,"edge":1,"source_step":1}],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]},{"step":1,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[{"task_handle":0,"runtime_edge_handle":0,"expanded_edge":0,"source_step":0,"target":{"kind":"complete"}},{"task_handle":0,"runtime_edge_handle":1,"expanded_edge":1,"source_step":1,"target":{"kind":"complete"}}]}}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();

        let valid = first_release_active_set_file(digest, 0, 0)?;
        assert!(replay_bytes(path, &valid, plan_path, plan)?.contains("traceability=traceable"));
        let forged = first_release_active_set_file(digest, 1, 1)?;
        assert!(replay_bytes(path, &forged, plan_path, plan).is_err());
        Ok(())
    }

    #[test]
    fn replay_requires_the_signed_child_entry_after_subworkflow_activation()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let plan = br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0},{"handle":1,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0,"child_instance":1},{"handle":1,"task_handle":0,"instance":1,"task_execution_order":1},{"handle":2,"task_handle":0,"instance":1,"task_execution_order":2}],"edges":[],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0,1],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"subworkflow","call_handle":0,"child_instance":1},"cancellation_boundary":false,"cancellation_branch_orders":[]},{"step":1,"node_kind":{"kind":"wait_cycles"},"cancellation_boundary":false,"cancellation_branch_orders":[]},{"step":2,"node_kind":{"kind":"wait_cycles"},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[]}}"#;
        let digest: [u8; 32] = Sha256::digest(plan).into();

        let valid = child_entry_active_set_file(digest, 1)?;
        assert!(replay_bytes(path, &valid, plan_path, plan)?.contains("traceability=traceable"));
        let forged = child_entry_active_set_file(digest, 2)?;
        assert!(replay_bytes(path, &forged, plan_path, plan).is_err());
        Ok(())
    }

    #[test]
    fn replay_rejects_wait_and_join_subtype_mismatches() -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        for (plan, kind, detail) in [
            (
                br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0}],"edges":[],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"wait_condition","has_timeout":true},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[]}}"#.as_slice(),
                WorkflowTraceEventKind::WaitObserved,
                6,
            ),
            (
                br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0}],"edges":[],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"wait_condition","has_timeout":false},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[]}}"#.as_slice(),
                WorkflowTraceEventKind::WaitObserved,
                5,
            ),
            (
                br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0}],"edges":[],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"join_all","branch_orders":[0,1]},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[]}}"#.as_slice(),
                WorkflowTraceEventKind::JoinSatisfied,
                3,
            ),
        ] {
            let digest: [u8; 32] = Sha256::digest(plan).into();
            let trace = structural_file(digest, kind, detail)?;
            assert!(replay_bytes(path, &trace, plan_path, plan).is_err());
        }
        Ok(())
    }

    #[test]
    fn replay_rejects_open_or_cross_task_plan_1_3_structure()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = Path::new("memory.workflow-trace");
        let plan_path = Path::new("memory.static-plan.json");
        let invalid_plans: [&[u8]; 6] = [
            br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[],"edges":[],"node_resources":[],"watches":[]}"#,
            br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0}],"edges":[],"node_resources":[],"watches":[],"trace_structure":{"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[]}}"#,
            br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[],"edges":[],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[],"root_instances":[0,0],"nodes":[],"edges":[]}}"#,
            br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0}],"steps":[],"edges":[],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[]}}"#,
            br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0},{"handle":1,"task_handle":1}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0},{"handle":1,"task_handle":1,"instance":1,"task_execution_order":0}],"edges":[{"handle":0,"instance":0,"edge":0,"source_step":0}],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0,1],"root_instances":[0,1],"nodes":[{"step":0,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]},{"step":1,"node_kind":{"kind":"action"},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[{"task_handle":0,"runtime_edge_handle":0,"expanded_edge":0,"source_step":0,"target":{"kind":"step","step":1}}]}}"#,
            br#"{"schema_version":{"major":1,"minor":3},"instances":[{"handle":0,"task_handle":0},{"handle":1,"task_handle":0}],"steps":[{"handle":0,"task_handle":0,"instance":0,"task_execution_order":0,"child_instance":1},{"handle":1,"task_handle":0,"instance":1,"task_execution_order":1}],"edges":[],"node_resources":[],"watches":[],"trace_structure":{"initial_active":[0],"root_instances":[0],"nodes":[{"step":0,"node_kind":{"kind":"subworkflow","call_handle":0,"child_instance":1},"cancellation_boundary":false,"cancellation_branch_orders":[]},{"step":1,"node_kind":{"kind":"wait_cycles"},"cancellation_boundary":false,"cancellation_branch_orders":[]}],"edges":[]}}"#,
        ];
        for plan in invalid_plans {
            let digest: [u8; 32] = Sha256::digest(plan).into();
            let trace = one_terminal_file(0, 0, digest)?;
            assert!(replay_bytes(path, &trace, plan_path, plan).is_err());
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_lines,
        reason = "test fixture spells out every contract field for canonical release ordering"
    )]
    fn value_file(
        plan_digest: [u8; 32],
        output_type_handle: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let initialized = initialized_record(epoch, LocalHandle::ZERO, 0)?;
        let deadline = WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::DeadlineObserved,
            1,
            LocalHandle::ZERO,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(1),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::ZERO,
            WorkflowTraceValueFragment::ABSENT,
        )?;
        let node = WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::NodeExecuted,
            0,
            LocalHandle::ZERO,
            0,
            Some(0),
            None,
            None,
            None,
            None,
            Some(0),
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(2),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::ZERO,
            WorkflowTraceValueFragment::ABSENT,
        )?;
        let output = WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::OutputStaged,
            2,
            LocalHandle::ZERO,
            0,
            Some(0),
            None,
            Some(0),
            Some(0),
            None,
            Some(0),
            Some(output_type_handle),
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(3),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::ZERO,
            WorkflowTraceValueFragment::ABSENT,
        )?;
        let digest: [u8; 32] = Sha256::digest([0_u8; 4]).into();
        let watch = WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::WatchedValue,
            0,
            LocalHandle::ZERO,
            0,
            None,
            None,
            None,
            Some(1),
            None,
            None,
            Some(77),
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(4),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::ZERO,
            WorkflowTraceValueFragment {
                index: 0,
                count: 1,
                bytes: 4,
                digest: Some(digest),
                storage: [0_u8; 32],
            },
        )?;
        let terminal = WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::ScanCommitted,
            0,
            LocalHandle::ZERO,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(5),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::new(1),
            WorkflowTraceValueFragment::ABSENT,
        )?;
        let mut bytes = Vec::from(WorkflowTraceFileHeader::new(epoch, plan_digest, 6, 0).encode());
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(initialized).as_bytes());
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(deadline).as_bytes());
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(node).as_bytes());
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(output).as_bytes());
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(watch).as_bytes());
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(terminal).as_bytes());
        Ok(bytes)
    }

    fn one_terminal_file(
        sequence: u64,
        dropped: u64,
        plan_digest: [u8; 32],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let initialized = initialized_record(epoch, LocalHandle::ZERO, sequence)?;
        let record = WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::ScanCommitted,
            0,
            LocalHandle::ZERO,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(sequence.checked_add(1).ok_or("sequence overflow")?),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::new(1),
            WorkflowTraceValueFragment::ABSENT,
        )?;
        let mut bytes =
            Vec::from(WorkflowTraceFileHeader::new(epoch, plan_digest, 2, dropped).encode());
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(initialized).as_bytes());
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(record).as_bytes());
        Ok(bytes)
    }

    fn empty_root_file(plan_digest: [u8; 32]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let initialized = initialized_record(epoch, LocalHandle::ZERO, 0)?;
        let completed = WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::WorkflowCompleted,
            0,
            LocalHandle::ZERO,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(1),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::ZERO,
            WorkflowTraceValueFragment::ABSENT,
        )?;
        let terminal = WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::ScanCommitted,
            0,
            LocalHandle::ZERO,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(2),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::new(1),
            WorkflowTraceValueFragment::ABSENT,
        )?;
        let mut bytes = Vec::from(WorkflowTraceFileHeader::new(epoch, plan_digest, 3, 0).encode());
        for record in [initialized, completed, terminal] {
            bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(record).as_bytes());
        }
        Ok(bytes)
    }

    fn structural_file(
        plan_digest: [u8; 32],
        kind: WorkflowTraceEventKind,
        detail: u16,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let records = [
            initialized_record(epoch, LocalHandle::ZERO, 0)?,
            WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::NodeExecuted,
                0,
                LocalHandle::ZERO,
                0,
                Some(0),
                None,
                None,
                None,
                None,
                Some(0),
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(1),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::ZERO,
                WorkflowTraceValueFragment::ABSENT,
            )?,
            WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                kind,
                detail,
                LocalHandle::ZERO,
                0,
                Some(0),
                None,
                None,
                None,
                None,
                Some(0),
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(2),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::ZERO,
                WorkflowTraceValueFragment::ABSENT,
            )?,
            WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::ScanCommitted,
                0,
                LocalHandle::ZERO,
                0,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(3),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::new(1),
                WorkflowTraceValueFragment::ABSENT,
            )?,
        ];
        let mut bytes = Vec::from(
            WorkflowTraceFileHeader::new(epoch, plan_digest, u64::try_from(records.len())?, 0)
                .encode(),
        );
        for record in records {
            bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(record).as_bytes());
        }
        Ok(bytes)
    }

    fn fork_file(
        plan_digest: [u8; 32],
        include_second_activation: bool,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let mut records = vec![initialized_record(epoch, LocalHandle::ZERO, 0)?];
        {
            let mut push = |kind, edge, branch| -> Result<(), Box<dyn std::error::Error>> {
                let sequence = u64::try_from(records.len())?;
                records.push(WorkflowTraceRecord::new(
                    WorkflowTraceVersion::V1_0,
                    kind,
                    0,
                    LocalHandle::ZERO,
                    0,
                    Some(0),
                    edge,
                    None,
                    None,
                    branch,
                    Some(0),
                    None,
                    None,
                    epoch,
                    TaskEpoch::new(1)?,
                    EventSequence::new(sequence),
                    ReleaseSequence::ZERO,
                    CommitSequence::ZERO,
                    CommitSequence::ZERO,
                    WorkflowTraceValueFragment::ABSENT,
                )?);
                Ok(())
            };
            push(WorkflowTraceEventKind::NodeExecuted, None, None)?;
            push(WorkflowTraceEventKind::TransitionTaken, Some(0), None)?;
            push(WorkflowTraceEventKind::TransitionTaken, Some(1), None)?;
            push(WorkflowTraceEventKind::ForkActivated, Some(0), Some(0))?;
            if include_second_activation {
                push(WorkflowTraceEventKind::ForkActivated, Some(1), Some(1))?;
            }
        }
        records.push(WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::ScanCommitted,
            0,
            LocalHandle::ZERO,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(u64::try_from(records.len())?),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::new(1),
            WorkflowTraceValueFragment::ABSENT,
        )?);
        let mut bytes = Vec::from(
            WorkflowTraceFileHeader::new(epoch, plan_digest, u64::try_from(records.len())?, 0)
                .encode(),
        );
        for record in records {
            bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(record).as_bytes());
        }
        Ok(bytes)
    }

    fn merge_file(
        plan_digest: [u8; 32],
        join_count: usize,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let mut records = vec![
            initialized_record(epoch, LocalHandle::ZERO, 0)?,
            structural_record(epoch, WorkflowTraceEventKind::NodeExecuted, 1, None)?,
            structural_record(epoch, WorkflowTraceEventKind::TransitionTaken, 2, Some(0))?,
        ];
        for _ in 0..join_count {
            records.push(WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::JoinSatisfied,
                3,
                LocalHandle::ZERO,
                0,
                Some(0),
                None,
                None,
                None,
                None,
                Some(0),
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(u64::try_from(records.len())?),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::ZERO,
                WorkflowTraceValueFragment::ABSENT,
            )?);
        }
        records.push(WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::ScanCommitted,
            0,
            LocalHandle::ZERO,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(u64::try_from(records.len())?),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::new(1),
            WorkflowTraceValueFragment::ABSENT,
        )?);
        let mut bytes = Vec::from(
            WorkflowTraceFileHeader::new(epoch, plan_digest, u64::try_from(records.len())?, 0)
                .encode(),
        );
        for record in records {
            bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(record).as_bytes());
        }
        Ok(bytes)
    }

    fn structural_record(
        epoch: BootEpochId,
        kind: WorkflowTraceEventKind,
        sequence: u64,
        edge: Option<u32>,
    ) -> Result<WorkflowTraceRecord, Box<dyn std::error::Error>> {
        Ok(WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            kind,
            0,
            LocalHandle::ZERO,
            0,
            Some(0),
            edge,
            None,
            None,
            None,
            Some(0),
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(sequence),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::ZERO,
            WorkflowTraceValueFragment::ABSENT,
        )?)
    }

    fn root_lifecycle_file(
        plan_digest: [u8; 32],
        take_complete: bool,
        emit_completed: bool,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let mut records = vec![
            initialized_record(epoch, LocalHandle::ZERO, 0)?,
            structural_record(epoch, WorkflowTraceEventKind::NodeExecuted, 1, None)?,
        ];
        if take_complete {
            records.push(structural_record(
                epoch,
                WorkflowTraceEventKind::TransitionTaken,
                u64::try_from(records.len())?,
                Some(0),
            )?);
            records.push(structural_record(
                epoch,
                WorkflowTraceEventKind::CompletionRequested,
                u64::try_from(records.len())?,
                None,
            )?);
        }
        if emit_completed {
            records.push(WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::WorkflowCompleted,
                0,
                LocalHandle::ZERO,
                0,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(u64::try_from(records.len())?),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::ZERO,
                WorkflowTraceValueFragment::ABSENT,
            )?);
        }
        records.push(WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::ScanCommitted,
            0,
            LocalHandle::ZERO,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(u64::try_from(records.len())?),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::new(1),
            WorkflowTraceValueFragment::ABSENT,
        )?);
        let mut bytes = Vec::from(
            WorkflowTraceFileHeader::new(epoch, plan_digest, u64::try_from(records.len())?, 0)
                .encode(),
        );
        for record in records {
            bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(record).as_bytes());
        }
        Ok(bytes)
    }

    fn first_release_active_set_file(
        plan_digest: [u8; 32],
        node: u32,
        edge: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let mut records = vec![initialized_record(epoch, LocalHandle::ZERO, 0)?];
        for (kind, event_node, event_edge) in [
            (WorkflowTraceEventKind::NodeExecuted, Some(node), None),
            (
                WorkflowTraceEventKind::TransitionTaken,
                Some(node),
                Some(edge),
            ),
            (
                WorkflowTraceEventKind::CompletionRequested,
                Some(node),
                None,
            ),
            (WorkflowTraceEventKind::WorkflowCompleted, None, None),
            (WorkflowTraceEventKind::ScanCommitted, None, None),
        ] {
            let committed = kind == WorkflowTraceEventKind::ScanCommitted;
            records.push(WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                kind,
                0,
                LocalHandle::ZERO,
                0,
                event_node,
                event_edge,
                None,
                None,
                None,
                event_node,
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(u64::try_from(records.len())?),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                if committed {
                    CommitSequence::new(1)
                } else {
                    CommitSequence::ZERO
                },
                WorkflowTraceValueFragment::ABSENT,
            )?);
        }
        let mut bytes = Vec::from(
            WorkflowTraceFileHeader::new(epoch, plan_digest, u64::try_from(records.len())?, 0)
                .encode(),
        );
        for record in records {
            bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(record).as_bytes());
        }
        Ok(bytes)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "fixture spells out two releases and the child activation identity"
    )]
    fn child_entry_active_set_file(
        plan_digest: [u8; 32],
        child_node: u32,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let child = 1_u32;
        let mut records = vec![initialized_record(epoch, LocalHandle::ZERO, 0)?];
        let mut push = |kind,
                        detail,
                        instance,
                        release,
                        commit_before,
                        commit_after,
                        node,
                        source|
         -> Result<(), Box<dyn std::error::Error>> {
            let sequence = u64::try_from(records.len())?;
            records.push(WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                kind,
                detail,
                LocalHandle::ZERO,
                instance,
                node,
                None,
                source,
                None,
                None,
                node,
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(sequence),
                ReleaseSequence::new(release),
                CommitSequence::new(commit_before),
                CommitSequence::new(commit_after),
                WorkflowTraceValueFragment::ABSENT,
            )?);
            Ok(())
        };
        push(
            WorkflowTraceEventKind::NodeExecuted,
            0,
            0,
            0,
            0,
            0,
            Some(0),
            None,
        )?;
        push(
            WorkflowTraceEventKind::SubworkflowActivated,
            0,
            child,
            0,
            0,
            0,
            Some(0),
            Some(0),
        )?;
        push(
            WorkflowTraceEventKind::ScanCommitted,
            0,
            0,
            0,
            0,
            1,
            None,
            None,
        )?;
        push(
            WorkflowTraceEventKind::NodeExecuted,
            0,
            child,
            1,
            1,
            1,
            Some(child_node),
            None,
        )?;
        push(
            WorkflowTraceEventKind::WaitObserved,
            1,
            child,
            1,
            1,
            1,
            Some(child_node),
            None,
        )?;
        push(
            WorkflowTraceEventKind::ScanCommitted,
            0,
            child,
            1,
            1,
            2,
            None,
            None,
        )?;
        let mut bytes = Vec::from(
            WorkflowTraceFileHeader::new(epoch, plan_digest, u64::try_from(records.len())?, 0)
                .encode(),
        );
        for record in records {
            bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(record).as_bytes());
        }
        Ok(bytes)
    }

    fn two_release_active_set_file(
        plan_digest: [u8; 32],
        include_first_transition: bool,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let mut records = vec![initialized_record(epoch, LocalHandle::ZERO, 0)?];
        let mut push = |kind,
                        release: u64,
                        commit_before: u64,
                        commit_after: u64,
                        node: Option<u32>,
                        edge: Option<u32>|
         -> Result<(), Box<dyn std::error::Error>> {
            let sequence = u64::try_from(records.len())?;
            records.push(WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                kind,
                0,
                LocalHandle::ZERO,
                0,
                node,
                edge,
                None,
                None,
                None,
                node,
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(sequence),
                ReleaseSequence::new(release),
                CommitSequence::new(commit_before),
                CommitSequence::new(commit_after),
                WorkflowTraceValueFragment::ABSENT,
            )?);
            Ok(())
        };
        push(WorkflowTraceEventKind::NodeExecuted, 0, 0, 0, Some(0), None)?;
        if include_first_transition {
            push(
                WorkflowTraceEventKind::TransitionTaken,
                0,
                0,
                0,
                Some(0),
                Some(0),
            )?;
        }
        push(WorkflowTraceEventKind::ScanCommitted, 0, 0, 1, None, None)?;
        push(WorkflowTraceEventKind::NodeExecuted, 1, 1, 1, Some(1), None)?;
        push(
            WorkflowTraceEventKind::TransitionTaken,
            1,
            1,
            1,
            Some(1),
            Some(1),
        )?;
        push(
            WorkflowTraceEventKind::CompletionRequested,
            1,
            1,
            1,
            Some(1),
            None,
        )?;
        push(
            WorkflowTraceEventKind::WorkflowCompleted,
            1,
            1,
            1,
            None,
            None,
        )?;
        push(WorkflowTraceEventKind::ScanCommitted, 1, 1, 2, None, None)?;
        let mut bytes = Vec::from(
            WorkflowTraceFileHeader::new(epoch, plan_digest, u64::try_from(records.len())?, 0)
                .encode(),
        );
        for record in records {
            bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(record).as_bytes());
        }
        Ok(bytes)
    }

    fn deadline_discard_file(plan_digest: [u8; 32]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let initialized = initialized_record(epoch, LocalHandle::ZERO, 0)?;
        let deadline = WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::DeadlineObserved,
            MissOutcome::FinishAfterDeadline as u16,
            LocalHandle::ZERO,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(1),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::ZERO,
            WorkflowTraceValueFragment::ABSENT,
        )?;
        let terminal = WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::ScanDiscarded,
            0,
            LocalHandle::ZERO,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(2),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::ZERO,
            WorkflowTraceValueFragment::ABSENT,
        )?;
        let mut bytes = Vec::from(WorkflowTraceFileHeader::new(epoch, plan_digest, 3, 0).encode());
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(initialized).as_bytes());
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(deadline).as_bytes());
        bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(terminal).as_bytes());
        Ok(bytes)
    }

    fn task_one_value_file(plan_digest: [u8; 32]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let task = LocalHandle::new(1)?;
        let records = [
            initialized_record(epoch, task, 0)?,
            WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::DeadlineObserved,
                1,
                task,
                0,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(1),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::ZERO,
                WorkflowTraceValueFragment::ABSENT,
            )?,
            WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::NodeExecuted,
                0,
                task,
                0,
                Some(0),
                None,
                None,
                None,
                None,
                Some(0),
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(2),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::ZERO,
                WorkflowTraceValueFragment::ABSENT,
            )?,
            WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::OutputStaged,
                2,
                task,
                0,
                Some(0),
                None,
                Some(0),
                Some(0),
                None,
                Some(0),
                Some(4),
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(3),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::ZERO,
                WorkflowTraceValueFragment::ABSENT,
            )?,
            WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::ScanCommitted,
                0,
                task,
                0,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(4),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::new(1),
                WorkflowTraceValueFragment::ABSENT,
            )?,
        ];
        let mut bytes = Vec::from(WorkflowTraceFileHeader::new(epoch, plan_digest, 5, 0).encode());
        for record in records {
            bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(record).as_bytes());
        }
        Ok(bytes)
    }

    fn initialized_record(
        epoch: BootEpochId,
        task: LocalHandle,
        sequence: u64,
    ) -> Result<WorkflowTraceRecord, Box<dyn std::error::Error>> {
        Ok(WorkflowTraceRecord::new(
            WorkflowTraceVersion::V1_0,
            WorkflowTraceEventKind::WorkflowInitialized,
            0,
            task,
            0,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            epoch,
            TaskEpoch::new(1)?,
            EventSequence::new(sequence),
            ReleaseSequence::ZERO,
            CommitSequence::ZERO,
            CommitSequence::ZERO,
            WorkflowTraceValueFragment::ABSENT,
        )?)
    }

    fn remove_record(bytes: &[u8], index: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut result = bytes.to_vec();
        let count = usize::try_from(u64::from_le_bytes(result[72..80].try_into()?))?;
        if index >= count {
            return Err("record index outside fixture".into());
        }
        let start = super::WORKFLOW_TRACE_FILE_HEADER_SIZE
            .checked_add(index * super::WORKFLOW_TRACE_RECORD_SIZE)
            .ok_or("record offset overflow")?;
        result.drain(start..start + super::WORKFLOW_TRACE_RECORD_SIZE);
        let new_count = count - 1;
        result[72..80].copy_from_slice(&u64::try_from(new_count)?.to_le_bytes());
        for record_index in index..new_count {
            let offset = super::WORKFLOW_TRACE_FILE_HEADER_SIZE
                + record_index * super::WORKFLOW_TRACE_RECORD_SIZE
                + 88;
            result[offset..offset + 8].copy_from_slice(&u64::try_from(record_index)?.to_le_bytes());
        }
        Ok(result)
    }

    fn cross_instance_edge_file(
        plan_digest: [u8; 32],
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let epoch = epoch()?;
        let records = [
            initialized_record(epoch, LocalHandle::ZERO, 0)?,
            WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::NodeExecuted,
                0,
                LocalHandle::ZERO,
                0,
                Some(0),
                None,
                None,
                None,
                None,
                Some(0),
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(1),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::ZERO,
                WorkflowTraceValueFragment::ABSENT,
            )?,
            WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::TransitionTaken,
                0,
                LocalHandle::ZERO,
                0,
                Some(0),
                Some(0),
                None,
                None,
                None,
                Some(0),
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(2),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::ZERO,
                WorkflowTraceValueFragment::ABSENT,
            )?,
            WorkflowTraceRecord::new(
                WorkflowTraceVersion::V1_0,
                WorkflowTraceEventKind::ScanCommitted,
                0,
                LocalHandle::ZERO,
                0,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                epoch,
                TaskEpoch::new(1)?,
                EventSequence::new(3),
                ReleaseSequence::ZERO,
                CommitSequence::ZERO,
                CommitSequence::new(1),
                WorkflowTraceValueFragment::ABSENT,
            )?,
        ];
        let mut bytes = Vec::from(
            WorkflowTraceFileHeader::new(epoch, plan_digest, u64::try_from(records.len())?, 0)
                .encode(),
        );
        for record in records {
            bytes.extend_from_slice(WorkflowTraceRecordBytes::encode(record).as_bytes());
        }
        Ok(bytes)
    }

    fn epoch() -> Result<BootEpochId, aurora_types::IdentifierError> {
        BootEpochId::from_bytes([
            0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07,
            0x39, 0x8f,
        ])
    }
}
