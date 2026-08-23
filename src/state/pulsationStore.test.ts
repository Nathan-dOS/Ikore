import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  GET_PULSATION_RUNTIME_SNAPSHOT_COMMAND,
  LIST_PULSATION_ARTIFACTS_COMMAND,
  type Artifact,
  type Pulsation
} from "../ipc/contracts";
import { installTauriRuntime, tauriInvokeMock } from "../test/mocks/tauriMock";
import { usePulsationStore } from "./pulsationStore";

const pulsation: Pulsation = {
  id: "pulse-1",
  projectId: "project-1",
  featureName: "Login",
  stage: "Code",
  status: "checkpoint",
  createdAt: "2026-08-22T00:00:00Z",
  updatedAt: "2026-08-22T00:01:00Z"
};

const firstArtifact: Artifact = {
  id: "artifact-1",
  pulsationId: "pulse-1",
  name: "Login plan",
  kind: "markdown",
  relativePath: "Features/Login.md",
  contentSha256: "abc",
  createdAt: "2026-08-22T00:01:00Z",
  updatedAt: "2026-08-22T00:01:00Z"
};

describe("pulsationStore", () => {
  let artifacts: Artifact[];

  beforeEach(() => {
    installTauriRuntime();
    vi.clearAllMocks();
    artifacts = [firstArtifact];
    usePulsationStore.getState().resetRuntime();
    tauriInvokeMock.mockImplementation(async (command) => {
      if (command === GET_PULSATION_RUNTIME_SNAPSHOT_COMMAND) {
        return {
          snapshot: { pulsations: [pulsation], hitlWaitingCount: 1, backgroundAgentsCount: 2 }
        };
      }
      if (command === LIST_PULSATION_ARTIFACTS_COMMAND) return { artifacts };
      return {};
    });
  });

  it("hydrates the active checkpoint, counters, and artifacts", async () => {
    await usePulsationStore.getState().loadProjectRuntime("project-1", "pulse-1");

    const state = usePulsationStore.getState();
    expect(state.activePulsationId).toBe("pulse-1");
    expect(state.workspaceViewMode).toBe("hitl");
    expect(state.hitlWaitingCount).toBe(1);
    expect(state.backgroundAgentsCount).toBe(2);
    expect(state.artifacts).toEqual([firstArtifact]);
  });

  it("refreshes artifacts for the same selected pulsation", async () => {
    await usePulsationStore.getState().loadProjectRuntime("project-1", "pulse-1");
    artifacts = [{ ...firstArtifact, id: "artifact-2", name: "Generated code" }];

    await usePulsationStore.getState().refreshRuntime();

    expect(usePulsationStore.getState().artifacts[0].name).toBe("Generated code");
    expect(tauriInvokeMock.mock.calls.filter(([command]) => command === LIST_PULSATION_ARTIFACTS_COMMAND)).toHaveLength(2);
  });
});
