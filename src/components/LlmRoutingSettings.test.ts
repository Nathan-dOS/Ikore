import { describe, expect, it } from "vitest";

import { classifyLlmEventSequence } from "./LlmRoutingSettings";

describe("LLM stream event sequencing", () => {
  it("accepts the first and immediately subsequent events", () => {
    expect(classifyLlmEventSequence(0, 1)).toBe("next");
    expect(classifyLlmEventSequence(8, 9)).toBe("next");
  });

  it("discards duplicate and older events", () => {
    expect(classifyLlmEventSequence(8, 8)).toBe("stale");
    expect(classifyLlmEventSequence(8, 4)).toBe("stale");
  });

  it("detects a gap that requires an execution snapshot", () => {
    expect(classifyLlmEventSequence(8, 11)).toBe("gap");
  });
});
