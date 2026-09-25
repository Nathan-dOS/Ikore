-- Distinguish a model reported in a public CLI event from the effective
-- per-turn model recorded by Codex in its own session rollout.
ALTER TABLE terminal_model_observations
  ADD COLUMN evidence_source TEXT NOT NULL DEFAULT 'stream_event'
  CHECK(evidence_source IN ('stream_event', 'codex_turn_context'));
