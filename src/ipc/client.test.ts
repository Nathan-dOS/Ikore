import { beforeEach, describe, expect, it } from "vitest";

import {
  normalizeWorkspaceError,
  openWorkspace,
  upsertProviderConnection,
  WorkspaceClientError
} from "./client";
import { OPEN_WORKSPACE_COMMAND, UPSERT_PROVIDER_CONNECTION_COMMAND } from "./contracts";
import {
  installTauriRuntime,
  tauriInvokeMock
} from "../test/mocks/tauriMock";

describe("IPC client", () => {
  beforeEach(() => {
    installTauriRuntime();
  });

  it("normalizes a serialized domain error", () => {
    expect(
      normalizeWorkspaceError('{"code":"conflict","message":"Revisão divergente."}')
    ).toEqual({
      code: "conflict",
      message: "Revisão divergente."
    });
  });

  it("falls back to io_error for a regular Error", () => {
    expect(normalizeWorkspaceError(new Error("Falha local."))).toEqual({
      code: "io_error",
      message: "Falha local."
    });
  });

  it("preserves enriched LLM error details and remediation", () => {
    expect(
      normalizeWorkspaceError({
        code: "missing_api_key",
        message: "Conexão sem API key.",
        details: { connectionId: "connection-1" },
        remediation: "Cadastre uma API key."
      })
    ).toEqual({
      code: "missing_api_key",
      message: "Conexão sem API key.",
      details: { connectionId: "connection-1" },
      remediation: "Cadastre uma API key."
    });
  });

  it("invokes open_workspace with the request envelope", async () => {
    tauriInvokeMock.mockResolvedValue({
      rootPath: "C:/workspace",
      tree: [],
      greenfield: {
        mode: "existing",
        initialized: false,
        message: "Workspace existente.",
        createdPaths: []
      }
    });

    await openWorkspace({ path: "C:/workspace" });

    expect(tauriInvokeMock).toHaveBeenCalledWith(OPEN_WORKSPACE_COMMAND, {
      request: { path: "C:/workspace" }
    });
  });

  it("turns a backend domain error into WorkspaceClientError", async () => {
    tauriInvokeMock.mockRejectedValue({
      code: "invalid_root",
      message: "A pasta selecionada não existe."
    });

    const promise = openWorkspace({ path: "C:/missing" });

    await expect(promise).rejects.toMatchObject({
      name: "WorkspaceClientError",
      code: "invalid_root",
      message: "A pasta selecionada não existe."
    } satisfies Partial<WorkspaceClientError>);
  });

  it("sends a connection secret only in the write request", async () => {
    tauriInvokeMock.mockResolvedValue({
      connection: {
        id: "connection-1",
        providerId: "openai",
        displayName: "Primary",
        endpointUrl: null,
        defaultModel: "gpt-4.1-mini",
        enabled: true,
        state: "active",
        blockedReason: null,
        cooldownUntil: null,
        maxConcurrency: 1,
        version: 1,
        hasApiKey: true,
        createdAt: "2026-08-18T00:00:00Z",
        updatedAt: "2026-08-18T00:00:00Z"
      }
    });

    const response = await upsertProviderConnection({
      providerId: "openai",
      displayName: "Primary",
      defaultModel: "gpt-4.1-mini",
      apiKey: "write-only-secret"
    });

    expect(tauriInvokeMock).toHaveBeenCalledWith(UPSERT_PROVIDER_CONNECTION_COMMAND, {
      request: expect.objectContaining({ apiKey: "write-only-secret" })
    });
    expect(JSON.stringify(response)).not.toContain("write-only-secret");
    expect(JSON.stringify(response)).not.toContain("credentialRef");
  });
});
