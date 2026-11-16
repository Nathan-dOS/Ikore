-- Host-coordinated stage composition (ADR-022, pipeline v4).
--
-- Additive: v1-v3 rows keep their meaning and behaviour. Two existing tables
-- are rebuilt only to widen CHECK constraints (SQLite cannot alter them in
-- place); every other change is a new table or a nullable column.
--
-- The migration host disables foreign keys and verifies them after migration.
-- `legacy_alter_table` keeps DROP+RENAME from rewriting FK clauses in the many
-- tables that reference `pulsations`, matching V24/V27/V30/V31.
PRAGMA legacy_alter_table=ON;

-- 1. pulsations.pipeline_version admits 4.
CREATE TABLE pulsations_v32 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  feature_name TEXT NOT NULL,
  stage TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL, queue_sequence INTEGER, input_revision INTEGER NOT NULL DEFAULT 1 CHECK(input_revision > 0), state_version INTEGER NOT NULL DEFAULT 1 CHECK(state_version > 0), pipeline_version INTEGER NOT NULL DEFAULT 1 CHECK(pipeline_version IN (1,2,3,4)), terminal_reason TEXT, revision_feedback TEXT, coordination_hold INTEGER NOT NULL DEFAULT 0 CHECK(coordination_hold IN (0,1)), integration_checkpoint_id TEXT REFERENCES checkpoints(id), integration_state_version INTEGER, specification_started INTEGER NOT NULL DEFAULT 0 CHECK(specification_started IN (0,1)),
  FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
);

INSERT INTO pulsations_v32("id","project_id","feature_name","stage","status","created_at","updated_at","queue_sequence","input_revision","state_version","pipeline_version","terminal_reason","revision_feedback","coordination_hold","integration_checkpoint_id","integration_state_version","specification_started") SELECT "id","project_id","feature_name","stage","status","created_at","updated_at","queue_sequence","input_revision","state_version","pipeline_version","terminal_reason","revision_feedback","coordination_hold","integration_checkpoint_id","integration_state_version","specification_started" FROM pulsations;

DROP TABLE pulsations;

ALTER TABLE pulsations_v32 RENAME TO pulsations;

CREATE INDEX idx_pulsations_project_id
  ON pulsations(project_id);

CREATE UNIQUE INDEX idx_pulse_sequence ON pulsations(project_id, queue_sequence);

CREATE TRIGGER pulse_sequence_immutable BEFORE UPDATE OF queue_sequence ON pulsations
WHEN OLD.queue_sequence IS NOT NULL AND NEW.queue_sequence IS NOT OLD.queue_sequence
BEGIN SELECT RAISE(ABORT, 'queue_sequence is immutable'); END;

-- 2. stage_failure_requests admits the v4 failure classes and carries the
--    composed execution, the permitted revision targets and the remaining
--    shared allowance for the checkpoint UI.
CREATE TABLE stage_failure_requests_v32 (
  checkpoint_id TEXT PRIMARY KEY REFERENCES checkpoints(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  job_id TEXT NOT NULL REFERENCES jobs(id),
  stage TEXT NOT NULL,
  failure_class TEXT NOT NULL CHECK(failure_class IN (
    'retry_budget_exhausted','provider_configuration','provider_transport',
    'infrastructure','executor_setup','host_contract','model_reported_failure',
    'output_truncated',
    'budget_exhausted','validation_failed','validation_unavailable','contract_conflict',
    'capacity_exceeded','ownership_violation','stale_input')),
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
  stage_execution_id TEXT,
  allowed_revision_targets_json TEXT CHECK(allowed_revision_targets_json IS NULL OR json_valid(allowed_revision_targets_json)),
  remaining_budget_json TEXT CHECK(remaining_budget_json IS NULL OR json_valid(remaining_budget_json)),
  CHECK(allow_retry=1 OR allow_revision=1),
  CHECK(allow_revision=0 OR revision_stage IS NOT NULL)
);

INSERT INTO stage_failure_requests_v32("checkpoint_id","pulsation_id","job_id","stage","failure_class","allow_retry","allow_revision","revision_stage","error_code","diagnostic","agent_run_id","decision","resolved_at","created_at","updated_at")
SELECT "checkpoint_id","pulsation_id","job_id","stage","failure_class","allow_retry","allow_revision","revision_stage","error_code","diagnostic","agent_run_id","decision","resolved_at","created_at","updated_at" FROM stage_failure_requests;

DROP TABLE stage_failure_requests;

ALTER TABLE stage_failure_requests_v32 RENAME TO stage_failure_requests;

CREATE INDEX idx_stage_failure_pulsation ON stage_failure_requests(pulsation_id,created_at DESC);

PRAGMA legacy_alter_table=OFF;

-- 3. Immutable policy snapshots: recipes, effective executor profiles,
--    validation command profiles and composition settings as frozen at
--    admission/dispatch. Never carries secrets.
CREATE TABLE composition_policy_snapshots (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('recipe','executor_profile','validation_profile','composition_settings')),
  project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
  sha256 TEXT NOT NULL,
  snapshot_json TEXT NOT NULL CHECK(json_valid(snapshot_json)),
  created_at TEXT NOT NULL,
  UNIQUE(kind, sha256)
);

