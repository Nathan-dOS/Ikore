#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hard_vibe_code::{llm::TauriEventSink, persistence_service, workspace_service};

use hard_vibe_code::llm::{
    CompressionPreviewRequest, DeleteProviderConnectionRequest, DeleteProviderRouteRequest,
    ResetLlmResilienceStateRequest, TestProviderConnectionRequest, TestProviderConnectionResponse,
    UpsertProviderConnectionRequest, UpsertProviderRouteBindingRequest, UpsertProviderRouteRequest,
};
use persistence_service::{
    ApplyHitlDecisionRequest, ApplyHitlDecisionResponse, ArchiveProjectRequest,
    ArchiveProjectResponse, CancelPulsationJobsRequest, CancelPulsationJobsResponse,
    ClearProviderApiKeyRequest, ClearProviderApiKeyResponse, CreateProjectRequest,
    CreateProjectResponse, CreatePulsationRequest, CreatePulsationResponse,
    DeleteProviderConnectionResponse, DeleteProviderRouteResponse,
    EnsureProjectForWorkspaceRequest, EnsureProjectForWorkspaceResponse,
    GetAgentRuntimeConfigResponse, GetLastActiveProjectResponse, GetLlmExecutionSnapshotRequest,
    GetLlmExecutionSnapshotResponse, GetLlmResilienceSnapshotResponse,
    GetLlmRoutingMetricsResponse, GetProviderKeyStatusesResponse,
    GetPulsationRuntimeSnapshotRequest, GetPulsationRuntimeSnapshotResponse, ListAgentRunsRequest,
    ListAgentRunsResponse, ListProjectsRequest, ListProjectsResponse,
    ListProviderConnectionsResponse, ListProviderProfilesResponse,
    ListProviderRouteBindingsResponse, ListProviderRoutesResponse, ListPulsationArtifactsRequest,
    ListPulsationArtifactsResponse, ListPulsationsRequest, ListPulsationsResponse,
    LoadUiSessionRequest, LoadUiSessionResponse, OpenProjectRequest, OpenProjectResponse,
    PersistenceCommandError, PersistenceService, PreviewContextCompressionResponse,
    ReindexWorkspaceEmbeddingsRequest, ReindexWorkspaceEmbeddingsResponse,
    ResetLlmResilienceStateResponse, SaveProviderApiKeyRequest, SaveProviderApiKeyResponse,
    SaveUiSessionRequest, SaveUiSessionResponse, UpdateAgentRuntimeConfigRequest,
    UpdateAgentRuntimeConfigResponse, UpsertProviderConnectionResponse,
    UpsertProviderRouteBindingResponse, UpsertProviderRouteResponse,
};
use tauri::{Manager, State};
use workspace_service::{
    BootstrapGreenfieldWorkspaceRequest, BootstrapGreenfieldWorkspaceResponse,
    GetContextCandidatesRequest, GetContextCandidatesResponse, GetWorkspaceVaultIndexRequest,
    GetWorkspaceVaultIndexResponse, OpenWorkspaceRequest, OpenWorkspaceResponse,
    ReadWorkspaceFileRequest, ReadWorkspaceFileResponse, RenameWorkspaceNoteRequest,
    RenameWorkspaceNoteResponse, SearchWorkspaceTextRequest, SearchWorkspaceTextResponse,
    VaultIndexCache, WorkspaceCommandError, WorkspaceServiceError, WorkspaceTreeResponse,
    WorkspaceWatcher, WriteWorkspaceFileAtomicRequest, WriteWorkspaceFileAtomicResponse,
};

struct AppState {
    workspace: Mutex<WorkspaceContext>,
    persistence: PersistenceService,
}

#[derive(Default)]
struct WorkspaceContext {
    root_path: Option<PathBuf>,
    watcher: Option<WorkspaceWatcher>,
    vault_index_cache: VaultIndexCache,
}

impl AppState {
    fn new(persistence: PersistenceService) -> Self {
        Self {
            workspace: Mutex::new(WorkspaceContext::default()),
            persistence,
        }
    }
}

