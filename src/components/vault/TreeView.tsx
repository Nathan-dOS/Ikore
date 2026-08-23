import type { FileNode } from "../../ipc/contracts";

interface TreeViewProps {
  nodes: FileNode[];
  selectedPath: string | null;
  onSelect: (relativePath: string) => void;
}

export function TreeView({ nodes, selectedPath, onSelect }: TreeViewProps) {
  if (nodes.length === 0) return <p className="muted">No supported files found.</p>;
  return <ul className="vault-tree">{nodes.map((node) => <li key={`${node.kind}:${node.relativePath}`}>
    {node.kind === "directory" ? <div className="tree-directory"><span aria-hidden="true">▾</span><strong>{node.name}</strong></div> : <button type="button" aria-pressed={selectedPath === node.relativePath} onClick={() => onSelect(node.relativePath)}><span aria-hidden="true">◇</span><span><strong>{node.name}</strong><small>{node.relativePath}</small></span></button>}
    {node.kind === "directory" && node.children?.length ? <TreeView nodes={node.children} selectedPath={selectedPath} onSelect={onSelect} /> : null}
  </li>)}</ul>;
}
