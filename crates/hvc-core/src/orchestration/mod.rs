//! ADR-007 / M0: contracts for the next orchestration runtime.
//!
//! These types and pure checks do not schedule jobs, publish files or change the
//! existing serial worker. M1 must enforce them inside its database transactions;
//! M2/M3 will connect scheduling and context collection to the application.

mod context;
mod contracts;
mod policy;

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
