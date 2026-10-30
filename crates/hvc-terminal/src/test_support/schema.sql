-- Native repository test fixture. Real migration compatibility is tested in hvc-test-support.
CREATE TABLE agent_runs (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL,
  agent_name TEXT NOT NULL,
  status TEXT NOT NULL,
  input_tokens INTEGER,
  output_tokens INTEGER,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL, provider TEXT, model TEXT, estimated_cost_usd REAL, route_id TEXT REFERENCES provider_routes(id) ON DELETE SET NULL, connection_id TEXT REFERENCES provider_connections(id) ON DELETE SET NULL, llm_execution_id TEXT,
  FOREIGN KEY(job_id) REFERENCES jobs(id) ON DELETE CASCADE
);

CREATE TABLE artifacts (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  relative_path TEXT,
  content_sha256 TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL, content TEXT, producer_job_id TEXT REFERENCES jobs(id), producer_stage TEXT, generation INTEGER, input_revision INTEGER, producer_state_version INTEGER, output_position INTEGER, acceptance TEXT NOT NULL DEFAULT 'candidate' CHECK(acceptance IN ('candidate','accepted','superseded')), publication_state TEXT NOT NULL DEFAULT 'not_required' CHECK(publication_state IN ('not_required','pending','applying','applied','conflict')), source_revisions_json TEXT NOT NULL DEFAULT '[]', legacy_adaptation TEXT, reconciled_at TEXT,
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE CASCADE
);

CREATE TABLE "checkpoints" (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL,
  stage TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'artifact_review'
    CHECK(kind IN ('artifact_review','clarification','cli_fallback','stage_failure','path_confirmation')),
  input_revision INTEGER NOT NULL DEFAULT 1,
  state_version INTEGER NOT NULL DEFAULT 1,
  producer_job_id TEXT REFERENCES jobs(id),
  generation INTEGER,
  purpose TEXT NOT NULL DEFAULT 'artifact_review'
    CHECK(purpose IN ('artifact_review','clarification','cli_fallback','stage_failure','path_confirmation')), visualizer_brief_json TEXT
  CHECK(visualizer_brief_json IS NULL OR json_valid(visualizer_brief_json)),
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE CASCADE
);

CREATE TABLE "cli_agent_configs" (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent','visualizer','notes_scribe','documentation_reconciler'
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

CREATE TABLE "cli_backend_attempts" (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL,
  backend TEXT NOT NULL CHECK(backend IN ('codex','claude','gemini','opencode','antigravity','custom')),
  error_code TEXT NOT NULL,
  diagnostic TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(pulsation_id,agent_role,backend)
);

CREATE TABLE "cli_fallback_requests" (
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

CREATE TABLE cli_model_catalog_entries (
  snapshot_id TEXT NOT NULL REFERENCES cli_model_catalog_snapshots(id) ON DELETE CASCADE,
  provider_id TEXT,
  model_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  context_window INTEGER,
  input_cost_usd_per_million REAL,
  output_cost_usd_per_million REAL,
  capabilities_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(capabilities_json)),
  profiles_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(profiles_json)),
  raw_metadata_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(raw_metadata_json)),
  position INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(snapshot_id,model_id)
);

