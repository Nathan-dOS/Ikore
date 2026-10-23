#![allow(
    clippy::unwrap_used,
    reason = "Manual visual fixtures fail immediately when isolated setup is invalid"
)]
//! Isolated native smoke of the "Leftover files" card:
//! `cargo run -p hvc-ui --example failed_pulsation_cleanup`.
//!
//! A local scripted provider drives one pulsation up to the Test checkpoint,
//! publishing two files on the Code stage, and then the fixture cancels it.
//! The window then shows a failed pulsation whose Overview offers the cleanup.
//! Only this disposable fixture's checkpoints are auto-approved; no installed
//! CLI or credentials are used. The workspace root is printed so the files can
//! be inspected before and after the removal.
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use hvc_core::persistence_service::{
    ApplyHitlDecisionRequest, CancelPulsationJobsRequest, CreatePulsationRequest,
    EnsureProjectForWorkspaceRequest, GetPulsationRuntimeSnapshotRequest, HitlDecision,
    PulsationStage, PulsationStatus, SaveUiSessionRequest,
};
use hvc_test_support::pulsation_benchmark::{configure_loopback, force_api_direct, stage_response};
use hvc_test_support::{CoreTestHarness, ScriptedHttpServer, ScriptedResponse};
use serde_json::{Value, json};

/// The benchmark's stage reply, except that Code publishes two files.
fn reply(request: &hvc_test_support::RecordedRequest) -> ScriptedResponse {
    let body: Value = serde_json::from_str(&request.body).expect("provider request JSON");
    let prompt = body["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let stage = prompt
        .lines()
        .find_map(|l| l.strip_prefix("- stage: "))
        .unwrap_or_default();
    if stage != "Code" {
        return stage_response(request);
    }
    let output = json!({
        "outputLanguage": "en",
        "summary": "Code fixture",
        "statusUpdate": "advance",
        "contextUsage": [],
        "notes": [],
        "artifacts": [
            {"name": "expired_token", "kind": "source", "relativePath": "src/generated/expired_token.rs",
             "content": "pub fn reject_expired_token() -> u16 { 401 }\n"},
            {"name": "expired-token", "kind": "doc", "relativePath": "docs/expired-token.md",
             "content": "# Expired token\nFixture: an expired token returns 401.\n"}
        ]
    });
    ScriptedResponse::json(200, json!({"choices":[{"message":{"content":output.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":64,"completion_tokens":80}}).to_string())
}

fn main() {
    let harness = CoreTestHarness::new();
    let core = harness.core();
    let server = ScriptedHttpServer::respond_with(|request| {
        std::thread::sleep(Duration::from_millis(500));
        reply(request)
    });
    configure_loopback(core.persistence(), &server);
    // The UI registers the workspace under its canonical path, so the seeded
    // project and session must use the same spelling to be picked up.
    let workspace = std::fs::canonicalize(harness.workspace_dir())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir_all(harness.workspace_dir().join("src")).unwrap();
    std::fs::write(
        harness.workspace_dir().join("src/lib.rs"),
        "pub fn existing() {}\n",
    )
    .unwrap();
    println!("workspace: {workspace}");
    let project = core
        .persistence()
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: workspace.clone(),
            preferred_name: Some("Leftover files smoke".into()),
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
    let worker_core = Arc::clone(&core);
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        let result = (|| -> anyhow::Result<()> {
            let pulse = worker_core
                .persistence()
                .create_pulsation(CreatePulsationRequest {
                    project_id: project.id.clone(),
                    feature_name:
                        "Reject an expired token with status 401. Fixture only; publishes two files."
                            .into(),
                })?
                .pulsation;
            let started = Instant::now();
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
                anyhow::ensure!(
                    current.status != PulsationStatus::Failed
                        && current.status != PulsationStatus::Done,
                    "fixture pulse ended before the Test checkpoint"
                );
                anyhow::ensure!(
                    started.elapsed() < Duration::from_secs(120),
                    "fixture deadline exceeded"
                );
                if let Ok(checkpoint) = worker_core.persistence().get_hitl_checkpoint(&pulse.id) {
                    if current.stage == PulsationStage::Test {
                        // Files are on disk; the user gives up here.
                        worker_core.persistence().cancel_pulsation_jobs(
                            CancelPulsationJobsRequest {
                                pulsation_id: pulse.id.clone(),
                            },
                        )?;
                        println!(
                            "pulsation {} cancelled at Test with published files",
                            pulse.id
                        );
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_secs(1));
                    worker_core
                        .persistence()
                        .apply_hitl_decision(ApplyHitlDecisionRequest {
                            pulsation_id: pulse.id.clone(),
                            checkpoint_id: checkpoint.checkpoint_id,
                            expected_state_version: checkpoint.state_version,
                            feedback: None,
                            decision: HitlDecision::Approve,
                        })?;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
        })();
        if let Err(error) = result {
            eprintln!("fixture failed: {error}");
        }
    });
    let handle = core.runtime_handle().clone();
    let _entered = handle.enter();
    gpui::Application::new()
        .with_assets(hvc_ui::HvcAssets)
        .run(move |cx| hvc_ui::open_main_window(core, cx).unwrap());
    worker.join().unwrap();
    // Evidence for the smoke: which published files survived the session.
    for path in [
        "src/generated/expired_token.rs",
        "docs/expired-token.md",
        "src/lib.rs",
    ] {
        println!(
            "{path}: {}",
            if harness.workspace_dir().join(path).exists() {
                "present"
            } else {
                "removed"
            }
        );
    }
    drop(server);
    drop(harness);
}
