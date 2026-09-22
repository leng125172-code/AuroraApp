//! Protocol-neutral, fixed-capacity Driver Adapter SDK and isolated-host admission boundary.
//!
//! R3-05 defines normalized lifecycle/exchange semantics shared by statically linked and isolated
//! drivers. It contains no concrete fieldbus stack, runtime plugin discovery, device access,
//! process launcher, C FFI, or backend-native handle.

mod adapter;
mod error;
mod host;
mod model;
mod simulator;

pub use adapter::{
    AdapterOperation, AdapterReport, DriverAdapter, DriverLifecycleState, ExchangeBuffer,
    ExchangeReport, ExchangeRequest, LifecycleRequest, MailboxCancellation, MailboxRequest,
};
pub use error::{DriverFaultKind, DriverSdkError};
pub use host::{
    AppliedSandbox, DeviceAccessGrant, DriverHostBoundary, DriverHostEvent, DriverHostGeneration,
    DriverHostRegistry, DriverHostState, LinuxCapability, NamespaceIsolation, SandboxPolicy,
    SharedSlotGrant,
};
pub use model::{
    BackendDigest, BackendIdentity, BackendInventory, BackendPackage, BackendPackageHandle,
    DeviceCatalogDigest, DeviceIdentity, DriverAuthority, DriverContractVersion,
    DriverExecutionMode, DriverGroupBinding, DriverImplementationKind, DriverInstanceHandle,
    DriverInstancePlan, DriverLimits, EvidenceDigest, InterfaceIdentity, SandboxDigest,
    SourceDigest,
};
pub use simulator::{
    DeterministicSimulator, FaultInjection, GoldenTraceRecord, SimulationFault, TraceResult,
};
