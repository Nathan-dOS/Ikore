-- A failed Feature Forge turn used to keep only the raw error string, so the
-- Incubation chat could print nothing better than that string. The typed
-- failure (kind, subject, redacted technical detail) is what the UI explains
-- and what the next turn's prompt reports to the agent. `notices_json` holds
-- the plain-language notes the host attaches when it repairs a slip in an
-- advisory field instead of rejecting the whole reply.
--
-- Both columns are nullable: rows written before this migration keep their
-- `error` text and are classified from it on read.
ALTER TABLE forge_turns ADD COLUMN failure_json TEXT CHECK(failure_json IS NULL OR json_valid(failure_json));
ALTER TABLE forge_turns ADD COLUMN notices_json TEXT CHECK(notices_json IS NULL OR json_valid(notices_json));
