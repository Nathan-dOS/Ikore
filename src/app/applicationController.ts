import {
  ensureProjectForWorkspace,
  getLastActiveProject,
  isTauriRuntime,
  loadUiSession,
  normalizeWorkspaceError,
  openWorkspace,
  pickWorkspaceRoot,
  saveUiSession
} from "../ipc/client";
import {
  subscribeChatEvents,
  subscribeLlmEvents,
  subscribeWorkspaceChanges,
  type IpcUnlisten
} from "../ipc/events";
import { disposeEditorTimers, useEditorStore, type CursorTarget } from "../state/editorStore";
import { useChatStore } from "../state/chatStore";
import { usePulsationStore } from "../state/pulsationStore";
import { useRuntimeStore } from "../state/runtimeStore";
import { selectContextPanelPaths, useVaultStore } from "../state/vaultStore";
import { useViewStore } from "../state/viewStore";
import { extractPathBaseName } from "../utils/formatters";

let started = false;
let disposed = false;
let hydrating = false;
let pollInFlight = false;
let persistenceFingerprint = "";
let persistenceTimer: number | null = null;
let watcherRefreshTimer: number | null = null;
let contextDocumentsTimer: number | null = null;
let pollingTimer: number | null = null;
let ipcUnlisteners: IpcUnlisten[] = [];
let storeUnlisteners: Array<() => void> = [];

function currentPersistenceFingerprint(): string {
  const view = useViewStore.getState();
  const editor = useEditorStore.getState();
  const pulsation = usePulsationStore.getState();
  const vault = useVaultStore.getState();
  const chat = useChatStore.getState();
  return JSON.stringify({
    activeView: view.activeView,
    sidebarCollapsed: view.sidebarCollapsed,
    contextDrawerOpen: view.contextDrawerOpen,
    activeTabPath: editor.activeTabPath,
    tabs: editor.tabs.map((tab) => [
      tab.relativePath,
      tab.content,
      tab.baseContent,
      tab.revision,
      tab.cursorLine,
      tab.cursorColumn,
      tab.scrollTop
    ]),
    workspaceViewMode: pulsation.workspaceViewMode,
    activePulsationId: pulsation.activePulsationId,
    selectedVaultPath: vault.selectedVaultPath,
    activeChatConversationId: chat.activeConversationId
  });
}

async function persistCurrentSession(): Promise<void> {
  const view = useViewStore.getState();
  const editor = useEditorStore.getState();
  const pulsation = usePulsationStore.getState();
  const vault = useVaultStore.getState();
  const chat = useChatStore.getState();
  if (!view.tauriAvailable || !pulsation.activeProject || !vault.workspaceRoot) return;
  const activeTab = editor.tabs.find((tab) => tab.relativePath === editor.activeTabPath);
  try {
    await saveUiSession({
      projectId: pulsation.activeProject.id,
      workspaceRoot: vault.workspaceRoot,
      activeTabPath: editor.activeTabPath,
      workspaceViewMode: pulsation.workspaceViewMode,
      activeView: view.activeView,
      sidebarCollapsed: view.sidebarCollapsed,
      contextDrawerOpen: view.contextDrawerOpen,
      activePulsationId: pulsation.activePulsationId,
      selectedVaultPath: vault.selectedVaultPath,
      activeChatConversationId: chat.activeConversationId,
      scrollTop: activeTab?.scrollTop ?? null,
      tabs: editor.toSessionTabs()
    });
    useViewStore.getState().setPersistenceWarning(null);
  } catch (cause) {
    useViewStore.getState().setPersistenceWarning(normalizeWorkspaceError(cause).message);
  }
}

function scheduleSessionPersistence(): void {
  if (hydrating || disposed) return;
  const nextFingerprint = currentPersistenceFingerprint();
  if (nextFingerprint === persistenceFingerprint) return;
  persistenceFingerprint = nextFingerprint;
  if (persistenceTimer !== null) window.clearTimeout(persistenceTimer);
  persistenceTimer = window.setTimeout(() => {
    persistenceTimer = null;
    void persistCurrentSession();
  }, 600);
}

function scheduleContextDocuments(): void {
  if (disposed) return;
  if (contextDocumentsTimer !== null) window.clearTimeout(contextDocumentsTimer);
  contextDocumentsTimer = window.setTimeout(() => {
    contextDocumentsTimer = null;
    const editor = useEditorStore.getState();
    const vault = useVaultStore.getState();
    const paths = selectContextPanelPaths(vault.vaultIndex, editor.activeTabPath);
    void vault.ensureDocuments(paths);
  }, 50);
}