#[tauri::command]
fn open_workspace(
    request: OpenWorkspaceRequest,
    state: State<AppState>,
    app_handle: tauri::AppHandle,
) -> Result<OpenWorkspaceResponse, WorkspaceCommandError> {
    let root_path = workspace_service::validate_workspace_root(&request.path)
        .map_err(WorkspaceCommandError::from)?;
    let greenfield = workspace_service::bootstrap_greenfield_workspace(&root_path)
        .map_err(WorkspaceCommandError::from)?;
    let tree = workspace_service::build_workspace_tree(&root_path)
        .map_err(WorkspaceCommandError::from)?
        .tree;
    let watcher = WorkspaceWatcher::start(root_path.clone(), app_handle)
        .map_err(WorkspaceCommandError::from)?;

    let mut workspace = state.workspace.lock().map_err(|_| WorkspaceCommandError {
        code: workspace_service::WorkspaceErrorCode::IoError,
        message: "Falha ao acessar o estado interno do workspace.".to_string(),
        details: None,
        remediation: None,
    })?;
    workspace.root_path = Some(root_path.clone());
    workspace.watcher = Some(watcher);
    workspace.vault_index_cache = VaultIndexCache::default();

    Ok(OpenWorkspaceResponse {
        root_path: root_path.to_string_lossy().to_string(),
        tree,
        greenfield,
    })
}

#[tauri::command]
fn bootstrap_greenfield_workspace(
    request: BootstrapGreenfieldWorkspaceRequest,
) -> Result<BootstrapGreenfieldWorkspaceResponse, WorkspaceCommandError> {
    workspace_service::bootstrap_greenfield_workspace_by_path(request)
        .map_err(WorkspaceCommandError::from)
}

#[tauri::command]
fn list_workspace_tree(
    state: State<AppState>,
) -> Result<WorkspaceTreeResponse, WorkspaceCommandError> {
    let root_path = get_workspace_root(&state)?;
    workspace_service::build_workspace_tree(&root_path).map_err(WorkspaceCommandError::from)
}

#[tauri::command]
fn read_workspace_file(
    request: ReadWorkspaceFileRequest,
    state: State<AppState>,
) -> Result<ReadWorkspaceFileResponse, WorkspaceCommandError> {
    let root_path = get_workspace_root(&state)?;
    workspace_service::read_workspace_file(&root_path, request).map_err(WorkspaceCommandError::from)
}

#[tauri::command]
fn write_workspace_file_atomic(
    request: WriteWorkspaceFileAtomicRequest,
    state: State<AppState>,
) -> Result<WriteWorkspaceFileAtomicResponse, WorkspaceCommandError> {
    let root_path = get_workspace_root(&state)?;
    workspace_service::write_workspace_file_atomic(&root_path, request)
        .map_err(WorkspaceCommandError::from)
}

#[tauri::command]
fn search_workspace_text(
    request: SearchWorkspaceTextRequest,
    state: State<AppState>,
) -> Result<SearchWorkspaceTextResponse, WorkspaceCommandError> {
    let root_path = get_workspace_root(&state)?;
    workspace_service::search_workspace_text(&root_path, request)
        .map_err(WorkspaceCommandError::from)
}

#[tauri::command]
fn get_workspace_vault_index(
    request: GetWorkspaceVaultIndexRequest,
    state: State<AppState>,
) -> Result<GetWorkspaceVaultIndexResponse, WorkspaceCommandError> {
    let mut workspace = state.workspace.lock().map_err(|_| WorkspaceCommandError {
        code: workspace_service::WorkspaceErrorCode::IoError,
        message: "Falha ao acessar o estado interno do workspace.".to_string(),
        details: None,
        remediation: None,
    })?;
    let root_path = workspace
        .root_path
        .clone()
        .ok_or_else(|| WorkspaceCommandError::from(WorkspaceServiceError::WorkspaceNotOpen))?;

    workspace_service::get_workspace_vault_index(
        &root_path,
        &mut workspace.vault_index_cache,
        request,
    )
    .map_err(WorkspaceCommandError::from)
}

#[tauri::command]
fn get_context_candidates(
    request: GetContextCandidatesRequest,
    state: State<AppState>,
) -> Result<GetContextCandidatesResponse, WorkspaceCommandError> {
    let mut workspace = state.workspace.lock().map_err(|_| WorkspaceCommandError {
        code: workspace_service::WorkspaceErrorCode::IoError,
        message: "Falha ao acessar o estado interno do workspace.".to_string(),
        details: None,
        remediation: None,
    })?;
    let root_path = workspace
        .root_path
        .clone()
        .ok_or_else(|| WorkspaceCommandError::from(WorkspaceServiceError::WorkspaceNotOpen))?;

    workspace_service::get_context_candidates(&root_path, &mut workspace.vault_index_cache, request)
        .map_err(WorkspaceCommandError::from)
}

