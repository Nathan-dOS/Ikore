CREATE TABLE IF NOT EXISTS provider_circuit_breakers (
  provider_id TEXT NOT NULL,
  endpoint_origin TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'closed' CHECK(state IN ('closed', 'degraded', 'open', 'half_open')),
  failure_count INTEGER NOT NULL DEFAULT 0,
  window_started_at TEXT,
  opened_at TEXT,
  probe_after TEXT,
  probe_in_flight INTEGER NOT NULL DEFAULT 0,
  version INTEGER NOT NULL DEFAULT 1,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(provider_id, endpoint_origin)
);

CREATE TABLE IF NOT EXISTS provider_model_lockouts (
  connection_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  reason TEXT NOT NULL,
  locked_until TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(connection_id, model_id),
  FOREIGN KEY(connection_id) REFERENCES provider_connections(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS llm_route_executions (
  id TEXT PRIMARY KEY,
  generation_id TEXT NOT NULL,
  project_id TEXT NOT NULL,
  pulsation_id TEXT,
  job_id TEXT,
  route_id TEXT NOT NULL,
  agent_role TEXT NOT NULL,
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
  FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE SET NULL,
  FOREIGN KEY(job_id) REFERENCES jobs(id) ON DELETE SET NULL,
  FOREIGN KEY(route_id) REFERENCES provider_routes(id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS llm_attempts (
  id TEXT PRIMARY KEY,
  execution_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  connection_id TEXT NOT NULL,
  provider_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  outcome TEXT NOT NULL,
  error_kind TEXT,
  http_status INTEGER,
  provider_code TEXT,
  latency_ms INTEGER,
  input_tokens INTEGER,
  output_tokens INTEGER,
  estimated_cost_usd REAL,
  started_at TEXT NOT NULL,
  finished_at TEXT,
  UNIQUE(execution_id, ordinal),
  UNIQUE(execution_id, connection_id, model_id),
  FOREIGN KEY(execution_id) REFERENCES llm_route_executions(id) ON DELETE CASCADE,
  FOREIGN KEY(connection_id) REFERENCES provider_connections(id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS llm_route_affinity (
  affinity_hash TEXT NOT NULL,
  route_id TEXT NOT NULL,
  connection_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  last_success_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  PRIMARY KEY(affinity_hash, route_id),
  FOREIGN KEY(route_id) REFERENCES provider_routes(id) ON DELETE CASCADE,
  FOREIGN KEY(connection_id) REFERENCES provider_connections(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_breakers_probe_after
  ON provider_circuit_breakers(state, probe_after);
CREATE INDEX IF NOT EXISTS idx_model_lockouts_eligibility
  ON provider_model_lockouts(connection_id, model_id, locked_until);
CREATE INDEX IF NOT EXISTS idx_llm_executions_project_started
  ON llm_route_executions(project_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_llm_attempts_metrics
  ON llm_attempts(provider_id, model_id, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_llm_affinity_expiry
  ON llm_route_affinity(expires_at);
