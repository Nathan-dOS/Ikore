import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import LlmRoutingSettings from "./LlmRoutingSettings";
import type {
  AgentRuntimeConfig,
  ProviderConnection,
  ProviderProfile,
  ProviderRoute,
  ProviderRouteBinding
} from "../ipc/contracts";
import { classifyLlmEventSequence, useRuntimeStore } from "../state/runtimeStore";

const mockProfiles: ProviderProfile[] = [
  {
    id: "openai",
    displayName: "OpenAI",
    protocol: "openai_responses",
    endpoint: "https://api.openai.com",
    endpointConfigurable: false,
    authStyle: "bearer",
    capabilities: { streaming: true, jsonObject: true, jsonSchema: true, usage: true, toolCalling: true },
    forbiddenParameters: [],
    defaultModel: "gpt-4.1-mini",
    local: false
  },
  {
    id: "ollama",
    displayName: "Ollama (Local)",
    protocol: "openai_chat",
    endpoint: "http://127.0.0.1:11434",
    endpointConfigurable: true,
    authStyle: "none",
    capabilities: { streaming: true, jsonObject: true, jsonSchema: false, usage: true, toolCalling: true },
    forbiddenParameters: [],
    defaultModel: "llama3:latest",
    local: true
  }
];

const mockConnections: ProviderConnection[] = [
  {
    id: "conn-openai-1",
    providerId: "openai",
    displayName: "OpenAI Primary",
    endpointUrl: null,
    defaultModel: "gpt-4.1-mini",
    enabled: true,
    state: "active",
    blockedReason: null,
    cooldownUntil: null,
    maxConcurrency: 1,
    version: 1,
    hasApiKey: true,
    chatEligible: true,
    createdAt: "2026-08-18T00:00:00Z",
    updatedAt: "2026-08-18T00:00:00Z"
  }
];

const mockRoutes: ProviderRoute[] = [
  {
    id: "legacy-default",
    name: "Legacy Default",
    strategy: "priority",
    maxAttempts: 3,
    attemptTimeoutMs: 30_000,
    routeTimeoutMs: 45_000,
    enabled: true,
    version: 1,
    targets: [
      { position: 0, connectionId: "conn-openai-1", modelId: "gpt-4.1-mini", enabled: true }
    ],
    createdAt: "2026-08-18T00:00:00Z",
    updatedAt: "2026-08-18T00:00:00Z"
  }
];

const mockBindings: ProviderRouteBinding[] = [
  {
    projectId: "project-1",
    agentRole: "backend_agent",
    stage: "Code",
    routeId: "legacy-default",
    enabled: true,
    version: 1,
    createdAt: "2026-08-18T00:00:00Z",
    updatedAt: "2026-08-18T00:00:00Z"
  }
];

const mockConfig: AgentRuntimeConfig = {
  provider: "openai",
  model: "gpt-4.1-mini",
  temperature: 0.2,
  maxOutputTokens: 4096,
  timeoutMs: 30_000,
  maxRetries: 3,
  contextCharsLimit: 120_000,
  maxContextFiles: 32,
  contextStrategy: "yaml_graph",
  embeddingEnabled: false,
  embeddingProvider: "ollama",
  embeddingModel: "nomic-embed-text",
  embeddingTopK: 8,
  routingV2Mode: "off"
};

const saveConnection = vi.fn().mockResolvedValue(undefined);
const setRoutingMode = vi.fn().mockResolvedValue(undefined);
const saveRoute = vi.fn().mockResolvedValue(undefined);
const saveBinding = vi.fn().mockResolvedValue(undefined);
const testConnection = vi.fn().mockResolvedValue(undefined);

describe("LLM stream event sequencing", () => {
  it("accepts consecutive events, discards stale events, and detects gaps", () => {
    expect(classifyLlmEventSequence(0, 1)).toBe("next");
    expect(classifyLlmEventSequence(8, 9)).toBe("next");
    expect(classifyLlmEventSequence(8, 8)).toBe("stale");
    expect(classifyLlmEventSequence(8, 4)).toBe("stale");
    expect(classifyLlmEventSequence(8, 11)).toBe("gap");
  });
});

describe("LlmRoutingSettings", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useRuntimeStore.setState({
      config: mockConfig,
      profiles: mockProfiles,
      connections: mockConnections,
      routes: mockRoutes,
      bindings: mockBindings,
      resilienceStates: [],
      busy: false,
      error: null,
      notice: null,
      saveConnection,
      setRoutingMode,
      saveRoute,
      saveBinding,
      removeConnection: vi.fn().mockResolvedValue(undefined),
      testConnection,
      removeRoute: vi.fn().mockResolvedValue(undefined)
    });
  });

  it("renders the store-backed connections, routes, and bindings", () => {
    render(<LlmRoutingSettings projectId="project-1" />);

    expect(screen.getAllByText("OpenAI Primary")).toHaveLength(2);
    expect(screen.getAllByText("Legacy Default")).toHaveLength(3);
    expect(screen.getByText("0 resilience states")).toBeInTheDocument();
    expect(screen.getAllByText("backend_agent")).toHaveLength(2);
  });

  it("delegates routing mode changes to the runtime store", async () => {
    const user = userEvent.setup();
    render(<LlmRoutingSettings projectId="project-1" />);

    await user.selectOptions(screen.getByLabelText("Routing v2 mode"), "on");

    expect(setRoutingMode).toHaveBeenCalledWith("on");
  });

  it("tests the selected provider connection through the runtime store", async () => {
    const user = userEvent.setup();
    render(<LlmRoutingSettings projectId="project-1" />);

    await user.click(screen.getByRole("button", { name: "Test" }));

    expect(testConnection).toHaveBeenCalledWith("conn-openai-1");
  });

  it("sends a credential directly to the store and clears the uncontrolled field", async () => {
    const user = userEvent.setup();
    render(<LlmRoutingSettings projectId="project-1" />);

    await user.type(screen.getByLabelText("Connection name"), "Secondary");
    const apiKey = screen.getByLabelText("API key") as HTMLInputElement;
    await user.type(apiKey, "test-secret-not-for-state");
    await user.click(screen.getByRole("button", { name: "Add connection" }));

    expect(apiKey.value).toBe("");
    await waitFor(() => expect(saveConnection).toHaveBeenCalledWith(expect.objectContaining({
      displayName: "Secondary",
      apiKey: "test-secret-not-for-state"
    })));
    expect(JSON.stringify(useRuntimeStore.getState())).not.toContain("test-secret-not-for-state");
  });

  it("clears the credential field even when persistence fails", async () => {
    const user = userEvent.setup();
    saveConnection.mockRejectedValueOnce(new Error("keyring unavailable"));
    render(<LlmRoutingSettings projectId="project-1" />);

    await user.type(screen.getByLabelText("Connection name"), "Secondary");
    const apiKey = screen.getByLabelText("API key") as HTMLInputElement;
    await user.type(apiKey, "ephemeral-secret");
    await user.click(screen.getByRole("button", { name: "Add connection" }));

    expect(apiKey.value).toBe("");
    await waitFor(() => expect(saveConnection).toHaveBeenCalled());
    expect(JSON.stringify(useRuntimeStore.getState())).not.toContain("ephemeral-secret");
  });
});
