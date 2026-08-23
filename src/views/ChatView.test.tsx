import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it } from "vitest";

import type { ProviderConnection } from "../ipc/contracts";
import { useChatStore } from "../state/chatStore";
import { usePulsationStore } from "../state/pulsationStore";
import { useRuntimeStore } from "../state/runtimeStore";
import { ChatView } from "./ChatView";

function connection(id: string, eligible: boolean): ProviderConnection {
  return {
    id,
    providerId: "openai",
    displayName: eligible ? "Compatível" : "Sem tool calling",
    endpointUrl: null,
    defaultModel: "gpt-4.1-mini",
    enabled: true,
    state: "active",
    blockedReason: null,
    cooldownUntil: null,
    maxConcurrency: 1,
    version: 1,
    hasApiKey: true,
    chatEligible: eligible,
    createdAt: "now",
    updatedAt: "now"
  };
}

describe("ChatView", () => {
  beforeEach(() => {
    usePulsationStore.setState({
      activeProject: {
        id: "project-1",
        name: "Project",
        workspacePath: "C:/project",
        isArchived: false,
        createdAt: "now",
        updatedAt: "now",
        lastOpenedAt: "now"
      },
      projectId: "project-1"
    });
    useRuntimeStore.setState({ connections: [connection("eligible", true), connection("blocked", false)] });
    useChatStore.setState({
      projectId: "project-1",
      conversations: [],
      activeConversationId: null,
      messagesByConversation: {},
      toolCallsByConversation: {},
      loading: false,
      busy: false,
      error: null
    });
  });

  it("shows only connections explicitly eligible for chat tool calling", () => {
    render(<ChatView />);
    expect(screen.getByRole("option", { name: /Compatível/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /Sem tool calling/ })).not.toBeInTheDocument();
  });

  it("requires a project before exposing the composer", () => {
    usePulsationStore.setState({ activeProject: null, projectId: null });
    render(<ChatView />);
    expect(screen.getByText("Abra um projeto para conversar")).toBeInTheDocument();
    expect(screen.queryByLabelText("Mensagem para o Companion")).not.toBeInTheDocument();
  });
});
