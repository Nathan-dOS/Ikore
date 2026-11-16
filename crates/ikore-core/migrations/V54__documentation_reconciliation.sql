-- ADR-034 M5: documentation reconciliation (code -> note).
--
-- The Documentation Reconciler is a canonical role routed like any other (API
-- or CLI), so, as V50 did for the Notes Scribe, the tables whose role column is
-- CHECK-constrained gain 'documentation_reconciler' and terminal runs gain the
-- 'reconciliation' origin. The definitions are the tables as V50 left them
-- (V51 to V53 did not touch them). `legacy_alter_table` keeps DROP+RENAME from
-- rewriting FK clauses elsewhere; the runner applies migrations with foreign
-- keys off. Everything after the rebuild is additive.
PRAGMA legacy_alter_table=ON;

CREATE TABLE cli_agent_configs_v54 (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent','visualizer','notes_scribe',
    'documentation_reconciler'
  )),
  execution_mode TEXT NOT NULL DEFAULT 'api_direct'
    CHECK(execution_mode IN ('api_direct','cli_process')),
  backend TEXT NOT NULL DEFAULT 'codex'
    CHECK(backend IN ('codex','claude','gemini','opencode','antigravity','custom')),
  model TEXT,
  thinking_level TEXT NOT NULL DEFAULT 'default'
    CHECK(thinking_level IN ('default','low','medium','high')),
  custom_executable TEXT,
  custom_args_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(custom_args_json)),
  custom_risk_ack INTEGER NOT NULL DEFAULT 0 CHECK(custom_risk_ack IN (0,1)),
  version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL, model_profile TEXT, model_source TEXT NOT NULL DEFAULT 'default'
  CHECK(model_source IN ('default','catalog','manual')),
  PRIMARY KEY(project_id,agent_role)
);
INSERT INTO cli_agent_configs_v54("project_id","agent_role","execution_mode","backend","model","thinking_level","custom_executable","custom_args_json","custom_risk_ack","version","created_at","updated_at","model_profile","model_source")
SELECT "project_id","agent_role","execution_mode","backend","model","thinking_level","custom_executable","custom_args_json","custom_risk_ack","version","created_at","updated_at","model_profile","model_source" FROM cli_agent_configs;
DROP TABLE cli_agent_configs;
ALTER TABLE cli_agent_configs_v54 RENAME TO cli_agent_configs;
CREATE INDEX idx_cli_agent_configs_project ON cli_agent_configs(project_id);

CREATE TABLE terminal_slots_v54 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('canonical','free_form','ephemeral')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent','visualizer','notes_scribe',
    'documentation_reconciler'
  )),
  label TEXT NOT NULL,
  execution_mode TEXT NOT NULL DEFAULT 'api_direct'
    CHECK(execution_mode IN ('api_direct','cli_process')),
  backend TEXT NOT NULL DEFAULT 'codex'
    CHECK(backend IN ('codex','claude','gemini','opencode','antigravity','custom')),
  model TEXT,
  thinking_level TEXT NOT NULL DEFAULT 'default'
    CHECK(thinking_level IN ('default','low','medium','high')),
  session_policy TEXT NOT NULL DEFAULT 'resume_explicit'
    CHECK(session_policy IN ('fresh_per_run','resume_per_pulsation','resume_explicit')),
  custom_executable TEXT,
  custom_args_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(custom_args_json)),
  custom_risk_ack INTEGER NOT NULL DEFAULT 0 CHECK(custom_risk_ack IN (0,1)),
  position INTEGER NOT NULL DEFAULT 0 CHECK(position >= 0),
  archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0,1)),
  version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  model_profile TEXT,
  model_source TEXT NOT NULL DEFAULT 'default'
    CHECK(model_source IN ('default','catalog','manual')),
  CHECK(
    (kind IN ('canonical','ephemeral') AND agent_role IS NOT NULL)
    OR (kind='free_form' AND agent_role IS NULL)
  )
);
INSERT INTO terminal_slots_v54("id","project_id","kind","agent_role","label","execution_mode","backend","model","thinking_level","session_policy","custom_executable","custom_args_json","custom_risk_ack","position","archived","version","created_at","updated_at","model_profile","model_source")
SELECT "id","project_id","kind","agent_role","label","execution_mode","backend","model","thinking_level","session_policy","custom_executable","custom_args_json","custom_risk_ack","position","archived","version","created_at","updated_at","model_profile","model_source" FROM terminal_slots;
DROP TABLE terminal_slots;
ALTER TABLE terminal_slots_v54 RENAME TO terminal_slots;
CREATE UNIQUE INDEX idx_terminal_slots_canonical_role
  ON terminal_slots(project_id,agent_role) WHERE kind='canonical';
CREATE INDEX idx_terminal_slots_project_position
  ON terminal_slots(project_id,archived,kind,position);

