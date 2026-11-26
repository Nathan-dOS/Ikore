-- Feature 08 M1: versioned project git rules (off by default, G8).
CREATE TABLE project_git_rules (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  version INTEGER NOT NULL CHECK(version > 0),
  enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0,1)),
  rules_json TEXT NOT NULL CHECK(json_valid(rules_json)),
  rules_sha256 TEXT NOT NULL CHECK(length(rules_sha256) = 64),
  created_at TEXT NOT NULL,
  PRIMARY KEY(project_id, version)
);
