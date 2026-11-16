-- Additive M1 schema. Coordination/background admission is deliberately left to M2+.
ALTER TABLE pulsations ADD COLUMN queue_sequence INTEGER;
ALTER TABLE pulsations ADD COLUMN input_revision INTEGER NOT NULL DEFAULT 1 CHECK(input_revision > 0);
ALTER TABLE pulsations ADD COLUMN state_version INTEGER NOT NULL DEFAULT 1 CHECK(state_version > 0);
ALTER TABLE pulsations ADD COLUMN pipeline_version INTEGER NOT NULL DEFAULT 1 CHECK(pipeline_version IN (1,2));
ALTER TABLE pulsations ADD COLUMN terminal_reason TEXT;
ALTER TABLE pulsations ADD COLUMN revision_feedback TEXT;
UPDATE pulsations SET queue_sequence = (
  SELECT COUNT(*) FROM pulsations p WHERE p.project_id = pulsations.project_id
  AND (p.created_at < pulsations.created_at OR (p.created_at = pulsations.created_at AND p.id <= pulsations.id))
);
CREATE UNIQUE INDEX idx_pulse_sequence ON pulsations(project_id, queue_sequence);
CREATE TRIGGER pulse_sequence_immutable BEFORE UPDATE OF queue_sequence ON pulsations
WHEN OLD.queue_sequence IS NOT NULL AND NEW.queue_sequence IS NOT OLD.queue_sequence
BEGIN SELECT RAISE(ABORT, 'queue_sequence is immutable'); END;

CREATE TABLE project_pipeline_state (
  project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
  active_pulsation_id TEXT REFERENCES pulsations(id) ON DELETE RESTRICT,
  base_epoch INTEGER NOT NULL DEFAULT 0,
  base_state TEXT NOT NULL DEFAULT 'ready' CHECK(base_state IN ('ready','publishing','needs_review')),
  state_version INTEGER NOT NULL DEFAULT 1,
  blocking_reason TEXT
);
INSERT INTO project_pipeline_state(project_id, active_pulsation_id, base_state, blocking_reason)
SELECT p.id,
  CASE WHEN COUNT(a.id) = 1 THEN MAX(a.id) END,
  CASE WHEN COUNT(a.id) > 1 OR EXISTS(SELECT 1 FROM pulsations f WHERE f.project_id=p.id AND f.status='failed') THEN 'needs_review' ELSE 'ready' END,
  CASE WHEN COUNT(a.id) > 1 THEN 'multiple_legacy_owners'
       WHEN EXISTS(SELECT 1 FROM pulsations f WHERE f.project_id=p.id AND f.status='failed') THEN 'legacy_failure_unverified' END
FROM projects p LEFT JOIN pulsations a ON a.project_id=p.id AND a.stage <> 'Intake' AND a.status NOT IN ('done','failed') GROUP BY p.id;
CREATE TRIGGER initialize_pipeline_state AFTER INSERT ON projects
BEGIN INSERT INTO project_pipeline_state(project_id) VALUES(NEW.id); END;

ALTER TABLE jobs ADD COLUMN project_id TEXT REFERENCES projects(id) ON DELETE CASCADE;
ALTER TABLE jobs ADD COLUMN input_revision INTEGER;
ALTER TABLE jobs ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE jobs ADD COLUMN state_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE jobs ADD COLUMN job_class TEXT NOT NULL DEFAULT 'main' CHECK(job_class IN ('main','background'));
ALTER TABLE jobs ADD COLUMN cancellation_reason TEXT CHECK(cancellation_reason IN ('user_cancel','shutdown','preempted','superseded'));
ALTER TABLE jobs ADD COLUMN idempotency_key TEXT;
UPDATE jobs SET project_id=(SELECT project_id FROM pulsations WHERE id=jobs.pulsation_id),
 input_revision=CASE WHEN pulsation_id IS NOT NULL THEN 1 END,
 generation=CASE WHEN status IN ('running','done','interrupted') THEN 1 ELSE 0 END,
 idempotency_key='legacy:' || id;
CREATE UNIQUE INDEX idx_job_idempotency ON jobs(idempotency_key);
UPDATE jobs SET cancellation_reason='user_cancel' WHERE cancel_requested_at IS NOT NULL;

