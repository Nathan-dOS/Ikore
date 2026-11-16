-- The migration host disables foreign keys and verifies them after migration.
-- `checkpoints` is referenced by four tables (specification_question_sets,
-- pulsations.integration_checkpoint_id, cli_fallback_requests,
-- pulse_execution_overrides), so legacy_alter_table keeps DROP+RENAME from
-- rewriting their FK clauses.
PRAGMA legacy_alter_table=ON;

CREATE TABLE checkpoints_v27 (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL,
  stage TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'artifact_review'
    CHECK(kind IN ('artifact_review','clarification','cli_fallback','stage_failure')),
  input_revision INTEGER NOT NULL DEFAULT 1,
  state_version INTEGER NOT NULL DEFAULT 1,
  producer_job_id TEXT REFERENCES jobs(id),
  generation INTEGER,
  purpose TEXT NOT NULL DEFAULT 'artifact_review'
    CHECK(purpose IN ('artifact_review','clarification','cli_fallback','stage_failure')),
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE CASCADE
);

INSERT INTO checkpoints_v27("id","pulsation_id","stage","status","created_at","updated_at","kind","input_revision","state_version","producer_job_id","generation","purpose")
SELECT "id","pulsation_id","stage","status","created_at","updated_at","kind","input_revision","state_version","producer_job_id","generation","purpose" FROM checkpoints;

-- V20 left `kind` behind `purpose`, which forced open_cli_fallback to write
-- kind='artifact_review'. Realign them so idx_checkpoint_producer separates the
-- checkpoint types again. This must run AFTER the copy: the old table's `kind`
-- CHECK would reject the cli_fallback rows.
UPDATE checkpoints_v27 SET kind=purpose;

DROP TABLE checkpoints;

ALTER TABLE checkpoints_v27 RENAME TO checkpoints;

CREATE INDEX idx_checkpoints_pulsation_id ON checkpoints(pulsation_id);
CREATE UNIQUE INDEX idx_checkpoint_producer ON checkpoints(producer_job_id,generation,kind);
CREATE INDEX idx_checkpoints_purpose ON checkpoints(pulsation_id,purpose,status);

PRAGMA legacy_alter_table=OFF;

-- A stage failure that would offer only Abort is a dead end; the host goes
-- terminal instead, and the CHECK below keeps that invariant in the schema.
CREATE TABLE stage_failure_requests (
  checkpoint_id TEXT PRIMARY KEY REFERENCES checkpoints(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  job_id TEXT NOT NULL REFERENCES jobs(id),
  stage TEXT NOT NULL,
  failure_class TEXT NOT NULL CHECK(failure_class IN (
    'retry_budget_exhausted','provider_configuration','provider_transport',
    'infrastructure','executor_setup','host_contract','model_reported_failure')),
  allow_retry INTEGER NOT NULL CHECK(allow_retry IN (0,1)),
  allow_revision INTEGER NOT NULL CHECK(allow_revision IN (0,1)),
  revision_stage TEXT,
  error_code TEXT NOT NULL,
  diagnostic TEXT NOT NULL,
  agent_run_id TEXT,
  decision TEXT CHECK(decision IS NULL OR decision IN ('retry_stage','request_revision','abort')),
  resolved_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK(allow_retry=1 OR allow_revision=1),
  CHECK(allow_revision=0 OR revision_stage IS NOT NULL)
);
CREATE INDEX idx_stage_failure_pulsation ON stage_failure_requests(pulsation_id,created_at DESC);
