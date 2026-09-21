//! Build-time fixed handles and exact mapping-closure validation.

use aurora_io_guardian_contracts::LayoutDigest;
use aurora_types::{LocalHandle, TagId};

use crate::{CapabilityDigest, ImageDirection, ImageError, ImageLayout};

/// Dense source identity used instead of backend-native pointers or address strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceHandle(u32);

impl SourceHandle {
    /// Creates a source handle.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the dense integer value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Direction-local dense update-group identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GroupHandle(u16);

impl GroupHandle {
    /// Creates a group handle.
    #[must_use]
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the direction-local integer value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Frozen protocol source categories admitted by R3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ProtocolSourceKind {
    /// `EtherCAT` process data.
    Ethercat = 0,
    /// Modbus TCP client data.
    ModbusTcp = 1,
    /// Modbus RTU master data.
    ModbusRtu = 2,
    /// Bounded non-Modbus serial frame data.
    Serial = 3,
    /// `SocketCAN` CAN 2.0/CAN FD frame data.
    Can = 4,
    /// LIN controller schedule data.
    Lin = 5,
}

/// One build-approved driver/protocol/device source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceDescriptor {
    handle: SourceHandle,
    protocol: ProtocolSourceKind,
    driver_identity: [u8; 32],
    device_identity: [u8; 32],
}

impl SourceDescriptor {
    /// Creates a source descriptor from fixed digests, never an address string.
    #[must_use]
    pub const fn new(
        handle: SourceHandle,
        protocol: ProtocolSourceKind,
        driver_identity: [u8; 32],
        device_identity: [u8; 32],
    ) -> Self {
        Self {
            handle,
            protocol,
            driver_identity,
            device_identity,
        }
    }

    /// Returns the dense source handle.
    #[must_use]
    pub const fn handle(self) -> SourceHandle {
        self.handle
    }

    /// Returns the fixed protocol category.
    #[must_use]
    pub const fn protocol(self) -> ProtocolSourceKind {
        self.protocol
    }

    /// Returns the approved driver identity digest.
    #[must_use]
    pub const fn driver_identity(self) -> [u8; 32] {
        self.driver_identity
    }

    /// Returns the exact device identity digest.
    #[must_use]
    pub const fn device_identity(self) -> [u8; 32] {
        self.device_identity
    }
}

/// One direction-local update group bound to a source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GroupDescriptor {
    direction: ImageDirection,
    handle: GroupHandle,
    source: SourceHandle,
}

impl GroupDescriptor {
    /// Creates a group descriptor.
    #[must_use]
    pub const fn new(direction: ImageDirection, handle: GroupHandle, source: SourceHandle) -> Self {
        Self {
            direction,
            handle,
            source,
        }
    }

    /// Returns the group direction.
    #[must_use]
    pub const fn direction(self) -> ImageDirection {
        self.direction
    }

    /// Returns the direction-local group handle.
    #[must_use]
    pub const fn handle(self) -> GroupHandle {
        self.handle
    }

    /// Returns the owning source handle.
    #[must_use]
    pub const fn source(self) -> SourceHandle {
        self.source
    }
}

/// Scalar storage type in one direction payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ScalarType {
    /// One packed bit.
    Bool = 0,
    /// Unsigned 8-bit integer.
    U8 = 1,
    /// Signed 8-bit integer.
    I8 = 2,
    /// Unsigned 16-bit integer.
    U16 = 3,
    /// Signed 16-bit integer.
    I16 = 4,
    /// Unsigned 32-bit integer.
    U32 = 5,
    /// Signed 32-bit integer.
    I32 = 6,
    /// IEEE 754 binary32.
    F32 = 7,
    /// Unsigned 64-bit integer.
    U64 = 8,
    /// Signed 64-bit integer.
    I64 = 9,
    /// IEEE 754 binary64.
    F64 = 10,
}

impl ScalarType {
    const fn width_bits(self) -> u64 {
        match self {
            Self::Bool => 1,
            Self::U8 | Self::I8 => 8,
            Self::U16 | Self::I16 => 16,
            Self::U32 | Self::I32 | Self::F32 => 32,
            Self::U64 | Self::I64 | Self::F64 => 64,
        }
    }
}

