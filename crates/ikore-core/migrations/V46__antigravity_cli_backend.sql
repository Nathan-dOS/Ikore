-- Antigravity's CLI (`agy`) becomes the sixth official CLI backend.
--
-- SQLite cannot widen a CHECK in place, so every table whose backend column
-- is CHECK-constrained is rebuilt with 'antigravity' added, and
-- `terminal_model_observations` with the 'cli_init' evidence source. The definitions
-- below are the tables as V45 left them, including the columns later
-- migrations added with ALTER TABLE. `legacy_alter_table` keeps DROP+RENAME
-- from rewriting FK clauses elsewhere, matching V24, V31 and V37; the runner
-- applies migrations with foreign keys off, so dropping a parent table does
-- not cascade into its history.
PRAGMA legacy_alter_table=ON;

CREATE TABLE cli_agent_configs_v46 (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent'
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
INSERT INTO cli_agent_configs_v46("project_id","agent_role","execution_mode","backend","model","thinking_level","custom_executable","custom_args_json","custom_risk_ack","version","created_at","updated_at","model_profile","model_source")
SELECT "project_id","agent_role","execution_mode","backend","model","thinking_level","custom_executable","custom_args_json","custom_risk_ack","version","created_at","updated_at","model_profile","model_source" FROM cli_agent_configs;
DROP TABLE cli_agent_configs;
ALTER TABLE cli_agent_configs_v46 RENAME TO cli_agent_configs;
CREATE INDEX idx_cli_agent_configs_project ON cli_agent_configs(project_id);

CREATE TABLE terminal_slots_v46 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('canonical','free_form','ephemeral')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent'
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
INSERT INTO terminal_slots_v46("id","project_id","kind","agent_role","label","execution_mode","backend","model","thinking_level","session_policy","custom_executable","custom_args_json","custom_risk_ack","position","archived","version","created_at","updated_at","model_profile","model_source")
SELECT "id","project_id","kind","agent_role","label","execution_mode","backend","model","thinking_level","session_policy","custom_executable","custom_args_json","custom_risk_ack","position","archived","version","created_at","updated_at","model_profile","model_source" FROM terminal_slots;
DROP TABLE terminal_slots;
ALTER TABLE terminal_slots_v46 RENAME TO terminal_slots;
CREATE UNIQUE INDEX idx_terminal_slots_canonical_role
  ON terminal_slots(project_id,agent_role) WHERE kind='canonical';
CREATE INDEX idx_terminal_slots_project_position
  ON terminal_slots(project_id,archived,kind,position);

CREATE TABLE terminal_runs_v46 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  slot_kind TEXT NOT NULL DEFAULT 'canonical'
    CHECK(slot_kind IN ('canonical','free_form','ephemeral')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent'
  )),
  free_form_slot_id TEXT,
  free_form_label TEXT,
  origin TEXT NOT NULL CHECK(origin IN ('pipeline','learning','manual','feature')),
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
INSERT INTO terminal_runs_v46("id","project_id","slot_kind","agent_role","free_form_slot_id","free_form_label","origin","execution_mode","backend","model","native_session_id","pulsation_id","job_id","agent_run_id","llm_execution_id","status","exit_code","error_code","error_message","created_at","started_at","finished_at","updated_at","deadline_at","termination_reason","output_bytes","event_count","workspace_snapshot_hash","launch_identity_sha256","detected_cli_version","adapter_contract_version","slot_id","parent_run_id","continuation_reason","session_fingerprint","requested_model_profile","model_source","observed_model","observation_trial_id","max_transport_silence_ms")
SELECT "id","project_id","slot_kind","agent_role","free_form_slot_id","free_form_label","origin","execution_mode","backend","model","native_session_id","pulsation_id","job_id","agent_run_id","llm_execution_id","status","exit_code","error_code","error_message","created_at","started_at","finished_at","updated_at","deadline_at","termination_reason","output_bytes","event_count","workspace_snapshot_hash","launch_identity_sha256","detected_cli_version","adapter_contract_version","slot_id","parent_run_id","continuation_reason","session_fingerprint","requested_model_profile","model_source","observed_model","observation_trial_id","max_transport_silence_ms" FROM terminal_runs;
DROP TABLE terminal_runs;
ALTER TABLE terminal_runs_v46 RENAME TO terminal_runs;
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

