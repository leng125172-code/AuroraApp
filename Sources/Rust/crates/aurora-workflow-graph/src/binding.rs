//! R2-05 host-side typed Action and condition binding contracts.
//!
//! These values are supplied separately from the Preview 1.0 Graph. All target handles are
//! resolved by earlier build stages; this crate never performs runtime discovery. The planning
//! pass validates the complete expanded binding closure and independently recomputes every write
//! footprint before publishing a Static Workflow Plan.

use serde::Serialize;

use crate::{StableId, WorkflowWriteRegion};

/// Supported typed binding contract major version.
pub const WORKFLOW_BINDING_MAJOR: u16 = 1;
/// Supported typed binding contract minor version.
pub const WORKFLOW_BINDING_MINOR: u16 = 0;

/// Exact reader/writer version of one Action binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WorkflowBindingVersion {
    /// Reader-incompatible version.
    pub major: u16,
    /// Backward-compatible version.
    pub minor: u16,
}

impl WorkflowBindingVersion {
    /// Exact Preview 1.0 binding contract.
    pub const V1_0: Self = Self {
        major: WORKFLOW_BINDING_MAJOR,
        minor: WORKFLOW_BINDING_MINOR,
    };
}

/// Fixed scalar value type accepted by the first typed binding contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowValueType {
    /// IEC BOOL encoded in one byte.
    Bool,
    /// Signed 8-bit integer.
    Sint,
    /// Signed 16-bit integer.
    Int,
    /// Signed 32-bit integer.
    Dint,
    /// Signed 64-bit integer.
    Lint,
    /// Unsigned 8-bit integer.
    Usint,
    /// Unsigned 16-bit integer.
    Uint,
    /// Unsigned 32-bit integer.
    Udint,
    /// Unsigned 64-bit integer.
    Ulint,
    /// IEEE-754 binary32.
    Real,
    /// IEEE-754 binary64.
    Lreal,
}

impl WorkflowValueType {
    /// Canonical fixed byte width.
    #[must_use]
    pub const fn size_bytes(self) -> u64 {
        match self {
            Self::Bool | Self::Sint | Self::Usint => 1,
            Self::Int | Self::Uint => 2,
            Self::Dint | Self::Udint | Self::Real => 4,
            Self::Lint | Self::Ulint | Self::Lreal => 8,
        }
    }
}

/// Task staging area addressed by one typed binding port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowValueArea {
    /// Workflow/ST application state staging bank.
    State,
    /// Task output staging bank; this is not physical I/O.
    Output,
}

/// One fixed typed storage slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WorkflowValueSlot {
    /// Stable ownership identity used by the global single-writer proof.
    pub target_id: StableId,
    /// Staging area containing the slot.
    pub area: WorkflowValueArea,
    /// Byte offset within the target.
    #[serde(serialize_with = "crate::planning::serialize_u64_decimal")]
    pub offset_bytes: u64,
    /// Resolved absolute byte offset within the owning task `area` image.
    ///
    /// `offset_bytes` remains target-relative and is used by the logical single-writer proof;
    /// this value is the independently resolved runtime address.
    #[serde(serialize_with = "crate::planning::serialize_u64_decimal")]
    pub image_offset_bytes: u64,
    /// Exact scalar type and width.
    pub value_type: WorkflowValueType,
}

impl WorkflowValueSlot {
    pub(crate) fn write_region(self) -> WorkflowWriteRegion {
        WorkflowWriteRegion {
            target_id: self.target_id,
            offset_bytes: self.offset_bytes,
            size_bytes: self.value_type.size_bytes(),
        }
    }

    pub(crate) const fn end_is_representable(self) -> bool {
        self.offset_bytes
            .checked_add(self.value_type.size_bytes())
            .is_some()
            && self
                .image_offset_bytes
                .checked_add(self.value_type.size_bytes())
                .is_some()
    }
}

/// Resolved application-state and output-image capacities for one bound R0 task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskBindingImageInput {
    /// Existing task handle; `u32::MAX` is reserved.
    pub task_handle: u32,
    /// Application state bytes addressable by Action ports and conditions.
    pub application_state_bytes: u64,
    /// Task output staging bytes addressable by Action ports and conditions.
    pub output_bytes: u64,
}

/// Trace value source supplied for one planned watch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkflowWatchBindingInput {
    /// Owning R0 task.
    pub task_handle: u32,
    /// Root `WorkflowId` followed by Subworkflow call-site `NodeId` values.
    pub instance_path: Vec<StableId>,
    /// Must exactly match one [`crate::WorkflowWatchInput`] identity.
    pub value_id: StableId,
    /// Stable type catalog handle; `u32::MAX` is reserved.
    pub type_handle: u32,
    /// Resolved task image area.
    pub area: WorkflowValueArea,
    /// Absolute byte offset within `area`.
    pub image_offset_bytes: u64,
    /// Exact canonical encoded width; must match the planned watch.
    pub encoded_bytes: u64,
}

/// Dense value handle used by R2-06 Workflow Trace records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WorkflowTraceValueHandle(pub u32);

