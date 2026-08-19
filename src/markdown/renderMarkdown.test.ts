import { describe, expect, it } from "vitest";

import { renderMarkdown } from "./renderMarkdown";

describe("renderMarkdown", () => {
  it("renders basic Markdown and GFM", () => {
    const html = renderMarkdown("# Título\n\n- [x] concluído");

    expect(html).toContain("<h1>Título</h1>");
    expect(html).toContain('type="checkbox"');
    expect(html).toContain("checked");
  });

  it("neutralizes script tags", () => {
    const html = renderMarkdown("Texto seguro<script>alert('xss')</script>");

    expect(html).toContain("Texto seguro");
    expect(html).not.toContain("<script");
  });

  it("removes unsafe URL protocols", () => {
    const html = renderMarkdown("[abrir](javascript:alert('xss'))");

    expect(html).not.toContain("javascript:");
  });
});
