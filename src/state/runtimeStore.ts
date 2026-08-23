import { create } from "zustand";

import {
  deleteProviderConnection,
  deleteProviderRoute,
  getAgentRuntimeConfig,
  getLlmExecutionSnapshot,
  getLlmResilienceSnapshot,
  getLlmRoutingMetrics,
  getProviderKeyStatuses,
  listAgentRuns,
  listProviderConnections,
  listProviderProfiles,
  listProviderRouteBindings,
  listProviderRoutes,
  normalizeWorkspaceError,
  reindexWorkspaceEmbeddings,
  testProviderConnection,
  updateAgentRuntimeConfig,
  upsertProviderConnection,
  upsertProviderRoute,
  upsertProviderRouteBinding
} from "../ipc/client";
import type {
  AgentRunListItem,
  AgentRuntimeConfig,
  ContextStrategy,
  LlmEventEnvelope,
  LlmRoutingMetric,
  ProviderConnection,
  ProviderKeyStatus,
  ProviderProfile,
  ProviderRoute,
  ProviderRouteBinding,
  ResilienceStateItem,
  RoutingV2Mode,
  UpdateAgentRuntimeConfigRequest,
  UpsertProviderConnectionRequest,
  UpsertProviderRouteBindingRequest,
  UpsertProviderRouteRequest,
  WorkspaceChangedEvent
} from "../ipc/contracts";

export function classifyLlmEventSequence(
  previousSequence: number,
  nextSequence: number
): "stale" | "next" | "gap" {
  if (nextSequence <= previousSequence) return "stale";
  if (previousSequence > 0 && nextSequence > previousSequence + 1) return "gap";
  return "next";
}

interface RuntimeStoreState {
  config: AgentRuntimeConfig | null;
  profiles: ProviderProfile[];
  connections: ProviderConnection[];
  routes: ProviderRoute[];
  bindings: ProviderRouteBinding[];
  keyStatuses: ProviderKeyStatus[];
  resilienceStates: ResilienceStateItem[];
  agentRuns: AgentRunListItem[];
  routingMetrics: LlmRoutingMetric[];
  workspaceEvents: WorkspaceChangedEvent[];
  llmEvents: LlmEventEnvelope[];
  partialOutput: string;
  partialIncomplete: boolean;
  activeExecutionId: string | null;
  loading: boolean;
  busy: boolean;
  error: string | null;
  notice: string | null;
  refreshConfiguration: () => Promise<void>;
  refreshObservability: (pulsationId: string | null) => Promise<void>;
  updateConfig: (request: UpdateAgentRuntimeConfigRequest) => Promise<void>;
  setRoutingMode: (mode: RoutingV2Mode) => Promise<void>;
  setContextStrategy: (strategy: ContextStrategy) => Promise<void>;
  reindexEmbeddings: () => Promise<void>;
  saveConnection: (request: UpsertProviderConnectionRequest) => Promise<void>;
  removeConnection: (connection: ProviderConnection) => Promise<void>;
  testConnection: (connectionId: string) => Promise<void>;
  saveRoute: (request: UpsertProviderRouteRequest) => Promise<void>;
  removeRoute: (route: ProviderRoute) => Promise<void>;
  saveBinding: (request: UpsertProviderRouteBindingRequest) => Promise<void>;
  addWorkspaceEvent: (event: WorkspaceChangedEvent) => void;
  consumeLlmEvent: (event: LlmEventEnvelope) => Promise<void>;
  setNotice: (message: string | null) => void;
  resetTransientState: () => void;
}

const lastSequences = new Map<string, number>();

function errorMessage(cause: unknown): string {
  const error = normalizeWorkspaceError(cause);
  return error.remediation ? `${error.message} ${error.remediation}` : error.message;
}

