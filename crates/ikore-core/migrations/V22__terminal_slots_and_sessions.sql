CREATE TABLE terminal_slots (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('canonical','free_form')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent'
  )),
  label TEXT NOT NULL,
  execution_mode TEXT NOT NULL DEFAULT 'api_direct'
    CHECK(execution_mode IN ('api_direct','cli_process')),
  backend TEXT NOT NULL DEFAULT 'codex'
    CHECK(backend IN ('codex','claude','gemini','opencode','custom')),
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
  CHECK(
    (kind='canonical' AND agent_role IS NOT NULL)
    OR (kind='free_form' AND agent_role IS NULL)
  )
);

CREATE UNIQUE INDEX idx_terminal_slots_canonical_role
  ON terminal_slots(project_id,agent_role)
  WHERE kind='canonical';
CREATE INDEX idx_terminal_slots_project_position
  ON terminal_slots(project_id,archived,kind,position);

INSERT INTO terminal_slots(
  id,project_id,kind,agent_role,label,execution_mode,backend,model,thinking_level,
  session_policy,custom_executable,custom_args_json,custom_risk_ack,position,
  archived,version,created_at,updated_at
)
SELECT
  'canonical:' || project_id || ':' || agent_role,
  project_id,
  'canonical',
  agent_role,
  CASE agent_role
    WHEN 'intake_router' THEN 'IntakeRouter'
    WHEN 'task_specifier' THEN 'TaskSpecifier'
    WHEN 'contract_agent' THEN 'ContractAgent'
    WHEN 'backend_agent' THEN 'BackendAgent'
    WHEN 'test_agent' THEN 'TestAgent'
    WHEN 'integrator' THEN 'Integrator'
    WHEN 'learning_agent' THEN 'LearningAgent'
  END,
  execution_mode,
  backend,
  model,
  thinking_level,
  'fresh_per_run',
  custom_executable,
  custom_args_json,
  custom_risk_ack,
  CASE agent_role
    WHEN 'intake_router' THEN 0
    WHEN 'task_specifier' THEN 1
    WHEN 'contract_agent' THEN 2
    WHEN 'backend_agent' THEN 3
    WHEN 'test_agent' THEN 4
    WHEN 'integrator' THEN 5
    WHEN 'learning_agent' THEN 6
  END,
  0,
  version,
  created_at,
  updated_at
FROM cli_agent_configs;

ALTER TABLE terminal_runs ADD COLUMN slot_id TEXT
  REFERENCES terminal_slots(id) ON DELETE SET NULL;
ALTER TABLE terminal_runs ADD COLUMN parent_run_id TEXT
  REFERENCES terminal_runs(id) ON DELETE SET NULL;
ALTER TABLE terminal_runs ADD COLUMN continuation_reason TEXT
  CHECK(continuation_reason IS NULL OR continuation_reason IN (
    'explicit_resume','resume_per_pulsation','restart_recovery'
  ));
ALTER TABLE terminal_runs ADD COLUMN session_fingerprint TEXT;

UPDATE terminal_runs
SET slot_id='canonical:' || project_id || ':' || agent_role
WHERE slot_kind='canonical' AND agent_role IS NOT NULL;

CREATE INDEX idx_terminal_runs_slot_created
  ON terminal_runs(project_id,slot_id,created_at DESC);
CREATE INDEX idx_terminal_runs_parent ON terminal_runs(parent_run_id);
CREATE UNIQUE INDEX idx_terminal_runs_one_active_per_slot
  ON terminal_runs(project_id,slot_id)
  WHERE slot_id IS NOT NULL AND status IN ('queued','starting','running');
