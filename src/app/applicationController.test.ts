import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  ENSURE_PROJECT_FOR_WORKSPACE_COMMAND,
  GET_AGENT_RUNTIME_CONFIG_COMMAND,
  GET_CHAT_CONVERSATION_COMMAND,
  GET_LAST_ACTIVE_PROJECT_COMMAND,
  GET_LLM_ROUTING_METRICS_COMMAND,
  GET_LLM_RESILIENCE_SNAPSHOT_COMMAND,
  GET_PULSATION_RUNTIME_SNAPSHOT_COMMAND,
  GET_PROVIDER_KEY_STATUSES_COMMAND,
  GET_WORKSPACE_VAULT_INDEX_COMMAND,
  LIST_AGENT_RUNS_COMMAND,
  LIST_CHAT_CONVERSATIONS_COMMAND,
  LIST_PROVIDER_CONNECTIONS_COMMAND,
  LIST_PROVIDER_PROFILES_COMMAND,
  LIST_PROVIDER_ROUTE_BINDINGS_COMMAND,
  LIST_PROVIDER_ROUTES_COMMAND,
  LIST_PULSATION_ARTIFACTS_COMMAND,
  LOAD_UI_SESSION_COMMAND,
  OPEN_WORKSPACE_COMMAND,
  READ_WORKSPACE_FILE_COMMAND,
  SAVE_UI_SESSION_COMMAND,
  type GetPulsationRuntimeSnapshotResponse,
  type Project
} from "../ipc/contracts";
import { useEditorStore } from "../state/editorStore";
import { useChatStore } from "../state/chatStore";
import { usePulsationStore } from "../state/pulsationStore";
import { useVaultStore } from "../state/vaultStore";
import { useViewStore } from "../state/viewStore";
import {
  installTauriRuntime,
  tauriInvokeMock,
  tauriListenMock
} from "../test/mocks/tauriMock";
import { openWorkspacePath, startApplication, stopApplication } from "./applicationController";

const emptySnapshot: GetPulsationRuntimeSnapshotResponse = {
  snapshot: { pulsations: [], hitlWaitingCount: 0, backgroundAgentsCount: 0 }
};

const project: Project = {
  id: "project-1",
  name: "Hard Vibe",
  workspacePath: "C:\\vault",
  isArchived: false,
  createdAt: "2026-08-22T00:00:00Z",
  updatedAt: "2026-08-22T00:00:00Z",
  lastOpenedAt: "2026-08-22T00:00:00Z"
};

