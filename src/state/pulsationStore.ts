import { create } from "zustand";

import {
  applyHitlDecision as applyHitlDecisionIpc,
  cancelPulsationJobs,
  createPulsation as createPulsationIpc,
  getPulsationRuntimeSnapshot,
  listPulsationArtifacts,
  normalizeWorkspaceError
} from "../ipc/client";
import type {
  Artifact,
  HitlDecision,
  Project,
  Pulsation,
  PulsationRuntimeSnapshot,
  PulsationStage,
  PulsationStatus,
  WorkspaceViewMode
} from "../ipc/contracts";

export type { HitlDecision, Pulsation, PulsationStage, PulsationStatus, WorkspaceViewMode };

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
  return manualOverride ?? (status === "checkpoint" ? "hitl" : "editor");
}

function selectNextActivePulsationId(
  pulsations: Pulsation[],
  preferredId: string | null,
  currentActiveId: string | null
): string | null {
  if (preferredId && pulsations.some((item) => item.id === preferredId)) return preferredId;
  if (currentActiveId && pulsations.some((item) => item.id === currentActiveId)) return currentActiveId;
  return pulsations[0]?.id ?? null;
}

interface PulsationStoreState {
  activeProject: Project | null;
  projectId: string | null;
  pulsations: Pulsation[];
  activePulsationId: string | null;
  workspaceViewMode: WorkspaceViewMode;
  workspaceViewOverride: WorkspaceViewMode | null;
  hitlWaitingCount: number;
  backgroundAgentsCount: number;
  artifacts: Artifact[];
  runtimeLoading: boolean;
  artifactsLoading: boolean;
  runtimeError: string | null;
  notice: string | null;
  setActiveProject: (project: Project | null) => void;
  setActivePulsation: (id: string) => void;
  setWorkspaceViewOverride: (mode: WorkspaceViewMode) => void;
  clearWorkspaceViewOverride: () => void;
  setNotice: (message: string | null) => void;
  resetRuntime: () => void;
  loadProjectRuntime: (projectId: string, preferredActiveId?: string | null) => Promise<void>;
  refreshRuntime: (preferredActiveId?: string | null) => Promise<void>;
  loadArtifacts: (pulsationId?: string | null) => Promise<void>;
  createPulsationForProject: (projectId: string, featureName: string) => Promise<void>;
  applyHitlDecision: (decision: HitlDecision) => Promise<void>;
  cancelActiveJobs: () => Promise<void>;
  retryActiveGeneration: () => Promise<void>;
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
  activeProject: null,
  projectId: null,
  pulsations: [],
  activePulsationId: null,
  workspaceViewMode: "editor",
  workspaceViewOverride: null,
  hitlWaitingCount: 0,
  backgroundAgentsCount: 0,
  artifacts: [],
  runtimeLoading: false,
  artifactsLoading: false,
  runtimeError: null,
  notice: null,
  setActiveProject: (activeProject) => set({ activeProject, projectId: activeProject?.id ?? null }),
  setActivePulsation: (id) => {
    set((state) => {
      const active = state.pulsations.find((item) => item.id === id);
      if (!active) return state;
      return {
        activePulsationId: id,
        workspaceViewMode: deriveWorkspaceMode(active.status, state.workspaceViewOverride),
        artifacts: []
      };
    });
    void get().loadArtifacts(id);
  },
  setWorkspaceViewOverride: (workspaceViewMode) =>
    set({ workspaceViewOverride: workspaceViewMode, workspaceViewMode }),
  clearWorkspaceViewOverride: () =>
    set((state) => {
      const active = state.pulsations.find((item) => item.id === state.activePulsationId);
      return {
        workspaceViewOverride: null,
        workspaceViewMode: deriveWorkspaceMode(active?.status, null)
      };
    }),
  setNotice: (notice) => set({ notice }),
  resetRuntime: () =>
    set({
      activeProject: null,
      projectId: null,
      pulsations: [],
      activePulsationId: null,
      hitlWaitingCount: 0,
      backgroundAgentsCount: 0,
      artifacts: [],
      workspaceViewOverride: null,
      workspaceViewMode: "editor",
      runtimeLoading: false,
      artifactsLoading: false,
      runtimeError: null,
      notice: null
    }),
  loadProjectRuntime: async (projectId, preferredActiveId = null) => {
    set({ runtimeLoading: true });
    try {
      const response = await getPulsationRuntimeSnapshot({ projectId });
      set((state) => ({
        ...applySnapshot(state, projectId, response.snapshot, preferredActiveId),
        runtimeLoading: false
      }));
      await get().loadArtifacts(get().activePulsationId);
    } catch (cause) {
      const runtimeError = normalizeWorkspaceError(cause).message;
      set({ runtimeLoading: false, runtimeError });
      throw cause;
    }
  },
  refreshRuntime: async (preferredActiveId = null) => {
    const { projectId } = get();
    if (!projectId) return;
    try {
      const response = await getPulsationRuntimeSnapshot({ projectId });
      set((state) => applySnapshot(state, projectId, response.snapshot, preferredActiveId));
      await get().loadArtifacts();
    } catch (cause) {
      set({ runtimeError: normalizeWorkspaceError(cause).message });
      throw cause;
    }
  },
  loadArtifacts: async (pulsationId = get().activePulsationId) => {
    if (!pulsationId) {
      set({ artifacts: [], artifactsLoading: false });
      return;
    }
    set({ artifactsLoading: true });
    try {
      const response = await listPulsationArtifacts({ pulsationId });
      if (get().activePulsationId === pulsationId) set({ artifacts: response.artifacts });
    } catch (cause) {
      set({ runtimeError: normalizeWorkspaceError(cause).message });
    } finally {
      set({ artifactsLoading: false });
    }
  },
  createPulsationForProject: async (projectId, featureName) => {
    set({ runtimeLoading: true, notice: null });
    try {
      const created = await createPulsationIpc({ projectId, featureName });
      const response = await getPulsationRuntimeSnapshot({ projectId });
      set((state) => ({
        ...applySnapshot(state, projectId, response.snapshot, created.pulsation.id),
        runtimeLoading: false,
        notice: `Pulsation “${created.pulsation.featureName}” started.`
      }));
      await get().loadArtifacts(created.pulsation.id);
    } catch (cause) {
      const runtimeError = normalizeWorkspaceError(cause).message;
      set({ runtimeLoading: false, runtimeError });
      throw cause;
    }
  },
  applyHitlDecision: async (decision) => {
    const { activePulsationId, projectId } = get();
    if (!activePulsationId || !projectId) return;
    set({ runtimeLoading: true, notice: null });
    try {
      await applyHitlDecisionIpc({ pulsationId: activePulsationId, decision });
      const response = await getPulsationRuntimeSnapshot({ projectId });
      set((state) => ({
        ...applySnapshot(state, projectId, response.snapshot, activePulsationId),
        workspaceViewOverride: null,
        workspaceViewMode: deriveWorkspaceMode(
          response.snapshot.pulsations.find((item) => item.id === activePulsationId)?.status,
          null
        ),
        runtimeLoading: false,
        notice: `HITL decision applied: ${decision.replace("_", " ")}.`
      }));
    } catch (cause) {
      const runtimeError = normalizeWorkspaceError(cause).message;
      set({ runtimeLoading: false, runtimeError });
      throw cause;
    }
  },
  cancelActiveJobs: async () => {
    const pulsationId = get().activePulsationId;
    if (!pulsationId) return;
    set({ runtimeLoading: true, notice: null });
    try {
      const response = await cancelPulsationJobs({ pulsationId });
      set({ notice: `${response.cancelledJobs} job(s) marked for cancellation.` });
      await get().refreshRuntime(pulsationId);
    } catch (cause) {
      set({ runtimeError: normalizeWorkspaceError(cause).message });
    } finally {
      set({ runtimeLoading: false });
    }
  },
  retryActiveGeneration: async () => {
    const { activeProject, pulsations, activePulsationId } = get();
    const active = pulsations.find((item) => item.id === activePulsationId);
    if (!activeProject || !active) return;
    await get().createPulsationForProject(activeProject.id, active.featureName);
    set({ notice: "A new generation started for the same feature." });
  }
}));

export function selectActivePulsation(state: PulsationStoreState): Pulsation | null {
  return state.pulsations.find((item) => item.id === state.activePulsationId) ?? state.pulsations[0] ?? null;
}
