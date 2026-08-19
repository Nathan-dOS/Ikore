import { Suspense, lazy, useCallback, useEffect, useMemo, useRef, useState, type FormEvent } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import { renderMarkdown } from "./markdown/renderMarkdown";
import {
  type AgentRunListItem,
  type AgentRuntimeConfig,
  type ContextStrategy,
  type FileNode,
  type GetWorkspaceVaultIndexResponse,
  type Project,
  type SearchMatch,
  type SearchMode,
  type SearchWorkspaceTextResponse,
  type UiSessionState,
  type UiTabSnapshot,
  type WorkspaceViewMode as PersistedWorkspaceViewMode,
  type WorkspaceChangedEvent,
  WORKSPACE_CHANGED_EVENT
} from "./ipc/contracts";
import {
  ensureProjectForWorkspace,
  cancelPulsationJobs,
  getWorkspaceVaultIndex,
  getAgentRuntimeConfig,
  getLastActiveProject,
  isTauriRuntime,
  listAgentRuns,
  listWorkspaceTree,
  loadUiSession,
  normalizeWorkspaceError,
  openWorkspace,
  pickWorkspaceRoot,
  readWorkspaceFile,
  renameWorkspaceNote,
  reindexWorkspaceEmbeddings,
  saveUiSession,
  searchWorkspaceText,
  updateAgentRuntimeConfig,
  writeWorkspaceFileAtomic
} from "./ipc/client";
import { useEditorStore } from "./state/editorStore";
import {
  PIPELINE_STAGES,
  type HitlDecision,
  type PulsationStage,
  type PulsationStatus,
  usePulsationStore
} from "./state/pulsationStore";
import type { WorkspaceEditorPaneHandle } from "./components/WorkspaceEditorPane";
import LlmRoutingSettings from "./components/LlmRoutingSettings";

const WorkspaceEditorPane = lazy(() => import("./components/WorkspaceEditorPane"));

interface CursorTarget {
  line: number;
  column: number;
}

interface TreeProps {
  nodes: FileNode[];
  activePath: string | null;
  onOpenFile: (relativePath: string) => void;
}

type DiffSideMark = "neutral" | "remove" | "add" | "change" | "empty";

interface DiffSide {
  line: number | null;
  text: string;
  marker: string;
  mark: DiffSideMark;
}

interface DiffRow {
  left: DiffSide;
  right: DiffSide;
}

interface VaultGraphNodeLayout {
  relativePath: string;
  title: string;
  x: number;
  y: number;
}

function splitLines(content: string): string[] {
  if (!content) {
    return [];
  }

  return content.replace(/\r/g, "").split("\n");
}

function buildDiffRows(input: string, generated: string): DiffRow[] {
  const leftLines = splitLines(input);
  const rightLines = splitLines(generated);
  const maxLength = Math.max(leftLines.length, rightLines.length);
  const rows: DiffRow[] = [];

  let leftLine = 0;
  let rightLine = 0;

  for (let index = 0; index < maxLength; index += 1) {
    const leftText = leftLines[index];
    const rightText = rightLines[index];

    const hasLeft = leftText !== undefined;
    const hasRight = rightText !== undefined;

    const leftNumber = hasLeft ? ++leftLine : null;
    const rightNumber = hasRight ? ++rightLine : null;

    if (leftText === rightText && hasLeft && hasRight) {
      rows.push({
        left: { line: leftNumber, text: leftText, marker: " ", mark: "neutral" },
        right: { line: rightNumber, text: rightText, marker: " ", mark: "neutral" }
      });
      continue;
    }

    if (!hasLeft && hasRight) {
      rows.push({
        left: { line: null, text: "", marker: " ", mark: "empty" },
        right: { line: rightNumber, text: rightText ?? "", marker: "+", mark: "add" }
      });
      continue;
    }

    if (hasLeft && !hasRight) {
      rows.push({
        left: { line: leftNumber, text: leftText ?? "", marker: "-", mark: "remove" },
        right: { line: null, text: "", marker: " ", mark: "empty" }
      });
      continue;
    }

    rows.push({
      left: { line: leftNumber, text: leftText ?? "", marker: "-", mark: "change" },
      right: { line: rightNumber, text: rightText ?? "", marker: "+", mark: "change" }
    });
  }

  return rows;
}

function flattenFileNodes(nodes: FileNode[]): FileNode[] {
  const files: FileNode[] = [];

  const visit = (items: FileNode[]) => {
    items.forEach((node) => {
      if (node.kind === "file") {
        files.push(node);
        return;
      }

      if (node.children && node.children.length > 0) {
        visit(node.children);
      }
    });
  };

  visit(nodes);
  return files;
}

function extractFileName(relativePath: string): string {
  const segments = relativePath.split("/");
  return segments[segments.length - 1] ?? relativePath;
}

function extractPathBaseName(pathValue: string): string {
  const segments = pathValue.split(/[\\/]/).filter((segment) => segment.length > 0);
  return segments[segments.length - 1] ?? pathValue;
}

function formatDuration(ms: number): string {
  const totalSeconds = Math.floor(ms / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);

  return `${hours.toString().padStart(2, "0")}h ${minutes.toString().padStart(2, "0")}m`;
}

function formatUsd(value: number | null): string {
  if (value === null || Number.isNaN(value)) {
    return "-";
  }
  return `$${value.toFixed(4)}`;
}

function formatFrontmatterValue(value: unknown): string {
  if (Array.isArray(value)) {
    return value.map((item) => formatFrontmatterValue(item)).join(", ");
  }
  if (value && typeof value === "object") {
    return JSON.stringify(value);
  }
  if (value === null || value === undefined) {
    return "-";
  }
  return String(value);
}

function normalizeNoteTitle(note: { title: string; relativePath: string }): string {
  const trimmedTitle = note.title.trim();
  if (trimmedTitle.length > 0) {
    return trimmedTitle;
  }
  return extractFileName(note.relativePath).replace(/\.md$/i, "");
}

function getPulsationSubtitle(status: PulsationStatus): string {
  if (status === "checkpoint") {
    return "HITL Review";
  }
  if (status === "done") {
    return "Integrator";
  }
  if (status === "failed") {
    return "Failed";
  }
  if (status === "planning") {
    return "Planning";
  }
  if (status === "idle") {
    return "Idle";
  }
  return "Implementation";
}

interface FallbackDiffDraft {
  title: string;
  input: string;
  generated: string;
  notes: string[];
}

function buildFallbackDiffDraft(
  featureName: string,
  stage: PulsationStage,
  status: PulsationStatus
): FallbackDiffDraft {
  const title = `${stage} Review - ${featureName}`;
  const input = [
    `- Pulsation: ${featureName}`,
    `- Stage: ${stage}`,
    `- Status: ${status}`,
    "- Source contract/spec snapshot loaded from orchestration runtime"
  ].join("\n");
  const generated = [
    `- Pulsation: ${featureName}`,
    `- Stage: ${stage}`,
    `- Runtime status: ${status}`,
    "- Generated checkpoint draft ready for HITL decision"
  ].join("\n");
  const notes = [
    "Draft generated from SQLite orchestration state machine.",
    "Use APPROVE / REQUEST REVISION / REJECT to continue the pulse flow."
  ];

  return { title, input, generated, notes };
}

function getPulsationBadgeClass(status: PulsationStatus): string {
  if (status === "checkpoint") {
    return "pill-purple";
  }

  if (status === "done") {
    return "pill-green";
  }

  if (status === "failed") {
    return "pill-red";
  }

  return "pill-cyan";
}

function getHeaderStatusLabel(status: PulsationStatus): string {
  if (status === "done") {
    return "Success-state";
  }

  if (status === "checkpoint") {
    return "HITL Review";
  }

  if (status === "failed") {
    return "Failed";
  }

  if (status === "idle") {
    return "Idle";
  }

  return "In Progress";
}

