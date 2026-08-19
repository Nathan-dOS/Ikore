export const OPEN_WORKSPACE_COMMAND = "open_workspace";
export const BOOTSTRAP_GREENFIELD_WORKSPACE_COMMAND = "bootstrap_greenfield_workspace";
export const LIST_WORKSPACE_TREE_COMMAND = "list_workspace_tree";
export const READ_WORKSPACE_FILE_COMMAND = "read_workspace_file";
export const WRITE_WORKSPACE_FILE_ATOMIC_COMMAND = "write_workspace_file_atomic";
export const SEARCH_WORKSPACE_TEXT_COMMAND = "search_workspace_text";
export const GET_WORKSPACE_VAULT_INDEX_COMMAND = "get_workspace_vault_index";
export const GET_CONTEXT_CANDIDATES_COMMAND = "get_context_candidates";
export const RENAME_WORKSPACE_NOTE_COMMAND = "rename_workspace_note";
export const CREATE_PROJECT_COMMAND = "create_project";
export const LIST_PROJECTS_COMMAND = "list_projects";
export const OPEN_PROJECT_COMMAND = "open_project";
export const ARCHIVE_PROJECT_COMMAND = "archive_project";
export const ENSURE_PROJECT_FOR_WORKSPACE_COMMAND = "ensure_project_for_workspace";
export const GET_LAST_ACTIVE_PROJECT_COMMAND = "get_last_active_project";
export const LOAD_UI_SESSION_COMMAND = "load_ui_session";
export const SAVE_UI_SESSION_COMMAND = "save_ui_session";
export const CREATE_PULSATION_COMMAND = "create_pulsation";
export const LIST_PULSATIONS_COMMAND = "list_pulsations";
export const GET_PULSATION_RUNTIME_SNAPSHOT_COMMAND = "get_pulsation_runtime_snapshot";
export const APPLY_HITL_DECISION_COMMAND = "apply_hitl_decision";
export const LIST_PULSATION_ARTIFACTS_COMMAND = "list_pulsation_artifacts";
export const GET_AGENT_RUNTIME_CONFIG_COMMAND = "get_agent_runtime_config";
export const UPDATE_AGENT_RUNTIME_CONFIG_COMMAND = "update_agent_runtime_config";
export const SAVE_PROVIDER_API_KEY_COMMAND = "save_provider_api_key";
export const CLEAR_PROVIDER_API_KEY_COMMAND = "clear_provider_api_key";
export const GET_PROVIDER_KEY_STATUSES_COMMAND = "get_provider_key_statuses";
export const CANCEL_PULSATION_JOBS_COMMAND = "cancel_pulsation_jobs";
export const LIST_AGENT_RUNS_COMMAND = "list_agent_runs";
export const REINDEX_WORKSPACE_EMBEDDINGS_COMMAND = "reindex_workspace_embeddings";
export const LIST_PROVIDER_PROFILES_COMMAND = "list_provider_profiles";
export const LIST_PROVIDER_CONNECTIONS_COMMAND = "list_provider_connections";
export const UPSERT_PROVIDER_CONNECTION_COMMAND = "upsert_provider_connection";
export const DELETE_PROVIDER_CONNECTION_COMMAND = "delete_provider_connection";
export const TEST_PROVIDER_CONNECTION_COMMAND = "test_provider_connection";
export const LIST_PROVIDER_ROUTES_COMMAND = "list_provider_routes";
export const UPSERT_PROVIDER_ROUTE_COMMAND = "upsert_provider_route";
export const DELETE_PROVIDER_ROUTE_COMMAND = "delete_provider_route";
export const LIST_PROVIDER_ROUTE_BINDINGS_COMMAND = "list_provider_route_bindings";
export const UPSERT_PROVIDER_ROUTE_BINDING_COMMAND = "upsert_provider_route_binding";
export const GET_LLM_RESILIENCE_SNAPSHOT_COMMAND = "get_llm_resilience_snapshot";
export const RESET_LLM_RESILIENCE_STATE_COMMAND = "reset_llm_resilience_state";
export const GET_LLM_EXECUTION_SNAPSHOT_COMMAND = "get_llm_execution_snapshot";
export const GET_LLM_ROUTING_METRICS_COMMAND = "get_llm_routing_metrics";
export const PREVIEW_CONTEXT_COMPRESSION_COMMAND = "preview_context_compression";

