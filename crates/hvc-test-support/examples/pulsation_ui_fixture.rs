//! Isolated fixture provider for manual M5 GPUI smoke. No worker; the GUI owns the supervisor.
//! Start with a NEW directory under target/. Close GUI then create STOP to terminate the fixture.
use hvc_core::{
    AppCoreDependencies,
    llm::{LlmEventEnvelope, LlmEventSink},
    orchestration::PreparationSettings,
    persistence_service::*,
};
use hvc_test_support::{
    FixedEmbeddingGateway, MemoryCredentialStore, ScriptedHttpServer, ScriptedResponse,
    pulsation_benchmark::{configure_loopback, stage_response},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

struct Quiet;
impl LlmEventSink for Quiet {
    fn emit(&self, _: &LlmEventEnvelope) {}
}
fn response(output: Value) -> ScriptedResponse {
    ScriptedResponse::json(200,json!({"choices":[{"message":{"content":output.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":40,"completion_tokens":80}}).to_string())
}
fn main() {
    let root = PathBuf::from(std::env::args_os().nth(1).expect("new fixture directory"));
    assert!(!root.exists(), "refuse to reuse or overwrite a profile");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
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
                json!({"schema_version":1,"observations":[],"recommendations":[],"limitations":["Smoke local simulado; não comprova execução de código ou ganho com LLM real."]}),
            );
        }
        if user.contains("- stage: SPECIFICATION")
            && user.contains("M5 Perguntas")
            && !user.contains("JWT assinado")
        {
            return response(
                json!({"summary":"Formato precisa de confirmação","statusUpdate":"needs_clarification","notes":[],"artifacts":[],"questions":[{"id":"token_format","question":"Qual formato de token usar?","rationale":"Responda JWT assinado para a fixture."}]}),
            );
        }
        if user.contains("- stage: Code") && user.contains("M5 Conflito") {
            return response(
                json!({"summary":"Proposta colide com edição humana","statusUpdate":"advance","notes":[],"artifacts":[{"name":"Conflito de base","kind":"stage_note","relativePath":"notes/conflict.md","content":"Proposta simulada que não deve sobrescrever edição humana."}]}),
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
    configure_loopback(&service, &server);
    service
        .update_preparation_settings(PreparationSettings {
            preparation_enabled: true,
            ..Default::default()
        })
        .unwrap();
    for name in ["M5 Principal", "M5 Segundo"] {
        let workspace = root.join(name);
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
    println!("HVC_DATA_DIR={}", data.display());
    println!("Loopback provider ready; GUI is the sole worker. STOP file or 45 min ends fixture.");
    let deadline = Instant::now() + Duration::from_secs(45 * 60);
    while !root.join("STOP").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
}
