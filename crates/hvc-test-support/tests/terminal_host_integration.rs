//! Host integration retained when the terminal runtime moved to its own crate.
#![allow(
    clippy::unwrap_used,
    reason = "Integration fixtures fail immediately when isolated setup is invalid"
)]

use std::time::Duration;

use hvc_core::llm::AgentRole;
use hvc_core::persistence_service::{CreateProjectRequest, PersistenceService};
use hvc_core::terminals::{
    AgentExecutionMode, CliAvailability, CliAvailabilityState, CliBackend, CliCapabilities,
    CliModelCatalogService, CliModelCatalogState, CliModelSelectionSource, CliOutputFormat,
    CreateTerminalRunRequest, ExecutionOrigin, TerminalEventEnvelope, TerminalEventSink,
    TerminalRepository, TerminalRunStatus, ThinkingLevel, UpdateCliAgentConfigRequest,
};
use rusqlite::{Connection, params};

struct NoEvents;
impl TerminalEventSink for NoEvents {
    fn emit(&self, _: &TerminalEventEnvelope) {}
}

/// Seed a fixed CLI reply through the real migrated schema; no private catalog API is exposed.
fn antigravity_catalog_project() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    PersistenceService,
    String,
) {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = PersistenceService::initialize(data.path()).unwrap();
    let project = persistence
        .create_project(CreateProjectRequest {
            workspace_path: workspace.path().to_string_lossy().into_owned(),
            name: Some("Antigravity effort".into()),
        })
        .unwrap()
        .project;
    let mut db = Connection::open(persistence.database_path()).unwrap();
    db.pragma_update(None, "foreign_keys", "ON").unwrap();
    let transaction = db.transaction().unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    transaction
        .execute(
            "INSERT INTO cli_model_catalog_snapshots(
            id,project_id,backend,adapter_contract_version,state,source,cli_version,
            refreshed_at,created_at
        ) VALUES('fixture-antigravity-catalog',?1,'antigravity',2,'ready','cli','1.3.5',?2,?2)",
            params![project.id, now],
        )
        .unwrap();
    for (position, provider, model, label, profiles) in [
        (
            0,
            "google",
            "gemini-3.8-flash",
            "Flash",
            vec!["low", "high"],
        ),
        (1, "openai", "gpt-oss-120b", "GPT-OSS 120B", vec!["medium"]),
        (2, "google", "unsuffixed-model", "Unsuffixed", vec![]),
    ] {
        transaction
            .execute(
                "INSERT INTO cli_model_catalog_entries(
                snapshot_id,provider_id,model_id,display_name,profiles_json,position
            ) VALUES('fixture-antigravity-catalog',?1,?2,?3,?4,?5)",
                params![
                    provider,
                    model,
                    label,
                    serde_json::to_string(&profiles).unwrap(),
                    position
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    let cached = CliModelCatalogService::new(persistence.database_path())
        .cached_catalog(&project.id, CliBackend::Antigravity)
        .unwrap()
        .unwrap();
    assert_eq!(cached.state, CliModelCatalogState::Ready);
    assert_eq!(cached.adapter_contract_version, 2);
    assert_eq!(cached.models.len(), 3);
    assert!(
        cached
            .models
            .iter()
            .any(|entry| entry.model_id.as_deref() == Some("gpt-oss-120b")
                && entry.profiles == ["medium"])
    );
    (data, workspace, persistence, project.id)
}

#[test]
fn interrupted_cli_usage_remains_partial_after_history_deletion() {
    use hvc_core::persistence_service::{CreateProjectRequest, PersistenceService};
    use hvc_core::usage::{UsageContext, UsageTransport, cli::CliUsageRecorder};

    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = PersistenceService::initialize(data.path()).unwrap();
    let project = persistence
        .create_project(CreateProjectRequest {
            workspace_path: workspace.path().to_string_lossy().into_owned(),
            name: Some("Usage coverage".into()),
        })
        .unwrap()
        .project;
    let repository = TerminalRepository::new(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, true)
        .unwrap();
    for cancelled in [true, false] {
        let run = repository
            .create_run(CreateTerminalRunRequest {
                project_id: project.id.clone(),
                agent_role: AgentRole::BackendAgent,
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(CliBackend::Codex),
                model: None,
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
            })
            .unwrap();
        repository
            .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
            .unwrap();
        repository
            .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
            .unwrap();
        let usage = CliUsageRecorder::new(
            persistence.database_path(),
            UsageContext {
                source_kind: "terminal_run".into(),
                source_id: run.id.clone(),
                pulsation_id: run.pulsation_id.clone(),
                project_id: Some(project.id.clone()),
                transport: UsageTransport::Cli,
                provider_id: "codex".into(),
                requested_model: None,
                started_at: run.created_at.clone(),
            },
            CliBackend::Codex,
        )
        .unwrap();
        usage
            .observe_line(
                br#"{"type":"turn.completed","usage":{"input_tokens":7,"output_tokens":3}}"#,
            )
            .unwrap();
        if cancelled {
            repository
                .set_run_status(
                    &run.id,
                    TerminalRunStatus::Cancelled,
                    None,
                    Some("user_cancelled"),
                    None,
                )
                .unwrap();
        } else {
            assert_eq!(repository.recover_interrupted_runs().unwrap(), 1);
        }
    }
    repository
        .clear_history(&project.id, Some(AgentRole::BackendAgent))
        .unwrap();
    let totals = hvc_core::usage::query_usage(persistence.database_path(), &Default::default())
        .unwrap()
        .totals;
    assert_eq!(totals.total_tokens, Some(20));
    assert_eq!(totals.partial_records, 2);
}

#[tokio::test]
async fn antigravity_effort_the_model_lacks_is_refused_on_save_and_before_launch() {
    use hvc_core::llm::{
        AgentExecutionTarget, AgentRole, DispatchPolicy, LlmRequest, ResponseFormat, UnitDispatch,
    };
    use hvc_core::orchestration::composition::ExecutorKind;
    use hvc_core::terminals::{
        AgentExecutionMode, SpecialtyOverride, TerminalService, UpdateCliAgentConfigRequest,
    };

    let (_data, _workspace, persistence, project) = antigravity_catalog_project();
    let terminals = std::sync::Arc::new(
        TerminalService::new(
            persistence.database_path(),
            tokio::runtime::Handle::current(),
            std::sync::Arc::new(NoEvents),
        )
        .unwrap(),
    );
    terminals
        .repository()
        .ensure_project_configs(&project, false)
        .unwrap();
    let kind = ExecutorKind::CodeImplementer;
    let role = AgentRole::for_stage(kind.stage().to_pulsation().as_db_value());
    let save_role = |model: &str, level: ThinkingLevel| {
        let current = terminals.repository().get_config(&project, role).unwrap();
        terminals.update_config(UpdateCliAgentConfigRequest {
            project_id: project.clone(),
            agent_role: role,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Antigravity,
            model: Some(model.into()),
            model_profile: None,
            model_source: CliModelSelectionSource::Catalog,
            thinking_level: level,
            custom_executable: None,
            custom_args: Vec::new(),
            custom_risk_ack: false,
            expected_version: current.version,
        })
    };
    let specialty = |level| SpecialtyOverride {
        execution_mode: AgentExecutionMode::CliProcess,
        backend: Some(CliBackend::Antigravity),
        model: None,
        thinking_level: level,
    };

    let refused = save_role("gpt-oss-120b", ThinkingLevel::Low).unwrap_err();
    assert_eq!(refused.code(), "cli_configuration_invalid");
    save_role("gemini-3.8-flash", ThinkingLevel::Default).unwrap();
    persistence
        .upsert_specialty_override(&project, kind, specialty(ThinkingLevel::Low))
        .unwrap();
    save_role("gpt-oss-120b", ThinkingLevel::Default).unwrap();
    let refused = persistence
        .upsert_specialty_override(&project, kind, specialty(ThinkingLevel::High))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("offers only medium"), "{refused}");

    // The `low` specialty saved while the role was on Gemini still exists.
    let request = LlmRequest {
        execution_id: "effort-execution".to_string().into(),
        generation_id: "effort-generation".to_string().into(),
        project_id: project.clone(),
        pulsation_id: None,
        job_id: None,
        agent_run_id: None,
        job_chain: "effort".into(),
        target: AgentExecutionTarget::canonical(role),
        stage: "Code".into(),
        system_prompt: "Read-only".into(),
        user_prompt: "unused".into(),
        response_format: ResponseFormat::Text,
        max_output_tokens: 100,
        temperature: 0.0,
        deadline: std::time::Instant::now() + Duration::from_secs(30),
        stream: true,
        legacy_provider: "fixture".into(),
        legacy_model: "fixture".into(),
        dispatch: DispatchPolicy {
            unit: Some(UnitDispatch {
                stage_execution_id: "execution".into(),
                unit_id: "unit".into(),
                attempt_id: "attempt".into(),
                executor_kind: kind.as_str().into(),
                frozen_transport: "cli".into(),
                frozen_backend: Some("antigravity".into()),
                frozen_model: "gpt-oss-120b".into(),
            }),
            ..DispatchPolicy::default()
        },
    };
    let error = terminals
        .execute_llm_request(&request, &tokio_util::sync::CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "cli_configuration_invalid");
    let message = error.to_string();
    assert!(
        message.contains("thinking level low is not available"),
        "{message}"
    );
    let runs: i64 = Connection::open(persistence.database_path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM terminal_runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(runs, 0, "nothing was spawned or recorded");
}

#[test]
fn stage_dispatch_keeps_preflight_blockers_when_the_recheck_still_requires_login_core_preflight() {
    let data = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let persistence = PersistenceService::initialize(data.path()).unwrap();
    let project = persistence
        .create_project(CreateProjectRequest {
            workspace_path: workspace.path().to_string_lossy().into_owned(),
            name: Some("CLI recovery preflight".into()),
        })
        .unwrap()
        .project;
    let repository = TerminalRepository::new(persistence.database_path());
    repository
        .ensure_project_configs(&project.id, false)
        .unwrap();
    for (_, _, role) in hvc_core::orchestration::catalog::STAGES {
        let current = repository.get_config(&project.id, role).unwrap();
        repository
            .update_config(UpdateCliAgentConfigRequest {
                project_id: project.id.clone(),
                agent_role: role,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: CliBackend::Claude,
                model: None,
                model_profile: None,
                model_source: CliModelSelectionSource::Default,
                thinking_level: ThinkingLevel::Default,
                custom_executable: None,
                custom_args: Vec::new(),
                custom_risk_ack: false,
                expected_version: current.version,
            })
            .unwrap();
    }
    let discovery = vec![CliAvailability {
        backend: CliBackend::Claude,
        state: CliAvailabilityState::AuthRequired,
        executable_path: Some("fixture-claude.exe".into()),
        version: Some("fixture".into()),
        diagnostic: Some("not-signed-in".into()),
        install_url: "https://example.test/cli",
        install_command: "fixture-install",
        capabilities: CliCapabilities {
            output_format: CliOutputFormat::JsonLines,
            structured_output: true,
            native_resume: false,
            thinking_levels: Vec::new(),
            auth_probe: true,
            read_only: true,
            model_catalog: true,
        },
        quota: Vec::new(),
    }];
    let db = Connection::open(persistence.database_path()).unwrap();
    let llm = hvc_core::llm::SqliteLlmRepository::new(persistence.database_path());
    let issues = hvc_core::orchestration::stage_preflight::check_stages(
        &db,
        &llm,
        &project.id,
        Some((&discovery, true)),
    )
    .unwrap();
    assert_eq!(issues.len(), hvc_core::orchestration::catalog::STAGES.len());
    assert!(
        issues
            .iter()
            .all(|issue| issue.blocking && issue.message.contains("needs a login"))
    );
}
