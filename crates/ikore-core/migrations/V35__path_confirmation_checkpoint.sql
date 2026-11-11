-- Adds the 'path_confirmation' checkpoint purpose/kind for the ADR-022
-- clarification-loop preventive HITL gate (P5): a host-authored advisory,
-- not a model question, so it reuses `checkpoints` rather than
-- `specification_question_sets`. Same legacy_alter_table rationale as V27
-- (checkpoints is referenced by specification_question_sets,
-- pulsations.integration_checkpoint_id, cli_fallback_requests and
-- pulse_execution_overrides).
PRAGMA legacy_alter_table=ON;

CREATE TABLE checkpoints_v35 (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL,
  stage TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'artifact_review'
    CHECK(kind IN ('artifact_review','clarification','cli_fallback','stage_failure','path_confirmation')),
  input_revision INTEGER NOT NULL DEFAULT 1,
  state_version INTEGER NOT NULL DEFAULT 1,
  producer_job_id TEXT REFERENCES jobs(id),
  generation INTEGER,
  purpose TEXT NOT NULL DEFAULT 'artifact_review'
    CHECK(purpose IN ('artifact_review','clarification','cli_fallback','stage_failure','path_confirmation')),
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE CASCADE
);

INSERT INTO checkpoints_v35("id","pulsation_id","stage","status","created_at","updated_at","kind","input_revision","state_version","producer_job_id","generation","purpose")
SELECT "id","pulsation_id","stage","status","created_at","updated_at","kind","input_revision","state_version","producer_job_id","generation","purpose" FROM checkpoints;

DROP TABLE checkpoints;

ALTER TABLE checkpoints_v35 RENAME TO checkpoints;

CREATE INDEX idx_checkpoints_pulsation_id ON checkpoints(pulsation_id);
CREATE UNIQUE INDEX idx_checkpoint_producer ON checkpoints(producer_job_id,generation,kind);
CREATE INDEX idx_checkpoints_purpose ON checkpoints(pulsation_id,purpose,status);

PRAGMA legacy_alter_table=OFF;
