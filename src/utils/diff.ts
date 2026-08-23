export type DiffMark = "neutral" | "remove" | "add" | "change" | "empty";

export interface DiffSide {
  line: number | null;
  text: string;
  marker: string;
  mark: DiffMark;
}

export interface DiffRow {
  left: DiffSide;
  right: DiffSide;
}

function splitLines(content: string): string[] {
  return content ? content.replace(/\r/g, "").split("\n") : [];
}

export function buildDiffRows(input: string, generated: string): DiffRow[] {
  const leftLines = splitLines(input);
  const rightLines = splitLines(generated);
  const rows: DiffRow[] = [];
  let leftLine = 0;
  let rightLine = 0;

  for (let index = 0; index < Math.max(leftLines.length, rightLines.length); index += 1) {
    const leftText = leftLines[index];
    const rightText = rightLines[index];
    const hasLeft = leftText !== undefined;
    const hasRight = rightText !== undefined;
    const leftNumber = hasLeft ? ++leftLine : null;
    const rightNumber = hasRight ? ++rightLine : null;

    if (hasLeft && hasRight && leftText === rightText) {
      rows.push({
        left: { line: leftNumber, text: leftText, marker: " ", mark: "neutral" },
        right: { line: rightNumber, text: rightText, marker: " ", mark: "neutral" }
      });
    } else if (!hasLeft && hasRight) {
      rows.push({
        left: { line: null, text: "", marker: " ", mark: "empty" },
        right: { line: rightNumber, text: rightText, marker: "+", mark: "add" }
      });
    } else if (hasLeft && !hasRight) {
      rows.push({
        left: { line: leftNumber, text: leftText, marker: "-", mark: "remove" },
        right: { line: null, text: "", marker: " ", mark: "empty" }
      });
    } else {
      rows.push({
        left: { line: leftNumber, text: leftText ?? "", marker: "-", mark: "change" },
        right: { line: rightNumber, text: rightText ?? "", marker: "+", mark: "change" }
      });
    }
  }

  return rows;
}