ALTER TABLE checkpoints ADD COLUMN kind TEXT NOT NULL DEFAULT 'artifact_review' CHECK(kind IN ('artifact_review','clarification'));
ALTER TABLE checkpoints ADD COLUMN input_revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE checkpoints ADD COLUMN state_version INTEGER NOT NULL DEFAULT 1;
ALTER TABLE checkpoints ADD COLUMN producer_job_id TEXT REFERENCES jobs(id);
ALTER TABLE checkpoints ADD COLUMN generation INTEGER;
CREATE UNIQUE INDEX idx_checkpoint_producer ON checkpoints(producer_job_id,generation,kind);

ALTER TABLE artifacts ADD COLUMN content TEXT;
ALTER TABLE artifacts ADD COLUMN producer_job_id TEXT REFERENCES jobs(id);
ALTER TABLE artifacts ADD COLUMN producer_stage TEXT;
ALTER TABLE artifacts ADD COLUMN generation INTEGER;
ALTER TABLE artifacts ADD COLUMN input_revision INTEGER;
ALTER TABLE artifacts ADD COLUMN producer_state_version INTEGER;
ALTER TABLE artifacts ADD COLUMN output_position INTEGER;
ALTER TABLE artifacts ADD COLUMN acceptance TEXT NOT NULL DEFAULT 'candidate' CHECK(acceptance IN ('candidate','accepted','superseded'));
ALTER TABLE artifacts ADD COLUMN publication_state TEXT NOT NULL DEFAULT 'not_required' CHECK(publication_state IN ('not_required','pending','applying','applied','conflict'));
ALTER TABLE artifacts ADD COLUMN source_revisions_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE artifacts ADD COLUMN legacy_adaptation TEXT;
CREATE UNIQUE INDEX idx_artifact_producer ON artifacts(producer_job_id,generation,output_position);

CREATE TABLE stage_acceptances (
  job_id TEXT NOT NULL REFERENCES jobs(id), generation INTEGER NOT NULL,
  agent_run_id TEXT NOT NULL UNIQUE REFERENCES agent_runs(id),
  fence_json TEXT NOT NULL, outcome_json TEXT NOT NULL, handoff_json TEXT NOT NULL,
  accepted_at TEXT NOT NULL,
  PRIMARY KEY(job_id,generation)
);
CREATE TABLE artifact_publications (
  artifact_id TEXT PRIMARY KEY REFERENCES artifacts(id) ON DELETE CASCADE,
  project_id TEXT NOT NULL REFERENCES projects(id), pulsation_id TEXT NOT NULL REFERENCES pulsations(id),
  relative_path TEXT NOT NULL, expected_sha256 TEXT, desired_sha256 TEXT NOT NULL,
  state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','applying','applied','conflict','cancelled')),
  last_error TEXT, updated_at TEXT NOT NULL
);
CREATE INDEX idx_publication_pending ON artifact_publications(state,project_id);
CREATE TABLE pulsation_preparation (
  pulsation_id TEXT PRIMARY KEY REFERENCES pulsations(id) ON DELETE CASCADE,
  input_revision INTEGER NOT NULL, assessment_json TEXT,
  review_state TEXT NOT NULL DEFAULT 'not_required', prespec_state TEXT NOT NULL DEFAULT 'not_scheduled',
  context_artifact_id TEXT REFERENCES artifacts(id), skip_reason TEXT, updated_at TEXT NOT NULL
);
CREATE TABLE specification_question_sets (
  id TEXT PRIMARY KEY, checkpoint_id TEXT NOT NULL UNIQUE REFERENCES checkpoints(id),
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id), input_revision INTEGER NOT NULL,
  generation INTEGER NOT NULL, state_version INTEGER NOT NULL,
  conversation_id TEXT NOT NULL, questions_json TEXT NOT NULL, answers_json TEXT,
  UNIQUE(pulsation_id,input_revision,generation,state_version)
);
INSERT INTO runtime_settings(key,value,updated_at) VALUES('orchestration.clarification_enabled','false',CURRENT_TIMESTAMP);
