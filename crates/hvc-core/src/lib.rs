// Os DTOs de erro Serde permanecem por valor porque também são persistidos e
// exercitados nos contratos tipados do core.
#![allow(clippy::result_large_err)]
#![allow(clippy::too_many_arguments)]

pub mod agent_output_language;
pub mod agent_runtime;
pub mod agent_timeout;
pub mod app_core;
pub mod attestation;
pub mod chat_service;
pub mod companion_dock;
pub mod companion_drawings;
pub mod companion_notes;
pub mod doc_reconciliation;
pub mod document_cache;
pub mod embedding_service;
pub mod events;
pub mod exchange;
pub mod feature_forge;
pub mod llm;
pub mod orchestration;
pub mod persistence_service;
pub mod project_deletion;
pub mod quota;
pub mod reports;
pub mod repository_context;
pub mod sanitization;
pub mod secret_redactor;
pub mod stage_briefs;
pub mod terminals;
mod terminals_host;
pub mod usage;
pub mod voice;
pub mod workspace_service;

pub use agent_timeout::{AgentTimeoutPolicy, ProgressWatchdog, TimeoutReason};
pub use app_core::{AppCore, AppCoreDependencies, AppPaths, DataDirectoryLock};
pub use events::{AppEvent, AppEventBus};

/// Lowercase hexadecimal of a digest. sha2 0.11 digests no longer implement `LowerHex`.
pub(crate) fn lower_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}
