import { beforeEach, describe, expect, it } from "vitest";

import type { UiSessionState } from "../ipc/contracts";
import { useViewStore } from "./viewStore";

describe("viewStore", () => {
  beforeEach(() => {
    useViewStore.setState({
      activeView: "workspace",
      sidebarCollapsed: false,
      contextDrawerOpen: true,
      diffExpanded: true
    });
  });

  it("hydrates persisted layout preferences and resets transient fullscreen state", () => {
    const session: UiSessionState = {
      projectId: "project-1",
      workspaceRoot: "C:\\vault",
      activeTabPath: null,
      workspaceViewMode: "editor",
      activeView: "vault",
      sidebarCollapsed: true,
      contextDrawerOpen: false,
      activePulsationId: "pulse-1",
      selectedVaultPath: "Features/Login.md",
      activeChatConversationId: "chat-1",
      tabs: [],
      updatedAt: "now"
    };

    useViewStore.getState().hydrateLayout(session);

    expect(useViewStore.getState()).toMatchObject({
      activeView: "vault",
      sidebarCollapsed: true,
      contextDrawerOpen: false,
      diffExpanded: false
    });
  });
});
