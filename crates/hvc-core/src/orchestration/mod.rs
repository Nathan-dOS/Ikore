//! ADR-007: contracts and serial integrity (M0/M1).
//!
//! The persistence facade enforces acceptance and publication through `serial.rs`.
//! M2/M3 will add ownership admission, background scheduling and code collection;
//! neither concurrent preparation nor clarification checkpoints are enabled here.

pub mod catalog;
mod context;
mod contracts;
mod policy;
pub(crate) mod publication;
pub(crate) mod upgrade;

#[cfg(test)]
mod tests;

pub use context::*;
pub use contracts::*;
pub use policy::*;

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
