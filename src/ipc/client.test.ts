import { beforeEach, describe, expect, it } from "vitest";

import {
  deleteProviderConnection,
  deleteProviderRoute,
  getLlmExecutionSnapshot,
  getLlmResilienceSnapshot,
  getLlmRoutingMetrics,
  listProviderConnections,
  listProviderProfiles,
  listProviderRouteBindings,
  listProviderRoutes,
  normalizeWorkspaceError,
  openWorkspace,
  previewContextCompression,
  resetLlmResilienceState,
  sendChatMessage,
  cancelChatTurn,
  testProviderConnection,
  upsertProviderConnection,
  upsertProviderRoute,
  upsertProviderRouteBinding,
  WorkspaceClientError
} from "./client";
import {
  DELETE_PROVIDER_CONNECTION_COMMAND,
  DELETE_PROVIDER_ROUTE_COMMAND,
  GET_LLM_EXECUTION_SNAPSHOT_COMMAND,
  GET_LLM_RESILIENCE_SNAPSHOT_COMMAND,
  GET_LLM_ROUTING_METRICS_COMMAND,
  LIST_PROVIDER_CONNECTIONS_COMMAND,
  LIST_PROVIDER_PROFILES_COMMAND,
  LIST_PROVIDER_ROUTE_BINDINGS_COMMAND,
  LIST_PROVIDER_ROUTES_COMMAND,
  OPEN_WORKSPACE_COMMAND,
  PREVIEW_CONTEXT_COMPRESSION_COMMAND,
  RESET_LLM_RESILIENCE_STATE_COMMAND,
  TEST_PROVIDER_CONNECTION_COMMAND,
  UPSERT_PROVIDER_CONNECTION_COMMAND,
  UPSERT_PROVIDER_ROUTE_BINDING_COMMAND,
  UPSERT_PROVIDER_ROUTE_COMMAND,
  SEND_CHAT_MESSAGE_COMMAND,
  CANCEL_CHAT_TURN_COMMAND
} from "./contracts";
import {
  installTauriRuntime,
  tauriInvokeMock
} from "../test/mocks/tauriMock";

