-- Signed local attestation of finished pulsations (ADR-036 M3).
--
-- Additive: four new tables, no rebuild. Keys are global, not a project's, and
-- a key an attestation names can never be deleted (the foreign key has no
-- delete action). Intents, attestations and verifications go with their
-- pulsation, and so with its project. An attestation never changes once
-- sealed, except that an unsigned one may be signed later: the trigger allows
-- that one transition, with every other column kept.

CREATE TABLE attestation_keys (
  key_id TEXT PRIMARY KEY CHECK(key_id LIKE 'hvc-ed25519:%' AND length(key_id) = 76),
  algorithm TEXT NOT NULL CHECK(algorithm = 'ed25519'),
  public_key TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('active','retired')),
  created_at TEXT NOT NULL,
  retired_at TEXT,
  CHECK((state = 'retired') = (retired_at IS NOT NULL))
);

CREATE UNIQUE INDEX idx_attestation_keys_one_active
  ON attestation_keys(state) WHERE state = 'active';

-- Created with the terminal transition; sealing runs from it, so a restart
-- between the two resumes. `failed` keeps the host error instead of retrying
-- a derivation that cannot succeed.
CREATE TABLE attestation_seal_intents (
  pulsation_id TEXT PRIMARY KEY REFERENCES pulsations(id) ON DELETE CASCADE,
  state TEXT NOT NULL CHECK(state IN ('pending','sealed','skipped','failed')),
  diagnostic TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK((state = 'failed') = (diagnostic IS NOT NULL))
);

CREATE INDEX idx_attestation_seal_intents_state
  ON attestation_seal_intents(state, created_at);

CREATE TABLE pulsation_attestations (
  pulsation_id TEXT PRIMARY KEY REFERENCES pulsations(id) ON DELETE CASCADE,
  schema_version INTEGER NOT NULL CHECK(schema_version = 1),
  predicate_type TEXT NOT NULL CHECK(predicate_type = 'urn:hvc:pulsation-attestation:v1'),
  deriver_version INTEGER NOT NULL CHECK(deriver_version >= 1),
  statement_json TEXT CHECK(statement_json IS NULL OR json_valid(statement_json)),
  statement_sha256 TEXT NOT NULL CHECK(length(statement_sha256) = 64),
  envelope_json TEXT CHECK(envelope_json IS NULL OR json_valid(envelope_json)),
  key_id TEXT REFERENCES attestation_keys(key_id),
  signature_state TEXT NOT NULL CHECK(signature_state IN ('signed','unsigned','too_large')),
  outcome TEXT NOT NULL CHECK(outcome IN ('done','failed','rejected','aborted')),
  subject_count INTEGER NOT NULL CHECK(subject_count >= 0),
  sealed_at TEXT NOT NULL,
  signed_at TEXT,
  CHECK((signature_state = 'signed')
    = (envelope_json IS NOT NULL AND key_id IS NOT NULL AND signed_at IS NOT NULL)),
  CHECK((signature_state = 'too_large') = (statement_json IS NULL))
);

CREATE TRIGGER pulsation_attestations_sign_only BEFORE UPDATE ON pulsation_attestations
WHEN NOT (
  OLD.signature_state = 'unsigned' AND NEW.signature_state = 'signed'
  AND NEW.pulsation_id = OLD.pulsation_id
  AND NEW.schema_version = OLD.schema_version
  AND NEW.predicate_type = OLD.predicate_type
  AND NEW.deriver_version = OLD.deriver_version
  AND NEW.statement_json = OLD.statement_json
  AND NEW.statement_sha256 = OLD.statement_sha256
  AND NEW.outcome = OLD.outcome
  AND NEW.subject_count = OLD.subject_count
  AND NEW.sealed_at = OLD.sealed_at
)
BEGIN SELECT RAISE(ABORT, 'attestations are immutable; only an unsigned one can be signed'); END;

-- The last check of an attestation, kept for display; it carries no
-- evidential weight and is recomputed on request.
CREATE TABLE attestation_verifications (
  pulsation_id TEXT PRIMARY KEY REFERENCES pulsations(id) ON DELETE CASCADE,
  checked_at TEXT NOT NULL,
  signature TEXT NOT NULL CHECK(signature IN ('valid','invalid','unsigned','unknown_key','too_large')),
  rederivation TEXT NOT NULL CHECK(rederivation IN ('matches','differs','unavailable')),
  difference TEXT,
  workspace_json TEXT NOT NULL CHECK(json_valid(workspace_json))
);
