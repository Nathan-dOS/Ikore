import { buildDiffRows } from "../../utils/diff";

interface MarkdownDiffViewProps {
  input: string;
  generated: string;
  notes: string[];
  expanded: boolean;
  onToggleExpanded: () => void;
}

export function MarkdownDiffView({
  input,
  generated,
  notes,
  expanded,
  onToggleExpanded
}: MarkdownDiffViewProps) {
  const rows = buildDiffRows(input, generated);
  return (
    <div className={`markdown-diff ${expanded ? "is-expanded" : ""}`}>
      <div className="section-heading">
        <div><span className="eyebrow">Checkpoint review</span><h2>Markdown Diff</h2></div>
        <button type="button" className="button-secondary" onClick={onToggleExpanded}>
          {expanded ? "Exit fullscreen" : "Expand"}
        </button>
      </div>
      <div className="diff-grid">
        {(["left", "right"] as const).map((side) => (
          <section className="diff-panel" key={side}>
            <header>{side === "left" ? "Input baseline" : "Generated draft"}</header>
            <div className="diff-lines">
              {rows.length === 0 ? <p className="muted">No content available.</p> : rows.map((row, index) => {
                const item = row[side];
                return (
                  <div className={`diff-line is-${item.mark}`} key={`${side}-${index}`}>
                    <span className="line-number">{item.line ?? " "}</span>
                    <span className="line-marker">{item.marker}</span>
                    <span>{item.text || " "}</span>
                  </div>
                );
              })}
            </div>
          </section>
        ))}
      </div>
      <div className="diff-notes"><strong>Notes</strong><ul>{notes.map((note) => <li key={note}>{note}</li>)}</ul></div>
    </div>
  );
}
