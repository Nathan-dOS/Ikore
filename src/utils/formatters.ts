export function extractFileName(relativePath: string): string {
  const segments = relativePath.split("/");
  return segments[segments.length - 1] ?? relativePath;
}

export function extractPathBaseName(pathValue: string): string {
  const segments = pathValue.split(/[\\/]/).filter(Boolean);
  return segments[segments.length - 1] ?? pathValue;
}

export function formatDuration(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  return `${hours.toString().padStart(2, "0")}h ${minutes.toString().padStart(2, "0")}m`;
}

export function formatUsd(value: number | null): string {
  return value === null || Number.isNaN(value) ? "—" : `$${value.toFixed(4)}`;
}

export function formatDateTime(value: string | null): string {
  if (!value) return "—";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString("en-US");
}

export function formatFrontmatterValue(value: unknown): string {
  if (Array.isArray(value)) return value.map(formatFrontmatterValue).join(", ");
  if (value && typeof value === "object") return JSON.stringify(value);
  if (value === null || value === undefined) return "—";
  return String(value);
}