function getHeaderStatusClass(status: PulsationStatus): string {
  if (status === "done") {
    return "status-green";
  }

  if (status === "checkpoint") {
    return "status-purple";
  }

  if (status === "failed") {
    return "status-red";
  }

  return "status-cyan";
}

function isTypingElement(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) {
    return false;
  }

  if (target.isContentEditable) {
    return true;
  }

  const tagName = target.tagName.toLowerCase();
  return tagName === "input" || tagName === "textarea" || tagName === "select";
}

function TreeView({ nodes, activePath, onOpenFile }: TreeProps) {
  if (nodes.length === 0) {
    return <p className="empty-text">No `.md` or `.txt` files found.</p>;
  }

  return (
    <ul className="tree">
      {nodes.map((node) => (
        <li key={`${node.kind}:${node.relativePath}`}>
          {node.kind === "directory" ? (
            <div className="tree-node">
              <span className="tree-icon">DIR</span>
              <span className="node-name">{node.name}</span>
              <span className="node-path">{node.relativePath}</span>
            </div>
          ) : (
            <button
              type="button"
              className={`tree-file-button ${
                activePath === node.relativePath ? "tree-file-button-active" : ""
              }`}
              onClick={() => onOpenFile(node.relativePath)}
            >
              <span className="tree-icon">FILE</span>
              <span className="node-name">{node.name}</span>
              <span className="node-path">{node.relativePath}</span>
            </button>
          )}

          {node.kind === "directory" && node.children && node.children.length > 0 ? (
            <TreeView nodes={node.children} activePath={activePath} onOpenFile={onOpenFile} />
          ) : null}
        </li>
      ))}
    </ul>
  );
}

function stageIndex(stage: PulsationStage): number {
  return PIPELINE_STAGES.indexOf(stage);
}

function toDraftContent(tabContent: string, baseContent: string): string | null {
  if (tabContent === baseContent) {
    return null;
  }
  return tabContent;
}

function isPersistedWorkspaceMode(mode: string): mode is PersistedWorkspaceViewMode {
  return mode === "editor" || mode === "hitl";
}

function buildVaultGraphLayout(
  vaultIndex: GetWorkspaceVaultIndexResponse | null,
  maxNodes = 10
): { nodes: VaultGraphNodeLayout[]; edges: Array<{ source: VaultGraphNodeLayout; target: VaultGraphNodeLayout }> } {
  if (!vaultIndex || vaultIndex.notes.length === 0) {
    return { nodes: [], edges: [] };
  }

  const selectedNotes = vaultIndex.notes.slice(0, maxNodes);
  const centerX = 140;
  const centerY = 86;
  const radius = selectedNotes.length === 1 ? 0 : 58;

  const nodes = selectedNotes.map((note, index) => {
    const angle = (Math.PI * 2 * index) / Math.max(1, selectedNotes.length);
    return {
      relativePath: note.relativePath,
      title: normalizeNoteTitle(note),
      x: centerX + Math.cos(angle) * radius,
      y: centerY + Math.sin(angle) * radius
    };
  });

  const nodesByPath = new Map(nodes.map((node) => [node.relativePath, node]));
  const edges = vaultIndex.edges
    .map((edge) => {
      const source = nodesByPath.get(edge.sourcePath);
      const target = nodesByPath.get(edge.targetPath);
      if (!source || !target) {
        return null;
      }
      return { source, target };
    })
    .filter((value): value is { source: VaultGraphNodeLayout; target: VaultGraphNodeLayout } => value !== null);

  return { nodes, edges };
}

