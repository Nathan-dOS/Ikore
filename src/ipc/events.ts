import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import {
  LLM_ATTEMPT_FAILED_EVENT,
  LLM_ATTEMPT_STARTED_EVENT,
  LLM_RESILIENCE_CHANGED_EVENT,
  LLM_ROUTE_COMPLETED_EVENT,
  LLM_ROUTE_FAILED_EVENT,
  LLM_ROUTE_STARTED_EVENT,
  LLM_TOKEN_DELTA_EVENT,
  WORKSPACE_CHANGED_EVENT,
  CHAT_TURN_STARTED_EVENT,
  CHAT_TOOL_STARTED_EVENT,
  CHAT_TOOL_COMPLETED_EVENT,
  CHAT_TOKEN_DELTA_EVENT,
  CHAT_CONTENT_RESET_EVENT,
  CHAT_TURN_COMPLETED_EVENT,
  CHAT_TURN_FAILED_EVENT,
  CHAT_TURN_CANCELLED_EVENT,
  type ChatEventEnvelope,
  type ChatEventName,
  type LlmEventEnvelope,
  type WorkspaceChangedEvent
} from "./contracts";

const LLM_EVENT_NAMES = [
  LLM_ROUTE_STARTED_EVENT,
  LLM_ATTEMPT_STARTED_EVENT,
  LLM_TOKEN_DELTA_EVENT,
  LLM_ATTEMPT_FAILED_EVENT,
  LLM_ROUTE_COMPLETED_EVENT,
  LLM_ROUTE_FAILED_EVENT,
  LLM_RESILIENCE_CHANGED_EVENT
] as const;

const CHAT_EVENT_NAMES: readonly ChatEventName[] = [
  CHAT_TURN_STARTED_EVENT,
  CHAT_TOOL_STARTED_EVENT,
  CHAT_TOOL_COMPLETED_EVENT,
  CHAT_TOKEN_DELTA_EVENT,
  CHAT_CONTENT_RESET_EVENT,
  CHAT_TURN_COMPLETED_EVENT,
  CHAT_TURN_FAILED_EVENT,
  CHAT_TURN_CANCELLED_EVENT
];

export type IpcUnlisten = () => void;

export async function subscribeWorkspaceChanges(
  handler: (event: WorkspaceChangedEvent) => void
): Promise<IpcUnlisten> {
  const unlisten = await listen<WorkspaceChangedEvent>(WORKSPACE_CHANGED_EVENT, (event) => {
    handler(event.payload);
  });
  return () => unlisten();
}

export async function subscribeLlmEvents(
  handler: (event: LlmEventEnvelope) => void
): Promise<IpcUnlisten> {
  const unlisteners: UnlistenFn[] = await Promise.all(
    LLM_EVENT_NAMES.map((eventName) =>
      listen<LlmEventEnvelope>(eventName, (event) => {
        handler(event.payload);
      })
    )
  );

  return () => {
    unlisteners.forEach((unlisten) => unlisten());
  };
}

export async function subscribeChatEvents(
  handler: (event: ChatEventEnvelope) => void
): Promise<IpcUnlisten> {
  const unlisteners: UnlistenFn[] = await Promise.all(
    CHAT_EVENT_NAMES.map((eventName) =>
      listen<Omit<ChatEventEnvelope, "kind">>(eventName, (event) => {
        handler({ ...event.payload, kind: eventName });
      })
    )
  );
  return () => unlisteners.forEach((unlisten) => unlisten());
}