/// Byte order for multi-byte scalar payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ByteOrder {
    /// Least-significant byte first.
    LittleEndian = 0,
    /// Most-significant byte first.
    BigEndian = 1,
}

/// Bit numbering within one byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum BitOrder {
    /// Bit zero is the least-significant bit.
    Lsb0 = 0,
    /// Bit zero is the most-significant bit.
    Msb0 = 1,
}

/// Required output protection class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ProtectionLevel {
    /// Guardian process must remain alive to hold the output.
    GuardianProtected = 0,
    /// Device-native watchdog supplies the second protection layer.
    DeviceWatchdogProtected = 1,
    /// An independent external safety/protection system owns the hazardous boundary.
    ExternalSafetyProtected = 2,
}

/// One exact local-handle mapping without a physical address string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ValueBinding {
    handle: LocalHandle,
    tag_id: TagId,
    direction: ImageDirection,
    scalar_type: ScalarType,
    byte_offset: u32,
    bit_offset: u8,
    byte_order: ByteOrder,
    bit_order: BitOrder,
    source: SourceHandle,
    group: GroupHandle,
    protection: Option<ProtectionLevel>,
}

impl ValueBinding {
    /// Creates a binding whose full closure is validated by [`ImageMapping::new`].
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub const fn new(
        handle: LocalHandle,
        tag_id: TagId,
        direction: ImageDirection,
        scalar_type: ScalarType,
        byte_offset: u32,
        bit_offset: u8,
        byte_order: ByteOrder,
        bit_order: BitOrder,
        source: SourceHandle,
        group: GroupHandle,
        protection: Option<ProtectionLevel>,
    ) -> Self {
        Self {
            handle,
            tag_id,
            direction,
            scalar_type,
            byte_offset,
            bit_offset,
            byte_order,
            bit_order,
            source,
            group,
            protection,
        }
    }

    /// Returns the global dense local handle.
    #[must_use]
    pub const fn handle(self) -> LocalHandle {
        self.handle
    }

    /// Returns the stable tag identity.
    #[must_use]
    pub const fn tag_id(self) -> TagId {
        self.tag_id
    }

    /// Returns the image direction.
    #[must_use]
    pub const fn direction(self) -> ImageDirection {
        self.direction
    }

    /// Returns the scalar storage type.
    #[must_use]
    pub const fn scalar_type(self) -> ScalarType {
        self.scalar_type
    }

    /// Returns the payload byte offset.
    #[must_use]
    pub const fn byte_offset(self) -> u32 {
        self.byte_offset
    }

    /// Returns the intra-byte bit offset.
    #[must_use]
    pub const fn bit_offset(self) -> u8 {
        self.bit_offset
    }

    /// Returns the scalar byte order.
    #[must_use]
    pub const fn byte_order(self) -> ByteOrder {
        self.byte_order
    }

    /// Returns the bit order.
    #[must_use]
    pub const fn bit_order(self) -> BitOrder {
        self.bit_order
    }

    /// Returns the source handle.
    #[must_use]
    pub const fn source(self) -> SourceHandle {
        self.source
    }

    /// Returns the direction-local group handle.
    #[must_use]
    pub const fn group(self) -> GroupHandle {
        self.group
    }

    /// Returns the mandatory output protection or `None` for input.
    #[must_use]
    pub const fn protection(self) -> Option<ProtectionLevel> {
        self.protection
    }

    fn bit_range(self) -> Result<(u64, u64), ImageError> {
        let start = u64::from(self.byte_offset)
            .checked_mul(8)
            .and_then(|value| value.checked_add(u64::from(self.bit_offset)))
            .ok_or(ImageError::ArithmeticOverflow)?;
        let end = start
            .checked_add(self.scalar_type.width_bits())
            .ok_or(ImageError::ArithmeticOverflow)?;
        Ok((start, end))
    }
}

