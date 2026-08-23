import { useMemo, useState } from "react";

import { SanitizedMarkdown } from "../SanitizedMarkdown";
import { parseChatContent } from "./chatContent";

interface ChatMessageContentProps {
  content: string;
}

export function ChatMessageContent({ content }: ChatMessageContentProps) {
  const parts = useMemo(() => parseChatContent(content), [content]);
  return (
    <div className="chat-message-content">
      {parts.map((part, index) =>
        part.kind === "diff" ? (
          <DiffProposal key={`${part.filePath}:${index}`} content={part.content} filePath={part.filePath} />
        ) : part.content ? (
          <SanitizedMarkdown
            key={`markdown:${index}`}
            content={part.content}
            className="markdown-surface chat-markdown"
          />
        ) : null
      )}
    </div>
  );
}

function DiffProposal({ content, filePath }: { content: string; filePath: string }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    await navigator.clipboard.writeText(content);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };
  return (
    <section className="diff-proposal" aria-label={`Proposed diff for ${filePath}`}>
      <header>
        <div>
          <strong>{filePath}</strong>
          <span>Proposta não aplicada</span>
        </div>
        <button type="button" onClick={() => void copy()}>
          {copied ? "Copiado" : "Copiar diff"}
        </button>
      </header>
      <pre><code>{content}</code></pre>
    </section>
  );
}
