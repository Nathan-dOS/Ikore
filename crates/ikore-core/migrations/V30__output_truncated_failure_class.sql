-- A model cut off by its own output-token budget is not the same failure as a
-- CLI that broke. The process exits cleanly and the answer is simply
-- incomplete, so the JSON never parses and an identical retry truncates again
-- at the same place. It needs its own class to carry a decision set that does
-- not offer that pointless retry.
--
-- SQLite cannot widen a CHECK in place, so the table is rebuilt. It is
-- referenced by nothing, and it references checkpoints/pulsations/jobs, so a
-- plain rebuild is safe; `legacy_alter_table` keeps DROP+RENAME from rewriting
-- FK clauses elsewhere, matching V27.
PRAGMA legacy_alter_table=ON;

CREATE TABLE stage_failure_requests_v30 (
  checkpoint_id TEXT PRIMARY KEY REFERENCES checkpoints(id) ON DELETE CASCADE,
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
  job_id TEXT NOT NULL REFERENCES jobs(id),
  stage TEXT NOT NULL,
  failure_class TEXT NOT NULL CHECK(failure_class IN (
    'retry_budget_exhausted','provider_configuration','provider_transport',
    'infrastructure','executor_setup','host_contract','model_reported_failure',
    'output_truncated')),
  allow_retry INTEGER NOT NULL CHECK(allow_retry IN (0,1)),
  allow_revision INTEGER NOT NULL CHECK(allow_revision IN (0,1)),
  revision_stage TEXT,
  error_code TEXT NOT NULL,
  diagnostic TEXT NOT NULL,
  agent_run_id TEXT,
  decision TEXT CHECK(decision IS NULL OR decision IN ('retry_stage','request_revision','abort')),
  resolved_at TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  CHECK(allow_retry=1 OR allow_revision=1),
  CHECK(allow_revision=0 OR revision_stage IS NOT NULL)
);

INSERT INTO stage_failure_requests_v30("checkpoint_id","pulsation_id","job_id","stage","failure_class","allow_retry","allow_revision","revision_stage","error_code","diagnostic","agent_run_id","decision","resolved_at","created_at","updated_at")
SELECT "checkpoint_id","pulsation_id","job_id","stage","failure_class","allow_retry","allow_revision","revision_stage","error_code","diagnostic","agent_run_id","decision","resolved_at","created_at","updated_at" FROM stage_failure_requests;

DROP TABLE stage_failure_requests;

ALTER TABLE stage_failure_requests_v30 RENAME TO stage_failure_requests;

CREATE INDEX idx_stage_failure_pulsation ON stage_failure_requests(pulsation_id,created_at DESC);
