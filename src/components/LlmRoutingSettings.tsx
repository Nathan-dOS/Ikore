import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  LLM_ATTEMPT_FAILED_EVENT,
  LLM_ATTEMPT_STARTED_EVENT,
  LLM_ROUTE_COMPLETED_EVENT,
  LLM_ROUTE_FAILED_EVENT,
  LLM_ROUTE_STARTED_EVENT,
  LLM_RESILIENCE_CHANGED_EVENT,
  LLM_TOKEN_DELTA_EVENT,
  type AgentRuntimeConfig,
  type LlmEventEnvelope,
  type ProviderConnection,
  type ProviderProfile,
  type ProviderRoute,
  type ProviderRouteBinding,
  type RoutingV2Mode
} from "../ipc/contracts";
import {
  deleteProviderConnection,
  deleteProviderRoute,
  getLlmExecutionSnapshot,
  getLlmResilienceSnapshot,
  listProviderConnections,
  listProviderProfiles,
  listProviderRouteBindings,
  listProviderRoutes,
  normalizeWorkspaceError,
  testProviderConnection,
  updateAgentRuntimeConfig,
  upsertProviderConnection,
  upsertProviderRoute,
  upsertProviderRouteBinding
} from "../ipc/client";

const AGENT_ROLES = [
  "intake_router",
  "task_specifier",
  "contract_agent",
  "backend_agent",
  "test_agent",
  "integrator"
] as const;

const STAGES = ["Intake", "SPECIFICATION", "Contract", "Code", "Test", "Integrator"] as const;

interface Props {
  projectId: string | null;
  enabled: boolean;
  runtimeConfig: AgentRuntimeConfig | null;
  onRuntimeConfig: (config: AgentRuntimeConfig) => void;
  onNotice: (message: string) => void;
  onError: (message: string) => void;
  onRetryGeneration: () => void;
}

interface ConnectionDraft {
  id?: string;
  providerId: string;
  displayName: string;
  endpointUrl: string;
  defaultModel: string;
  apiKey: string;
  version?: number;
}

interface RouteDraft {
  id?: string;
  name: string;
  strategy: "priority" | "lkgp" | "score";
  selectedConnectionIds: string[];
  models: Record<string, string>;
  version?: number;
}

const emptyConnection: ConnectionDraft = {
  providerId: "openai",
  displayName: "",
  endpointUrl: "",
  defaultModel: "gpt-4.1-mini",
  apiKey: ""
};

const emptyRoute: RouteDraft = {
  name: "",
  strategy: "priority",
  selectedConnectionIds: [],
  models: {}
};

export function classifyLlmEventSequence(
  previousSequence: number,
  nextSequence: number
): "stale" | "next" | "gap" {
  if (nextSequence <= previousSequence) return "stale";
  if (previousSequence > 0 && nextSequence > previousSequence + 1) return "gap";
  return "next";
}

