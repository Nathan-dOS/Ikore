import { create } from "zustand";

import {
  cancelChatTurn,
  createChatConversation,
  deleteChatConversation,
  getChatConversation,
  getChatTurnSnapshot,
  listChatConversations,
  normalizeWorkspaceError,
  sendChatMessage
} from "../ipc/client";
import {
  CHAT_CONTENT_RESET_EVENT,
  CHAT_TOKEN_DELTA_EVENT,
  CHAT_TOOL_COMPLETED_EVENT,
  CHAT_TOOL_STARTED_EVENT,
  CHAT_TURN_CANCELLED_EVENT,
  CHAT_TURN_COMPLETED_EVENT,
  CHAT_TURN_FAILED_EVENT,
  type ChatConversation,
  type ChatEventEnvelope,
  type ChatMessage,
  type ChatToolCall
} from "../ipc/contracts";

export function classifyChatEventSequence(
  previousSequence: number,
  nextSequence: number
): "stale" | "next" | "gap" {
  if (nextSequence <= previousSequence) return "stale";
  if (nextSequence > previousSequence + 1) return "gap";
  return "next";
}

interface ChatStoreState {
  projectId: string | null;
  conversations: ChatConversation[];
  activeConversationId: string | null;
  messagesByConversation: Record<string, ChatMessage[]>;
  toolCallsByConversation: Record<string, ChatToolCall[]>;
  lastSequences: Record<string, number>;
  loading: boolean;
  busy: boolean;
  error: string | null;
  hydrateProject: (projectId: string, restoredConversationId?: string | null) => Promise<void>;
  resetProject: () => void;
  selectConversation: (conversationId: string) => Promise<void>;
  createConversation: (connectionId: string) => Promise<void>;
  deleteConversation: (conversationId: string) => Promise<void>;
  sendMessage: (content: string) => Promise<void>;
  cancelActiveTurn: () => Promise<void>;
  consumeEvent: (event: ChatEventEnvelope) => Promise<void>;
  reconcileTurn: (generationId: string, conversationId?: string) => Promise<void>;
  clearError: () => void;
}

function sortMessages(messages: ChatMessage[]): ChatMessage[] {
  return [...messages].sort((left, right) => left.ordinal - right.ordinal);
}

function mergeMessage(messages: ChatMessage[], message: ChatMessage): ChatMessage[] {
  const withoutCurrent = messages.filter((item) => item.id !== message.id);
  return sortMessages([...withoutCurrent, message]);
}

function errorMessage(cause: unknown): string {
  return normalizeWorkspaceError(cause).message;
}

