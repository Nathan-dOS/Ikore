import { open } from "@tauri-apps/plugin-dialog";

import {
  APPLY_HITL_DECISION_COMMAND,
  ARCHIVE_PROJECT_COMMAND,
  BOOTSTRAP_GREENFIELD_WORKSPACE_COMMAND,
  CANCEL_PULSATION_JOBS_COMMAND,
  CLEAR_PROVIDER_API_KEY_COMMAND,
  CREATE_PULSATION_COMMAND,
  CREATE_PROJECT_COMMAND,
  ENSURE_PROJECT_FOR_WORKSPACE_COMMAND,
  GET_CONTEXT_CANDIDATES_COMMAND,
  GET_WORKSPACE_VAULT_INDEX_COMMAND,
  GET_AGENT_RUNTIME_CONFIG_COMMAND,
  GET_LAST_ACTIVE_PROJECT_COMMAND,
  GET_PROVIDER_KEY_STATUSES_COMMAND,
  GET_PULSATION_RUNTIME_SNAPSHOT_COMMAND,
  LIST_AGENT_RUNS_COMMAND,
  LIST_PULSATION_ARTIFACTS_COMMAND,
  LIST_PULSATIONS_COMMAND,
  LIST_PROJECTS_COMMAND,
  LIST_WORKSPACE_TREE_COMMAND,
  LOAD_UI_SESSION_COMMAND,
  OPEN_WORKSPACE_COMMAND,
  OPEN_PROJECT_COMMAND,
  RENAME_WORKSPACE_NOTE_COMMAND,
  READ_WORKSPACE_FILE_COMMAND,
  SAVE_PROVIDER_API_KEY_COMMAND,
  SAVE_UI_SESSION_COMMAND,
  SEARCH_WORKSPACE_TEXT_COMMAND,
  REINDEX_WORKSPACE_EMBEDDINGS_COMMAND,
  UPDATE_AGENT_RUNTIME_CONFIG_COMMAND,
  LIST_PROVIDER_PROFILES_COMMAND,
  LIST_PROVIDER_CONNECTIONS_COMMAND,
  UPSERT_PROVIDER_CONNECTION_COMMAND,
  DELETE_PROVIDER_CONNECTION_COMMAND,
  TEST_PROVIDER_CONNECTION_COMMAND,
  LIST_PROVIDER_ROUTES_COMMAND,
  UPSERT_PROVIDER_ROUTE_COMMAND,
  DELETE_PROVIDER_ROUTE_COMMAND,
  LIST_PROVIDER_ROUTE_BINDINGS_COMMAND,
  UPSERT_PROVIDER_ROUTE_BINDING_COMMAND,
  GET_LLM_RESILIENCE_SNAPSHOT_COMMAND,
  RESET_LLM_RESILIENCE_STATE_COMMAND,
  GET_LLM_EXECUTION_SNAPSHOT_COMMAND,
  GET_LLM_ROUTING_METRICS_COMMAND,
  PREVIEW_CONTEXT_COMPRESSION_COMMAND,
  LIST_CHAT_CONVERSATIONS_COMMAND,
  CREATE_CHAT_CONVERSATION_COMMAND,
  GET_CHAT_CONVERSATION_COMMAND,
  DELETE_CHAT_CONVERSATION_COMMAND,
  SEND_CHAT_MESSAGE_COMMAND,
  CANCEL_CHAT_TURN_COMMAND,
  GET_CHAT_TURN_SNAPSHOT_COMMAND,
  type AppDomainError,
  type AppErrorCode,
  type ApplyHitlDecisionRequest,
  type ApplyHitlDecisionResponse,
  type ArchiveProjectRequest,
  type ArchiveProjectResponse,
  type CancelPulsationJobsRequest,
  type CancelPulsationJobsResponse,
  type ClearProviderApiKeyRequest,
  type ClearProviderApiKeyResponse,
  type CreatePulsationRequest,
  type CreatePulsationResponse,
  type CreateProjectRequest,
  type CreateProjectResponse,
  type BootstrapGreenfieldWorkspaceRequest,
  type BootstrapGreenfieldWorkspaceResponse,
  type GetContextCandidatesRequest,
  type GetContextCandidatesResponse,
  type EnsureProjectForWorkspaceRequest,
  type EnsureProjectForWorkspaceResponse,
  type GetWorkspaceVaultIndexRequest,
  type GetWorkspaceVaultIndexResponse,
  type GetAgentRuntimeConfigResponse,
  type GetLastActiveProjectResponse,
  type GetProviderKeyStatusesResponse,
  type GetPulsationRuntimeSnapshotRequest,
  type GetPulsationRuntimeSnapshotResponse,
  type ListAgentRunsRequest,
  type ListAgentRunsResponse,
  type ListPulsationArtifactsRequest,
  type ListPulsationArtifactsResponse,
  type ListPulsationsRequest,
  type ListPulsationsResponse,
  type ListProjectsRequest,
  type ListProjectsResponse,
  type LoadUiSessionRequest,
  type LoadUiSessionResponse,
  type OpenProjectRequest,
  type OpenProjectResponse,
  type RenameWorkspaceNoteRequest,
  type RenameWorkspaceNoteResponse,
  type SaveProviderApiKeyRequest,
  type SaveProviderApiKeyResponse,
  type SaveUiSessionRequest,
  type SaveUiSessionResponse,
  type SearchWorkspaceTextRequest,
  type SearchWorkspaceTextResponse,
  type ReindexWorkspaceEmbeddingsRequest,
  type ReindexWorkspaceEmbeddingsResponse,
  type UpdateAgentRuntimeConfigRequest,
  type UpdateAgentRuntimeConfigResponse,
  type ListProviderProfilesResponse,
  type ListProviderConnectionsResponse,
  type UpsertProviderConnectionRequest,
  type UpsertProviderConnectionResponse,
  type DeleteProviderConnectionRequest,
  type DeleteProviderConnectionResponse,
  type TestProviderConnectionRequest,
  type TestProviderConnectionResponse,
  type ListProviderRoutesResponse,
  type UpsertProviderRouteRequest,
  type UpsertProviderRouteResponse,
  type DeleteProviderRouteRequest,
  type DeleteProviderRouteResponse,
  type ListProviderRouteBindingsResponse,
  type UpsertProviderRouteBindingRequest,
  type UpsertProviderRouteBindingResponse,
  type GetLlmResilienceSnapshotResponse,
  type ResetLlmResilienceStateRequest,
  type ResetLlmResilienceStateResponse,
  type GetLlmExecutionSnapshotRequest,
  type GetLlmExecutionSnapshotResponse,
  type GetLlmRoutingMetricsResponse,
  type CompressionPreviewRequest,
  type PreviewContextCompressionResponse,
  type ListChatConversationsRequest,
  type ListChatConversationsResponse,
  type CreateChatConversationRequest,
  type CreateChatConversationResponse,
  type GetChatConversationRequest,
  type GetChatConversationResponse,
  type DeleteChatConversationRequest,
  type DeleteChatConversationResponse,
  type SendChatMessageRequest,
  type SendChatMessageResponse,
  type CancelChatTurnRequest,
  type CancelChatTurnResponse,
  type GetChatTurnSnapshotRequest,
  type GetChatTurnSnapshotResponse,
  WRITE_WORKSPACE_FILE_ATOMIC_COMMAND,
  type ListWorkspaceTreeResponse,
  type OpenWorkspaceRequest,
  type OpenWorkspaceResponse,
  type PickWorkspaceRootResponse,
  type ReadWorkspaceFileRequest,
  type ReadWorkspaceFileResponse,
  type WriteWorkspaceFileAtomicRequest,
  type WriteWorkspaceFileAtomicResponse
} from "./contracts";