export default function LlmRoutingSettings({
  projectId,
  enabled,
  runtimeConfig,
  onRuntimeConfig,
  onNotice,
  onError,
  onRetryGeneration
}: Props) {
  const [profiles, setProfiles] = useState<ProviderProfile[]>([]);
  const [connections, setConnections] = useState<ProviderConnection[]>([]);
  const [routes, setRoutes] = useState<ProviderRoute[]>([]);
  const [bindings, setBindings] = useState<ProviderRouteBinding[]>([]);
  const [connectionDraft, setConnectionDraft] = useState<ConnectionDraft>(emptyConnection);
  const [routeDraft, setRouteDraft] = useState<RouteDraft>(emptyRoute);
  const [bindingRole, setBindingRole] = useState<string>("backend_agent");
  const [bindingStage, setBindingStage] = useState<string>("Code");
  const [bindingRouteId, setBindingRouteId] = useState<string>("legacy-default");
  const [resilienceCount, setResilienceCount] = useState(0);
  const [busy, setBusy] = useState(false);
  const [partialOutput, setPartialOutput] = useState("");
  const [partialIncomplete, setPartialIncomplete] = useState(false);
  const [activeExecutionId, setActiveExecutionId] = useState<string | null>(null);
  const lastSequence = useRef<Record<string, number>>({});

  const selectedProfile = useMemo(
    () => profiles.find((profile) => profile.id === connectionDraft.providerId),
    [connectionDraft.providerId, profiles]
  );

  const reportError = useCallback(
    (cause: unknown) => {
      const error = normalizeWorkspaceError(cause);
      onError(error.remediation ? `${error.message} ${error.remediation}` : error.message);
    },
    [onError]
  );

  const refresh = useCallback(async () => {
    if (!enabled) return;
    try {
      const [profileResponse, connectionResponse, routeResponse, bindingResponse, resilienceResponse] =
        await Promise.all([
          listProviderProfiles(),
          listProviderConnections(),
          listProviderRoutes(),
          listProviderRouteBindings(),
          getLlmResilienceSnapshot()
        ]);
      setProfiles(profileResponse.profiles);
      setConnections(connectionResponse.connections);
      setRoutes(routeResponse.routes);
      setBindings(bindingResponse.bindings);
      setResilienceCount(resilienceResponse.snapshot.states.length);
      if (!bindingRouteId && routeResponse.routes[0]) setBindingRouteId(routeResponse.routes[0].id);
    } catch (cause) {
      reportError(cause);
    }
  }, [bindingRouteId, enabled, reportError]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!enabled) return;
    const eventNames = [
      LLM_ROUTE_STARTED_EVENT,
      LLM_ATTEMPT_STARTED_EVENT,
      LLM_TOKEN_DELTA_EVENT,
      LLM_ATTEMPT_FAILED_EVENT,
      LLM_ROUTE_COMPLETED_EVENT,
      LLM_ROUTE_FAILED_EVENT,
      LLM_RESILIENCE_CHANGED_EVENT
    ];
    const unlisteners: UnlistenFn[] = [];
    let disposed = false;
    const consume = async (payload: LlmEventEnvelope) => {
      const previous = lastSequence.current[payload.generationId] ?? 0;
      const sequenceState = classifyLlmEventSequence(previous, payload.sequence);
      if (sequenceState === "stale") return;
      if (sequenceState === "gap") {
        try {
          const response = await getLlmExecutionSnapshot({ executionId: payload.executionId });
          setPartialOutput(response.snapshot.visibleOutput ?? "");
          setPartialIncomplete(!response.snapshot.visibleOutputComplete);
        } catch {
          // O próximo evento/snapshot explícito pode recuperar a visualização.
        }
      }
      lastSequence.current[payload.generationId] = payload.sequence;
      setActiveExecutionId(payload.executionId);
      if (payload.kind === "route_started") {
        setPartialOutput("");
        setPartialIncomplete(false);
      } else if (payload.kind === "token_delta" && payload.text) {
        setPartialOutput((current) => current + payload.text);
      } else if (payload.kind === "route_completed") {
        setPartialIncomplete(false);
      } else if (payload.kind === "route_failed") {
        setPartialIncomplete(true);
      } else if (payload.kind === "resilience_changed") {
        void getLlmResilienceSnapshot()
          .then((response) => setResilienceCount(response.snapshot.states.length))
          .catch(() => undefined);
      }
    };
    void Promise.all(
      eventNames.map(async (name) => {
        const unlisten = await listen<LlmEventEnvelope>(name, (event) => void consume(event.payload));
        if (disposed) void unlisten();
        else unlisteners.push(unlisten);
      })
    );
    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => void unlisten());
    };
  }, [enabled]);

  const handleMode = async (mode: RoutingV2Mode) => {
    setBusy(true);
    try {
      const response = await updateAgentRuntimeConfig({ routingV2Mode: mode });
      onRuntimeConfig(response.config);
      onNotice(`Roteamento v2: ${mode}.`);
    } catch (cause) {
      reportError(cause);
    } finally {
      setBusy(false);
    }
  };

  const handleConnectionSubmit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    try {
      await upsertProviderConnection({
        id: connectionDraft.id,
        providerId: connectionDraft.providerId,
        displayName: connectionDraft.displayName,
        endpointUrl: connectionDraft.endpointUrl || undefined,
        defaultModel: connectionDraft.defaultModel,
        apiKey: connectionDraft.apiKey || undefined,
        enabled: true,
        maxConcurrency: 1,
        version: connectionDraft.version
      });
      setConnectionDraft({ ...emptyConnection, defaultModel: profiles[0]?.defaultModel ?? "gpt-4.1-mini" });
      onNotice("Conexão salva; a API key permaneceu somente no keyring.");
      await refresh();
    } catch (cause) {
      setConnectionDraft((current) => ({ ...current, apiKey: "" }));
      reportError(cause);
    } finally {
      setBusy(false);
    }
  };

  const handleRouteSubmit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    try {
      await upsertProviderRoute({
        id: routeDraft.id,
        name: routeDraft.name,
        strategy: routeDraft.strategy,
        maxAttempts: 3,
        attemptTimeoutMs: 30_000,
        routeTimeoutMs: 45_000,
        enabled: true,
        version: routeDraft.version,
        targets: routeDraft.selectedConnectionIds.map((connectionId, position) => ({
          position,
          connectionId,
          modelId:
            routeDraft.models[connectionId] ||
            connections.find((connection) => connection.id === connectionId)?.defaultModel ||
            "",
          enabled: true
        }))
      });
      setRouteDraft(emptyRoute);
      onNotice("Rota salva com controle otimista de versão.");
      await refresh();
    } catch (cause) {
      reportError(cause);
    } finally {
      setBusy(false);
    }
  };

  const editConnection = (connection: ProviderConnection) => {
    setConnectionDraft({
      id: connection.id,
      providerId: connection.providerId,
      displayName: connection.displayName,
      endpointUrl: connection.endpointUrl ?? "",
      defaultModel: connection.defaultModel,
      apiKey: "",
      version: connection.version
    });
  };

  const editRoute = (route: ProviderRoute) => {
    setRouteDraft({
      id: route.id,
      name: route.name,
      strategy: route.strategy,
      selectedConnectionIds: route.targets.map((target) => target.connectionId),
      models: Object.fromEntries(route.targets.map((target) => [target.connectionId, target.modelId])),
      version: route.version
    });
  };

  const moveTarget = (connectionId: string, direction: -1 | 1) => {
    setRouteDraft((current) => {
      const items = [...current.selectedConnectionIds];
      const index = items.indexOf(connectionId);
      const next = index + direction;
      if (index < 0 || next < 0 || next >= items.length) return current;
      [items[index], items[next]] = [items[next], items[index]];
      return { ...current, selectedConnectionIds: items };
    });
  };

  const saveBinding = async () => {
    if (!projectId) return;
    setBusy(true);
    try {
      const current = bindings.find(
        (binding) =>
          binding.projectId === projectId && binding.agentRole === bindingRole && binding.stage === bindingStage
      );
      await upsertProviderRouteBinding({
        projectId,
        agentRole: bindingRole,
        stage: bindingStage,
        routeId: bindingRouteId,
        enabled: true,
        version: current?.version
      });
      onNotice("Vínculo exato projeto/agente/etapa salvo.");
      await refresh();
    } catch (cause) {
      reportError(cause);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="llm-routing-settings">
      <div className="workspace-box">
        <span className="label">Routing v2 / Resilience</span>
        <div className="llm-inline-row">
          <select
            aria-label="Routing v2 mode"
            value={runtimeConfig?.routingV2Mode ?? "off"}
            disabled={!enabled || busy}
            onChange={(event) => void handleMode(event.target.value as RoutingV2Mode)}
          >
            <option value="off">off — legado</option>
            <option value="shadow">shadow — decisão</option>
            <option value="on">on — router</option>
          </select>
          <span className={`badge-pill ${resilienceCount > 0 ? "pill-red" : "pill-green"}`}>
            {resilienceCount} estados
          </span>
        </div>
      </div>

      <details>
        <summary>Conexões ({connections.length})</summary>
        <form className="llm-config-form" onSubmit={handleConnectionSubmit}>
          <select
            aria-label="Provider"
            value={connectionDraft.providerId}
            disabled={Boolean(connectionDraft.id?.startsWith("legacy-"))}
            onChange={(event) => {
              const profile = profiles.find((item) => item.id === event.target.value);
              setConnectionDraft((current) => ({
                ...current,
                providerId: event.target.value,
                defaultModel: profile?.defaultModel ?? current.defaultModel,
                displayName: current.displayName || profile?.displayName || ""
              }));
            }}
          >
            {profiles
              .filter((profile) => profile.id !== "openai_legacy" || connectionDraft.id === "legacy-openai")
              .map((profile) => <option key={profile.id} value={profile.id}>{profile.displayName}</option>)}
          </select>
          <input aria-label="Connection name" placeholder="Nome da conexão" value={connectionDraft.displayName} onChange={(event) => setConnectionDraft((current) => ({ ...current, displayName: event.target.value }))} required />
          <input aria-label="Default model" placeholder="Modelo" value={connectionDraft.defaultModel} onChange={(event) => setConnectionDraft((current) => ({ ...current, defaultModel: event.target.value }))} required />
          {selectedProfile?.endpointConfigurable ? (
            <input aria-label="Ollama endpoint" placeholder="http://127.0.0.1:11434" value={connectionDraft.endpointUrl} onChange={(event) => setConnectionDraft((current) => ({ ...current, endpointUrl: event.target.value }))} />
          ) : null}
          {selectedProfile?.authStyle !== "none" ? (
            <input aria-label="API key" type="password" autoComplete="new-password" placeholder={connectionDraft.id ? "Nova key (opcional)" : "API key"} value={connectionDraft.apiKey} onChange={(event) => setConnectionDraft((current) => ({ ...current, apiKey: event.target.value }))} />
          ) : null}
          <div className="actions">
            <button type="submit" disabled={busy}>{connectionDraft.id ? "Atualizar" : "Adicionar"}</button>
            {connectionDraft.id ? <button type="button" onClick={() => setConnectionDraft(emptyConnection)}>Cancelar</button> : null}
          </div>
        </form>
        <ul className="events llm-config-list">
          {connections.map((connection) => (
            <li key={connection.id}>
              <span><strong>{connection.displayName}</strong><small>{connection.providerId} · {connection.defaultModel}</small></span>
              <span className={`badge-pill ${connection.hasApiKey && connection.state === "active" ? "pill-green" : "pill-red"}`}>{connection.hasApiKey ? connection.state : "missing key"}</span>
              <div className="actions">
                <button type="button" disabled={busy} onClick={() => editConnection(connection)}>Editar</button>
                <button type="button" disabled={busy} onClick={async () => { try { const result = await testProviderConnection({ connectionId: connection.id }); onNotice(`${result.message} ${result.latencyMs}ms`); } catch (cause) { reportError(cause); } }}>Testar</button>
                {!connection.id.startsWith("legacy-") ? <button type="button" disabled={busy} onClick={async () => { try { await deleteProviderConnection({ connectionId: connection.id, version: connection.version }); await refresh(); } catch (cause) { reportError(cause); } }}>Excluir</button> : null}
              </div>
            </li>
          ))}
        </ul>
      </details>

      <details>
        <summary>Rotas ({routes.length})</summary>
        <form className="llm-config-form" onSubmit={handleRouteSubmit}>
          <input aria-label="Route name" placeholder="Nome da rota" value={routeDraft.name} onChange={(event) => setRouteDraft((current) => ({ ...current, name: event.target.value }))} required />
          <select value={routeDraft.strategy} onChange={(event) => setRouteDraft((current) => ({ ...current, strategy: event.target.value as RouteDraft["strategy"] }))}>
            <option value="priority">priority</option><option value="lkgp">LKGP</option><option value="score">score</option>
          </select>
          <div className="llm-target-picker">
            {connections.map((connection) => {
              const selected = routeDraft.selectedConnectionIds.includes(connection.id);
              return <label key={connection.id}>
                <input type="checkbox" checked={selected} onChange={() => setRouteDraft((current) => ({ ...current, selectedConnectionIds: selected ? current.selectedConnectionIds.filter((id) => id !== connection.id) : [...current.selectedConnectionIds, connection.id] }))} />
                {connection.displayName}
                {selected ? <input aria-label={`Model ${connection.displayName}`} value={routeDraft.models[connection.id] ?? connection.defaultModel} onChange={(event) => setRouteDraft((current) => ({ ...current, models: { ...current.models, [connection.id]: event.target.value } }))} /> : null}
                {selected ? <span className="actions"><button type="button" onClick={() => moveTarget(connection.id, -1)}>↑</button><button type="button" onClick={() => moveTarget(connection.id, 1)}>↓</button></span> : null}
              </label>;
            })}
          </div>
          <div className="actions"><button type="submit" disabled={busy || routeDraft.selectedConnectionIds.length === 0}>{routeDraft.id ? "Atualizar rota" : "Criar rota"}</button>{routeDraft.id ? <button type="button" onClick={() => setRouteDraft(emptyRoute)}>Cancelar</button> : null}</div>
        </form>
        <ul className="events llm-config-list">
          {routes.map((route) => <li key={route.id}><span><strong>{route.name}</strong><small>{route.strategy} · {route.targets.length} alvos</small></span><div className="actions"><button type="button" onClick={() => editRoute(route)}>Editar</button>{route.id !== "legacy-default" ? <button type="button" onClick={async () => { try { await deleteProviderRoute({ routeId: route.id, version: route.version }); await refresh(); } catch (cause) { reportError(cause); } }}>Excluir</button> : null}</div></li>)}
        </ul>
      </details>

      <details>
        <summary>Vínculos exatos ({bindings.length})</summary>
        <div className="llm-config-form">
          <select value={bindingRole} onChange={(event) => setBindingRole(event.target.value)}>{AGENT_ROLES.map((role) => <option key={role}>{role}</option>)}</select>
          <select value={bindingStage} onChange={(event) => setBindingStage(event.target.value)}>{STAGES.map((stage) => <option key={stage}>{stage}</option>)}</select>
          <select value={bindingRouteId} onChange={(event) => setBindingRouteId(event.target.value)}>{routes.map((route) => <option key={route.id} value={route.id}>{route.name}</option>)}</select>
          <button type="button" disabled={!projectId || busy} onClick={() => void saveBinding()}>Salvar vínculo</button>
        </div>
        <div className="llm-items" aria-label="Matriz de vínculos do projeto">
          {bindings
            .filter((binding) => binding.projectId === projectId)
            .map((binding) => (
              <div className="llm-item" key={`${binding.projectId}:${binding.agentRole}:${binding.stage}`}>
                <span>{binding.agentRole}</span>
                <span>{binding.stage}</span>
                <strong>{routes.find((route) => route.id === binding.routeId)?.name ?? binding.routeId}</strong>
                <span>{binding.enabled ? "ativo" : "inativo"}</span>
              </div>
            ))}
        </div>
      </details>

      {activeExecutionId && partialOutput ? (
        <div className={`llm-partial-output ${partialIncomplete ? "is-incomplete" : ""}`}>
          <span className="label">Geração {partialIncomplete ? "incompleta" : "em andamento"}</span>
          <pre>{partialOutput}</pre>
          {partialIncomplete ? <button type="button" onClick={onRetryGeneration}>Retry explícito</button> : null}
        </div>
      ) : null}
    </div>
  );
}