export const useChatStore = create<ChatStoreState>((set, get) => ({
  projectId: null,
  conversations: [],
  activeConversationId: null,
  messagesByConversation: {},
  toolCallsByConversation: {},
  lastSequences: {},
  loading: false,
  busy: false,
  error: null,
  hydrateProject: async (projectId, restoredConversationId = null) => {
    set({
      projectId,
      loading: true,
      error: null,
      conversations: [],
      activeConversationId: null,
      messagesByConversation: {},
      toolCallsByConversation: {},
      lastSequences: {}
    });
    try {
      const response = await listChatConversations({ projectId });
      if (get().projectId !== projectId) return;
      const selected =
        response.conversations.find((conversation) => conversation.id === restoredConversationId) ??
        response.conversations[0] ??
        null;
      set({ conversations: response.conversations, activeConversationId: selected?.id ?? null });
      if (selected) await get().selectConversation(selected.id);
    } catch (cause) {
      set({ error: errorMessage(cause) });
    } finally {
      if (get().projectId === projectId) set({ loading: false });
    }
  },
  resetProject: () =>
    set({
      projectId: null,
      conversations: [],
      activeConversationId: null,
      messagesByConversation: {},
      toolCallsByConversation: {},
      lastSequences: {},
      loading: false,
      busy: false,
      error: null
    }),
  selectConversation: async (conversationId) => {
    const projectId = get().projectId;
    set({ activeConversationId: conversationId, loading: true, error: null });
    try {
      const response = await getChatConversation({ conversationId });
      if (
        get().projectId !== projectId ||
        !get().conversations.some((conversation) => conversation.id === conversationId)
      ) {
        return;
      }
      set((state) => ({
        conversations: state.conversations.map((conversation) =>
          conversation.id === response.conversation.id ? response.conversation : conversation
        ),
        messagesByConversation: {
          ...state.messagesByConversation,
          [conversationId]: sortMessages(response.messages)
        },
        toolCallsByConversation: {
          ...state.toolCallsByConversation,
          [conversationId]: response.toolCalls
        },
        lastSequences: response.messages.reduce(
          (sequences, message) =>
            message.generationId
              ? { ...sequences, [message.generationId]: message.lastSequence }
              : sequences,
          state.lastSequences
        )
      }));
    } catch (cause) {
      if (get().projectId === projectId) set({ error: errorMessage(cause) });
    } finally {
      if (get().projectId === projectId) set({ loading: false });
    }
  },
  createConversation: async (connectionId) => {
    const projectId = get().projectId;
    if (!projectId) return;
    set({ busy: true, error: null });
    try {
      const response = await createChatConversation({ projectId, connectionId });
      set((state) => ({
        conversations: [response.conversation, ...state.conversations],
        activeConversationId: response.conversation.id,
        messagesByConversation: {
          ...state.messagesByConversation,
          [response.conversation.id]: []
        },
        toolCallsByConversation: {
          ...state.toolCallsByConversation,
          [response.conversation.id]: []
        }
      }));
    } catch (cause) {
      set({ error: errorMessage(cause) });
      throw cause;
    } finally {
      set({ busy: false });
    }
  },
  deleteConversation: async (conversationId) => {
    set({ busy: true, error: null });
    try {
      await deleteChatConversation({ conversationId });
      const remaining = get().conversations.filter((conversation) => conversation.id !== conversationId);
      const nextActive =
        get().activeConversationId === conversationId
          ? remaining[0]?.id ?? null
          : get().activeConversationId;
      set((state) => {
        const messagesByConversation = { ...state.messagesByConversation };
        const toolCallsByConversation = { ...state.toolCallsByConversation };
        delete messagesByConversation[conversationId];
        delete toolCallsByConversation[conversationId];
        return {
          conversations: remaining,
          activeConversationId: nextActive,
          messagesByConversation,
          toolCallsByConversation
        };
      });
      if (nextActive && !get().messagesByConversation[nextActive]) {
        await get().selectConversation(nextActive);
      }
    } catch (cause) {
      set({ error: errorMessage(cause) });
      throw cause;
    } finally {
      set({ busy: false });
    }
  },
  sendMessage: async (content) => {
    const conversationId = get().activeConversationId;
    if (!conversationId) return;
    set({ busy: true, error: null });
    try {
      const started = await sendChatMessage({ conversationId, content });
      set((state) => ({
        lastSequences: { ...state.lastSequences, [started.generationId]: 0 }
      }));
      await get().selectConversation(conversationId);
    } catch (cause) {
      set({ error: errorMessage(cause) });
      throw cause;
    } finally {
      set({ busy: false });
    }
  },
  cancelActiveTurn: async () => {
    const conversationId = get().activeConversationId;
    if (!conversationId) return;
    const active = [...(get().messagesByConversation[conversationId] ?? [])]
      .reverse()
      .find(
        (message) =>
          message.role === "assistant" &&
          (message.status === "pending" || message.status === "streaming") &&
          message.executionId
      );
    if (!active?.executionId) return;
    try {
      await cancelChatTurn({ executionId: active.executionId });
    } catch (cause) {
      set({ error: errorMessage(cause) });
    }
  },
  consumeEvent: async (event) => {
    if (
      !get().conversations.some((conversation) => conversation.id === event.conversationId) &&
      !get().messagesByConversation[event.conversationId]
    ) {
      return;
    }
    const previous = get().lastSequences[event.generationId] ?? 0;
    const classification = classifyChatEventSequence(previous, event.sequence);
    if (classification === "stale") return;
    if (classification === "gap") {
      await get().reconcileTurn(event.generationId, event.conversationId);
      return;
    }
    set((state) => ({
      lastSequences: { ...state.lastSequences, [event.generationId]: event.sequence }
    }));

    const messages = get().messagesByConversation[event.conversationId] ?? [];
    const message = messages.find((item) => item.id === event.assistantMessageId);
    if (event.kind === CHAT_TOKEN_DELTA_EVENT && message) {
      const delta = event.detail.delta ?? "";
      set((state) => ({
        messagesByConversation: {
          ...state.messagesByConversation,
          [event.conversationId]: (state.messagesByConversation[event.conversationId] ?? []).map(
            (item) =>
              item.id === event.assistantMessageId
                ? { ...item, content: item.content + delta, status: "streaming", lastSequence: event.sequence }
                : item
          )
        }
      }));
      return;
    }
    if (event.kind === CHAT_CONTENT_RESET_EVENT && message) {
      set((state) => ({
        messagesByConversation: {
          ...state.messagesByConversation,
          [event.conversationId]: (state.messagesByConversation[event.conversationId] ?? []).map(
            (item) =>
              item.id === event.assistantMessageId
                ? { ...item, content: "", lastSequence: event.sequence }
                : item
          )
        }
      }));
      return;
    }
    if (
      event.kind === CHAT_TOOL_STARTED_EVENT ||
      event.kind === CHAT_TOOL_COMPLETED_EVENT ||
      event.kind === CHAT_TURN_COMPLETED_EVENT ||
      event.kind === CHAT_TURN_FAILED_EVENT ||
      event.kind === CHAT_TURN_CANCELLED_EVENT ||
      !message
    ) {
      await get().reconcileTurn(event.generationId, event.conversationId);
    }
  },
  reconcileTurn: async (generationId, conversationId) => {
    try {
      const response = await getChatTurnSnapshot({ generationId });
      const targetConversationId = conversationId ?? response.message.conversationId;
      set((state) => ({
        messagesByConversation: {
          ...state.messagesByConversation,
          [targetConversationId]: mergeMessage(
            state.messagesByConversation[targetConversationId] ?? [],
            response.message
          )
        },
        toolCallsByConversation: {
          ...state.toolCallsByConversation,
          [targetConversationId]: response.toolCalls
        },
        lastSequences: {
          ...state.lastSequences,
          [generationId]: response.message.lastSequence
        }
      }));
    } catch (cause) {
      set({ error: errorMessage(cause) });
    }
  },
  clearError: () => set({ error: null })
}));
