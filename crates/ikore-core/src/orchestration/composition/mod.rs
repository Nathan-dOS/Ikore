//! Host-coordinated stage composition (ADR-022, pipeline v4).
//!
//! A stage is executed as a versioned host recipe: deterministic preparation,
//! zero or more bounded semantic work units, deterministic validation and one
//! aggregate outcome. Workers never own persistent identifiers, hashes, paths,
//! publication, approval or stage advancement; every one of those is produced
//! or checked here.
//!
//! The wire contract is `stage-composition-v1` (see `fixtures/`). This module
//! is additive: v1-v3 pulsations never enter it.

pub mod aggregate;
pub mod anchors;
pub mod budget;
pub mod contracts;
pub mod edits;
// Lives in `ikore-contracts`, so the terminal crate renders resume notices with it (ADR-037 M3).
pub use ikore_contracts::line_diff;
pub mod parse;
pub mod plan;
pub mod planner;
pub mod policy;
pub mod projection;
pub mod recipes;
pub mod references;
pub mod render;
pub mod review_boundary;
pub mod review_policy;
pub mod risk;
pub mod schema;
pub mod settings;
pub mod validate;
pub mod validation;

use serde::{Deserialize, Serialize};

pub use contracts::*;

/// The invariant registry from `Stage-Composition-Contracts.md` §7.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Invariant {
    #[serde(rename = "SC-001")]
    Sc001,
    #[serde(rename = "SC-002")]
    Sc002,
    #[serde(rename = "SC-003")]
    Sc003,
    #[serde(rename = "SC-004")]
    Sc004,
    #[serde(rename = "SC-005")]
    Sc005,
    #[serde(rename = "SC-006")]
    Sc006,
    #[serde(rename = "SC-007")]
    Sc007,
    #[serde(rename = "SC-008")]
    Sc008,
    #[serde(rename = "SC-009")]
    Sc009,
    #[serde(rename = "SC-010")]
    Sc010,
    #[serde(rename = "SC-011")]
    Sc011,
    #[serde(rename = "SC-012")]
    Sc012,
    #[serde(rename = "SC-013")]
    Sc013,
    #[serde(rename = "SC-014")]
    Sc014,
    #[serde(rename = "SC-015")]
    Sc015,
    #[serde(rename = "SC-016")]
    Sc016,
    #[serde(rename = "SC-017")]
    Sc017,
    #[serde(rename = "SC-018")]
    Sc018,
    #[serde(rename = "SC-019")]
    Sc019,
    #[serde(rename = "SC-020")]
    Sc020,
    #[serde(rename = "SC-021")]
    Sc021,
    #[serde(rename = "SC-022")]
    Sc022,
    #[serde(rename = "SC-023")]
    Sc023,
    #[serde(rename = "SC-024")]
    Sc024,
}

impl Invariant {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sc001 => "SC-001",
            Self::Sc002 => "SC-002",
            Self::Sc003 => "SC-003",
            Self::Sc004 => "SC-004",
            Self::Sc005 => "SC-005",
            Self::Sc006 => "SC-006",
            Self::Sc007 => "SC-007",
            Self::Sc008 => "SC-008",
            Self::Sc009 => "SC-009",
            Self::Sc010 => "SC-010",
            Self::Sc011 => "SC-011",
            Self::Sc012 => "SC-012",
            Self::Sc013 => "SC-013",
            Self::Sc014 => "SC-014",
            Self::Sc015 => "SC-015",
            Self::Sc016 => "SC-016",
            Self::Sc017 => "SC-017",
            Self::Sc018 => "SC-018",
            Self::Sc019 => "SC-019",
            Self::Sc020 => "SC-020",
            Self::Sc021 => "SC-021",
            Self::Sc022 => "SC-022",
            Self::Sc023 => "SC-023",
            Self::Sc024 => "SC-024",
        }
    }
}

