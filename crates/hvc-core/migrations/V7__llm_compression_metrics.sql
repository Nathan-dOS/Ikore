CREATE TABLE IF NOT EXISTS llm_compression_metrics (
  id TEXT PRIMARY KEY,
  execution_id TEXT,
  engine TEXT NOT NULL,
  engine_version TEXT NOT NULL,
  filter_name TEXT,
  content_kind TEXT NOT NULL,
  mode TEXT NOT NULL,
  original_sha256 TEXT NOT NULL,
  original_bytes INTEGER NOT NULL,
  compressed_bytes INTEGER NOT NULL,
  duration_ms INTEGER NOT NULL,
  result TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(execution_id) REFERENCES llm_route_executions(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_compression_metrics_created
  ON llm_compression_metrics(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_compression_metrics_execution
  ON llm_compression_metrics(execution_id);