describe("IPC client", () => {
  beforeEach(() => {
    installTauriRuntime();
  });

  it("normalizes a serialized domain error", () => {
    expect(
      normalizeWorkspaceError('{"code":"conflict","message":"Revisão divergente."}')
    ).toEqual({
      code: "conflict",
      message: "Revisão divergente."
    });
  });

  it("falls back to io_error for a regular Error", () => {
    expect(normalizeWorkspaceError(new Error("Falha local."))).toEqual({
      code: "io_error",
      message: "Falha local."
    });
  });

  it("preserves enriched LLM error details and remediation", () => {
    expect(
      normalizeWorkspaceError({
        code: "missing_api_key",
        message: "Conexão sem API key.",
        details: { connectionId: "connection-1" },
        remediation: "Cadastre uma API key."
      })
    ).toEqual({
      code: "missing_api_key",
      message: "Conexão sem API key.",
      details: { connectionId: "connection-1" },
      remediation: "Cadastre uma API key."
    });
  });

  it("invokes open_workspace with the request envelope", async () => {
    tauriInvokeMock.mockResolvedValue({
      rootPath: "C:/workspace",
      tree: [],
      greenfield: {
        mode: "existing",
        initialized: false,
        message: "Workspace existente.",
        createdPaths: []
      }
    });

    await openWorkspace({ path: "C:/workspace" });

    expect(tauriInvokeMock).toHaveBeenCalledWith(OPEN_WORKSPACE_COMMAND, {
      request: { path: "C:/workspace" }
    });
  });

  it("starts and cancels chat turns through read-only chat IPC", async () => {
    tauriInvokeMock
      .mockResolvedValueOnce({
        executionId: "execution-1",
        generationId: "generation-1",
        userMessageId: "user-1",
        assistantMessageId: "assistant-1"
      })
      .mockResolvedValueOnce({ cancelled: true });

    await sendChatMessage({ conversationId: "conversation-1", content: "Analise a arquitetura" });
    expect(tauriInvokeMock).toHaveBeenCalledWith(SEND_CHAT_MESSAGE_COMMAND, {
      request: { conversationId: "conversation-1", content: "Analise a arquitetura" }
    });
    await cancelChatTurn({ executionId: "execution-1" });
    expect(tauriInvokeMock).toHaveBeenCalledWith(CANCEL_CHAT_TURN_COMMAND, {
      request: { executionId: "execution-1" }
    });
  });

  it("turns a backend domain error into WorkspaceClientError", async () => {
    tauriInvokeMock.mockRejectedValue({
      code: "invalid_root",
      message: "A pasta selecionada não existe."
    });

    const promise = openWorkspace({ path: "C:/missing" });

    await expect(promise).rejects.toMatchObject({
      name: "WorkspaceClientError",
      code: "invalid_root",
      message: "A pasta selecionada não existe."
    } satisfies Partial<WorkspaceClientError>);
  });

  it("sends a connection secret only in the write request", async () => {
    tauriInvokeMock.mockResolvedValue({
      connection: {
        id: "connection-1",
        providerId: "openai",
        displayName: "Primary",
        endpointUrl: null,
        defaultModel: "gpt-4.1-mini",
        enabled: true,
        state: "active",
        blockedReason: null,
        cooldownUntil: null,
        maxConcurrency: 1,
        version: 1,
        hasApiKey: true,
        createdAt: "2026-08-18T00:00:00Z",
        updatedAt: "2026-08-18T00:00:00Z"
      }
    });

    const response = await upsertProviderConnection({
      providerId: "openai",
      displayName: "Primary",
      defaultModel: "gpt-4.1-mini",
      apiKey: "write-only-secret"
    });

    expect(tauriInvokeMock).toHaveBeenCalledWith(UPSERT_PROVIDER_CONNECTION_COMMAND, {
      request: expect.objectContaining({ apiKey: "write-only-secret" })
    });
    expect(JSON.stringify(response)).not.toContain("write-only-secret");
    expect(JSON.stringify(response)).not.toContain("credentialRef");
  });

  it("lists provider profiles", async () => {
    tauriInvokeMock.mockResolvedValue({
      profiles: [
        {
          id: "openai",
          displayName: "OpenAI",
          protocol: "openai_responses",
          supportsStreaming: true
        }
      ]
    });

    const response = await listProviderProfiles();
    expect(tauriInvokeMock).toHaveBeenCalledWith(LIST_PROVIDER_PROFILES_COMMAND, undefined);
    expect(response.profiles).toHaveLength(1);
    expect(response.profiles[0].id).toBe("openai");
  });

  it("tests a provider connection and deletes it", async () => {
    tauriInvokeMock.mockResolvedValueOnce({
      connectionId: "conn-1",
      reachable: true,
      providerId: "openai",
      model: "gpt-4.1-mini",
      latencyMs: 142,
      message: "OK"
    });
    tauriInvokeMock.mockResolvedValueOnce({ deleted: true });

    const testRes = await testProviderConnection({ connectionId: "conn-1" });
    expect(tauriInvokeMock).toHaveBeenCalledWith(TEST_PROVIDER_CONNECTION_COMMAND, {
      request: { connectionId: "conn-1" }
    });
    expect(testRes.reachable).toBe(true);

    const delRes = await deleteProviderConnection({ connectionId: "conn-1", version: 1 });
    expect(tauriInvokeMock).toHaveBeenCalledWith(DELETE_PROVIDER_CONNECTION_COMMAND, {
      request: { connectionId: "conn-1", version: 1 }
    });
    expect(delRes.deleted).toBe(true);
  });

  it("manages routes and bindings", async () => {
    tauriInvokeMock.mockResolvedValueOnce({
      routes: [
        {
          id: "route-1",
          name: "Default Route",
          strategy: "priority",
          maxAttempts: 3,
          attemptTimeoutMs: 5000,
          routeTimeoutMs: 10000,
          enabled: true,
          version: 1,
          targets: []
        }
      ]
    });
    tauriInvokeMock.mockResolvedValueOnce({
      route: {
        id: "route-1",
        name: "Default Route Updated",
        strategy: "priority",
        maxAttempts: 3,
        attemptTimeoutMs: 5000,
        routeTimeoutMs: 10000,
        enabled: true,
        version: 2,
        targets: []
      }
    });
    tauriInvokeMock.mockResolvedValueOnce({ deleted: true });
    tauriInvokeMock.mockResolvedValueOnce({ bindings: [] });
    tauriInvokeMock.mockResolvedValueOnce({
      binding: {
        id: "b-1",
        projectId: "p-1",
        agentRole: "backend_agent",
        stage: "Code",
        routeId: "route-1",
        enabled: true,
        version: 1
      }
    });

    const routes = await listProviderRoutes();
    expect(tauriInvokeMock).toHaveBeenCalledWith(LIST_PROVIDER_ROUTES_COMMAND, undefined);
    expect(routes.routes).toHaveLength(1);

    const upsertRouteRes = await upsertProviderRoute({
      id: "route-1",
      name: "Default Route Updated",
      strategy: "priority",
      targets: []
    });
    expect(tauriInvokeMock).toHaveBeenCalledWith(UPSERT_PROVIDER_ROUTE_COMMAND, {
      request: {
        id: "route-1",
        name: "Default Route Updated",
        strategy: "priority",
        targets: []
      }
    });
    expect(upsertRouteRes.route.version).toBe(2);

    const delRouteRes = await deleteProviderRoute({ routeId: "route-1", version: 1 });
    expect(tauriInvokeMock).toHaveBeenCalledWith(DELETE_PROVIDER_ROUTE_COMMAND, {
      request: { routeId: "route-1", version: 1 }
    });
    expect(delRouteRes.deleted).toBe(true);

    const bindings = await listProviderRouteBindings();
    expect(tauriInvokeMock).toHaveBeenCalledWith(LIST_PROVIDER_ROUTE_BINDINGS_COMMAND, undefined);
    expect(bindings.bindings).toEqual([]);

    const upsertBindingRes = await upsertProviderRouteBinding({
      projectId: "p-1",
      agentRole: "backend_agent",
      stage: "Code",
      routeId: "route-1"
    });
    expect(tauriInvokeMock).toHaveBeenCalledWith(UPSERT_PROVIDER_ROUTE_BINDING_COMMAND, {
      request: {
        projectId: "p-1",
        agentRole: "backend_agent",
        stage: "Code",
        routeId: "route-1"
      }
    });
    expect(upsertBindingRes.binding.routeId).toBe("route-1");
  });

  it("handles resilience snapshots and reset", async () => {
    tauriInvokeMock.mockResolvedValueOnce({
      snapshot: {
        generatedAt: "2026-08-18T00:00:00Z",
        states: [
          {
            scope: "breaker",
            scopeId: "openai@https://api.openai.com",
            state: "open",
            retryAt: "2026-08-18T00:00:30Z",
            reason: null
          }
        ]
      }
    });
    tauriInvokeMock.mockResolvedValueOnce({ resetCount: 1 });

    const snapshot = await getLlmResilienceSnapshot();
    expect(tauriInvokeMock).toHaveBeenCalledWith(GET_LLM_RESILIENCE_SNAPSHOT_COMMAND, undefined);
    expect(snapshot.snapshot.states).toHaveLength(1);

    const resetRes = await resetLlmResilienceState({ providerId: "openai" });
    expect(tauriInvokeMock).toHaveBeenCalledWith(RESET_LLM_RESILIENCE_STATE_COMMAND, {
      request: { providerId: "openai" }
    });
    expect(resetRes.resetCount).toBe(1);
  });

  it("retrieves execution snapshot, metrics and compression preview", async () => {
    tauriInvokeMock.mockResolvedValueOnce({
      snapshot: {
        executionId: "exec-1",
        generationId: "gen-1",
        routeId: "route-1",
        status: "success",
        decision: {},
        nextEligibleAt: null,
        visibleOutput: "Result",
        visibleOutputComplete: true,
        attempts: []
      }
    });
    tauriInvokeMock.mockResolvedValueOnce({
      metrics: [
        {
          providerId: "openai",
          modelId: "gpt-4.1-mini",
          attempts: 10,
          successes: 10,
          averageLatencyMs: 120,
          inputTokens: 1000,
          outputTokens: 500,
          estimatedCostUsd: 0.005
        }
      ]
    });
    tauriInvokeMock.mockResolvedValueOnce({
      preview: {
        engine: "rtk",
        engineVersion: "1.0",
        filterName: "rtk_filter",
        contentKind: "code",
        originalBytes: 200,
        compressedBytes: 80,
        savedBytes: 120,
        durationMs: 5,
        applied: true,
        result: "minified",
        output: "minified",
        originalSha256: "hash"
      }
    });

    const exec = await getLlmExecutionSnapshot({ executionId: "exec-1" });
    expect(tauriInvokeMock).toHaveBeenCalledWith(GET_LLM_EXECUTION_SNAPSHOT_COMMAND, {
      request: { executionId: "exec-1" }
    });
    expect(exec.snapshot.status).toBe("success");

    const metrics = await getLlmRoutingMetrics();
    expect(tauriInvokeMock).toHaveBeenCalledWith(GET_LLM_ROUTING_METRICS_COMMAND, undefined);
    expect(metrics.metrics).toHaveLength(1);

    const preview = await previewContextCompression({
      content: "verbose code context",
      contentKind: "code",
      engine: "rtk"
    });
    expect(tauriInvokeMock).toHaveBeenCalledWith(PREVIEW_CONTEXT_COMPRESSION_COMMAND, {
      request: { content: "verbose code context", contentKind: "code", engine: "rtk" }
    });
    expect(preview.preview.savedBytes).toBe(120);
  });
});