CREATE TABLE pulse_execution_overrides_v46 (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent'
  )),
  execution_mode TEXT NOT NULL CHECK(execution_mode IN ('api_direct','cli_process')),
  -- Required when the override keeps the CLI transport, forbidden otherwise:
  -- an api_direct row has no backend to name.
  backend TEXT CHECK(backend IS NULL OR backend IN ('codex','claude','gemini','opencode','antigravity','custom')),
  source_checkpoint_id TEXT REFERENCES checkpoints(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  PRIMARY KEY(pulsation_id,agent_role),
  CHECK(
    (execution_mode='api_direct' AND backend IS NULL)
    OR
    (execution_mode='cli_process' AND backend IS NOT NULL)
  )
);
INSERT INTO pulse_execution_overrides_v46("pulsation_id","agent_role","execution_mode","backend","source_checkpoint_id","created_at")
SELECT "pulsation_id","agent_role","execution_mode","backend","source_checkpoint_id","created_at" FROM pulse_execution_overrides;
DROP TABLE pulse_execution_overrides;
ALTER TABLE pulse_execution_overrides_v46 RENAME TO pulse_execution_overrides;

CREATE TABLE cli_backend_attempts_v46 (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL,
  backend TEXT NOT NULL CHECK(backend IN ('codex','claude','gemini','opencode','antigravity','custom')),
  error_code TEXT NOT NULL,
  diagnostic TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(pulsation_id,agent_role,backend)
);
INSERT INTO cli_backend_attempts_v46("pulsation_id","agent_role","backend","error_code","diagnostic","created_at")
SELECT "pulsation_id","agent_role","backend","error_code","diagnostic","created_at" FROM cli_backend_attempts;
DROP TABLE cli_backend_attempts;
ALTER TABLE cli_backend_attempts_v46 RENAME TO cli_backend_attempts;

CREATE TABLE composition_specialty_overrides_v46 (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  executor_kind TEXT NOT NULL CHECK(executor_kind IN (
    'intake_classifier','specification_author','contract_author','code_implementer',
    'code_repairer','test_author','test_repairer','integration_reviewer')),
  execution_mode TEXT NOT NULL CHECK(execution_mode IN ('api_direct','cli_process')),
  backend TEXT CHECK(backend IS NULL OR backend IN ('codex','claude','gemini','opencode','antigravity','custom')),
  model TEXT,
  thinking_level TEXT NOT NULL DEFAULT 'default',
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, executor_kind),
  CHECK((execution_mode='api_direct' AND backend IS NULL) OR (execution_mode='cli_process' AND backend IS NOT NULL))
);
INSERT INTO composition_specialty_overrides_v46("project_id","executor_kind","execution_mode","backend","model","thinking_level","updated_at")
SELECT "project_id","executor_kind","execution_mode","backend","model","thinking_level","updated_at" FROM composition_specialty_overrides;
DROP TABLE composition_specialty_overrides;
ALTER TABLE composition_specialty_overrides_v46 RENAME TO composition_specialty_overrides;

CREATE TABLE chat_conversations_v46 (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    connection_id TEXT,
    provider_id_snapshot TEXT NOT NULL,
    model_id_snapshot TEXT NOT NULL,
    title TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    transport_kind TEXT NOT NULL DEFAULT 'api'
      CHECK(transport_kind IN ('api','cli')),
    cli_backend TEXT
      CHECK(cli_backend IS NULL OR cli_backend IN ('codex','claude','gemini','opencode','antigravity')),
    model_profile TEXT,
    model_source TEXT NOT NULL DEFAULT 'catalog'
      CHECK(model_source IN ('default','catalog','manual')),
    FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY(connection_id) REFERENCES provider_connections(id) ON DELETE SET NULL
);
INSERT INTO chat_conversations_v46("id","project_id","connection_id","provider_id_snapshot","model_id_snapshot","title","created_at","updated_at","transport_kind","cli_backend","model_profile","model_source")
SELECT "id","project_id","connection_id","provider_id_snapshot","model_id_snapshot","title","created_at","updated_at","transport_kind","cli_backend","model_profile","model_source" FROM chat_conversations;
DROP TABLE chat_conversations;
ALTER TABLE chat_conversations_v46 RENAME TO chat_conversations;
CREATE INDEX idx_chat_conversations_project_updated
    ON chat_conversations(project_id, updated_at DESC);

