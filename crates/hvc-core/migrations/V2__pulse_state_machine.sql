ALTER TABLE jobs ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE jobs ADD COLUMN last_error TEXT;
ALTER TABLE jobs ADD COLUMN started_at TEXT;
ALTER TABLE jobs ADD COLUMN finished_at TEXT;

CREATE TABLE IF NOT EXISTS artifacts (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  relative_path TEXT,
  content_sha256 TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS pulse_events (
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

CREATE INDEX IF NOT EXISTS idx_jobs_status_created_at
  ON jobs(status, created_at);

CREATE INDEX IF NOT EXISTS idx_artifacts_pulsation_id
  ON artifacts(pulsation_id);

CREATE INDEX IF NOT EXISTS idx_pulse_events_pulsation_id_created_at
  ON pulse_events(pulsation_id, created_at DESC);
