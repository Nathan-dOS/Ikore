-- `kind` has a legacy CHECK constraint and is referenced by more than one table.
-- Keep the upgrade additive so existing checkpoint foreign keys are never rebuilt.
ALTER TABLE checkpoints ADD COLUMN purpose TEXT NOT NULL DEFAULT 'artifact_review'
  CHECK(purpose IN ('artifact_review','clarification','cli_fallback'));
UPDATE checkpoints SET purpose=kind;
CREATE INDEX idx_checkpoints_purpose ON checkpoints(pulsation_id,purpose,status);

CREATE TABLE cli_agent_configs (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent'
  )),
  execution_mode TEXT NOT NULL DEFAULT 'api_direct'
    CHECK(execution_mode IN ('api_direct','cli_process')),
  backend TEXT NOT NULL DEFAULT 'codex'
    CHECK(backend IN ('codex','claude','gemini','opencode','custom')),
  model TEXT,
  thinking_level TEXT NOT NULL DEFAULT 'default'
    CHECK(thinking_level IN ('default','low','medium','high')),
  custom_executable TEXT,
  custom_args_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(custom_args_json)),
  custom_risk_ack INTEGER NOT NULL DEFAULT 0 CHECK(custom_risk_ack IN (0,1)),
  version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id,agent_role)
);

CREATE TABLE terminal_runs (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  slot_kind TEXT NOT NULL DEFAULT 'canonical'
    CHECK(slot_kind IN ('canonical','free_form')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent'
  )),
  free_form_slot_id TEXT,
  free_form_label TEXT,
  origin TEXT NOT NULL CHECK(origin IN ('pipeline','learning','manual')),
  execution_mode TEXT NOT NULL CHECK(execution_mode IN ('api_direct','cli_process')),
  backend TEXT CHECK(backend IS NULL OR backend IN ('codex','claude','gemini','opencode','custom')),
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
  CHECK(
    (slot_kind='canonical' AND agent_role IS NOT NULL AND free_form_slot_id IS NULL)
    OR
    (slot_kind='free_form' AND agent_role IS NULL AND free_form_slot_id IS NOT NULL)
  )
);

CREATE TABLE terminal_events (
  id TEXT PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES terminal_runs(id) ON DELETE CASCADE,
  sequence INTEGER NOT NULL CHECK(sequence > 0),
  kind TEXT NOT NULL CHECK(kind IN (
    'system','message','tool_use','tool_result','stdout','stderr','warning',
    'error','completed'
  )),
  stream TEXT CHECK(stream IS NULL OR stream IN ('stdout','stderr')),
  content TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(run_id,sequence)
);

CREATE TABLE cli_fallback_requests (
  checkpoint_id TEXT PRIMARY KEY REFERENCES checkpoints(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent'
  )),
  terminal_run_id TEXT REFERENCES terminal_runs(id) ON DELETE SET NULL,
  error_code TEXT NOT NULL,
  diagnostic TEXT NOT NULL,
  decision TEXT CHECK(decision IS NULL OR decision IN ('use_api','retry_cli','cancel')),
  resolved_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE pulse_execution_overrides (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent'
  )),
  execution_mode TEXT NOT NULL CHECK(execution_mode='api_direct'),
  source_checkpoint_id TEXT NOT NULL REFERENCES checkpoints(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  PRIMARY KEY(pulsation_id,agent_role)
);

CREATE INDEX idx_cli_agent_configs_project ON cli_agent_configs(project_id);
CREATE INDEX idx_terminal_runs_project_role_created
  ON terminal_runs(project_id,agent_role,created_at DESC);
CREATE INDEX idx_terminal_runs_pulsation ON terminal_runs(pulsation_id,created_at DESC);
CREATE INDEX idx_terminal_events_run_sequence ON terminal_events(run_id,sequence);
CREATE INDEX idx_cli_fallback_pulsation ON cli_fallback_requests(pulsation_id,created_at DESC);
