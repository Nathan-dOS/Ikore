import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  GET_WORKSPACE_VAULT_INDEX_COMMAND,
  LIST_WORKSPACE_TREE_COMMAND,
  READ_WORKSPACE_FILE_COMMAND,
  RENAME_WORKSPACE_NOTE_COMMAND,
  SEARCH_WORKSPACE_TEXT_COMMAND,
  type GetWorkspaceVaultIndexResponse
} from "../ipc/contracts";
import { installTauriRuntime, tauriInvokeMock } from "../test/mocks/tauriMock";
import {
  buildVaultGraphLayout,
  selectContextPanelPaths,
  useVaultStore
} from "./vaultStore";

const index: GetWorkspaceVaultIndexResponse = {
  indexedAt: "2026-08-22T00:00:00Z",
  notes: [
    {
      relativePath: "Features/Login.md",
      title: "Login",
      revision: "r1",
      frontmatter: null,
      links: [],
      outgoingPaths: ["Architecture/Auth.md"],
      backlinkPaths: []
    },
    {
      relativePath: "Architecture/Auth.md",
      title: "Auth",
      revision: "r2",
      frontmatter: null,
      links: [],
      outgoingPaths: [],
      backlinkPaths: ["Features/Login.md"]
    }
  ],
  edges: [{ sourcePath: "Features/Login.md", targetPath: "Architecture/Auth.md" }],
  brokenLinks: []
};

describe("vaultStore", () => {
  beforeEach(() => {
    installTauriRuntime();
    vi.clearAllMocks();
    useVaultStore.setState({
      workspaceRoot: "C:\\vault",
      tree: [],
      vaultIndex: index,
      documents: {},
      loadingDocuments: [],
      selectedVaultPath: "Features/Login.md",
      searchQuery: "token",
      searchMode: "regex",
      searchCaseSensitive: true,
      searchResponse: null,
      error: null
    });
  });

  it("derives deterministic graph and context layouts", () => {
    expect(selectContextPanelPaths(index, "Features/Login.md")).toEqual([
      "Features/Login.md",
      "Architecture/Auth.md"
    ]);
    const layout = buildVaultGraphLayout(index);
    expect(layout.nodes).toHaveLength(2);
    expect(layout.edges).toHaveLength(1);
    expect(layout.edges[0].source.relativePath).toBe("Features/Login.md");
  });

  it("delegates substring or regex search through the typed IPC boundary", async () => {
    tauriInvokeMock.mockResolvedValue({
      matches: [{ relativePath: "Features/Login.md", line: 3, column: 4, lineText: "token" }],
      truncated: false,
      scannedFiles: 2
    });

    await useVaultStore.getState().search();

    expect(tauriInvokeMock).toHaveBeenCalledWith(SEARCH_WORKSPACE_TEXT_COMMAND, {
      request: { query: "token", mode: "regex", caseSensitive: true, maxResults: 200 }
    });
    expect(useVaultStore.getState().searchResponse?.matches).toHaveLength(1);
  });

  it("renames relative notes, updates cached documents, and refreshes tree and graph", async () => {
    useVaultStore.setState({ documents: { "Features/Login.md": "# Login" } });
    const renamedIndex: GetWorkspaceVaultIndexResponse = {
      ...index,
      notes: index.notes.map((note) =>
        note.relativePath === "Features/Login.md"
          ? { ...note, relativePath: "Features/Sign-in.md", title: "Sign-in" }
          : note
      ),
      edges: [{ sourcePath: "Features/Sign-in.md", targetPath: "Architecture/Auth.md" }]
    };
    tauriInvokeMock.mockImplementation(async (command) => {
      if (command === RENAME_WORKSPACE_NOTE_COMMAND) {
        return { renamedTo: "Features/Sign-in.md", updatedReferences: 1, affectedFiles: 1 };
      }
      if (command === LIST_WORKSPACE_TREE_COMMAND) return { tree: [] };
      if (command === GET_WORKSPACE_VAULT_INDEX_COMMAND) return renamedIndex;
      return {};
    });

    await useVaultStore.getState().renameNote("Features/Login.md", "Features/Sign-in.md");

    expect(tauriInvokeMock).toHaveBeenCalledWith(RENAME_WORKSPACE_NOTE_COMMAND, {
      request: {
        fromRelativePath: "Features/Login.md",
        toRelativePath: "Features/Sign-in.md"
      }
    });
    expect(useVaultStore.getState().documents).toEqual({ "Features/Sign-in.md": "# Login" });
    expect(useVaultStore.getState().selectedVaultPath).toBe("Features/Sign-in.md");
  });

  it("loads each context document once even when requests overlap", async () => {
    let resolveRead: ((value: unknown) => void) | undefined;
    tauriInvokeMock.mockImplementation((command) => {
      if (command !== READ_WORKSPACE_FILE_COMMAND) return Promise.resolve({});
      return new Promise((resolve) => { resolveRead = resolve; });
    });

    const first = useVaultStore.getState().ensureDocuments(["Architecture/Auth.md"]);
    const second = useVaultStore.getState().ensureDocuments(["Architecture/Auth.md"]);
    await vi.waitFor(() => expect(tauriInvokeMock).toHaveBeenCalledTimes(1));
    resolveRead?.({ content: "# Auth", revision: "r2" });
    await Promise.all([first, second]);

    expect(useVaultStore.getState().documents["Architecture/Auth.md"]).toBe("# Auth");
  });
});
