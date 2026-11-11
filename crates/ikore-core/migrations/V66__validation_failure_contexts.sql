-- Historical failure evidence is separate from human guidance and active artifacts.
CREATE TABLE validation_failure_contexts (
    checkpoint_id TEXT PRIMARY KEY REFERENCES checkpoints(id) ON DELETE CASCADE,
    execution_id TEXT NOT NULL REFERENCES stage_executions(id) ON DELETE CASCADE,
    -- Escaping can expand bounded control characters to six JSON characters.
    context_json TEXT NOT NULL CHECK(json_valid(context_json) AND length(context_json) <= 2097152),
    created_at TEXT NOT NULL
);
CREATE TRIGGER validation_failure_context_immutable BEFORE UPDATE ON validation_failure_contexts
BEGIN SELECT RAISE(ABORT, 'validation failure context is immutable'); END;

-- Evidence belongs to the exact human-selected recovery job, never every future stage.
CREATE TABLE validation_failure_recovery_jobs (
    job_id TEXT PRIMARY KEY REFERENCES jobs(id) ON DELETE CASCADE,
    checkpoint_id TEXT NOT NULL REFERENCES validation_failure_contexts(checkpoint_id) ON DELETE CASCADE
);
CREATE TRIGGER validation_failure_recovery_job_immutable BEFORE UPDATE ON validation_failure_recovery_jobs
BEGIN SELECT RAISE(ABORT, 'validation recovery binding is immutable'); END;
