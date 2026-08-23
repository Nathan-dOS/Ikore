import { create } from "zustand";

import { normalizeWorkspaceError, readWorkspaceFile, writeWorkspaceFileAtomic } from "../ipc/client";
import type { UiSessionState, UiTabSnapshot, WorkspaceChangedEvent } from "../ipc/contracts";
import { extractFileName } from "../utils/formatters";

export interface EditorConflictState {
  message: string;
}

export interface CursorTarget {
  line: number;
  column: number;
}

export interface EditorTab {
  relativePath: string;
  name: string;
  content: string;
  baseContent: string;
  revision: string;
  dirty: boolean;
  saving: boolean;
  conflict: EditorConflictState | null;
  saveError: string | null;
  cursorLine: number | null;
  cursorColumn: number | null;
  scrollTop: number | null;
}

interface OpenTabPayload {
  relativePath: string;
  name: string;
  content: string;
  revision: string;
  baseContent?: string;
  dirty?: boolean;
  saveError?: string | null;
  cursorLine?: number | null;
  cursorColumn?: number | null;
  scrollTop?: number | null;
}

interface EditorStoreState {
  tabs: EditorTab[];
  activeTabPath: string | null;
  pendingCursor: ({ relativePath: string } & CursorTarget) | null;
  openTab: (payload: OpenTabPayload) => void;
  openFile: (relativePath: string, cursor?: CursorTarget) => Promise<void>;
  hydrateTabs: (session: UiSessionState | null) => Promise<void>;
  setActiveTab: (relativePath: string) => void;
  closeTab: (relativePath: string) => void;
  resetTabs: () => void;
  updateTabContent: (relativePath: string, content: string) => void;
  editTabContent: (relativePath: string, content: string) => void;
  setTabSaving: (relativePath: string, saving: boolean) => void;
  markTabSaved: (relativePath: string, revision: string) => void;
  setTabConflict: (relativePath: string, message: string) => void;
  clearTabConflict: (relativePath: string) => void;
  replaceTabFromDisk: (relativePath: string, content: string, revision: string) => void;
  setTabError: (relativePath: string, message: string | null) => void;
  setTabCursor: (relativePath: string, line: number | null, column: number | null) => void;
  setTabScrollTop: (relativePath: string, scrollTop: number | null) => void;
  clearPendingCursor: () => void;
  performAutosave: (relativePath: string) => Promise<void>;
  scheduleAutosave: (relativePath: string) => void;
  resolveConflictByReload: (relativePath: string) => Promise<void>;
  resolveConflictByOverwrite: (relativePath: string) => Promise<void>;
  handleExternalChange: (event: WorkspaceChangedEvent) => Promise<void>;
  toSessionTabs: () => UiTabSnapshot[];
}

const autosaveTimers = new Map<string, number>();

function clearAutosaveTimer(relativePath: string): void {
  const timer = autosaveTimers.get(relativePath);
  if (timer !== undefined) {
    window.clearTimeout(timer);
    autosaveTimers.delete(relativePath);
  }
}

export function disposeEditorTimers(): void {
  autosaveTimers.forEach((timer) => window.clearTimeout(timer));
  autosaveTimers.clear();
}

function updateTab(
  tabs: EditorTab[],
  relativePath: string,
  updater: (tab: EditorTab) => EditorTab
): EditorTab[] {
  return tabs.map((tab) => (tab.relativePath === relativePath ? updater(tab) : tab));
}

function createEditorTab(payload: OpenTabPayload): EditorTab {
  return {
    relativePath: payload.relativePath,
    name: payload.name,
    content: payload.content,
    baseContent: payload.baseContent ?? payload.content,
    revision: payload.revision,
    dirty: payload.dirty ?? false,
    saving: false,
    conflict: null,
    saveError: payload.saveError ?? null,
    cursorLine: payload.cursorLine ?? null,
    cursorColumn: payload.cursorColumn ?? null,
    scrollTop: payload.scrollTop ?? null
  };
}