/// Exact producer of one trace value descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowTraceValueSource {
    /// One writable Action port.
    Output {
        /// Owning expanded Action step.
        step: crate::WorkflowStepHandle,
        /// Stable zero-based port index.
        port: u32,
    },
    /// One planned watch.
    Watch {
        /// Dense planned watch handle.
        watch: crate::WorkflowWatchHandle,
    },
}

/// Canonical Static Plan 1.2 value descriptor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PlannedTraceValue {
    /// Globally dense value handle.
    pub handle: WorkflowTraceValueHandle,
    /// Owning task.
    pub task_handle: u32,
    /// Expanded instance used for call-site isolation.
    pub instance: crate::WorkflowInstanceHandle,
    /// Stable logical value identity.
    pub value_id: StableId,
    /// Exact output or watch producer.
    pub source: WorkflowTraceValueSource,
    /// Stable type catalog handle. Action Outputs freeze `BOOL..LREAL` to `1..=11` in enum order;
    /// Watch handles come from the host type catalog and must not use `u32::MAX`.
    pub type_handle: u32,
    /// Resolved image area.
    pub area: WorkflowValueArea,
    /// Absolute resolved image byte offset.
    #[serde(serialize_with = "crate::planning::serialize_u64_decimal")]
    pub image_offset_bytes: u64,
    /// Canonical encoded byte width.
    #[serde(serialize_with = "crate::planning::serialize_u64_decimal")]
    pub encoded_bytes: u64,
    /// Exact count of 32-byte Trace fragments.
    pub fragment_count: u16,
}

/// Direction of one statically ordered Action port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowPortDirection {
    /// Read-only input.
    Input,
    /// Write-only output.
    Output,
    /// Read/write value.
    InOut,
}

/// One typed port mapped to a fixed task staging slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WorkflowActionPortBinding {
    /// Contract declaration order is semantically significant.
    pub port: u32,
    /// Static access direction.
    pub direction: WorkflowPortDirection,
    /// Exact storage slot.
    pub slot: WorkflowValueSlot,
}

/// Supported cycle-safe Action boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowActionKind {
    /// A statically linked R1 ST POU wrapper.
    StPou,
    /// A prevalidated operation over latched/staging I/O images only.
    IoImage,
    /// A fixed typed command staged into the task output image.
    TypedCommand,
}

/// Fully resolved binding for one expanded Action node.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ExpandedActionBindingInput {
    /// Exact contract version.
    pub version: WorkflowBindingVersion,
    /// Stable binding identity; unique in the compiled closure.
    pub binding_id: StableId,
    /// Supported cycle-safe target class.
    pub kind: WorkflowActionKind,
    /// Dense build-resolved target handle; `u32::MAX` is reserved.
    pub target_handle: u32,
    /// Absolute start of this expanded invocation's exclusive mutable state in the task state
    /// image. The range length is `committed_state_bytes`.
    #[serde(serialize_with = "crate::planning::serialize_u64_decimal")]
    pub invocation_state_offset_bytes: u64,
    /// Ordered fixed typed ports.
    pub ports: Vec<WorkflowActionPortBinding>,
    /// Additional committed bytes owned by this invocation/activation.
    #[serde(serialize_with = "crate::planning::serialize_u64_decimal")]
    pub committed_state_bytes: u64,
    /// Additional staging bytes owned by this invocation/activation.
    #[serde(serialize_with = "crate::planning::serialize_u64_decimal")]
    pub staging_state_bytes: u64,
    /// Additional binding events reserved for R2-06 Trace.
    #[serde(serialize_with = "crate::planning::serialize_u64_decimal")]
    pub trace_events_per_release: u64,
}

impl ExpandedActionBindingInput {
    pub(crate) fn derived_writes(&self) -> Vec<WorkflowWriteRegion> {
        let mut writes = self
            .ports
            .iter()
            .filter(|port| {
                matches!(
                    port.direction,
                    WorkflowPortDirection::Output | WorkflowPortDirection::InOut
                )
            })
            .map(|port| port.slot.write_region())
            .collect::<Vec<_>>();
        writes.sort();
        writes
    }
}

/// One expanded-instance BOOL condition source. A condition can be referenced by multiple sites.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct WorkflowConditionBindingInput {
    /// Owning R0 task.
    pub task_handle: u32,
    /// Root `WorkflowId` followed by each Subworkflow call-site `NodeId`.
    pub instance_path: Vec<StableId>,
    /// Stable condition identity from the Graph.
    pub condition_id: StableId,
    /// Fixed BOOL slot evaluated without a callback or dynamic lookup.
    pub source: WorkflowValueSlot,
}

/// Dense condition handle published in Static Workflow Plan 1.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WorkflowConditionHandle(pub u32);

/// Canonical condition binding retained in the plan and digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PlannedConditionBinding {
    /// Dense plan-local handle.
    pub handle: WorkflowConditionHandle,
    /// Owning task.
    pub task_handle: u32,
    /// Expanded instance owning the condition source.
    pub instance: crate::planning::WorkflowInstanceHandle,
    /// Stable condition identity.
    pub condition_id: StableId,
    /// Exact BOOL source.
    pub source: WorkflowValueSlot,
}
