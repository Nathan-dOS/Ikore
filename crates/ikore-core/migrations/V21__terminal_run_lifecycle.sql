ALTER TABLE terminal_runs ADD COLUMN deadline_at TEXT;

ALTER TABLE terminal_runs ADD COLUMN termination_reason TEXT
  CHECK(termination_reason IS NULL OR termination_reason IN (
    'exited','user_cancelled','pipeline_cancelled','shutdown','timeout',
    'output_limit','spawn_failed','io_failed','persistence_failed','interrupted'
  ));

ALTER TABLE terminal_runs ADD COLUMN output_bytes INTEGER NOT NULL DEFAULT 0
  CHECK(output_bytes >= 0);

ALTER TABLE terminal_runs ADD COLUMN event_count INTEGER NOT NULL DEFAULT 0
  CHECK(event_count >= 0);

ALTER TABLE terminal_runs ADD COLUMN workspace_snapshot_hash TEXT;

ALTER TABLE terminal_runs ADD COLUMN launch_identity_sha256 TEXT;

ALTER TABLE terminal_runs ADD COLUMN detected_cli_version TEXT;

ALTER TABLE terminal_runs ADD COLUMN adapter_contract_version INTEGER NOT NULL DEFAULT 1
  CHECK(adapter_contract_version > 0);

ALTER TABLE terminal_events ADD COLUMN redaction_count INTEGER NOT NULL DEFAULT 0
  CHECK(redaction_count >= 0);

ALTER TABLE terminal_events ADD COLUMN redaction_categories_json TEXT NOT NULL DEFAULT '[]'
  CHECK(json_valid(redaction_categories_json));

CREATE INDEX idx_terminal_runs_active
  ON terminal_runs(project_id,status,updated_at DESC)
  WHERE status IN ('queued','starting','running');
