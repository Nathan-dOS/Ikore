//! Development fixture for ADR-024: a v4 pulsation parked at its Specification
//! checkpoint, so the real GUI can be opened on the Review Brief tab.
//!
//! Usage: `review_brief_fixture NEW_DIRECTORY`, then start `hvc-app` with
//! `HVC_DATA_DIR` set to the printed data directory. Every provider reply comes
//! from the scripted composition worker; the worker stops once the checkpoint
//! is waiting, so the GUI never races it. Provider credentials point at
//! fixture-only names, never the developer's.
use hvc_core::{
    AppCoreDependencies,
    feature_forge::{
        AcceptanceCriterion, CreatePulsationFromTasksRequest, CreateTasksRequest, TaskDraft,
        TaskKind, TaskSelection, TaskService,
    },
    llm::{LlmEventEnvelope, LlmEventSink},
    orchestration::{PreparationSettings, composition::settings::CompositionSettings},
    persistence_service::*,
    workspace_service::bootstrap_greenfield_workspace,
};
use hvc_test_support::composition_replies::{
    PromptFacts, ReplyOptions, parse_prompt, unit_response, unit_response_with,
};
use hvc_test_support::pulsation_benchmark::{configure_loopback, force_api_direct};
use hvc_test_support::{
    FixedEmbeddingGateway, MemoryCredentialStore, ScriptedHttpServer, ScriptedResponse,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const SOURCE: &str = "pub struct Token {\n    pub expires_at: u64,\n}\n\npub fn validate_token(token: &Token, now: u64) -> bool {\n    let _ = (token, now);\n    true\n}\n";

struct Quiet;
impl LlmEventSink for Quiet {
    fn emit(&self, _: &LlmEventEnvelope) {}
}

fn criterion(id: &str, description: &str) -> AcceptanceCriterion {
    AcceptanceCriterion {
        id: id.into(),
        description: description.into(),
    }
}

fn tasks() -> Vec<TaskDraft> {
    vec![
        TaskDraft {
            title: "Reject expired tokens".into(),
            kind: TaskKind::Bug,
            description: "validate_token in src/auth.rs accepts every token; add the expiry check and a unit test in tests/auth_test.rs".into(),
            criteria: vec![
                criterion("AC-1", "An expired token is rejected by validate_token"),
                criterion("AC-2", "A token that expires exactly now is rejected"),
            ],
            dependencies: vec![],
            feature_id: None,
        },
        TaskDraft {
            title: "Keep valid tokens working".into(),
            kind: TaskKind::Adjustment,
            description: "validate_token in src/auth.rs must keep accepting tokens that are still valid".into(),
            criteria: vec![criterion(
                "AC-3",
                "A token that expires in the future is still accepted",
            )],
            dependencies: vec![],
            feature_id: None,
        },
    ]
}

/// The default scripted reply, except that the Specification proposes one
/// code unit covering every criterion and a test unit after it, as the host
/// requires for a multi-task batch.
fn reply(facts: &PromptFacts) -> Value {
    if facts.executor != "specification_author" {
        return unit_response(facts);
    }
    let source = facts.handle_for_path("src/auth.rs");
    let test_destination = facts
        .references
        .iter()
        .find(|(_, row)| row.path == "tests/auth_test.rs" && row.planned)
        .map(|(handle, _)| handle.as_str())
        .unwrap_or(source);
    let first_task = facts.tasks[0].as_str();
    let first_task_criteria: Vec<&str> = facts
        .criteria
        .iter()
        .filter(|(task, _)| task == first_task)
        .map(|(_, criterion)| criterion.as_str())
        .collect();
    let options = ReplyOptions {
        proposals: Some(json!([
            {
                "unitKey": "implement_expiry", "kind": "code", "taskRefs": facts.tasks,
                "criterionRefs": facts.criteria.iter().map(|(_, criterion)| criterion).collect::<Vec<_>>(),
                "inputRefs": [source], "destinationRefs": [source], "dependsOn": [],
                "objective": "Implement the expiry comparison in validate_token."
            },
            {
                "unitKey": "cover_expiry", "kind": "test", "taskRefs": [first_task],
                "criterionRefs": first_task_criteria,
                "inputRefs": [source], "destinationRefs": [test_destination],
                "dependsOn": ["implement_expiry"],
                "objective": "Cover expired and boundary tokens with unit tests."
            }
        ])),
        ..ReplyOptions::default()
    };
    unit_response_with(facts, &options)
}

fn main() {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("usage: review_brief_fixture NEW_DIRECTORY"),
    );
    assert!(!root.exists(), "refuse to reuse or overwrite {root:?}");
    fs::create_dir_all(&root).unwrap();
    // The GUI registers projects by their canonical root (`\\?\` form on
    // Windows); any other spelling would restore as a second, empty project.
    let root = root.canonicalize().unwrap();
    let workspace = root.join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    // Follow the real host's bootstrap so the GUI can restore the session.
    bootstrap_greenfield_workspace(&workspace).unwrap();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/auth.rs"), SOURCE).unwrap();

    let server = ScriptedHttpServer::respond_with(|request| {
        if request.body.trim().is_empty() {
            return ScriptedResponse::json(400, r#"{"error":"empty request"}"#);
        }
        let content = reply(&parse_prompt(request));
        ScriptedResponse::json(
            200,
            json!({"choices":[{"message":{"content":content.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":120,"completion_tokens":60}}).to_string(),
        )
    });
    let data = root.join("data");
    let deps = AppCoreDependencies::default();
    let service = PersistenceService::initialize_with_dependencies(
        &data,
        Arc::new(Quiet),
        Arc::new(MemoryCredentialStore::default()),
        Arc::new(FixedEmbeddingGateway::default()),
        deps.clock,
        deps.jitter,
        deps.http_client,
    )
    .unwrap();
    configure_loopback(&service, &server);
    rusqlite::Connection::open(service.database_path())
        .unwrap()
        .execute(
            "UPDATE provider_connections SET credential_ref='review-brief-fixture:' || id",
            [],
        )
        .unwrap();
    service
        .update_preparation_settings(PreparationSettings {
            preparation_enabled: false,
            learning_enabled: false,
            ..PreparationSettings::default()
        })
        .unwrap();
    let project = service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: workspace.to_str().unwrap().into(),
            preferred_name: Some("Review brief fixture".into()),
        })
        .unwrap()
        .project
        .id;
    force_api_direct(&service, &project);
    service
        .update_composition_settings(
            &project,
            CompositionSettings {
                enabled: true,
                ..CompositionSettings::default()
            }
            .with_all_recipes_qualified(),
        )
        .unwrap();
    let created = TaskService::new(service.clone())
        .create(CreateTasksRequest {
            project_id: project.clone(),
            request_id: "review-brief-fixture-tasks".into(),
            tasks: tasks(),
        })
        .unwrap();
    let pulse = service
        .create_pulsation_from_tasks(CreatePulsationFromTasksRequest {
            project_id: project.clone(),
            request_id: "review-brief-fixture-dispatch".into(),
            title: "Token expiry".into(),
            tasks: created
                .iter()
                .map(|task| TaskSelection {
                    task_id: task.id.clone(),
                    expected_revision: task.revision,
                    manual_path_hint: None,
                })
                .collect(),
        })
        .unwrap()
        .id;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let stop = CancellationToken::new();
    let worker = service.clone();
    let token = stop.clone();
    runtime.spawn(async move { worker.run_job_worker_until(token).await });
    let database = rusqlite::Connection::open(service.database_path()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let parked: bool = database
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pulsations p JOIN checkpoints c ON c.pulsation_id=p.id WHERE p.id=?1 AND p.stage='SPECIFICATION' AND p.status='checkpoint' AND c.status='waiting' AND c.kind='artifact_review')",
                [&pulse],
                |row| row.get(0),
            )
            .unwrap();
        if parked {
            break;
        }
        let status: String = database
            .query_row(
                "SELECT status FROM pulsations WHERE id=?1",
                [&pulse],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            status != "failed" && Instant::now() < deadline,
            "pulsation did not reach the Specification checkpoint (status {status})"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    stop.cancel();
    runtime.shutdown_timeout(Duration::from_secs(5));

    let checkpoint = service.get_hitl_checkpoint(&pulse).unwrap();
    let brief = service
        .read_checkpoint_brief(&checkpoint)
        .unwrap()
        .expect("the Specification checkpoint has a brief");
    service
        .save_ui_session(
            serde_json::from_value(json!({
                "projectId": project,
                "workspaceRoot": workspace,
                "workspaceViewMode": "editor",
                "activeView": "pulsations",
                "activePulsationId": pulse,
            }))
            .unwrap(),
        )
        .unwrap();
    println!(
        "Brief: {} criteria, {} judgement items, {} work units",
        brief.facts.criteria,
        brief.judgement.len(),
        brief.facts.units
    );
    println!("HVC_DATA_DIR={}", data.display());
}
