import type { Pulsation, PulsationStatus } from "../ipc/contracts";

export function pulsationStatusLabel(status: PulsationStatus): string {
  if (status === "checkpoint") return "HITL Review";
  if (status === "done") return "Completed";
  if (status === "failed") return "Failed";
  if (status === "planning") return "Planning";
  if (status === "idle") return "Idle";
  return "Running";
}

export function pulsationTone(status: PulsationStatus): "cyan" | "purple" | "green" | "red" {
  if (status === "checkpoint") return "purple";
  if (status === "done") return "green";
  if (status === "failed") return "red";
  return "cyan";
}

export interface DiffDraft {
  title: string;
  input: string;
  generated: string;
  notes: string[];
}

export function buildFallbackDiffDraft(pulsation: Pulsation): DiffDraft {
  return {
    title: `${pulsation.stage} Review — ${pulsation.featureName}`,
    input: [
      `- Pulsation: ${pulsation.featureName}`,
      `- Stage: ${pulsation.stage}`,
      `- Status: ${pulsation.status}`,
      "- Source snapshot loaded from the orchestration runtime"
    ].join("\n"),
    generated: [
      `- Pulsation: ${pulsation.featureName}`,
      `- Stage: ${pulsation.stage}`,
      `- Runtime status: ${pulsation.status}`,
      "- Generated checkpoint draft ready for a HITL decision"
    ].join("\n"),
    notes: [
      "This fallback is derived from the persisted state machine.",
      "Approve, request a revision, or reject to continue the pulsation."
    ]
  };
}
