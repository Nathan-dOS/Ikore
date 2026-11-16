-- The migration host disables foreign keys and verifies them after migration.
PRAGMA legacy_alter_table=ON;

CREATE TABLE pulsations_v24 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  feature_name TEXT NOT NULL,
  stage TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL, queue_sequence INTEGER, input_revision INTEGER NOT NULL DEFAULT 1 CHECK(input_revision > 0), state_version INTEGER NOT NULL DEFAULT 1 CHECK(state_version > 0), pipeline_version INTEGER NOT NULL DEFAULT 1 CHECK(pipeline_version IN (1,2,3)), terminal_reason TEXT, revision_feedback TEXT, coordination_hold INTEGER NOT NULL DEFAULT 0 CHECK(coordination_hold IN (0,1)), integration_checkpoint_id TEXT REFERENCES checkpoints(id), integration_state_version INTEGER, specification_started INTEGER NOT NULL DEFAULT 0 CHECK(specification_started IN (0,1)),
  FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
);

INSERT INTO pulsations_v24("id","project_id","feature_name","stage","status","created_at","updated_at","queue_sequence","input_revision","state_version","pipeline_version","terminal_reason","revision_feedback","coordination_hold","integration_checkpoint_id","integration_state_version","specification_started") SELECT "id","project_id","feature_name","stage","status","created_at","updated_at","queue_sequence","input_revision","state_version","pipeline_version","terminal_reason","revision_feedback","coordination_hold","integration_checkpoint_id","integration_state_version","specification_started" FROM pulsations;

DROP TABLE pulsations;

ALTER TABLE pulsations_v24 RENAME TO pulsations;

CREATE INDEX idx_pulsations_project_id
  ON pulsations(project_id);

CREATE UNIQUE INDEX idx_pulse_sequence ON pulsations(project_id, queue_sequence);

CREATE TRIGGER pulse_sequence_immutable BEFORE UPDATE OF queue_sequence ON pulsations
WHEN OLD.queue_sequence IS NOT NULL AND NEW.queue_sequence IS NOT OLD.queue_sequence
BEGIN SELECT RAISE(ABORT, 'queue_sequence is immutable'); END;

CREATE TABLE cli_agent_configs_v24 (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent'
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
  updated_at TEXT NOT NULL, model_profile TEXT, model_source TEXT NOT NULL DEFAULT 'default'
  CHECK(model_source IN ('default','catalog','manual')),
  PRIMARY KEY(project_id,agent_role)
);

INSERT INTO cli_agent_configs_v24("project_id","agent_role","execution_mode","backend","model","thinking_level","custom_executable","custom_args_json","custom_risk_ack","version","created_at","updated_at","model_profile","model_source") SELECT "project_id","agent_role","execution_mode","backend","model","thinking_level","custom_executable","custom_args_json","custom_risk_ack","version","created_at","updated_at","model_profile","model_source" FROM cli_agent_configs;

DROP TABLE cli_agent_configs;

ALTER TABLE cli_agent_configs_v24 RENAME TO cli_agent_configs;

CREATE INDEX idx_cli_agent_configs_project ON cli_agent_configs(project_id);

CREATE TABLE terminal_slots_v24 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('canonical','free_form')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent'
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
  updated_at TEXT NOT NULL, model_profile TEXT, model_source TEXT NOT NULL DEFAULT 'default'
  CHECK(model_source IN ('default','catalog','manual')),
  CHECK(
    (kind='canonical' AND agent_role IS NOT NULL)
    OR (kind='free_form' AND agent_role IS NULL)
  )
);

INSERT INTO terminal_slots_v24("id","project_id","kind","agent_role","label","execution_mode","backend","model","thinking_level","session_policy","custom_executable","custom_args_json","custom_risk_ack","position","archived","version","created_at","updated_at","model_profile","model_source") SELECT "id","project_id","kind","agent_role","label","execution_mode","backend","model","thinking_level","session_policy","custom_executable","custom_args_json","custom_risk_ack","position","archived","version","created_at","updated_at","model_profile","model_source" FROM terminal_slots;

