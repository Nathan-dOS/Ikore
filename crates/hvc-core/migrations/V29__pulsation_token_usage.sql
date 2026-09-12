-- Attribution survives deletion of the source history, like the counters.
ALTER TABLE token_usage_records ADD COLUMN pulsation_id TEXT;
CREATE INDEX idx_token_usage_pulsation_started ON token_usage_records(pulsation_id,started_at);

UPDATE token_usage_records SET pulsation_id=(
    SELECT e.pulsation_id FROM llm_attempts a
    JOIN llm_route_executions e ON e.id=a.execution_id
    WHERE a.id=token_usage_records.source_id
) WHERE source_kind='llm_attempt';

UPDATE token_usage_records SET pulsation_id=(
    SELECT t.pulsation_id FROM terminal_runs t WHERE t.id=token_usage_records.source_id
) WHERE source_kind='terminal_run';

UPDATE token_usage_records SET pulsation_id=(
    SELECT j.pulsation_id FROM agent_runs a JOIN jobs j ON j.id=a.job_id
    WHERE a.id=token_usage_records.source_id
) WHERE source_kind='legacy_agent_run';
