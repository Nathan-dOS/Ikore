-- Feature 25, M2a: the developer's own pipelines and the automation lane.
--
-- Their steps (and, from M4 on, the verifiers) run as CLI runs of their own,
-- on a slot without a role: terminal slots gain the kind 'automation' and
-- terminal runs the origins 'automation' and 'verification' (the second is
-- used from M4 on, so that M4 needs no second rebuild). The definitions are
-- the tables as V54 left them (V55 to V69 did not touch them). An API step
-- runs through the router, so `llm_route_executions` gains the automation
-- slot as a third kind of target.
-- `legacy_alter_table` keeps DROP+RENAME from rewriting FK clauses elsewhere;
-- the runner applies migrations with foreign keys off. Everything after the
-- rebuild is additive.
PRAGMA legacy_alter_table=ON;

CREATE TABLE terminal_slots_v70 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('canonical','free_form','ephemeral','automation')),
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
    OR (kind IN ('free_form','automation') AND agent_role IS NULL)
  )
);
INSERT INTO terminal_slots_v70("id","project_id","kind","agent_role","label","execution_mode","backend","model","thinking_level","session_policy","custom_executable","custom_args_json","custom_risk_ack","position","archived","version","created_at","updated_at","model_profile","model_source")
SELECT "id","project_id","kind","agent_role","label","execution_mode","backend","model","thinking_level","session_policy","custom_executable","custom_args_json","custom_risk_ack","position","archived","version","created_at","updated_at","model_profile","model_source" FROM terminal_slots;
DROP TABLE terminal_slots;
ALTER TABLE terminal_slots_v70 RENAME TO terminal_slots;
CREATE UNIQUE INDEX idx_terminal_slots_canonical_role
  ON terminal_slots(project_id,agent_role) WHERE kind='canonical';
CREATE INDEX idx_terminal_slots_project_position
  ON terminal_slots(project_id,archived,kind,position);

CREATE TABLE terminal_runs_v70 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  slot_kind TEXT NOT NULL DEFAULT 'canonical'
    CHECK(slot_kind IN ('canonical','free_form','ephemeral','automation')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent','visualizer','notes_scribe',
    'documentation_reconciler'
  )),
  free_form_slot_id TEXT,
  free_form_label TEXT,
  origin TEXT NOT NULL CHECK(origin IN ('pipeline','learning','manual','feature','review','notes','reconciliation','automation','verification')),
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
    OR (slot_kind='automation' AND agent_role IS NULL AND free_form_slot_id IS NULL)
  )
);
INSERT INTO terminal_runs_v70("id","project_id","slot_kind","agent_role","free_form_slot_id","free_form_label","origin","execution_mode","backend","model","native_session_id","pulsation_id","job_id","agent_run_id","llm_execution_id","status","exit_code","error_code","error_message","created_at","started_at","finished_at","updated_at","deadline_at","termination_reason","output_bytes","event_count","workspace_snapshot_hash","launch_identity_sha256","detected_cli_version","adapter_contract_version","slot_id","parent_run_id","continuation_reason","session_fingerprint","requested_model_profile","model_source","observed_model","observation_trial_id","max_transport_silence_ms")
SELECT "id","project_id","slot_kind","agent_role","free_form_slot_id","free_form_label","origin","execution_mode","backend","model","native_session_id","pulsation_id","job_id","agent_run_id","llm_execution_id","status","exit_code","error_code","error_message","created_at","started_at","finished_at","updated_at","deadline_at","termination_reason","output_bytes","event_count","workspace_snapshot_hash","launch_identity_sha256","detected_cli_version","adapter_contract_version","slot_id","parent_run_id","continuation_reason","session_fingerprint","requested_model_profile","model_source","observed_model","observation_trial_id","max_transport_silence_ms" FROM terminal_runs;
DROP TABLE terminal_runs;
ALTER TABLE terminal_runs_v70 RENAME TO terminal_runs;
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

-- An API step runs through the router like any agent, and its execution row
-- names its automation slot: the target is a role, a free-form slot or an
-- automation slot, exactly one. Columns as V34 and V49 left them.
CREATE TABLE llm_route_executions_v70 (
  id TEXT PRIMARY KEY,
  generation_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  pulsation_id TEXT,
  job_id TEXT,
  route_id TEXT NOT NULL,
  agent_role TEXT,
  free_form_slot_id TEXT,
  stage TEXT NOT NULL,
  strategy TEXT NOT NULL,
  mode TEXT NOT NULL,
  status TEXT NOT NULL,
  decision_json TEXT NOT NULL,
  next_eligible_at TEXT,
  visible_output TEXT,
  visible_output_complete INTEGER NOT NULL DEFAULT 0,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  output_redaction_count INTEGER NOT NULL DEFAULT 0 CHECK(output_redaction_count >= 0),
  output_redaction_categories_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(output_redaction_categories_json)),
  automation_slot_id TEXT,
  FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE SET NULL,
  FOREIGN KEY(job_id) REFERENCES jobs(id) ON DELETE SET NULL,
  FOREIGN KEY(route_id) REFERENCES provider_routes(id) ON DELETE RESTRICT,
  CHECK(
    (agent_role IS NOT NULL) + (free_form_slot_id IS NOT NULL) + (automation_slot_id IS NOT NULL) = 1
  )
);

