import { useEffect, useMemo, useState, type FormEvent } from "react";

import { openFileInWorkspace, renameVaultNote } from "../app/applicationController";
import { EmptyState } from "../components/EmptyState";
import { SanitizedMarkdown } from "../components/SanitizedMarkdown";
import { TreeView } from "../components/vault/TreeView";
import { VaultGraph } from "../components/vault/VaultGraph";
import { useVaultStore } from "../state/vaultStore";
import { extractFileName, formatFrontmatterValue } from "../utils/formatters";

interface RenameNoteFormProps {
  relativePath: string;
}

function RenameNoteForm({ relativePath }: RenameNoteFormProps) {
  const [value, setValue] = useState(() => extractFileName(relativePath));

  const onSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    void renameVaultNote(relativePath, value);
  };

  return (
    <form className="rename-form" onSubmit={onSubmit}>
      <label htmlFor="rename-note">Rename note</label>
      <div>
        <input
          id="rename-note"
          value={value}
          onChange={(event) => setValue(event.target.value)}
        />
        <button type="submit" className="button-secondary">Rename</button>
      </div>
    </form>
  );
}

export function VaultView() {
  const root = useVaultStore((state) => state.workspaceRoot);
  const tree = useVaultStore((state) => state.tree);
  const index = useVaultStore((state) => state.vaultIndex);
  const selectedPath = useVaultStore((state) => state.selectedVaultPath);
  const setSelectedPath = useVaultStore((state) => state.setSelectedVaultPath);
  const documents = useVaultStore((state) => state.documents);
  const loadDocument = useVaultStore((state) => state.loadDocument);
  const query = useVaultStore((state) => state.searchQuery);
  const mode = useVaultStore((state) => state.searchMode);
  const caseSensitive = useVaultStore((state) => state.searchCaseSensitive);
  const searchResponse = useVaultStore((state) => state.searchResponse);
  const searching = useVaultStore((state) => state.searching);
  const indexing = useVaultStore((state) => state.indexing);
  const setSearchQuery = useVaultStore((state) => state.setSearchQuery);
  const setSearchMode = useVaultStore((state) => state.setSearchMode);
  const setSearchCaseSensitive = useVaultStore((state) => state.setSearchCaseSensitive);
  const search = useVaultStore((state) => state.search);
  const note = useMemo(() => index?.notes.find((item) => item.relativePath === selectedPath) ?? null, [index, selectedPath]);
  const content = selectedPath ? documents[selectedPath] ?? "" : "";

  useEffect(() => {
    if (selectedPath && documents[selectedPath] === undefined) {
      void loadDocument(selectedPath).catch(() => undefined);
    }
  }, [documents, loadDocument, selectedPath]);

  const onSearch = (event: FormEvent<HTMLFormElement>) => { event.preventDefault(); void search(); };

  if (!root) return <EmptyState title="No vault open" description="Open a workspace to explore files, frontmatter, and wikilinks." />;

  return <div className="vault-view">
    <aside className="panel vault-browser">
      <div className="section-heading"><div><span className="eyebrow">{indexing ? "Indexing" : "Workspace"}</span><h2>Files</h2></div><span className="count-chip">{index?.notes.length ?? 0}</span></div>
      <form className="vault-search" onSubmit={onSearch}>
        <input value={query} onChange={(event) => setSearchQuery(event.target.value)} placeholder="Search text or regex" aria-label="Vault search" />
        <div><select value={mode} onChange={(event) => setSearchMode(event.target.value as "substring" | "regex")}><option value="substring">Substring</option><option value="regex">Regex</option></select><label><input type="checkbox" checked={caseSensitive} onChange={(event) => setSearchCaseSensitive(event.target.checked)} /> Case sensitive</label></div>
        <button type="submit" disabled={searching || !query.trim()}>{searching ? "Searching…" : "Search vault"}</button>
      </form>
      {searchResponse ? <div className="search-results"><p>{searchResponse.matches.length} matches in {searchResponse.scannedFiles} files{searchResponse.truncated ? " · truncated" : ""}</p>{searchResponse.matches.map((match, position) => <button type="button" key={`${match.relativePath}:${match.line}:${position}`} onClick={() => void openFileInWorkspace(match.relativePath, { line: match.line, column: match.column })}><strong>{match.relativePath}</strong><small>Line {match.line}, column {match.column}</small><span>{match.lineText}</span></button>)}</div> : <TreeView nodes={tree} selectedPath={selectedPath} onSelect={setSelectedPath} />}
    </aside>
    <section className="panel graph-panel">
      <div className="section-heading"><div><span className="eyebrow">Knowledge topology</span><h2>Wikilink graph</h2></div><span className="count-chip">{index?.edges.length ?? 0} links</span></div>
      <VaultGraph index={index} selectedPath={selectedPath} onSelect={setSelectedPath} />
      {index?.brokenLinks.length ? <div className="broken-links"><strong>{index.brokenLinks.length} broken links</strong><div>{index.brokenLinks.map((link, position) => <button type="button" key={`${link.sourcePath}:${link.target}:${position}`} onClick={() => setSelectedPath(link.sourcePath)}><span>{link.sourcePath}</span><code>[[{link.target}]]</code></button>)}</div></div> : <p className="integrity-ok">No broken wikilinks detected.</p>}
    </section>
    <aside className="panel vault-inspector">
      <div className="section-heading"><div><span className="eyebrow">Selected note</span><h2>{selectedPath ? extractFileName(selectedPath) : "Inspector"}</h2></div>{selectedPath ? <button type="button" className="button-secondary" onClick={() => void openFileInWorkspace(selectedPath)}>Open in workspace</button> : null}</div>
      {!selectedPath ? <p className="muted">Select a node or file to inspect it.</p> : <>
        {selectedPath.toLowerCase().endsWith(".md") ? <RenameNoteForm key={selectedPath} relativePath={selectedPath} /> : null}
        {note ? <><div className="note-stats"><span>{note.links.length} outgoing</span><span>{(note.backlinkPaths ?? []).length} backlinks</span></div>{Object.keys(note.frontmatter ?? {}).length ? <dl className="metadata-list">{Object.entries(note.frontmatter ?? {}).map(([key, value]) => <div key={key}><dt>{key}</dt><dd>{formatFrontmatterValue(value)}</dd></div>)}</dl> : <p className="muted">No frontmatter properties.</p>}{(note.schemaWarnings ?? []).length ? <ul className="schema-warnings">{(note.schemaWarnings ?? []).map((warning) => <li key={warning}>{warning}</li>)}</ul> : null}</> : null}
        {content ? <SanitizedMarkdown content={content} className="markdown-surface" /> : <p className="muted">Loading preview…</p>}
      </>}
    </aside>
  </div>;
}
