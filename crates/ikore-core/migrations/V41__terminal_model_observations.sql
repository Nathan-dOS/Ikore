-- Preserve requested-vs-reported model identity at each provider generation.
-- A row with is_generation=1 and model_id=NULL is an explicit unverified identity.
CREATE TABLE terminal_model_observations (
  run_id TEXT NOT NULL REFERENCES terminal_runs(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK(ordinal > 0),
  event_sequence INTEGER NOT NULL CHECK(event_sequence >= 0),
  model_id TEXT,
  message_id TEXT,
  parent_invocation_id TEXT,
  agent_id TEXT,
  is_generation INTEGER NOT NULL CHECK(is_generation IN (0,1)),
  created_at TEXT NOT NULL,
  PRIMARY KEY(run_id, ordinal),
  CHECK(model_id IS NULL OR length(model_id) BETWEEN 1 AND 256),
  CHECK(message_id IS NULL OR length(message_id) BETWEEN 1 AND 256),
  CHECK(parent_invocation_id IS NULL OR length(parent_invocation_id) BETWEEN 1 AND 256),
  CHECK(agent_id IS NULL OR length(agent_id) BETWEEN 1 AND 256)
);

CREATE INDEX terminal_model_observations_generation
  ON terminal_model_observations(run_id, is_generation, ordinal);

ALTER TABLE llm_attempts ADD COLUMN observed_model TEXT;

CREATE TABLE llm_model_observations (
  attempt_id TEXT NOT NULL REFERENCES llm_attempts(id) ON DELETE CASCADE,
  ordinal INTEGER NOT NULL CHECK(ordinal > 0),
  generation_id TEXT NOT NULL,
  agent_role TEXT,
  agent_target TEXT NOT NULL,
  parent_invocation_id TEXT,
  provider_id TEXT NOT NULL,
  requested_model TEXT NOT NULL,
  observed_model TEXT,
  created_at TEXT NOT NULL,
  PRIMARY KEY(attempt_id, ordinal),
  CHECK(observed_model IS NULL OR length(observed_model) BETWEEN 1 AND 256)
);
