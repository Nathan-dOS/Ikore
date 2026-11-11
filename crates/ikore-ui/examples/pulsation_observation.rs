#![allow(
    clippy::unwrap_used,
    reason = "Manual visual fixtures fail immediately when isolated setup is invalid"
)]
//! Isolated native smoke: `cargo run -p ikore-ui --example pulsation_observation`.
//! A local scripted provider runs one real pulsation. Only this disposable
//! fixture's checkpoints are auto-approved. No installed CLI or credentials are used.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use ikore_core::persistence_service::{
    ApplyHitlDecisionRequest, CreatePulsationRequest, EnsureProjectForWorkspaceRequest,
    GetPulsationRuntimeSnapshotRequest, HitlDecision, PulsationStatus, SaveUiSessionRequest,
};
use ikore_test_support::pulsation_benchmark::{
    configure_loopback, force_api_direct, stage_response,
};
use ikore_test_support::{CoreTestHarness, ScriptedHttpServer};

fn main() {
    let harness = CoreTestHarness::new();
    let core = harness.core();
    let server = ScriptedHttpServer::respond_with(|request| {
        std::thread::sleep(Duration::from_secs(2));
        stage_response(request)
    });
    configure_loopback(core.persistence(), &server);
    let workspace = harness.workspace_dir().to_string_lossy().into_owned();
    let project = core
        .persistence()
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: workspace.clone(),
            preferred_name: Some("Isolated observation smoke".into()),
        })
        .unwrap()
        .project;
    force_api_direct(core.persistence(), &project.id);
    core.persistence()
        .save_ui_session(SaveUiSessionRequest {
            project_id: project.id.clone(),
            workspace_root: workspace,
            active_view: "pulsations".into(),
            active_tab_path: None,
            workspace_view_mode: "editor".into(),
            sidebar_collapsed: false,
            context_drawer_open: false,
            active_pulsation_id: None,
            selected_vault_path: None,
            active_chat_conversation_id: None,
            scroll_top: None,
            tabs: Vec::new(),
            auto_open_review: false,
        })
        .unwrap();
    let report = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/tab-independence-native-report.json");
    let worker_core = Arc::clone(&core);
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(10));
        let result = (|| -> anyhow::Result<serde_json::Value> {
            let pulse = worker_core.persistence().create_pulsation(CreatePulsationRequest {
                project_id: project.id.clone(),
                feature_name: "Describe validation of an expired token returning status 401. Fixture only.".into(),
            })?.pulsation;
            let started = Instant::now();
            let mut approvals = 0;
            loop {
                let snapshot = worker_core.persistence().get_pulsation_runtime_snapshot(
                    GetPulsationRuntimeSnapshotRequest {
                        project_id: project.id.clone(),
                    },
                )?;
                let current = snapshot
                    .snapshot
                    .pulsations
                    .iter()
                    .find(|item| item.id == pulse.id)
                    .unwrap();
                if current.status == PulsationStatus::Done {
                    return Ok(
                        serde_json::json!({"status":"passed", "transport":"scripted_loopback_api",
                        "approvals":approvals, "elapsed_ms":started.elapsed().as_millis(),
                        "window_states":"Record separately through native observation; not inferred from completion"}),
                    );
                }
                anyhow::ensure!(
                    current.status != PulsationStatus::Failed,
                    "fixture pulse failed"
                );
                anyhow::ensure!(
                    started.elapsed() < Duration::from_secs(90),
                    "fixture deadline exceeded"
                );
                if let Ok(checkpoint) = worker_core.persistence().get_hitl_checkpoint(&pulse.id) {
                    std::thread::sleep(Duration::from_secs(2));
                    worker_core
                        .persistence()
                        .apply_hitl_decision(ApplyHitlDecisionRequest {
                            pulsation_id: pulse.id.clone(),
                            checkpoint_id: checkpoint.checkpoint_id,
                            expected_state_version: checkpoint.state_version,
                            feedback: None,
                            decision: HitlDecision::Approve,
                        })?;
                    approvals += 1;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        })();
        let value = result.unwrap_or_else(
            |error| serde_json::json!({"status":"failed", "error":error.to_string()}),
        );
        std::fs::write(report, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    });
    let handle = core.runtime_handle().clone();
    let _entered = handle.enter();
    gpui::Application::new()
        .with_assets(ikore_ui::HvcAssets)
        .run(move |cx| ikore_ui::open_main_window(core, cx).unwrap());
    worker.join().unwrap();
    drop(server);
    drop(harness);
}
