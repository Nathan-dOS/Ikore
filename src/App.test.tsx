import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import { App } from "./App";
import { usePulsationStore } from "./state/pulsationStore";
import { useVaultStore } from "./state/vaultStore";
import { useViewStore } from "./state/viewStore";

describe("application shell", () => {
  beforeEach(() => {
    useViewStore.setState({
      activeView: "workspace",
      sidebarCollapsed: false,
      contextDrawerOpen: true,
      diffExpanded: false,
      tauriAvailable: false,
      globalError: null,
      persistenceWarning: null,
      notice: null
    });
    usePulsationStore.setState({
      activeProject: null,
      projectId: null,
      pulsations: [],
      activePulsationId: null,
      hitlWaitingCount: 0,
      backgroundAgentsCount: 0,
      artifacts: []
    });
    useVaultStore.setState({ workspaceRoot: null, tree: [], vaultIndex: null });
  });

  it("switches between all six dedicated views without remounting the shell", async () => {
    const user = userEvent.setup();
    render(<App />);

    const navigation = screen.getByRole("navigation");
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Workspace & HITL");

    for (const label of [
      "Pulsations & Pipeline",
      "Vault & Graph",
      "Chat / Companion",
      "Settings & AI Runtime",
      "Logs & Observability",
      "Workspace & HITL"
    ]) {
      await user.click(screen.getByRole("button", { name: label }));
      expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(label);
      expect(screen.getByRole("navigation")).toBe(navigation);
      expect(screen.getByRole("button", { name: label })).toHaveAttribute("aria-current", "page");
    }
  });

  it("shows semantic badges and preserves collapsed navigation across view changes", async () => {
    const user = userEvent.setup();
    usePulsationStore.setState({ hitlWaitingCount: 2, backgroundAgentsCount: 3 });
    render(<App />);

    expect(screen.getByLabelText("2 pending")).toBeInTheDocument();
    expect(screen.getByLabelText("3 pending")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Collapse sidebar" }));
    await user.click(screen.getByRole("button", { name: "Logs & Observability" }));

    expect(useViewStore.getState().sidebarCollapsed).toBe(true);
    expect(screen.getByRole("button", { name: "Expand sidebar" })).toBeInTheDocument();
  });
});