CREATE TABLE terminal_runs_v54 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  slot_kind TEXT NOT NULL DEFAULT 'canonical'
    CHECK(slot_kind IN ('canonical','free_form','ephemeral')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent','visualizer','notes_scribe',
    'documentation_reconciler'
  )),
  free_form_slot_id TEXT,
  free_form_label TEXT,
  origin TEXT NOT NULL CHECK(origin IN ('pipeline','learning','manual','feature','review','notes','reconciliation')),
  execution_mode TEXT NOT NULL CHECK(execution_mode IN ('api_direct','cli_process')),
  backend TEXT CHECK(backend IS NULL OR backend IN ('codex','claude','gemini','opencode','antigravity','custom')),
  model TEXT,
  native_session_id TEXT,
  pulsation_id TEXT REFERENCES pulsations(id) ON DELETE SET NULL,
  job_id TEXT REFERENCES jobs(id) ON DELETE SET NULL,
  agent_run_id TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
  llm_execution_id TEXT,
  status TEXT NOT NULL CHECK(status IN (
    'queued','starting','running','awaiting_fallback','completed','failed',
    'cancelled','interrupted'
  )),
  exit_code INTEGER,
  error_code TEXT,
  error_message TEXT,
  created_at TEXT NOT NULL,
  started_at TEXT,
  finished_at TEXT,
  updated_at TEXT NOT NULL,
  deadline_at TEXT,
  termination_reason TEXT CHECK(termination_reason IS NULL OR termination_reason IN (
    'exited','user_cancelled','pipeline_cancelled','shutdown','timeout',
    'output_limit','spawn_failed','io_failed','persistence_failed','interrupted'
  )),
  output_bytes INTEGER NOT NULL DEFAULT 0 CHECK(output_bytes >= 0),
  event_count INTEGER NOT NULL DEFAULT 0 CHECK(event_count >= 0),
  workspace_snapshot_hash TEXT,
  launch_identity_sha256 TEXT,
  detected_cli_version TEXT,
  adapter_contract_version INTEGER NOT NULL DEFAULT 1 CHECK(adapter_contract_version > 0),
  slot_id TEXT REFERENCES terminal_slots(id) ON DELETE SET NULL,
  parent_run_id TEXT REFERENCES terminal_runs(id) ON DELETE SET NULL,
  continuation_reason TEXT CHECK(continuation_reason IS NULL OR continuation_reason IN (
    'explicit_resume','resume_per_pulsation','restart_recovery'
  )),
  session_fingerprint TEXT,
  requested_model_profile TEXT,
  model_source TEXT NOT NULL DEFAULT 'default'
    CHECK(model_source IN ('default','catalog','manual')),
  observed_model TEXT,
  observation_trial_id TEXT REFERENCES cli_observation_trials(id) ON DELETE SET NULL,
  max_transport_silence_ms INTEGER
    CHECK(max_transport_silence_ms IS NULL OR max_transport_silence_ms >= 0),
  CHECK(
    (slot_kind IN ('canonical','ephemeral') AND agent_role IS NOT NULL AND free_form_slot_id IS NULL)
    OR (slot_kind='free_form' AND agent_role IS NULL AND free_form_slot_id IS NOT NULL)
  )
);
INSERT INTO terminal_runs_v54("id","project_id","slot_kind","agent_role","free_form_slot_id","free_form_label","origin","execution_mode","backend","model","native_session_id","pulsation_id","job_id","agent_run_id","llm_execution_id","status","exit_code","error_code","error_message","created_at","started_at","finished_at","updated_at","deadline_at","termination_reason","output_bytes","event_count","workspace_snapshot_hash","launch_identity_sha256","detected_cli_version","adapter_contract_version","slot_id","parent_run_id","continuation_reason","session_fingerprint","requested_model_profile","model_source","observed_model","observation_trial_id","max_transport_silence_ms")
SELECT "id","project_id","slot_kind","agent_role","free_form_slot_id","free_form_label","origin","execution_mode","backend","model","native_session_id","pulsation_id","job_id","agent_run_id","llm_execution_id","status","exit_code","error_code","error_message","created_at","started_at","finished_at","updated_at","deadline_at","termination_reason","output_bytes","event_count","workspace_snapshot_hash","launch_identity_sha256","detected_cli_version","adapter_contract_version","slot_id","parent_run_id","continuation_reason","session_fingerprint","requested_model_profile","model_source","observed_model","observation_trial_id","max_transport_silence_ms" FROM terminal_runs;
DROP TABLE terminal_runs;
ALTER TABLE terminal_runs_v54 RENAME TO terminal_runs;
CREATE INDEX idx_terminal_runs_active ON terminal_runs(project_id,status,updated_at DESC)
  WHERE status IN ('queued','starting','running');
CREATE INDEX idx_terminal_runs_observation_trial
  ON terminal_runs(observation_trial_id, created_at);
CREATE UNIQUE INDEX idx_terminal_runs_one_active_per_slot
  ON terminal_runs(project_id,slot_id)
  WHERE slot_id IS NOT NULL AND status IN ('queued','starting','running');
CREATE INDEX idx_terminal_runs_parent ON terminal_runs(parent_run_id);
CREATE INDEX idx_terminal_runs_project_role_created
  ON terminal_runs(project_id,agent_role,created_at DESC);
