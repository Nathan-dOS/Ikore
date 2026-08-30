//! ADR-007: contracts, integrity, coordination and usable preparation (M0–M4).
//!
//! The persistence facade enforces acceptance and publication through `serial.rs`.
//! The persistence facade also owns FIFO admission, reconciliation and supervisor slots.
//! Local preparation is opt-in; Specification uses safe code context independently.
//! Consultative Learning and delta indexing use the bounded background executor.

pub mod brownfield;
pub mod catalog;
mod code_context;
mod context;
mod contracts;
pub mod learning_report;
mod policy;
mod tree_sitter_context;
pub use learning_report::{LearningActionRequest, LearningRunSnapshot};
pub(crate) mod publication;
pub mod task_refine;
pub(crate) mod upgrade;

#[cfg(test)]
mod tests;

pub use code_context::{LexicalCodeContextService, RoutedCodeContextService};
pub use context::*;
pub use contracts::*;
pub use policy::*;
pub use tree_sitter_context::{
    LEXICAL_FALLBACK_VERSION, RUST_QUERY_VERSION, SourceLanguage, SourceStructuralFacts,
    StructuralParseStatus, TREE_SITTER_PARSER_VERSION, TYPESCRIPT_QUERY_VERSION,
    TreeSitterExtractor,
};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    #[error("Contrato inválido: {0}.")]
    Invalid(&'static str),
    #[error("Versão de contrato não suportada: {0}.")]
    UnsupportedVersion(u8),
    #[error("A execução pertence a uma revisão ou geração obsoleta.")]
    StaleExecution,
    #[error("A execução já foi cancelada ou encerrada.")]
    ExecutionClosed,
    #[error("O orçamento não comporta o conteúdo obrigatório.")]
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