DROP TABLE terminal_slots;

ALTER TABLE terminal_slots_v24 RENAME TO terminal_slots;

CREATE UNIQUE INDEX idx_terminal_slots_canonical_role
  ON terminal_slots(project_id,agent_role)
  WHERE kind='canonical';

CREATE INDEX idx_terminal_slots_project_position
  ON terminal_slots(project_id,archived,kind,position);

CREATE TABLE terminal_runs_v24 (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  slot_kind TEXT NOT NULL DEFAULT 'canonical'
    CHECK(slot_kind IN ('canonical','free_form')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent'
  )),
  free_form_slot_id TEXT,
  free_form_label TEXT,
  origin TEXT NOT NULL CHECK(origin IN ('pipeline','learning','manual','feature')),
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
  updated_at TEXT NOT NULL, deadline_at TEXT, termination_reason TEXT
  CHECK(termination_reason IS NULL OR termination_reason IN (
    'exited','user_cancelled','pipeline_cancelled','shutdown','timeout',
    'output_limit','spawn_failed','io_failed','persistence_failed','interrupted'
  )), output_bytes INTEGER NOT NULL DEFAULT 0
  CHECK(output_bytes >= 0), event_count INTEGER NOT NULL DEFAULT 0
  CHECK(event_count >= 0), workspace_snapshot_hash TEXT, launch_identity_sha256 TEXT, detected_cli_version TEXT, adapter_contract_version INTEGER NOT NULL DEFAULT 1
  CHECK(adapter_contract_version > 0), slot_id TEXT
  REFERENCES terminal_slots(id) ON DELETE SET NULL, parent_run_id TEXT
  REFERENCES terminal_runs(id) ON DELETE SET NULL, continuation_reason TEXT
  CHECK(continuation_reason IS NULL OR continuation_reason IN (
    'explicit_resume','resume_per_pulsation','restart_recovery'
  )), session_fingerprint TEXT, requested_model_profile TEXT, model_source TEXT NOT NULL DEFAULT 'default'
  CHECK(model_source IN ('default','catalog','manual')), observed_model TEXT,
  CHECK(
    (slot_kind='canonical' AND agent_role IS NOT NULL AND free_form_slot_id IS NULL)
    OR
    (slot_kind='free_form' AND agent_role IS NULL AND free_form_slot_id IS NOT NULL)
  )
);

INSERT INTO terminal_runs_v24("id","project_id","slot_kind","agent_role","free_form_slot_id","free_form_label","origin","execution_mode","backend","model","native_session_id","pulsation_id","job_id","agent_run_id","llm_execution_id","status","exit_code","error_code","error_message","created_at","started_at","finished_at","updated_at","deadline_at","termination_reason","output_bytes","event_count","workspace_snapshot_hash","launch_identity_sha256","detected_cli_version","adapter_contract_version","slot_id","parent_run_id","continuation_reason","session_fingerprint","requested_model_profile","model_source","observed_model") SELECT "id","project_id","slot_kind","agent_role","free_form_slot_id","free_form_label","origin","execution_mode","backend","model","native_session_id","pulsation_id","job_id","agent_run_id","llm_execution_id","status","exit_code","error_code","error_message","created_at","started_at","finished_at","updated_at","deadline_at","termination_reason","output_bytes","event_count","workspace_snapshot_hash","launch_identity_sha256","detected_cli_version","adapter_contract_version","slot_id","parent_run_id","continuation_reason","session_fingerprint","requested_model_profile","model_source","observed_model" FROM terminal_runs;

DROP TABLE terminal_runs;

ALTER TABLE terminal_runs_v24 RENAME TO terminal_runs;

CREATE INDEX idx_terminal_runs_project_role_created
  ON terminal_runs(project_id,agent_role,created_at DESC);

CREATE INDEX idx_terminal_runs_pulsation ON terminal_runs(pulsation_id,created_at DESC);

