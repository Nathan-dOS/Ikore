import { useMemo, useState, type FormEvent } from "react";

import { openFileInWorkspace } from "../app/applicationController";
import { EmptyState } from "../components/EmptyState";
import { PipelineProgress } from "../components/pulsations/PipelineProgress";
import { usePulsationStore } from "../state/pulsationStore";
import { formatDateTime } from "../utils/formatters";
import { pulsationStatusLabel, pulsationTone } from "../utils/pulsation";

export function PulsationsView() {
  const project = usePulsationStore((state) => state.activeProject);
  const pulsations = usePulsationStore((state) => state.pulsations);
  const activePulsationId = usePulsationStore((state) => state.activePulsationId);
  const setActivePulsation = usePulsationStore((state) => state.setActivePulsation);
  const createPulsation = usePulsationStore((state) => state.createPulsationForProject);
  const cancelActiveJobs = usePulsationStore((state) => state.cancelActiveJobs);
  const artifacts = usePulsationStore((state) => state.artifacts);
  const artifactsLoading = usePulsationStore((state) => state.artifactsLoading);
  const busy = usePulsationStore((state) => state.runtimeLoading);
  const notice = usePulsationStore((state) => state.notice);
  const error = usePulsationStore((state) => state.runtimeError);
  const [featureName, setFeatureName] = useState("");
  const active = useMemo(
    () => pulsations.find((item) => item.id === activePulsationId) ?? pulsations[0] ?? null,
    [activePulsationId, pulsations]
  );

  const onSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const name = featureName.trim();
    if (!project || !name) return;
    void createPulsation(project.id, name)
      .then(() => setFeatureName(""))
      .catch(() => undefined);
  };

  if (!project) return <EmptyState title="No active project" description="Open a workspace before creating or monitoring pulsations." />;

  return (
    <div className="pulsations-view view-grid">
      <aside className="panel pulsation-sidebar">
        <div className="section-heading"><div><span className="eyebrow">Project flow</span><h2>Pulsations</h2></div><span className="count-chip">{pulsations.length}</span></div>
        <form className="inline-create-form" onSubmit={onSubmit}>
          <label htmlFor="feature-name">New pulsation</label>
          <div><input id="feature-name" value={featureName} onChange={(event) => setFeatureName(event.target.value)} placeholder="Feature name" /><button type="submit" disabled={busy || !featureName.trim()}>Create</button></div>
        </form>
        <div className="pulsation-cards">
          {pulsations.map((pulsation) => (
            <button type="button" className="pulsation-card" aria-pressed={pulsation.id === activePulsationId} key={pulsation.id} onClick={() => setActivePulsation(pulsation.id)}>
              <span className="card-topline"><strong>{pulsation.featureName}</strong><span className={`status-dot tone-${pulsationTone(pulsation.status)}`} /></span>
              <span>{pulsation.stage}</span><small>{pulsationStatusLabel(pulsation.status)}</small>
            </button>
          ))}
        </div>
      </aside>
      <section className="panel pulsation-detail">
        {!active ? <EmptyState title="No pulsations yet" description="Create the first pulsation for this project." /> : <>
          <div className="section-heading pulsation-title"><div><span className="eyebrow">Selected pulsation</span><h2>{active.featureName}</h2><p>{active.id}</p></div><div className="toolbar-actions"><span className={`status-pill tone-${pulsationTone(active.status)}`}>{pulsationStatusLabel(active.status)}</span><button type="button" className="button-danger" disabled={busy} onClick={() => void cancelActiveJobs()}>Cancel active jobs</button></div></div>
          <PipelineProgress stage={active.stage} />
          <div className="facts-grid">
            <div><span>Current stage</span><strong>{active.stage}</strong></div>
            <div><span>Created</span><strong>{formatDateTime(active.createdAt)}</strong></div>
            <div><span>Last transition</span><strong>{formatDateTime(active.updatedAt)}</strong></div>
          </div>
          <div className="data-disclaimer">This stage overview reflects the current persisted snapshot. Historical job tracing remains part of Phase 7.</div>
          <section className="artifact-section">
            <div className="section-heading"><div><span className="eyebrow">Generated output</span><h2>Artifacts</h2></div><span className="count-chip">{artifacts.length}</span></div>
            {artifactsLoading ? <p className="muted">Loading artifacts…</p> : artifacts.length === 0 ? <p className="muted">No artifacts have been generated for this pulsation yet.</p> : <div className="artifact-list">{artifacts.map((artifact) => <button type="button" key={artifact.id} disabled={!artifact.relativePath} onClick={() => artifact.relativePath && void openFileInWorkspace(artifact.relativePath)}><span><strong>{artifact.name}</strong><small>{artifact.kind}</small></span><code>{artifact.relativePath ?? "Stored without a workspace path"}</code></button>)}</div>}
          </section>
          {notice ? <p className="inline-notice">{notice}</p> : null}{error ? <p className="inline-error">{error}</p> : null}
        </>}
      </section>
    </div>
  );
}
