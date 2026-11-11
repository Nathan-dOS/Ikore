-- ADR-029: Companion Notes and the Notes Scribe, the agent that turns a
-- developer's loose capture into a structured note.
--
-- The Scribe is a canonical role routed like any other (API or CLI), so the
-- tables whose role column is CHECK-constrained for configuration, slots and
-- runs gain 'notes_scribe', and terminal runs gain the 'notes' origin. The
-- definitions are the tables as V47 left them (V48 and V49 did not touch them).
-- `legacy_alter_table` keeps DROP+RENAME from rewriting FK clauses elsewhere,
-- as in V46/V47; the runner applies migrations with foreign keys off.
PRAGMA legacy_alter_table=ON;

CREATE TABLE cli_agent_configs_v50 (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent','visualizer','notes_scribe'
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
INSERT INTO cli_agent_configs_v50("project_id","agent_role","execution_mode","backend","model","thinking_level","custom_executable","custom_args_json","custom_risk_ack","version","created_at","updated_at","model_profile","model_source")
SELECT "project_id","agent_role","execution_mode","backend","model","thinking_level","custom_executable","custom_args_json","custom_risk_ack","version","created_at","updated_at","model_profile","model_source" FROM cli_agent_configs;
DROP TABLE cli_agent_configs;
ALTER TABLE cli_agent_configs_v50 RENAME TO cli_agent_configs;
CREATE INDEX idx_cli_agent_configs_project ON cli_agent_configs(project_id);

CREATE TABLE terminal_slots_v50 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('canonical','free_form','ephemeral')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent','visualizer','notes_scribe'
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
INSERT INTO terminal_slots_v50("id","project_id","kind","agent_role","label","execution_mode","backend","model","thinking_level","session_policy","custom_executable","custom_args_json","custom_risk_ack","position","archived","version","created_at","updated_at","model_profile","model_source")
SELECT "id","project_id","kind","agent_role","label","execution_mode","backend","model","thinking_level","session_policy","custom_executable","custom_args_json","custom_risk_ack","position","archived","version","created_at","updated_at","model_profile","model_source" FROM terminal_slots;
DROP TABLE terminal_slots;
ALTER TABLE terminal_slots_v50 RENAME TO terminal_slots;
CREATE UNIQUE INDEX idx_terminal_slots_canonical_role
  ON terminal_slots(project_id,agent_role) WHERE kind='canonical';
CREATE INDEX idx_terminal_slots_project_position
  ON terminal_slots(project_id,archived,kind,position);

CREATE TABLE terminal_runs_v50 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  slot_kind TEXT NOT NULL DEFAULT 'canonical'
    CHECK(slot_kind IN ('canonical','free_form','ephemeral')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent','visualizer','notes_scribe'
  )),
  free_form_slot_id TEXT,
  free_form_label TEXT,
  origin TEXT NOT NULL CHECK(origin IN ('pipeline','learning','manual','feature','review','notes')),
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
INSERT INTO terminal_runs_v50("id","project_id","slot_kind","agent_role","free_form_slot_id","free_form_label","origin","execution_mode","backend","model","native_session_id","pulsation_id","job_id","agent_run_id","llm_execution_id","status","exit_code","error_code","error_message","created_at","started_at","finished_at","updated_at","deadline_at","termination_reason","output_bytes","event_count","workspace_snapshot_hash","launch_identity_sha256","detected_cli_version","adapter_contract_version","slot_id","parent_run_id","continuation_reason","session_fingerprint","requested_model_profile","model_source","observed_model","observation_trial_id","max_transport_silence_ms")
SELECT "id","project_id","slot_kind","agent_role","free_form_slot_id","free_form_label","origin","execution_mode","backend","model","native_session_id","pulsation_id","job_id","agent_run_id","llm_execution_id","status","exit_code","error_code","error_message","created_at","started_at","finished_at","updated_at","deadline_at","termination_reason","output_bytes","event_count","workspace_snapshot_hash","launch_identity_sha256","detected_cli_version","adapter_contract_version","slot_id","parent_run_id","continuation_reason","session_fingerprint","requested_model_profile","model_source","observed_model","observation_trial_id","max_transport_silence_ms" FROM terminal_runs;
DROP TABLE terminal_runs;
ALTER TABLE terminal_runs_v50 RENAME TO terminal_runs;
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

