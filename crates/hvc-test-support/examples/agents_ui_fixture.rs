//! Deterministic, isolated profile for visual QA of the Agents CLI/API panel.
//! Usage: `cargo run -p hvc-test-support --example agents_ui_fixture -- <new-root>`.

use std::path::{Path, PathBuf};

use hvc_core::llm::AgentRole;
use hvc_core::persistence_service::{
    EnsureProjectForWorkspaceRequest, PersistenceService, SaveUiSessionRequest,
};
use hvc_core::terminals::{
    AgentExecutionMode, CliBackend, CliModelSelectionSource, CreateFreeFormSlotRequest,
    CreateSessionRunRequest, CreateTerminalRunRequest, ExecutionOrigin, SessionPolicy, SessionSlot,
    TerminalEventKind, TerminalRepository, TerminalRunStatus, ThinkingLevel,
    UpdateCliAgentConfigRequest, UpdateFreeFormSlotRequest,
};
use hvc_core::workspace_service::bootstrap_greenfield_workspace;

const FIXTURE_MARKER: &str = "agents-ui-fixture-v1\n";

fn main() {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .expect("informe um diretório novo para o fixture"),
    );
    assert!(
        !root.exists(),
        "o fixture se recusa a reutilizar um diretório"
    );
    seed_profile(&root);
    println!("HVC_DATA_DIR={}", root.join("data").display());
    println!("WORKSPACE={}", root.join("workspace").display());
}