export const WORKSPACE_CHANGED_EVENT = "workspace://changed";
export const LLM_ROUTE_STARTED_EVENT = "llm://route-started";
export const LLM_ATTEMPT_STARTED_EVENT = "llm://attempt-started";
export const LLM_TOKEN_DELTA_EVENT = "llm://token-delta";
export const LLM_ATTEMPT_FAILED_EVENT = "llm://attempt-failed";
export const LLM_ROUTE_COMPLETED_EVENT = "llm://route-completed";
export const LLM_ROUTE_FAILED_EVENT = "llm://route-failed";
export const LLM_RESILIENCE_CHANGED_EVENT = "llm://resilience-changed";

export type FileNodeKind = "directory" | "file";

export interface FileNode {
  name: string;
  relativePath: string;
  kind: FileNodeKind;
  children?: FileNode[];
}

export interface PickWorkspaceRootResponse {
  path: string | null;
}

export interface OpenWorkspaceRequest {
  path: string;
}

export interface OpenWorkspaceResponse {
  rootPath: string;
  tree: FileNode[];
  greenfield: GreenfieldBootstrapStatus;
}

export interface GreenfieldBootstrapStatus {
  mode: string;
  initialized: boolean;
  message: string;
  createdPaths: string[];
}

export interface BootstrapGreenfieldWorkspaceRequest {
  path: string;
}

export interface BootstrapGreenfieldWorkspaceResponse {
  rootPath: string;
  status: GreenfieldBootstrapStatus;
}

export interface ListWorkspaceTreeResponse {
  tree: FileNode[];
}

export interface ReadWorkspaceFileRequest {
  relativePath: string;
}

export interface ReadWorkspaceFileResponse {
  content: string;
  revision: string;
}

export interface WriteWorkspaceFileAtomicRequest {
  relativePath: string;
  content: string;
  expectedRevision: string;
}

export interface WriteWorkspaceFileAtomicResponse {
  revision: string;
}

export type SearchMode = "substring" | "regex";

export interface SearchWorkspaceTextRequest {
  query: string;
  mode: SearchMode;
  caseSensitive?: boolean;
  maxResults?: number;
}

export interface SearchMatch {
  relativePath: string;
  line: number;
  column: number;
  lineText: string;
}

export interface SearchWorkspaceTextResponse {
  matches: SearchMatch[];
  truncated: boolean;
  scannedFiles: number;
}

export interface GetWorkspaceVaultIndexRequest {
  forceRebuild?: boolean;
}

export interface GetContextCandidatesRequest {
  featureName: string;
  stageName?: string;
  agentScope?: string;
  maxCandidates?: number;
  forceRebuild?: boolean;
}

export interface ContextCandidate {
  relativePath: string;
  score: number;
}

export interface GetContextCandidatesResponse {
  strategy: string;
  fallbackUsed: boolean;
  featureNotePath: string | null;
  scopeFile: string | null;
  candidates: ContextCandidate[];
}

export interface VaultLinkReference {
  target: string;
  alias: string | null;
  line: number;
  resolvedPath: string | null;
}

export interface VaultBrokenLink {
  sourcePath: string;
  target: string;
  line: number;
}

export interface VaultEdge {
  sourcePath: string;
  targetPath: string;
}

export interface VaultNoteSummary {
  relativePath: string;
  title: string;
  revision: string;
  frontmatter: Record<string, unknown> | null;
  normalizedProperties?: Record<string, unknown> | null;
  links: VaultLinkReference[];
  outgoingPaths: string[];
  backlinkPaths?: string[];
  schemaWarnings?: string[];
}

export interface GetWorkspaceVaultIndexResponse {
  notes: VaultNoteSummary[];
  edges: VaultEdge[];
  brokenLinks: VaultBrokenLink[];
  indexedAt: string;
}

export interface RenameWorkspaceNoteRequest {
  fromRelativePath: string;
  toRelativePath: string;
}

export interface RenameWorkspaceNoteResponse {
  renamedTo: string;
  updatedReferences: number;
  affectedFiles: number;
}

export type WorkspaceErrorCode =
  | "invalid_root"
  | "path_forbidden"
  | "invalid_extension"
  | "invalid_query"
  | "conflict"
  | "io_error";

export type PersistenceErrorCode =
  | "validation_error"
  | "not_found"
  | "db_error"
  | "migration_error"
  | "io_error"
  | "runtime_error"
  | "missing_api_key"
  | "provider_timeout"
  | "provider_blocked"
  | "all_targets_exhausted"
  | "schema_violation"
  | "route_conflict"
  | "stream_failed";

export type AppErrorCode = WorkspaceErrorCode | PersistenceErrorCode;

