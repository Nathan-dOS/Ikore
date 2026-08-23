// A UI consome temporariamente os DTOs de erro por valor do contrato compartilhado.
#![allow(clippy::result_large_err)]

mod app_model;
mod native_editor;
mod shell;
#[cfg(test)]
mod test_support;
mod theme;
mod views;

pub use app_model::{AppModel, AppView};
pub use native_editor::NativeEditor;
pub use shell::open_main_window;
