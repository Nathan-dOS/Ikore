ALTER TABLE brownfield_edges RENAME TO brownfield_edges_v14;

CREATE TABLE brownfield_edges (
  project_id TEXT NOT NULL,
  source_path TEXT NOT NULL,
  reference_key TEXT NOT NULL,
  relation TEXT NOT NULL CHECK(relation IN ('import','reexport','call','type_use','inherits','implements','lexical')),
  observed_text TEXT NOT NULL,
  line INTEGER NOT NULL CHECK(line > 0),
  normalized_reference_json TEXT
    CHECK(normalized_reference_json IS NULL OR json_valid(normalized_reference_json)),
  source_symbol_key TEXT,
  destination_path TEXT,
  destination_revision_sha256 TEXT
    CHECK(destination_revision_sha256 IS NULL OR length(destination_revision_sha256) = 64),
  destination_symbol_key TEXT,
  resolution TEXT NOT NULL CHECK(resolution IN ('unresolved','ambiguous','external','resolved')),
  external_module TEXT,
  resolution_method TEXT,
  resolution_reason TEXT,
  candidate_count INTEGER NOT NULL DEFAULT 0 CHECK(candidate_count >= 0),
  resolver_version TEXT,
  resolution_context_sha256 TEXT
    CHECK(resolution_context_sha256 IS NULL OR length(resolution_context_sha256) = 64),
  source_revision_sha256 TEXT NOT NULL CHECK(length(source_revision_sha256) = 64),
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, source_path, reference_key),
  FOREIGN KEY(project_id, source_path)
    REFERENCES brownfield_source_cache(project_id, relative_path) ON DELETE CASCADE
);

INSERT INTO brownfield_edges(
  project_id,source_path,reference_key,relation,observed_text,line,
  destination_path,resolution,resolution_reason,candidate_count,
  source_revision_sha256,updated_at
)
SELECT
  project_id,
  source_path,
  printf('legacy:%s:%d:%s',relation,line,hex(observed_text)),
  relation,
  observed_text,
  line,
  NULL,
  'unresolved',
  'legacy_v14_requires_reresolution',
  0,
  source_revision_sha256,
  updated_at
FROM brownfield_edges_v14;

DROP TABLE brownfield_edges_v14;

CREATE TABLE brownfield_symbols (
  project_id TEXT NOT NULL,
  source_path TEXT NOT NULL,
  symbol_key TEXT NOT NULL,
  language TEXT NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN ('function','method','struct','class','trait','interface','enum','type_alias')),
  name TEXT NOT NULL,
  qualified_name TEXT NOT NULL,
  owner_symbol_key TEXT,
  visibility TEXT NOT NULL CHECK(visibility IN ('unknown','private','project','public','exported')),
  signature TEXT NOT NULL,
  start_line INTEGER NOT NULL CHECK(start_line > 0),
  end_line INTEGER NOT NULL CHECK(end_line >= start_line),
  source_revision_sha256 TEXT NOT NULL CHECK(length(source_revision_sha256) = 64),
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, source_path, symbol_key),
  FOREIGN KEY(project_id, source_path)
    REFERENCES brownfield_source_cache(project_id, relative_path) ON DELETE CASCADE
);

CREATE TABLE brownfield_resolution_state (
  project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  resolver_version TEXT NOT NULL,
  resolution_context_sha256 TEXT NOT NULL CHECK(length(resolution_context_sha256) = 64),
  state TEXT NOT NULL CHECK(state IN ('pending','running','partial','complete','failed')),
  cursor_reference_key TEXT,
  examined_edges INTEGER NOT NULL DEFAULT 0 CHECK(examined_edges >= 0),
  resolved_edges INTEGER NOT NULL DEFAULT 0 CHECK(resolved_edges >= 0),
  ambiguous_edges INTEGER NOT NULL DEFAULT 0 CHECK(ambiguous_edges >= 0),
  external_edges INTEGER NOT NULL DEFAULT 0 CHECK(external_edges >= 0),
  unresolved_edges INTEGER NOT NULL DEFAULT 0 CHECK(unresolved_edges >= 0),
  elapsed_us INTEGER NOT NULL DEFAULT 0 CHECK(elapsed_us >= 0),
  diagnostic TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX idx_brownfield_edges_destination
  ON brownfield_edges(project_id, destination_path, destination_symbol_key);
CREATE INDEX idx_brownfield_edges_source_revision
  ON brownfield_edges(project_id, source_path, source_revision_sha256);
CREATE INDEX idx_brownfield_edges_resolution
  ON brownfield_edges(project_id, resolution, relation);
CREATE INDEX idx_brownfield_symbols_name
  ON brownfield_symbols(project_id, name, source_path);
CREATE INDEX idx_brownfield_symbols_qualified
  ON brownfield_symbols(project_id, qualified_name);
