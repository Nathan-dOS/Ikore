CREATE TABLE brownfield_component_notes (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  proposal_artifact_id TEXT NOT NULL UNIQUE REFERENCES artifacts(id),
  publication_artifact_id TEXT UNIQUE REFERENCES artifacts(id),
  component_id TEXT NOT NULL,
  title TEXT NOT NULL,
  relative_path TEXT NOT NULL,
  mapping_status TEXT NOT NULL CHECK(mapping_status IN ('proposed','validated','stale')),
  publication_state TEXT NOT NULL CHECK(publication_state IN ('not_scheduled','pending','publishing','published','conflict')),
  generator TEXT NOT NULL,
  generator_version TEXT NOT NULL,
  confidence INTEGER NOT NULL CHECK(confidence BETWEEN 0 AND 100),
  diagnostic TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(project_id, component_id),
  UNIQUE(project_id, relative_path)
);

CREATE TABLE brownfield_component_sources (
  note_id TEXT NOT NULL REFERENCES brownfield_component_notes(id) ON DELETE CASCADE,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL,
  observed_revision_sha256 TEXT NOT NULL CHECK(length(observed_revision_sha256) = 64),
  PRIMARY KEY(note_id, relative_path)
);

CREATE INDEX idx_brownfield_component_sources_revision
  ON brownfield_component_sources(project_id, relative_path, observed_revision_sha256);

CREATE TABLE brownfield_enrichment_runs (
  job_id TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  state TEXT NOT NULL CHECK(state IN ('pending','running','budget_exhausted','completed','partial','failed','cancelled')),
  expansion_radius INTEGER NOT NULL CHECK(expansion_radius IN (1,2)),
  budget_limit INTEGER NOT NULL CHECK(budget_limit BETWEEN 0 AND 8),
  budget_calls INTEGER NOT NULL DEFAULT 0 CHECK(budget_calls BETWEEN 0 AND 8),
  max_proposals INTEGER NOT NULL DEFAULT 2 CHECK(max_proposals BETWEEN 1 AND 2),
  max_tokens_per_note INTEGER NOT NULL DEFAULT 1200 CHECK(max_tokens_per_note BETWEEN 1 AND 1200),
  seed_paths_json TEXT NOT NULL CHECK(json_valid(seed_paths_json)),
  candidate_paths_json TEXT CHECK(candidate_paths_json IS NULL OR json_valid(candidate_paths_json)),
  input_json TEXT CHECK(input_json IS NULL OR json_valid(input_json)),
  output_json TEXT CHECK(output_json IS NULL OR json_valid(output_json)),
  proposal_count INTEGER NOT NULL DEFAULT 0 CHECK(proposal_count BETWEEN 0 AND 2),
  diagnostic TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(pulsation_id)
);

CREATE INDEX idx_brownfield_enrichment_admission
  ON brownfield_enrichment_runs(state, created_at, job_id);

INSERT OR IGNORE INTO runtime_settings(key,value,updated_at)
VALUES('brownfield_llm_budget','1',CURRENT_TIMESTAMP);

INSERT OR IGNORE INTO runtime_settings(key,value,updated_at)
VALUES('brownfield_expansion_radius','1',CURRENT_TIMESTAMP);