-- 4. Per-project composition settings (off by default) and optional
--    specialty overrides. Editing them affects future admissions only.
CREATE TABLE project_composition_settings (
  project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
  max_in_flight_units INTEGER NOT NULL DEFAULT 1 CHECK(max_in_flight_units IN (1,2)),
  validation_enabled INTEGER NOT NULL DEFAULT 0 CHECK(validation_enabled IN (0,1)),
  settings_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(settings_json)),
  updated_at TEXT NOT NULL
);

CREATE TABLE composition_specialty_overrides (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  executor_kind TEXT NOT NULL CHECK(executor_kind IN (
    'intake_classifier','specification_author','contract_author','code_implementer',
    'code_repairer','test_author','test_repairer','integration_reviewer')),
  execution_mode TEXT NOT NULL CHECK(execution_mode IN ('api_direct','cli_process')),
  backend TEXT CHECK(backend IS NULL OR backend IN ('codex','claude','gemini','opencode','custom')),
  model TEXT,
  thinking_level TEXT NOT NULL DEFAULT 'default',
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, executor_kind),
  CHECK((execution_mode='api_direct' AND backend IS NULL) OR (execution_mode='cli_process' AND backend IS NOT NULL))
);

CREATE TABLE project_validation_profiles (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  check_ref TEXT NOT NULL,
  profile_version INTEGER NOT NULL DEFAULT 1 CHECK(profile_version > 0),
  enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
  profile_json TEXT NOT NULL CHECK(json_valid(profile_json)),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, check_ref)
);

-- 5. Shared stage budget: one durable account per logical stage, retained
--    across parent Retry generations. Reservations are counted before
--    dispatch; consumed amounts never decrease.
CREATE TABLE stage_budget_accounts (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  stage TEXT NOT NULL,
  logical_stage_key TEXT NOT NULL UNIQUE,
  policy_sha256 TEXT NOT NULL,
  initial_invocation_limit INTEGER NOT NULL CHECK(initial_invocation_limit BETWEEN 0 AND 16),
  extra_invocation_limit INTEGER NOT NULL CHECK(extra_invocation_limit BETWEEN 0 AND 2),
  initial_output_token_limit INTEGER NOT NULL CHECK(initial_output_token_limit > 0),
  extra_output_token_limit INTEGER NOT NULL CHECK(extra_output_token_limit >= 0),
  initial_prompt_char_limit INTEGER NOT NULL CHECK(initial_prompt_char_limit > 0),
  extra_prompt_char_limit INTEGER NOT NULL CHECK(extra_prompt_char_limit >= 0),
  reserved_initial_invocations INTEGER NOT NULL DEFAULT 0 CHECK(reserved_initial_invocations >= 0),
  reserved_extra_invocations INTEGER NOT NULL DEFAULT 0 CHECK(reserved_extra_invocations >= 0),
  reserved_initial_output_tokens INTEGER NOT NULL DEFAULT 0 CHECK(reserved_initial_output_tokens >= 0),
  reserved_extra_output_tokens INTEGER NOT NULL DEFAULT 0 CHECK(reserved_extra_output_tokens >= 0),
  reserved_initial_prompt_chars INTEGER NOT NULL DEFAULT 0 CHECK(reserved_initial_prompt_chars >= 0),
  reserved_extra_prompt_chars INTEGER NOT NULL DEFAULT 0 CHECK(reserved_extra_prompt_chars >= 0),
  state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','exhausted','closed')),
  predecessor_account_id TEXT REFERENCES stage_budget_accounts(id),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX idx_stage_budget_accounts_pulsation ON stage_budget_accounts(pulsation_id, stage);

-- 6. Stage executions: one per parent job generation, with the frozen plan.
CREATE TABLE stage_executions (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  job_id TEXT NOT NULL REFERENCES jobs(id),
  parent_generation INTEGER NOT NULL CHECK(parent_generation > 0),
  parent_input_revision INTEGER NOT NULL CHECK(parent_input_revision > 0),
  parent_state_version INTEGER NOT NULL CHECK(parent_state_version > 0),
  stage TEXT NOT NULL,
  budget_account_id TEXT NOT NULL REFERENCES stage_budget_accounts(id),
  recipe_version INTEGER NOT NULL,
  policy_snapshot_id TEXT NOT NULL REFERENCES composition_policy_snapshots(id),
  plan_json TEXT NOT NULL CHECK(json_valid(plan_json)),
  plan_sha256 TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('planned','running','validating','ready','accepted','parked','failed','cancelled','superseded')),
  execution_version INTEGER NOT NULL DEFAULT 1 CHECK(execution_version > 0),
  blocked_code TEXT,
  blocked_reason TEXT,
  accepted_aggregate_sha256 TEXT,
  acceptance_id TEXT UNIQUE,
  predecessor_execution_id TEXT REFERENCES stage_executions(id),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(job_id, parent_generation)
);
CREATE INDEX idx_stage_executions_pulsation ON stage_executions(pulsation_id, stage, created_at DESC);