export const useEditorStore = create<EditorStoreState>((set, get) => ({
  tabs: [],
  activeTabPath: null,
  pendingCursor: null,
  openTab: (payload) =>
    set((state) => {
      if (state.tabs.some((tab) => tab.relativePath === payload.relativePath)) {
        return { activeTabPath: payload.relativePath };
      }
      return { tabs: [...state.tabs, createEditorTab(payload)], activeTabPath: payload.relativePath };
    }),
  openFile: async (relativePath, cursor) => {
    const existing = get().tabs.find((tab) => tab.relativePath === relativePath);
    if (!existing) {
      const response = await readWorkspaceFile({ relativePath });
      get().openTab({
        relativePath,
        name: extractFileName(relativePath),
        content: response.content,
        revision: response.revision
      });
    } else {
      get().setActiveTab(relativePath);
    }
    if (cursor) set({ pendingCursor: { relativePath, ...cursor } });
  },
  hydrateTabs: async (session) => {
    disposeEditorTimers();
    if (!session) {
      set({ tabs: [], activeTabPath: null, pendingCursor: null });
      return;
    }
    const tabs = await Promise.all(
      session.tabs.map(async (snapshot): Promise<EditorTab> => {
        let diskContent = "";
        let revision = snapshot.baseRevision ?? "";
        let saveError: string | null = null;
        try {
          const response = await readWorkspaceFile({ relativePath: snapshot.relativePath });
          diskContent = response.content;
          revision = response.revision;
        } catch (cause) {
          saveError = `Unable to load from disk: ${normalizeWorkspaceError(cause).message}`;
        }
        const hasDraft = snapshot.draftContent !== undefined && snapshot.draftContent !== null;
        const content = hasDraft ? snapshot.draftContent ?? "" : diskContent;
        return createEditorTab({
          relativePath: snapshot.relativePath,
          name: extractFileName(snapshot.relativePath),
          content,
          baseContent: saveError ? "" : diskContent,
          revision,
          dirty: hasDraft && content !== diskContent,
          saveError,
          cursorLine: snapshot.cursorLine,
          cursorColumn: snapshot.cursorColumn,
          scrollTop: snapshot.scrollTop
        });
      })
    );
    const requestedActive = session.activeTabPath;
    set({
      tabs,
      activeTabPath:
        requestedActive && tabs.some((tab) => tab.relativePath === requestedActive)
          ? requestedActive
          : tabs[0]?.relativePath ?? null,
      pendingCursor: null
    });
  },
  setActiveTab: (relativePath) =>
    set((state) =>
      state.tabs.some((tab) => tab.relativePath === relativePath) ? { activeTabPath: relativePath } : state
    ),
  closeTab: (relativePath) => {
    clearAutosaveTimer(relativePath);
    set((state) => {
      const tabs = state.tabs.filter((tab) => tab.relativePath !== relativePath);
      return {
        tabs,
        activeTabPath:
          state.activeTabPath === relativePath
            ? tabs[tabs.length - 1]?.relativePath ?? null
            : state.activeTabPath
      };
    });
  },
  resetTabs: () => {
    disposeEditorTimers();
    set({ tabs: [], activeTabPath: null, pendingCursor: null });
  },
  updateTabContent: (relativePath, content) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) =>
        tab.content === content
          ? tab
          : { ...tab, content, dirty: content !== tab.baseContent, saveError: null }
      )
    })),
  editTabContent: (relativePath, content) => {
    get().updateTabContent(relativePath, content);
    const tab = get().tabs.find((item) => item.relativePath === relativePath);
    if (tab && !tab.conflict) get().scheduleAutosave(relativePath);
  },
  setTabSaving: (relativePath, saving) =>
    set((state) => ({ tabs: updateTab(state.tabs, relativePath, (tab) => ({ ...tab, saving })) })),
  markTabSaved: (relativePath, revision) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) => ({
        ...tab,
        revision,
        baseContent: tab.content,
        dirty: false,
        saving: false,
        conflict: null,
        saveError: null
      }))
    })),
  setTabConflict: (relativePath, message) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) => ({
        ...tab,
        saving: false,
        conflict: { message }
      }))
    })),
  clearTabConflict: (relativePath) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) => ({ ...tab, conflict: null }))
    })),
  replaceTabFromDisk: (relativePath, content, revision) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) => ({
        ...tab,
        content,
        baseContent: content,
        revision,
        dirty: false,
        saving: false,
        conflict: null,
        saveError: null
      }))
    })),
  setTabError: (relativePath, message) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) => ({ ...tab, saving: false, saveError: message }))
    })),
  setTabCursor: (relativePath, cursorLine, cursorColumn) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) => ({ ...tab, cursorLine, cursorColumn }))
    })),
  setTabScrollTop: (relativePath, scrollTop) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) => ({ ...tab, scrollTop }))
    })),
  clearPendingCursor: () => set({ pendingCursor: null }),
  performAutosave: async (relativePath) => {
    clearAutosaveTimer(relativePath);
    const tab = get().tabs.find((item) => item.relativePath === relativePath);
    if (!tab || !tab.dirty || tab.saving || tab.conflict) return;
    get().setTabSaving(relativePath, true);
    try {
      const response = await writeWorkspaceFileAtomic({
        relativePath,
        content: tab.content,
        expectedRevision: tab.revision
      });
      get().markTabSaved(relativePath, response.revision);
    } catch (cause) {
      const error = normalizeWorkspaceError(cause);
      if (error.code === "conflict") get().setTabConflict(relativePath, error.message);
      else get().setTabError(relativePath, error.message);
    } finally {
      get().setTabSaving(relativePath, false);
    }
  },
  scheduleAutosave: (relativePath) => {
    clearAutosaveTimer(relativePath);
    autosaveTimers.set(relativePath, window.setTimeout(() => void get().performAutosave(relativePath), 1000));
  },
  resolveConflictByReload: async (relativePath) => {
    try {
      const response = await readWorkspaceFile({ relativePath });
      get().replaceTabFromDisk(relativePath, response.content, response.revision);
    } catch (cause) {
      get().setTabError(relativePath, normalizeWorkspaceError(cause).message);
    }
  },
  resolveConflictByOverwrite: async (relativePath) => {
    const tab = get().tabs.find((item) => item.relativePath === relativePath);
    if (!tab) return;
    get().setTabSaving(relativePath, true);
    try {
      const latest = await readWorkspaceFile({ relativePath });
      const current = get().tabs.find((item) => item.relativePath === relativePath);
      if (!current) return;
      const response = await writeWorkspaceFileAtomic({
        relativePath,
        content: current.content,
        expectedRevision: latest.revision
      });
      get().markTabSaved(relativePath, response.revision);
    } catch (cause) {
      const error = normalizeWorkspaceError(cause);
      if (error.code === "conflict") get().setTabConflict(relativePath, error.message);
      else get().setTabError(relativePath, error.message);
    } finally {
      get().setTabSaving(relativePath, false);
    }
  },
  handleExternalChange: async (event) => {
    const tab = get().tabs.find((item) => item.relativePath === event.relativePath);
    if (!tab) return;
    if (event.kind === "removed") {
      get().setTabError(event.relativePath, "The file was removed externally.");
      return;
    }
    if (tab.dirty || tab.saving) {
      get().setTabConflict(
        event.relativePath,
        "External changes were detected while this tab had local edits. Reload or overwrite explicitly."
      );
      return;
    }
    try {
      const latest = await readWorkspaceFile({ relativePath: event.relativePath });
      get().replaceTabFromDisk(event.relativePath, latest.content, latest.revision);
    } catch {
      // Temporary rename bursts from external editors are reconciled by the next watcher event.
    }
  },
  toSessionTabs: () =>
    get().tabs.map((tab) => ({
      relativePath: tab.relativePath,
      draftContent: tab.content === tab.baseContent ? null : tab.content,
      baseRevision: tab.revision,
      cursorLine: tab.cursorLine,
      cursorColumn: tab.cursorColumn,
      scrollTop: tab.scrollTop
    }))
}));
