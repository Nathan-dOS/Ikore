//! Agent exchange (ADR-037): the provenance of everything a model reads, and
//! the durable log of what changed.
//!
//! The vocabulary (`ExchangeItemV1`, producers, trust classes) lives in
//! `hvc_contracts::exchange`; `render` turns it into prompt text and `log`
//! records domain writes for subscribers that fall behind. The baseline
//! measurements of ADR-037 are in `baseline_tests.rs`, a test module of
//! `persistence_service`.

pub mod log;
pub(crate) mod render;

/// What a resumed CLI session is told changed (ADR-037 M3).
pub use hvc_contracts::external_changes;
