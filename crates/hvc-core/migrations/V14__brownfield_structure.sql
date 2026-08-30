CREATE TABLE brownfield_source_cache (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL,
  content_sha256 TEXT NOT NULL CHECK(length(content_sha256) = 64),
  language TEXT NOT NULL,
  parser_version TEXT NOT NULL,
  query_version TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('mapped','partial','unsupported')),
  symbols_json TEXT NOT NULL CHECK(json_valid(symbols_json)),
  references_json TEXT NOT NULL CHECK(json_valid(references_json)),
  parse_elapsed_us INTEGER NOT NULL DEFAULT 0 CHECK(parse_elapsed_us >= 0),
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, relative_path)
);

CREATE TABLE brownfield_edges (
  project_id TEXT NOT NULL,
  source_path TEXT NOT NULL,
  relation TEXT NOT NULL CHECK(relation IN ('import','call','lexical')),
  observed_text TEXT NOT NULL,
  line INTEGER NOT NULL CHECK(line > 0),
  destination_path TEXT,
  resolution TEXT NOT NULL CHECK(resolution IN ('unresolved','external','resolved')),
  source_revision_sha256 TEXT NOT NULL CHECK(length(source_revision_sha256) = 64),
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, source_path, relation, observed_text, line),
  FOREIGN KEY(project_id, source_path)
    REFERENCES brownfield_source_cache(project_id, relative_path) ON DELETE CASCADE
);

CREATE INDEX idx_brownfield_edges_destination
  ON brownfield_edges(project_id, destination_path);
CREATE INDEX idx_brownfield_edges_source_revision
  ON brownfield_edges(project_id, source_path, source_revision_sha256);

ALTER TABLE jobs ADD COLUMN maintenance_metrics_json TEXT
  CHECK(maintenance_metrics_json IS NULL OR json_valid(maintenance_metrics_json));