/// Borrowed, fully validated exact mapping closure.
#[derive(Debug, Clone, Copy)]
pub struct ImageMapping<'a> {
    layout: ImageLayout,
    layout_digest: LayoutDigest,
    capability_digest: CapabilityDigest,
    sources: &'a [SourceDescriptor],
    groups: &'a [GroupDescriptor],
    values: &'a [ValueBinding],
}

impl<'a> ImageMapping<'a> {
    /// Validates sources, groups, values, ranges, direction order, and exact reference closure.
    ///
    /// Canonical generation order is dense sources, then dense input groups followed by output
    /// groups, then dense input values followed by output values. This makes missing, duplicate,
    /// reordered, and extra generated entries distinguishable.
    ///
    /// # Errors
    ///
    /// Returns a typed mapping failure without publishing a partial view.
    pub fn new(
        layout: ImageLayout,
        layout_digest: LayoutDigest,
        capability_digest: CapabilityDigest,
        sources: &'a [SourceDescriptor],
        groups: &'a [GroupDescriptor],
        values: &'a [ValueBinding],
    ) -> Result<Self, ImageError> {
        validate_sources(sources)?;
        validate_groups(layout, sources, groups)?;
        validate_values(layout, sources, groups, values)?;
        validate_closure(sources, groups, values)?;
        Ok(Self {
            layout,
            layout_digest,
            capability_digest,
            sources,
            groups,
            values,
        })
    }

    /// Returns the checked region layout.
    #[must_use]
    pub const fn layout(self) -> ImageLayout {
        self.layout
    }

    /// Returns the signed digest that fixes all mapping/catalog semantics for this layout.
    #[must_use]
    pub const fn layout_digest(self) -> LayoutDigest {
        self.layout_digest
    }

    /// Returns the exact capability-catalog digest admitted for this mapping.
    #[must_use]
    pub const fn capability_digest(self) -> CapabilityDigest {
        self.capability_digest
    }

    /// Returns the exact ordered source catalog.
    #[must_use]
    pub const fn sources(self) -> &'a [SourceDescriptor] {
        self.sources
    }

    /// Returns the exact ordered group catalog.
    #[must_use]
    pub const fn groups(self) -> &'a [GroupDescriptor] {
        self.groups
    }

    /// Returns the exact ordered value catalog.
    #[must_use]
    pub const fn values(self) -> &'a [ValueBinding] {
        self.values
    }
}

fn validate_sources(sources: &[SourceDescriptor]) -> Result<(), ImageError> {
    for (index, source) in sources.iter().enumerate() {
        let expected = u32::try_from(index).map_err(|_| ImageError::InvalidCapacity)?;
        if source.handle.get() != expected {
            return Err(ImageError::NonDenseHandle);
        }
    }
    Ok(())
}

fn validate_groups(
    layout: ImageLayout,
    sources: &[SourceDescriptor],
    groups: &[GroupDescriptor],
) -> Result<(), ImageError> {
    let expected_len = usize::from(layout.input().group_count())
        .checked_add(usize::from(layout.output().group_count()))
        .ok_or(ImageError::ArithmeticOverflow)?;
    if groups.len() != expected_len {
        return Err(ImageError::MappingClosureMismatch);
    }
    for (index, group) in groups.iter().enumerate() {
        let input_count = usize::from(layout.input().group_count());
        let (direction, local_index) = if index < input_count {
            (ImageDirection::Input, index)
        } else {
            (ImageDirection::Output, index - input_count)
        };
        let expected = u16::try_from(local_index).map_err(|_| ImageError::InvalidCapacity)?;
        if group.direction != direction || group.handle.get() != expected {
            return Err(ImageError::NonDenseHandle);
        }
        if usize::try_from(group.source.get()).map_or(true, |value| value >= sources.len()) {
            return Err(ImageError::UnknownSource);
        }
    }
    Ok(())
}

