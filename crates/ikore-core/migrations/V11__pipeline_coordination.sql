-- M2: durable ownership is distinct from a live executor slot.
ALTER TABLE pulsations ADD COLUMN coordination_hold INTEGER NOT NULL DEFAULT 0 CHECK(coordination_hold IN (0,1));
ALTER TABLE pulsations ADD COLUMN integration_checkpoint_id TEXT REFERENCES checkpoints(id);
ALTER TABLE pulsations ADD COLUMN integration_state_version INTEGER;
ALTER TABLE artifacts ADD COLUMN reconciled_at TEXT;

CREATE TABLE runtime_slots (
  job_class TEXT PRIMARY KEY CHECK(job_class IN ('main','background')),
  token TEXT NOT NULL UNIQUE,
  project_id TEXT REFERENCES projects(id),
  job_id TEXT REFERENCES jobs(id),
  acquired_at TEXT NOT NULL
);
CREATE TABLE integration_learning_intents (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id),
  integration_epoch INTEGER NOT NULL,
  report_version INTEGER NOT NULL DEFAULT 1,
  state TEXT NOT NULL CHECK(state IN ('unavailable','pending','cancelled')),
  created_at TEXT NOT NULL,
  PRIMARY KEY(pulsation_id,integration_epoch,report_version)
);
INSERT OR IGNORE INTO runtime_settings(key,value,updated_at)
VALUES('orchestration.learning_enabled','true',CURRENT_TIMESTAMP);

CREATE TABLE base_reconciliations (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id),
  previous_state_version INTEGER NOT NULL,
  base_epoch INTEGER NOT NULL,
  resumed_pulsation_id TEXT REFERENCES pulsations(id),
  explanation TEXT NOT NULL,
  observations_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE project_file_baselines (
  project_id TEXT NOT NULL REFERENCES projects(id),
  relative_path TEXT NOT NULL COLLATE NOCASE,
  content_sha256 TEXT,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(project_id,relative_path)
);
INSERT INTO project_file_baselines(project_id,relative_path,content_sha256,updated_at)
SELECT project_id,relative_path,content_sha256,updated_at FROM (
  SELECT p.project_id,a.relative_path,a.content_sha256,a.updated_at,
    ROW_NUMBER() OVER (PARTITION BY p.project_id,lower(a.relative_path) ORDER BY COALESCE(o.updated_at,a.updated_at) DESC,a.id DESC) AS position
  FROM artifacts a JOIN pulsations p ON p.id=a.pulsation_id
  LEFT JOIN artifact_publications o ON o.artifact_id=a.id
  WHERE a.publication_state='applied' AND a.relative_path IS NOT NULL
) WHERE position=1;

-- M1 did not enforce ownership. Preserve all advanced work, including v2 work.
UPDATE project_pipeline_state SET active_pulsation_id=(
  SELECT p.id FROM pulsations p WHERE p.project_id=project_pipeline_state.project_id
  AND p.stage<>'Intake' AND p.status NOT IN ('done','failed')
) WHERE (SELECT COUNT(*) FROM pulsations p WHERE p.project_id=project_pipeline_state.project_id
  AND p.stage<>'Intake' AND p.status NOT IN ('done','failed'))=1;
UPDATE project_pipeline_state SET active_pulsation_id=NULL,base_state='needs_review',
  blocking_reason='multiple_legacy_owners',state_version=state_version+1
WHERE (SELECT COUNT(*) FROM pulsations p WHERE p.project_id=project_pipeline_state.project_id
  AND p.stage<>'Intake' AND p.status NOT IN ('done','failed'))>1;
UPDATE pulsations SET coordination_hold=1 WHERE stage<>'Intake' AND status NOT IN ('done','failed')
  AND project_id IN (SELECT project_id FROM project_pipeline_state WHERE blocking_reason='multiple_legacy_owners');
UPDATE jobs SET state_version=(SELECT state_version FROM pulsations p WHERE p.id=jobs.pulsation_id)
WHERE pulsation_id IS NOT NULL AND status IN ('pending','running','interrupted');

-- Capture only the latest approval, never an approval preceding a revision.
UPDATE pulsations SET integration_checkpoint_id=(
  SELECT c.id FROM checkpoints c WHERE c.pulsation_id=pulsations.id AND c.stage='Integrator'
  ORDER BY c.updated_at DESC,c.rowid DESC LIMIT 1
),integration_state_version=state_version WHERE stage='Integrator' AND status='running';

CREATE VIEW coordinated_pulsations AS
SELECT p.id FROM pulsations p JOIN project_pipeline_state s ON s.project_id=p.project_id
WHERE p.status NOT IN ('done','failed') AND p.coordination_hold=0 AND s.base_state<>'needs_review'
AND (s.active_pulsation_id=p.id OR (s.active_pulsation_id IS NULL AND NOT EXISTS(
  SELECT 1 FROM pulsations prior WHERE prior.project_id=p.project_id AND prior.status NOT IN ('done','failed')
  AND prior.queue_sequence<p.queue_sequence
)));
CREATE INDEX idx_jobs_admission ON jobs(job_class,status,available_at,project_id);
CREATE TRIGGER pipeline_owner_project BEFORE UPDATE OF active_pulsation_id ON project_pipeline_state
WHEN NEW.active_pulsation_id IS NOT NULL AND NOT EXISTS(
  SELECT 1 FROM pulsations p WHERE p.id=NEW.active_pulsation_id AND p.project_id=NEW.project_id
)
BEGIN SELECT RAISE(ABORT,'owner belongs to another project'); END;
