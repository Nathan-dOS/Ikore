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
    let paths = AppPaths::resolve()?;
    let runtime_handle = runtime.handle().clone();
    let core_initialization = std::thread::Builder::new()
        .name("hvc-core-init".into())
        .spawn(move || AppCore::initialize(paths, runtime_handle).map_err(anyhow::Error::from))
        .context("falha ao iniciar a inicialização paralela do core")?;

    // O backend gráfico do GPUI e o AppCore não dependem um do outro. Criá-los
    // em paralelo retira migrations/recovery do caminho serial de abertura sem
    // permitir que a UI acesse um core parcialmente inicializado.
    let application = Application::new();
    let core = core_initialization.join().map_err(|_| {
        anyhow::anyhow!("worker de inicialização do core encerrou inesperadamente")
    })??;
    let ui_core = Arc::clone(&core);

    let _tokio_guard = runtime.enter();

    application.run(move |cx: &mut App| {
        hvc_ui::open_main_window(Arc::clone(&ui_core), cx)
            .expect("falha ao abrir a janela principal GPUI");
    });

    runtime.block_on(core.shutdown());
    drop(core);
    runtime.shutdown_timeout(Duration::from_secs(5));
    Ok(())
}