CREATE TABLE chat_model_calls_v46 (
    id TEXT PRIMARY KEY NOT NULL,
    assistant_message_id TEXT NOT NULL,
    round_index INTEGER NOT NULL,
    provider_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    latency_ms INTEGER,
    input_tokens INTEGER,
    output_tokens INTEGER,
    estimated_cost_usd REAL,
    status TEXT NOT NULL,
    error_code TEXT,
    transport_kind TEXT NOT NULL DEFAULT 'api'
      CHECK(transport_kind IN ('api','cli')),
    cli_backend TEXT
      CHECK(cli_backend IS NULL OR cli_backend IN ('codex','claude','gemini','opencode','antigravity')),
    model_profile TEXT,
    detected_cli_version TEXT,
    exit_code INTEGER,
    termination_reason TEXT,
    output_bytes INTEGER NOT NULL DEFAULT 0,
    redaction_count INTEGER NOT NULL DEFAULT 0,
    observed_model TEXT,
    model_verification TEXT NOT NULL DEFAULT 'unverified'
      CHECK(model_verification IN ('unverified', 'verified', 'family_verified')),
    FOREIGN KEY(assistant_message_id) REFERENCES chat_messages(id) ON DELETE CASCADE
);
INSERT INTO chat_model_calls_v46("id","assistant_message_id","round_index","provider_id","model_id","started_at","completed_at","latency_ms","input_tokens","output_tokens","estimated_cost_usd","status","error_code","transport_kind","cli_backend","model_profile","detected_cli_version","exit_code","termination_reason","output_bytes","redaction_count","observed_model","model_verification")
SELECT "id","assistant_message_id","round_index","provider_id","model_id","started_at","completed_at","latency_ms","input_tokens","output_tokens","estimated_cost_usd","status","error_code","transport_kind","cli_backend","model_profile","detected_cli_version","exit_code","termination_reason","output_bytes","redaction_count","observed_model","model_verification" FROM chat_model_calls;
DROP TABLE chat_model_calls;
ALTER TABLE chat_model_calls_v46 RENAME TO chat_model_calls;

CREATE TABLE cli_model_catalog_snapshots_v46 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  backend TEXT NOT NULL CHECK(backend IN ('codex','claude','gemini','opencode','antigravity')),
  adapter_contract_version INTEGER NOT NULL,
  state TEXT NOT NULL CHECK(state IN (
    'ready','stale','missing','auth_required','unsupported_version','failed'
  )),
  source TEXT NOT NULL CHECK(source IN ('preset','cli')),
  cli_version TEXT,
  diagnostic TEXT,
  refreshed_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(project_id,backend)
);
INSERT INTO cli_model_catalog_snapshots_v46("id","project_id","backend","adapter_contract_version","state","source","cli_version","diagnostic","refreshed_at","created_at")
SELECT "id","project_id","backend","adapter_contract_version","state","source","cli_version","diagnostic","refreshed_at","created_at" FROM cli_model_catalog_snapshots;
DROP TABLE cli_model_catalog_snapshots;
ALTER TABLE cli_model_catalog_snapshots_v46 RENAME TO cli_model_catalog_snapshots;

-- `agy` names only the model it resolved from `--model`, in its `init` event;
-- no later frame repeats it. That evidence is recorded as its own source so an
-- audit (and the ADR-025 trial) can tell it from a per-generation report.
CREATE TABLE terminal_model_observations_v46 (
  run_id TEXT NOT NULL REFERENCES terminal_runs(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK(ordinal > 0),
  event_sequence INTEGER NOT NULL CHECK(event_sequence >= 0),
  model_id TEXT,
  message_id TEXT,
  parent_invocation_id TEXT,
  agent_id TEXT,
  is_generation INTEGER NOT NULL CHECK(is_generation IN (0,1)),
  created_at TEXT NOT NULL,
  evidence_source TEXT NOT NULL DEFAULT 'stream_event'
    CHECK(evidence_source IN ('stream_event', 'codex_turn_context', 'cli_init')),
  PRIMARY KEY(run_id, ordinal),
  CHECK(model_id IS NULL OR length(model_id) BETWEEN 1 AND 256),
  CHECK(message_id IS NULL OR length(message_id) BETWEEN 1 AND 256),
  CHECK(parent_invocation_id IS NULL OR length(parent_invocation_id) BETWEEN 1 AND 256),
  CHECK(agent_id IS NULL OR length(agent_id) BETWEEN 1 AND 256)
);
INSERT INTO terminal_model_observations_v46("run_id","ordinal","event_sequence","model_id","message_id","parent_invocation_id","agent_id","is_generation","created_at","evidence_source")
SELECT "run_id","ordinal","event_sequence","model_id","message_id","parent_invocation_id","agent_id","is_generation","created_at","evidence_source" FROM terminal_model_observations;
DROP TABLE terminal_model_observations;
ALTER TABLE terminal_model_observations_v46 RENAME TO terminal_model_observations;
CREATE INDEX terminal_model_observations_generation
  ON terminal_model_observations(run_id, is_generation, ordinal);
