-- ADR-028 plan-quota history: one row per usage window per observation. Holds no token,
-- account identifier, e-mail or response body. Pruned after 30 days.
CREATE TABLE quota_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  backend TEXT NOT NULL CHECK (backend IN ('codex','claude','gemini','opencode','antigravity')),
  scope TEXT CHECK (scope IS NULL OR length(scope) <= 48),
  window_minutes INTEGER CHECK (window_minutes IS NULL OR window_minutes > 0),
  used_percent REAL NOT NULL CHECK (used_percent >= 0 AND used_percent <= 100),
  resets_at TEXT,
  source TEXT NOT NULL CHECK (source IN (
    'codex_app_server','codex_rollout','codex_endpoint',
    'claude_endpoint','claude_stream_event','antigravity_usage'
  )),
  plan TEXT CHECK (plan IS NULL OR length(plan) <= 48),
  observed_at TEXT NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('sample','run_start','run_end')),
  run_id TEXT,
  pulsation_id TEXT,
  CHECK ((kind = 'sample') = (run_id IS NULL))
);

CREATE INDEX idx_quota_snapshots_window
  ON quota_snapshots(backend, scope, window_minutes, observed_at);
CREATE INDEX idx_quota_snapshots_run
  ON quota_snapshots(run_id) WHERE run_id IS NOT NULL;
CREATE INDEX idx_quota_snapshots_observed
  ON quota_snapshots(observed_at);
