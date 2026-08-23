import { useMemo, useRef, useState, type FormEvent } from "react";

import type { ProviderConnection, ProviderRoute } from "../ipc/contracts";
import { useRuntimeStore } from "../state/runtimeStore";

const AGENT_ROLES = [
  "intake_router",
  "task_specifier",
  "contract_agent",
  "backend_agent",
  "test_agent",
  "integrator"
] as const;

const STAGES = ["Intake", "SPECIFICATION", "Contract", "Code", "Test", "Integrator"] as const;

interface LlmRoutingSettingsProps {
  projectId: string | null;
}

interface ConnectionDraft {
  id?: string;
  providerId: string;
  displayName: string;
  endpointUrl: string;
  defaultModel: string;
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

const EMPTY_CONNECTION: ConnectionDraft = {
  providerId: "openai",
  displayName: "",
  endpointUrl: "",
  defaultModel: "gpt-4.1-mini"
};

const EMPTY_ROUTE: RouteDraft = {
  name: "",
  strategy: "priority",
  selectedConnectionIds: [],
  models: {}
};

export default function LlmRoutingSettings({ projectId }: LlmRoutingSettingsProps) {
  const profiles = useRuntimeStore((state) => state.profiles);
  const connections = useRuntimeStore((state) => state.connections);
  const routes = useRuntimeStore((state) => state.routes);
  const bindings = useRuntimeStore((state) => state.bindings);
  const config = useRuntimeStore((state) => state.config);
  const resilienceStates = useRuntimeStore((state) => state.resilienceStates);
  const busy = useRuntimeStore((state) => state.busy);
  const saveConnection = useRuntimeStore((state) => state.saveConnection);
  const removeConnection = useRuntimeStore((state) => state.removeConnection);
  const testConnection = useRuntimeStore((state) => state.testConnection);
  const saveRoute = useRuntimeStore((state) => state.saveRoute);
  const removeRoute = useRuntimeStore((state) => state.removeRoute);
  const saveBinding = useRuntimeStore((state) => state.saveBinding);
  const setRoutingMode = useRuntimeStore((state) => state.setRoutingMode);
  const [connectionDraft, setConnectionDraft] = useState<ConnectionDraft>(EMPTY_CONNECTION);
  const [routeDraft, setRouteDraft] = useState<RouteDraft>(EMPTY_ROUTE);
  const [bindingRole, setBindingRole] = useState<string>("backend_agent");
  const [bindingStage, setBindingStage] = useState<string>("Code");
  const [bindingRouteId, setBindingRouteId] = useState<string>("legacy-default");
  const apiKeyRef = useRef<HTMLInputElement | null>(null);
  const selectedProfile = useMemo(
    () => profiles.find((profile) => profile.id === connectionDraft.providerId),
    [connectionDraft.providerId, profiles]
  );

  const resetConnectionDraft = () => {
    setConnectionDraft({ ...EMPTY_CONNECTION, defaultModel: profiles[0]?.defaultModel ?? "gpt-4.1-mini" });
    if (apiKeyRef.current) apiKeyRef.current.value = "";
  };

  const submitConnection = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const apiKey = apiKeyRef.current?.value.trim() || undefined;
    if (apiKeyRef.current) apiKeyRef.current.value = "";
    void saveConnection({
      id: connectionDraft.id,
      providerId: connectionDraft.providerId,
      displayName: connectionDraft.displayName,
      endpointUrl: connectionDraft.endpointUrl || undefined,
      defaultModel: connectionDraft.defaultModel,
      apiKey,
      enabled: true,
      maxConcurrency: 1,
      version: connectionDraft.version
    }).then(resetConnectionDraft).catch(() => undefined);
  };