const APP_ERROR_CODES: AppErrorCode[] = [
  "invalid_root",
  "path_forbidden",
  "invalid_extension",
  "invalid_query",
  "conflict",
  "io_error",
  "validation_error",
  "not_found",
  "db_error",
  "migration_error",
  "runtime_error",
  "missing_api_key",
  "provider_timeout",
  "provider_blocked",
  "all_targets_exhausted",
  "schema_violation",
  "route_conflict",
  "stream_failed",
  "chat_persistence",
  "chat_runtime",
  "invalid_request",
  "invalid_message",
  "project_not_found",
  "conversation_not_found",
  "turn_not_found",
  "turn_active",
  "workspace_not_open",
  "workspace_mismatch",
  "connection_not_found",
  "connection_removed",
  "connection_changed",
  "connection_unavailable",
  "credential_unavailable",
  "provider_incompatible",
  "model_incompatible",
  "repository_unavailable",
  "provider_unavailable",
  "invalid_response",
  "invalid_tool_arguments",
  "empty_response",
  "tool_call_limit",
  "tool_result_limit",
  "model_round_limit",
  "turn_timeout",
  "cancelled"
];

type TauriWindow = Window & {
  __TAURI_INTERNALS__?: {
    invoke?: (command: string, args?: Record<string, unknown>) => Promise<unknown>;
  };
};

