-- Longest wait between two reads of a CLI run's pipes (stdout or stderr),
-- from spawn to exit. The CLI observation trial (ADR-025) reports it next to
-- the audited frame silence; runs before this migration stay NULL.
ALTER TABLE terminal_runs
  ADD COLUMN max_transport_silence_ms INTEGER
  CHECK(max_transport_silence_ms IS NULL OR max_transport_silence_ms >= 0);
