import { useEffect, useState } from "react";

import { usePulsationStore } from "../../state/pulsationStore";
import { useViewStore } from "../../state/viewStore";
import { formatDuration } from "../../utils/formatters";

function VibeTimer() {
  const startedAt = useViewStore((state) => state.sessionStartedAt);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  return <span>Vibe Time: {formatDuration(now - startedAt)}</span>;
}

export function StatusBar() {
  const hitlCount = usePulsationStore((state) => state.hitlWaitingCount);
  const agentsCount = usePulsationStore((state) => state.backgroundAgentsCount);
  return (
    <footer className="status-bar">
      <span className={hitlCount > 0 ? "status-attention" : ""}>{hitlCount} HITL review(s) waiting</span>
      <span>{agentsCount} agent(s) processing in background</span>
      <VibeTimer />
    </footer>
  );
}