export class WorkspaceClientError extends Error {
  public readonly code: AppErrorCode;

  constructor(domainError: AppDomainError) {
    super(domainError.message);
    this.code = domainError.code;
    this.name = "WorkspaceClientError";
  }
}

export function isTauriRuntime(): boolean {
  if (typeof window === "undefined") {
    return false;
  }

  const tauriWindow = window as TauriWindow;
  return typeof tauriWindow.__TAURI_INTERNALS__?.invoke === "function";
}

async function invokeTauri<T>(command: string, args?: unknown): Promise<T> {
  if (!isTauriRuntime()) {
    throw new Error(
      "IPC indisponível no modo navegador. Abra via `npm run tauri:dev` para usar comandos Rust."
    );
  }

  const { invoke } = await import("@tauri-apps/api/core");
  const payload =
    args && typeof args === "object" ? (args as Record<string, unknown>) : undefined;
  return invoke<T>(command, payload);
}

function isAppErrorCode(value: unknown): value is AppErrorCode {
  return typeof value === "string" && APP_ERROR_CODES.includes(value as AppErrorCode);
}

function parseDomainError(input: unknown): AppDomainError | null {
  if (!input || typeof input !== "object") {
    return null;
  }

  const candidate = input as Partial<AppDomainError>;
  if (!isAppErrorCode(candidate.code) || typeof candidate.message !== "string") {
    return null;
  }

  const normalized: AppDomainError = {
    code: candidate.code,
    message: candidate.message
  };
  if (candidate.details !== undefined) normalized.details = candidate.details;
  if (typeof candidate.remediation === "string") normalized.remediation = candidate.remediation;
  return normalized;
}

export function normalizeWorkspaceError(error: unknown): AppDomainError {
  const directDomainError = parseDomainError(error);
  if (directDomainError) {
    return directDomainError;
  }

  if (typeof error === "string") {
    try {
      const parsed = JSON.parse(error);
      const parsedDomainError = parseDomainError(parsed);
      if (parsedDomainError) {
        return parsedDomainError;
      }
    } catch {
      return { code: "io_error", message: error };
    }
  }

  if (error instanceof Error) {
    try {
      const parsed = JSON.parse(error.message);
      const parsedDomainError = parseDomainError(parsed);
      if (parsedDomainError) {
        return parsedDomainError;
      }
    } catch {
      return { code: "io_error", message: error.message };
    }

    return { code: "io_error", message: error.message };
  }

  return {
    code: "io_error",
    message: "Falha inesperada na comunicação com o backend."
  };
}

