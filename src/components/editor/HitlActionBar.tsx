import { useEffect } from "react";

import type { HitlDecision } from "../../ipc/contracts";

interface HitlActionBarProps {
  busy: boolean;
  onDecision: (decision: HitlDecision) => void;
}

function isTypingElement(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable) return true;
  return ["input", "textarea", "select"].includes(target.tagName.toLowerCase());
}

export function HitlActionBar({ busy, onDecision }: HitlActionBarProps) {
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (busy || isTypingElement(event.target)) return;
      if (event.key === "Enter") {
        event.preventDefault();
        onDecision("approve");
      } else if (event.key.toLowerCase() === "r") {
        event.preventDefault();
        onDecision("request_revision");
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [busy, onDecision]);

  return (
    <div className="hitl-action-bar" aria-label="HITL decisions">
      <button type="button" className="button-approve" disabled={busy} onClick={() => onDecision("approve")}>Approve <kbd>Enter</kbd></button>
      <button type="button" className="button-revision" disabled={busy} onClick={() => onDecision("request_revision")}>Request revision <kbd>R</kbd></button>
      <button type="button" className="button-danger" disabled={busy} onClick={() => onDecision("reject")}>Reject</button>
    </div>
  );
}