CREATE INDEX idx_terminal_runs_pulsation ON terminal_runs(pulsation_id,created_at DESC);
CREATE INDEX idx_terminal_runs_slot_created
  ON terminal_runs(project_id,slot_id,created_at DESC);

PRAGMA legacy_alter_table=OFF;

-- The code bytes of a baseline, deduplicated by hash, so a proposal can show
-- the line diff base -> current. Only UTF-8 files up to 256 KiB are stored,
-- and at most 64 MiB per project (enforced by the host); above that only the
-- hash is kept (`base_unstored`). Local only, the same exposure class as
-- `artifacts.content`; a blob no baseline references is deleted.
CREATE TABLE doc_reconciliation_blobs (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  content_sha256 TEXT NOT NULL,
  content TEXT NOT NULL,
  byte_len INTEGER NOT NULL CHECK(byte_len BETWEEN 0 AND 262144),
  created_at TEXT NOT NULL,
  PRIMARY KEY(project_id, content_sha256)
);

-- One row per explicit link note -> code file (`source-files`/`test-files`):
-- the hashes at the last reconciliation, or at the first observation, which
-- never raises a signal. `code_sha256` is NULL when the file was absent.
CREATE TABLE doc_reconciliation_baselines (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  note_path TEXT NOT NULL CHECK(length(note_path) BETWEEN 1 AND 1024),
  code_path TEXT NOT NULL CHECK(length(code_path) BETWEEN 1 AND 1024),
  link_kind TEXT NOT NULL CHECK(link_kind IN ('source','test')),
  note_sha256 TEXT,
  code_sha256 TEXT,
  base_state TEXT NOT NULL CHECK(base_state IN ('stored','base_unstored','absent')),
  state TEXT NOT NULL DEFAULT 'tracking' CHECK(state IN ('tracking','silenced')),
  reason TEXT CHECK(reason IS NULL OR length(reason) BETWEEN 1 AND 400),
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, note_path, code_path),
  CHECK((base_state='absent')=(code_sha256 IS NULL))
);
CREATE INDEX doc_reconciliation_baselines_code
  ON doc_reconciliation_baselines(project_id, code_path);

-- A linked file whose hash left its baseline. At most one active signal
-- (open or proposed) per note -> path; a newer change updates it. The origin
-- only records the trigger: it is never proof of a defect or of authorship.
CREATE TABLE doc_drift_signals (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  note_path TEXT NOT NULL,
  code_path TEXT NOT NULL,
  base_sha256 TEXT,
  current_sha256 TEXT,
  origin TEXT NOT NULL CHECK(origin IN ('watcher','startup_scan','pulsation')),
  pulsation_id TEXT REFERENCES pulsations(id) ON DELETE SET NULL,
  state TEXT NOT NULL CHECK(state IN ('open','proposed','resolved','ignored')),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK(origin='pulsation' OR pulsation_id IS NULL)
);
CREATE UNIQUE INDEX doc_drift_signals_one_active
  ON doc_drift_signals(project_id, note_path, code_path)
  WHERE state IN ('open','proposed');

-- A note update the developer asked for (decision D6), with the hashes it was
-- generated against: any divergence at approval invalidates it.
CREATE TABLE doc_reconciliation_proposals (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  note_path TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('pending','edited','published','rejected','invalidated')),
  expected_note_sha256 TEXT NOT NULL,
  expected_code_json TEXT NOT NULL CHECK(json_valid(expected_code_json)),
  request_sha256 TEXT NOT NULL,
  summary_json TEXT NOT NULL CHECK(json_valid(summary_json)),
  edits_json TEXT NOT NULL CHECK(json_valid(edits_json)),
  unresolved_json TEXT NOT NULL CHECK(json_valid(unresolved_json)),
  code_diffs_json TEXT NOT NULL CHECK(json_valid(code_diffs_json)),
  proposed_content TEXT NOT NULL,
  preview_diff TEXT NOT NULL,
  reason TEXT CHECK(reason IS NULL OR length(reason) BETWEEN 1 AND 400),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX doc_reconciliation_proposals_one_open
  ON doc_reconciliation_proposals(project_id, note_path)
  WHERE state IN ('pending','edited');

-- The last scan at project open: `partial_scan` when the links exceeded the
-- per-scan limit, so the gap is visible.
CREATE TABLE doc_reconciliation_scans (
  project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  state TEXT NOT NULL CHECK(state IN ('complete','partial_scan')),
  scanned INTEGER NOT NULL CHECK(scanned >= 0),
  total INTEGER NOT NULL CHECK(total >= scanned),
  scanned_at TEXT NOT NULL
);

-- Finished pulsations whose publications were already compared with the
-- baselines, so each is handled once.
CREATE TABLE doc_reconciliation_pulsations (
  pulsation_id TEXT PRIMARY KEY REFERENCES pulsations(id) ON DELETE CASCADE,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  processed_at TEXT NOT NULL
);
