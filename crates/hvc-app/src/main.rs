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
        .context("failed to initialize the async runtime")?;
    let paths = AppPaths::resolve()?;
    let runtime_handle = runtime.handle().clone();
    let core_initialization = std::thread::Builder::new()
        .name("hvc-core-init".into())
        .spawn(move || AppCore::initialize(paths, runtime_handle).map_err(anyhow::Error::from))
        .context("failed to start the parallel core initialization")?;

    let application = Application::new().with_assets(hvc_ui::HvcAssets);
    let core = core_initialization
        .join()
        .map_err(|_| anyhow::anyhow!("the core initialization worker ended unexpectedly"))??;
    let ui_core = Arc::clone(&core);

    let _tokio_guard = runtime.enter();

    application.run(move |cx: &mut App| {
        hvc_ui::open_main_window(Arc::clone(&ui_core), cx)
            .expect("failed to open the main GPUI window");
    });

    runtime.block_on(core.shutdown());
    drop(core);
    runtime.shutdown_timeout(Duration::from_secs(5));
    Ok(())
}