fn validate_values(
    layout: ImageLayout,
    sources: &[SourceDescriptor],
    groups: &[GroupDescriptor],
    values: &[ValueBinding],
) -> Result<(), ImageError> {
    if usize::try_from(layout.value_count()) != Ok(values.len()) {
        return Err(ImageError::MappingClosureMismatch);
    }
    let input_value_count =
        usize::try_from(layout.input().value_count()).map_err(|_| ImageError::InvalidCapacity)?;
    for (index, value) in values.iter().enumerate() {
        let expected = u32::try_from(index).map_err(|_| ImageError::InvalidCapacity)?;
        if value.handle.get() != expected {
            return Err(ImageError::NonDenseHandle);
        }
        let expected_direction = if index < input_value_count {
            ImageDirection::Input
        } else {
            ImageDirection::Output
        };
        if value.direction != expected_direction {
            return Err(ImageError::NonDenseHandle);
        }
        if values[..index]
            .iter()
            .any(|prior| prior.tag_id == value.tag_id)
        {
            return Err(ImageError::DuplicateTag);
        }
        if usize::try_from(value.source.get()).map_or(true, |source| source >= sources.len()) {
            return Err(ImageError::UnknownSource);
        }
        let group_index = group_index(layout, value.direction, value.group)?;
        let Some(group) = groups.get(group_index) else {
            return Err(ImageError::UnknownGroup);
        };
        if group.direction != value.direction
            || group.handle != value.group
            || group.source != value.source
        {
            return Err(ImageError::UnknownGroup);
        }
        validate_scalar(
            *value,
            layout.slot(value.direction).payload_capacity_bytes(),
        )?;
        for prior in &values[..index] {
            if prior.direction == value.direction && ranges_overlap(*prior, *value)? {
                return Err(ImageError::ValueOverlap);
            }
        }
        match (value.direction, value.protection) {
            (ImageDirection::Input, None) | (ImageDirection::Output, Some(_)) => {}
            (ImageDirection::Input, Some(_)) | (ImageDirection::Output, None) => {
                return Err(ImageError::InvalidScalarMapping);
            }
        }
    }
    Ok(())
}

fn validate_scalar(value: ValueBinding, payload_capacity_bytes: u32) -> Result<(), ImageError> {
    match value.scalar_type {
        ScalarType::Bool => {
            if value.bit_offset > 7 || value.byte_order != ByteOrder::LittleEndian {
                return Err(ImageError::InvalidScalarMapping);
            }
        }
        _ => {
            if value.bit_offset != 0 || value.bit_order != BitOrder::Lsb0 {
                return Err(ImageError::InvalidScalarMapping);
            }
        }
    }
    let (_, end) = value.bit_range()?;
    let payload_bits = u64::from(payload_capacity_bytes)
        .checked_mul(8)
        .ok_or(ImageError::ArithmeticOverflow)?;
    if end > payload_bits {
        return Err(ImageError::ValueOutOfBounds);
    }
    Ok(())
}

fn ranges_overlap(left: ValueBinding, right: ValueBinding) -> Result<bool, ImageError> {
    let (left_start, left_end) = left.bit_range()?;
    let (right_start, right_end) = right.bit_range()?;
    Ok(left_start < right_end && right_start < left_end)
}

fn group_index(
    layout: ImageLayout,
    direction: ImageDirection,
    handle: GroupHandle,
) -> Result<usize, ImageError> {
    let local = usize::from(handle.get());
    match direction {
        ImageDirection::Input => {
            if local < usize::from(layout.input().group_count()) {
                Ok(local)
            } else {
                Err(ImageError::UnknownGroup)
            }
        }
        ImageDirection::Output => {
            if local >= usize::from(layout.output().group_count()) {
                return Err(ImageError::UnknownGroup);
            }
            usize::from(layout.input().group_count())
                .checked_add(local)
                .ok_or(ImageError::ArithmeticOverflow)
        }
    }
}

fn validate_closure(
    sources: &[SourceDescriptor],
    groups: &[GroupDescriptor],
    values: &[ValueBinding],
) -> Result<(), ImageError> {
    for source in sources {
        if !groups.iter().any(|group| group.source == source.handle) {
            return Err(ImageError::MappingClosureMismatch);
        }
    }
    for group in groups {
        if !values.iter().any(|value| {
            value.direction == group.direction
                && value.group == group.handle
                && value.source == group.source
        }) {
            return Err(ImageError::MappingClosureMismatch);
        }
    }
    Ok(())
}
