//! Versioned, platform-neutral contracts between Aurora Control and I/O Guardian.
//!
//! This crate contains no device, socket, shared-memory, scheduler, or driver implementation.
//! It validates the R3-01 negotiation and lease state before R3-02/R3-05 platform adapters act.

mod capability;
mod error;
mod identity;
mod lease;
mod transport;
mod version;

pub use capability::{CapabilitySet, IoCapability};
pub use error::{GuardianContractError, GuardianErrorCode};
pub use identity::{
    ConfigurationDigest, ConfigurationGeneration, GuardianConfiguration, GuardianEpoch,
    ImageSequence, LayoutDigest, LeaseId, LeaseIdentity, LeaseSequence,
};
pub use lease::{
    DeadlineObservation, GuardianLeaseMachine, GuardianState, LeasePolicy, LeaseRequest,
    OutputImageIdentity,
};
pub use transport::{MemfdSealSet, PeerCredentials, PeerPolicy, SharedRegionOffer};
pub use version::{ContractOffer, NegotiatedContract, negotiate};