-- 7. Work units, dependencies and destination ownership.
CREATE TABLE stage_work_units (
  id TEXT PRIMARY KEY,
  execution_id TEXT NOT NULL REFERENCES stage_executions(id) ON DELETE CASCADE,
  logical_unit_key TEXT NOT NULL,
  ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
  executor_kind TEXT NOT NULL CHECK(executor_kind IN (
    'intake_classifier','specification_author','contract_author','code_implementer',
    'code_repairer','test_author','test_repairer','integration_reviewer')),
  spec_json TEXT NOT NULL CHECK(json_valid(spec_json)),
  spec_sha256 TEXT NOT NULL,
  generation INTEGER NOT NULL DEFAULT 1 CHECK(generation > 0),
  state_version INTEGER NOT NULL DEFAULT 1 CHECK(state_version > 0),
  state TEXT NOT NULL CHECK(state IN ('pending','running','validated','failed','cancelled','invalidated')),
  input_sha256 TEXT,
  reuse_key_sha256 TEXT,
  validated_result_json TEXT CHECK(validated_result_json IS NULL OR json_valid(validated_result_json)),
  validated_result_sha256 TEXT,
  reused_from_attempt_id TEXT,
  failure_code TEXT,
  failure_message TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(execution_id, logical_unit_key),
  UNIQUE(execution_id, ordinal)
);

CREATE TABLE stage_unit_dependencies (
  execution_id TEXT NOT NULL REFERENCES stage_executions(id) ON DELETE CASCADE,
  predecessor_unit_id TEXT NOT NULL REFERENCES stage_work_units(id) ON DELETE CASCADE,
  successor_unit_id TEXT NOT NULL REFERENCES stage_work_units(id) ON DELETE CASCADE,
  PRIMARY KEY(execution_id, predecessor_unit_id, successor_unit_id),
  CHECK(predecessor_unit_id <> successor_unit_id)
);

CREATE TABLE stage_unit_ownership (
  execution_id TEXT NOT NULL REFERENCES stage_executions(id) ON DELETE CASCADE,
  canonical_destination TEXT NOT NULL,
  relative_path TEXT NOT NULL,
  owner_unit_id TEXT NOT NULL REFERENCES stage_work_units(id) ON DELETE CASCADE,
  PRIMARY KEY(execution_id, canonical_destination)
);