CREATE INDEX idx_terminal_runs_active
  ON terminal_runs(project_id,status,updated_at DESC)
  WHERE status IN ('queued','starting','running');

CREATE INDEX idx_terminal_runs_slot_created
  ON terminal_runs(project_id,slot_id,created_at DESC);

CREATE INDEX idx_terminal_runs_parent ON terminal_runs(parent_run_id);

CREATE UNIQUE INDEX idx_terminal_runs_one_active_per_slot
  ON terminal_runs(project_id,slot_id)
  WHERE slot_id IS NOT NULL AND status IN ('queued','starting','running');

CREATE TABLE cli_fallback_requests_v24 (
  checkpoint_id TEXT PRIMARY KEY REFERENCES checkpoints(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent'
  )),
  terminal_run_id TEXT REFERENCES terminal_runs(id) ON DELETE SET NULL,
  error_code TEXT NOT NULL,
  diagnostic TEXT NOT NULL,
  decision TEXT CHECK(decision IS NULL OR decision IN ('use_api','retry_cli','cancel')),
  resolved_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

INSERT INTO cli_fallback_requests_v24("checkpoint_id","pulsation_id","agent_role","terminal_run_id","error_code","diagnostic","decision","resolved_at","created_at","updated_at") SELECT "checkpoint_id","pulsation_id","agent_role","terminal_run_id","error_code","diagnostic","decision","resolved_at","created_at","updated_at" FROM cli_fallback_requests;

DROP TABLE cli_fallback_requests;

ALTER TABLE cli_fallback_requests_v24 RENAME TO cli_fallback_requests;

CREATE INDEX idx_cli_fallback_pulsation ON cli_fallback_requests(pulsation_id,created_at DESC);

CREATE TABLE pulse_execution_overrides_v24 (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent'
  )),
  execution_mode TEXT NOT NULL CHECK(execution_mode='api_direct'),
  source_checkpoint_id TEXT NOT NULL REFERENCES checkpoints(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  PRIMARY KEY(pulsation_id,agent_role)
);

INSERT INTO pulse_execution_overrides_v24("pulsation_id","agent_role","execution_mode","source_checkpoint_id","created_at") SELECT "pulsation_id","agent_role","execution_mode","source_checkpoint_id","created_at" FROM pulse_execution_overrides;

DROP TABLE pulse_execution_overrides;

ALTER TABLE pulse_execution_overrides_v24 RENAME TO pulse_execution_overrides;

PRAGMA legacy_alter_table=OFF;

CREATE TABLE forge_features (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 title TEXT NOT NULL, original_idea TEXT NOT NULL,
 revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0), approved_revision INTEGER,
 state TEXT NOT NULL DEFAULT 'incubating' CHECK(state IN ('incubating','in_triage','approved','planned','executing','ready_for_review','done','archived')),
 brief_json TEXT NOT NULL CHECK(json_valid(brief_json)), evaluation_json TEXT NOT NULL CHECK(json_valid(evaluation_json)),
 structural_level INTEGER NOT NULL DEFAULT 0 CHECK(structural_level BETWEEN 0 AND 3),
 wave TEXT CHECK(wave IN ('current','next','horizon')), position INTEGER NOT NULL DEFAULT 0,
 archive_reason TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE INDEX forge_features_project ON forge_features(project_id,created_at,id);
