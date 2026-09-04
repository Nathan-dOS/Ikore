// Os DTOs de erro Serde permanecem por valor porque também são persistidos e
// exercitados nos contratos tipados do core.
#![allow(clippy::result_large_err)]
#![allow(clippy::too_many_arguments)]

pub mod agent_output_language;
pub mod agent_runtime;
pub mod app_core;
pub mod chat_service;
pub mod document_cache;
pub mod embedding_service;
pub mod events;
pub mod llm;
pub mod orchestration;
pub mod persistence_service;
pub mod repository_context;
pub mod secret_redactor;
pub mod terminals;
pub mod workspace_service;

pub use app_core::{AppCore, AppCoreDependencies, AppPaths, DataDirectoryLock};
pub use events::{AppEvent, AppEventBus};
