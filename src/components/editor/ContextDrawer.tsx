import { useMemo } from "react";

import { SanitizedMarkdown } from "../SanitizedMarkdown";
import { useEditorStore } from "../../state/editorStore";
import { selectContextPanelPaths, useVaultStore } from "../../state/vaultStore";
import { useViewStore } from "../../state/viewStore";
import { extractFileName, formatFrontmatterValue } from "../../utils/formatters";

export function ContextDrawer() {
  const open = useViewStore((state) => state.contextDrawerOpen);
  const toggle = useViewStore((state) => state.toggleContextDrawer);
  const activeTabPath = useEditorStore((state) => state.activeTabPath);
  const tabs = useEditorStore((state) => state.tabs);
  const index = useVaultStore((state) => state.vaultIndex);
  const documents = useVaultStore((state) => state.documents);
  const notesByPath = useMemo(() => new Map((index?.notes ?? []).map((note) => [note.relativePath, note])), [index]);
  const paths = useMemo(
    () => selectContextPanelPaths(index, activeTabPath),
    [activeTabPath, index]
  );

  return (
    <aside className={`context-drawer ${open ? "is-open" : ""}`}>
      <button type="button" className="context-toggle" onClick={toggle} aria-expanded={open}>
        {open ? "Hide context" : "Show context"}
      </button>
      {open ? <div className="context-scroll">
        <div className="section-heading"><div><span className="eyebrow">Vault context</span><h2>Related notes</h2></div><span className="count-chip">{paths.length}</span></div>
        {paths.length === 0 ? <p className="muted">Open a file to load related notes.</p> : paths.map((path) => {
          const note = notesByPath.get(path);
          const tab = tabs.find((item) => item.relativePath === path);
          const content = tab?.content ?? documents[path] ?? "";
          return <article className="context-note" key={path}>
            <header><strong>{extractFileName(path)}</strong>{note ? <span>{note.links.length} links</span> : null}</header>
            {note && Object.keys(note.frontmatter ?? {}).length > 0 ? <dl className="metadata-list">{Object.entries(note.frontmatter ?? {}).slice(0, 4).map(([key, value]) => <div key={key}><dt>{key}</dt><dd>{formatFrontmatterValue(value)}</dd></div>)}</dl> : null}
            {content ? <SanitizedMarkdown content={content} className="markdown-surface compact" /> : <p className="muted">Loading…</p>}
          </article>;
        })}
      </div> : null}
    </aside>
  );
}
