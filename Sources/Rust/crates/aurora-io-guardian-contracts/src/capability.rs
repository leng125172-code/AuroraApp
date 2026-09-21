//! Fixed Guardian capability catalog and allocation-free sets.

use crate::GuardianContractError;

/// One known Preview 1.0 Guardian capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum IoCapability {
    /// Guardian lease and state contract.
    Guardian = 0,
    /// Shared I/O image layout.
    Image = 1,
    /// Driver Adapter contract.
    DriverSdk = 2,
    /// `EtherCAT MainDevice` role.
    EthercatMainDevice = 3,
    /// Modbus TCP Client role.
    ModbusTcpClient = 4,
    /// Modbus RTU Master role.
    ModbusRtuMaster = 5,
    /// Bounded serial transport.
    Serial = 6,
    /// Linux `SocketCAN` transport.
    SocketCan = 7,
    /// LIN controller and fixed schedule.
    LinController = 8,
}

impl IoCapability {
    /// Exact catalog order frozen by SPEC-R3-001.
    pub const ALL: [Self; 9] = [
        Self::Guardian,
        Self::Image,
        Self::DriverSdk,
        Self::EthercatMainDevice,
        Self::ModbusTcpClient,
        Self::ModbusRtuMaster,
        Self::Serial,
        Self::SocketCan,
        Self::LinController,
    ];

    /// Returns the stable capability identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Guardian => "aurora.io.guardian@1",
            Self::Image => "aurora.io.image@1",
            Self::DriverSdk => "aurora.io.driver-sdk@1",
            Self::EthercatMainDevice => "aurora.io.ethercat-main-device@1",
            Self::ModbusTcpClient => "aurora.io.modbus-tcp-client@1",
            Self::ModbusRtuMaster => "aurora.io.modbus-rtu-master@1",
            Self::Serial => "aurora.io.serial@1",
            Self::SocketCan => "aurora.io.socketcan@1",
            Self::LinController => "aurora.io.lin-controller@1",
        }
    }

    const fn bit(self) -> u16 {
        1_u16 << (self as u8)
    }
}

/// Fixed bitset whose only representable entries are the known I/O capabilities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapabilitySet(u16);

impl CapabilitySet {
    /// Empty capability set.
    pub const EMPTY: Self = Self(0);
    /// Base capabilities required by every Guardian/Control session.
    pub const SESSION_BASE: Self = Self(IoCapability::Guardian.bit() | IoCapability::Image.bit());
    const KNOWN_MASK: u16 = (1_u16 << IoCapability::ALL.len()) - 1;

    /// Creates a set from canonical raw bits.
    ///
    /// # Errors
    ///
    /// Returns [`GuardianContractError::UnknownCapability`] when an unknown bit is set.
    pub const fn from_raw_bits(bits: u16) -> Result<Self, GuardianContractError> {
        if bits & !Self::KNOWN_MASK == 0 {
            Ok(Self(bits))
        } else {
            Err(GuardianContractError::UnknownCapability)
        }
    }

    /// Creates a set from entries in exact catalog order.
    ///
    /// Missing entries are allowed because actual targets advertise a subset. Duplicate or
    /// reordered entries are rejected before a set is returned.
    ///
    /// # Errors
    ///
    /// Returns [`GuardianContractError::DuplicateCapability`] for repetition or
    /// [`GuardianContractError::CapabilityOutOfOrder`] for non-canonical order.
    pub fn from_ordered(entries: &[IoCapability]) -> Result<Self, GuardianContractError> {
        let mut bits = 0_u16;
        let mut previous = None;
        for entry in entries {
            let ordinal = *entry as u8;
            if previous == Some(ordinal) {
                return Err(GuardianContractError::DuplicateCapability);
            }
            if previous.is_some_and(|value| value > ordinal) {
                return Err(GuardianContractError::CapabilityOutOfOrder);
            }
            bits |= entry.bit();
            previous = Some(ordinal);
        }
        Ok(Self(bits))
    }

    /// Returns the canonical raw bit representation.
    #[must_use]
    pub const fn raw_bits(self) -> u16 {
        self.0
    }

    /// Returns whether one capability is present.
    #[must_use]
    pub const fn contains(self, capability: IoCapability) -> bool {
        self.0 & capability.bit() != 0
    }

    /// Returns whether every entry in `required` is present.
    #[must_use]
    pub const fn contains_all(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    /// Returns the intersection of two fixed sets.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Returns the union of two fixed sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns whether the set has no entries.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::{CapabilitySet, IoCapability};
    use crate::GuardianContractError;

    #[test]
    fn exact_catalog_has_one_bit_per_entry_and_rejects_unknown_bits() {
        let catalog = CapabilitySet::from_ordered(&IoCapability::ALL);
        assert_eq!(catalog.map(CapabilitySet::raw_bits), Ok(0x01ff));
        assert_eq!(
            CapabilitySet::from_raw_bits(0x0200),
            Err(GuardianContractError::UnknownCapability)
        );
    }

    #[test]
    fn constructor_rejects_duplicate_and_reordered_entries_without_partial_set() {
        assert_eq!(
            CapabilitySet::from_ordered(&[IoCapability::Guardian, IoCapability::Guardian]),
            Err(GuardianContractError::DuplicateCapability)
        );
        assert_eq!(
            CapabilitySet::from_ordered(&[IoCapability::Image, IoCapability::Guardian]),
            Err(GuardianContractError::CapabilityOutOfOrder)
        );
        let subset = CapabilitySet::from_ordered(&[
            IoCapability::Guardian,
            IoCapability::Image,
            IoCapability::SocketCan,
        ]);
        assert!(subset.is_ok_and(|value| value.contains(IoCapability::SocketCan)));
    }
}