CREATE TABLE "cli_model_catalog_snapshots" (
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

CREATE TABLE cli_observation_samples (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id TEXT NOT NULL REFERENCES terminal_runs(id) ON DELETE CASCADE,
  observed_at TEXT NOT NULL,
  elapsed_ms INTEGER NOT NULL CHECK(elapsed_ms >= 0),
  user_cpu_100ns INTEGER,
  kernel_cpu_100ns INTEGER,
  peak_process_memory_bytes INTEGER,
  peak_job_memory_bytes INTEGER
);

CREATE TABLE cli_observation_trials (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  state TEXT NOT NULL CHECK(state IN ('active','closing','completed','cancelled')),
  target_runs INTEGER NOT NULL DEFAULT 20 CHECK(target_runs = 20),
  admitted_runs INTEGER NOT NULL DEFAULT 0 CHECK(admitted_runs BETWEEN 0 AND 20),
  settled_runs INTEGER NOT NULL DEFAULT 0 CHECK(settled_runs BETWEEN 0 AND 20),
  started_at TEXT NOT NULL,
  closed_at TEXT,
  completed_at TEXT
);

CREATE TABLE "composition_specialty_overrides" (
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

CREATE TABLE jobs (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, last_error TEXT, started_at TEXT, finished_at TEXT, cancel_requested_at TEXT, available_at TEXT, project_id TEXT REFERENCES projects(id) ON DELETE CASCADE, input_revision INTEGER, generation INTEGER NOT NULL DEFAULT 0, state_version INTEGER NOT NULL DEFAULT 1, job_class TEXT NOT NULL DEFAULT 'main' CHECK(job_class IN ('main','background')), cancellation_reason TEXT CHECK(cancellation_reason IN ('user_cancel','shutdown','preempted','superseded')), idempotency_key TEXT, maintenance_metrics_json TEXT
  CHECK(maintenance_metrics_json IS NULL OR json_valid(maintenance_metrics_json)),
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE SET NULL
);

CREATE TABLE projects (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  workspace_path TEXT NOT NULL UNIQUE,
  is_archived INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_opened_at TEXT
);

CREATE TABLE provider_connections (
  id TEXT PRIMARY KEY,
  provider_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  credential_ref TEXT NOT NULL UNIQUE,
  endpoint_url TEXT,
  default_model TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  state TEXT NOT NULL DEFAULT 'active' CHECK(state IN ('active', 'blocked')),
  blocked_reason TEXT,
  cooldown_until TEXT,
  backoff_level INTEGER NOT NULL DEFAULT 0,
  max_concurrency INTEGER NOT NULL DEFAULT 1 CHECK(max_concurrency > 0),
  version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE provider_routes (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL UNIQUE,
  strategy TEXT NOT NULL DEFAULT 'priority' CHECK(strategy IN ('priority', 'lkgp', 'score')),
  max_attempts INTEGER NOT NULL DEFAULT 3 CHECK(max_attempts BETWEEN 1 AND 10),
  attempt_timeout_ms INTEGER NOT NULL DEFAULT 30000 CHECK(attempt_timeout_ms BETWEEN 1000 AND 300000),
  route_timeout_ms INTEGER NOT NULL DEFAULT 45000 CHECK(route_timeout_ms BETWEEN 1000 AND 600000),
  enabled INTEGER NOT NULL DEFAULT 1,
  version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE "pulsations" (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  feature_name TEXT NOT NULL,
  stage TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL, queue_sequence INTEGER, input_revision INTEGER NOT NULL DEFAULT 1 CHECK(input_revision > 0), state_version INTEGER NOT NULL DEFAULT 1 CHECK(state_version > 0), pipeline_version INTEGER NOT NULL DEFAULT 1 CHECK(pipeline_version IN (1,2,3,4)), terminal_reason TEXT, revision_feedback TEXT, coordination_hold INTEGER NOT NULL DEFAULT 0 CHECK(coordination_hold IN (0,1)), integration_checkpoint_id TEXT REFERENCES checkpoints(id), integration_state_version INTEGER, specification_started INTEGER NOT NULL DEFAULT 0 CHECK(specification_started IN (0,1)),
  FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
);

CREATE TABLE pulse_events (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL,
  event_type TEXT NOT NULL,
  from_status TEXT,
  to_status TEXT,
  from_stage TEXT,
  to_stage TEXT,
  message TEXT,
  created_at TEXT NOT NULL,
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE CASCADE
);

CREATE TABLE "pulse_execution_overrides" (
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

CREATE TABLE quota_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  backend TEXT NOT NULL CHECK (backend IN ('codex','claude','gemini','opencode','antigravity')),
  scope TEXT CHECK (scope IS NULL OR length(scope) <= 48),
  window_minutes INTEGER CHECK (window_minutes IS NULL OR window_minutes > 0),
  used_percent REAL NOT NULL CHECK (used_percent >= 0 AND used_percent <= 100),
  resets_at TEXT,
  source TEXT NOT NULL CHECK (source IN (
    'codex_app_server','codex_rollout','codex_endpoint',
    'claude_endpoint','claude_stream_event','antigravity_usage'
  )),
  plan TEXT CHECK (plan IS NULL OR length(plan) <= 48),
  observed_at TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('sample','run_start','run_end')),
  run_id TEXT,
  pulsation_id TEXT,
  CHECK ((kind = 'sample') = (run_id IS NULL))
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
  created_at TEXT NOT NULL, redaction_count INTEGER NOT NULL DEFAULT 0
  CHECK(redaction_count >= 0), redaction_categories_json TEXT NOT NULL DEFAULT '[]'
  CHECK(json_valid(redaction_categories_json)),
  UNIQUE(run_id,sequence)
);

CREATE TABLE "terminal_model_observations" (
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

CREATE TABLE "terminal_runs" (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  slot_kind TEXT NOT NULL DEFAULT 'canonical'
    CHECK(slot_kind IN ('canonical','free_form','ephemeral')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent','visualizer','notes_scribe','documentation_reconciler'
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
-- V63 (ADR-037 M3)
-- A run that records a manifest (fingerprint version 2) and, when it resumed
-- a session, the digest of the change list its notice described. A run with
-- no row is version 1: its sessions resume only on an unchanged workspace.
CREATE TABLE terminal_run_resume_state (
  run_id TEXT PRIMARY KEY REFERENCES terminal_runs(id) ON DELETE CASCADE,
  session_fingerprint_version INTEGER NOT NULL CHECK(session_fingerprint_version IN (1, 2)),
  external_changes_sha256 TEXT
    CHECK(external_changes_sha256 IS NULL OR length(external_changes_sha256) = 64)
);
CREATE TABLE terminal_run_snapshot_files (
  run_id TEXT NOT NULL REFERENCES terminal_runs(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL CHECK(length(relative_path) BETWEEN 1 AND 1024),
  original_sha256 TEXT NOT NULL CHECK(length(original_sha256) = 64),
  projected_sha256 TEXT NOT NULL CHECK(length(projected_sha256) = 64),
  byte_len INTEGER NOT NULL CHECK(byte_len >= 0),
  blob_sha256 TEXT CHECK(blob_sha256 IS NULL OR length(blob_sha256) = 64),
  PRIMARY KEY(run_id, relative_path)
);
CREATE TABLE terminal_snapshot_blobs (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
  content TEXT NOT NULL,
  byte_len INTEGER NOT NULL CHECK(byte_len BETWEEN 0 AND 32768),
  created_at TEXT NOT NULL,
  PRIMARY KEY(project_id, sha256)
);

CREATE TABLE "terminal_slots" (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('canonical','free_form','ephemeral')),
  agent_role TEXT CHECK(agent_role IS NULL OR agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent','visualizer','notes_scribe','documentation_reconciler'
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

CREATE TABLE token_usage_records (
    source_kind TEXT NOT NULL,
    source_id TEXT NOT NULL,
    unit_key TEXT NOT NULL,
    project_id TEXT,
    transport TEXT NOT NULL CHECK(transport IN ('api','cli')),
    provider_id TEXT NOT NULL,
    requested_model TEXT,
    observed_model TEXT,
    input_tokens INTEGER CHECK(input_tokens >= 0),
    output_tokens INTEGER CHECK(output_tokens >= 0),
    total_tokens INTEGER CHECK(total_tokens >= 0),
    cache_read_tokens INTEGER CHECK(cache_read_tokens >= 0),
    cache_write_tokens INTEGER CHECK(cache_write_tokens >= 0),
    reasoning_tokens INTEGER CHECK(reasoning_tokens >= 0),
    quality TEXT NOT NULL CHECK(quality IN ('complete','partial','unavailable')),
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    historical INTEGER NOT NULL DEFAULT 0 CHECK(historical IN (0,1)), pulsation_id TEXT, stage_execution_id TEXT, work_unit_id TEXT, attempt_id TEXT, executor_kind TEXT,
    PRIMARY KEY(source_kind,source_id,unit_key)
);

CREATE INDEX idx_projects_last_opened_at
  ON projects(last_opened_at DESC);

CREATE INDEX idx_jobs_pulsation_id
  ON jobs(pulsation_id);

CREATE INDEX idx_agent_runs_job_id
  ON agent_runs(job_id);

CREATE INDEX idx_jobs_status_created_at
  ON jobs(status, created_at);

CREATE INDEX idx_artifacts_pulsation_id
  ON artifacts(pulsation_id);

CREATE INDEX idx_pulse_events_pulsation_id_created_at
  ON pulse_events(pulsation_id, created_at DESC);

CREATE INDEX idx_agent_runs_provider_model
  ON agent_runs(provider, model);

CREATE INDEX idx_provider_connections_provider_enabled
  ON provider_connections(provider_id, enabled);

CREATE INDEX idx_jobs_available
  ON jobs(status, available_at, created_at);

CREATE INDEX idx_agent_runs_llm_execution
  ON agent_runs(llm_execution_id);

CREATE TRIGGER initialize_pipeline_state AFTER INSERT ON projects
BEGIN INSERT INTO project_pipeline_state(project_id) VALUES(NEW.id); END;

CREATE UNIQUE INDEX idx_job_idempotency ON jobs(idempotency_key);

CREATE UNIQUE INDEX idx_artifact_producer ON artifacts(producer_job_id,generation,output_position);

CREATE INDEX idx_jobs_admission ON jobs(job_class,status,available_at,project_id);

CREATE INDEX idx_terminal_events_run_sequence ON terminal_events(run_id,sequence);

CREATE INDEX idx_cli_model_catalog_entries_snapshot_position
  ON cli_model_catalog_entries(snapshot_id,position,model_id);

CREATE INDEX idx_cli_fallback_pulsation ON cli_fallback_requests(pulsation_id,created_at DESC);

CREATE INDEX idx_token_usage_project_started ON token_usage_records(project_id,started_at);

CREATE INDEX idx_token_usage_started ON token_usage_records(started_at);

CREATE INDEX idx_token_usage_pulsation_started ON token_usage_records(pulsation_id,started_at);

CREATE INDEX idx_pulsations_project_id
  ON pulsations(project_id);

CREATE UNIQUE INDEX idx_pulse_sequence ON pulsations(project_id, queue_sequence);

CREATE TRIGGER pulse_sequence_immutable BEFORE UPDATE OF queue_sequence ON pulsations
WHEN OLD.queue_sequence IS NOT NULL AND NEW.queue_sequence IS NOT OLD.queue_sequence
BEGIN SELECT RAISE(ABORT, 'queue_sequence is immutable'); END;

CREATE INDEX idx_token_usage_stage_execution ON token_usage_records(stage_execution_id);

CREATE INDEX idx_checkpoints_pulsation_id ON checkpoints(pulsation_id);

CREATE UNIQUE INDEX idx_checkpoint_producer ON checkpoints(producer_job_id,generation,kind);

CREATE INDEX idx_checkpoints_purpose ON checkpoints(pulsation_id,purpose,status);

CREATE UNIQUE INDEX idx_cli_observation_trials_one_open
  ON cli_observation_trials(project_id)
  WHERE state IN ('active','closing');

CREATE INDEX idx_cli_observation_samples_run_time
  ON cli_observation_samples(run_id, observed_at);

CREATE INDEX terminal_model_observations_generation
  ON terminal_model_observations(run_id, is_generation, ordinal);

CREATE INDEX idx_quota_snapshots_window
  ON quota_snapshots(backend, scope, window_minutes, observed_at);

CREATE INDEX idx_quota_snapshots_run
  ON quota_snapshots(run_id) WHERE run_id IS NOT NULL;

CREATE INDEX idx_quota_snapshots_observed
  ON quota_snapshots(observed_at);

CREATE INDEX idx_cli_agent_configs_project ON cli_agent_configs(project_id);

CREATE UNIQUE INDEX idx_terminal_slots_canonical_role
  ON terminal_slots(project_id,agent_role) WHERE kind='canonical';

CREATE INDEX idx_terminal_slots_project_position
  ON terminal_slots(project_id,archived,kind,position);

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

CREATE TABLE runtime_settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE secret_sanitization_receipts (
 call_id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 pulsation_id TEXT REFERENCES pulsations(id) ON DELETE CASCADE,
 policy_identity TEXT NOT NULL,
 input_redaction_count INTEGER NOT NULL CHECK(input_redaction_count >= 0),
 categories_json TEXT NOT NULL,
 examined_bytes INTEGER NOT NULL CHECK(examined_bytes >= 0),
 duration_ms INTEGER NOT NULL CHECK(duration_ms >= 0),
 exception_count INTEGER NOT NULL DEFAULT 0 CHECK(exception_count >= 0),
 created_at TEXT NOT NULL
);

-- Referenced by the project initialization trigger.
CREATE TABLE project_pipeline_state (
  project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  active_pulsation_id TEXT REFERENCES pulsations(id) ON DELETE RESTRICT,
  base_epoch INTEGER NOT NULL DEFAULT 0,
  base_state TEXT NOT NULL DEFAULT 'ready' CHECK(base_state IN ('ready','publishing','needs_review')),
  state_version INTEGER NOT NULL DEFAULT 1,
  blocking_reason TEXT
);

-- Host job fixtures referenced by observation reports.
CREATE TABLE learning_runs (
  job_id TEXT PRIMARY KEY REFERENCES jobs(id),
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id),
  integration_epoch INTEGER NOT NULL, report_version INTEGER NOT NULL DEFAULT 1,
  state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','running','preempted','deferred_budget','disabled','publishing','published','conflict','failed','cancelled')),
  budget_calls INTEGER NOT NULL DEFAULT 0 CHECK(budget_calls BETWEEN 0 AND 3),
  total_calls INTEGER NOT NULL DEFAULT 0, transient_failures INTEGER NOT NULL DEFAULT 0,
  budget_grants INTEGER NOT NULL DEFAULT 1, input_json TEXT, report_json TEXT,
  artifact_id TEXT REFERENCES artifacts(id), diagnostic TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  UNIQUE(pulsation_id,integration_epoch,report_version)
);

-- Composition outcomes read by terminal observation reports.
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

CREATE TABLE composition_policy_snapshots (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK(kind IN ('recipe','executor_profile','validation_profile','composition_settings')),
  project_id TEXT REFERENCES projects(id) ON DELETE CASCADE,
  sha256 TEXT NOT NULL,
  snapshot_json TEXT NOT NULL CHECK(json_valid(snapshot_json)),
  created_at TEXT NOT NULL,
  UNIQUE(kind, sha256)
);

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
