import { useMemo, useState } from "react";

import { EmptyState } from "../components/EmptyState";
import { usePulsationStore } from "../state/pulsationStore";
import { useRuntimeStore } from "../state/runtimeStore";
import { formatDateTime, formatUsd } from "../utils/formatters";

export function LogsView() {
  const project = usePulsationStore((state) => state.activeProject);
  const pulsations = usePulsationStore((state) => state.pulsations);
  const activePulsationId = usePulsationStore((state) => state.activePulsationId);
  const setActivePulsation = usePulsationStore((state) => state.setActivePulsation);
  const retryActiveGeneration = usePulsationStore((state) => state.retryActiveGeneration);
  const runs = useRuntimeStore((state) => state.agentRuns);
  const metrics = useRuntimeStore((state) => state.routingMetrics);
  const workspaceEvents = useRuntimeStore((state) => state.workspaceEvents);
  const llmEvents = useRuntimeStore((state) => state.llmEvents);
  const partialOutput = useRuntimeStore((state) => state.partialOutput);
  const partialIncomplete = useRuntimeStore((state) => state.partialIncomplete);
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);
  const selectedRun = useMemo(
    () => runs.find((run) => run.id === selectedRunId) ?? runs[0] ?? null,
    [runs, selectedRunId]
  );

  if (!project) return <EmptyState title="No observability context" description="Open a project to inspect agent runs and live runtime events." />;

  return <div className="logs-view">
    <section className="panel runs-panel">
      <div className="section-heading"><div><span className="eyebrow">Persisted audit</span><h2>Agent runs</h2></div><select aria-label="Observed pulsation" value={activePulsationId ?? ""} onChange={(event) => setActivePulsation(event.target.value)}>{pulsations.map((pulsation) => <option value={pulsation.id} key={pulsation.id}>{pulsation.featureName}</option>)}</select></div>
      {runs.length === 0 ? <p className="muted">No agent runs recorded for this pulsation.</p> : <div className="table-scroll"><table><thead><tr><th>Agent</th><th>Status</th><th>Provider / model</th><th>Tokens in/out</th><th>Cost</th><th>Started</th></tr></thead><tbody>{runs.map((run) => <tr key={run.id} aria-selected={selectedRun?.id === run.id} onClick={() => setSelectedRunId(run.id)}><td>{run.agentName}</td><td><span className={`status-pill ${run.status === "done" ? "tone-green" : run.status === "failed" || run.status === "interrupted" ? "tone-red" : "tone-cyan"}`}>{run.status}</span></td><td>{run.provider ?? "—"} / {run.model ?? "—"}</td><td>{run.inputTokens ?? 0} / {run.outputTokens ?? 0}</td><td>{formatUsd(run.estimatedCostUsd)}</td><td>{formatDateTime(run.startedAt)}</td></tr>)}</tbody></table></div>}
      {selectedRun ? <div className="run-inspector"><div className="section-heading"><div><span className="eyebrow">Run payload</span><h2>{selectedRun.agentName}</h2></div><span>{selectedRun.id}</span></div>{selectedRun.lastError ? <p className="inline-error">{selectedRun.lastError}</p> : null}<div className="payload-grid"><div><strong>Structured output</strong><pre>{selectedRun.structuredOutput ? JSON.stringify(selectedRun.structuredOutput, null, 2) : "No structured output"}</pre></div><div><strong>Raw output</strong><pre>{selectedRun.rawOutput ?? "No raw output"}</pre></div></div></div> : null}
    </section>
    <section className="panel metrics-panel">
      <div className="section-heading"><div><span className="eyebrow">Router telemetry</span><h2>Model metrics</h2></div><span className="count-chip">{metrics.length}</span></div>
      <div className="metric-grid">{metrics.map((metric) => <article key={`${metric.providerId}:${metric.modelId}`}><strong>{metric.providerId}</strong><span>{metric.modelId}</span><dl><div><dt>Success</dt><dd>{metric.successes}/{metric.attempts}</dd></div><div><dt>Latency</dt><dd>{metric.averageLatencyMs === null ? "—" : `${Math.round(metric.averageLatencyMs)}ms`}</dd></div><div><dt>Tokens</dt><dd>{metric.inputTokens + metric.outputTokens}</dd></div><div><dt>Cost</dt><dd>{formatUsd(metric.estimatedCostUsd)}</dd></div></dl></article>)}</div>
      <div className={`stream-output ${partialIncomplete ? "is-incomplete" : ""}`}><div className="section-heading"><div><span className="eyebrow">Live generation</span><h2>{partialIncomplete ? "Incomplete output" : "Visible output"}</h2></div>{partialIncomplete ? <button type="button" onClick={() => void retryActiveGeneration().catch(() => undefined)}>Retry explicitly</button> : null}</div><pre>{partialOutput || "No streamed output in this session."}</pre></div>
    </section>
    <section className="panel event-panel">
      <div className="event-column"><div className="section-heading"><div><span className="eyebrow">Filesystem</span><h2>Watcher events</h2></div><span className="count-chip">{workspaceEvents.length}</span></div>{workspaceEvents.length ? <div className="event-feed">{workspaceEvents.map((event, index) => <div key={`${event.timestamp}:${event.relativePath}:${index}`}><span className="event-kind">{event.kind}</span><code>{event.relativePath}</code><time>{formatDateTime(event.timestamp)}</time></div>)}</div> : <p className="muted">Waiting for workspace changes.</p>}</div>
      <div className="event-column"><div className="section-heading"><div><span className="eyebrow">Streaming router</span><h2>LLM events</h2></div><span className="count-chip">{llmEvents.length}</span></div>{llmEvents.length ? <div className="event-feed">{llmEvents.map((event) => <div key={`${event.generationId}:${event.sequence}`}><span className="event-kind">{event.kind}</span><code>{event.providerId ?? event.routeId ?? event.executionId}</code><time>#{event.sequence}</time></div>)}</div> : <p className="muted">Waiting for LLM runtime events.</p>}</div>
      <div className="phase-seven-note"><strong>Advanced traces are not available yet.</strong><span>Persisted application logs and end-to-end prompt traces remain scoped to Phase 7. This view only displays real data exposed by the current IPC contract.</span></div>
    </section>
  </div>;
}
