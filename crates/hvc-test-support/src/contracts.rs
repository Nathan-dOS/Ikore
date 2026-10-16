use std::fs;
use std::time::Duration;

use hvc_core::llm::{
    CompressionEngine, CompressionPreviewRequest, ContentKind, DeleteProviderConnectionRequest,
    ResetLlmResilienceStateRequest, RouteTarget, TestProviderConnectionRequest,
    UpsertProviderConnectionRequest, UpsertProviderRouteBindingRequest, UpsertProviderRouteRequest,
};
use hvc_core::persistence_service::{
    EnsureProjectForWorkspaceRequest, GetLlmExecutionSnapshotRequest, PersistenceCommandError,
};
use hvc_core::workspace_service::{
    OpenWorkspaceRequest, ReadWorkspaceFileRequest, WorkspaceErrorCode,
    WriteWorkspaceFileAtomicRequest,
};

use crate::{CoreTestHarness, ScriptedHttpServer, ScriptedResponse};

#[test]
fn preserves_typed_domain_error_fields() {
    let harness = CoreTestHarness::new();
    let core = harness.core();
    let workspace = greenfield_workspace(&harness);
    core.open_workspace(OpenWorkspaceRequest {
        path: workspace.to_string_lossy().into_owned(),
    })
    .unwrap();
    let snapshot = core
        .read_workspace_file(ReadWorkspaceFileRequest {
            relative_path: "docs/PROJECT_AGENT.md".into(),
        })
        .unwrap();
    fs::write(workspace.join("docs/PROJECT_AGENT.md"), "# changed\n").unwrap();

    let error = core
        .write_workspace_file_atomic(WriteWorkspaceFileAtomicRequest {
            relative_path: "docs/PROJECT_AGENT.md".into(),
            content: "# mine\n".into(),
            expected_revision: snapshot.revision,
        })
        .unwrap_err();

    assert!(matches!(error.code, WorkspaceErrorCode::Conflict));
    assert!(error.details.is_some());
    assert!(error.remediation.is_some());
}

#[test]
fn preserves_llm_details_and_remediation() {
    let harness = CoreTestHarness::new();
    let core = harness.core();
    let connection = core
        .persistence()
        .upsert_provider_connection(connection_request("missing-key", "openai", None, None))
        .unwrap()
        .connection;

    let error = core
        .runtime_handle()
        .block_on(
            core.persistence()
                .test_provider_connection(TestProviderConnectionRequest {
                    connection_id: connection.id,
                }),
        )
        .unwrap_err();
    let command = PersistenceCommandError::from(error);

    assert!(command.details.is_some());
    assert!(command.remediation.is_some());
    assert!(!command.message.contains("sk-"));
}

#[test]
fn opens_workspace_through_typed_request() {
    let harness = CoreTestHarness::new();
    let workspace = greenfield_workspace(&harness);
    let opened = harness
        .core()
        .open_workspace(OpenWorkspaceRequest {
            path: workspace.to_string_lossy().into_owned(),
        })
        .unwrap();

    assert!(opened.root_path.ends_with("workspace\\greenfield"));
    assert!(!opened.tree.is_empty());
}

#[test]
fn never_persists_or_logs_connection_secret() {
    let harness = CoreTestHarness::new();
    let request = connection_request(
        "secret-contract",
        "openai",
        None,
        Some("sk-contract-secret".into()),
    );
    assert!(!format!("{request:?}").contains("sk-contract-secret"));

    let connection = harness
        .core()
        .persistence()
        .upsert_provider_connection(request)
        .unwrap()
        .connection;
    let response = serde_json::to_string(&connection).unwrap();

    assert!(!response.contains("sk-contract-secret"));
    assert!(harness.credentials().contains("connection:secret-contract"));
}

#[test]
fn tests_and_deletes_provider_connection() {
    let server = ScriptedHttpServer::start([ScriptedResponse::json(
        200,
        r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}]}"#,
    )]);
    let harness = CoreTestHarness::new();
    let core = harness.core();
    let connection = core
        .persistence()
        .upsert_provider_connection(connection_request(
            "connection-lifecycle",
            "ollama",
            Some(server.url("")),
            None,
        ))
        .unwrap()
        .connection;
    let tested = core
        .runtime_handle()
        .block_on(
            core.persistence()
                .test_provider_connection(TestProviderConnectionRequest {
                    connection_id: connection.id.clone(),
                }),
        )
        .unwrap();
    assert!(tested.reachable);
    assert_eq!(server.wait_for_requests(1, Duration::from_secs(1)).len(), 1);

    let deleted = core
        .persistence()
        .delete_provider_connection(DeleteProviderConnectionRequest {
            connection_id: connection.id,
            version: connection.version,
        })
        .unwrap();
    assert!(deleted.deleted);
}

