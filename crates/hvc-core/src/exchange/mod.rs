//! Agent exchange (ADR-037): the provenance of everything a model reads.
//!
//! The vocabulary (`ExchangeItemV1`, producers, trust classes) lives in
//! `hvc_contracts::exchange`; this module renders it for prompts. The
//! baseline measurements of ADR-037 are in `baseline_tests.rs`, a test module
//! of `persistence_service`.

pub(crate) mod render;
