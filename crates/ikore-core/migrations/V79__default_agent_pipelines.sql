-- Feature 27 (Camadas A + B + C): built-in default agent pipelines, canonical steps,
-- and transactional domain trigger outbox.

ALTER TABLE custom_pipelines ADD COLUMN template_key TEXT;
CREATE UNIQUE INDEX idx_custom_pipelines_template_key
  ON custom_pipelines(template_key)
  WHERE template_key IS NOT NULL;

CREATE TABLE domain_trigger_outbox (
  id TEXT PRIMARY KEY NOT NULL,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  event TEXT NOT NULL CHECK(event IN ('feature_approved','note_captured','docs_stale','checkpoint_ready')),
  stage TEXT CHECK(stage IS NULL OR stage IN ('Intake','SPECIFICATION','Contract','Code','Test','Integrator')),
  subject_id TEXT,
  payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
  state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','dispatched','discarded')),
  discard_reason TEXT,
  created_at TEXT NOT NULL,
  dispatched_at TEXT
);
CREATE INDEX idx_domain_trigger_outbox_pending
  ON domain_trigger_outbox(created_at) WHERE state = 'pending';
CREATE INDEX idx_domain_trigger_outbox_project
  ON domain_trigger_outbox(project_id, created_at);

PRAGMA legacy_alter_table = ON;

CREATE TABLE custom_pipeline_runs_v79 (
  id TEXT PRIMARY KEY,
  pipeline_id TEXT NOT NULL REFERENCES custom_pipelines(id) ON DELETE CASCADE,
  version INTEGER NOT NULL,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  trigger_kind TEXT NOT NULL CHECK(trigger_kind IN ('manual','default_event','domain_event')),
  trigger_payload_json TEXT NOT NULL CHECK(json_valid(trigger_payload_json)),
  source_pulsation_id TEXT REFERENCES pulsations(id) ON DELETE SET NULL,
  -- The definition the run follows, resolved when it started (T4).
  resolved_definition_json TEXT NOT NULL CHECK(json_valid(resolved_definition_json)),
  resolved_sha256 TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN
    ('queued','running','waiting_gate','done','failed','cancelled','interrupted')),
  current_step INTEGER NOT NULL DEFAULT 0 CHECK(current_step >= 0),
  error TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  finished_at TEXT,
  trigger_outbox_id TEXT REFERENCES pipeline_trigger_outbox(id),
  domain_event_id TEXT REFERENCES domain_trigger_outbox(id) ON DELETE SET NULL,
  FOREIGN KEY(pipeline_id, version) REFERENCES custom_pipeline_versions(pipeline_id, version)
);
INSERT INTO custom_pipeline_runs_v79(
  id, pipeline_id, version, project_id, trigger_kind, trigger_payload_json,
  source_pulsation_id, resolved_definition_json, resolved_sha256, status,
  current_step, error, created_at, updated_at, finished_at, trigger_outbox_id, domain_event_id
)
SELECT
  id, pipeline_id, version, project_id, trigger_kind, trigger_payload_json,
  source_pulsation_id, resolved_definition_json, resolved_sha256, status,
  current_step, error, created_at, updated_at, finished_at, trigger_outbox_id, NULL
FROM custom_pipeline_runs;
DROP TABLE custom_pipeline_runs;
ALTER TABLE custom_pipeline_runs_v79 RENAME TO custom_pipeline_runs;
CREATE INDEX idx_custom_pipeline_runs_project
  ON custom_pipeline_runs(project_id, status, created_at);
CREATE INDEX idx_custom_pipeline_runs_pipeline
  ON custom_pipeline_runs(pipeline_id, created_at DESC);
CREATE UNIQUE INDEX custom_pipeline_runs_once_per_event
  ON custom_pipeline_runs(pipeline_id, trigger_outbox_id) WHERE trigger_outbox_id IS NOT NULL;
CREATE UNIQUE INDEX custom_pipeline_runs_once_per_domain_event
  ON custom_pipeline_runs(pipeline_id, domain_event_id) WHERE domain_event_id IS NOT NULL;

CREATE TABLE custom_pipeline_step_runs_v79 (
  run_id TEXT NOT NULL REFERENCES custom_pipeline_runs(id) ON DELETE CASCADE,
  step_index INTEGER NOT NULL CHECK(step_index >= 0),
  attempt INTEGER NOT NULL DEFAULT 1 CHECK(attempt > 0),
  step_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('agent','gate','host_tool','canonical')),
  status TEXT NOT NULL CHECK(status IN
    ('queued','running','waiting_gate','passed','rejected','done','failed','cancelled','interrupted')),
  terminal_run_id TEXT REFERENCES terminal_runs(id) ON DELETE SET NULL,
  llm_execution_id TEXT,
  output_text TEXT,
  verdict_json TEXT CHECK(verdict_json IS NULL OR json_valid(verdict_json)),
  gate_decision TEXT CHECK(gate_decision IS NULL OR gate_decision IN ('approved','rejected')),
  gate_feedback TEXT,
  started_at TEXT,
  finished_at TEXT,
  error TEXT,
  git_operation_id TEXT REFERENCES git_operations(id) ON DELETE SET NULL,
  PRIMARY KEY(run_id, step_index, attempt)
);
INSERT INTO custom_pipeline_step_runs_v79(
  run_id, step_index, attempt, step_id, kind, status, terminal_run_id,
  llm_execution_id, output_text, verdict_json, gate_decision, gate_feedback,
  started_at, finished_at, error, git_operation_id
)
SELECT
  run_id, step_index, attempt, step_id, kind, status, terminal_run_id,
  llm_execution_id, output_text, verdict_json, gate_decision, gate_feedback,
  started_at, finished_at, error, git_operation_id
FROM custom_pipeline_step_runs;
DROP TABLE custom_pipeline_step_runs;
ALTER TABLE custom_pipeline_step_runs_v79 RENAME TO custom_pipeline_step_runs;
CREATE INDEX idx_custom_pipeline_step_runs_git_op
  ON custom_pipeline_step_runs(git_operation_id)
  WHERE git_operation_id IS NOT NULL;

PRAGMA legacy_alter_table = OFF;
