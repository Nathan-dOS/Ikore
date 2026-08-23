import { create } from "zustand";

import {
  getWorkspaceVaultIndex,
  listWorkspaceTree,
  normalizeWorkspaceError,
  readWorkspaceFile,
  renameWorkspaceNote,
  searchWorkspaceText
} from "../ipc/client";
import type {
  FileNode,
  GetWorkspaceVaultIndexResponse,
  OpenWorkspaceResponse,
  SearchMode,
  SearchWorkspaceTextResponse
} from "../ipc/contracts";

interface VaultStoreState {
  workspaceRoot: string | null;
  tree: FileNode[];
  vaultIndex: GetWorkspaceVaultIndexResponse | null;
  documents: Record<string, string>;
  loadingDocuments: string[];
  selectedVaultPath: string | null;
  searchQuery: string;
  searchMode: SearchMode;
  searchCaseSensitive: boolean;
  searchResponse: SearchWorkspaceTextResponse | null;
  refreshingTree: boolean;
  indexing: boolean;
  searching: boolean;
  error: string | null;
  hydrateWorkspace: (response: OpenWorkspaceResponse) => void;
  resetWorkspace: () => void;
  refreshTree: () => Promise<void>;
  refreshIndex: (forceRebuild?: boolean) => Promise<void>;
  setSelectedVaultPath: (relativePath: string | null) => void;
  restoreSelection: (relativePath: string | null) => void;
  setSearchQuery: (query: string) => void;
  setSearchMode: (mode: SearchMode) => void;
  setSearchCaseSensitive: (caseSensitive: boolean) => void;
  search: () => Promise<void>;
  loadDocument: (relativePath: string) => Promise<string>;
  ensureDocuments: (relativePaths: string[]) => Promise<void>;
  renameNote: (fromRelativePath: string, toRelativePath: string) => Promise<void>;
}

export const useVaultStore = create<VaultStoreState>((set, get) => ({
  workspaceRoot: null,
  tree: [],
  vaultIndex: null,
  documents: {},
  loadingDocuments: [],
  selectedVaultPath: null,
  searchQuery: "",
  searchMode: "substring",
  searchCaseSensitive: false,
  searchResponse: null,
  refreshingTree: false,
  indexing: false,
  searching: false,
  error: null,
  hydrateWorkspace: (response) =>
    set({
      workspaceRoot: response.rootPath,
      tree: response.tree,
      vaultIndex: null,
      documents: {},
      loadingDocuments: [],
      selectedVaultPath: null,
      searchResponse: null,
      refreshingTree: false,
      indexing: false,
      searching: false,
      error: null
    }),
  resetWorkspace: () =>
    set({
      workspaceRoot: null,
      tree: [],
      vaultIndex: null,
      documents: {},
      loadingDocuments: [],
      selectedVaultPath: null,
      searchResponse: null,
      error: null
    }),
  refreshTree: async () => {
    if (!get().workspaceRoot) return;
    set({ refreshingTree: true });
    try {
      const response = await listWorkspaceTree();
      set({ tree: response.tree, error: null });
    } catch (cause) {
      set({ error: normalizeWorkspaceError(cause).message });
    } finally {
      set({ refreshingTree: false });
    }
  },
  refreshIndex: async (forceRebuild = false) => {
    if (!get().workspaceRoot) {
      set({ vaultIndex: null });
      return;
    }
    set({ indexing: true });
    try {
      const response = await getWorkspaceVaultIndex({ forceRebuild });
      const selected = get().selectedVaultPath;
      set({
        vaultIndex: response,
        selectedVaultPath:
          selected && response.notes.some((note) => note.relativePath === selected)
            ? selected
            : response.notes[0]?.relativePath ?? null,
        error: null
      });
    } catch (cause) {
      set({ error: normalizeWorkspaceError(cause).message });
    } finally {
      set({ indexing: false });
    }
  },
  setSelectedVaultPath: (selectedVaultPath) => set({ selectedVaultPath }),
  restoreSelection: (relativePath) => {
    const index = get().vaultIndex;
    set({
      selectedVaultPath:
        relativePath && index?.notes.some((note) => note.relativePath === relativePath)
          ? relativePath
          : index?.notes[0]?.relativePath ?? null
    });
  },
  setSearchQuery: (searchQuery) => set({ searchQuery }),
  setSearchMode: (searchMode) => set({ searchMode }),
  setSearchCaseSensitive: (searchCaseSensitive) => set({ searchCaseSensitive }),
  search: async () => {
    const { workspaceRoot, searchQuery, searchMode, searchCaseSensitive } = get();
    if (!workspaceRoot || !searchQuery.trim()) {
      set({ searchResponse: null });
      return;
    }
    set({ searching: true });
    try {
      const searchResponse = await searchWorkspaceText({
        query: searchQuery.trim(),
        mode: searchMode,
        caseSensitive: searchCaseSensitive,
        maxResults: 200
      });
      set({ searchResponse, error: null });
    } catch (cause) {
      set({ error: normalizeWorkspaceError(cause).message });
    } finally {
      set({ searching: false });
    }
  },
  loadDocument: async (relativePath) => {
    const cached = get().documents[relativePath];
    if (cached !== undefined) return cached;
    const response = await readWorkspaceFile({ relativePath });
    set((state) => ({ documents: { ...state.documents, [relativePath]: response.content } }));
    return response.content;
  },
  ensureDocuments: async (relativePaths) => {
    const uniquePaths = [...new Set(relativePaths)];
    const pendingPaths = uniquePaths.filter(
      (relativePath) =>
        get().documents[relativePath] === undefined &&
        !get().loadingDocuments.includes(relativePath)
    );
    if (!pendingPaths.length) return;
    set((state) => ({
      loadingDocuments: [...new Set([...state.loadingDocuments, ...pendingPaths])]
    }));
    const loaded = await Promise.all(
      pendingPaths.map(async (relativePath) => {
        try {
          const response = await readWorkspaceFile({ relativePath });
          return { relativePath, content: response.content };
        } catch {
          return null;
        }
      })
    );
    set((state) => {
      const documents = { ...state.documents };
      loaded.forEach((item) => {
        if (item) documents[item.relativePath] = item.content;
      });
      return {
        documents,
        loadingDocuments: state.loadingDocuments.filter(
          (relativePath) => !pendingPaths.includes(relativePath)
        )
      };
    });
  },
  renameNote: async (fromRelativePath, toRelativePath) => {
    await renameWorkspaceNote({ fromRelativePath, toRelativePath });
    set((state) => {
      const documents = { ...state.documents };
      if (documents[fromRelativePath] !== undefined) {
        documents[toRelativePath] = documents[fromRelativePath];
        delete documents[fromRelativePath];
      }
      return {
        documents,
        selectedVaultPath:
          state.selectedVaultPath === fromRelativePath ? toRelativePath : state.selectedVaultPath
      };
    });
    await Promise.all([get().refreshTree(), get().refreshIndex(true)]);
  }
}));

