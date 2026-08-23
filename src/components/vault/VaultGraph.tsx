import { buildVaultGraphLayout } from "../../state/vaultStore";
import type { GetWorkspaceVaultIndexResponse } from "../../ipc/contracts";

interface VaultGraphProps {
  index: GetWorkspaceVaultIndexResponse | null;
  selectedPath: string | null;
  onSelect: (relativePath: string) => void;
}

export function VaultGraph({ index, selectedPath, onSelect }: VaultGraphProps) {
  const graph = buildVaultGraphLayout(index);
  if (graph.nodes.length === 0) return <div className="graph-empty">Open a Markdown vault to render its wikilink graph.</div>;
  return <div className="vault-graph"><svg viewBox="0 0 760 460" role="img" aria-label="Interactive vault graph">
    <g className="graph-edges">{graph.edges.map((edge, indexValue) => <line key={`${edge.source.relativePath}:${edge.target.relativePath}:${indexValue}`} x1={edge.source.x} y1={edge.source.y} x2={edge.target.x} y2={edge.target.y} />)}</g>
    <g className="graph-nodes">{graph.nodes.map((node) => {
      const selected = node.relativePath === selectedPath;
      return <g className={selected ? "is-selected" : ""} key={node.relativePath} role="button" tabIndex={0} aria-label={`Select ${node.title}`} onClick={() => onSelect(node.relativePath)} onKeyDown={(event) => { if (event.key === "Enter" || event.key === " ") onSelect(node.relativePath); }}>
        <circle cx={node.x} cy={node.y} r={selected ? 14 : 10} />
        <text x={node.x} y={node.y + 26}>{node.title.length > 24 ? `${node.title.slice(0, 24)}…` : node.title}</text>
      </g>;
    })}</g>
  </svg></div>;
}
