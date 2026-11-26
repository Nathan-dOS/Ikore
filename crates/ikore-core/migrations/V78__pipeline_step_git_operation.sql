-- Feature 25 M5: link host_tool pipeline step runs to git_operations audit rows.
ALTER TABLE custom_pipeline_step_runs ADD COLUMN git_operation_id TEXT REFERENCES git_operations(id) ON DELETE SET NULL;
CREATE INDEX idx_custom_pipeline_step_runs_git_op
  ON custom_pipeline_step_runs(git_operation_id)
  WHERE git_operation_id IS NOT NULL;
