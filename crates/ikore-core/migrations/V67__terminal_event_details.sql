-- ADR-040 M1: the structured detail of an action or withholding event
-- (`{"v":1,"type":"action_call",...}`, never free text) and when its frame
-- reached the host. Additive: older events keep both columns NULL.
-- V66 is taken on another branch; the number is settled at integration.
ALTER TABLE terminal_events ADD COLUMN detail_json TEXT
  CHECK(detail_json IS NULL OR json_valid(detail_json));
ALTER TABLE terminal_events ADD COLUMN observed_at TEXT;