async function invokeWorkspace<T>(
  command: string,
  args?: unknown
): Promise<T> {
  try {
    return await invokeTauri<T>(command, args);
  } catch (error) {
    throw new WorkspaceClientError(normalizeWorkspaceError(error));
  }
}

export async function pickWorkspaceRoot(): Promise<PickWorkspaceRootResponse> {
  if (!isTauriRuntime()) {
    throw new WorkspaceClientError({
      code: "io_error",
      message: "Seleção de pasta disponível apenas no app desktop."
    });
  }

  const selectedPath = await open({
    directory: true,
    multiple: false,
    title: "Selecione a pasta de workspace"
  });

  if (typeof selectedPath === "string") {
    return { path: selectedPath };
  }

  return { path: null };
}

export async function openWorkspace(
  payload: OpenWorkspaceRequest
): Promise<OpenWorkspaceResponse> {
  return invokeWorkspace<OpenWorkspaceResponse>(OPEN_WORKSPACE_COMMAND, { request: payload });
}

export async function bootstrapGreenfieldWorkspace(
  payload: BootstrapGreenfieldWorkspaceRequest
): Promise<BootstrapGreenfieldWorkspaceResponse> {
  return invokeWorkspace<BootstrapGreenfieldWorkspaceResponse>(
    BOOTSTRAP_GREENFIELD_WORKSPACE_COMMAND,
    { request: payload }
  );
}

export async function listWorkspaceTree(): Promise<ListWorkspaceTreeResponse> {
  return invokeWorkspace<ListWorkspaceTreeResponse>(LIST_WORKSPACE_TREE_COMMAND);
}

export async function readWorkspaceFile(
  payload: ReadWorkspaceFileRequest
): Promise<ReadWorkspaceFileResponse> {
  return invokeWorkspace<ReadWorkspaceFileResponse>(READ_WORKSPACE_FILE_COMMAND, {
    request: payload
  });
}

export async function writeWorkspaceFileAtomic(
  payload: WriteWorkspaceFileAtomicRequest
): Promise<WriteWorkspaceFileAtomicResponse> {
  return invokeWorkspace<WriteWorkspaceFileAtomicResponse>(
    WRITE_WORKSPACE_FILE_ATOMIC_COMMAND,
    { request: payload }
  );
}

export async function searchWorkspaceText(
  payload: SearchWorkspaceTextRequest
): Promise<SearchWorkspaceTextResponse> {
  return invokeWorkspace<SearchWorkspaceTextResponse>(SEARCH_WORKSPACE_TEXT_COMMAND, {
    request: payload
  });
}

export async function getWorkspaceVaultIndex(
  payload: GetWorkspaceVaultIndexRequest = {}
): Promise<GetWorkspaceVaultIndexResponse> {
  return invokeWorkspace<GetWorkspaceVaultIndexResponse>(GET_WORKSPACE_VAULT_INDEX_COMMAND, {
    request: payload
  });
}

export async function getContextCandidates(
  payload: GetContextCandidatesRequest
): Promise<GetContextCandidatesResponse> {
  return invokeWorkspace<GetContextCandidatesResponse>(GET_CONTEXT_CANDIDATES_COMMAND, {
    request: payload
  });
}

export async function renameWorkspaceNote(
  payload: RenameWorkspaceNoteRequest
): Promise<RenameWorkspaceNoteResponse> {
  return invokeWorkspace<RenameWorkspaceNoteResponse>(RENAME_WORKSPACE_NOTE_COMMAND, {
    request: payload
  });
}

export async function createProject(
  payload: CreateProjectRequest
): Promise<CreateProjectResponse> {
  return invokeWorkspace<CreateProjectResponse>(CREATE_PROJECT_COMMAND, { request: payload });
}

