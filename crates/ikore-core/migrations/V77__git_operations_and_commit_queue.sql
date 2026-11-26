-- Feature 08 M2: host git operation history and pulsation commit queue.
-- Additive.

CREATE TABLE git_operations (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('commit','push','pull')),
  pulsation_id TEXT REFERENCES pulsations(id) ON DELETE SET NULL,
  rules_version INTEGER NOT NULL,
  rules_sha256 TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('running','succeeded','blocked','failed')),
  receipt_json TEXT CHECK(receipt_json IS NULL OR json_valid(receipt_json)),
  violations_json TEXT CHECK(violations_json IS NULL OR json_valid(violations_json)),
  started_at TEXT NOT NULL,
  finished_at TEXT
);

CREATE INDEX idx_git_operations_project
  ON git_operations(project_id, started_at DESC);

CREATE INDEX idx_git_operations_pulsation
  ON git_operations(pulsation_id, started_at DESC)
  WHERE pulsation_id IS NOT NULL;

CREATE TABLE pulsation_commit_state (
  pulsation_id TEXT PRIMARY KEY REFERENCES pulsations(id) ON DELETE CASCADE,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  completed_at TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('pending','committed','skipped')),
  skip_reason TEXT,
  branch TEXT,
  head_commit TEXT,
  updated_at TEXT NOT NULL,
  CHECK((state = 'skipped') = (skip_reason IS NOT NULL AND length(trim(skip_reason)) > 0))
);

CREATE INDEX idx_pulsation_commit_state_queue
  ON pulsation_commit_state(project_id, state, completed_at, pulsation_id);
