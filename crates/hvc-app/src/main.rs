#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use gpui::{App, Application};
use hvc_core::{AppCore, AppPaths};

fn main() -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("hvc-runtime")
        .build()
        .context("falha ao inicializar o runtime assíncrono")?;
    let core = AppCore::initialize(AppPaths::resolve()?, runtime.handle().clone())?;
    let ui_core = Arc::clone(&core);

    Application::new().run(move |cx: &mut App| {
        hvc_ui::open_main_window(Arc::clone(&ui_core), cx)
            .expect("falha ao abrir a janela principal GPUI");
    });

    runtime.block_on(core.shutdown());
    drop(core);
    runtime.shutdown_timeout(Duration::from_secs(5));
    Ok(())
}
