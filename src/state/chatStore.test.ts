import { beforeEach, describe, expect, it, vi } from "vitest";

const { cancelChatTurn, getChatConversation, getChatTurnSnapshot, listChatConversations } = vi.hoisted(() => ({
  cancelChatTurn: vi.fn(),
  getChatConversation: vi.fn(),
  getChatTurnSnapshot: vi.fn(),
  listChatConversations: vi.fn()
}));

vi.mock("../ipc/client", () => ({
  cancelChatTurn,
  getChatTurnSnapshot,
  getChatConversation,
  createChatConversation: vi.fn(),
  deleteChatConversation: vi.fn(),
  listChatConversations,
  sendChatMessage: vi.fn(),
  normalizeWorkspaceError: (cause: unknown) => ({
    code: "io_error",
    message: cause instanceof Error ? cause.message : "erro"
  })
}));

import { CHAT_TOKEN_DELTA_EVENT, type ChatEventEnvelope, type ChatMessage } from "../ipc/contracts";
import { classifyChatEventSequence, useChatStore } from "./chatStore";

const message: ChatMessage = {
  id: "assistant-1",
  conversationId: "conversation-1",
  ordinal: 1,
  role: "assistant",
  content: "",
  status: "streaming",
  executionId: "execution-1",
  generationId: "generation-1",
  lastSequence: 0,
  inputTokens: null,
  outputTokens: null,
  estimatedCostUsd: null,
  errorCode: null,
  errorMessage: null,
  createdAt: "now",
  updatedAt: "now"
};

function delta(sequence: number, text: string): ChatEventEnvelope {
  return {
    kind: CHAT_TOKEN_DELTA_EVENT,
    schemaVersion: 1,
    sequence,
    occurredAt: "now",
    conversationId: "conversation-1",
    assistantMessageId: "assistant-1",
    executionId: "execution-1",
    generationId: "generation-1",
    detail: { delta: text }
  };
}

describe("chatStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useChatStore.setState({
      projectId: "project-1",
      activeConversationId: "conversation-1",
      conversations: [],
      messagesByConversation: { "conversation-1": [message] },
      toolCallsByConversation: { "conversation-1": [] },
      lastSequences: {},
      error: null
    });
  });

  it("classifies duplicate, next and gap sequences", () => {
    expect(classifyChatEventSequence(3, 3)).toBe("stale");
    expect(classifyChatEventSequence(3, 4)).toBe("next");
    expect(classifyChatEventSequence(3, 6)).toBe("gap");
    expect(classifyChatEventSequence(0, 2)).toBe("gap");
  });

  it("deduplicates deltas and reconciles a detected gap", async () => {
    await useChatStore.getState().consumeEvent(delta(1, "olá"));
    await useChatStore.getState().consumeEvent(delta(1, " duplicado"));
    expect(useChatStore.getState().messagesByConversation["conversation-1"][0].content).toBe("olá");

    getChatTurnSnapshot.mockResolvedValue({
      message: { ...message, content: "olá mundo", lastSequence: 3 },
      toolCalls: []
    });
    await useChatStore.getState().consumeEvent(delta(3, " perdido"));
    expect(getChatTurnSnapshot).toHaveBeenCalledWith({ generationId: "generation-1" });
    expect(useChatStore.getState().messagesByConversation["conversation-1"][0].content).toBe("olá mundo");
  });

  it("cancels the active execution without changing provider state", async () => {
    cancelChatTurn.mockResolvedValue({ cancelled: true });
    await useChatStore.getState().cancelActiveTurn();
    expect(cancelChatTurn).toHaveBeenCalledWith({ executionId: "execution-1" });
  });

  it("discards a late conversation response after switching projects", async () => {
    let resolveProjectA: ((value: unknown) => void) | undefined;
    const conversation = (projectId: string) => ({
      id: `conversation-${projectId}`,
      projectId,
      connectionId: "connection-1",
      providerId: "openai",
      modelId: "gpt-4.1-mini",
      title: projectId,
      connectionAvailable: true,
      createdAt: "now",
      updatedAt: "now"
    });
    listChatConversations.mockImplementation(({ projectId }) =>
      Promise.resolve({ conversations: [conversation(projectId)] })
    );
    getChatConversation.mockImplementation(({ conversationId }) => {
      if (conversationId === "conversation-project-a") {
        return new Promise((resolve) => { resolveProjectA = resolve; });
      }
      return Promise.resolve({
        conversation: conversation("project-b"),
        messages: [],
        toolCalls: []
      });
    });

    const projectAHydration = useChatStore.getState().hydrateProject("project-a");
    await vi.waitFor(() => expect(getChatConversation).toHaveBeenCalledWith({
      conversationId: "conversation-project-a"
    }));
    await useChatStore.getState().hydrateProject("project-b");

    resolveProjectA?.({
      conversation: conversation("project-a"),
      messages: [{ ...message, id: "late-a", conversationId: "conversation-project-a" }],
      toolCalls: []
    });
    await projectAHydration;

    expect(useChatStore.getState()).toMatchObject({
      projectId: "project-b",
      activeConversationId: "conversation-project-b",
      loading: false
    });
    expect(useChatStore.getState().messagesByConversation).toEqual({
      "conversation-project-b": []
    });
  });

  it("falls back to the newest persisted conversation when the restored selection was deleted", async () => {
    const liveConversation = {
      id: "conversation-live",
      projectId: "project-1",
      connectionId: "connection-1",
      providerId: "openai",
      modelId: "gpt-4.1-mini",
      title: "Live",
      connectionAvailable: true,
      createdAt: "now",
      updatedAt: "now"
    };
    listChatConversations.mockResolvedValue({ conversations: [liveConversation] });
    getChatConversation.mockResolvedValue({
      conversation: liveConversation,
      messages: [],
      toolCalls: []
    });

    await useChatStore.getState().hydrateProject("project-1", "conversation-deleted");

    expect(useChatStore.getState().activeConversationId).toBe("conversation-live");
    expect(getChatConversation).toHaveBeenCalledWith({ conversationId: "conversation-live" });
  });
});