fn seed_profile(root: &Path) {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    bootstrap_greenfield_workspace(&workspace).unwrap();
    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(
        workspace.join("src/lib.rs"),
        "pub fn agents_visual_fixture() -> &'static str { \"ready\" }\n",
    )
    .unwrap();
    let workspace = workspace.canonicalize().unwrap();

    let data = root.join("data");
    let service = PersistenceService::initialize(&data).unwrap();
    let project = service
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: workspace.to_string_lossy().into_owned(),
            preferred_name: Some("CLI Visual QA".into()),
        })
        .unwrap()
        .project;
    let repository = TerminalRepository::new(service.database_path());
    repository
        .ensure_project_configs(&project.id, false)
        .unwrap();

    let configurations = [
        (
            AgentRole::IntakeRouter,
            AgentExecutionMode::CliProcess,
            CliBackend::Codex,
        ),
        (
            AgentRole::TaskSpecifier,
            AgentExecutionMode::CliProcess,
            CliBackend::Claude,
        ),
        (
            AgentRole::ContractAgent,
            AgentExecutionMode::CliProcess,
            CliBackend::Gemini,
        ),
        (
            AgentRole::BackendAgent,
            AgentExecutionMode::CliProcess,
            CliBackend::Opencode,
        ),
        (
            AgentRole::TestAgent,
            AgentExecutionMode::ApiDirect,
            CliBackend::Custom,
        ),
        (
            AgentRole::Integrator,
            AgentExecutionMode::ApiDirect,
            CliBackend::Codex,
        ),
        (
            AgentRole::LearningAgent,
            AgentExecutionMode::ApiDirect,
            CliBackend::Claude,
        ),
    ];
    for (role, mode, backend) in configurations {
        let current = repository.get_config(&project.id, role).unwrap();
        repository
            .update_config(UpdateCliAgentConfigRequest {
                project_id: project.id.clone(),
                agent_role: role,
                execution_mode: mode,
                backend,
                model: Some(format!("fixture-{}", backend.as_str())),
                // Manual fixture models have no verified catalog profile.
                model_profile: None,
                model_source: CliModelSelectionSource::Manual,
                thinking_level: ThinkingLevel::Default,
                custom_executable: (backend == CliBackend::Custom).then(|| {
                    std::env::current_exe()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                }),
                custom_args: Vec::new(),
                custom_risk_ack: false,
                expected_version: current.version,
            })
            .unwrap();
    }

    for (role, status) in [
        (AgentRole::IntakeRouter, TerminalRunStatus::Completed),
        (AgentRole::TaskSpecifier, TerminalRunStatus::Failed),
        (AgentRole::ContractAgent, TerminalRunStatus::Cancelled),
        (AgentRole::BackendAgent, TerminalRunStatus::Running),
        (AgentRole::Integrator, TerminalRunStatus::AwaitingFallback),
        (AgentRole::LearningAgent, TerminalRunStatus::Completed),
    ] {
        seed_canonical_run(&repository, &project.id, role, status);
    }

    let resumable = repository
        .create_free_form_slot(CreateFreeFormSlotRequest {
            project_id: project.id.clone(),
            label: "Revisão arquitetural retomável".into(),
        })
        .unwrap();
    let resumable = repository
        .update_free_form_slot(UpdateFreeFormSlotRequest {
            id: resumable.id.clone(),
            project_id: project.id.clone(),
            label: resumable.label,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: CliBackend::Codex,
            model: Some("fixture-codex".into()),
            model_profile: None,
            model_source: CliModelSelectionSource::Manual,
            thinking_level: ThinkingLevel::Default,
            session_policy: SessionPolicy::ResumeExplicit,
            custom_executable: None,
            custom_args: Vec::new(),
            custom_risk_ack: false,
            position: resumable.position,
            archived: false,
            expected_version: resumable.version,
        })
        .unwrap();
    seed_free_form_run(&repository, &project.id, &resumable.id);

    let custom = repository
        .create_free_form_slot(CreateFreeFormSlotRequest {
            project_id: project.id.clone(),
            label: "Custom aguardando confirmação de risco".into(),
        })
        .unwrap();
    repository
        .update_free_form_slot(UpdateFreeFormSlotRequest {
            id: custom.id.clone(),
            project_id: project.id.clone(),
            label: custom.label,
            execution_mode: AgentExecutionMode::ApiDirect,
            backend: CliBackend::Custom,
            model: None,
            model_profile: None,
            model_source: CliModelSelectionSource::Default,
            thinking_level: ThinkingLevel::Default,
            session_policy: SessionPolicy::FreshPerRun,
            custom_executable: Some(
                std::env::current_exe()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ),
            custom_args: vec!["--visual-fixture".into()],
            custom_risk_ack: false,
            position: custom.position,
            archived: false,
            expected_version: custom.version,
        })
        .unwrap();

    let archived = repository
        .create_free_form_slot(CreateFreeFormSlotRequest {
            project_id: project.id.clone(),
            label: "Experimento arquivado".into(),
        })
        .unwrap();
    repository
        .update_free_form_slot(UpdateFreeFormSlotRequest {
            id: archived.id.clone(),
            project_id: project.id.clone(),
            label: archived.label,
            execution_mode: archived.execution_mode,
            backend: archived.backend,
            model: archived.model,
            model_profile: archived.model_profile,
            model_source: archived.model_source,
            thinking_level: archived.thinking_level,
            session_policy: archived.session_policy,
            custom_executable: archived.custom_executable,
            custom_args: archived.custom_args,
            custom_risk_ack: archived.custom_risk_ack,
            position: archived.position,
            archived: true,
            expected_version: archived.version,
        })
        .unwrap();

    // A completed, pathless artifact exercises the native detail tabs without scheduling work.
    let connection = rusqlite::Connection::open(service.database_path()).unwrap();
    connection.execute(
        "INSERT INTO pulsations(id, project_id, feature_name, stage, status, created_at, updated_at, queue_sequence)
         VALUES ('tabs-layout-pulse', ?1, 'Review tab navigation and artifact layout', 'Integrator', 'done', '2026-09-05T00:00:00Z', '2026-09-05T00:00:00Z', 1)",
        [&project.id],
    ).unwrap();
    connection.execute(
        "INSERT INTO artifacts(id, pulsation_id, name, kind, content, acceptance, created_at, updated_at)
         VALUES ('tabs-layout-artifact', 'tabs-layout-pulse', 'Layout verification', 'markdown', ?1, 'accepted', '2026-09-05T00:00:00Z', '2026-09-05T00:00:00Z')",
        ["# Layout verification\n\nThis completed fixture exercises the Overview, Preparation, and Artifacts sections.\n\nNo runtime job or external provider request is required.\n"],
    ).unwrap();

    service
        .save_ui_session(SaveUiSessionRequest {
            project_id: project.id,
            workspace_root: workspace.to_string_lossy().into_owned(),
            active_tab_path: None,
            workspace_view_mode: "editor".into(),
            active_view: "agents".into(),
            sidebar_collapsed: false,
            context_drawer_open: false,
            active_pulsation_id: None,
            selected_vault_path: None,
            active_chat_conversation_id: None,
            scroll_top: Some(0.0),
            tabs: Vec::new(),
            auto_open_review: false,
        })
        .unwrap();
    std::fs::write(root.join("AGENTS_UI_FIXTURE"), FIXTURE_MARKER).unwrap();
}