export function App() {
  const [workspaceRoot, setWorkspaceRoot] = useState<string | null>(null);
  const [activeProject, setActiveProject] = useState<Project | null>(null);
  const [tree, setTree] = useState<FileNode[]>([]);
  const [events, setEvents] = useState<WorkspaceChangedEvent[]>([]);
  const [loadingWorkspace, setLoadingWorkspace] = useState(false);
  const [refreshingTree, setRefreshingTree] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [persistenceWarning, setPersistenceWarning] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchMode, setSearchMode] = useState<SearchMode>("substring");
  const [searchCaseSensitive, setSearchCaseSensitive] = useState(false);
  const [searching, setSearching] = useState(false);
  const [searchResponse, setSearchResponse] = useState<SearchWorkspaceTextResponse | null>(null);
  const [diffExpanded, setDiffExpanded] = useState(false);
  const [editorMountedVersion, setEditorMountedVersion] = useState(0);
  const [vaultDocuments, setVaultDocuments] = useState<Record<string, string>>({});
  const [vaultIndex, setVaultIndex] = useState<GetWorkspaceVaultIndexResponse | null>(null);
  const [vaultIndexLoading, setVaultIndexLoading] = useState(false);
  const [collapsedVaultPanels, setCollapsedVaultPanels] = useState<Record<string, boolean>>({});
  const [selectedVaultNodePath, setSelectedVaultNodePath] = useState<string | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [runtimeConfig, setRuntimeConfig] = useState<AgentRuntimeConfig | null>(null);
  const [agentRuns, setAgentRuns] = useState<AgentRunListItem[]>([]);
  const [runtimeBusy, setRuntimeBusy] = useState(false);
  const [runtimeNotice, setRuntimeNotice] = useState<string | null>(null);

  const tabs = useEditorStore((state) => state.tabs);
  const activeTabPath = useEditorStore((state) => state.activeTabPath);
  const openTab = useEditorStore((state) => state.openTab);
  const setActiveTab = useEditorStore((state) => state.setActiveTab);
  const closeTab = useEditorStore((state) => state.closeTab);
  const resetTabs = useEditorStore((state) => state.resetTabs);
  const updateTabContent = useEditorStore((state) => state.updateTabContent);
  const setTabSaving = useEditorStore((state) => state.setTabSaving);
  const markTabSaved = useEditorStore((state) => state.markTabSaved);
  const setTabConflict = useEditorStore((state) => state.setTabConflict);
  const clearTabConflict = useEditorStore((state) => state.clearTabConflict);
  const replaceTabFromDisk = useEditorStore((state) => state.replaceTabFromDisk);
  const setTabError = useEditorStore((state) => state.setTabError);
  const setTabCursor = useEditorStore((state) => state.setTabCursor);
  const setTabScrollTop = useEditorStore((state) => state.setTabScrollTop);

  const pulsations = usePulsationStore((state) => state.pulsations);
  const activePulsationId = usePulsationStore((state) => state.activePulsationId);
  const workspaceViewMode = usePulsationStore((state) => state.workspaceViewMode);
  const workspaceViewOverride = usePulsationStore((state) => state.workspaceViewOverride);
  const hitlWaitingCount = usePulsationStore((state) => state.hitlWaitingCount);
  const backgroundAgentsCount = usePulsationStore((state) => state.backgroundAgentsCount);
  const sessionStartedAt = usePulsationStore((state) => state.sessionStartedAt);
  const runtimeLoading = usePulsationStore((state) => state.runtimeLoading);
  const runtimeError = usePulsationStore((state) => state.runtimeError);
  const setActivePulsation = usePulsationStore((state) => state.setActivePulsation);
  const setWorkspaceViewOverride = usePulsationStore((state) => state.setWorkspaceViewOverride);
  const clearWorkspaceViewOverride = usePulsationStore((state) => state.clearWorkspaceViewOverride);
  const resetRuntime = usePulsationStore((state) => state.resetRuntime);
  const loadProjectRuntime = usePulsationStore((state) => state.loadProjectRuntime);
  const refreshRuntime = usePulsationStore((state) => state.refreshRuntime);
  const createPulsationForProject = usePulsationStore((state) => state.createPulsationForProject);
  const applyHitlDecisionAction = usePulsationStore((state) => state.applyHitlDecision);

  const activePulsation = useMemo(
    () => pulsations.find((item) => item.id === activePulsationId) ?? pulsations[0] ?? null,
    [activePulsationId, pulsations]
  );

  const activeTab = useMemo(
    () => tabs.find((tab) => tab.relativePath === activeTabPath) ?? null,
    [activeTabPath, tabs]
  );

  const latestAgentRun = useMemo(() => agentRuns[0] ?? null, [agentRuns]);

  const activeDiffDraft = useMemo(() => {
    if (!activePulsation) {
      return null;
    }
    return buildFallbackDiffDraft(
      activePulsation.featureName,
      activePulsation.stage,
      activePulsation.status
    );
  }, [activePulsation]);

  const hitlInput = useMemo(() => {
    if (activeTab) {
      return activeTab.baseContent;
    }
    return activeDiffDraft?.input ?? "";
  }, [activeDiffDraft, activeTab]);

  const hitlGenerated = useMemo(() => {
    if (activeTab) {
      return activeTab.content;
    }
    return activeDiffDraft?.generated ?? "";
  }, [activeDiffDraft, activeTab]);

  const hitlRows = useMemo(() => buildDiffRows(hitlInput, hitlGenerated), [hitlGenerated, hitlInput]);

  const hitlNotes = useMemo(() => {
    if (!activeDiffDraft) {
      return [];
    }
    if (activeTab?.dirty) {
      return [...activeDiffDraft.notes, "Unsaved local edits are included in this draft view."];
    }
    return activeDiffDraft.notes;
  }, [activeDiffDraft, activeTab?.dirty]);

  const workspaceTitle = useMemo(() => {
    if (!activePulsation) {
      return "ACTION REQUIRED: Select a pulsation";
    }
    if (activePulsation.status === "checkpoint") {
      return `ACTION REQUIRED: ${activeDiffDraft?.title ?? activePulsation.featureName}`;
    }
    return `WORKSPACE: ${activePulsation.featureName}`;
  }, [activeDiffDraft, activePulsation]);

  const refreshTimerRef = useRef<number | null>(null);
  const autosaveTimersRef = useRef<Record<string, number>>({});
  const saveSessionTimerRef = useRef<number | null>(null);
  const hydratingSessionRef = useRef(false);
  const editorPaneRef = useRef<WorkspaceEditorPaneHandle | null>(null);
  const pendingCursorRef = useRef<({ relativePath: string } & CursorTarget) | null>(null);
  const tauriRuntime = useMemo(() => isTauriRuntime(), []);

  const vaultNotesByPath = useMemo(() => {
    const entries = vaultIndex?.notes ?? [];
    return new Map(entries.map((note) => [note.relativePath, note]));
  }, [vaultIndex]);

  const contextPanelPaths = useMemo(() => {
    const deduped: string[] = [];
    const seen = new Set<string>();
    const push = (relativePath: string | null | undefined) => {
      if (!relativePath || seen.has(relativePath)) {
        return;
      }
      seen.add(relativePath);
      deduped.push(relativePath);
    };

    if (activeTabPath) {
      push(activeTabPath);
      const activeVaultNote = vaultNotesByPath.get(activeTabPath);
      activeVaultNote?.outgoingPaths.forEach((path) => push(path));
    }

    (vaultIndex?.notes ?? []).forEach((note) => push(note.relativePath));

    return deduped.slice(0, 4);
  }, [activeTabPath, vaultIndex?.notes, vaultNotesByPath]);

  const vaultGraph = useMemo(() => buildVaultGraphLayout(vaultIndex), [vaultIndex]);

  const sessionDurationLabel = useMemo(
    () => formatDuration(Math.max(0, now - sessionStartedAt)),
    [now, sessionStartedAt]
  );

  const refreshTree = useCallback(async () => {
    if (!workspaceRoot) {
      return;
    }

    setRefreshingTree(true);
    try {
      const response = await listWorkspaceTree();
      setTree(response.tree);
    } catch (cause) {
      setError(normalizeWorkspaceError(cause).message);
    } finally {
      setRefreshingTree(false);
    }
  }, [workspaceRoot]);

  const refreshVaultIndex = useCallback(
    async (forceRebuild = false) => {
      if (!workspaceRoot) {
        setVaultIndex(null);
        return;
      }

      setVaultIndexLoading(true);
      try {
        const response = await getWorkspaceVaultIndex({ forceRebuild });
        setVaultIndex(response);

        setSelectedVaultNodePath((current) => {
          if (current && response.notes.some((note) => note.relativePath === current)) {
            return current;
          }
          return response.notes[0]?.relativePath ?? null;
        });
      } catch (cause) {
        setError(normalizeWorkspaceError(cause).message);
      } finally {
        setVaultIndexLoading(false);
      }
    },
    [workspaceRoot]
  );

  const refreshRuntimeOps = useCallback(
    async (pulsationId: string | null) => {
      if (!tauriRuntime) {
        return;
      }

      const configResponse = await getAgentRuntimeConfig();
      setRuntimeConfig(configResponse.config);

      if (!pulsationId) {
        setAgentRuns([]);
        return;
      }

      const runsResponse = await listAgentRuns({ pulsationId });
      setAgentRuns(runsResponse.runs);
    },
    [tauriRuntime]
  );

  const clearAutosaveTimer = useCallback((relativePath: string) => {
    const activeTimer = autosaveTimersRef.current[relativePath];
    if (activeTimer !== undefined) {
      window.clearTimeout(activeTimer);
      delete autosaveTimersRef.current[relativePath];
    }
  }, []);

  const performAutosave = useCallback(
    async (relativePath: string) => {
      const tab = useEditorStore
        .getState()
        .tabs.find((currentTab) => currentTab.relativePath === relativePath);
      if (!tab || !tab.dirty || tab.saving || tab.conflict) {
        return;
      }

      setTabSaving(relativePath, true);
      try {
        const response = await writeWorkspaceFileAtomic({
          relativePath,
          content: tab.content,
          expectedRevision: tab.revision
        });
        markTabSaved(relativePath, response.revision);
      } catch (cause) {
        const domainError = normalizeWorkspaceError(cause);
        if (domainError.code === "conflict") {
          setTabConflict(relativePath, domainError.message);
        } else {
          setTabError(relativePath, domainError.message);
        }
      } finally {
        setTabSaving(relativePath, false);
      }
    },
    [markTabSaved, setTabConflict, setTabError, setTabSaving]
  );

  const scheduleAutosave = useCallback(
    (relativePath: string) => {
      clearAutosaveTimer(relativePath);
      autosaveTimersRef.current[relativePath] = window.setTimeout(() => {
        void performAutosave(relativePath);
      }, 1000);
    },
    [clearAutosaveTimer, performAutosave]
  );

  const hydrateSessionTabs = useCallback(
    async (session: UiSessionState | null) => {
      if (!session) {
        clearWorkspaceViewOverride();
        return;
      }

      if (isPersistedWorkspaceMode(session.workspaceViewMode)) {
        setWorkspaceViewOverride(session.workspaceViewMode);
      } else {
        clearWorkspaceViewOverride();
      }

      for (const snapshot of session.tabs) {
        const hasDraft = snapshot.draftContent !== undefined && snapshot.draftContent !== null;
        let diskContent = "";
        let revision = snapshot.baseRevision ?? "";
        let saveError: string | null = null;

        try {
          const response = await readWorkspaceFile({ relativePath: snapshot.relativePath });
          diskContent = response.content;
          revision = response.revision;
        } catch (cause) {
          const message = normalizeWorkspaceError(cause).message;
          saveError = `Unable to load from disk: ${message}`;
        }

        const restoredContent = hasDraft
          ? snapshot.draftContent ?? ""
          : saveError
            ? ""
            : diskContent;
        const baseContent = saveError ? "" : diskContent;

        openTab({
          relativePath: snapshot.relativePath,
          name: extractFileName(snapshot.relativePath),
          content: restoredContent,
          baseContent,
          revision,
          dirty: hasDraft && restoredContent !== baseContent,
          saveError,
          cursorLine: snapshot.cursorLine ?? null,
          cursorColumn: snapshot.cursorColumn ?? null,
          scrollTop: snapshot.scrollTop ?? null
        });
      }

      if (session.activeTabPath) {
        setActiveTab(session.activeTabPath);
      }
    },
    [clearWorkspaceViewOverride, openTab, setActiveTab, setWorkspaceViewOverride]
  );

  const openWorkspaceAndHydrate = useCallback(
    async (workspacePath: string) => {
      const response = await openWorkspace({ path: workspacePath });
      setWorkspaceRoot(response.rootPath);
      setRuntimeNotice(response.greenfield.message);
      setTree(response.tree);
      setEvents([]);
      setSearchResponse(null);
      setVaultDocuments({});
      setVaultIndex(null);
      setCollapsedVaultPanels({});
      setSelectedVaultNodePath(null);
      setPersistenceWarning(null);
      setRuntimeConfig(null);
      setAgentRuns([]);
      setRuntimeNotice(null);
      resetTabs();
      setActiveProject(null);
      resetRuntime();

      const ensuredProject = await ensureProjectForWorkspace({
        workspacePath: response.rootPath,
        preferredName: extractPathBaseName(response.rootPath)
      });
      setActiveProject(ensuredProject.project);
      await loadProjectRuntime(ensuredProject.project.id);
      if (usePulsationStore.getState().pulsations.length === 0) {
        await createPulsationForProject(ensuredProject.project.id, "Feat: User Login");
      }
      await refreshRuntimeOps(usePulsationStore.getState().activePulsationId);
      const vaultResponse = await getWorkspaceVaultIndex({ forceRebuild: true });
      setVaultIndex(vaultResponse);
      setSelectedVaultNodePath(vaultResponse.notes[0]?.relativePath ?? null);

      hydratingSessionRef.current = true;
      try {
        const loadedSession = await loadUiSession({
          projectId: ensuredProject.project.id
        });
        await hydrateSessionTabs(loadedSession.session);
      } finally {
        hydratingSessionRef.current = false;
      }
    },
    [
      createPulsationForProject,
      hydrateSessionTabs,
      loadProjectRuntime,
      refreshRuntimeOps,
      resetRuntime,
      resetTabs
    ]
  );

  const openFileInTab = useCallback(
    async (relativePath: string, cursor?: CursorTarget) => {
      const existingTab = useEditorStore
        .getState()
        .tabs.find((tab) => tab.relativePath === relativePath);
      if (existingTab) {
        setActiveTab(relativePath);
        if (cursor) {
          pendingCursorRef.current = { relativePath, ...cursor };
        }
        return;
      }

      try {
        const response = await readWorkspaceFile({ relativePath });
        openTab({
          relativePath,
          name: extractFileName(relativePath),
          content: response.content,
          revision: response.revision
        });
        if (cursor) {
          pendingCursorRef.current = { relativePath, ...cursor };
        }
      } catch (cause) {
        setError(normalizeWorkspaceError(cause).message);
      }
    },
    [openTab, setActiveTab]
  );

  const handleRenameNote = useCallback(
    async (fromRelativePath: string) => {
      const currentFileName = extractFileName(fromRelativePath);
      const proposedName = window.prompt("Rename note (.md)", currentFileName)?.trim();
      if (!proposedName || proposedName === currentFileName) {
        return;
      }

      const normalizedName = proposedName.toLowerCase().endsWith(".md") ? proposedName : `${proposedName}.md`;
      const fromSegments = fromRelativePath.split("/");
      fromSegments[fromSegments.length - 1] = normalizedName;
      const toRelativePath = fromSegments.join("/");

      try {
        await renameWorkspaceNote({
          fromRelativePath,
          toRelativePath
        });

        setVaultDocuments((current) => {
          const next = { ...current };
          if (next[fromRelativePath] !== undefined) {
            next[toRelativePath] = next[fromRelativePath];
            delete next[fromRelativePath];
          }
          return next;
        });
        setCollapsedVaultPanels((current) => {
          const next = { ...current };
          if (next[fromRelativePath] !== undefined) {
            next[toRelativePath] = next[fromRelativePath];
            delete next[fromRelativePath];
          }
          return next;
        });
        setSelectedVaultNodePath((current) => (current === fromRelativePath ? toRelativePath : current));

        closeTab(fromRelativePath);
        await openFileInTab(toRelativePath);
        await refreshTree();
        await refreshVaultIndex(true);
      } catch (cause) {
        setError(normalizeWorkspaceError(cause).message);
      }
    },
    [closeTab, openFileInTab, refreshTree, refreshVaultIndex]
  );

  const resolveConflictByReload = useCallback(async () => {
    if (!activeTab) {
      return;
    }

    try {
      const response = await readWorkspaceFile({ relativePath: activeTab.relativePath });
      replaceTabFromDisk(activeTab.relativePath, response.content, response.revision);
    } catch (cause) {
      setError(normalizeWorkspaceError(cause).message);
    }
  }, [activeTab, replaceTabFromDisk]);

  const resolveConflictByOverwrite = useCallback(async () => {
    if (!activeTab) {
      return;
    }

    const path = activeTab.relativePath;
    setTabSaving(path, true);

    try {
      const latest = await readWorkspaceFile({ relativePath: path });
      const currentTab = useEditorStore.getState().tabs.find((tab) => tab.relativePath === path);
      if (!currentTab) {
        return;
      }

      const writeResponse = await writeWorkspaceFileAtomic({
        relativePath: path,
        content: currentTab.content,
        expectedRevision: latest.revision
      });

      clearTabConflict(path);
      markTabSaved(path, writeResponse.revision);
    } catch (cause) {
      const domainError = normalizeWorkspaceError(cause);
      if (domainError.code === "conflict") {
        setTabConflict(path, domainError.message);
      } else {
        setTabError(path, domainError.message);
      }
    } finally {
      setTabSaving(path, false);
    }
  }, [activeTab, clearTabConflict, markTabSaved, setTabConflict, setTabError, setTabSaving]);

  const handleHitlDecision = useCallback(
    async (decision: HitlDecision) => {
      setError(null);
      try {
        await applyHitlDecisionAction(decision);
        setDiffExpanded(false);
        await refreshRuntimeOps(usePulsationStore.getState().activePulsationId);
      } catch (cause) {
        setError(normalizeWorkspaceError(cause).message);
      }
    },
    [applyHitlDecisionAction, refreshRuntimeOps]
  );

  useEffect(() => {
    const timer = window.setInterval(() => {
      setNow(Date.now());
    }, 1000);
    return () => {
      window.clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    return () => {
      Object.values(autosaveTimersRef.current).forEach((timerId) => {
        window.clearTimeout(timerId);
      });
      autosaveTimersRef.current = {};
      if (saveSessionTimerRef.current !== null) {
        window.clearTimeout(saveSessionTimerRef.current);
        saveSessionTimerRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    if (!tauriRuntime) {
      return;
    }

    let disposed = false;
    setLoadingWorkspace(true);

    const restoreLastWorkspace = async () => {
      try {
        const response = await getLastActiveProject();
        if (!response.project || disposed) {
          return;
        }

        await openWorkspaceAndHydrate(response.project.workspacePath);
      } catch (cause) {
        if (!disposed) {
          setPersistenceWarning(normalizeWorkspaceError(cause).message);
        }
      } finally {
        if (!disposed) {
          setLoadingWorkspace(false);
        }
      }
    };

    void restoreLastWorkspace();

    return () => {
      disposed = true;
    };
  }, [openWorkspaceAndHydrate, tauriRuntime]);

  useEffect(() => {
    if (!tauriRuntime || !activeProject) {
      return;
    }

    let disposed = false;
    const refresh = async () => {
      try {
        await refreshRuntime();
        await refreshRuntimeOps(usePulsationStore.getState().activePulsationId);
      } catch (cause) {
        if (!disposed) {
          setError(normalizeWorkspaceError(cause).message);
        }
      }
    };

    const timer = window.setInterval(() => {
      void refresh();
    }, 1200);

    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [activeProject, refreshRuntime, refreshRuntimeOps, tauriRuntime]);

  useEffect(() => {
    if (!tauriRuntime || !activeProject) {
      return;
    }

    void refreshRuntimeOps(activePulsationId);
  }, [activeProject, activePulsationId, refreshRuntimeOps, tauriRuntime]);

  useEffect(() => {
    if (!tauriRuntime || !workspaceRoot || !activeProject || hydratingSessionRef.current) {
      return;
    }

    if (saveSessionTimerRef.current !== null) {
      window.clearTimeout(saveSessionTimerRef.current);
      saveSessionTimerRef.current = null;
    }

    saveSessionTimerRef.current = window.setTimeout(() => {
      const currentTabs = useEditorStore.getState().tabs;
      const tabSnapshots: UiTabSnapshot[] = currentTabs.map((tab) => ({
        relativePath: tab.relativePath,
        draftContent: toDraftContent(tab.content, tab.baseContent),
        baseRevision: tab.revision,
        cursorLine: tab.cursorLine,
        cursorColumn: tab.cursorColumn,
        scrollTop: tab.scrollTop
      }));

      void saveUiSession({
        projectId: activeProject.id,
        workspaceRoot,
        activeTabPath,
        workspaceViewMode,
        scrollTop: activeTab?.scrollTop ?? null,
        tabs: tabSnapshots
      })
        .then(() => {
          setPersistenceWarning(null);
        })
        .catch((cause) => {
          setPersistenceWarning(normalizeWorkspaceError(cause).message);
        });
    }, 600);

    return () => {
      if (saveSessionTimerRef.current !== null) {
        window.clearTimeout(saveSessionTimerRef.current);
        saveSessionTimerRef.current = null;
      }
    };
  }, [activeProject, activeTab?.scrollTop, activeTabPath, tabs, tauriRuntime, workspaceRoot, workspaceViewMode]);

  useEffect(() => {
    if (!tauriRuntime || !workspaceRoot) {
      return;
    }

    let disposed = false;
    let unlisten: UnlistenFn | null = null;

    const applyExternalChangeToOpenTab = async (payload: WorkspaceChangedEvent) => {
      const openTabState = useEditorStore
        .getState()
        .tabs.find((tab) => tab.relativePath === payload.relativePath);
      if (!openTabState) {
        return;
      }

      if (payload.kind === "removed") {
        setTabError(payload.relativePath, "File was removed externally. Rename or recreate it to continue.");
        return;
      }

      if (openTabState.dirty || openTabState.saving) {
        setTabConflict(
          payload.relativePath,
          "External changes were detected while this tab had local edits. Resolve conflict by reloading or overwriting."
        );
        return;
      }

      try {
        const latest = await readWorkspaceFile({ relativePath: payload.relativePath });
        replaceTabFromDisk(payload.relativePath, latest.content, latest.revision);
      } catch {
        // Ignore read errors on rapid watcher bursts (e.g., temp rename dance by external editor).
      }
    };

    const subscribe = async () => {
      try {
        unlisten = await listen<WorkspaceChangedEvent>(WORKSPACE_CHANGED_EVENT, (event) => {
          if (disposed) {
            return;
          }

          setEvents((currentEvents) => [event.payload, ...currentEvents].slice(0, 15));
          void applyExternalChangeToOpenTab(event.payload);
          if (refreshTimerRef.current !== null) {
            window.clearTimeout(refreshTimerRef.current);
          }
          refreshTimerRef.current = window.setTimeout(() => {
            void refreshTree();
            void refreshVaultIndex();
          }, 250);
        });
      } catch (cause) {
        if (!disposed) {
          setError(normalizeWorkspaceError(cause).message);
        }
      }
    };

    void subscribe();

    return () => {
      disposed = true;
      if (refreshTimerRef.current !== null) {
        window.clearTimeout(refreshTimerRef.current);
        refreshTimerRef.current = null;
      }
      if (unlisten) {
        void unlisten();
      }
    };
  }, [refreshTree, refreshVaultIndex, replaceTabFromDisk, setTabConflict, setTabError, tauriRuntime, workspaceRoot]);

  useEffect(() => {
    const pending = pendingCursorRef.current;
    if (!pending || pending.relativePath !== activeTabPath || !editorPaneRef.current) {
      return;
    }

    const focused = editorPaneRef.current.focusAt(pending.line, pending.column);
    if (focused) {
      pendingCursorRef.current = null;
    }
  }, [activeTab?.content, activeTabPath, workspaceViewMode, editorMountedVersion]);

  useEffect(() => {
    if (!workspaceRoot) {
      setVaultDocuments({});
      return;
    }

    let disposed = false;

    const loadContextDocuments = async () => {
      const openTabPaths = new Set(tabs.map((tab) => tab.relativePath));
      const pendingPaths = contextPanelPaths.filter(
        (relativePath) =>
          !openTabPaths.has(relativePath) && vaultDocuments[relativePath] === undefined
      );
      if (pendingPaths.length === 0) {
        return;
      }

      const loaded = await Promise.all(
        pendingPaths.map(async (relativePath) => {
          try {
            const response = await readWorkspaceFile({ relativePath });
            return { relativePath, content: response.content };
          } catch {
            return null;
          }
        })
      );

      if (disposed) {
        return;
      }

      setVaultDocuments((current) => {
        const next = { ...current };
        loaded.forEach((entry) => {
          if (entry) {
            next[entry.relativePath] = entry.content;
          }
        });
        return next;
      });
    };

    void loadContextDocuments();

    return () => {
      disposed = true;
    };
  }, [contextPanelPaths, tabs, vaultDocuments, workspaceRoot]);

  useEffect(() => {
    if (!activePulsation || activePulsation.status !== "checkpoint") {
      return;
    }

    const onKeyDown = (event: KeyboardEvent) => {
      if (isTypingElement(event.target)) {
        return;
      }

      if (event.key === "Enter") {
        event.preventDefault();
        void handleHitlDecision("approve");
        return;
      }

      if (event.key.toLowerCase() === "r") {
        event.preventDefault();
        void handleHitlDecision("request_revision");
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
    };
  }, [activePulsation, handleHitlDecision]);

  const handleOpenWorkspace = async () => {
    setError(null);
    setPersistenceWarning(null);
    setLoadingWorkspace(true);

    try {
      const selected = await pickWorkspaceRoot();
      if (!selected.path) {
        return;
      }

      await openWorkspaceAndHydrate(selected.path);
    } catch (cause) {
      const domainError = normalizeWorkspaceError(cause);
      if (domainError.code === "db_error" || domainError.code === "migration_error") {
        setPersistenceWarning(domainError.message);
      } else {
        setError(domainError.message);
      }
    } finally {
      setLoadingWorkspace(false);
    }
  };

  const handleCreatePulsation = async () => {
    if (!activeProject) {
      return;
    }

    const suggestedName = `Feat: Pulse ${pulsations.length + 1}`;
    const selectedName = window.prompt("Feature name", suggestedName);
    const featureName = selectedName?.trim();
    if (!featureName) {
      return;
    }

    setError(null);
    try {
      await createPulsationForProject(activeProject.id, featureName);
      await refreshRuntimeOps(usePulsationStore.getState().activePulsationId);
    } catch (cause) {
      setError(normalizeWorkspaceError(cause).message);
    }
  };

  const handleRetryLlmGeneration = async () => {
    if (!activeProject || !activePulsation) {
      setError("Não há pulsação ativa para repetir.");
      return;
    }
    setRuntimeNotice(null);
    try {
      await createPulsationForProject(activeProject.id, activePulsation.featureName);
      setRuntimeNotice("Nova geração iniciada explicitamente para a mesma feature.");
      await refreshRuntimeOps(usePulsationStore.getState().activePulsationId);
    } catch (cause) {
      setError(normalizeWorkspaceError(cause).message);
    }
  };

  const handleSetContextStrategy = async (strategy: ContextStrategy) => {
    setRuntimeBusy(true);
    setRuntimeNotice(null);
    try {
      const response = await updateAgentRuntimeConfig({
        contextStrategy: strategy,
        embeddingEnabled: strategy === "yaml_graph_rag"
      });
      setRuntimeConfig(response.config);
      setRuntimeNotice(`Estratégia de contexto ativa: ${response.config.contextStrategy}.`);
    } catch (cause) {
      setError(normalizeWorkspaceError(cause).message);
    } finally {
      setRuntimeBusy(false);
    }
  };

  const handleReindexEmbeddings = async () => {
    setRuntimeBusy(true);
    setRuntimeNotice(null);
    try {
      const response = await reindexWorkspaceEmbeddings();
      setRuntimeNotice(
        `Embeddings reindexados (${response.model}): +${response.indexedCount}, =${response.skippedCount}, -${response.deletedCount}, falhas=${response.failedCount}.`
      );
    } catch (cause) {
      setError(normalizeWorkspaceError(cause).message);
    } finally {
      setRuntimeBusy(false);
    }
  };

  const handleCancelActiveRun = async () => {
    if (!activePulsation) {
      return;
    }

    setRuntimeBusy(true);
    setRuntimeNotice(null);
    try {
      const response = await cancelPulsationJobs({ pulsationId: activePulsation.id });
      setRuntimeNotice(`${response.cancelledJobs} job(s) sinalizado(s) para cancelamento.`);
      await refreshRuntimeOps(activePulsation.id);
    } catch (cause) {
      setError(normalizeWorkspaceError(cause).message);
    } finally {
      setRuntimeBusy(false);
    }
  };

  const handleEditorChange = (nextValue: string | undefined) => {
    if (!activeTabPath) {
      return;
    }

    const safeValue = nextValue ?? "";
    updateTabContent(activeTabPath, safeValue);

    const currentTab = useEditorStore
      .getState()
      .tabs.find((tab) => tab.relativePath === activeTabPath);
    if (currentTab && !currentTab.conflict) {
      scheduleAutosave(activeTabPath);
    }
  };

  const handleEditorCursorChange = useCallback(
    (relativePath: string, line: number | null, column: number | null) => {
      setTabCursor(relativePath, line, column);
    },
    [setTabCursor]
  );

  const handleEditorScrollChange = useCallback(
    (relativePath: string, scrollTop: number | null) => {
      setTabScrollTop(relativePath, scrollTop);
    },
    [setTabScrollTop]
  );

  const handleCloseTab = (relativePath: string) => {
    clearAutosaveTimer(relativePath);
    closeTab(relativePath);
  };

  const handleSearchSubmit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (!workspaceRoot || !searchQuery.trim()) {
      setSearchResponse(null);
      return;
    }

    setSearching(true);
    try {
      const response = await searchWorkspaceText({
        query: searchQuery.trim(),
        mode: searchMode,
        caseSensitive: searchCaseSensitive,
        maxResults: 200
      });
      setSearchResponse(response);
    } catch (cause) {
      setError(normalizeWorkspaceError(cause).message);
    } finally {
      setSearching(false);
    }
  };

  const handleOpenSearchMatch = async (match: SearchMatch) => {
    await openFileInTab(match.relativePath, { line: match.line, column: match.column });
  };

  const handleFindInCurrentFile = () => {
    editorPaneRef.current?.runFindInFile();
  };

  const handleToggleVaultPanel = (relativePath: string) => {
    setCollapsedVaultPanels((current) => ({
      ...current,
      [relativePath]: !current[relativePath]
    }));
  };

  const handleSelectVaultNode = useCallback(
    async (relativePath: string) => {
      setSelectedVaultNodePath(relativePath);
      await openFileInTab(relativePath);
    },
    [openFileInTab]
  );

  const currentStageIndex = activePulsation ? stageIndex(activePulsation.stage) : -1;
  const activeStatusClass = activePulsation ? getHeaderStatusClass(activePulsation.status) : "status-cyan";
  const activeStatusLabel = activePulsation ? getHeaderStatusLabel(activePulsation.status) : "In Progress";

  return (
    <div className="app-shell">
      <header className="app-title-strip">
        <span className="app-title-text">Hard Vibe Code.</span>
      </header>

      <main className={`app-grid ${diffExpanded ? "app-grid-expanded" : ""}`}>
        <section className="panel nav-column">
          <div className="panel-header">
            <h2>NAVIGATION</h2>
            <span className="phase-pill">Phase 6</span>
          </div>
          <p className="subtitle">Active Pulsations</p>

          {pulsations.length === 0 ? (
            <p className="empty-text">No pulsations yet for this project.</p>
          ) : (
            <ul className="pulsation-list">
              {pulsations.map((pulsation) => {
                const subtitle = getPulsationSubtitle(pulsation.status);
                return (
                  <li key={pulsation.id}>
                    <button
                      type="button"
                      className={`pulsation-card ${
                        activePulsationId === pulsation.id ? "pulsation-card-active" : ""
                      }`}
                      onClick={() => setActivePulsation(pulsation.id)}
                    >
                      <div className="pulsation-card-head">
                        <strong>{pulsation.featureName}</strong>
                        <span className={`badge-pill ${getPulsationBadgeClass(pulsation.status)}`}>
                          {subtitle}
                        </span>
                      </div>
                      <small>{subtitle}</small>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}

          {!tauriRuntime ? (
            <p className="error">
              IPC unavailable in browser mode. Run with <code>npm run tauri:dev</code>.
            </p>
          ) : null}

          <div className="actions">
            <button type="button" onClick={handleOpenWorkspace} disabled={loadingWorkspace || !tauriRuntime}>
              {loadingWorkspace ? "Opening..." : "Open folder"}
            </button>
            <button
              type="button"
              onClick={() => void handleCreatePulsation()}
              disabled={!activeProject || runtimeLoading || !tauriRuntime}
            >
              New pulse
            </button>
            {refreshingTree ? <span className="status-chip">Refreshing tree...</span> : null}
          </div>

          <div className="workspace-box">
            <span className="label">Workspace root</span>
            <code>{workspaceRoot ?? "No workspace open"}</code>
          </div>

          <div className="workspace-box">
            <span className="label">Active project</span>
            <code>{activeProject ? `${activeProject.name} (${activeProject.id})` : "No project loaded"}</code>
          </div>

          {error ? <p className="error">{error}</p> : null}
          {persistenceWarning ? <p className="error">Persistence warning: {persistenceWarning}</p> : null}
          {runtimeError ? <p className="error">Runtime warning: {runtimeError}</p> : null}
          {runtimeNotice ? <p className="status-chip">{runtimeNotice}</p> : null}

          <section className="nav-section">
            <h3>Agent Runtime</h3>
            <LlmRoutingSettings
              projectId={activeProject?.id ?? null}
              enabled={tauriRuntime}
              runtimeConfig={runtimeConfig}
              onRuntimeConfig={setRuntimeConfig}
              onNotice={setRuntimeNotice}
              onError={setError}
              onRetryGeneration={() => void handleRetryLlmGeneration()}
            />
            <div className="workspace-box">
              <span className="label">Context strategy</span>
              <code>{runtimeConfig?.contextStrategy ?? "-"}</code>
            </div>
            <div className="workspace-box">
              <span className="label">Embeddings</span>
              <code>
                {runtimeConfig
                  ? `${runtimeConfig.embeddingEnabled ? "on" : "off"} / ${runtimeConfig.embeddingProvider} / ${
                      runtimeConfig.embeddingModel
                    }`
                  : "-"}
              </code>
            </div>
            <div className="actions">
              <button
                type="button"
                onClick={() => void handleSetContextStrategy("yaml_graph")}
                disabled={runtimeBusy || !tauriRuntime}
              >
                Use YAML graph
              </button>
              <button
                type="button"
                onClick={() => void handleSetContextStrategy("yaml_graph_rag")}
                disabled={runtimeBusy || !tauriRuntime}
              >
                Use YAML+RAG
              </button>
              <button
                type="button"
                onClick={() => void handleCancelActiveRun()}
                disabled={!activePulsation || runtimeBusy || !tauriRuntime}
              >
                Cancel active run
              </button>
              <button type="button" onClick={() => void handleReindexEmbeddings()} disabled={runtimeBusy || !tauriRuntime}>
                Reindex embeddings
              </button>
            </div>
            <div className="workspace-box">
              <span className="label">Latest run</span>
              <code>{latestAgentRun ? `${latestAgentRun.agentName} (${latestAgentRun.status})` : "No runs yet"}</code>
            </div>
            <div className="workspace-box">
              <span className="label">Tokens / Cost</span>
              <code>
                {latestAgentRun
                  ? `${latestAgentRun.inputTokens ?? 0}/${latestAgentRun.outputTokens ?? 0} | ${formatUsd(
                      latestAgentRun.estimatedCostUsd
                    )}`
                  : "-"}
              </code>
            </div>
          </section>

          <section className="nav-section">
            <h3>Workspace Files</h3>
            <TreeView
              nodes={tree}
              activePath={activeTabPath}
              onOpenFile={(relativePath) => void openFileInTab(relativePath)}
            />
          </section>

          <section className="nav-section">
            <h3>Global Search</h3>
            <form className="search-form" onSubmit={handleSearchSubmit}>
              <input
                type="text"
                value={searchQuery}
                onChange={(event) => setSearchQuery(event.target.value)}
                placeholder="Search text or regex"
              />
              <div className="search-options">
                <select value={searchMode} onChange={(event) => setSearchMode(event.target.value as SearchMode)}>
                  <option value="substring">Substring</option>
                  <option value="regex">Regex</option>
                </select>
                <label>
                  <input
                    type="checkbox"
                    checked={searchCaseSensitive}
                    onChange={(event) => setSearchCaseSensitive(event.target.checked)}
                  />
                  Case sensitive
                </label>
              </div>
              <button type="submit" disabled={searching || !workspaceRoot}>
                {searching ? "Searching..." : "Search"}
              </button>
            </form>

            {searchResponse ? (
              <>
                <p className="search-summary">
                  {searchResponse.matches.length} matches in {searchResponse.scannedFiles} files
                  {searchResponse.truncated ? " (truncated)" : ""}
                </p>
                <ul className="search-results">
                  {searchResponse.matches.map((match, index) => (
                    <li key={`${match.relativePath}:${match.line}:${match.column}:${index}`}>
                      <button
                        type="button"
                        className="search-result-button"
                        onClick={() => void handleOpenSearchMatch(match)}
                      >
                        <strong>{match.relativePath}</strong>
                        <small>
                          line {match.line}, col {match.column}
                        </small>
                        <span>{match.lineText}</span>
                      </button>
                    </li>
                  ))}
                </ul>
              </>
            ) : null}
          </section>

          <section className="nav-section">
            <h3>Watcher Events</h3>
            {events.length === 0 ? (
              <p className="empty-text">Waiting for external file changes...</p>
            ) : (
              <ul className="events">
                {events.map((event, index) => (
                  <li key={`${event.timestamp}:${event.relativePath}:${index}`}>
                    <span className={`badge-pill badge-event-${event.kind}`}>{event.kind}</span>
                    <code>{event.relativePath}</code>
                    <small>{new Date(event.timestamp).toLocaleTimeString("pt-BR")}</small>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="nav-section vault-graph-section">
            <div className="vault-graph-head">
              <h3>Markdown Vault</h3>
              <small>{vaultIndexLoading ? "Syncing..." : `${vaultIndex?.notes.length ?? 0} notes`}</small>
            </div>
            {vaultGraph.nodes.length === 0 ? (
              <p className="empty-text">Open a workspace with markdown notes to render the graph.</p>
            ) : (
              <div className="vault-graph-canvas">
                <svg viewBox="0 0 280 172" role="img" aria-label="Vault graph">
                  {vaultGraph.edges.map((edge, index) => (
                    <line
                      key={`${edge.source.relativePath}:${edge.target.relativePath}:${index}`}
                      x1={edge.source.x}
                      y1={edge.source.y}
                      x2={edge.target.x}
                      y2={edge.target.y}
                      className="vault-graph-edge"
                    />
                  ))}
                  {vaultGraph.nodes.map((node) => {
                    const isSelected = node.relativePath === selectedVaultNodePath;
                    return (
                      <g key={node.relativePath}>
                        <circle
                          cx={node.x}
                          cy={node.y}
                          r={isSelected ? 13 : 11}
                          className={isSelected ? "vault-graph-node selected" : "vault-graph-node"}
                          onClick={() => void handleSelectVaultNode(node.relativePath)}
                        />
                        <text x={node.x} y={node.y + 24} className="vault-graph-label">
                          {node.title.length > 18 ? `${node.title.slice(0, 18)}...` : node.title}
                        </text>
                      </g>
                    );
                  })}
                </svg>
              </div>
            )}
            {vaultIndex && vaultIndex.brokenLinks.length > 0 ? (
              <p className="vault-broken-summary">
                {vaultIndex.brokenLinks.length} broken wikilinks detected in this vault.
              </p>
            ) : null}
          </section>
        </section>

        <section
          className={`panel workspace-column ${
            activePulsation?.status === "checkpoint" ? "workspace-checkpoint" : ""
          }`}
        >
          <div className="workspace-header">
            <div className="workspace-title-block">
              <span className="workspace-app-label">Hard Vibe Code.</span>
              <h1>{workspaceTitle}</h1>
            </div>
            <span className={`workspace-status-pill ${activeStatusClass}`}>{activeStatusLabel}</span>
          </div>

          <div className="workspace-controls">
            <div className="view-toggle">
              <button
                type="button"
                className={workspaceViewMode === "editor" ? "toggle-active" : ""}
                onClick={() => setWorkspaceViewOverride("editor")}
              >
                Editor
              </button>
              <button
                type="button"
                className={workspaceViewMode === "hitl" ? "toggle-active" : ""}
                onClick={() => setWorkspaceViewOverride("hitl")}
              >
                HITL Review
              </button>
            </div>
            <div className="workspace-actions-inline">
              <button type="button" onClick={handleFindInCurrentFile} disabled={!activeTab}>
                Find in file
              </button>
              {workspaceViewOverride ? (
                <button type="button" onClick={clearWorkspaceViewOverride}>
                  Back to auto mode
                </button>
              ) : null}
            </div>
          </div>

          <div className="pipeline-progress">
            {PIPELINE_STAGES.map((stage, index) => {
              const stageClass =
                index < currentStageIndex
                  ? "pipeline-complete"
                  : index === currentStageIndex
                    ? "pipeline-current"
                    : "pipeline-upcoming";

              return (
                <div key={stage} className={`pipeline-step ${stageClass}`}>
                  <span>{stage}</span>
                </div>
              );
            })}
          </div>

          {workspaceViewMode === "editor" ? (
            <Suspense fallback={<div className="editor-loading">Loading editor...</div>}>
              <WorkspaceEditorPane
                ref={editorPaneRef}
                tabs={tabs}
                activeTab={activeTab}
                activeTabPath={activeTabPath}
                setActiveTab={setActiveTab}
                onCloseTab={handleCloseTab}
                onContentChange={handleEditorChange}
                onResolveConflictByReload={() => void resolveConflictByReload()}
                onResolveConflictByOverwrite={() => void resolveConflictByOverwrite()}
                onEditorMounted={() => setEditorMountedVersion((current) => current + 1)}
                onCursorChange={handleEditorCursorChange}
                onScrollChange={handleEditorScrollChange}
              />
            </Suspense>
          ) : (
            <div className={`hitl-mode ${diffExpanded ? "hitl-mode-expanded" : ""}`}>
              <div className="hitl-head">
                <h3>Markdown Diff View</h3>
                <button type="button" onClick={() => setDiffExpanded((current) => !current)}>
                  {diffExpanded ? "Exit fullscreen" : "⤢ Expand"}
                </button>
              </div>
              <div className="hitl-diff-grid">
                <section className="hitl-panel">
                  <header>Input from the prompt:</header>
                  <div className="hitl-lines">
                    {hitlRows.length === 0 ? (
                      <p className="empty-text">No input baseline available.</p>
                    ) : (
                      hitlRows.map((row, index) => (
                        <div key={`left-${index}`} className={`hitl-line hitl-${row.left.mark}`}>
                          <span className="hitl-line-number">{row.left.line ?? " "}</span>
                          <span className="hitl-line-marker">{row.left.marker}</span>
                          <span>{row.left.text || " "}</span>
                        </div>
                      ))
                    )}
                  </div>
                </section>
                <section className="hitl-panel">
                  <header>Generated agent update draft:</header>
                  <div className="hitl-lines">
                    {hitlRows.length === 0 ? (
                      <p className="empty-text">No generated draft available.</p>
                    ) : (
                      hitlRows.map((row, index) => (
                        <div key={`right-${index}`} className={`hitl-line hitl-${row.right.mark}`}>
                          <span className="hitl-line-number">{row.right.line ?? " "}</span>
                          <span className="hitl-line-marker">{row.right.marker}</span>
                          <span>{row.right.text || " "}</span>
                        </div>
                      ))
                    )}
                  </div>
                </section>
              </div>
              <div className="hitl-notes">
                <strong>Note:</strong>
                <ul>
                  {hitlNotes.map((note, index) => (
                    <li key={`${index}:${note}`}>{note}</li>
                  ))}
                </ul>
              </div>
            </div>
          )}

          {activePulsation?.status === "checkpoint" ? (
            <div className="hitl-action-bar">
              <button type="button" className="action-approve" onClick={() => void handleHitlDecision("approve")}>
                APPROVE (Enter)
              </button>
              <button
                type="button"
                className="action-revision"
                onClick={() => void handleHitlDecision("request_revision")}
              >
                REQUEST REVISION (R)
              </button>
              <button type="button" className="action-reject" onClick={() => void handleHitlDecision("reject")}>
                REJECT
              </button>
            </div>
          ) : null}
        </section>

        <section className="panel context-column">
          <div className="panel-header">
            <h2>CONTEXT_VAULT_VIEW</h2>
            <span className="status-chip">
              {vaultIndexLoading ? "Indexing..." : `${vaultIndex?.edges.length ?? 0} links`}
            </span>
          </div>

          <div className="context-panel-stack">
            {contextPanelPaths.length === 0 ? (
              <p className="empty-text">Open a workspace to populate context panels.</p>
            ) : (
              contextPanelPaths.map((relativePath) => {
                const tab = tabs.find((item) => item.relativePath === relativePath);
                const note = vaultNotesByPath.get(relativePath) ?? null;
                const content = tab?.content ?? vaultDocuments[relativePath] ?? "";
                const isCollapsed = collapsedVaultPanels[relativePath] ?? false;
                const frontmatterEntries = Object.entries(note?.frontmatter ?? {});
                const normalized = (note?.normalizedProperties ?? {}) as Record<string, unknown>;
                const rawFrontmatterType = (note?.frontmatter as Record<string, unknown> | null)?.type;
                const noteType =
                  typeof normalized.type === "string"
                    ? normalized.type
                    : typeof rawFrontmatterType === "string"
                      ? rawFrontmatterType
                      : null;
                const noteRisk = typeof normalized.risk === "string" ? normalized.risk : null;
                const schemaWarnings = note?.schemaWarnings ?? [];
                const isMarkdown = relativePath.toLowerCase().endsWith(".md");

                return (
                  <article key={relativePath} className="vault-panel">
                    <div className="vault-panel-head">
                      <div className="vault-panel-title-row">
                        <button
                          type="button"
                          className="vault-collapse-btn"
                          onClick={() => handleToggleVaultPanel(relativePath)}
                        >
                          {isCollapsed ? "▸" : "▾"}
                        </button>
                        <strong>{extractFileName(relativePath).toUpperCase()}</strong>
                      </div>
                      <div className="vault-mini-tabs">
                        <span className="vault-mini-tab active">{note ? "Markdown note" : "Workspace file"}</span>
                        {note ? (
                          <span className="vault-mini-tab">{note.links.length} wikilinks</span>
                        ) : (
                          <span className="vault-mini-tab">No index data</span>
                        )}
                        {note?.backlinkPaths?.length ? (
                          <span className="vault-mini-tab">{note.backlinkPaths.length} backlinks</span>
                        ) : null}
                        {isMarkdown ? (
                          <button
                            type="button"
                            className="vault-mini-tab vault-mini-action"
                            onClick={() => void handleRenameNote(relativePath)}
                          >
                            Rename
                          </button>
                        ) : null}
                      </div>
                    </div>
                    {!isCollapsed ? (
                      <div className="vault-panel-body">
                        {noteType || noteRisk ? (
                          <div className="actions">
                            {noteType ? <span className="badge-pill pill-cyan">{noteType}</span> : null}
                            {noteRisk ? (
                              <span className={`badge-pill ${noteRisk === "high" ? "pill-red" : noteRisk === "medium" ? "pill-purple" : "pill-green"}`}>
                                risk:{noteRisk}
                              </span>
                            ) : null}
                          </div>
                        ) : null}
                        {schemaWarnings.length > 0 ? (
                          <ul className="events">
                            {schemaWarnings.map((warning) => (
                              <li key={`${relativePath}:${warning}`}>
                                <small>{warning}</small>
                              </li>
                            ))}
                          </ul>
                        ) : null}
                        {frontmatterEntries.length > 0 ? (
                          <dl className="vault-frontmatter-list">
                            {frontmatterEntries.slice(0, 6).map(([key, value]) => (
                              <div key={`${relativePath}:${key}`}>
                                <dt>{key}</dt>
                                <dd>{formatFrontmatterValue(value)}</dd>
                              </div>
                            ))}
                          </dl>
                        ) : null}
                        {content ? (
                          <div dangerouslySetInnerHTML={{ __html: renderMarkdown(content) }} />
                        ) : (
                          <p className="empty-text">Loading...</p>
                        )}
                      </div>
                    ) : null}
                  </article>
                );
              })
            )}
          </div>
        </section>
      </main>

      <footer className="status-bar">
        <span>{hitlWaitingCount} HITL Reviews Waiting</span>
        <span>{backgroundAgentsCount} Agents Processing Background Tasks</span>
        <span>Vibe Time: {sessionDurationLabel}</span>
      </footer>
    </div>
  );
}
