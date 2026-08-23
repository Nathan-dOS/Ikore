import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SanitizedMarkdown } from "./SanitizedMarkdown";

describe("SanitizedMarkdown", () => {
  it("removes executable markup and unsafe URLs before DOM insertion", () => {
    const { container } = render(
      <SanitizedMarkdown content={'# Safe\n<script>alert(1)</script>\n[bad](javascript:alert(1))'} />
    );

    expect(container.querySelector("h1")).toHaveTextContent("Safe");
    expect(container.querySelector("script")).toBeNull();
    expect(container.innerHTML).not.toContain("javascript:");
  });
});
