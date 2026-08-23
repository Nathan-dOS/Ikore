import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState } from "react";

import { openWorkspaceFromDialog } from "../app/applicationController";
import { ContextDrawer } from "../components/editor/ContextDrawer";
import { HitlActionBar } from "../components/editor/HitlActionBar";
import { MarkdownDiffView } from "../components/editor/MarkdownDiffView";
import { EmptyState } from "../components/EmptyState";
import type { WorkspaceEditorPaneHandle } from "../components/WorkspaceEditorPane";
import type { HitlDecision } from "../ipc/contracts";
import { useEditorStore } from "../state/editorStore";
import { usePulsationStore } from "../state/pulsationStore";
import { useVaultStore } from "../state/vaultStore";
import { useViewStore } from "../state/viewStore";
import { buildFallbackDiffDraft, pulsationStatusLabel, pulsationTone } from "../utils/pulsation";

const WorkspaceEditorPane = lazy(() => import("../components/WorkspaceEditorPane"));

export function WorkspaceView() {
  const workspaceRoot = useVaultStore((state) => state.workspaceRoot);
  const tabs = useEditorStore((state) => state.tabs);
  const activeTabPath = useEditorStore((state) => state.activeTabPath);
  const pendingCursor = useEditorStore((state) => state.pendingCursor);
  const setActiveTab = useEditorStore((state) => state.setActiveTab);
  const closeTab = useEditorStore((state) => state.closeTab);
  const editTabContent = useEditorStore((state) => state.editTabContent);
  const setTabCursor = useEditorStore((state) => state.setTabCursor);
  const setTabScrollTop = useEditorStore((state) => state.setTabScrollTop);
  const clearPendingCursor = useEditorStore((state) => state.clearPendingCursor);
  const resolveConflictByReload = useEditorStore((state) => state.resolveConflictByReload);
  const resolveConflictByOverwrite = useEditorStore((state) => state.resolveConflictByOverwrite);
  const pulsations = usePulsationStore((state) => state.pulsations);
  const activePulsationId = usePulsationStore((state) => state.activePulsationId);
  const workspaceViewMode = usePulsationStore((state) => state.workspaceViewMode);
  const workspaceViewOverride = usePulsationStore((state) => state.workspaceViewOverride);
  const setWorkspaceViewOverride = usePulsationStore((state) => state.setWorkspaceViewOverride);
  const clearWorkspaceViewOverride = usePulsationStore((state) => state.clearWorkspaceViewOverride);
  const applyHitlDecision = usePulsationStore((state) => state.applyHitlDecision);
  const busy = usePulsationStore((state) => state.runtimeLoading);
  const diffExpanded = useViewStore((state) => state.diffExpanded);
  const setDiffExpanded = useViewStore((state) => state.setDiffExpanded);
  const editorRef = useRef<WorkspaceEditorPaneHandle | null>(null);
  const [editorVersion, setEditorVersion] = useState(0);

  const activeTab = useMemo(
    () => tabs.find((tab) => tab.relativePath === activeTabPath) ?? null,
    [activeTabPath, tabs]
  );
  const activePulsation = useMemo(
    () => pulsations.find((item) => item.id === activePulsationId) ?? pulsations[0] ?? null,
    [activePulsationId, pulsations]
  );
  const fallback = useMemo(
    () => activePulsation ? buildFallbackDiffDraft(activePulsation) : null,
    [activePulsation]
  );
  const input = activeTab?.baseContent ?? fallback?.input ?? "";
  const generated = activeTab?.content ?? fallback?.generated ?? "";
  const notes = activeTab?.dirty && fallback
    ? [...fallback.notes, "Unsaved local edits are included in this review."]
    : fallback?.notes ?? [];

  useEffect(() => {
    if (!pendingCursor || pendingCursor.relativePath !== activeTabPath || !editorRef.current) return;
    if (editorRef.current.focusAt(pendingCursor.line, pendingCursor.column)) clearPendingCursor();
  }, [activeTab?.content, activeTabPath, clearPendingCursor, editorVersion, pendingCursor, workspaceViewMode]);

  const handleDecision = useCallback((decision: HitlDecision) => {
    void applyHitlDecision(decision)
      .then(() => setDiffExpanded(false))
      .catch(() => undefined);
  }, [applyHitlDecision, setDiffExpanded]);

  if (!workspaceRoot) {
    return <EmptyState title="Open a workspace" description="Choose a folder to edit files and participate in HITL checkpoints." action={<button type="button" onClick={() => void openWorkspaceFromDialog()}>Open folder</button>} />;
  }

  return (
    <div className={`workspace-view ${diffExpanded ? "has-expanded-diff" : ""}`}>
      <section className={`workspace-main panel ${activePulsation?.status === "checkpoint" ? "has-checkpoint" : ""}`}>
        <div className="workspace-toolbar">
          <div>
            <span className="eyebrow">{activePulsation?.stage ?? "Workspace"}</span>
            <h2>{activePulsation?.status === "checkpoint" ? `Action required: ${fallback?.title}` : activePulsation?.featureName ?? "Select a pulsation"}</h2>
          </div>
          <div className="toolbar-actions">
            {activePulsation ? <span className={`status-pill tone-${pulsationTone(activePulsation.status)}`}>{pulsationStatusLabel(activePulsation.status)}</span> : null}
            <div className="segmented-control" aria-label="Workspace mode">
              <button type="button" aria-pressed={workspaceViewMode === "editor"} onClick={() => setWorkspaceViewOverride("editor")}>Editor</button>
              <button type="button" aria-pressed={workspaceViewMode === "hitl"} onClick={() => setWorkspaceViewOverride("hitl")}>HITL review</button>
            </div>
            {workspaceViewOverride ? <button type="button" className="button-ghost" onClick={clearWorkspaceViewOverride}>Auto mode</button> : null}
          </div>
        </div>
        <div className="workspace-content">
          {workspaceViewMode === "editor" ? (
            <Suspense fallback={<div className="editor-loading">Loading the editor…</div>}>
              <WorkspaceEditorPane
                ref={editorRef}
                tabs={tabs}
                activeTab={activeTab}
                activeTabPath={activeTabPath}
                setActiveTab={setActiveTab}
                onCloseTab={closeTab}
                onContentChange={(value) => activeTabPath && editTabContent(activeTabPath, value ?? "")}
                onResolveConflictByReload={() => activeTabPath && void resolveConflictByReload(activeTabPath)}
                onResolveConflictByOverwrite={() => activeTabPath && void resolveConflictByOverwrite(activeTabPath)}
                onEditorMounted={() => setEditorVersion((version) => version + 1)}
                onCursorChange={setTabCursor}
                onScrollChange={setTabScrollTop}
              />
            </Suspense>
          ) : (
            <MarkdownDiffView input={input} generated={generated} notes={notes} expanded={diffExpanded} onToggleExpanded={() => setDiffExpanded(!diffExpanded)} />
          )}
        </div>
        {activePulsation?.status === "checkpoint" ? <HitlActionBar busy={busy} onDecision={handleDecision} /> : null}
      </section>
      {!diffExpanded ? <ContextDrawer /> : null}
    </div>
  );
}
