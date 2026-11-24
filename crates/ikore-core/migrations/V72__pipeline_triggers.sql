-- Feature 25, M3: the Ikore pipeline's events start the developer's own
-- pipelines. Additive.
--
-- `transition_pulsation` writes one row per event a box of the project listens
-- to, in the transaction that moves the pulsation, so closing the app never
-- loses it. The automation lane dispatches it once the stage's publications
-- are applied, or discards it with the reason when the pulsation failed or
-- the publications were cancelled first (plan R17, T6).
CREATE TABLE pipeline_trigger_outbox (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  event TEXT NOT NULL CHECK(event IN ('stage_passed','pulsation_done')),
  -- `PulsationStage::as_db_value()` of the stage that passed; NULL on
  -- `pulsation_done`.
  stage TEXT CHECK((event = 'stage_passed') = (stage IS NOT NULL)),
  -- The pulsation's state version after the transition.
  state_version INTEGER NOT NULL,
  state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','dispatched','discarded')),
  discard_reason TEXT,
  created_at TEXT NOT NULL,
  dispatched_at TEXT,
  discarded_at TEXT,
  CHECK((state = 'dispatched') = (dispatched_at IS NOT NULL)),
  CHECK((state = 'discarded') = (discarded_at IS NOT NULL AND discard_reason IS NOT NULL))
);
-- stage is NULL on pulsation_done: COALESCE so the UNIQUE holds (SQLite NULLs
-- are distinct).
CREATE UNIQUE INDEX pipeline_trigger_outbox_once
  ON pipeline_trigger_outbox(pulsation_id, event, COALESCE(stage, ''), state_version);
CREATE INDEX idx_pipeline_trigger_outbox_pending
  ON pipeline_trigger_outbox(created_at) WHERE state = 'pending';

-- The event that started a run: each event starts each pipeline once.
ALTER TABLE custom_pipeline_runs ADD COLUMN trigger_outbox_id TEXT
  REFERENCES pipeline_trigger_outbox(id);
CREATE UNIQUE INDEX custom_pipeline_runs_once_per_event
  ON custom_pipeline_runs(pipeline_id, trigger_outbox_id) WHERE trigger_outbox_id IS NOT NULL;
