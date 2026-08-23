// Os DTOs de erro Serde permanecem por valor para manter os contratos Tauri durante a migração.
#![allow(clippy::result_large_err)]
#![allow(clippy::too_many_arguments)]

pub mod agent_runtime;
pub mod app_core;
pub mod chat_service;
pub mod embedding_service;
pub mod events;
pub mod llm;
pub mod persistence_service;
pub mod repository_context;
pub mod workspace_service;

pub use app_core::{AppCore, AppPaths, DataDirectoryLock};
pub use events::{AppEvent, AppEventBus};
