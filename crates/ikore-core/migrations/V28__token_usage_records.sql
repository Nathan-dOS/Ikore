-- Durable, content-free accounting. Source history can be deleted independently.
CREATE TABLE token_usage_records (
    source_kind TEXT NOT NULL,
    source_id TEXT NOT NULL,
    unit_key TEXT NOT NULL,
    project_id TEXT,
    transport TEXT NOT NULL CHECK(transport IN ('api','cli')),
    provider_id TEXT NOT NULL,
    requested_model TEXT,
    observed_model TEXT,
    input_tokens INTEGER CHECK(input_tokens >= 0),
    output_tokens INTEGER CHECK(output_tokens >= 0),
    total_tokens INTEGER CHECK(total_tokens >= 0),
    cache_read_tokens INTEGER CHECK(cache_read_tokens >= 0),
    cache_write_tokens INTEGER CHECK(cache_write_tokens >= 0),
    reasoning_tokens INTEGER CHECK(reasoning_tokens >= 0),
    quality TEXT NOT NULL CHECK(quality IN ('complete','partial','unavailable')),
    started_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    historical INTEGER NOT NULL DEFAULT 0 CHECK(historical IN (0,1)),
    PRIMARY KEY(source_kind,source_id,unit_key)
);
CREATE INDEX idx_token_usage_project_started ON token_usage_records(project_id,started_at);
CREATE INDEX idx_token_usage_started ON token_usage_records(started_at);

-- Old counters lack normalized cache/coverage metadata. Preserve known values
-- and identify them as partial. This migration runs exactly once.
INSERT INTO token_usage_records (
    source_kind,source_id,unit_key,project_id,transport,provider_id,requested_model,
    input_tokens,output_tokens,total_tokens,quality,started_at,updated_at,historical
)
SELECT 'llm_attempt',a.id,'response',e.project_id,'api',a.provider_id,a.model_id,
       CASE WHEN a.input_tokens>=0 THEN a.input_tokens END,
       CASE WHEN a.output_tokens>=0 THEN a.output_tokens END,
       CASE WHEN a.input_tokens>=0 OR a.output_tokens>=0
            THEN max(COALESCE(a.input_tokens,0),0)+max(COALESCE(a.output_tokens,0),0) END,
       CASE WHEN a.input_tokens>=0 OR a.output_tokens>=0 THEN 'partial' ELSE 'unavailable' END,
       a.started_at,COALESCE(a.finished_at,a.started_at),1
FROM llm_attempts a JOIN llm_route_executions e ON e.id=a.execution_id;

INSERT INTO token_usage_records (
    source_kind,source_id,unit_key,project_id,transport,provider_id,requested_model,
    input_tokens,output_tokens,total_tokens,quality,started_at,updated_at,historical
)
SELECT 'chat_model_call',c.id,'response',v.project_id,'api',c.provider_id,c.model_id,
       CASE WHEN c.input_tokens>=0 THEN c.input_tokens END,
       CASE WHEN c.output_tokens>=0 THEN c.output_tokens END,
       CASE WHEN c.input_tokens>=0 OR c.output_tokens>=0
            THEN max(COALESCE(c.input_tokens,0),0)+max(COALESCE(c.output_tokens,0),0) END,
       CASE WHEN c.input_tokens>=0 OR c.output_tokens>=0 THEN 'partial' ELSE 'unavailable' END,
       c.started_at,COALESCE(c.completed_at,c.started_at),1
FROM chat_model_calls c
JOIN chat_messages m ON m.id=c.assistant_message_id
JOIN chat_conversations v ON v.id=m.conversation_id
WHERE c.transport_kind='api';

-- Legacy agent rows are fallback evidence only, never additional copies of a
-- routed or terminal execution. Synthetic fallback has no reported usage.
INSERT INTO token_usage_records (
    source_kind,source_id,unit_key,project_id,transport,provider_id,requested_model,
    input_tokens,output_tokens,total_tokens,quality,started_at,updated_at,historical
)
SELECT 'legacy_agent_run',a.id,'response',p.project_id,'api',a.provider,a.model,
       CASE WHEN a.input_tokens>=0 THEN a.input_tokens END,
       CASE WHEN a.output_tokens>=0 THEN a.output_tokens END,
       max(COALESCE(a.input_tokens,0),0)+max(COALESCE(a.output_tokens,0),0),
       'partial',a.created_at,a.updated_at,1
FROM agent_runs a JOIN jobs j ON j.id=a.job_id
LEFT JOIN pulsations p ON p.id=j.pulsation_id
WHERE a.llm_execution_id IS NULL
  AND a.provider IS NOT NULL
  AND (a.input_tokens>=0 OR a.output_tokens>=0)
  AND NOT EXISTS (SELECT 1 FROM llm_route_executions e WHERE e.job_id=a.job_id)
  AND NOT EXISTS (SELECT 1 FROM terminal_runs t WHERE t.agent_run_id=a.id);
