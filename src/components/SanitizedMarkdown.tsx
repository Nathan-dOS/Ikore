import { useMemo } from "react";

import { renderMarkdown } from "../markdown/renderMarkdown";

interface SanitizedMarkdownProps {
  content: string;
  className?: string;
}

export function SanitizedMarkdown({ content, className }: SanitizedMarkdownProps) {
  const sanitizedHtml = useMemo(() => renderMarkdown(content), [content]);
  return <div className={className} dangerouslySetInnerHTML={{ __html: sanitizedHtml }} />;
}