#[tauri::command]
fn rename_workspace_note(
    request: RenameWorkspaceNoteRequest,
    state: State<AppState>,
) -> Result<RenameWorkspaceNoteResponse, WorkspaceCommandError> {
    let mut workspace = state.workspace.lock().map_err(|_| WorkspaceCommandError {
        code: workspace_service::WorkspaceErrorCode::IoError,
        message: "Falha ao acessar o estado interno do workspace.".to_string(),
        details: None,
        remediation: None,
    })?;
    let root_path = workspace
        .root_path
        .clone()
        .ok_or_else(|| WorkspaceCommandError::from(WorkspaceServiceError::WorkspaceNotOpen))?;

    workspace_service::rename_workspace_note(&root_path, &mut workspace.vault_index_cache, request)
        .map_err(WorkspaceCommandError::from)
}

#[tauri::command]
fn create_project(
    request: CreateProjectRequest,
    state: State<AppState>,
) -> Result<CreateProjectResponse, PersistenceCommandError> {
    state
        .persistence
        .create_project(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn list_projects(
    request: ListProjectsRequest,
    state: State<AppState>,
) -> Result<ListProjectsResponse, PersistenceCommandError> {
    state
        .persistence
        .list_projects(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn open_project(
    request: OpenProjectRequest,
    state: State<AppState>,
) -> Result<OpenProjectResponse, PersistenceCommandError> {
    state
        .persistence
        .open_project(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn archive_project(
    request: ArchiveProjectRequest,
    state: State<AppState>,
) -> Result<ArchiveProjectResponse, PersistenceCommandError> {
    state
        .persistence
        .archive_project(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn ensure_project_for_workspace(
    request: EnsureProjectForWorkspaceRequest,
    state: State<AppState>,
) -> Result<EnsureProjectForWorkspaceResponse, PersistenceCommandError> {
    state
        .persistence
        .ensure_project_for_workspace(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn get_last_active_project(
    state: State<AppState>,
) -> Result<GetLastActiveProjectResponse, PersistenceCommandError> {
    state
        .persistence
        .get_last_active_project()
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn load_ui_session(
    request: LoadUiSessionRequest,
    state: State<AppState>,
) -> Result<LoadUiSessionResponse, PersistenceCommandError> {
    state
        .persistence
        .load_ui_session(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn save_ui_session(
    request: SaveUiSessionRequest,
    state: State<AppState>,
) -> Result<SaveUiSessionResponse, PersistenceCommandError> {
    state
        .persistence
        .save_ui_session(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn create_pulsation(
    request: CreatePulsationRequest,
    state: State<AppState>,
) -> Result<CreatePulsationResponse, PersistenceCommandError> {
    state
        .persistence
        .create_pulsation(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn list_pulsations(
    request: ListPulsationsRequest,
    state: State<AppState>,
) -> Result<ListPulsationsResponse, PersistenceCommandError> {
    state
        .persistence
        .list_pulsations(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn get_pulsation_runtime_snapshot(
    request: GetPulsationRuntimeSnapshotRequest,
    state: State<AppState>,
) -> Result<GetPulsationRuntimeSnapshotResponse, PersistenceCommandError> {
    state
        .persistence
        .get_pulsation_runtime_snapshot(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn apply_hitl_decision(
    request: ApplyHitlDecisionRequest,
    state: State<AppState>,
) -> Result<ApplyHitlDecisionResponse, PersistenceCommandError> {
    state
        .persistence
        .apply_hitl_decision(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn list_pulsation_artifacts(
    request: ListPulsationArtifactsRequest,
    state: State<AppState>,
) -> Result<ListPulsationArtifactsResponse, PersistenceCommandError> {
    state
        .persistence
        .list_pulsation_artifacts(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn get_agent_runtime_config(
    state: State<AppState>,
) -> Result<GetAgentRuntimeConfigResponse, PersistenceCommandError> {
    state
        .persistence
        .get_agent_runtime_config()
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn update_agent_runtime_config(
    request: UpdateAgentRuntimeConfigRequest,
    state: State<AppState>,
) -> Result<UpdateAgentRuntimeConfigResponse, PersistenceCommandError> {
    state
        .persistence
        .update_agent_runtime_config(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn save_provider_api_key(
    request: SaveProviderApiKeyRequest,
    state: State<AppState>,
) -> Result<SaveProviderApiKeyResponse, PersistenceCommandError> {
    state
        .persistence
        .save_provider_api_key(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn clear_provider_api_key(
    request: ClearProviderApiKeyRequest,
    state: State<AppState>,
) -> Result<ClearProviderApiKeyResponse, PersistenceCommandError> {
    state
        .persistence
        .clear_provider_api_key(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn get_provider_key_statuses(
    state: State<AppState>,
) -> Result<GetProviderKeyStatusesResponse, PersistenceCommandError> {
    state
        .persistence
        .get_provider_key_statuses()
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn list_provider_profiles(
    state: State<AppState>,
) -> Result<ListProviderProfilesResponse, PersistenceCommandError> {
    Ok(state.persistence.list_provider_profiles())
}

#[tauri::command]
fn list_provider_connections(
    state: State<AppState>,
) -> Result<ListProviderConnectionsResponse, PersistenceCommandError> {
    state
        .persistence
        .list_provider_connections()
        .map_err(Into::into)
}

#[tauri::command]
fn upsert_provider_connection(
    request: UpsertProviderConnectionRequest,
    state: State<AppState>,
) -> Result<UpsertProviderConnectionResponse, PersistenceCommandError> {
    state
        .persistence
        .upsert_provider_connection(request)
        .map_err(Into::into)
}

#[tauri::command]
fn delete_provider_connection(
    request: DeleteProviderConnectionRequest,
    state: State<AppState>,
) -> Result<DeleteProviderConnectionResponse, PersistenceCommandError> {
    state
        .persistence
        .delete_provider_connection(request)
        .map_err(Into::into)
}

#[tauri::command]
async fn test_provider_connection(
    request: TestProviderConnectionRequest,
    state: State<'_, AppState>,
) -> Result<TestProviderConnectionResponse, PersistenceCommandError> {
    state
        .persistence
        .test_provider_connection(request)
        .await
        .map_err(Into::into)
}

#[tauri::command]
fn list_provider_routes(
    state: State<AppState>,
) -> Result<ListProviderRoutesResponse, PersistenceCommandError> {
    state.persistence.list_provider_routes().map_err(Into::into)
}

#[tauri::command]
fn upsert_provider_route(
    request: UpsertProviderRouteRequest,
    state: State<AppState>,
) -> Result<UpsertProviderRouteResponse, PersistenceCommandError> {
    state
        .persistence
        .upsert_provider_route(request)
        .map_err(Into::into)
}

#[tauri::command]
fn delete_provider_route(
    request: DeleteProviderRouteRequest,
    state: State<AppState>,
) -> Result<DeleteProviderRouteResponse, PersistenceCommandError> {
    state
        .persistence
        .delete_provider_route(request)
        .map_err(Into::into)
}

#[tauri::command]
fn list_provider_route_bindings(
    state: State<AppState>,
) -> Result<ListProviderRouteBindingsResponse, PersistenceCommandError> {
    state
        .persistence
        .list_provider_route_bindings()
        .map_err(Into::into)
}

#[tauri::command]
fn upsert_provider_route_binding(
    request: UpsertProviderRouteBindingRequest,
    state: State<AppState>,
) -> Result<UpsertProviderRouteBindingResponse, PersistenceCommandError> {
    state
        .persistence
        .upsert_provider_route_binding(request)
        .map_err(Into::into)
}

#[tauri::command]
fn get_llm_resilience_snapshot(
    state: State<AppState>,
) -> Result<GetLlmResilienceSnapshotResponse, PersistenceCommandError> {
    state
        .persistence
        .get_llm_resilience_snapshot()
        .map_err(Into::into)
}

#[tauri::command]
fn reset_llm_resilience_state(
    request: ResetLlmResilienceStateRequest,
    state: State<AppState>,
) -> Result<ResetLlmResilienceStateResponse, PersistenceCommandError> {
    state
        .persistence
        .reset_llm_resilience_state(request)
        .map_err(Into::into)
}

#[tauri::command]
fn get_llm_execution_snapshot(
    request: GetLlmExecutionSnapshotRequest,
    state: State<AppState>,
) -> Result<GetLlmExecutionSnapshotResponse, PersistenceCommandError> {
    state
        .persistence
        .get_llm_execution_snapshot(request)
        .map_err(Into::into)
}

#[tauri::command]
fn get_llm_routing_metrics(
    state: State<AppState>,
) -> Result<GetLlmRoutingMetricsResponse, PersistenceCommandError> {
    state
        .persistence
        .get_llm_routing_metrics()
        .map_err(Into::into)
}

#[tauri::command]
fn preview_context_compression(
    request: CompressionPreviewRequest,
    state: State<AppState>,
) -> Result<PreviewContextCompressionResponse, PersistenceCommandError> {
    state
        .persistence
        .preview_context_compression(request)
        .map_err(Into::into)
}

#[tauri::command]
fn cancel_pulsation_jobs(
    request: CancelPulsationJobsRequest,
    state: State<AppState>,
) -> Result<CancelPulsationJobsResponse, PersistenceCommandError> {
    state
        .persistence
        .cancel_pulsation_jobs(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
fn list_agent_runs(
    request: ListAgentRunsRequest,
    state: State<AppState>,
) -> Result<ListAgentRunsResponse, PersistenceCommandError> {
    state
        .persistence
        .list_agent_runs(request)
        .map_err(PersistenceCommandError::from)
}

#[tauri::command]
async fn reindex_workspace_embeddings(
    request: ReindexWorkspaceEmbeddingsRequest,
    state: State<'_, AppState>,
) -> Result<ReindexWorkspaceEmbeddingsResponse, PersistenceCommandError> {
    let root_path = get_workspace_root(&state).map_err(|error| PersistenceCommandError {
        code: persistence_service::PersistenceErrorCode::ValidationError,
        message: error.message,
        details: None,
        remediation: None,
    })?;
    let workspace_path = root_path.to_string_lossy().to_string();
    state
        .persistence
        .reindex_workspace_embeddings_for_workspace(workspace_path.as_str(), request)
        .await
        .map_err(PersistenceCommandError::from)
}

fn get_workspace_root(state: &State<AppState>) -> Result<PathBuf, WorkspaceCommandError> {
    let workspace = state.workspace.lock().map_err(|_| WorkspaceCommandError {
        code: workspace_service::WorkspaceErrorCode::IoError,
        message: "Falha ao acessar o estado interno do workspace.".to_string(),
        details: None,
        remediation: None,
    })?;
    workspace
        .root_path
        .clone()
        .ok_or_else(|| WorkspaceCommandError::from(WorkspaceServiceError::WorkspaceNotOpen))
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let app_data_dir = app.path().app_data_dir().map_err(|error| {
                io::Error::other(format!("Não foi possível resolver AppData: {error}"))
            })?;
            let event_sink = Arc::new(TauriEventSink::new(app.handle().clone()));
            let persistence =
                PersistenceService::initialize_with_event_sink(&app_data_dir, event_sink).map_err(
                    |error| io::Error::other(PersistenceCommandError::from(error).message),
                )?;
            persistence.recover_interrupted_jobs().map_err(|error| {
                io::Error::other(PersistenceCommandError::from(error).message.clone())
            })?;
            let worker_persistence = persistence.clone();
            tauri::async_runtime::spawn(async move {
                worker_persistence.run_job_worker_loop().await;
            });
            app.manage(AppState::new(persistence));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            open_workspace,
            bootstrap_greenfield_workspace,
            list_workspace_tree,
            read_workspace_file,
            write_workspace_file_atomic,
            search_workspace_text,
            get_workspace_vault_index,
            get_context_candidates,
            rename_workspace_note,
            create_project,
            list_projects,
            open_project,
            archive_project,
            ensure_project_for_workspace,
            get_last_active_project,
            load_ui_session,
            save_ui_session,
            create_pulsation,
            list_pulsations,
            get_pulsation_runtime_snapshot,
            apply_hitl_decision,
            list_pulsation_artifacts,
            get_agent_runtime_config,
            update_agent_runtime_config,
            save_provider_api_key,
            clear_provider_api_key,
            get_provider_key_statuses,
            list_provider_profiles,
            list_provider_connections,
            upsert_provider_connection,
            delete_provider_connection,
            test_provider_connection,
            list_provider_routes,
            upsert_provider_route,
            delete_provider_route,
            list_provider_route_bindings,
            upsert_provider_route_binding,
            get_llm_resilience_snapshot,
            reset_llm_resilience_state,
            get_llm_execution_snapshot,
            get_llm_routing_metrics,
            preview_context_compression,
            cancel_pulsation_jobs,
            list_agent_runs,
            reindex_workspace_embeddings
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
