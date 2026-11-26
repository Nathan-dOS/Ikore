-- Feature 25, M4: stage verifiers that can reject a stage result, behind Labs.
--
-- `terminal_runs` already admits `origin = 'verification'` (added in V70 so M4
-- needs no second rebuild). Three existing tables are rebuilt here only to
-- widen CHECK constraints and the producer uniqueness index:
--   1. `checkpoints`: widens `kind` and `purpose` with `'verification_error'`,
--      keeps `visualizer_brief_json` (V47), and makes `idx_checkpoint_producer`
--      partial (`WHERE kind <> 'verification_error'`) so a retry or a second
--      verifier error on the same stage generation does not collide.
--   2. `stage_failure_requests`: widens `failure_class` with
--      `'verification_rejected'` and `decision` with `'continue_anyway'`, and
--      adds `allow_continue_anyway` and optional `justification` (<= 500 chars).
--   3. `checkpoint_approval_receipts`: widens `purpose` with
--      `'verification_error'` and `decision` with `'continue_anyway'`,
--      `'retry_verifier'` and `'skip_verifier'`, keeping its append-only trigger.
--
-- `legacy_alter_table` keeps DROP+RENAME from rewriting FK clauses in the ten
-- tables that reference `checkpoints`; the runner applies migrations with
-- foreign keys off and verifies `pragma_foreign_key_check` immediately after.
-- Everything after the rebuild is additive.
PRAGMA legacy_alter_table=ON;

-- 1. checkpoints: widen kind & purpose with 'verification_error', keep V47's
--    visualizer_brief_json, and exclude verification_error from producer uniqueness.
CREATE TABLE checkpoints_v75 (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL,
  stage TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'artifact_review'
    CHECK(kind IN (
      'artifact_review','clarification','cli_fallback','stage_failure',
      'path_confirmation','verification_error'
    )),
  input_revision INTEGER NOT NULL DEFAULT 1,
  state_version INTEGER NOT NULL DEFAULT 1,
  producer_job_id TEXT REFERENCES jobs(id),
  generation INTEGER,
  purpose TEXT NOT NULL DEFAULT 'artifact_review'
    CHECK(purpose IN (
      'artifact_review','clarification','cli_fallback','stage_failure',
      'path_confirmation','verification_error'
    )),
  visualizer_brief_json TEXT
    CHECK(visualizer_brief_json IS NULL OR json_valid(visualizer_brief_json)),
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE CASCADE
);

INSERT INTO checkpoints_v75(
  "id","pulsation_id","stage","status","created_at","updated_at","kind",
  "input_revision","state_version","producer_job_id","generation","purpose",
  "visualizer_brief_json"
)
SELECT
  "id","pulsation_id","stage","status","created_at","updated_at","kind",
  "input_revision","state_version","producer_job_id","generation","purpose",
  "visualizer_brief_json"
FROM checkpoints;

DROP TABLE checkpoints;

ALTER TABLE checkpoints_v75 RENAME TO checkpoints;

CREATE INDEX idx_checkpoints_pulsation_id ON checkpoints(pulsation_id);
CREATE UNIQUE INDEX idx_checkpoint_producer
  ON checkpoints(producer_job_id,generation,kind)
  WHERE kind <> 'verification_error';
CREATE INDEX idx_checkpoints_purpose ON checkpoints(pulsation_id,purpose,status);