/// Closed error classes from `Stage-Composition-Persistence-and-Recovery.md` §6,
/// plus the host-side admission classes that never reach a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidShape,
    InvalidLanguage,
    OutputTruncated,
    ReferenceConflict,
    StaleInput,
    OwnershipViolation,
    UnsupportedOperation,
    ContractConflict,
    ImplementationMismatch,
    ScopeInsufficient,
    NeedsInput,
    ValidationFailed,
    ValidationUnavailable,
    BackendUnavailable,
    TransportFailure,
    BudgetExhausted,
    CapacityExceeded,
    EvidenceUnresolved,
    HostContractError,
    Cancelled,
    Superseded,
    StaleResult,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidShape => "invalid_shape",
            Self::InvalidLanguage => "invalid_language",
            Self::OutputTruncated => "output_truncated",
            Self::ReferenceConflict => "reference_conflict",
            Self::StaleInput => "stale_input",
            Self::OwnershipViolation => "ownership_violation",
            Self::UnsupportedOperation => "unsupported_operation",
            Self::ContractConflict => "contract_conflict",
            Self::ImplementationMismatch => "implementation_mismatch",
            Self::ScopeInsufficient => "scope_insufficient",
            Self::NeedsInput => "needs_input",
            Self::ValidationFailed => "validation_failed",
            Self::ValidationUnavailable => "validation_unavailable",
            Self::BackendUnavailable => "backend_unavailable",
            Self::TransportFailure => "transport_failure",
            Self::BudgetExhausted => "budget_exhausted",
            Self::CapacityExceeded => "capacity_exceeded",
            Self::EvidenceUnresolved => "evidence_unresolved",
            Self::HostContractError => "host_contract_error",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
            Self::StaleResult => "stale_result",
        }
    }
}

/// A bounded, English, machine-classified rejection. Raw model excerpts are
/// never embedded; `related` carries only handles the model was shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "camelCase")]
#[error("{sc:?} {code:?}: {message}")]
pub struct CompositionError {
    pub sc: Invariant,
    pub code: ErrorCode,
    pub message: String,
    pub related: Vec<Handle>,
}

impl CompositionError {
    pub fn new(sc: Invariant, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            sc,
            code,
            message: message.into(),
            related: Vec::new(),
        }
    }

    pub fn with_related(mut self, related: impl IntoIterator<Item = Handle>) -> Self {
        self.related.extend(related);
        self
    }

    pub fn shape(message: impl Into<String>) -> Self {
        Self::new(Invariant::Sc001, ErrorCode::InvalidShape, message)
    }

    pub fn identity(message: impl Into<String>) -> Self {
        Self::new(Invariant::Sc003, ErrorCode::InvalidShape, message)
    }
}

#[cfg(test)]
mod fixtures {
    //! Versioned schema and examples for the stage-composition contract.
    //! See `docs/adr/022-stage-composition.md` for the public contract map.
    //! Historical fixture digests remain in the original validation records.
    pub const SCHEMA: &str = include_str!("fixtures/stage-composition-v1.schema.json");
    pub const EXAMPLES: &str = include_str!("fixtures/stage-composition-v1.examples.json");

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Example {
        pub name: String,
        pub definition: String,
        pub schema_valid: bool,
        pub host_rule_violation: Option<String>,
        pub instance: serde_json::Value,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct Manifest {
        pub examples: Vec<Example>,
    }

    pub fn schema() -> serde_json::Value {
        serde_json::from_str(SCHEMA).expect("registry schema parses")
    }

    pub fn examples() -> Vec<Example> {
        serde_json::from_str::<Manifest>(EXAMPLES)
            .expect("example manifest parses")
            .examples
    }

    /// A validator for one `$defs` entry of the registry.
    pub fn validator_for(definition: &str) -> jsonschema::Validator {
        let mut schema = schema();
        schema["allOf"] = serde_json::json!([{ "$ref": format!("#/$defs/{definition}") }]);
        jsonschema::options()
            .should_validate_formats(true)
            .build(&schema)
            .expect("registry definition compiles")
    }

    #[test]
    fn registry_fixture_is_the_documented_one() {
        let schema = schema();
        assert_eq!(schema["$id"], "urn:hvc:stage-composition:v1");
        assert_eq!(schema["$defs"].as_object().unwrap().len(), 48);
        assert_eq!(examples().len(), 53);
    }
}
