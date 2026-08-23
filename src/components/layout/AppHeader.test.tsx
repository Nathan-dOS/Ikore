import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it } from "vitest";

import { usePulsationStore } from "../../state/pulsationStore";
import { useRuntimeStore } from "../../state/runtimeStore";
import { useVaultStore } from "../../state/vaultStore";
import { useViewStore } from "../../state/viewStore";
import { AppHeader } from "./AppHeader";

describe("AppHeader notifications", () => {
  beforeEach(() => {
    useViewStore.setState({
      activeView: "workspace",
      tauriAvailable: false,
      loadingWorkspace: false,
      globalError: null,
      persistenceWarning: "database unavailable",
      notice: "Workspace saved"
    });
    usePulsationStore.setState({ activeProject: null, projectId: null });
    useRuntimeStore.setState({ error: null });
    useVaultStore.setState({ workspaceRoot: null, error: null });
  });

  it("keeps warnings and errors in header flow and reveals the queued notice after dismissal", async () => {
    const user = userEvent.setup();
    const { container } = render(<AppHeader />);

    expect(screen.getByText(/IPC is unavailable/)).toBeInTheDocument();
    const error = screen.getByRole("button", { name: /Persistence: database unavailable/ });
    const header = container.querySelector(".app-header");
    expect(error.parentElement).toBe(header);
    expect(error).toHaveStyle({ position: "static", gridColumn: "1 / -1" });
    expect(screen.queryByRole("button", { name: "Workspace saved" })).not.toBeInTheDocument();

    await user.click(error);

    expect(screen.queryByRole("button", { name: /database unavailable/ })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Workspace saved" })).toBeInTheDocument();
  });
});