export async function listProjects(
  payload: ListProjectsRequest = {}
): Promise<ListProjectsResponse> {
  return invokeWorkspace<ListProjectsResponse>(LIST_PROJECTS_COMMAND, { request: payload });
}

export async function openProject(
  payload: OpenProjectRequest
): Promise<OpenProjectResponse> {
  return invokeWorkspace<OpenProjectResponse>(OPEN_PROJECT_COMMAND, { request: payload });
}

export async function archiveProject(
  payload: ArchiveProjectRequest
): Promise<ArchiveProjectResponse> {
  return invokeWorkspace<ArchiveProjectResponse>(ARCHIVE_PROJECT_COMMAND, { request: payload });
}

export async function ensureProjectForWorkspace(
  payload: EnsureProjectForWorkspaceRequest
): Promise<EnsureProjectForWorkspaceResponse> {
  return invokeWorkspace<EnsureProjectForWorkspaceResponse>(
    ENSURE_PROJECT_FOR_WORKSPACE_COMMAND,
    { request: payload }
  );
}

export async function getLastActiveProject(): Promise<GetLastActiveProjectResponse> {
  return invokeWorkspace<GetLastActiveProjectResponse>(GET_LAST_ACTIVE_PROJECT_COMMAND);
}

export async function loadUiSession(
  payload: LoadUiSessionRequest
): Promise<LoadUiSessionResponse> {
  return invokeWorkspace<LoadUiSessionResponse>(LOAD_UI_SESSION_COMMAND, { request: payload });
}

export async function saveUiSession(
  payload: SaveUiSessionRequest
): Promise<SaveUiSessionResponse> {
  return invokeWorkspace<SaveUiSessionResponse>(SAVE_UI_SESSION_COMMAND, { request: payload });
}

export async function createPulsation(
  payload: CreatePulsationRequest
): Promise<CreatePulsationResponse> {
  return invokeWorkspace<CreatePulsationResponse>(CREATE_PULSATION_COMMAND, {
    request: payload
  });
}

export async function listPulsations(
  payload: ListPulsationsRequest
): Promise<ListPulsationsResponse> {
  return invokeWorkspace<ListPulsationsResponse>(LIST_PULSATIONS_COMMAND, {
    request: payload
  });
}

export async function getPulsationRuntimeSnapshot(
  payload: GetPulsationRuntimeSnapshotRequest
): Promise<GetPulsationRuntimeSnapshotResponse> {
  return invokeWorkspace<GetPulsationRuntimeSnapshotResponse>(
    GET_PULSATION_RUNTIME_SNAPSHOT_COMMAND,
    { request: payload }
  );
}

export async function applyHitlDecision(
  payload: ApplyHitlDecisionRequest
): Promise<ApplyHitlDecisionResponse> {
  return invokeWorkspace<ApplyHitlDecisionResponse>(APPLY_HITL_DECISION_COMMAND, {
    request: payload
  });
}

export async function listPulsationArtifacts(
  payload: ListPulsationArtifactsRequest
): Promise<ListPulsationArtifactsResponse> {
  return invokeWorkspace<ListPulsationArtifactsResponse>(
    LIST_PULSATION_ARTIFACTS_COMMAND,
    { request: payload }
  );
}

export async function getAgentRuntimeConfig(): Promise<GetAgentRuntimeConfigResponse> {
  return invokeWorkspace<GetAgentRuntimeConfigResponse>(GET_AGENT_RUNTIME_CONFIG_COMMAND);
}

export async function updateAgentRuntimeConfig(
  payload: UpdateAgentRuntimeConfigRequest
): Promise<UpdateAgentRuntimeConfigResponse> {
  return invokeWorkspace<UpdateAgentRuntimeConfigResponse>(
    UPDATE_AGENT_RUNTIME_CONFIG_COMMAND,
    { request: payload }
  );
}

