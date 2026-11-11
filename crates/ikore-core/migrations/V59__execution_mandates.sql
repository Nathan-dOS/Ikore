-- Execution mandates and approval receipts (ADR-035 M1).
--
-- Additive: three new tables, no rebuild. Mandate revisions and receipts are
-- only ever appended, so a trigger aborts any UPDATE; rows still go with their
-- pulsation (and so with their project) through ON DELETE CASCADE. The
-- checkpoint a row names is kept as plain text: a foreign key with SET NULL
-- would be an UPDATE the triggers refuse.

CREATE TABLE execution_mandates (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  revision INTEGER NOT NULL CHECK(revision >= 1),
  cause TEXT NOT NULL CHECK(cause IN ('issued','reissued','scope_grant','transport_fallback')),
  enforcement TEXT NOT NULL CHECK(enforcement IN ('enforced_v4','not_applied_legacy')),
  mandate_json TEXT NOT NULL CHECK(json_valid(mandate_json)),
  mandate_sha256 TEXT NOT NULL CHECK(length(mandate_sha256) = 64),
  previous_sha256 TEXT CHECK(previous_sha256 IS NULL OR length(previous_sha256) = 64),
  checkpoint_id TEXT,
  created_at TEXT NOT NULL,
  UNIQUE(pulsation_id, revision),
  CHECK((revision = 1) = (previous_sha256 IS NULL))
);

CREATE TRIGGER execution_mandates_append_only BEFORE UPDATE ON execution_mandates
BEGIN SELECT RAISE(ABORT, 'execution mandates are append-only'); END;

CREATE TABLE checkpoint_approval_receipts (
  checkpoint_id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  purpose TEXT NOT NULL CHECK(purpose IN ('artifact_review','clarification','cli_fallback','stage_failure','path_confirmation')),
  decision TEXT NOT NULL CHECK(decision IN (
    'approved','rejected','revision_requested',
    'retry_stage','request_revision','abort','grant_scope',
    'use_api','retry_cli','cancel',
    'confirmed','answered')),
  receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
  receipt_sha256 TEXT NOT NULL CHECK(length(receipt_sha256) = 64),
  mandate_revision INTEGER CHECK(mandate_revision IS NULL OR mandate_revision >= 1),
  created_at TEXT NOT NULL
);

CREATE INDEX idx_checkpoint_approval_receipts_pulsation
  ON checkpoint_approval_receipts(pulsation_id, created_at);

CREATE TRIGGER checkpoint_approval_receipts_append_only BEFORE UPDATE ON checkpoint_approval_receipts
BEGIN SELECT RAISE(ABORT, 'approval receipts are append-only'); END;

-- Writes and dispatches the mandate refused. Bounded by the host: past 200
-- rows per pulsation only the newest row's counter grows.
CREATE TABLE mandate_enforcement_events (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  stage TEXT NOT NULL,
  rule TEXT NOT NULL CHECK(rule IN ('scope_violation','capability_mismatch')),
  relative_path TEXT,
  detail TEXT NOT NULL,
  mandate_revision INTEGER NOT NULL CHECK(mandate_revision >= 1),
  occurrences INTEGER NOT NULL DEFAULT 1 CHECK(occurrences >= 1),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE INDEX idx_mandate_enforcement_events_pulsation
  ON mandate_enforcement_events(pulsation_id, created_at);
