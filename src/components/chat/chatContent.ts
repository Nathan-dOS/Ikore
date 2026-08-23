export type ContentPart =
  | { kind: "markdown"; content: string }
  | { kind: "diff"; content: string; filePath: string };

export function parseChatContent(content: string): ContentPart[] {
  const parts: ContentPart[] = [];
  const expression = /```diff\s*\n([\s\S]*?)```/g;
  let cursor = 0;
  for (const match of content.matchAll(expression)) {
    const index = match.index ?? 0;
    if (index > cursor) parts.push({ kind: "markdown", content: content.slice(cursor, index) });
    const diff = match[1].trimEnd();
    const oldHeader = diff.match(/^---\s+a\/(.+)$/m);
    const newHeader = diff.match(/^\+\+\+\s+b\/(.+)$/m);
    if (oldHeader && newHeader && oldHeader[1] === newHeader[1]) {
      parts.push({ kind: "diff", content: diff, filePath: newHeader[1] });
    } else {
      parts.push({ kind: "markdown", content: match[0] });
    }
    cursor = index + match[0].length;
  }
  if (cursor < content.length) parts.push({ kind: "markdown", content: content.slice(cursor) });
  return parts.length > 0 ? parts : [{ kind: "markdown", content }];
}
