//! Feature 08 M1: host-owned git & gh executable discovery, safe subprocess
//! launch (direct argv, no shell, hidden window, `ProcessTreeJob`), project git
//! rules persistence, and strictly read-only status inspection.

pub mod process;
pub mod rules;
pub mod status;
#[cfg(test)]
mod tests;

use thiserror::Error;

pub use ikore_contracts::git_tool::*;
pub use process::{HostBinary, ResolvedHostBinary, resolve_host_binary};
pub use rules::{canonical_rules_json_and_sha256, validate_commit_rules};
pub use status::{inspect_git_status_for_path, parse_gh_auth_status, scrub_remote_url};

#[derive(Debug, Error)]
pub enum GitToolError {
    #[error("{0}")]
    InvalidExecutable(String),
    #[error("Git tool subprocess failed: {0}")]
    Process(String),
    #[error("Git tool timed out after {0} seconds.")]
    TimedOut(u64),
    #[error("{0}")]
    SecretRedactorUnavailable(String),
}
