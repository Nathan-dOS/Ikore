import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { ChatMessageContent } from "./ChatMessageContent";
import { parseChatContent } from "./chatContent";

describe("ChatMessageContent", () => {
  beforeEach(() => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: vi.fn().mockResolvedValue(undefined) }
    });
  });

  it("recognizes only unified diffs with matching a/ and b/ headers", () => {
    const valid = parseChatContent("```diff\n--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1 +1 @@\n-a\n+b\n```");
    const invalid = parseChatContent("```diff\n-old\n+new\n```");
    expect(valid[0]).toMatchObject({ kind: "diff", filePath: "src/a.ts" });
    expect(invalid[0]).toMatchObject({ kind: "markdown" });
  });

  it("sanitizes markdown and offers copy without any apply action", async () => {
    const user = userEvent.setup();
    const writeText = vi.spyOn(navigator.clipboard, "writeText");
    const diff = "--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1 +1 @@\n-a\n+b";
    const { container } = render(
      <ChatMessageContent content={`Seguro<script>alert(1)</script>\n\n\`\`\`diff\n${diff}\n\`\`\``} />
    );
    expect(container.querySelector("script")).toBeNull();
    expect(screen.getByText("Proposta não aplicada")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /aplicar/i })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Copiar diff" }));
    expect(writeText).toHaveBeenCalledWith(diff);
  });
});