export async function saveProviderApiKey(
  payload: SaveProviderApiKeyRequest
): Promise<SaveProviderApiKeyResponse> {
  return invokeWorkspace<SaveProviderApiKeyResponse>(SAVE_PROVIDER_API_KEY_COMMAND, {
    request: payload
  });
}

export async function clearProviderApiKey(
  payload: ClearProviderApiKeyRequest
): Promise<ClearProviderApiKeyResponse> {
  return invokeWorkspace<ClearProviderApiKeyResponse>(CLEAR_PROVIDER_API_KEY_COMMAND, {
    request: payload
  });
}

export async function getProviderKeyStatuses(): Promise<GetProviderKeyStatusesResponse> {
  return invokeWorkspace<GetProviderKeyStatusesResponse>(GET_PROVIDER_KEY_STATUSES_COMMAND);
}

export async function cancelPulsationJobs(
  payload: CancelPulsationJobsRequest
): Promise<CancelPulsationJobsResponse> {
  return invokeWorkspace<CancelPulsationJobsResponse>(CANCEL_PULSATION_JOBS_COMMAND, {
    request: payload
  });
}

export async function listAgentRuns(
  payload: ListAgentRunsRequest
): Promise<ListAgentRunsResponse> {
  return invokeWorkspace<ListAgentRunsResponse>(LIST_AGENT_RUNS_COMMAND, {
    request: payload
  });
}

export async function reindexWorkspaceEmbeddings(
  payload: ReindexWorkspaceEmbeddingsRequest = {}
): Promise<ReindexWorkspaceEmbeddingsResponse> {
  return invokeWorkspace<ReindexWorkspaceEmbeddingsResponse>(
    REINDEX_WORKSPACE_EMBEDDINGS_COMMAND,
    { request: payload }
  );
}

export async function listProviderProfiles(): Promise<ListProviderProfilesResponse> {
  return invokeWorkspace<ListProviderProfilesResponse>(LIST_PROVIDER_PROFILES_COMMAND);
}

export async function listProviderConnections(): Promise<ListProviderConnectionsResponse> {
  return invokeWorkspace<ListProviderConnectionsResponse>(LIST_PROVIDER_CONNECTIONS_COMMAND);
}

export async function listChatConversations(
  payload: ListChatConversationsRequest
): Promise<ListChatConversationsResponse> {
  return invokeWorkspace<ListChatConversationsResponse>(LIST_CHAT_CONVERSATIONS_COMMAND, {
    request: payload
  });
}

export async function createChatConversation(
  payload: CreateChatConversationRequest
): Promise<CreateChatConversationResponse> {
  return invokeWorkspace<CreateChatConversationResponse>(CREATE_CHAT_CONVERSATION_COMMAND, {
    request: payload
  });
}

export async function getChatConversation(
  payload: GetChatConversationRequest
): Promise<GetChatConversationResponse> {
  return invokeWorkspace<GetChatConversationResponse>(GET_CHAT_CONVERSATION_COMMAND, {
    request: payload
  });
}

export async function deleteChatConversation(
  payload: DeleteChatConversationRequest
): Promise<DeleteChatConversationResponse> {
  return invokeWorkspace<DeleteChatConversationResponse>(DELETE_CHAT_CONVERSATION_COMMAND, {
    request: payload
  });
}

export async function sendChatMessage(
  payload: SendChatMessageRequest
): Promise<SendChatMessageResponse> {
  return invokeWorkspace<SendChatMessageResponse>(SEND_CHAT_MESSAGE_COMMAND, {
    request: payload
  });
}

export async function cancelChatTurn(
  payload: CancelChatTurnRequest
): Promise<CancelChatTurnResponse> {
  return invokeWorkspace<CancelChatTurnResponse>(CANCEL_CHAT_TURN_COMMAND, {
    request: payload
  });
}

export async function getChatTurnSnapshot(
  payload: GetChatTurnSnapshotRequest
): Promise<GetChatTurnSnapshotResponse> {
  return invokeWorkspace<GetChatTurnSnapshotResponse>(GET_CHAT_TURN_SNAPSHOT_COMMAND, {
    request: payload
  });
}

