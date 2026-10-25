-- ADR-034 M6: pre-flight architectural conformance in Forge triage.
-- Additive: one new table, no rebuild of an existing table.

-- What one triage found when it compared a feature revision with the
-- project's accepted decisions, and how the developer resolved a blocking
-- conflict. The decisions are recorded as paths, states and hashes only.
CREATE TABLE forge_preflight_findings (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  feature_id TEXT NOT NULL REFERENCES forge_features(id) ON DELETE CASCADE,
  feature_revision INTEGER NOT NULL CHECK(feature_revision>=1),
  turn_id TEXT,
  inputs_state TEXT NOT NULL CHECK(inputs_state IN ('absent','loaded','partial','invalid','over_budget')),
  inputs_json TEXT NOT NULL CHECK(json_valid(inputs_json)),
  inputs_sha256 TEXT NOT NULL,
  conflicts_json TEXT NOT NULL CHECK(json_valid(conflicts_json)),
  blocking INTEGER NOT NULL CHECK(blocking>=0),
  resolution TEXT NOT NULL CHECK(resolution IN ('none','pending','adjusted','adr_proposed','dismissed')),
  reason TEXT CHECK(reason IS NULL OR length(reason) BETWEEN 1 AND 400),
  proposal_feature_id TEXT,
  created_at TEXT NOT NULL,
  resolved_at TEXT,
  CHECK((blocking=0)=(resolution='none')),
  CHECK((resolution='dismissed')=(reason IS NOT NULL))
);
CREATE INDEX forge_preflight_findings_feature
  ON forge_preflight_findings(feature_id,feature_revision,created_at);
