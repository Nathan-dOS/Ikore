import type { FormEvent } from "react";

import LlmRoutingSettings from "../components/LlmRoutingSettings";
import { usePulsationStore } from "../state/pulsationStore";
import { useRuntimeStore } from "../state/runtimeStore";
import { useVaultStore } from "../state/vaultStore";

export function SettingsView() {
  const project = usePulsationStore((state) => state.activeProject);
  const root = useVaultStore((state) => state.workspaceRoot);
  const config = useRuntimeStore((state) => state.config);
  const keyStatuses = useRuntimeStore((state) => state.keyStatuses);
  const busy = useRuntimeStore((state) => state.busy);
  const error = useRuntimeStore((state) => state.error);
  const notice = useRuntimeStore((state) => state.notice);
  const setContextStrategy = useRuntimeStore((state) => state.setContextStrategy);
  const reindexEmbeddings = useRuntimeStore((state) => state.reindexEmbeddings);
  const updateConfig = useRuntimeStore((state) => state.updateConfig);
  const saveEmbedding = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const formData = new FormData(event.currentTarget);
    const embeddingModel = String(formData.get("embeddingModel") ?? "").trim();
    const embeddingTopK = Number(formData.get("embeddingTopK"));
    if (embeddingModel && Number.isFinite(embeddingTopK)) {
      void updateConfig({ embeddingModel, embeddingTopK }).catch(() => undefined);
    }
  };

  return <div className="settings-view">
    <section className="settings-overview panel">
      <div className="section-heading"><div><span className="eyebrow">Retrieval controls</span><h2>Context and embeddings</h2></div><span className={`status-pill ${config?.embeddingEnabled ? "tone-green" : "tone-cyan"}`}>{config?.embeddingEnabled ? "RAG enabled" : "Graph only"}</span></div>
      <div className="strategy-picker">
        <button type="button" aria-pressed={config?.contextStrategy === "yaml_graph"} disabled={busy} onClick={() => void setContextStrategy("yaml_graph").catch(() => undefined)}><strong>YAML graph</strong><span>Use structured properties and wikilinks only.</span></button>
        <button type="button" aria-pressed={config?.contextStrategy === "yaml_graph_rag"} disabled={busy} onClick={() => void setContextStrategy("yaml_graph_rag").catch(() => undefined)}><strong>YAML graph + RAG</strong><span>Refine graph candidates with vector similarity.</span></button>
      </div>
      <form className="embedding-form" onSubmit={saveEmbedding}>
        <label className="field"><span>Embedding model</span><input key={config?.embeddingModel} name="embeddingModel" defaultValue={config?.embeddingModel ?? ""} required /></label>
        <label className="field"><span>Top K</span><input key={config?.embeddingTopK} name="embeddingTopK" type="number" min={1} max={100} defaultValue={config?.embeddingTopK ?? 8} /></label>
        <button type="submit" disabled={busy || !config}>Save embedding config</button>
        <button type="button" className="button-secondary" disabled={busy || !root} onClick={() => void reindexEmbeddings()}>Reindex workspace</button>
      </form>
      <div className="keyring-status"><span className="eyebrow">OS keyring</span>{keyStatuses.map((status) => <div key={status.provider}><strong>{status.provider}</strong><span className={`status-pill ${status.hasApiKey ? "tone-green" : "tone-red"}`}>{status.hasApiKey ? "Credential stored" : "Missing credential"}</span></div>)}</div>
      {notice ? <p className="inline-notice">{notice}</p> : null}{error ? <p className="inline-error">{error}</p> : null}
    </section>
    <LlmRoutingSettings projectId={project?.id ?? null} />
  </div>;
}