function subscribeStorePersistence(): void {
  storeUnlisteners = [
    useViewStore.subscribe(scheduleSessionPersistence),
    useEditorStore.subscribe(() => {
      scheduleSessionPersistence();
      scheduleContextDocuments();
    }),
    usePulsationStore.subscribe(scheduleSessionPersistence),
    useVaultStore.subscribe(() => {
      scheduleSessionPersistence();
      scheduleContextDocuments();
    }),
    useChatStore.subscribe(scheduleSessionPersistence)
  ];
}

async function refreshRuntimeSnapshot(): Promise<void> {
  const pulsation = usePulsationStore.getState();
  if (!pulsation.projectId || pollInFlight) return;
  pollInFlight = true;
  try {
    await pulsation.refreshRuntime();
    const activePulsationId = usePulsationStore.getState().activePulsationId;
    await useRuntimeStore.getState().refreshObservability(activePulsationId);
  } catch (cause) {
    useViewStore.getState().setGlobalError(normalizeWorkspaceError(cause).message);
  } finally {
    pollInFlight = false;
  }
}

function startPolling(): void {
  if (pollingTimer !== null) window.clearInterval(pollingTimer);
  pollingTimer = window.setInterval(() => void refreshRuntimeSnapshot(), 1200);
}

function handleWorkspaceChange(): void {
  if (watcherRefreshTimer !== null) window.clearTimeout(watcherRefreshTimer);
  watcherRefreshTimer = window.setTimeout(() => {
    watcherRefreshTimer = null;
    const vault = useVaultStore.getState();
    void Promise.all([vault.refreshTree(), vault.refreshIndex()]);
  }, 250);
}

async function startIpcSubscriptions(): Promise<void> {
  try {
    const [workspaceUnlisten, llmUnlisten, chatUnlisten] = await Promise.all([
      subscribeWorkspaceChanges((event) => {
        useRuntimeStore.getState().addWorkspaceEvent(event);
        void useEditorStore.getState().handleExternalChange(event);
        handleWorkspaceChange();
      }),
      subscribeLlmEvents((event) => void useRuntimeStore.getState().consumeLlmEvent(event)),
      subscribeChatEvents((event) => void useChatStore.getState().consumeEvent(event))
    ]);
    if (disposed) {
      workspaceUnlisten();
      llmUnlisten();
      chatUnlisten();
      return;
    }
    ipcUnlisteners = [workspaceUnlisten, llmUnlisten, chatUnlisten];
  } catch (cause) {
    useViewStore.getState().setGlobalError(normalizeWorkspaceError(cause).message);
  }
}

export async function openWorkspacePath(workspacePath: string): Promise<void> {
  const view = useViewStore.getState();
  view.setLoadingWorkspace(true);
  view.setGlobalError(null);
  view.setPersistenceWarning(null);
  hydrating = true;
  try {
    const response = await openWorkspace({ path: workspacePath });
    useEditorStore.getState().resetTabs();
    usePulsationStore.getState().resetRuntime();
    useRuntimeStore.getState().resetTransientState();
    useChatStore.getState().resetProject();
    useVaultStore.getState().hydrateWorkspace(response);

    const ensured = await ensureProjectForWorkspace({
      workspacePath: response.rootPath,
      preferredName: extractPathBaseName(response.rootPath)
    });
    usePulsationStore.getState().setActiveProject(ensured.project);
    const loadedSession = await loadUiSession({ projectId: ensured.project.id });
    useViewStore.getState().hydrateLayout(loadedSession.session);
    await useEditorStore.getState().hydrateTabs(loadedSession.session);
    await useVaultStore.getState().refreshIndex(true);
    useVaultStore.getState().restoreSelection(loadedSession.session?.selectedVaultPath ?? null);
    await usePulsationStore
      .getState()
      .loadProjectRuntime(ensured.project.id, loadedSession.session?.activePulsationId ?? null);
    await useChatStore
      .getState()
      .hydrateProject(
        ensured.project.id,
        loadedSession.session?.activeChatConversationId ?? null
      );
    if (usePulsationStore.getState().pulsations.length === 0) {
      await usePulsationStore.getState().createPulsationForProject(ensured.project.id, "Feat: User Login");
    }
    await Promise.all([
      useRuntimeStore.getState().refreshConfiguration(),
      useRuntimeStore
        .getState()
        .refreshObservability(usePulsationStore.getState().activePulsationId)
    ]);
    view.setNotice(response.greenfield.message);
  } catch (cause) {
    const error = normalizeWorkspaceError(cause);
    if (error.code === "db_error" || error.code === "migration_error") {
      view.setPersistenceWarning(error.message);
    } else {
      view.setGlobalError(error.message);
    }
    throw cause;
  } finally {
    hydrating = false;
    persistenceFingerprint = currentPersistenceFingerprint();
    view.setLoadingWorkspace(false);
  }
}

