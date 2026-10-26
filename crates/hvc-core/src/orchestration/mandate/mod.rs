//! Execution mandates (ADR-035 M1): the explicit, hash-chained record of what
//! the developer authorized by approving the Specification, and the receipts
//! of every checkpoint decision.
//!
//! Agents never write to the workspace; the host publishes every file. A
//! mandate is therefore not a sandbox: it gathers the authority the v4 plan
//! already enforces (owned destinations, executor policies, check profiles,
//! budget) into one record the developer sees before approving, and adds one
//! more check at the existing write and dispatch points.
//!
//! The pure parts live here; `persistence_service::mandate_persistence`
//! gathers the inputs, issues revisions and runs the checks in the host's
//! transactions. No model ever receives, cites or edits a mandate.

pub mod contract;
pub mod derive;
pub mod guard;
#[cfg(test)]
mod tests;

pub use contract::*;
pub use derive::{
    IssueInputs, amend_transport, derive_issued, digest_of_set, executor_capabilities,
};
pub use guard::{
    WriteSource, WriteVerdict, check_checks, check_executor, check_transport, check_write,
};