fn seed_canonical_run(
    repository: &TerminalRepository,
    project_id: &str,
    role: AgentRole,
    status: TerminalRunStatus,
) {
    let config = repository.get_config(project_id, role).unwrap();
    let run = repository
        .create_run(CreateTerminalRunRequest {
            project_id: project_id.into(),
            agent_role: role,
            origin: ExecutionOrigin::Manual,
            execution_mode: config.execution_mode,
            backend: Some(config.backend),
            model: config.model,
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
        })
        .unwrap();
    repository
        .set_adapter_metadata(&run.id, Some("1.0.0-fixture"))
        .unwrap();
    repository
        .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
        .unwrap();
    repository
        .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
        .unwrap();
    for (sequence, kind, text) in [
        (
            1,
            TerminalEventKind::System,
            "Execução iniciada no snapshot seguro.",
        ),
        (2, TerminalEventKind::ToolUse, "read_file src/lib.rs"),
        (
            3,
            TerminalEventKind::ToolResult,
            "Arquivo lido com sucesso.",
        ),
        (
            4,
            TerminalEventKind::Stdout,
            "Analisando contratos e dependências…",
        ),
        (
            5,
            TerminalEventKind::Stderr,
            "Authorization: Bearer visual-fixture-secret",
        ),
    ] {
        let stream = match kind {
            TerminalEventKind::Stdout => Some("stdout"),
            TerminalEventKind::Stderr => Some("stderr"),
            _ => None,
        };
        repository
            .append_event(&run.id, sequence, kind, stream, text)
            .unwrap();
    }
    if status != TerminalRunStatus::Running {
        let (exit, code, message) = match status {
            TerminalRunStatus::Completed => (Some(0), None, None),
            TerminalRunStatus::Failed => (
                Some(1),
                Some("fixture_failure"),
                Some(
                    "Falha longa controlada para validar wrapping, diagnóstico e ações sem overflow visual.",
                ),
            ),
            TerminalRunStatus::Cancelled => {
                (None, Some("user_cancel"), Some("Cancelado pelo usuário."))
            }
            TerminalRunStatus::AwaitingFallback => (
                Some(41),
                Some("cli_auth_required"),
                Some("Autenticação necessária; a API só pode ser usada após decisão explícita."),
            ),
            _ => unreachable!(),
        };
        repository
            .set_run_status(&run.id, status, exit, code, message)
            .unwrap();
    }
}

fn seed_free_form_run(repository: &TerminalRepository, project_id: &str, slot_id: &str) {
    let run = repository
        .create_session_run(CreateSessionRunRequest {
            project_id: project_id.into(),
            slot: SessionSlot::FreeForm { id: slot_id.into() },
            origin: ExecutionOrigin::Manual,
            execution_mode: AgentExecutionMode::CliProcess,
            backend: Some(CliBackend::Codex),
            model: Some("fixture-codex".into()),
            pulsation_id: None,
            job_id: None,
            agent_run_id: None,
            llm_execution_id: None,
            parent_run_id: None,
            continuation_reason: None,
        })
        .unwrap();
    repository
        .set_workspace_snapshot_hash(&run.id, "fixture-workspace-hash")
        .unwrap();
    repository
        .set_session_fingerprint(&run.id, "fixture-session-fingerprint")
        .unwrap();
    repository
        .set_native_session_id(&run.id, "fixture-native-session")
        .unwrap();
    repository
        .set_adapter_metadata(&run.id, Some("1.0.0-fixture"))
        .unwrap();
    repository
        .set_run_status(&run.id, TerminalRunStatus::Starting, None, None, None)
        .unwrap();
    repository
        .set_run_status(&run.id, TerminalRunStatus::Running, None, None, None)
        .unwrap();
    for sequence in 1..=80 {
        repository
            .append_event(
                &run.id,
                sequence,
                TerminalEventKind::Stdout,
                Some("stdout"),
                &format!("Linha determinística {sequence:02} — Unicode: ação, revisão, ✓"),
            )
            .unwrap();
    }
    repository
        .set_run_status(&run.id, TerminalRunStatus::Completed, Some(0), None, None)
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_is_isolated_redacted_and_contains_active_and_archived_free_form_slots() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("fixture");
        seed_profile(&root);
        let database = root.join("data/hard-vibe-code.db");
        let connection = rusqlite::Connection::open(database).unwrap();
        let leaked: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM terminal_events WHERE content LIKE '%visual-fixture-secret%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let active: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM terminal_slots WHERE kind='free_form' AND archived=0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let archived: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM terminal_slots WHERE kind='free_form' AND archived=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(leaked, 0);
        assert_eq!((active, archived), (2, 1));
        let jobs: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE pulsation_id='tabs-layout-pulse'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(jobs, 0);
        let project_id: String = connection
            .query_row(
                "SELECT project_id FROM pulsations WHERE id='tabs-layout-pulse'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let service = PersistenceService::initialize(&root.join("data")).unwrap();
        let snapshot = service
            .get_pulsation_runtime_snapshot(
                hvc_core::persistence_service::GetPulsationRuntimeSnapshotRequest { project_id },
            )
            .unwrap()
            .snapshot;
        assert_eq!(snapshot.pulsations.len(), 1);
        assert_eq!(snapshot.details.len(), 1);
        assert_eq!(snapshot.main_agents_count, 0);
        assert_eq!(snapshot.background_agents_count, 0);
        assert_eq!(
            std::fs::read_to_string(root.join("AGENTS_UI_FIXTURE")).unwrap(),
            FIXTURE_MARKER
        );
    }
}