  const submitRoute = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    void saveRoute({
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
        modelId: routeDraft.models[connectionId] || connections.find((item) => item.id === connectionId)?.defaultModel || "",
        enabled: true
      }))
    }).then(() => setRouteDraft(EMPTY_ROUTE)).catch(() => undefined);
  };

  const editConnection = (connection: ProviderConnection) => {
    setConnectionDraft({
      id: connection.id,
      providerId: connection.providerId,
      displayName: connection.displayName,
      endpointUrl: connection.endpointUrl ?? "",
      defaultModel: connection.defaultModel,
      version: connection.version
    });
    if (apiKeyRef.current) apiKeyRef.current.value = "";
  };

  const editRoute = (route: ProviderRoute) => setRouteDraft({
    id: route.id,
    name: route.name,
    strategy: route.strategy,
    selectedConnectionIds: route.targets.map((target) => target.connectionId),
    models: Object.fromEntries(route.targets.map((target) => [target.connectionId, target.modelId])),
    version: route.version
  });

  const moveTarget = (connectionId: string, direction: -1 | 1) => setRouteDraft((current) => {
    const values = [...current.selectedConnectionIds];
    const index = values.indexOf(connectionId);
    const next = index + direction;
    if (index < 0 || next < 0 || next >= values.length) return current;
    [values[index], values[next]] = [values[next], values[index]];
    return { ...current, selectedConnectionIds: values };
  });

  const persistBinding = () => {
    if (!projectId || !bindingRouteId) return;
    const current = bindings.find((binding) => binding.projectId === projectId && binding.agentRole === bindingRole && binding.stage === bindingStage);
    void saveBinding({
      projectId,
      agentRole: bindingRole,
      stage: bindingStage,
      routeId: bindingRouteId,
      enabled: true,
      version: current?.version
    });
  };

  return <div className="routing-settings">
    <section className="settings-card">
      <div className="section-heading"><div><span className="eyebrow">Decision engine</span><h2>Routing and resilience</h2></div><span className={`status-pill ${resilienceStates.length ? "tone-red" : "tone-green"}`}>{resilienceStates.length} resilience states</span></div>
      <label className="field"><span>Routing v2 mode</span><select aria-label="Routing v2 mode" value={config?.routingV2Mode ?? "off"} disabled={busy} onChange={(event) => void setRoutingMode(event.target.value as "off" | "shadow" | "on").catch(() => undefined)}><option value="off">Off — legacy</option><option value="shadow">Shadow — observe</option><option value="on">On — native router</option></select></label>
    </section>

    <section className="settings-card">
      <div className="section-heading"><div><span className="eyebrow">Providers</span><h2>Connections</h2></div><span className="count-chip">{connections.length}</span></div>
      <form className="settings-form" onSubmit={submitConnection}>
        <label className="field"><span>Provider</span><select aria-label="Provider" value={connectionDraft.providerId} disabled={Boolean(connectionDraft.id?.startsWith("legacy-"))} onChange={(event) => { const profile = profiles.find((item) => item.id === event.target.value); setConnectionDraft((current) => ({ ...current, providerId: event.target.value, defaultModel: profile?.defaultModel ?? current.defaultModel, displayName: current.displayName || profile?.displayName || "" })); }}>{profiles.filter((profile) => profile.id !== "openai_legacy" || connectionDraft.id === "legacy-openai").map((profile) => <option key={profile.id} value={profile.id}>{profile.displayName}</option>)}</select></label>
        <label className="field"><span>Connection name</span><input aria-label="Connection name" value={connectionDraft.displayName} onChange={(event) => setConnectionDraft((current) => ({ ...current, displayName: event.target.value }))} required /></label>
        <label className="field"><span>Default model</span><input aria-label="Default model" value={connectionDraft.defaultModel} onChange={(event) => setConnectionDraft((current) => ({ ...current, defaultModel: event.target.value }))} required /></label>
        {selectedProfile?.endpointConfigurable ? <label className="field"><span>Endpoint</span><input aria-label="Ollama endpoint" value={connectionDraft.endpointUrl} placeholder="http://127.0.0.1:11434" onChange={(event) => setConnectionDraft((current) => ({ ...current, endpointUrl: event.target.value }))} /></label> : null}
        {selectedProfile?.authStyle !== "none" ? <label className="field"><span>API key</span><input ref={apiKeyRef} aria-label="API key" type="password" autoComplete="new-password" placeholder={connectionDraft.id ? "New key (optional)" : "Stored directly in OS keyring"} /></label> : null}
        <div className="form-actions"><button type="submit" disabled={busy}>{connectionDraft.id ? "Update connection" : "Add connection"}</button>{connectionDraft.id ? <button type="button" className="button-secondary" onClick={resetConnectionDraft}>Cancel</button> : null}</div>
      </form>
      <div className="settings-list">{connections.map((connection) => <article key={connection.id}><div><strong>{connection.displayName}</strong><span>{connection.providerId} · {connection.defaultModel}</span></div><span className={`status-pill ${connection.hasApiKey && connection.state === "active" ? "tone-green" : "tone-red"}`}>{connection.hasApiKey ? connection.state : "Missing key"}</span><div className="row-actions"><button type="button" className="button-ghost" onClick={() => editConnection(connection)}>Edit</button><button type="button" className="button-secondary" disabled={busy} onClick={() => void testConnection(connection.id)}>Test</button>{!connection.id.startsWith("legacy-") ? <button type="button" className="button-danger" disabled={busy} onClick={() => void removeConnection(connection)}>Delete</button> : null}</div></article>)}</div>
    </section>

    <section className="settings-card">
      <div className="section-heading"><div><span className="eyebrow">Fallback order</span><h2>Routes</h2></div><span className="count-chip">{routes.length}</span></div>
      <form className="settings-form" onSubmit={submitRoute}>
        <label className="field"><span>Route name</span><input aria-label="Route name" value={routeDraft.name} onChange={(event) => setRouteDraft((current) => ({ ...current, name: event.target.value }))} required /></label>
        <label className="field"><span>Strategy</span><select value={routeDraft.strategy} onChange={(event) => setRouteDraft((current) => ({ ...current, strategy: event.target.value as RouteDraft["strategy"] }))}><option value="priority">Priority</option><option value="lkgp">LKGP</option><option value="score">Score</option></select></label>
        <div className="route-targets">{connections.map((connection) => { const selected = routeDraft.selectedConnectionIds.includes(connection.id); return <label key={connection.id}><input type="checkbox" checked={selected} onChange={() => setRouteDraft((current) => ({ ...current, selectedConnectionIds: selected ? current.selectedConnectionIds.filter((id) => id !== connection.id) : [...current.selectedConnectionIds, connection.id] }))} /><span>{connection.displayName}</span>{selected ? <input aria-label={`Model ${connection.displayName}`} value={routeDraft.models[connection.id] ?? connection.defaultModel} onChange={(event) => setRouteDraft((current) => ({ ...current, models: { ...current.models, [connection.id]: event.target.value } }))} /> : null}{selected ? <span className="move-actions"><button type="button" onClick={() => moveTarget(connection.id, -1)}>↑</button><button type="button" onClick={() => moveTarget(connection.id, 1)}>↓</button></span> : null}</label>; })}</div>
        <div className="form-actions"><button type="submit" disabled={busy || !routeDraft.selectedConnectionIds.length}>{routeDraft.id ? "Update route" : "Create route"}</button>{routeDraft.id ? <button type="button" className="button-secondary" onClick={() => setRouteDraft(EMPTY_ROUTE)}>Cancel</button> : null}</div>
      </form>
      <div className="settings-list">{routes.map((route) => <article key={route.id}><div><strong>{route.name}</strong><span>{route.strategy} · {route.targets.length} targets</span></div><div className="row-actions"><button type="button" className="button-ghost" onClick={() => editRoute(route)}>Edit</button>{route.id !== "legacy-default" ? <button type="button" className="button-danger" onClick={() => void removeRoute(route)}>Delete</button> : null}</div></article>)}</div>
    </section>

    <section className="settings-card">
      <div className="section-heading"><div><span className="eyebrow">Project overrides</span><h2>Exact bindings</h2></div><span className="count-chip">{bindings.filter((binding) => binding.projectId === projectId).length}</span></div>
      <div className="binding-form"><select aria-label="Agent role" value={bindingRole} onChange={(event) => setBindingRole(event.target.value)}>{AGENT_ROLES.map((role) => <option key={role}>{role}</option>)}</select><select aria-label="Stage" value={bindingStage} onChange={(event) => setBindingStage(event.target.value)}>{STAGES.map((stage) => <option key={stage}>{stage}</option>)}</select><select aria-label="Binding route" value={bindingRouteId} onChange={(event) => setBindingRouteId(event.target.value)}>{routes.map((route) => <option key={route.id} value={route.id}>{route.name}</option>)}</select><button type="button" disabled={!projectId || busy || !bindingRouteId} onClick={persistBinding}>Save binding</button></div>
      <div className="binding-matrix">{bindings.filter((binding) => binding.projectId === projectId).map((binding) => <div key={`${binding.projectId}:${binding.agentRole}:${binding.stage}`}><span>{binding.agentRole}</span><span>{binding.stage}</span><strong>{routes.find((route) => route.id === binding.routeId)?.name ?? binding.routeId}</strong><span>{binding.enabled ? "Active" : "Inactive"}</span></div>)}</div>
    </section>
  </div>;
}
