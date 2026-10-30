#![allow(
    clippy::unwrap_used,
    reason = "Test-only harnesses fail immediately when fixture setup is invalid"
)]
pub mod brownfield_benchmark;
pub mod brownfield_graph_benchmark;
pub mod composition_benchmark;
pub mod composition_replies;
#[cfg(test)]
mod contracts;
mod controlled_gateway;
mod core_harness;
mod doubles;
mod http_fixture;
mod parity;
pub mod prompt_inventory;
pub mod pulsation_benchmark;
mod pulsation_fixture;

pub use controlled_gateway::ControlledLlmGateway;
pub use core_harness::{CoreTestHarness, DatabaseVersion, IsolatedTestEnvironment};
pub use doubles::{
    FixedEmbeddingGateway, FixedJitter, ManualClock, MemoryClipboard, MemoryCredentialStore,
    ScriptedPathPicker,
};
pub use http_fixture::{RecordedRequest, ScriptedHttpServer, ScriptedResponse};
/// Moved to `hvc-core` so the app's pulsation report shares it (ADR-027 M6).
pub use hvc_core::reports::analysis as pulsation_analysis;
pub use parity::{ParityArea, ParityCase, ParityLedger, ParityStatus};
pub use pulsation_fixture::{LegacyPulseLayout, PulsationFixture};
