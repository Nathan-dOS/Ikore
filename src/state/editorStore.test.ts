import { beforeEach, describe, expect, it } from "vitest";

import { useEditorStore } from "./editorStore";

function openNote(relativePath: string, content = "Inicial"): void {
  const pathSegments = relativePath.split("/");
  useEditorStore.getState().openTab({
    relativePath,
    name: pathSegments[pathSegments.length - 1] ?? relativePath,
    content,
    revision: "rev-1"
  });
}

describe("editorStore", () => {
  beforeEach(() => {
    useEditorStore.getState().resetTabs();
  });

  it("opens a tab once and makes it active", () => {
    openNote("notes/one.md");
    openNote("notes/one.md", "Conteúdo ignorado");

    const state = useEditorStore.getState();
    expect(state.tabs).toHaveLength(1);
    expect(state.activeTabPath).toBe("notes/one.md");
    expect(state.tabs[0].content).toBe("Inicial");
  });

  it("tracks dirty state relative to the saved base content", () => {
    openNote("notes/one.md");

    useEditorStore.getState().updateTabContent("notes/one.md", "Alterado");
    expect(useEditorStore.getState().tabs[0].dirty).toBe(true);

    useEditorStore.getState().updateTabContent("notes/one.md", "Inicial");
    expect(useEditorStore.getState().tabs[0].dirty).toBe(false);
  });

  it("clears transient state after a successful save", () => {
    openNote("notes/one.md");
    useEditorStore.getState().updateTabContent("notes/one.md", "Salvo");
    useEditorStore.getState().setTabConflict("notes/one.md", "Conflito");
    useEditorStore.getState().markTabSaved("notes/one.md", "rev-2");

    expect(useEditorStore.getState().tabs[0]).toMatchObject({
      content: "Salvo",
      baseContent: "Salvo",
      revision: "rev-2",
      dirty: false,
      saving: false,
      conflict: null,
      saveError: null
    });
  });

  it("activates the last remaining tab when the current tab closes", () => {
    openNote("notes/one.md");
    openNote("notes/two.md");

    useEditorStore.getState().closeTab("notes/two.md");

    expect(useEditorStore.getState().activeTabPath).toBe("notes/one.md");
  });
});