CREATE TABLE forge_feature_revisions (
 feature_id TEXT NOT NULL REFERENCES forge_features(id) ON DELETE CASCADE, revision INTEGER NOT NULL,
 brief_json TEXT NOT NULL CHECK(json_valid(brief_json)), approved_at TEXT,
 PRIMARY KEY(feature_id,revision)
);
CREATE TABLE forge_feature_dependencies (
 feature_id TEXT NOT NULL REFERENCES forge_features(id) ON DELETE CASCADE,
 prerequisite_id TEXT NOT NULL REFERENCES forge_features(id) ON DELETE RESTRICT,
 rationale TEXT NOT NULL, PRIMARY KEY(feature_id,prerequisite_id), CHECK(feature_id<>prerequisite_id)
);
CREATE TABLE forge_project_settings (
 project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
 current_wave_limit INTEGER NOT NULL DEFAULT 3 CHECK(current_wave_limit>0)
);
CREATE TABLE work_tasks (
 id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0), draft_json TEXT NOT NULL CHECK(json_valid(draft_json)),
 feature_id TEXT REFERENCES forge_features(id) ON DELETE RESTRICT, feature_revision INTEGER,
 state TEXT NOT NULL DEFAULT 'ready' CHECK(state IN ('ready','queued','done','superseded')),
 active_pulsation_id TEXT REFERENCES pulsations(id) ON DELETE RESTRICT,
 completed_pulsation_id TEXT REFERENCES pulsations(id) ON DELETE RESTRICT,
 created_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE INDEX work_tasks_project ON work_tasks(project_id,created_at,id);
CREATE TABLE work_task_revisions (
 task_id TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE CASCADE, revision INTEGER NOT NULL,
 draft_json TEXT NOT NULL CHECK(json_valid(draft_json)), PRIMARY KEY(task_id,revision)
);
CREATE TABLE work_task_dependencies (
 task_id TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE CASCADE,
 prerequisite_id TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE RESTRICT,
 PRIMARY KEY(task_id,prerequisite_id), CHECK(task_id<>prerequisite_id)
);
CREATE TABLE pulsation_batches (
 pulsation_id TEXT PRIMARY KEY REFERENCES pulsations(id) ON DELETE CASCADE,
 manifest_json TEXT NOT NULL CHECK(json_valid(manifest_json)), manifest_sha256 TEXT NOT NULL
);
CREATE TABLE pulsation_tasks (
 pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
 task_id TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE RESTRICT,
 revision INTEGER NOT NULL, position INTEGER NOT NULL,
 PRIMARY KEY(pulsation_id,task_id)
);
CREATE TABLE task_evidence (
 pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
 job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE, generation INTEGER NOT NULL,
 stage TEXT NOT NULL, task_id TEXT NOT NULL REFERENCES work_tasks(id) ON DELETE RESTRICT,
 evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)), PRIMARY KEY(job_id,generation,task_id)
);
CREATE TABLE forge_commands (
 project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 request_id TEXT NOT NULL, kind TEXT NOT NULL, input_sha256 TEXT NOT NULL,
 result_json TEXT NOT NULL CHECK(json_valid(result_json)),
 PRIMARY KEY(project_id,request_id)
);
CREATE TABLE forge_turns (
 id TEXT PRIMARY KEY, feature_id TEXT NOT NULL REFERENCES forge_features(id) ON DELETE CASCADE,
 feature_revision INTEGER NOT NULL, ordinal INTEGER NOT NULL, request_id TEXT NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('running','completed','failed','cancelled','interrupted')),
 user_message TEXT NOT NULL, assistant_message TEXT, response_json TEXT,
 error TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
 UNIQUE(feature_id,request_id), UNIQUE(feature_id,ordinal)
);
CREATE UNIQUE INDEX forge_one_active_turn ON forge_turns(feature_id) WHERE status='running';
CREATE TABLE forge_intake_cache (
 feature_id TEXT PRIMARY KEY REFERENCES forge_features(id) ON DELETE CASCADE,
 input_sha256 TEXT NOT NULL, analysis_json TEXT NOT NULL CHECK(json_valid(analysis_json)),
 sources_json TEXT NOT NULL CHECK(json_valid(sources_json))
);

CREATE TABLE forge_task_conversions (task_id TEXT PRIMARY KEY REFERENCES work_tasks(id),feature_id TEXT NOT NULL UNIQUE REFERENCES forge_features(id));

CREATE TABLE forge_publications (feature_id TEXT NOT NULL REFERENCES forge_features(id), revision INTEGER NOT NULL, relative_path TEXT NOT NULL UNIQUE,content TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','applied','conflict')),error TEXT,PRIMARY KEY(feature_id,revision));
