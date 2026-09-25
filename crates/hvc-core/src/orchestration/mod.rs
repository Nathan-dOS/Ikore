//! ADR-007: contracts, integrity, coordination and usable preparation (M0–M4).
//!
//! The persistence facade enforces acceptance and publication through `serial.rs`.
//! The persistence facade also owns FIFO admission, reconciliation and supervisor slots.
//! Local preparation is opt-in; Specification uses safe code context independently.
//! Consultative Learning and delta indexing use the bounded background executor.

pub mod brownfield;
pub mod catalog;
mod code_context;
pub mod composition;
mod context;
mod contracts;
pub mod failure;
pub mod learning_report;
mod pipeline_version;
mod policy;
mod prompt_context;
mod tree_sitter_context;
pub use learning_report::{
    ContextAuditStageSnapshot, LearningActionRequest, LearningGraphDecision,
    LearningGraphProposalSnapshot, LearningRefinementMode, LearningRunSnapshot,
    ReviewLearningGraphDecision, ReviewLearningGraphRequest,
};
pub(crate) mod publication;
pub mod task_refine;
pub(crate) mod upgrade;

#[cfg(test)]
mod tests;

pub use code_context::{LexicalCodeContextService, RoutedCodeContextService};
pub use context::*;
pub use contracts::*;
pub use failure::*;
pub use pipeline_version::PipelineVersion;
pub use policy::*;
pub(crate) use prompt_context::code_context_prompt_json;
pub use tree_sitter_context::{
    LEXICAL_FALLBACK_VERSION, RUST_QUERY_VERSION, SourceLanguage, SourceStructuralFacts,
    StructuralParseStatus, TREE_SITTER_PARSER_VERSION, TYPESCRIPT_QUERY_VERSION,
    TreeSitterExtractor,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    #[error("Invalid contract: {0}.")]
    Invalid(&'static str),
    #[error("Unsupported contract version: {0}.")]
    UnsupportedVersion(u8),
    #[error("The run belongs to a stale revision or generation.")]
    StaleExecution,
    #[error("The run was already cancelled or finished.")]
    ExecutionClosed,
    #[error("The budget cannot fit the required content.")]
    BudgetExceeded,
    #[error("Coleta cancelada.")]
    Cancelled,
}

/// A persisted envelope must opt into a known schema; unknown versions fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct SchemaV1;

impl TryFrom<u8> for SchemaV1 {
    type Error = ContractError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value == 1 {
            Ok(Self)
        } else {
            Err(ContractError::UnsupportedVersion(value))
        }
    }
}

impl From<SchemaV1> for u8 {
    fn from(_: SchemaV1) -> Self {
        1
    }
}
