import { create } from "zustand";

export interface EditorConflictState {
  message: string;
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
  openTab: (payload: OpenTabPayload) => void;
  setActiveTab: (relativePath: string) => void;
  closeTab: (relativePath: string) => void;
  resetTabs: () => void;
  updateTabContent: (relativePath: string, content: string) => void;
  setTabSaving: (relativePath: string, saving: boolean) => void;
  markTabSaved: (relativePath: string, revision: string) => void;
  setTabConflict: (relativePath: string, message: string) => void;
  clearTabConflict: (relativePath: string) => void;
  replaceTabFromDisk: (relativePath: string, content: string, revision: string) => void;
  setTabError: (relativePath: string, message: string | null) => void;
  setTabCursor: (relativePath: string, line: number | null, column: number | null) => void;
  setTabScrollTop: (relativePath: string, scrollTop: number | null) => void;
}

function updateTab(
  tabs: EditorTab[],
  relativePath: string,
  updater: (tab: EditorTab) => EditorTab
): EditorTab[] {
  return tabs.map((tab) => (tab.relativePath === relativePath ? updater(tab) : tab));
}

export const useEditorStore = create<EditorStoreState>((set) => ({
  tabs: [],
  activeTabPath: null,
  openTab: (payload) =>
    set((state) => {
      const existing = state.tabs.find((tab) => tab.relativePath === payload.relativePath);
      if (existing) {
        return { activeTabPath: payload.relativePath };
      }

      return {
        tabs: [
          ...state.tabs,
          {
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
          }
        ],
        activeTabPath: payload.relativePath
      };
    }),
  setActiveTab: (relativePath) => set({ activeTabPath: relativePath }),
  closeTab: (relativePath) =>
    set((state) => {
      const tabs = state.tabs.filter((tab) => tab.relativePath !== relativePath);
      if (state.activeTabPath !== relativePath) {
        return { tabs };
      }

      return {
        tabs,
        activeTabPath: tabs.length > 0 ? tabs[tabs.length - 1].relativePath : null
      };
    }),
  resetTabs: () => set({ tabs: [], activeTabPath: null }),
  updateTabContent: (relativePath, content) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) => {
        if (tab.content === content) {
          return tab;
        }

        return {
          ...tab,
          content,
          dirty: content !== tab.baseContent,
          saveError: null
        };
      })
    })),
  setTabSaving: (relativePath, saving) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) => ({
        ...tab,
        saving
      }))
    })),
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
      tabs: updateTab(state.tabs, relativePath, (tab) => ({
        ...tab,
        conflict: null
      }))
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
      tabs: updateTab(state.tabs, relativePath, (tab) => ({
        ...tab,
        saving: false,
        saveError: message
      }))
    })),
  setTabCursor: (relativePath, line, column) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) => ({
        ...tab,
        cursorLine: line,
        cursorColumn: column
      }))
    })),
  setTabScrollTop: (relativePath, scrollTop) =>
    set((state) => ({
      tabs: updateTab(state.tabs, relativePath, (tab) => ({
        ...tab,
        scrollTop
      }))
    }))
}));
