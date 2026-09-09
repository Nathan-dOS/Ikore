// A UI consome temporariamente os DTOs de erro por valor do contrato compartilhado.
#![allow(clippy::result_large_err)]

mod agents;
mod agents_view;
mod app_model;
mod chat;
mod chat_view;
mod choices;
mod errors;
mod flow;
mod forge;
mod forge_view;
mod hitl;
#[cfg(test)]
mod layout_tests;
mod lifecycle;
mod logs;
mod logs_view;
mod native_editor;
mod notifications;
mod performance;
mod pulsations;
mod pulsations_view;
mod review_view;
mod select_menu;
mod session;
mod settings;
mod settings_view;
mod shell;
mod split_pane;
#[cfg(test)]
mod test_support;
mod text_input;
mod theme;
pub mod time;
mod usage;
mod vault;
mod vault_view;
mod views;
mod widgets;
mod workspace;
mod workspace_view;

pub use app_model::{AppModel, AppTab, AppView};
pub use native_editor::NativeEditor;
pub use shell::open_main_window;
