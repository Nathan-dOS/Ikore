-- Durable exchange log (ADR-037 M2).
--
-- Additive: two new tables, no rebuild and no change to an existing table.
-- The log keeps references, not copies: each item names the domain row that
-- holds the content (`payload_table`, `payload_id`). An item is written in
-- the transaction of its domain write and never changes afterwards. Streams
-- and items go with their project; a deleted pulsation takes its own stream
-- (explicit cleanup in the host, since the subject is not a foreign key).

CREATE TABLE exchange_streams (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK(kind IN ('pulsation','stage_execution','terminal_run',
                                    'chat_conversation','forge','notes','drawings','docs')),
  subject_id TEXT NOT NULL CHECK(length(subject_id) BETWEEN 1 AND 200),
  next_seq INTEGER NOT NULL DEFAULT 1 CHECK(next_seq >= 1),
  created_at TEXT NOT NULL,
  UNIQUE(project_id, kind, subject_id)
);

-- `global_seq` orders every item of the database; a project's items are a
-- subsequence of it, read through `exchange_items_by_project`. `kind` holds a
-- log record kind and has no CHECK, so a new kind never rebuilds the table.
CREATE TABLE exchange_items (
  global_seq INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  stream_id TEXT NOT NULL REFERENCES exchange_streams(id) ON DELETE CASCADE,
  seq INTEGER NOT NULL CHECK(seq >= 1),
  kind TEXT NOT NULL CHECK(length(kind) BETWEEN 1 AND 64),
  producer_kind TEXT NOT NULL CHECK(producer_kind IN ('host','human','agent','tool','repository','external')),
  producer_ref TEXT CHECK(producer_ref IS NULL OR length(producer_ref) BETWEEN 1 AND 160),
  trust TEXT NOT NULL CHECK(trust IN ('host','human','agent_output','tool_output','repository')),
  content_sha256 TEXT CHECK(content_sha256 IS NULL OR length(content_sha256) = 64),
  payload_table TEXT CHECK(payload_table IS NULL OR length(payload_table) BETWEEN 1 AND 64),
  payload_id TEXT CHECK(payload_id IS NULL OR length(payload_id) BETWEEN 1 AND 200),
  idempotency_key TEXT NOT NULL CHECK(length(idempotency_key) BETWEEN 1 AND 200),
  created_at TEXT NOT NULL,
  UNIQUE(stream_id, seq),
  UNIQUE(stream_id, idempotency_key),
  CHECK((payload_table IS NULL) = (payload_id IS NULL))
);

CREATE INDEX exchange_items_by_project ON exchange_items(project_id, global_seq);

-- Items are append-only.
CREATE TRIGGER exchange_items_append_only
BEFORE UPDATE ON exchange_items
BEGIN
  SELECT RAISE(ABORT, 'exchange items are append-only');
END;
