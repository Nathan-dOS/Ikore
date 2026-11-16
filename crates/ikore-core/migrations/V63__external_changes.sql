-- External changes on resume (ADR-037 M3).
--
-- Additive: four new tables, no rebuild and no change to an existing table.
-- A resumable run keeps the manifest of the snapshot it ran on and, for small
-- text files, the sanitized content it saw, so a later resume can tell the
-- agent what changed. Blobs are shared per project and removed when
-- no manifest points at them. A Companion read of a repository file records
-- the digest it read, so a later turn can say the file changed.

CREATE TABLE terminal_run_snapshot_files (
  run_id TEXT NOT NULL REFERENCES terminal_runs(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL CHECK(length(relative_path) BETWEEN 1 AND 1024),
  original_sha256 TEXT NOT NULL CHECK(length(original_sha256) = 64),
  projected_sha256 TEXT NOT NULL CHECK(length(projected_sha256) = 64),
  byte_len INTEGER NOT NULL CHECK(byte_len >= 0),
  -- The sanitized content kept for hunks, when the file is small text and
  -- the run's blob budget allows it.
  blob_sha256 TEXT CHECK(blob_sha256 IS NULL OR length(blob_sha256) = 64),
  PRIMARY KEY(run_id, relative_path)
);

CREATE TABLE terminal_snapshot_blobs (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  sha256 TEXT NOT NULL CHECK(length(sha256) = 64),
  content TEXT NOT NULL,
  byte_len INTEGER NOT NULL CHECK(byte_len BETWEEN 0 AND 32768),
  created_at TEXT NOT NULL,
  PRIMARY KEY(project_id, sha256)
);

CREATE TABLE chat_tool_file_observations (
  tool_call_id TEXT NOT NULL REFERENCES chat_tool_calls(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL CHECK(length(relative_path) BETWEEN 1 AND 1024),
  content_sha256 TEXT NOT NULL CHECK(length(content_sha256) = 64),
  observed_at TEXT NOT NULL,
  PRIMARY KEY(tool_call_id, relative_path)
);

-- A run that records a manifest (fingerprint version 2) and, when it resumed
-- a session, the digest of the change list its notice described. A run with
-- no row is version 1: its sessions resume only on an unchanged workspace.
CREATE TABLE terminal_run_resume_state (
  run_id TEXT PRIMARY KEY REFERENCES terminal_runs(id) ON DELETE CASCADE,
  session_fingerprint_version INTEGER NOT NULL CHECK(session_fingerprint_version IN (1, 2)),
  external_changes_sha256 TEXT
    CHECK(external_changes_sha256 IS NULL OR length(external_changes_sha256) = 64)
);
