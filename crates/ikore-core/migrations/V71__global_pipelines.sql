-- Feature 25, M2b: global pipelines, their boxes in each project and child
-- pipelines. Additive: the columns V70's tables were missing for it.
--
-- A child names its parent; its versions hold `custom-pipeline-child-v1`
-- documents (the changes it makes to its parent), resolved over the parent's
-- version the project adopted. A parent with children cannot be deleted
-- (checked by the host; the foreign key is NO ACTION, so a project deletion
-- that takes both in one statement still works).
ALTER TABLE custom_pipelines
  ADD COLUMN parent_pipeline_id TEXT REFERENCES custom_pipelines(id);
CREATE INDEX idx_custom_pipelines_parent
  ON custom_pipelines(parent_pipeline_id) WHERE parent_pipeline_id IS NOT NULL;
CREATE INDEX idx_custom_pipelines_scope
  ON custom_pipelines(scope, archived);

-- The parent's version a project's box of a child resolves over (P2), and the
-- project's own trigger for a global pipeline (R4b, used from M3 on).
ALTER TABLE custom_pipeline_activations
  ADD COLUMN adopted_parent_version INTEGER
  CHECK(adopted_parent_version IS NULL OR adopted_parent_version > 0);
ALTER TABLE custom_pipeline_activations
  ADD COLUMN trigger_override_json TEXT
  CHECK(trigger_override_json IS NULL OR json_valid(trigger_override_json));
CREATE INDEX idx_custom_pipeline_activations_pipeline
  ON custom_pipeline_activations(pipeline_id);