export interface AppDomainError {
  code: AppErrorCode;
  message: string;
  details?: unknown;
  remediation?: string;
}

export interface WorkspaceChangedEvent {
  kind: string;
  relativePath: string;
  timestamp: string;
}

export interface Project {
  id: string;
  name: string;
  workspacePath: string;
  isArchived: boolean;
  createdAt: string;
  updatedAt: string;
  lastOpenedAt: string | null;
}

export interface CreateProjectRequest {
  workspacePath: string;
  name?: string;
}

export interface CreateProjectResponse {
  project: Project;
}

export interface ListProjectsRequest {
  includeArchived?: boolean;
}

export interface ListProjectsResponse {
  projects: Project[];
}

export interface OpenProjectRequest {
  projectId: string;
}

export interface OpenProjectResponse {
  project: Project;
}

export interface ArchiveProjectRequest {
  projectId: string;
}

export interface ArchiveProjectResponse {
  project: Project;
}

export interface EnsureProjectForWorkspaceRequest {
  workspacePath: string;
  preferredName?: string;
}

export interface EnsureProjectForWorkspaceResponse {
  project: Project;
}

export interface GetLastActiveProjectResponse {
  project: Project | null;
}

export type PulsationStage =
  | "Intake"
  | "SPECIFICATION"
  | "Contract"
  | "Code"
  | "Test"
  | "Integrator";

export type PulsationStatus =
  | "idle"
  | "planning"
  | "running"
  | "checkpoint"
  | "done"
  | "failed";

export type HitlDecision = "approve" | "request_revision" | "reject";

export interface Pulsation {
  id: string;
  projectId: string;
  featureName: string;
  stage: PulsationStage;
  status: PulsationStatus;
  createdAt: string;
  updatedAt: string;
}

export interface CreatePulsationRequest {
  projectId: string;
  featureName: string;
}

export interface CreatePulsationResponse {
  pulsation: Pulsation;
}

export interface ListPulsationsRequest {
  projectId: string;
}

export interface ListPulsationsResponse {
  pulsations: Pulsation[];
}

export interface PulsationRuntimeSnapshot {
  pulsations: Pulsation[];
  hitlWaitingCount: number;
  backgroundAgentsCount: number;
}

export interface GetPulsationRuntimeSnapshotRequest {
  projectId: string;
}

export interface GetPulsationRuntimeSnapshotResponse {
  snapshot: PulsationRuntimeSnapshot;
}

export interface ApplyHitlDecisionRequest {
  pulsationId: string;
  decision: HitlDecision;
}

export interface ApplyHitlDecisionResponse {
  pulsation: Pulsation;
}