-- 8. Reservations and attempts. A reservation exists before any external
--    request; an attempt references exactly one reservation and, once
--    started, the real agent run it reported into.
CREATE TABLE stage_invocation_reservations (
  id TEXT PRIMARY KEY,
  budget_account_id TEXT NOT NULL REFERENCES stage_budget_accounts(id) ON DELETE CASCADE,
  execution_id TEXT NOT NULL REFERENCES stage_executions(id) ON DELETE CASCADE,
  unit_id TEXT NOT NULL REFERENCES stage_work_units(id) ON DELETE CASCADE,
  logical_unit_key TEXT NOT NULL,
  idempotency_key TEXT NOT NULL UNIQUE,
  pool TEXT NOT NULL CHECK(pool IN ('initial','extra')),
  reserved_output_tokens INTEGER NOT NULL CHECK(reserved_output_tokens >= 0),
  reserved_prompt_chars INTEGER NOT NULL CHECK(reserved_prompt_chars >= 0),
  state TEXT NOT NULL CHECK(state IN ('reserved','started','settled','released')),
  llm_execution_id TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX idx_stage_reservations_account ON stage_invocation_reservations(budget_account_id, pool, state);

CREATE TABLE stage_work_unit_attempts (
  id TEXT PRIMARY KEY,
  unit_id TEXT NOT NULL REFERENCES stage_work_units(id) ON DELETE CASCADE,
  execution_id TEXT NOT NULL REFERENCES stage_executions(id) ON DELETE CASCADE,
  unit_generation INTEGER NOT NULL CHECK(unit_generation > 0),
  ordinal INTEGER NOT NULL CHECK(ordinal > 0),
  reservation_id TEXT NOT NULL REFERENCES stage_invocation_reservations(id),
  agent_run_id TEXT REFERENCES agent_runs(id),
  llm_execution_id TEXT,
  executor_kind TEXT NOT NULL,
  profile_snapshot_id TEXT NOT NULL REFERENCES composition_policy_snapshots(id),
  effective_model TEXT,
  observed_model TEXT,
  dispatch_sha256 TEXT,
  input_sha256 TEXT,
  result_sha256 TEXT,
  state TEXT NOT NULL CHECK(state IN ('reserved','running','succeeded','rejected','interrupted','cancelled')),
  error_code TEXT,
  diagnostic TEXT,
  started_at TEXT,
  finished_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(unit_id, unit_generation, ordinal)
);

-- 9. Host validation receipts (executed in M4; the table exists so every v4
--    schema change lands in one migration).
CREATE TABLE host_validation_runs (
  id TEXT PRIMARY KEY,
  execution_id TEXT NOT NULL REFERENCES stage_executions(id) ON DELETE CASCADE,
  parent_generation INTEGER NOT NULL CHECK(parent_generation > 0),
  check_ref TEXT NOT NULL,
  profile_snapshot_id TEXT NOT NULL REFERENCES composition_policy_snapshots(id),
  profile_sha256 TEXT NOT NULL,
  candidate_sha256 TEXT NOT NULL,
  input_manifest_sha256 TEXT NOT NULL,
  toolchain_sha256 TEXT NOT NULL,
  environment_sha256 TEXT NOT NULL,
  invocation_ordinal INTEGER NOT NULL CHECK(invocation_ordinal > 0),
  status TEXT NOT NULL CHECK(status IN ('passed','failed','timed_out','cancelled','unavailable','not_run','error')),
  exit_code INTEGER,
  started_at TEXT,
  finished_at TEXT,
  receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
  baseline_receipt_id TEXT REFERENCES host_validation_runs(id),
  created_at TEXT NOT NULL,
  UNIQUE(execution_id, check_ref, candidate_sha256, profile_sha256, invocation_ordinal)
);

-- 10. Idempotent recovery commands from the UI.
CREATE TABLE stage_recovery_requests (
  request_id TEXT PRIMARY KEY,
  stage_execution_id TEXT NOT NULL,
  action TEXT NOT NULL,
  result_json TEXT NOT NULL CHECK(json_valid(result_json)),
  created_at TEXT NOT NULL
);

-- 11. Traceability on existing tables. Nullable: v1-v3 rows never set them.
ALTER TABLE stage_acceptances ADD COLUMN stage_execution_id TEXT REFERENCES stage_executions(id);

ALTER TABLE token_usage_records ADD COLUMN stage_execution_id TEXT;
ALTER TABLE token_usage_records ADD COLUMN work_unit_id TEXT;
ALTER TABLE token_usage_records ADD COLUMN attempt_id TEXT;
ALTER TABLE token_usage_records ADD COLUMN executor_kind TEXT;
CREATE INDEX idx_token_usage_stage_execution ON token_usage_records(stage_execution_id);
