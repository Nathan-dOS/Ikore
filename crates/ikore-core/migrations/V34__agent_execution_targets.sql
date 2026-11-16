-- The migration host disables foreign keys and verifies them after migration.
-- Keep referencing tables pointed at the final table name during the rebuild.
PRAGMA legacy_alter_table=ON;

CREATE TABLE free_form_provider_route_bindings (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  slot_id TEXT NOT NULL REFERENCES terminal_slots(id) ON DELETE CASCADE,
  route_id TEXT NOT NULL REFERENCES provider_routes(id) ON DELETE RESTRICT,
  enabled INTEGER NOT NULL DEFAULT 1,
  version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, slot_id)
);

CREATE INDEX idx_free_form_route_bindings_route
  ON free_form_provider_route_bindings(route_id, enabled);

CREATE TABLE llm_route_executions_v34 (
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
  FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE SET NULL,
  FOREIGN KEY(job_id) REFERENCES jobs(id) ON DELETE SET NULL,
  FOREIGN KEY(route_id) REFERENCES provider_routes(id) ON DELETE RESTRICT,
  CHECK(
    (agent_role IS NOT NULL AND free_form_slot_id IS NULL)
    OR
    (agent_role IS NULL AND free_form_slot_id IS NOT NULL)
  )
);

INSERT INTO llm_route_executions_v34(
  id,generation_id,project_id,pulsation_id,job_id,route_id,agent_role,
  free_form_slot_id,stage,strategy,mode,status,decision_json,next_eligible_at,
  visible_output,visible_output_complete,started_at,finished_at
)
SELECT
  id,generation_id,project_id,pulsation_id,job_id,route_id,agent_role,
  NULL,stage,strategy,mode,status,decision_json,next_eligible_at,
  visible_output,visible_output_complete,started_at,finished_at
FROM llm_route_executions;

DROP TABLE llm_route_executions;

ALTER TABLE llm_route_executions_v34 RENAME TO llm_route_executions;

CREATE INDEX idx_llm_executions_project_started
  ON llm_route_executions(project_id, started_at DESC);

CREATE INDEX idx_llm_executions_project_role_started
  ON llm_route_executions(project_id, agent_role, started_at DESC)
  WHERE agent_role IS NOT NULL;

CREATE INDEX idx_llm_executions_project_free_form_started
  ON llm_route_executions(project_id, free_form_slot_id, started_at DESC)
  WHERE free_form_slot_id IS NOT NULL;
