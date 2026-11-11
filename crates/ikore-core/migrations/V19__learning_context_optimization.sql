CREATE TABLE context_retrieval_runs (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  pulsation_id TEXT REFERENCES pulsations(id) ON DELETE CASCADE,
  job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
  agent_run_id TEXT NOT NULL UNIQUE REFERENCES agent_runs(id) ON DELETE CASCADE,
  stage_name TEXT NOT NULL,
  requested_strategy TEXT NOT NULL,
  strategy_used TEXT NOT NULL,
  adaptive_mode TEXT NOT NULL CHECK(adaptive_mode IN ('off','shadow','on')),
  expansion_depth INTEGER,
  fallback_used INTEGER NOT NULL DEFAULT 0 CHECK(fallback_used IN (0,1)),
  max_similarity REAL,
  duration_ms INTEGER NOT NULL DEFAULT 0,
  considered_total INTEGER NOT NULL DEFAULT 0,
  delivered_total INTEGER NOT NULL DEFAULT 0,
  cited_total INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE context_retrieval_items (
  id TEXT PRIMARY KEY,
  retrieval_run_id TEXT NOT NULL REFERENCES context_retrieval_runs(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL,
  item_kind TEXT NOT NULL DEFAULT 'note' CHECK(item_kind IN ('note','exact_code_context')),
  considered INTEGER NOT NULL DEFAULT 1 CHECK(considered IN (0,1)),
  delivered INTEGER NOT NULL DEFAULT 0 CHECK(delivered IN (0,1)),
  cited INTEGER NOT NULL DEFAULT 0 CHECK(cited IN (0,1)),
  content_sha256 TEXT,
  rank INTEGER NOT NULL,
  graph_score INTEGER,
  semantic_similarity REAL,
  final_score INTEGER,
  delivered_chars INTEGER NOT NULL DEFAULT 0,
  citation_reason TEXT,
  UNIQUE(retrieval_run_id,relative_path)
);

CREATE TABLE learning_context_assessments (
  id TEXT PRIMARY KEY,
  learning_job_id TEXT NOT NULL REFERENCES learning_runs(job_id) ON DELETE CASCADE,
  report_version INTEGER NOT NULL DEFAULT 2 CHECK(report_version >= 2),
  retrieval_run_id TEXT NOT NULL REFERENCES context_retrieval_runs(id) ON DELETE CASCADE,
  verdict TEXT NOT NULL CHECK(verdict IN ('material','supporting','no_observable_use','indeterminate')),
  evidence_json TEXT NOT NULL DEFAULT '[]',
  rationale TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(learning_job_id,retrieval_run_id,report_version)
);

CREATE TABLE learning_graph_proposals (
  id TEXT PRIMARY KEY,
  learning_job_id TEXT NOT NULL REFERENCES learning_runs(job_id) ON DELETE CASCADE,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  candidate_id TEXT NOT NULL,
  source_path TEXT NOT NULL,
  target_path TEXT NOT NULL,
  relation TEXT NOT NULL CHECK(relation IN ('tested-by','contracts','affects','wikilink')),
  source_expected_sha256 TEXT NOT NULL,
  target_expected_sha256 TEXT NOT NULL,
  preview_diff TEXT NOT NULL,
  rationale TEXT NOT NULL,
  evidence_json TEXT NOT NULL DEFAULT '[]',
  decision TEXT NOT NULL DEFAULT 'pending' CHECK(decision IN ('pending','approved','rejected')),
  decision_generation INTEGER NOT NULL DEFAULT 0,
  publication_state TEXT NOT NULL DEFAULT 'not_requested' CHECK(publication_state IN ('not_requested','pending','applying','applied','stale','conflict','cancelled')),
  artifact_id TEXT REFERENCES artifacts(id) ON DELETE SET NULL,
  diagnostic TEXT,
  decided_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(learning_job_id,candidate_id)
);

CREATE INDEX idx_context_retrieval_pulse_stage
  ON context_retrieval_runs(project_id,pulsation_id,stage_name,created_at);
CREATE INDEX idx_context_retrieval_items_run_rank
  ON context_retrieval_items(retrieval_run_id,rank);
CREATE INDEX idx_learning_assessments_job
  ON learning_context_assessments(learning_job_id,report_version);
CREATE INDEX idx_learning_graph_review
  ON learning_graph_proposals(project_id,decision,publication_state,created_at);

INSERT OR IGNORE INTO runtime_settings(key,value,updated_at)
VALUES('orchestration.learning_refinement_mode','observe',CURRENT_TIMESTAMP);
