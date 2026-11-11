-- Just-in-time scope requests (ADR-035 M4).
--
-- Additive: one table, no rebuild and no change to an existing table. A
-- request names the failure checkpoint it parked on (one request per
-- checkpoint) and goes with its pulsation. It is decided once (granted,
-- declined or found stale); a granted request is later superseded by a
-- revision of its stage. Nothing else about it ever changes.

CREATE TABLE mandate_scope_requests (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  stage_execution_id TEXT NOT NULL REFERENCES stage_executions(id) ON DELETE CASCADE,
  budget_account_id TEXT NOT NULL,
  unit_id TEXT NOT NULL,
  unit_key TEXT NOT NULL,
  checkpoint_id TEXT NOT NULL UNIQUE REFERENCES checkpoints(id) ON DELETE CASCADE,
  stage TEXT NOT NULL CHECK(stage IN ('Code','Test')),
  related_refs_json TEXT NOT NULL CHECK(json_valid(related_refs_json)),
  accepted_json TEXT NOT NULL CHECK(json_valid(accepted_json)),
  refused_json TEXT NOT NULL CHECK(json_valid(refused_json)),
  derivation_json TEXT NOT NULL CHECK(json_valid(derivation_json)),
  derivation_sha256 TEXT NOT NULL CHECK(length(derivation_sha256) = 64),
  risk_before TEXT NOT NULL CHECK(risk_before IN ('low','medium','high','unknown')),
  risk_after TEXT NOT NULL CHECK(risk_after IN ('low','medium','high','unknown')),
  agent_message TEXT CHECK(agent_message IS NULL OR length(agent_message) <= 400),
  agent_message_sha256 TEXT CHECK(agent_message_sha256 IS NULL OR length(agent_message_sha256) = 64),
  state TEXT NOT NULL CHECK(state IN ('pending','granted','declined','stale','superseded')),
  -- The checkpoint decision that settled it: `grant_scope` for a grant, the
  -- chosen action for a decline.
  decision TEXT CHECK(decision IS NULL OR decision IN ('grant_scope','retry_stage','request_revision','abort')),
  -- The mandate revision a grant issued.
  mandate_revision INTEGER CHECK(mandate_revision IS NULL OR mandate_revision >= 1),
  decided_at TEXT,
  created_at TEXT NOT NULL,
  CHECK((state = 'pending') = (decided_at IS NULL)),
  CHECK((decision IS 'grant_scope') = (mandate_revision IS NOT NULL)),
  CHECK(state <> 'granted' OR decision IS 'grant_scope'),
  CHECK(state <> 'declined' OR decision IN ('retry_stage','request_revision','abort')),
  CHECK(state NOT IN ('pending','stale') OR decision IS NULL)
);

CREATE INDEX idx_mandate_scope_requests_stage
  ON mandate_scope_requests(pulsation_id, stage, state);
CREATE INDEX idx_mandate_scope_requests_account
  ON mandate_scope_requests(budget_account_id);

CREATE TRIGGER mandate_scope_requests_decided_once BEFORE UPDATE ON mandate_scope_requests
WHEN NOT (
  (OLD.state = 'pending' AND NEW.state <> 'pending'
    OR OLD.state = 'granted' AND NEW.state = 'superseded')
  AND NEW.id = OLD.id
  AND NEW.pulsation_id = OLD.pulsation_id
  AND NEW.stage_execution_id = OLD.stage_execution_id
  AND NEW.unit_key = OLD.unit_key
  AND NEW.checkpoint_id = OLD.checkpoint_id
  AND NEW.accepted_json = OLD.accepted_json
  AND NEW.refused_json = OLD.refused_json
  AND NEW.derivation_json = OLD.derivation_json
  AND NEW.derivation_sha256 = OLD.derivation_sha256
  AND NEW.created_at = OLD.created_at
  AND (OLD.state = 'pending' OR (NEW.decision IS OLD.decision AND NEW.mandate_revision IS OLD.mandate_revision))
)
BEGIN SELECT RAISE(ABORT, 'scope requests are decided once'); END;