-- 2. stage_failure_requests: admit 'verification_rejected' and 'continue_anyway',
--    allow ContinueAnyway as an offered choice, and store its optional justification.
CREATE TABLE stage_failure_requests_v75 (
  checkpoint_id TEXT PRIMARY KEY REFERENCES checkpoints(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  job_id TEXT NOT NULL REFERENCES jobs(id),
  stage TEXT NOT NULL,
  failure_class TEXT NOT NULL CHECK(failure_class IN (
    'retry_budget_exhausted','provider_configuration','provider_transport',
    'infrastructure','executor_setup','host_contract','model_reported_failure',
    'output_truncated',
    'budget_exhausted','validation_failed','validation_unavailable','contract_conflict',
    'capacity_exceeded','ownership_violation','stale_input',
    'verification_rejected')),
  allow_retry INTEGER NOT NULL CHECK(allow_retry IN (0,1)),
  allow_revision INTEGER NOT NULL CHECK(allow_revision IN (0,1)),
  revision_stage TEXT,
  error_code TEXT NOT NULL,
  diagnostic TEXT NOT NULL,
  agent_run_id TEXT,
  decision TEXT CHECK(decision IS NULL OR decision IN (
    'retry_stage','request_revision','abort','continue_anyway')),
  resolved_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  stage_execution_id TEXT,
  allowed_revision_targets_json TEXT
    CHECK(allowed_revision_targets_json IS NULL OR json_valid(allowed_revision_targets_json)),
  remaining_budget_json TEXT
    CHECK(remaining_budget_json IS NULL OR json_valid(remaining_budget_json)),
  allow_continue_anyway INTEGER NOT NULL DEFAULT 0 CHECK(allow_continue_anyway IN (0,1)),
  justification TEXT CHECK(justification IS NULL OR length(justification) <= 500),
  CHECK(allow_retry=1 OR allow_revision=1 OR allow_continue_anyway=1),
  CHECK(allow_revision=0 OR revision_stage IS NOT NULL)
);

INSERT INTO stage_failure_requests_v75(
  "checkpoint_id","pulsation_id","job_id","stage","failure_class",
  "allow_retry","allow_revision","revision_stage","error_code","diagnostic",
  "agent_run_id","decision","resolved_at","created_at","updated_at",
  "stage_execution_id","allowed_revision_targets_json","remaining_budget_json",
  "allow_continue_anyway","justification"
)
SELECT
  "checkpoint_id","pulsation_id","job_id","stage","failure_class",
  "allow_retry","allow_revision","revision_stage","error_code","diagnostic",
  "agent_run_id","decision","resolved_at","created_at","updated_at",
  "stage_execution_id","allowed_revision_targets_json","remaining_budget_json",
  0,NULL
FROM stage_failure_requests;

DROP TABLE stage_failure_requests;

ALTER TABLE stage_failure_requests_v75 RENAME TO stage_failure_requests;

CREATE INDEX idx_stage_failure_pulsation
  ON stage_failure_requests(pulsation_id,created_at DESC);

-- 3. checkpoint_approval_receipts: admit 'verification_error' and the M4
--    checkpoint decisions while preserving its append-only trigger.
CREATE TABLE checkpoint_approval_receipts_v75 (
  checkpoint_id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  purpose TEXT NOT NULL CHECK(purpose IN (
    'artifact_review','clarification','cli_fallback','stage_failure',
    'path_confirmation','verification_error')),
  decision TEXT NOT NULL CHECK(decision IN (
    'approved','rejected','revision_requested',
    'retry_stage','request_revision','abort','grant_scope',
    'continue_anyway','retry_verifier','skip_verifier',
    'use_api','retry_cli','cancel',
    'confirmed','answered')),
  receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
  receipt_sha256 TEXT NOT NULL CHECK(length(receipt_sha256) = 64),
  mandate_revision INTEGER CHECK(mandate_revision IS NULL OR mandate_revision >= 1),
  created_at TEXT NOT NULL
);

INSERT INTO checkpoint_approval_receipts_v75(
  "checkpoint_id","pulsation_id","purpose","decision",
  "receipt_json","receipt_sha256","mandate_revision","created_at"
)
SELECT
  "checkpoint_id","pulsation_id","purpose","decision",
  "receipt_json","receipt_sha256","mandate_revision","created_at"
FROM checkpoint_approval_receipts;

DROP TABLE checkpoint_approval_receipts;

ALTER TABLE checkpoint_approval_receipts_v75 RENAME TO checkpoint_approval_receipts;

CREATE INDEX idx_checkpoint_approval_receipts_pulsation
  ON checkpoint_approval_receipts(pulsation_id, created_at);

CREATE TRIGGER checkpoint_approval_receipts_append_only BEFORE UPDATE ON checkpoint_approval_receipts
BEGIN SELECT RAISE(ABORT, 'approval receipts are append-only'); END;

PRAGMA legacy_alter_table=OFF;

-- 4. Project-scoped verifiers attached to any of the six default stages.
--    `stage` uses `PulsationStage::as_db_value()`.
CREATE TABLE stage_verifiers (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  stage TEXT NOT NULL CHECK(stage IN ('Intake','SPECIFICATION','Contract','Code','Test','Integrator')),
  label TEXT NOT NULL CHECK(length(trim(label)) BETWEEN 1 AND 60 AND length(label) <= 60),
  position INTEGER NOT NULL DEFAULT 0 CHECK(position >= 0),
  enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
  reject_limit INTEGER NOT NULL DEFAULT 2 CHECK(reject_limit BETWEEN 0 AND 5),
  agent_json TEXT NOT NULL CHECK(json_valid(agent_json)),
  instructions TEXT NOT NULL CHECK(length(instructions) <= 16384),
  version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(project_id, stage, label)
);
CREATE INDEX idx_stage_verifiers_project_stage
  ON stage_verifiers(project_id, stage, position);

-- 5. Historical and active verifier runs. `ON DELETE RESTRICT` on `verifier_id`
--    keeps audit history intact when a developer tries to delete a verifier
--    that already ran (permanent project deletion runs with foreign_keys OFF
--    and removes the entire project graph cleanly).
CREATE TABLE stage_verifier_runs (
  id TEXT PRIMARY KEY,
  verifier_id TEXT NOT NULL REFERENCES stage_verifiers(id) ON DELETE RESTRICT,
  -- Snapshot of the verifier config at creation: edits never mutate past runs.
  config_json TEXT NOT NULL CHECK(json_valid(config_json)),
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  stage TEXT NOT NULL CHECK(stage IN ('Intake','SPECIFICATION','Contract','Code','Test','Integrator')),
  stage_job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  stage_generation INTEGER NOT NULL CHECK(stage_generation >= 1),
  attempt INTEGER NOT NULL DEFAULT 1 CHECK(attempt >= 1),
  recovery_count INTEGER NOT NULL DEFAULT 0 CHECK(recovery_count >= 0),
  status TEXT NOT NULL CHECK(status IN (
    'pending','running','passed','rejected','error','skipped','overridden','cancelled')),
  terminal_run_id TEXT REFERENCES terminal_runs(id) ON DELETE SET NULL,
  llm_execution_id TEXT,
  verdict_json TEXT CHECK(verdict_json IS NULL OR json_valid(verdict_json)),
  created_at TEXT NOT NULL,
  started_at TEXT,
  finished_at TEXT,
  error TEXT,
  UNIQUE(verifier_id, stage_job_id, stage_generation, attempt)
);
CREATE INDEX idx_stage_verifier_runs_pending
  ON stage_verifier_runs(status, created_at) WHERE status = 'pending';
CREATE INDEX idx_stage_verifier_runs_pulsation_stage
  ON stage_verifier_runs(pulsation_id, stage, stage_generation, created_at);
CREATE INDEX idx_stage_verifier_runs_verifier
  ON stage_verifier_runs(verifier_id, created_at DESC);

-- 6. Deferred default-pipeline disposition while verifiers run on a stage generation.
--    `deferred_disposition` is `'advance:<stage>'` or `'await_review'`.
CREATE TABLE pulsation_stage_verification (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  stage TEXT NOT NULL CHECK(stage IN ('Intake','SPECIFICATION','Contract','Code','Test','Integrator')),
  stage_generation INTEGER NOT NULL CHECK(stage_generation >= 1),
  stage_job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  input_revision INTEGER NOT NULL CHECK(input_revision >= 1),
  state TEXT NOT NULL CHECK(state IN (
    'running','passed','rejected','error','overridden','cancelled')),
  deferred_disposition TEXT NOT NULL CHECK(
    deferred_disposition = 'await_review'
    OR deferred_disposition IN (
      'advance:SPECIFICATION','advance:Contract','advance:Code','advance:Test','advance:Integrator'
    )
  ),
  updated_at TEXT NOT NULL,
  PRIMARY KEY(pulsation_id, stage, stage_generation)
);
CREATE INDEX idx_pulsation_stage_verification_job
  ON pulsation_stage_verification(stage_job_id);

-- 7. Companion detail table for `verification_error` checkpoints (mirrors
--    `stage_failure_requests` and `cli_fallback_requests`).
CREATE TABLE verification_error_requests (
  checkpoint_id TEXT PRIMARY KEY REFERENCES checkpoints(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  verifier_run_id TEXT NOT NULL REFERENCES stage_verifier_runs(id) ON DELETE CASCADE,
  stage TEXT NOT NULL CHECK(stage IN ('Intake','SPECIFICATION','Contract','Code','Test','Integrator')),
  error_message TEXT NOT NULL,
  decision TEXT CHECK(decision IS NULL OR decision IN ('retry_verifier','skip_verifier','abort')),
  resolved_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX idx_verification_error_pulsation
  ON verification_error_requests(pulsation_id, created_at DESC);
CREATE INDEX idx_verification_error_run
  ON verification_error_requests(verifier_run_id);
