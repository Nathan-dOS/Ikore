//! Isolated fixture provider for manual M5 GPUI smoke. No worker; the GUI owns the supervisor.
//! Start with a NEW directory under target/. Pass --resume only for a marked fixture whose
//! 45-minute server expired. Close GUI then create STOP to terminate the fixture.
use hvc_core::{
    AppCoreDependencies,
    llm::{LlmEventEnvelope, LlmEventSink},
    orchestration::PreparationSettings,
    persistence_service::*,
    workspace_service::bootstrap_greenfield_workspace,
};
use hvc_test_support::{
    FixedEmbeddingGateway, MemoryCredentialStore, ScriptedHttpServer, ScriptedResponse,
    pulsation_benchmark::{configure_loopback, stage_response},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

const FIXTURE_MARKER: &str = "pulsation-ui-fixture-v1\n";

struct Quiet;
impl LlmEventSink for Quiet {
    fn emit(&self, _: &LlmEventEnvelope) {}
}
fn response(output: Value) -> ScriptedResponse {
    ScriptedResponse::json(200,json!({"choices":[{"message":{"content":output.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":40,"completion_tokens":80}}).to_string())
}
fn create_workspace(workspace: &Path) {
    // Follow the real host's bootstrap before adding simulated project files.
    // Seeding only persistence bypasses this guard and breaks session restore.
    std::fs::create_dir_all(workspace).unwrap();
    bootstrap_greenfield_workspace(workspace).unwrap();
    std::fs::create_dir_all(workspace.join("notes")).unwrap();
    std::fs::write(
        workspace.join("auth.rs"),
        "pub fn validate_token() -> bool { false }\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("notes/conflict.md"),
        "Edição humana preservada.\n",
    )
    .unwrap();
}
fn configure_ui_loopback(service: &PersistenceService, server: &ScriptedHttpServer) {
    configure_loopback(service, server);
    // The real GUI uses the OS credential store. Never resolve the user's legacy
    // credential names, even for an unused route or a Settings status query.
    let db = rusqlite::Connection::open(service.database_path()).unwrap();
    db.execute(
        "UPDATE provider_connections SET credential_ref='m5-ui-fixture:' || id,
         enabled=(id='legacy-openai')",
        [],
    )
    .unwrap();
}
fn validate_resume_root(root: &Path) -> Result<PathBuf, String> {
    let root = root
        .canonicalize()
        .map_err(|error| format!("invalid resume root: {error}"))?;
    match std::fs::read_to_string(root.join("M5_UI_FIXTURE")) {
        Ok(marker) if marker == FIXTURE_MARKER => {}
        _ => return Err("refuse to resume an unmarked profile".into()),
    }
    let db = rusqlite::Connection::open_with_flags(
        root.join("data/hard-vibe-code.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|error| format!("invalid fixture database: {error}"))?;
    let unsafe_connections: u64 = db
        .query_row(
            "SELECT COUNT(*) FROM provider_connections
             WHERE credential_ref NOT LIKE 'm5-ui-fixture:%'
                OR (enabled=1 AND (id<>'legacy-openai' OR provider_id<>'ollama'))",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("invalid fixture connections: {error}"))?;
    let projects: u64 = db
        .query_row(
            "SELECT COUNT(*) FROM projects
             WHERE name IN ('M5 Principal','M5 Segundo')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("invalid fixture projects: {error}"))?;
    let all_projects: u64 = db
        .query_row("SELECT COUNT(*) FROM projects", [], |row| row.get(0))
        .map_err(|error| format!("invalid fixture project count: {error}"))?;
    if unsafe_connections != 0 || projects != 2 || all_projects != 2 {
        return Err("refuse to resume a profile that is not the isolated M5 fixture".into());
    }
    Ok(root)
}
fn main() {
    let mut args = std::env::args_os().skip(1);
    let requested_root = PathBuf::from(args.next().expect("fixture directory"));
    let resume = args.any(|argument| argument == "--resume");
    let root = if resume {
        validate_resume_root(&requested_root).expect("refuse unsafe fixture resume")
    } else {
        assert!(
            !requested_root.exists(),
            "refuse to reuse or overwrite a profile"
        );
        std::fs::create_dir_all(&requested_root).unwrap();
        requested_root.canonicalize().unwrap()
    };
    let server = ScriptedHttpServer::respond_with(|request| {
        let body: Value = serde_json::from_str(&request.body).unwrap();
        let user = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "user")
            .unwrap()["content"]
            .as_str()
            .unwrap();
        if let Ok(input) = serde_json::from_str::<Value>(user)
            && input["evidence"].is_array()
        {
            return response(
                json!({"schema_version":2,"observations":[],"recommendations":[],"context_assessments":[],"graph_selections":[],"limitations":["The simulated local smoke does not prove code execution or gains from a real LLM."]}),
            );
        }
        if user.contains("- stage: SPECIFICATION")
            && user.contains("M5 Perguntas")
            && !user.contains("JWT assinado")
        {
            return response(
                json!({"outputLanguage":"en","summary":"The format requires confirmation","statusUpdate":"needs_clarification","contextUsage":[],"notes":[],"artifacts":[],"componentProposals":[],"questions":[{"id":"token_format","question":"Which token format should be used?","rationale":"Use a signed JWT for the fixture."}]}),
            );
        }
        if user.contains("- stage: Code") && user.contains("M5 Conflito") {
            return response(
                json!({"outputLanguage":"en","summary":"The proposal conflicts with a human edit","statusUpdate":"advance","contextUsage":[],"notes":[],"artifacts":[{"name":"Baseline conflict","kind":"stage_note","relativePath":"notes/conflict.md","content":"This simulated proposal must not overwrite a human edit."}]}),
            );
        }
        stage_response(request)
    });
    let deps = AppCoreDependencies::default();
    let data = root.join("data");
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
    configure_ui_loopback(&service, &server);
    service
        .update_preparation_settings(PreparationSettings {
            preparation_enabled: true,
            ..Default::default()
        })
        .unwrap();
    if !resume {
        for name in ["M5 Principal", "M5 Segundo"] {
            let workspace = root.join(name);
            create_workspace(&workspace);
            let project = service
                .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                    workspace_path: workspace.to_str().unwrap().into(),
                    preferred_name: Some(name.into()),
                })
                .unwrap()
                .project;
            let pulse = service
                .create_pulsation(CreatePulsationRequest {
                    project_id: project.id.clone(),
                    feature_name: "melhore o sistema".into(),
                })
                .unwrap()
                .pulsation;
            service.save_ui_session(serde_json::from_value(json!({"projectId":project.id,"workspaceRoot":workspace,"workspaceViewMode":"editor","activeView":"pulsations","activePulsationId":pulse.id})).unwrap()).unwrap();
        }
        std::fs::write(root.join("M5_UI_FIXTURE"), FIXTURE_MARKER).unwrap();
    }
    println!("HVC_DATA_DIR={}", data.display());
    println!("MODE={}", if resume { "resume" } else { "new" });
    println!("Loopback provider ready; GUI is the sole worker. STOP file or 45 min ends fixture.");
    let deadline = Instant::now() + Duration::from_secs(45 * 60);
    while !root.join("STOP").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_fixture_reopens_through_the_real_greenfield_guard() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("M5 Projeto");
        create_workspace(&workspace);
        let reopened = bootstrap_greenfield_workspace(&workspace).unwrap();
        assert!(!reopened.initialized);
        assert_eq!(reopened.mode, "greenfield");
        assert_eq!(
            std::fs::read_to_string(workspace.join("notes/conflict.md")).unwrap(),
            "Edição humana preservada.\n"
        );
    }

    #[test]
    fn ui_fixture_disables_remote_connections_and_uses_isolated_credential_names() {
        let root = tempfile::tempdir().unwrap();
        let deps = AppCoreDependencies::default();
        let service = PersistenceService::initialize_with_dependencies(
            root.path(),
            Arc::new(Quiet),
            Arc::new(MemoryCredentialStore::default()),
            Arc::new(FixedEmbeddingGateway::default()),
            deps.clock,
            deps.jitter,
            deps.http_client,
        )
        .unwrap();
        let server = ScriptedHttpServer::respond_with(stage_response);
        configure_ui_loopback(&service, &server);
        let db = rusqlite::Connection::open(service.database_path()).unwrap();
        let unsafe_connections: u64 = db.query_row(
            "SELECT COUNT(*) FROM provider_connections
             WHERE credential_ref NOT LIKE 'm5-ui-fixture:%'
                OR (enabled=1 AND (id<>'legacy-openai' OR provider_id<>'ollama' OR endpoint_url<>?1))",
            [server.url("")],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(unsafe_connections, 0);
        assert_eq!(
            db.query_row(
                "SELECT COUNT(*) FROM provider_connections WHERE enabled=1",
                [],
                |row| row.get::<_, u64>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn resume_requires_the_exact_marker_connections_and_two_fixture_projects() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("fixture");
        let data = root.join("data");
        std::fs::create_dir_all(&data).unwrap();
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
        let server = ScriptedHttpServer::respond_with(stage_response);
        configure_ui_loopback(&service, &server);
        assert!(validate_resume_root(&root).is_err());
        for name in ["M5 Principal", "M5 Segundo"] {
            let workspace = root.join(name);
            create_workspace(&workspace);
            service
                .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                    workspace_path: workspace.to_str().unwrap().into(),
                    preferred_name: Some(name.into()),
                })
                .unwrap();
        }
        std::fs::write(root.join("M5_UI_FIXTURE"), FIXTURE_MARKER).unwrap();
        assert_eq!(
            validate_resume_root(&root).unwrap(),
            root.canonicalize().unwrap()
        );
    }
}
