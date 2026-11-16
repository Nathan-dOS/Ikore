-- M4 does not replay historical unavailable Learning intents.
ALTER TABLE artifact_publications ADD COLUMN job_class TEXT NOT NULL DEFAULT 'main' CHECK(job_class IN ('main','background'));
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
CREATE TABLE embedding_sync_state (
  workspace_path TEXT PRIMARY KEY, dirty INTEGER NOT NULL DEFAULT 1, revision INTEGER NOT NULL DEFAULT 0,
  next_reconcile_at TEXT NOT NULL, last_error TEXT, retry_failed INTEGER NOT NULL DEFAULT 0,
  scan_cursor TEXT NOT NULL DEFAULT '', embedding_model TEXT NOT NULL DEFAULT ''
);
CREATE TABLE embedding_deltas (
  workspace_path TEXT NOT NULL, relative_path TEXT NOT NULL,
  embedding_model TEXT NOT NULL, desired_hash TEXT,
  job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id),
  PRIMARY KEY(workspace_path,relative_path,embedding_model)
);
CREATE INDEX idx_learning_admission ON learning_runs(state,created_at,job_id);