export interface VaultGraphNodeLayout {
  relativePath: string;
  title: string;
  x: number;
  y: number;
}

export function selectContextPanelPaths(
  index: GetWorkspaceVaultIndexResponse | null,
  activeTabPath: string | null
): string[] {
  const paths: string[] = [];
  const add = (relativePath?: string | null) => {
    if (relativePath && !paths.includes(relativePath)) paths.push(relativePath);
  };
  add(activeTabPath);
  index?.notes
    .find((note) => note.relativePath === activeTabPath)
    ?.outgoingPaths.forEach(add);
  index?.notes.forEach((note) => add(note.relativePath));
  return paths.slice(0, 4);
}

export interface VaultGraphLayout {
  nodes: VaultGraphNodeLayout[];
  edges: Array<{ source: VaultGraphNodeLayout; target: VaultGraphNodeLayout }>;
}

export function buildVaultGraphLayout(index: GetWorkspaceVaultIndexResponse | null): VaultGraphLayout {
  if (!index?.notes.length) return { nodes: [], edges: [] };
  const width = 760;
  const height = 460;
  const columns = Math.max(1, Math.ceil(Math.sqrt(index.notes.length)));
  const rows = Math.ceil(index.notes.length / columns);
  const nodes = index.notes.map((note, position) => ({
    relativePath: note.relativePath,
    title: note.title.trim() || note.relativePath.replace(/\.md$/i, ""),
    x: ((position % columns) + 0.5) * (width / columns),
    y: (Math.floor(position / columns) + 0.5) * (height / rows)
  }));
  const nodesByPath = new Map(nodes.map((node) => [node.relativePath, node]));
  const edges = index.edges.flatMap((edge) => {
    const source = nodesByPath.get(edge.sourcePath);
    const target = nodesByPath.get(edge.targetPath);
    return source && target ? [{ source, target }] : [];
  });
  return { nodes, edges };
}