describe("applicationController", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    installTauriRuntime();
    vi.clearAllMocks();
    stopApplication();
    useEditorStore.getState().resetTabs();
    usePulsationStore.getState().resetRuntime();
    useVaultStore.getState().resetWorkspace();
    useChatStore.getState().resetProject();
    useViewStore.setState({
      activeView: "workspace",
      sidebarCollapsed: false,
      contextDrawerOpen: true,
      globalError: null,
      persistenceWarning: null
    });
    useChatStore.setState({ projectId: project.id, activeConversationId: "chat-1" });
    tauriListenMock.mockResolvedValue(vi.fn());
  });

  afterEach(() => {
    stopApplication();
    vi.useRealTimers();
  });

  it("polls every 1.2 seconds without overlapping and disposes listeners and timers", async () => {
    let snapshotCalls = 0;
    let resolveFirstSnapshot: ((value: GetPulsationRuntimeSnapshotResponse) => void) | undefined;
    const unlisten = vi.fn();
    tauriListenMock.mockResolvedValue(unlisten);
    tauriInvokeMock.mockImplementation((command) => {
      if (command === GET_LAST_ACTIVE_PROJECT_COMMAND) return Promise.resolve({ project: null });
      if (command === GET_LLM_ROUTING_METRICS_COMMAND) return Promise.resolve({ metrics: [] });
      if (command === GET_PULSATION_RUNTIME_SNAPSHOT_COMMAND) {
        snapshotCalls += 1;
        if (snapshotCalls === 1) {
          return new Promise((resolve) => { resolveFirstSnapshot = resolve; });
        }
        return Promise.resolve(emptySnapshot);
      }
      return Promise.resolve({});
    });

    startApplication();
    usePulsationStore.setState({ projectId: "project-1" });
    await vi.advanceTimersByTimeAsync(1_200);
    await vi.advanceTimersByTimeAsync(2_400);
    expect(snapshotCalls).toBe(1);

    resolveFirstSnapshot?.(emptySnapshot);
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(1_200);
    expect(snapshotCalls).toBe(2);

    stopApplication();
    expect(unlisten).toHaveBeenCalledTimes(16);
    await vi.advanceTimersByTimeAsync(2_400);
    expect(snapshotCalls).toBe(2);
  });

  it("persists restorable layout and domain selections with debounce", async () => {
    tauriInvokeMock.mockImplementation((command) => {
      if (command === GET_LAST_ACTIVE_PROJECT_COMMAND) return Promise.resolve({ project: null });
      if (command === SAVE_UI_SESSION_COMMAND) return Promise.resolve({ updatedAt: "now" });
      return Promise.resolve({});
    });
    startApplication();
    usePulsationStore.setState({
      activeProject: project,
      projectId: project.id,
      activePulsationId: "pulse-1",
      workspaceViewMode: "hitl"
    });
    useVaultStore.setState({
      workspaceRoot: project.workspacePath,
      selectedVaultPath: "Features/Login.md"
    });
    useViewStore.setState({
      activeView: "settings",
      sidebarCollapsed: true,
      contextDrawerOpen: false
    });

    await vi.advanceTimersByTimeAsync(599);
    expect(tauriInvokeMock.mock.calls.some(([command]) => command === SAVE_UI_SESSION_COMMAND)).toBe(false);
    await vi.advanceTimersByTimeAsync(1);

    expect(tauriInvokeMock).toHaveBeenCalledWith(SAVE_UI_SESSION_COMMAND, {
      request: expect.objectContaining({
        projectId: "project-1",
        workspaceRoot: "C:\\vault",
        activeView: "settings",
        sidebarCollapsed: true,
        contextDrawerOpen: false,
        activePulsationId: "pulse-1",
        selectedVaultPath: "Features/Login.md",
        workspaceViewMode: "hitl",
        activeChatConversationId: "chat-1"
      })
    });
  });

  it("flushes the latest pending session when the application stops inside the debounce window", async () => {
    tauriInvokeMock.mockImplementation((command) => {
      if (command === GET_LAST_ACTIVE_PROJECT_COMMAND) return Promise.resolve({ project: null });
      if (command === SAVE_UI_SESSION_COMMAND) return Promise.resolve({ updatedAt: "now" });
      return Promise.resolve({});
    });
    startApplication();
    usePulsationStore.setState({ activeProject: project, projectId: project.id });
    useVaultStore.setState({ workspaceRoot: project.workspacePath });
    useViewStore.setState({ activeView: "vault" });
    useViewStore.setState({ activeView: "chat" });

    await vi.advanceTimersByTimeAsync(599);
    expect(tauriInvokeMock.mock.calls.some(([command]) => command === SAVE_UI_SESSION_COMMAND)).toBe(false);

    stopApplication();
    await vi.advanceTimersByTimeAsync(0);

    const saves = tauriInvokeMock.mock.calls.filter(([command]) => command === SAVE_UI_SESSION_COMMAND);
    expect(saves).toHaveLength(1);
    expect(saves[0][1]).toEqual({
      request: expect.objectContaining({ projectId: project.id, activeView: "chat" })
    });
  });

  it("rehydrates every persisted domain selection when reopening a workspace", async () => {
    const conversation = {
      id: "chat-1",
      projectId: project.id,
      connectionId: "connection-1",
      providerId: "openai",
      modelId: "gpt-4.1-mini",
      title: "Architecture",
      connectionAvailable: true,
      createdAt: "now",
      updatedAt: "now"
    };
    tauriInvokeMock.mockImplementation((command) => {
      switch (command) {
        case OPEN_WORKSPACE_COMMAND:
          return Promise.resolve({
            rootPath: project.workspacePath,
            tree: [],
            greenfield: { mode: "existing", initialized: false, message: "Existing", createdPaths: [] }
          });
        case ENSURE_PROJECT_FOR_WORKSPACE_COMMAND:
          return Promise.resolve({ project });
        case LOAD_UI_SESSION_COMMAND:
          return Promise.resolve({
            session: {
              projectId: project.id,
              workspaceRoot: project.workspacePath,
              activeTabPath: "Features/Login.md",
              workspaceViewMode: "hitl",
              activeView: "chat",
              sidebarCollapsed: true,
              contextDrawerOpen: false,
              activePulsationId: "pulse-1",
              selectedVaultPath: "Features/Login.md",
              activeChatConversationId: conversation.id,
              scrollTop: 44,
              tabs: [{
                relativePath: "Features/Login.md",
                draftContent: "# Local draft",
                baseRevision: "1:1",
                cursorLine: 7,
                cursorColumn: 3,
                scrollTop: 44
              }],
              updatedAt: "now"
            }
          });
        case READ_WORKSPACE_FILE_COMMAND:
          return Promise.resolve({ content: "# Disk", revision: "2:1" });
        case GET_WORKSPACE_VAULT_INDEX_COMMAND:
          return Promise.resolve({ notes: [{ relativePath: "Features/Login.md" }], links: [], brokenLinks: [] });
        case GET_PULSATION_RUNTIME_SNAPSHOT_COMMAND:
          return Promise.resolve({
            snapshot: {
              pulsations: [{ id: "pulse-1", featureName: "Login", status: "waiting_hitl" }],
              hitlWaitingCount: 1,
              backgroundAgentsCount: 0
            }
          });
        case LIST_PULSATION_ARTIFACTS_COMMAND:
          return Promise.resolve({ artifacts: [] });
        case LIST_CHAT_CONVERSATIONS_COMMAND:
          return Promise.resolve({ conversations: [conversation] });
        case GET_CHAT_CONVERSATION_COMMAND:
          return Promise.resolve({ conversation, messages: [], toolCalls: [] });
        case GET_AGENT_RUNTIME_CONFIG_COMMAND:
          return Promise.resolve({ config: null });
        case LIST_PROVIDER_PROFILES_COMMAND:
          return Promise.resolve({ profiles: [] });
        case LIST_PROVIDER_CONNECTIONS_COMMAND:
          return Promise.resolve({ connections: [] });
        case LIST_PROVIDER_ROUTES_COMMAND:
          return Promise.resolve({ routes: [] });
        case LIST_PROVIDER_ROUTE_BINDINGS_COMMAND:
          return Promise.resolve({ bindings: [] });
        case GET_PROVIDER_KEY_STATUSES_COMMAND:
          return Promise.resolve({ providers: [] });
        case GET_LLM_RESILIENCE_SNAPSHOT_COMMAND:
          return Promise.resolve({ snapshot: { states: [] } });
        case LIST_AGENT_RUNS_COMMAND:
          return Promise.resolve({ runs: [] });
        case GET_LLM_ROUTING_METRICS_COMMAND:
          return Promise.resolve({ metrics: [] });
        default:
          return Promise.resolve({});
      }
    });

    await openWorkspacePath(project.workspacePath);

    expect(useViewStore.getState()).toMatchObject({
      activeView: "chat",
      sidebarCollapsed: true,
      contextDrawerOpen: false
    });
    expect(useEditorStore.getState()).toMatchObject({ activeTabPath: "Features/Login.md" });
    expect(useEditorStore.getState().tabs[0]).toMatchObject({
      relativePath: "Features/Login.md",
      content: "# Local draft",
      dirty: true,
      cursorLine: 7,
      cursorColumn: 3,
      scrollTop: 44
    });
    expect(usePulsationStore.getState().activePulsationId).toBe("pulse-1");
    expect(useVaultStore.getState().selectedVaultPath).toBe("Features/Login.md");
    expect(useChatStore.getState().activeConversationId).toBe("chat-1");
  });
});