INSERT INTO llm_route_executions_v70(
  id,generation_id,project_id,pulsation_id,job_id,route_id,agent_role,
  free_form_slot_id,stage,strategy,mode,status,decision_json,next_eligible_at,
  visible_output,visible_output_complete,started_at,finished_at,
  output_redaction_count,output_redaction_categories_json,automation_slot_id
)
SELECT
  id,generation_id,project_id,pulsation_id,job_id,route_id,agent_role,
  free_form_slot_id,stage,strategy,mode,status,decision_json,next_eligible_at,
  visible_output,visible_output_complete,started_at,finished_at,
  output_redaction_count,output_redaction_categories_json,NULL
FROM llm_route_executions;

DROP TABLE llm_route_executions;

ALTER TABLE llm_route_executions_v70 RENAME TO llm_route_executions;

CREATE INDEX idx_llm_executions_project_started
  ON llm_route_executions(project_id, started_at DESC);

CREATE INDEX idx_llm_executions_project_role_started
  ON llm_route_executions(project_id, agent_role, started_at DESC)
  WHERE agent_role IS NOT NULL;

CREATE INDEX idx_llm_executions_project_free_form_started
  ON llm_route_executions(project_id, free_form_slot_id, started_at DESC)
  WHERE free_form_slot_id IS NOT NULL;

CREATE INDEX idx_llm_executions_project_automation_started
  ON llm_route_executions(project_id, automation_slot_id, started_at DESC)
  WHERE automation_slot_id IS NOT NULL;

PRAGMA legacy_alter_table=OFF;

-- A pipeline of the developer's. `scope` is 'project' until M2b brings the
-- global ones, which belong to no project.
CREATE TABLE custom_pipelines (
  id TEXT PRIMARY KEY,
  scope TEXT NOT NULL DEFAULT 'project' CHECK(scope IN ('project','global')),
  project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 60),
  description TEXT NOT NULL DEFAULT '' CHECK(length(description) <= 500),
  current_version INTEGER NOT NULL CHECK(current_version > 0),
  archived INTEGER NOT NULL DEFAULT 0 CHECK(archived IN (0,1)),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK((scope = 'global') = (project_id IS NULL))
);
CREATE INDEX idx_custom_pipelines_project ON custom_pipelines(project_id, archived);

-- Every edit is a new version; a run keeps the one it started with.
CREATE TABLE custom_pipeline_versions (
  pipeline_id TEXT NOT NULL REFERENCES custom_pipelines(id) ON DELETE CASCADE,
  version INTEGER NOT NULL CHECK(version > 0),
  definition_json TEXT NOT NULL CHECK(json_valid(definition_json)),
  definition_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(pipeline_id, version)
);

-- A pipeline's box in a project: its place on the canvas and the version the
-- project uses. A project's own pipelines have one too (T3).
CREATE TABLE custom_pipeline_activations (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  pipeline_id TEXT NOT NULL REFERENCES custom_pipelines(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK(position >= 0),
  adopted_version INTEGER NOT NULL CHECK(adopted_version > 0),
  state TEXT NOT NULL DEFAULT 'ready' CHECK(state IN ('ready','needs_edit')),
  needs_edit_reason TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, pipeline_id)
);

CREATE TABLE custom_pipeline_runs (
  id TEXT PRIMARY KEY,
  pipeline_id TEXT NOT NULL REFERENCES custom_pipelines(id) ON DELETE CASCADE,
  version INTEGER NOT NULL,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  trigger_kind TEXT NOT NULL CHECK(trigger_kind IN ('manual','default_event')),
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
  FOREIGN KEY(pipeline_id, version) REFERENCES custom_pipeline_versions(pipeline_id, version)
);
CREATE INDEX idx_custom_pipeline_runs_project
  ON custom_pipeline_runs(project_id, status, created_at);
CREATE INDEX idx_custom_pipeline_runs_pipeline
  ON custom_pipeline_runs(pipeline_id, created_at DESC);

CREATE TABLE custom_pipeline_step_runs (
  run_id TEXT NOT NULL REFERENCES custom_pipeline_runs(id) ON DELETE CASCADE,
  step_index INTEGER NOT NULL CHECK(step_index >= 0),
  attempt INTEGER NOT NULL DEFAULT 1 CHECK(attempt > 0),
  step_id TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('agent','gate','host_tool')),
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
  PRIMARY KEY(run_id, step_index, attempt)
);

-- The automation lane's places. Apart from runtime_slots on purpose: the
-- main lane's admission does not change.
CREATE TABLE automation_slots (
  token TEXT PRIMARY KEY,
  class TEXT NOT NULL CHECK(class IN ('verification','custom_step')),
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  owner_id TEXT NOT NULL,
  acquired_at TEXT NOT NULL
);
