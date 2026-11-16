ALTER TABLE jobs ADD COLUMN cancel_requested_at TEXT;

ALTER TABLE agent_runs ADD COLUMN provider TEXT;
ALTER TABLE agent_runs ADD COLUMN model TEXT;
ALTER TABLE agent_runs ADD COLUMN estimated_cost_usd REAL;

CREATE TABLE IF NOT EXISTS runtime_settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS agent_run_outputs (
  id TEXT PRIMARY KEY,
  agent_run_id TEXT NOT NULL UNIQUE,
  job_id TEXT NOT NULL,
  pulsation_id TEXT,
  request_payload TEXT NOT NULL,
  response_payload TEXT NOT NULL,
  raw_output TEXT NOT NULL,
  structured_output TEXT,
  schema_json TEXT,
  validation_error TEXT,
  tool_trace TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(agent_run_id) REFERENCES agent_runs(id) ON DELETE CASCADE,
  FOREIGN KEY(job_id) REFERENCES jobs(id) ON DELETE CASCADE,
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_agent_runs_provider_model
  ON agent_runs(provider, model);

CREATE INDEX IF NOT EXISTS idx_agent_run_outputs_pulsation_created_at
  ON agent_run_outputs(pulsation_id, created_at DESC);
