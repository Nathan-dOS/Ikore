-- API model discovery is intentionally separate from the CLI catalog.  A row belongs to one
-- saved API connection; it must never be used to infer the models or reasoning settings of a
-- locally installed CLI.
CREATE TABLE api_model_catalog_snapshots (
  id TEXT PRIMARY KEY,
  connection_id TEXT NOT NULL REFERENCES provider_connections(id) ON DELETE CASCADE,
  provider_id TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('ready','stale','failed','unsupported')),
  source TEXT NOT NULL CHECK(source IN ('provider_api')),
  diagnostic TEXT,
  refreshed_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(connection_id)
);

CREATE TABLE api_model_catalog_entries (
  snapshot_id TEXT NOT NULL REFERENCES api_model_catalog_snapshots(id) ON DELETE CASCADE,
  model_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  context_window INTEGER,
  max_output_tokens INTEGER,
  capabilities_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(capabilities_json)),
  reasoning_efforts_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(reasoning_efforts_json)),
  raw_metadata_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(raw_metadata_json)),
  position INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(snapshot_id,model_id)
);

CREATE INDEX idx_api_model_catalog_entries_snapshot_position
  ON api_model_catalog_entries(snapshot_id,position,model_id);
