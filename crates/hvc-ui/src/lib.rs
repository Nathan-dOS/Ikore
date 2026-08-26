// A UI consome temporariamente os DTOs de erro por valor do contrato compartilhado.
#![allow(clippy::result_large_err)]

mod app_model;
mod errors;
mod hitl;
mod lifecycle;
mod logs;
mod logs_view;
mod native_editor;
mod pulsations;
mod pulsations_view;
mod session;
mod settings;
mod settings_view;
mod shell;
#[cfg(test)]
mod test_support;
mod text_input;
mod theme;
mod vault;
mod vault_view;
mod views;
mod workspace;

pub use app_model::{AppModel, AppView};
pub use native_editor::NativeEditor;
pub use shell::open_main_window;
