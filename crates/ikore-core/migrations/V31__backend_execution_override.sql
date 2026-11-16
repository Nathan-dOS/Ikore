-- The per-pulsation override could only ever say "stop using the CLI and call
-- the API". That left one recovery for a failing CLI backend that a human had
-- to authorise, while the recovery that actually worked in the field - run this
-- role on a different CLI - was only reachable by editing project settings by
-- hand, mid-run.
--
-- Widening this row lets the host elect the next healthy backend itself and
-- record that choice with the same scope and the same audit trail the API
-- fallback already had.
--
-- SQLite cannot widen a CHECK in place, so the table is rebuilt.
-- `legacy_alter_table` keeps DROP+RENAME from rewriting FK clauses elsewhere,
-- matching V27 and V30.
PRAGMA legacy_alter_table=ON;

CREATE TABLE pulse_execution_overrides_v31 (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL CHECK(agent_role IN (
    'intake_router','task_specifier','contract_agent','backend_agent',
    'test_agent','integrator','learning_agent','feature_agent'
  )),
  execution_mode TEXT NOT NULL CHECK(execution_mode IN ('api_direct','cli_process')),
  -- Required when the override keeps the CLI transport, forbidden otherwise:
  -- an api_direct row has no backend to name.
  backend TEXT CHECK(backend IS NULL OR backend IN ('codex','claude','gemini','opencode','custom')),
  source_checkpoint_id TEXT REFERENCES checkpoints(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  PRIMARY KEY(pulsation_id,agent_role),
  CHECK(
    (execution_mode='api_direct' AND backend IS NULL)
    OR
    (execution_mode='cli_process' AND backend IS NOT NULL)
  )
);

INSERT INTO pulse_execution_overrides_v31("pulsation_id","agent_role","execution_mode","backend","source_checkpoint_id","created_at")
SELECT "pulsation_id","agent_role","execution_mode",NULL,"source_checkpoint_id","created_at" FROM pulse_execution_overrides;

DROP TABLE pulse_execution_overrides;

ALTER TABLE pulse_execution_overrides_v31 RENAME TO pulse_execution_overrides;

-- Which backends a pulsation already burned on a role, so the ladder never
-- re-elects one that just failed and can tell when it is exhausted.
CREATE TABLE cli_backend_attempts (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  agent_role TEXT NOT NULL,
  backend TEXT NOT NULL CHECK(backend IN ('codex','claude','gemini','opencode','custom')),
  error_code TEXT NOT NULL,
  diagnostic TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(pulsation_id,agent_role,backend)
);
