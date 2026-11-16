CREATE TABLE brownfield_frontier (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending'
    CHECK(status IN ('pending','in_progress','partial','mapped','stale','ignored','failed')),
  content_sha256 TEXT CHECK(content_sha256 IS NULL OR length(content_sha256) = 64),
  in_degree INTEGER NOT NULL DEFAULT 0 CHECK(in_degree >= 0),
  priority_reason TEXT NOT NULL DEFAULT 'discovered',
  recent_use_count INTEGER NOT NULL DEFAULT 0 CHECK(recent_use_count BETWEEN 0 AND 8),
  age_factor INTEGER NOT NULL DEFAULT 0 CHECK(age_factor >= 0),
  budget_consumed INTEGER NOT NULL DEFAULT 0 CHECK(budget_consumed >= 0),
  attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
  last_used_at TEXT,
  priority_since TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, relative_path)
);

CREATE INDEX idx_brownfield_frontier_admission
  ON brownfield_frontier(project_id, status, recent_use_count, in_degree, age_factor, priority_since);

CREATE TABLE brownfield_frontier_state (
  project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  initialized INTEGER NOT NULL DEFAULT 0 CHECK(initialized IN (0,1)),
  initialized_at TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE brownfield_refresh_queue (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL,
  content_revision_key TEXT NOT NULL,
  content_sha256 TEXT CHECK(content_sha256 IS NULL OR length(content_sha256) = 64),
  event_kind TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'pending'
    CHECK(state IN ('pending','in_progress','done','failed','ignored','superseded')),
  attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
  diagnostic TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id, relative_path, content_revision_key)
);

CREATE INDEX idx_brownfield_refresh_admission
  ON brownfield_refresh_queue(state, created_at, project_id, relative_path);

-- Existing BF2 caches become the initial derived frontier without touching the filesystem.
INSERT INTO brownfield_frontier(
  project_id, relative_path, status, content_sha256, priority_reason,
  priority_since, created_at, updated_at
)
SELECT project_id, relative_path,
       CASE WHEN status = 'mapped' THEN 'mapped' ELSE 'partial' END,
       content_sha256, 'migrated_from_structural_cache', updated_at, updated_at, updated_at
FROM brownfield_source_cache;

INSERT INTO brownfield_frontier_state(project_id, initialized, initialized_at, updated_at)
SELECT DISTINCT project_id, 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP
FROM brownfield_source_cache;
