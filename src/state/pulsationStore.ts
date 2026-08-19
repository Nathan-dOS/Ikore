import { create } from "zustand";

import {
  applyHitlDecision as applyHitlDecisionIpc,
  createPulsation as createPulsationIpc,
  getPulsationRuntimeSnapshot
} from "../ipc/client";
import type {
  HitlDecision,
  Pulsation,
  PulsationRuntimeSnapshot,
  PulsationStage,
  PulsationStatus
} from "../ipc/contracts";

export type { HitlDecision, Pulsation, PulsationStage, PulsationStatus };

export type WorkspaceViewMode = "editor" | "hitl";

export const PIPELINE_STAGES: PulsationStage[] = [
  "Intake",
  "SPECIFICATION",
  "Contract",
  "Code",
  "Test",
  "Integrator"
];

function deriveWorkspaceMode(
  status: PulsationStatus | undefined,
  manualOverride: WorkspaceViewMode | null
): WorkspaceViewMode {
  if (manualOverride) {
    return manualOverride;
  }

  return status === "checkpoint" ? "hitl" : "editor";
}

function selectNextActivePulsationId(
  pulsations: Pulsation[],
  preferredId: string | null,
  currentActiveId: string | null
): string | null {
  if (preferredId && pulsations.some((item) => item.id === preferredId)) {
    return preferredId;
  }

  if (currentActiveId && pulsations.some((item) => item.id === currentActiveId)) {
    return currentActiveId;
  }

  return pulsations[0]?.id ?? null;
}

interface PulsationStoreState {
  projectId: string | null;
  pulsations: Pulsation[];
  activePulsationId: string | null;
  workspaceViewMode: WorkspaceViewMode;
  workspaceViewOverride: WorkspaceViewMode | null;
  hitlWaitingCount: number;
  backgroundAgentsCount: number;
  sessionStartedAt: number;
  runtimeLoading: boolean;
  runtimeError: string | null;
  setActivePulsation: (id: string) => void;
  setWorkspaceViewOverride: (mode: WorkspaceViewMode) => void;
  clearWorkspaceViewOverride: () => void;
  resetRuntime: () => void;
  loadProjectRuntime: (projectId: string, preferredActiveId?: string | null) => Promise<void>;
  refreshRuntime: (preferredActiveId?: string | null) => Promise<void>;
  createPulsationForProject: (projectId: string, featureName: string) => Promise<void>;
  applyHitlDecision: (decision: HitlDecision) => Promise<void>;
}

function applySnapshot(
  state: PulsationStoreState,
  projectId: string,
  snapshot: PulsationRuntimeSnapshot,
  preferredActiveId: string | null = null
) {
  const activePulsationId = selectNextActivePulsationId(
    snapshot.pulsations,
    preferredActiveId,
    state.activePulsationId
  );
  const active = snapshot.pulsations.find((item) => item.id === activePulsationId);

  return {
    projectId,
    pulsations: snapshot.pulsations,
    activePulsationId,
    workspaceViewMode: deriveWorkspaceMode(active?.status, state.workspaceViewOverride),
    hitlWaitingCount: snapshot.hitlWaitingCount,
    backgroundAgentsCount: snapshot.backgroundAgentsCount,
    runtimeError: null
  };
}

export const usePulsationStore = create<PulsationStoreState>((set, get) => ({
  projectId: null,
  pulsations: [],
  activePulsationId: null,
  workspaceViewMode: "editor",
  workspaceViewOverride: null,
  hitlWaitingCount: 0,
  backgroundAgentsCount: 0,
  sessionStartedAt: Date.now(),
  runtimeLoading: false,
  runtimeError: null,
  setActivePulsation: (id) =>
    set((state) => {
      const active = state.pulsations.find((item) => item.id === id);
      if (!active) {
        return state;
      }

      return {
        activePulsationId: id,
        workspaceViewMode: deriveWorkspaceMode(active.status, state.workspaceViewOverride)
      };
    }),
  setWorkspaceViewOverride: (mode) =>
    set({
      workspaceViewOverride: mode,
      workspaceViewMode: mode
    }),
  clearWorkspaceViewOverride: () =>
    set((state) => {
      const active = state.pulsations.find((item) => item.id === state.activePulsationId);
      return {
        workspaceViewOverride: null,
        workspaceViewMode: deriveWorkspaceMode(active?.status, null)
      };
    }),
  resetRuntime: () =>
    set((state) => ({
      projectId: null,
      pulsations: [],
      activePulsationId: null,
      hitlWaitingCount: 0,
      backgroundAgentsCount: 0,
      workspaceViewOverride: null,
      workspaceViewMode: "editor",
      runtimeLoading: false,
      runtimeError: null,
      sessionStartedAt: state.sessionStartedAt
    })),
  loadProjectRuntime: async (projectId, preferredActiveId = null) => {
    set({ runtimeLoading: true });
    try {
      const response = await getPulsationRuntimeSnapshot({ projectId });
      set((state) => ({
        ...applySnapshot(state, projectId, response.snapshot, preferredActiveId),
        runtimeLoading: false
      }));
    } catch (error) {
      const message = error instanceof Error ? error.message : "Falha ao carregar pulsações.";
      set({ runtimeLoading: false, runtimeError: message });
      throw error;
    }
  },
  refreshRuntime: async (preferredActiveId = null) => {
    const { projectId } = get();
    if (!projectId) {
      return;
    }

    try {
      const response = await getPulsationRuntimeSnapshot({ projectId });
      set((state) => ({
        ...applySnapshot(state, projectId, response.snapshot, preferredActiveId)
      }));
    } catch (error) {
      const message = error instanceof Error ? error.message : "Falha ao atualizar runtime.";
      set({ runtimeError: message });
      throw error;
    }
  },
  createPulsationForProject: async (projectId, featureName) => {
    set({ runtimeLoading: true });
    try {
      const created = await createPulsationIpc({ projectId, featureName });
      const response = await getPulsationRuntimeSnapshot({ projectId });
      set((state) => ({
        ...applySnapshot(state, projectId, response.snapshot, created.pulsation.id),
        runtimeLoading: false
      }));
    } catch (error) {
      const message = error instanceof Error ? error.message : "Falha ao criar pulsação.";
      set({ runtimeLoading: false, runtimeError: message });
      throw error;
    }
  },
  applyHitlDecision: async (decision) => {
    const { activePulsationId, projectId } = get();
    if (!activePulsationId || !projectId) {
      return;
    }

    set({ runtimeLoading: true });
    try {
      await applyHitlDecisionIpc({
        pulsationId: activePulsationId,
        decision
      });
      const response = await getPulsationRuntimeSnapshot({ projectId });
      set((state) => ({
        ...applySnapshot(state, projectId, response.snapshot, activePulsationId),
        workspaceViewOverride: null,
        workspaceViewMode: deriveWorkspaceMode(
          response.snapshot.pulsations.find((item) => item.id === activePulsationId)?.status,
          null
        ),
        runtimeLoading: false
      }));
    } catch (error) {
      const message = error instanceof Error ? error.message : "Falha ao aplicar decisão HITL.";
      set({ runtimeLoading: false, runtimeError: message });
      throw error;
    }
  }
}));
