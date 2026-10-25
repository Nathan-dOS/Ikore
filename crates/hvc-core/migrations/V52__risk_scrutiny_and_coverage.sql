-- ADR-034 M3: risk-driven scrutiny of composed pulsations.
-- Additive: four new tables, no rebuild of an existing table.

-- The risk level the host assessed for a composed stage of one input
-- revision, with the checks it made required. A rerun of the same stage and
-- revision replaces its row; the level of a later stage can only rise.
CREATE TABLE pulsation_risk_assessments (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  stage TEXT NOT NULL CHECK(stage IN ('Code','Test','Integrator')),
  input_revision INTEGER NOT NULL CHECK(input_revision>=1),
  level TEXT NOT NULL CHECK(level IN ('low','medium','high','unknown')),
  policy_version INTEGER NOT NULL CHECK(policy_version>=1),
  policy_sha256 TEXT NOT NULL,
  assessment_json TEXT NOT NULL CHECK(json_valid(assessment_json)),
  selection_json TEXT NOT NULL CHECK(json_valid(selection_json)),
  created_at TEXT NOT NULL,
  PRIMARY KEY(pulsation_id,stage,input_revision)
);

-- Every REVIEW.md of the repository at the pulsation's first Intake
-- admission. The Integrator reviews under this capture; the texts are
-- cleared when the pulsation ends and the hashes stay for its report.
CREATE TABLE pulsation_review_policy_snapshots (
  pulsation_id TEXT PRIMARY KEY REFERENCES pulsations(id) ON DELETE CASCADE,
  state TEXT NOT NULL CHECK(state IN ('absent','captured','partial')),
  files_json TEXT NOT NULL CHECK(json_valid(files_json)),
  omitted_json TEXT NOT NULL CHECK(json_valid(omitted_json)),
  content_json TEXT CHECK(content_json IS NULL OR json_valid(content_json)),
  snapshot_sha256 TEXT NOT NULL,
  captured_at TEXT NOT NULL
);

-- What backs each criterion and each risk-required check of one Integrator
-- execution: a fresh receipt, the model's review only, or nothing.
CREATE TABLE integrator_coverage (
  stage_execution_id TEXT PRIMARY KEY REFERENCES stage_executions(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  input_revision INTEGER NOT NULL CHECK(input_revision>=1),
  level TEXT NOT NULL CHECK(level IN ('low','medium','high','unknown')),
  matrix_json TEXT NOT NULL CHECK(json_valid(matrix_json)),
  pending INTEGER NOT NULL CHECK(pending>=0),
  created_at TEXT NOT NULL
);
CREATE INDEX integrator_coverage_pulsation
  ON integrator_coverage(pulsation_id,input_revision);

-- The developer's reason for approving a high-risk review that still had
-- unverified or unrun items.
CREATE TABLE pulsation_risk_acknowledgements (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  checkpoint_id TEXT NOT NULL UNIQUE REFERENCES checkpoints(id) ON DELETE CASCADE,
  input_revision INTEGER NOT NULL CHECK(input_revision>=1),
  level TEXT NOT NULL CHECK(level='high'),
  pending INTEGER NOT NULL CHECK(pending>=1),
  reason TEXT NOT NULL CHECK(length(reason) BETWEEN 1 AND 400),
  created_at TEXT NOT NULL
);
CREATE INDEX pulsation_risk_acknowledgements_pulsation
  ON pulsation_risk_acknowledgements(pulsation_id,input_revision,created_at);