export async function upsertProviderConnection(
  payload: UpsertProviderConnectionRequest
): Promise<UpsertProviderConnectionResponse> {
  return invokeWorkspace<UpsertProviderConnectionResponse>(UPSERT_PROVIDER_CONNECTION_COMMAND, {
    request: payload
  });
}

export async function deleteProviderConnection(
  payload: DeleteProviderConnectionRequest
): Promise<DeleteProviderConnectionResponse> {
  return invokeWorkspace<DeleteProviderConnectionResponse>(DELETE_PROVIDER_CONNECTION_COMMAND, {
    request: payload
  });
}

export async function testProviderConnection(
  payload: TestProviderConnectionRequest
): Promise<TestProviderConnectionResponse> {
  return invokeWorkspace<TestProviderConnectionResponse>(TEST_PROVIDER_CONNECTION_COMMAND, {
    request: payload
  });
}

export async function listProviderRoutes(): Promise<ListProviderRoutesResponse> {
  return invokeWorkspace<ListProviderRoutesResponse>(LIST_PROVIDER_ROUTES_COMMAND);
}

export async function upsertProviderRoute(
  payload: UpsertProviderRouteRequest
): Promise<UpsertProviderRouteResponse> {
  return invokeWorkspace<UpsertProviderRouteResponse>(UPSERT_PROVIDER_ROUTE_COMMAND, {
    request: payload
  });
}

export async function deleteProviderRoute(
  payload: DeleteProviderRouteRequest
): Promise<DeleteProviderRouteResponse> {
  return invokeWorkspace<DeleteProviderRouteResponse>(DELETE_PROVIDER_ROUTE_COMMAND, {
    request: payload
  });
}

export async function listProviderRouteBindings(): Promise<ListProviderRouteBindingsResponse> {
  return invokeWorkspace<ListProviderRouteBindingsResponse>(LIST_PROVIDER_ROUTE_BINDINGS_COMMAND);
}

export async function upsertProviderRouteBinding(
  payload: UpsertProviderRouteBindingRequest
): Promise<UpsertProviderRouteBindingResponse> {
  return invokeWorkspace<UpsertProviderRouteBindingResponse>(
    UPSERT_PROVIDER_ROUTE_BINDING_COMMAND,
    { request: payload }
  );
}

export async function getLlmResilienceSnapshot(): Promise<GetLlmResilienceSnapshotResponse> {
  return invokeWorkspace<GetLlmResilienceSnapshotResponse>(GET_LLM_RESILIENCE_SNAPSHOT_COMMAND);
}

export async function resetLlmResilienceState(
  payload: ResetLlmResilienceStateRequest
): Promise<ResetLlmResilienceStateResponse> {
  return invokeWorkspace<ResetLlmResilienceStateResponse>(RESET_LLM_RESILIENCE_STATE_COMMAND, {
    request: payload
  });
}

export async function getLlmExecutionSnapshot(
  payload: GetLlmExecutionSnapshotRequest
): Promise<GetLlmExecutionSnapshotResponse> {
  return invokeWorkspace<GetLlmExecutionSnapshotResponse>(GET_LLM_EXECUTION_SNAPSHOT_COMMAND, {
    request: payload
  });
}

export async function getLlmRoutingMetrics(): Promise<GetLlmRoutingMetricsResponse> {
  return invokeWorkspace<GetLlmRoutingMetricsResponse>(GET_LLM_ROUTING_METRICS_COMMAND);
}

export async function previewContextCompression(
  payload: CompressionPreviewRequest
): Promise<PreviewContextCompressionResponse> {
  return invokeWorkspace<PreviewContextCompressionResponse>(PREVIEW_CONTEXT_COMPRESSION_COMMAND, {
    request: payload
  });
}