export const useRuntimeStore = create<RuntimeStoreState>((set, get) => ({
  config: null,
  profiles: [],
  connections: [],
  routes: [],
  bindings: [],
  keyStatuses: [],
  resilienceStates: [],
  agentRuns: [],
  routingMetrics: [],
  workspaceEvents: [],
  llmEvents: [],
  partialOutput: "",
  partialIncomplete: false,
  activeExecutionId: null,
  loading: false,
  busy: false,
  error: null,
  notice: null,
  refreshConfiguration: async () => {
    set({ loading: true });
    try {
      const [
        configResponse,
        profileResponse,
        connectionResponse,
        routeResponse,
        bindingResponse,
        keyStatusResponse,
        resilienceResponse
      ] = await Promise.all([
        getAgentRuntimeConfig(),
        listProviderProfiles(),
        listProviderConnections(),
        listProviderRoutes(),
        listProviderRouteBindings(),
        getProviderKeyStatuses(),
        getLlmResilienceSnapshot()
      ]);
      set({
        config: configResponse.config,
        profiles: profileResponse.profiles,
        connections: connectionResponse.connections,
        routes: routeResponse.routes,
        bindings: bindingResponse.bindings,
        keyStatuses: keyStatusResponse.providers,
        resilienceStates: resilienceResponse.snapshot.states,
        error: null
      });
    } catch (cause) {
      set({ error: errorMessage(cause) });
    } finally {
      set({ loading: false });
    }
  },
  refreshObservability: async (pulsationId) => {
    try {
      const [runsResponse, metricsResponse] = await Promise.all([
        pulsationId ? listAgentRuns({ pulsationId }) : Promise.resolve({ runs: [] }),
        getLlmRoutingMetrics()
      ]);
      set({ agentRuns: runsResponse.runs, routingMetrics: metricsResponse.metrics, error: null });
    } catch (cause) {
      set({ error: errorMessage(cause) });
    }
  },
  updateConfig: async (request) => {
    set({ busy: true, notice: null });
    try {
      const response = await updateAgentRuntimeConfig(request);
      set({ config: response.config, notice: "Runtime configuration saved.", error: null });
    } catch (cause) {
      set({ error: errorMessage(cause) });
      throw cause;
    } finally {
      set({ busy: false });
    }
  },
  setRoutingMode: async (routingV2Mode) => {
    await get().updateConfig({ routingV2Mode });
    set({ notice: `Routing v2 mode: ${routingV2Mode}.` });
  },
  setContextStrategy: async (contextStrategy) => {
    await get().updateConfig({
      contextStrategy,
      embeddingEnabled: contextStrategy === "yaml_graph_rag"
    });
    set({ notice: `Context strategy: ${contextStrategy}.` });
  },
  reindexEmbeddings: async () => {
    set({ busy: true, notice: null });
    try {
      const response = await reindexWorkspaceEmbeddings();
      set({
        notice: `Embeddings reindexed (${response.model}): +${response.indexedCount}, =${response.skippedCount}, -${response.deletedCount}, failures=${response.failedCount}.`,
        error: null
      });
    } catch (cause) {
      set({ error: errorMessage(cause) });
    } finally {
      set({ busy: false });
    }
  },
  saveConnection: async (request) => {
    set({ busy: true, notice: null });
    try {
      await upsertProviderConnection(request);
      set({ notice: "Connection saved. Credentials were sent directly to the OS keyring." });
      await get().refreshConfiguration();
    } catch (cause) {
      set({ error: errorMessage(cause) });
      throw cause;
    } finally {
      set({ busy: false });
    }
  },
  removeConnection: async (connection) => {
    set({ busy: true, notice: null });
    try {
      await deleteProviderConnection({ connectionId: connection.id, version: connection.version });
      set({ notice: `Connection “${connection.displayName}” removed.` });
      await get().refreshConfiguration();
    } catch (cause) {
      set({ error: errorMessage(cause) });
    } finally {
      set({ busy: false });
    }
  },
  testConnection: async (connectionId) => {
    set({ busy: true, notice: null });
    try {
      const response = await testProviderConnection({ connectionId });
      set({ notice: `${response.message} ${response.latencyMs}ms`, error: null });
    } catch (cause) {
      set({ error: errorMessage(cause) });
    } finally {
      set({ busy: false });
    }
  },
  saveRoute: async (request) => {
    set({ busy: true, notice: null });
    try {
      await upsertProviderRoute(request);
      set({ notice: "Route saved with optimistic version control." });
      await get().refreshConfiguration();
    } catch (cause) {
      set({ error: errorMessage(cause) });
      throw cause;
    } finally {
      set({ busy: false });
    }
  },
  removeRoute: async (route) => {
    set({ busy: true, notice: null });
    try {
      await deleteProviderRoute({ routeId: route.id, version: route.version });
      set({ notice: `Route “${route.name}” removed.` });
      await get().refreshConfiguration();
    } catch (cause) {
      set({ error: errorMessage(cause) });
    } finally {
      set({ busy: false });
    }
  },
  saveBinding: async (request) => {
    set({ busy: true, notice: null });
    try {
      await upsertProviderRouteBinding(request);
      set({ notice: "Project, agent, and stage binding saved." });
      await get().refreshConfiguration();
    } catch (cause) {
      set({ error: errorMessage(cause) });
    } finally {
      set({ busy: false });
    }
  },
  addWorkspaceEvent: (event) =>
    set((state) => ({ workspaceEvents: [event, ...state.workspaceEvents].slice(0, 200) })),
  consumeLlmEvent: async (event) => {
    const previous = lastSequences.get(event.generationId) ?? 0;
    const sequenceState = classifyLlmEventSequence(previous, event.sequence);
    if (sequenceState === "stale") return;
    if (sequenceState === "gap") {
      try {
        const response = await getLlmExecutionSnapshot({ executionId: event.executionId });
        set({
          partialOutput: response.snapshot.visibleOutput ?? "",
          partialIncomplete: !response.snapshot.visibleOutputComplete
        });
      } catch {
        // A later event or explicit snapshot can reconcile the visible stream.
      }
    }
    lastSequences.set(event.generationId, event.sequence);
    set((state) => ({
      activeExecutionId: event.executionId,
      llmEvents: [event, ...state.llmEvents].slice(0, 200),
      partialOutput:
        event.kind === "route_started"
          ? ""
          : event.kind === "token_delta" && event.text
            ? state.partialOutput + event.text
            : state.partialOutput,
      partialIncomplete:
        event.kind === "route_started" || event.kind === "route_completed"
          ? false
          : event.kind === "route_failed"
            ? true
            : state.partialIncomplete
    }));
    if (event.kind === "resilience_changed") void get().refreshConfiguration();
  },
  setNotice: (notice) => set({ notice }),
  resetTransientState: () => {
    lastSequences.clear();
    set({
      agentRuns: [],
      workspaceEvents: [],
      llmEvents: [],
      partialOutput: "",
      partialIncomplete: false,
      activeExecutionId: null,
      error: null,
      notice: null
    });
  }
}));