export interface Artifact {
  id: string;
  pulsationId: string;
  name: string;
  kind: string;
  relativePath: string | null;
  contentSha256: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface ListPulsationArtifactsRequest {
  pulsationId: string;
}

export interface ListPulsationArtifactsResponse {
  artifacts: Artifact[];
}

export type WorkspaceViewMode = "editor" | "hitl";

export interface UiTabSnapshot {
  relativePath: string;
  draftContent?: string | null;
  baseRevision?: string | null;
  cursorLine?: number | null;
  cursorColumn?: number | null;
  scrollTop?: number | null;
}

export interface UiSessionState {
  projectId: string;
  workspaceRoot: string;
  activeTabPath: string | null;
  workspaceViewMode: WorkspaceViewMode;
  scrollTop?: number | null;
  tabs: UiTabSnapshot[];
  updatedAt: string;
}

export interface LoadUiSessionRequest {
  projectId: string;
}

export interface LoadUiSessionResponse {
  session: UiSessionState | null;
}

export interface SaveUiSessionRequest {
  projectId: string;
  workspaceRoot: string;
  activeTabPath: string | null;
  workspaceViewMode: WorkspaceViewMode;
  scrollTop?: number | null;
  tabs: UiTabSnapshot[];
}

export interface SaveUiSessionResponse {
  updatedAt: string;
}

export type LlmProvider = "openai" | "anthropic";
export type ProviderId =
  | "openai"
  | "anthropic"
  | "gemini"
  | "deepseek"
  | "groq"
  | "mistral"
  | "ollama";
export type ProviderProtocol = "openai_responses" | "openai_chat" | "anthropic_messages";
export type RoutingV2Mode = "off" | "shadow" | "on";
export type ContextStrategy = "yaml_graph" | "yaml_graph_rag";
export type EmbeddingProvider = "ollama";

export interface AgentRuntimeConfig {
  provider: LlmProvider;
  model: string;
  temperature: number;
  maxOutputTokens: number;
  timeoutMs: number;
  maxRetries: number;
  contextCharsLimit: number;
  maxContextFiles: number;
  contextStrategy: ContextStrategy;
  embeddingEnabled: boolean;
  embeddingProvider: EmbeddingProvider;
  embeddingModel: string;
  embeddingTopK: number;
  routingV2Mode: RoutingV2Mode;
}

export interface GetAgentRuntimeConfigResponse {
  config: AgentRuntimeConfig;
}

export interface UpdateAgentRuntimeConfigRequest {
  provider?: LlmProvider;
  model?: string;
  temperature?: number;
  maxOutputTokens?: number;
  timeoutMs?: number;
  maxRetries?: number;
  contextCharsLimit?: number;
  maxContextFiles?: number;
  contextStrategy?: ContextStrategy;
  embeddingEnabled?: boolean;
  embeddingProvider?: EmbeddingProvider;
  embeddingModel?: string;
  embeddingTopK?: number;
  routingV2Mode?: RoutingV2Mode;
}

export interface UpdateAgentRuntimeConfigResponse {
  config: AgentRuntimeConfig;
}

export interface SaveProviderApiKeyRequest {
  provider: LlmProvider;
  apiKey: string;
}

export interface SaveProviderApiKeyResponse {
  provider: LlmProvider;
  saved: boolean;
}

export interface ClearProviderApiKeyRequest {
  provider: LlmProvider;
}

export interface ClearProviderApiKeyResponse {
  provider: LlmProvider;
  removed: boolean;
}

export interface ProviderKeyStatus {
  provider: LlmProvider;
  hasApiKey: boolean;
}

export interface GetProviderKeyStatusesResponse {
  providers: ProviderKeyStatus[];
}

export interface CancelPulsationJobsRequest {
  pulsationId: string;
}

export interface CancelPulsationJobsResponse {
  cancelledJobs: number;
}

export interface ListAgentRunsRequest {
  pulsationId: string;
}

export interface ReindexWorkspaceEmbeddingsRequest {
  model?: string;
}

export interface ReindexWorkspaceEmbeddingsResponse {
  workspacePath: string;
  model: string;
  indexedCount: number;
  skippedCount: number;
  deletedCount: number;
  failedCount: number;
}

export interface AgentRunListItem {
  id: string;
  jobId: string;
  agentName: string;
  status: string;
  inputTokens: number | null;
  outputTokens: number | null;
  provider: string | null;
  model: string | null;
  estimatedCostUsd: number | null;
  lastError: string | null;
  startedAt: string | null;
  finishedAt: string | null;
  rawOutput: string | null;
  structuredOutput: Record<string, unknown> | null;
  createdAt: string;
  updatedAt: string;
}

export interface ListAgentRunsResponse {
  runs: AgentRunListItem[];
}

export interface ProviderCapabilities {
  streaming: boolean;
  jsonObject: boolean;
  jsonSchema: boolean;
  usage: boolean;
}

export interface ProviderProfile {
  id: string;
  displayName: string;
  protocol: ProviderProtocol;
  endpoint: string;
  endpointConfigurable: boolean;
  authStyle: "bearer" | "anthropic" | "none";
  capabilities: ProviderCapabilities;
  forbiddenParameters: string[];
  defaultModel: string;
  local: boolean;
}

export interface ProviderConnection {
  id: string;
  providerId: string;
  displayName: string;
  endpointUrl: string | null;
  defaultModel: string;
  enabled: boolean;
  state: string;
  blockedReason: string | null;
  cooldownUntil: string | null;
  maxConcurrency: number;
  version: number;
  hasApiKey: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface RouteTarget {
  position: number;
  connectionId: string;
  modelId: string;
  enabled: boolean;
}

export interface ProviderRoute {
  id: string;
  name: string;
  strategy: "priority" | "lkgp" | "score";
  maxAttempts: number;
  attemptTimeoutMs: number;
  routeTimeoutMs: number;
  enabled: boolean;
  version: number;
  targets: RouteTarget[];
  createdAt: string;
  updatedAt: string;
}

export interface ProviderRouteBinding {
  projectId: string;
  agentRole: string;
  stage: string;
  routeId: string;
  enabled: boolean;
  version: number;
  createdAt: string;
  updatedAt: string;
}

export interface ListProviderProfilesResponse { profiles: ProviderProfile[]; }
export interface ListProviderConnectionsResponse { connections: ProviderConnection[]; }
export interface ListProviderRoutesResponse { routes: ProviderRoute[]; }
export interface ListProviderRouteBindingsResponse { bindings: ProviderRouteBinding[]; }

export interface UpsertProviderConnectionRequest {
  id?: string;
  providerId: string;
  displayName: string;
  endpointUrl?: string | null;
  defaultModel: string;
  enabled?: boolean;
  maxConcurrency?: number;
  apiKey?: string;
  version?: number;
}
export interface UpsertProviderConnectionResponse { connection: ProviderConnection; }
export interface DeleteProviderConnectionRequest { connectionId: string; version: number; }
export interface DeleteProviderConnectionResponse { deleted: boolean; }
export interface TestProviderConnectionRequest { connectionId: string; }
export interface TestProviderConnectionResponse {
  connectionId: string;
  reachable: boolean;
  providerId: string;
  model: string;
  latencyMs: number;
  message: string;
}

export interface UpsertProviderRouteRequest {
  id?: string;
  name: string;
  strategy: "priority" | "lkgp" | "score";
  maxAttempts?: number;
  attemptTimeoutMs?: number;
  routeTimeoutMs?: number;
  enabled?: boolean;
  targets: RouteTarget[];
  version?: number;
}
export interface UpsertProviderRouteResponse { route: ProviderRoute; }
export interface DeleteProviderRouteRequest { routeId: string; version: number; }
export interface DeleteProviderRouteResponse { deleted: boolean; }
export interface UpsertProviderRouteBindingRequest {
  projectId: string;
  agentRole: string;
  stage: string;
  routeId: string;
  enabled?: boolean;
  version?: number;
}
export interface UpsertProviderRouteBindingResponse { binding: ProviderRouteBinding; }

export interface ResilienceStateItem {
  scope: string;
  scopeId: string;
  state: string;
  retryAt: string | null;
  reason: string | null;
}
export interface LlmResilienceSnapshot { generatedAt: string; states: ResilienceStateItem[]; }
export interface GetLlmResilienceSnapshotResponse { snapshot: LlmResilienceSnapshot; }
export interface ResetLlmResilienceStateRequest { connectionId?: string; providerId?: string; }
export interface ResetLlmResilienceStateResponse { resetCount: number; }

export interface LlmExecutionSnapshot {
  executionId: string;
  generationId: string;
  routeId: string;
  status: string;
  decision: Record<string, unknown> | null;
  nextEligibleAt: string | null;
  visibleOutput: string | null;
  visibleOutputComplete: boolean;
  attempts: Array<Record<string, unknown>>;
}
export interface GetLlmExecutionSnapshotRequest { executionId: string; }
export interface GetLlmExecutionSnapshotResponse { snapshot: LlmExecutionSnapshot; }

export interface LlmRoutingMetric {
  providerId: string;
  modelId: string;
  attempts: number;
  successes: number;
  averageLatencyMs: number | null;
  inputTokens: number;
  outputTokens: number;
  estimatedCostUsd: number | null;
}
export interface GetLlmRoutingMetricsResponse { metrics: LlmRoutingMetric[]; }

export type ContentKind = "tool_output" | "vault_prose" | "system_prompt" | "json" | "yaml" | "code" | "diff" | "adr" | "contract" | "requirement" | "specification" | "unknown";
export type CompressionEngine = "none" | "rtk" | "caveman";
export interface CompressionPreviewRequest { content: string; contentKind: ContentKind; engine: CompressionEngine; cavemanOptIn?: boolean; }
export interface CompressionPreview {
  engine: string;
  engineVersion: string;
  filterName: string | null;
  contentKind: ContentKind;
  originalBytes: number;
  compressedBytes: number;
  savedBytes: number;
  durationMs: number;
  applied: boolean;
  result: string;
  output: string;
  originalSha256: string;
}
export interface PreviewContextCompressionResponse { preview: CompressionPreview; }

export type LlmStreamEventKind = "route_started" | "attempt_started" | "token_delta" | "attempt_failed" | "route_completed" | "route_failed" | "resilience_changed";
export interface LlmEventEnvelope {
  protocolVersion: 1;
  executionId: string;
  generationId: string;
  sequence: number;
  kind: LlmStreamEventKind;
  routeId?: string;
  ordinal?: number;
  text?: string;
  errorCode?: string;
  nextEligibleAt?: string | null;
  providerId?: string;
  connectionId?: string;
  model?: string;
  scope?: string;
  scopeId?: string;
  fromState?: string;
  toState?: string;
  retryAt?: string | null;
}
