import type { PulsationStage } from "../../ipc/contracts";
import { PIPELINE_STAGES } from "../../state/pulsationStore";

interface PipelineProgressProps {
  stage: PulsationStage;
}

export function PipelineProgress({ stage }: PipelineProgressProps) {
  const current = PIPELINE_STAGES.indexOf(stage);
  return (
    <ol className="pipeline-progress" aria-label="Pulsation stages">
      {PIPELINE_STAGES.map((item, index) => (
        <li className={index < current ? "is-complete" : index === current ? "is-current" : "is-upcoming"} key={item}>
          <span className="pipeline-dot">{index < current ? "✓" : index + 1}</span>
          <span>{item}</span>
        </li>
      ))}
    </ol>
  );
}
