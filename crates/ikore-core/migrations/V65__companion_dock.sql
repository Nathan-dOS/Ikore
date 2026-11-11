-- Companion UI state is written only by explicit interaction, never by a
-- pipeline transition. Drafts are not sent messages or approval evidence.
CREATE TABLE companion_dock_sessions (
  project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  revision INTEGER NOT NULL CHECK(revision >= 1),
  layout_json TEXT NOT NULL CHECK(json_valid(layout_json)),
  updated_at TEXT NOT NULL
);

CREATE TABLE companion_drafts (
  topic_id TEXT NOT NULL,
  conversation_id TEXT REFERENCES chat_conversations(id) ON DELETE CASCADE,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  revision INTEGER NOT NULL CHECK(revision >= 1),
  content TEXT NOT NULL CHECK(length(content) <= 20000),
  decision_id TEXT,
  decision_state_version INTEGER,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id,topic_id)
);
CREATE INDEX companion_drafts_project ON companion_drafts(project_id);

CREATE TABLE decision_conversations (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  decision_id TEXT NOT NULL,
  source_kind TEXT NOT NULL,
  source_id TEXT NOT NULL,
  conversation_id TEXT NOT NULL UNIQUE REFERENCES chat_conversations(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL,
  PRIMARY KEY(project_id, decision_id)
);

-- Retain both the historical binding and its generalized lookup.
INSERT INTO decision_conversations(project_id,decision_id,source_kind,source_id,conversation_id,created_at)
SELECT c.project_id,'checkpoint:' || q.checkpoint_id,'clarification',q.id,r.conversation_id,r.created_at
FROM specification_resolution_conversations r
JOIN specification_question_sets q ON q.id=r.question_set_id
JOIN chat_conversations c ON c.id=r.conversation_id;