export async function openWorkspaceFromDialog(): Promise<void> {
  if (!useViewStore.getState().tauriAvailable) return;
  try {
    const selected = await pickWorkspaceRoot();
    if (selected.path) await openWorkspacePath(selected.path);
  } catch (cause) {
    useViewStore.getState().setGlobalError(normalizeWorkspaceError(cause).message);
  }
}

export async function openFileInWorkspace(relativePath: string, cursor?: CursorTarget): Promise<void> {
  try {
    await useEditorStore.getState().openFile(relativePath, cursor);
    useViewStore.getState().setActiveView("workspace");
  } catch (cause) {
    useViewStore.getState().setGlobalError(normalizeWorkspaceError(cause).message);
  }
}

export async function renameVaultNote(fromRelativePath: string, requestedName: string): Promise<void> {
  const normalizedName = requestedName.trim();
  if (!normalizedName || normalizedName.includes("/") || normalizedName.includes("\\") || normalizedName.includes("..")) {
    useVaultStore.getState().setSelectedVaultPath(fromRelativePath);
    useViewStore.getState().setGlobalError("Enter a file name without directories or traversal segments.");
    return;
  }
  const fileName = normalizedName.toLowerCase().endsWith(".md") ? normalizedName : `${normalizedName}.md`;
  const segments = fromRelativePath.split("/");
  segments[segments.length - 1] = fileName;
  const toRelativePath = segments.join("/");
  if (toRelativePath === fromRelativePath) return;
  try {
    await useVaultStore.getState().renameNote(fromRelativePath, toRelativePath);
    const wasOpen = useEditorStore.getState().tabs.some((tab) => tab.relativePath === fromRelativePath);
    if (wasOpen) {
      useEditorStore.getState().closeTab(fromRelativePath);
      await useEditorStore.getState().openFile(toRelativePath);
    }
    useViewStore.getState().setNotice(`Renamed ${fromRelativePath} to ${toRelativePath}.`);
  } catch (cause) {
    useViewStore.getState().setGlobalError(normalizeWorkspaceError(cause).message);
  }
}

async function restoreLastWorkspace(): Promise<void> {
  try {
    const response = await getLastActiveProject();
    if (response.project && !disposed) await openWorkspacePath(response.project.workspacePath);
  } catch (cause) {
    if (!disposed) useViewStore.getState().setPersistenceWarning(normalizeWorkspaceError(cause).message);
  }
}

export function startApplication(): () => void {
  if (started) return stopApplication;
  started = true;
  disposed = false;
  const tauriAvailable = isTauriRuntime();
  useViewStore.getState().setTauriAvailable(tauriAvailable);
  subscribeStorePersistence();
  if (tauriAvailable) {
    void startIpcSubscriptions();
    startPolling();
    void restoreLastWorkspace();
  }
  return stopApplication;
}

export function stopApplication(): void {
  if (!started) return;
  started = false;
  disposed = true;
  const shouldFlushPendingPersistence = persistenceTimer !== null && !hydrating;
  if (persistenceTimer !== null) window.clearTimeout(persistenceTimer);
  if (watcherRefreshTimer !== null) window.clearTimeout(watcherRefreshTimer);
  if (contextDocumentsTimer !== null) window.clearTimeout(contextDocumentsTimer);
  if (pollingTimer !== null) window.clearInterval(pollingTimer);
  persistenceTimer = null;
  watcherRefreshTimer = null;
  contextDocumentsTimer = null;
  pollingTimer = null;
  ipcUnlisteners.forEach((unlisten) => unlisten());
  ipcUnlisteners = [];
  storeUnlisteners.forEach((unlisten) => unlisten());
  storeUnlisteners = [];
  disposeEditorTimers();
  if (shouldFlushPendingPersistence) void persistCurrentSession();
}