#[test]
fn manages_routes_targets_and_bindings() {
    let harness = CoreTestHarness::new();
    let core = harness.core();
    let workspace = greenfield_workspace(&harness);
    core.open_workspace(OpenWorkspaceRequest {
        path: workspace.to_string_lossy().into_owned(),
    })
    .unwrap();
    let project = core
        .persistence()
        .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
            workspace_path: workspace.to_string_lossy().into_owned(),
            preferred_name: Some("Fixture".into()),
        })
        .unwrap()
        .project;
    let connection = core
        .persistence()
        .upsert_provider_connection(connection_request(
            "route-connection",
            "ollama",
            Some("http://127.0.0.1:11434".into()),
            None,
        ))
        .unwrap()
        .connection;
    let route = core
        .persistence()
        .upsert_provider_route(UpsertProviderRouteRequest {
            id: Some("contract-route".into()),
            name: "Contract route".into(),
            strategy: "priority".into(),
            max_attempts: Some(2),
            attempt_timeout_ms: Some(5_000),
            route_timeout_ms: Some(10_000),
            enabled: Some(true),
            targets: vec![RouteTarget {
                position: 0,
                connection_id: connection.id,
                model_id: "fixture-model".into(),
                enabled: true,
            }],
            version: None,
        })
        .unwrap()
        .route;
    let binding = core
        .persistence()
        .upsert_provider_route_binding(UpsertProviderRouteBindingRequest {
            project_id: project.id,
            agent_role: "task_specifier".into(),
            stage: "Specification".into(),
            route_id: route.id.clone(),
            enabled: Some(true),
            version: None,
        })
        .unwrap()
        .binding;

    assert_eq!(binding.route_id, route.id);
    assert!(
        core.persistence()
            .list_provider_routes()
            .unwrap()
            .routes
            .iter()
            .any(|item| item.id == route.id)
    );
    assert!(
        core.persistence()
            .list_provider_route_bindings()
            .unwrap()
            .bindings
            .iter()
            .any(|item| item.route_id == route.id)
    );
}

#[test]
fn loads_and_resets_resilience_snapshot() {
    let harness = CoreTestHarness::new();
    let persistence = harness.core();
    let before = persistence
        .persistence()
        .get_llm_resilience_snapshot()
        .unwrap()
        .snapshot;
    let reset = persistence
        .persistence()
        .reset_llm_resilience_state(ResetLlmResilienceStateRequest {
            connection_id: None,
            provider_id: None,
        })
        .unwrap();
    let after = persistence
        .persistence()
        .get_llm_resilience_snapshot()
        .unwrap()
        .snapshot;

    assert!(before.states.is_empty());
    assert_eq!(reset.reset_count, 0);
    assert!(after.states.is_empty());
}

#[test]
fn loads_execution_metrics_and_compression_preview() {
    let harness = CoreTestHarness::new();
    let core = harness.core();
    let metrics = core.persistence().get_llm_routing_metrics().unwrap();
    let preview = core
        .persistence()
        .preview_context_compression(CompressionPreviewRequest {
            content: "line one\nline one\nline two\n".into(),
            content_kind: ContentKind::ToolOutput,
            engine: CompressionEngine::Rtk,
            caveman_opt_in: false,
        })
        .unwrap();
    let missing = core
        .persistence()
        .get_llm_execution_snapshot(GetLlmExecutionSnapshotRequest {
            execution_id: "missing-execution".into(),
        })
        .unwrap_err();

    assert!(metrics.metrics.is_empty());
    assert_eq!(preview.preview.original_bytes, 27);
    assert!(
        PersistenceCommandError::from(missing)
            .message
            .contains("LLM execution not found")
    );
}

fn connection_request(
    id: &str,
    provider_id: &str,
    endpoint_url: Option<String>,
    api_key: Option<String>,
) -> UpsertProviderConnectionRequest {
    UpsertProviderConnectionRequest {
        id: Some(id.to_string()),
        provider_id: provider_id.to_string(),
        display_name: format!("{provider_id} fixture"),
        endpoint_url,
        default_model: "fixture-model".into(),
        enabled: Some(true),
        max_concurrency: Some(1),
        api_key,
        version: None,
    }
}

fn greenfield_workspace(harness: &CoreTestHarness) -> std::path::PathBuf {
    let path = harness.workspace_dir().join("greenfield");
    fs::create_dir_all(&path).unwrap();
    path
}
