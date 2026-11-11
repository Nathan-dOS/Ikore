ALTER TABLE pulsations ADD COLUMN specification_started INTEGER NOT NULL DEFAULT 0 CHECK(specification_started IN (0,1));
UPDATE pulsations SET specification_started=1 WHERE stage NOT IN ('Intake','SPECIFICATION')
 OR EXISTS(SELECT 1 FROM jobs j WHERE j.pulsation_id=pulsations.id AND j.kind IN ('run_stage::SPECIFICATION','run_stage::Specification') AND j.attempts>0);

CREATE TABLE pulsation_description_revisions (
  pulsation_id TEXT NOT NULL REFERENCES pulsations(id), input_revision INTEGER NOT NULL,
  description TEXT NOT NULL, assessment_json TEXT, review_state TEXT NOT NULL,
  confirmed_at TEXT, created_at TEXT NOT NULL, PRIMARY KEY(pulsation_id,input_revision)
);
INSERT INTO pulsation_description_revisions(pulsation_id,input_revision,description,review_state,created_at)
 SELECT id,input_revision,feature_name,'not_required',created_at FROM pulsations;

ALTER TABLE pulsation_preparation ADD COLUMN predecessor_id TEXT REFERENCES pulsations(id);
ALTER TABLE pulsation_preparation ADD COLUMN job_id TEXT REFERENCES jobs(id);
ALTER TABLE pulsation_preparation ADD COLUMN consumed_job_id TEXT REFERENCES jobs(id);
ALTER TABLE pulsation_preparation ADD COLUMN context_disposition TEXT;
ALTER TABLE pulsation_preparation ADD COLUMN diagnostic TEXT;

CREATE TABLE specification_conversations (
  pulsation_id TEXT PRIMARY KEY REFERENCES pulsations(id),
  conversation_id TEXT NOT NULL UNIQUE REFERENCES chat_conversations(id) ON DELETE RESTRICT
);

CREATE VIEW anticipatable_pulsations AS
 SELECT p.id FROM pulsations p
 JOIN project_pipeline_state s ON s.project_id=p.project_id
 JOIN pulsations owner ON owner.id=s.active_pulsation_id
 WHERE p.status IN ('planning','running') AND p.stage IN ('Intake','SPECIFICATION')
 AND p.coordination_hold=0 AND s.base_state!='needs_review' AND owner.queue_sequence<p.queue_sequence
 AND NOT EXISTS(SELECT 1 FROM pulsations earlier WHERE earlier.project_id=p.project_id
   AND earlier.status NOT IN ('done','failed') AND earlier.queue_sequence>owner.queue_sequence AND earlier.queue_sequence<p.queue_sequence);

INSERT INTO runtime_settings(key,value,updated_at) VALUES('orchestration.preparation_enabled','false',CURRENT_TIMESTAMP);
INSERT INTO runtime_settings(key,value,updated_at) VALUES('orchestration.vagueness_threshold','40',CURRENT_TIMESTAMP);
UPDATE runtime_settings SET value='true',updated_at=CURRENT_TIMESTAMP WHERE key='orchestration.clarification_enabled';
