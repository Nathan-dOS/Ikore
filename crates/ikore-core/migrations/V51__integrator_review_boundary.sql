-- ADR-032: the Integrator's evidence boundary and the human convergence policy.
-- Additive: three new tables, no rebuild of an existing table.

-- The bytes each path held before this pulsation first published it. The
-- first capture wins, so later revision rounds keep the pre-pulsation base.
-- Content is kept only for small files and cleared when the pulsation ends.
CREATE TABLE pulsation_file_bases (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  path_key TEXT NOT NULL,
  relative_path TEXT NOT NULL,
  state TEXT NOT NULL CHECK(state IN ('absent','present','present_unstored')),
  content_sha256 TEXT,
  content TEXT,
  byte_len INTEGER CHECK(byte_len IS NULL OR byte_len>=0),
  reason TEXT CHECK(reason IS NULL OR reason IN ('too_large','replayed','unreadable')),
  first_artifact_id TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
  captured_at TEXT NOT NULL,
  PRIMARY KEY(pulsation_id,path_key),
  CHECK((state='absent')=(content_sha256 IS NULL)),
  CHECK(content IS NULL OR state='present')
);

-- The classified inputs of one Integrator execution: canonical manifest
-- without execution identifiers, its digest and the review policy state.
CREATE TABLE integrator_review_inputs (
  stage_execution_id TEXT PRIMARY KEY REFERENCES stage_executions(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  input_revision INTEGER NOT NULL CHECK(input_revision>=1),
  manifest_json TEXT NOT NULL CHECK(json_valid(manifest_json)),
  manifest_sha256 TEXT NOT NULL,
  review_policy_state TEXT NOT NULL CHECK(review_policy_state IN ('absent','loaded','partial','invalid','over_budget')),
  review_policy_sha256 TEXT,
  gap_count INTEGER NOT NULL CHECK(gap_count>=0),
  created_at TEXT NOT NULL
);
CREATE INDEX integrator_review_inputs_pulsation
  ON integrator_review_inputs(pulsation_id,input_revision);

-- An explicit decision to request another revision beyond the configured
-- human-round threshold, or to replan from the Specification.
CREATE TABLE pulsation_convergence_decisions (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  checkpoint_id TEXT NOT NULL UNIQUE REFERENCES checkpoints(id) ON DELETE CASCADE,
  input_revision INTEGER NOT NULL CHECK(input_revision>=1),
  stage TEXT NOT NULL,
  decision TEXT NOT NULL CHECK(decision IN ('continue_beyond_threshold','replan')),
  rounds_at_decision INTEGER NOT NULL CHECK(rounds_at_decision>=0),
  threshold INTEGER NOT NULL CHECK(threshold BETWEEN 1 AND 10),
  reason TEXT NOT NULL CHECK(length(reason) BETWEEN 1 AND 2000),
  created_at TEXT NOT NULL
);
CREATE INDEX pulsation_convergence_decisions_pulsation
  ON pulsation_convergence_decisions(pulsation_id,input_revision,created_at);
