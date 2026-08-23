import { create } from "zustand";

import type { AppView, UiSessionState } from "../ipc/contracts";

export interface ViewDefinition {
  id: AppView;
  label: string;
  shortLabel: string;
  description: string;
  glyph: string;
}

export const APP_VIEWS: readonly ViewDefinition[] = [
  {
    id: "workspace",
    label: "Workspace & HITL",
    shortLabel: "Workspace",
    description: "Edit files and review active checkpoints.",
    glyph: "W"
  },
  {
    id: "pulsations",
    label: "Pulsations & Pipeline",
    shortLabel: "Pulsations",
    description: "Create, inspect, and control orchestration flows.",
    glyph: "P"
  },
  {
    id: "vault",
    label: "Vault & Graph",
    shortLabel: "Vault",
    description: "Explore notes, wikilinks, metadata, and search.",
    glyph: "V"
  },
  {
    id: "chat",
    label: "Chat / Companion",
    shortLabel: "Companion",
    description: "Discuss architecture and code with read-only repository context.",
    glyph: "C"
  },
  {
    id: "settings",
    label: "Settings & AI Runtime",
    shortLabel: "Settings",
    description: "Configure providers, routing, context, and embeddings.",
    glyph: "S"
  },
  {
    id: "logs",
    label: "Logs & Observability",
    shortLabel: "Logs",
    description: "Inspect runs, costs, outputs, and live events.",
    glyph: "L"
  }
] as const;

interface ViewStoreState {
  activeView: AppView;
  sidebarCollapsed: boolean;
  contextDrawerOpen: boolean;
  diffExpanded: boolean;
  sessionStartedAt: number;
  tauriAvailable: boolean;
  loadingWorkspace: boolean;
  globalError: string | null;
  persistenceWarning: string | null;
  notice: string | null;
  setActiveView: (view: AppView) => void;
  setSidebarCollapsed: (collapsed: boolean) => void;
  toggleSidebar: () => void;
  setContextDrawerOpen: (open: boolean) => void;
  toggleContextDrawer: () => void;
  setDiffExpanded: (expanded: boolean) => void;
  setTauriAvailable: (available: boolean) => void;
  setLoadingWorkspace: (loading: boolean) => void;
  setGlobalError: (message: string | null) => void;
  setPersistenceWarning: (message: string | null) => void;
  setNotice: (message: string | null) => void;
  hydrateLayout: (session: UiSessionState | null) => void;
}

export const useViewStore = create<ViewStoreState>((set) => ({
  activeView: "workspace",
  sidebarCollapsed: false,
  contextDrawerOpen: true,
  diffExpanded: false,
  sessionStartedAt: Date.now(),
  tauriAvailable: false,
  loadingWorkspace: false,
  globalError: null,
  persistenceWarning: null,
  notice: null,
  setActiveView: (activeView) => set({ activeView, diffExpanded: false }),
  setSidebarCollapsed: (sidebarCollapsed) => set({ sidebarCollapsed }),
  toggleSidebar: () => set((state) => ({ sidebarCollapsed: !state.sidebarCollapsed })),
  setContextDrawerOpen: (contextDrawerOpen) => set({ contextDrawerOpen }),
  toggleContextDrawer: () => set((state) => ({ contextDrawerOpen: !state.contextDrawerOpen })),
  setDiffExpanded: (diffExpanded) => set({ diffExpanded }),
  setTauriAvailable: (tauriAvailable) => set({ tauriAvailable }),
  setLoadingWorkspace: (loadingWorkspace) => set({ loadingWorkspace }),
  setGlobalError: (globalError) => set({ globalError }),
  setPersistenceWarning: (persistenceWarning) => set({ persistenceWarning }),
  setNotice: (notice) => set({ notice }),
  hydrateLayout: (session) =>
    set({
      activeView: session?.activeView ?? "workspace",
      sidebarCollapsed: session?.sidebarCollapsed ?? false,
      contextDrawerOpen: session?.contextDrawerOpen ?? true,
      diffExpanded: false
    })
}));

export function getActiveViewDefinition(view: AppView): ViewDefinition {
  return APP_VIEWS.find((definition) => definition.id === view) ?? APP_VIEWS[0];
}