-- One row per capture. It is written before any model call, so a capture
-- survives a failed or cancelled Scribe turn (`failed`, with its raw text).
-- `raw_input` is the developer's text after secret redaction. The text
-- fields are the Scribe's, in the developer's language (ADR-029 decision 5).
CREATE TABLE companion_notes (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  capture_request_id TEXT NOT NULL
    CHECK(length(capture_request_id) BETWEEN 1 AND 200),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision > 0),
  status TEXT NOT NULL DEFAULT 'structuring'
    CHECK(status IN ('structuring','ready','failed')),
  source_type TEXT NOT NULL DEFAULT 'text' CHECK(source_type IN ('text','voice')),
  raw_input TEXT NOT NULL CHECK(length(raw_input) BETWEEN 1 AND 8000),
  raw_redactions INTEGER NOT NULL DEFAULT 0 CHECK(raw_redactions >= 0),
  title TEXT CHECK(title IS NULL OR length(title) BETWEEN 1 AND 80),
  summary TEXT CHECK(summary IS NULL OR length(summary) BETWEEN 1 AND 280),
  content_markdown TEXT
    CHECK(content_markdown IS NULL OR length(content_markdown) BETWEEN 1 AND 4000),
  tags_json TEXT NOT NULL DEFAULT '[]'
    CHECK(json_valid(tags_json) AND json_type(tags_json) = 'array'),
  output_language TEXT
    CHECK(output_language IS NULL OR length(output_language) BETWEEN 2 AND 12),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(project_id, capture_request_id),
  CHECK(status <> 'ready' OR (title IS NOT NULL AND summary IS NOT NULL
        AND content_markdown IS NOT NULL AND output_language IS NOT NULL))
);
CREATE INDEX companion_notes_project_created
  ON companion_notes(project_id, created_at DESC, id);

-- One row per Scribe turn on a note: the first structuring, a retry of it,
-- or a refinement the developer asked for. `calls` counts the answer and the
-- single corrective call a rejected answer may get; `notices_json` holds what
-- the host accepted with a reservation (an answer in an unexpected language).
CREATE TABLE companion_note_turns (
  id TEXT PRIMARY KEY,
  note_id TEXT NOT NULL REFERENCES companion_notes(id) ON DELETE CASCADE,
  note_revision INTEGER NOT NULL CHECK(note_revision > 0),
  ordinal INTEGER NOT NULL CHECK(ordinal > 0),
  request_id TEXT NOT NULL CHECK(length(request_id) BETWEEN 1 AND 200),
  kind TEXT NOT NULL CHECK(kind IN ('structure','refine')),
  status TEXT NOT NULL
    CHECK(status IN ('running','completed','failed','cancelled','interrupted')),
  user_message TEXT
    CHECK(user_message IS NULL OR length(user_message) BETWEEN 1 AND 2000),
  user_redactions INTEGER NOT NULL DEFAULT 0 CHECK(user_redactions >= 0),
  assistant_message TEXT,
  response_json TEXT CHECK(response_json IS NULL OR json_valid(response_json)),
  failure_json TEXT CHECK(failure_json IS NULL OR json_valid(failure_json)),
  notices_json TEXT CHECK(notices_json IS NULL OR json_valid(notices_json)),
  calls INTEGER NOT NULL DEFAULT 0 CHECK(calls BETWEEN 0 AND 2),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(note_id, request_id),
  UNIQUE(note_id, ordinal),
  CHECK((kind = 'refine') = (user_message IS NOT NULL)),
  CHECK(status <> 'completed' OR (response_json IS NOT NULL AND assistant_message IS NOT NULL))
);
CREATE UNIQUE INDEX companion_note_one_running_turn
  ON companion_note_turns(note_id) WHERE status = 'running';
