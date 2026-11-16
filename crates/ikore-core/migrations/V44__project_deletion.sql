-- A permanent deletion keeps only provider usage attribution in a separate ledger.
CREATE TABLE retained_usage_projects (
    project_id TEXT PRIMARY KEY,
    project_name TEXT NOT NULL,
    deleted_at TEXT NOT NULL
);

CREATE TABLE retained_project_usage (
    source_kind TEXT NOT NULL,
    source_id TEXT NOT NULL,
    unit_key TEXT NOT NULL,
    project_id TEXT NOT NULL REFERENCES retained_usage_projects(project_id),
    transport TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    requested_model TEXT,
    observed_model TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    total_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_write_tokens INTEGER,
    reasoning_tokens INTEGER,
    quality TEXT NOT NULL,
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    historical INTEGER NOT NULL,
    pulsation_id TEXT,
    stage_execution_id TEXT,
    work_unit_id TEXT,
    attempt_id TEXT,
    executor_kind TEXT,
    PRIMARY KEY(project_id,source_kind,source_id,unit_key)
);
CREATE INDEX retained_project_usage_started ON retained_project_usage(project_id,started_at);
CREATE INDEX retained_project_usage_pulsation ON retained_project_usage(pulsation_id,started_at);

CREATE VIEW all_token_usage_records AS
SELECT * FROM token_usage_records
UNION ALL
SELECT * FROM retained_project_usage;

-- The journal has no project FK: it must survive the project's transaction.
CREATE TABLE project_deletion_journal (
    project_id TEXT PRIMARY KEY,
    root_path TEXT NOT NULL,
    quarantine_path TEXT NOT NULL,
    was_archived INTEGER NOT NULL CHECK(was_archived IN (0,1)),
    phase TEXT NOT NULL CHECK(phase IN ('prepared','purged')),
    started_at TEXT NOT NULL
);
