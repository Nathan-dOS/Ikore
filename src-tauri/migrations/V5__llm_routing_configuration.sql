CREATE TABLE IF NOT EXISTS provider_connections (
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

CREATE TABLE IF NOT EXISTS provider_routes (
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

CREATE TABLE IF NOT EXISTS provider_route_targets (
  route_id TEXT NOT NULL,
  position INTEGER NOT NULL CHECK(position >= 0),
  connection_id TEXT NOT NULL,
  model_id TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(route_id, position),
  UNIQUE(route_id, connection_id, model_id),
  FOREIGN KEY(route_id) REFERENCES provider_routes(id) ON DELETE RESTRICT,
  FOREIGN KEY(connection_id) REFERENCES provider_connections(id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS provider_route_bindings (
  project_id TEXT NOT NULL,
  agent_role TEXT NOT NULL,
  stage TEXT NOT NULL,
  route_id TEXT NOT NULL,
  enabled INTEGER NOT NULL DEFAULT 1,
  version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, agent_role, stage),
  FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
  FOREIGN KEY(route_id) REFERENCES provider_routes(id) ON DELETE RESTRICT
);

ALTER TABLE jobs ADD COLUMN available_at TEXT;

ALTER TABLE agent_runs ADD COLUMN route_id TEXT REFERENCES provider_routes(id) ON DELETE SET NULL;
ALTER TABLE agent_runs ADD COLUMN connection_id TEXT REFERENCES provider_connections(id) ON DELETE SET NULL;
ALTER TABLE agent_runs ADD COLUMN llm_execution_id TEXT;

CREATE INDEX IF NOT EXISTS idx_provider_connections_provider_enabled
  ON provider_connections(provider_id, enabled);
CREATE INDEX IF NOT EXISTS idx_provider_route_targets_route_position
  ON provider_route_targets(route_id, position);
CREATE INDEX IF NOT EXISTS idx_provider_bindings_lookup
  ON provider_route_bindings(project_id, agent_role, stage, enabled);
CREATE INDEX IF NOT EXISTS idx_jobs_available
  ON jobs(status, available_at, created_at);
CREATE INDEX IF NOT EXISTS idx_agent_runs_llm_execution
  ON agent_runs(llm_execution_id);

INSERT INTO runtime_settings (key, value, updated_at)
VALUES ('runtime.routing_v2_mode', 'on', CURRENT_TIMESTAMP)
ON CONFLICT(key) DO NOTHING;
