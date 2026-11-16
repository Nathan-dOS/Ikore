-- ADR-022 clarification loop (P4/A4.1): the developer's free-form
-- conversation with the Companion while resolving a Specification question
-- set, distinct from the host-authored/host-presented `specification_conversations`
-- row (which only ever holds the rendered questions/answers, no LLM turns).
-- Keyed by question_set_id, not pulsation_id, because a pulsation can accumulate
-- more than one question set across Specification retries, and each one gets
-- its own resolution conversation (the earlier ones stay read-only history).
CREATE TABLE specification_resolution_conversations (
  question_set_id TEXT PRIMARY KEY REFERENCES specification_question_sets(id) ON DELETE CASCADE,
  conversation_id TEXT NOT NULL UNIQUE REFERENCES chat_conversations(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL
);
CREATE INDEX idx_spec_resolution_conv ON specification_resolution_conversations(conversation_id);
