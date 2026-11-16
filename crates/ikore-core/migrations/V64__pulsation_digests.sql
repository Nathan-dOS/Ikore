-- Pulsation digests for the Companion (ADR-037 M4).
--
-- Additive: two tables, one FTS5 index over the digests and its triggers,
-- no rebuild. A digest is derived by the host from a finished pulsation's
-- rows, without a model, and goes with its pulsation and its project. The
-- paths a pulsation published are kept apart so a search can filter on
-- them.

CREATE TABLE companion_pulsation_digests (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL UNIQUE REFERENCES pulsations(id) ON DELETE CASCADE,
  digest_version INTEGER NOT NULL CHECK(digest_version >= 1),
  title TEXT NOT NULL,
  content TEXT NOT NULL CHECK(length(CAST(content AS BLOB)) <= 65536),
  outcome TEXT NOT NULL CHECK(outcome IN ('done','failed')),
  ended_at TEXT NOT NULL,
  content_sha256 TEXT NOT NULL CHECK(length(content_sha256) = 64),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX companion_pulsation_digests_by_project
  ON companion_pulsation_digests(project_id, ended_at DESC);

CREATE VIRTUAL TABLE companion_pulsation_digest_fts USING fts5(
  title,
  content,
  content='companion_pulsation_digests',
  content_rowid='id',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER companion_pulsation_digests_ai AFTER INSERT ON companion_pulsation_digests BEGIN
  INSERT INTO companion_pulsation_digest_fts(rowid,title,content)
  VALUES(new.id,new.title,new.content);
END;

CREATE TRIGGER companion_pulsation_digests_ad AFTER DELETE ON companion_pulsation_digests BEGIN
  INSERT INTO companion_pulsation_digest_fts(companion_pulsation_digest_fts,rowid,title,content)
  VALUES('delete',old.id,old.title,old.content);
END;

CREATE TRIGGER companion_pulsation_digests_au AFTER UPDATE ON companion_pulsation_digests BEGIN
  INSERT INTO companion_pulsation_digest_fts(companion_pulsation_digest_fts,rowid,title,content)
  VALUES('delete',old.id,old.title,old.content);
  INSERT INTO companion_pulsation_digest_fts(rowid,title,content)
  VALUES(new.id,new.title,new.content);
END;

CREATE TABLE companion_pulsation_paths (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  relative_path TEXT NOT NULL CHECK(length(relative_path) BETWEEN 1 AND 1024),
  effect TEXT NOT NULL CHECK(effect IN ('modified','created','deleted')),
  PRIMARY KEY(pulsation_id, relative_path)
);

CREATE INDEX companion_pulsation_paths_by_path
  ON companion_pulsation_paths(relative_path);
