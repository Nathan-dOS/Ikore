ALTER TABLE ui_sessions ADD COLUMN active_chat_conversation_id TEXT;

CREATE TABLE IF NOT EXISTS chat_conversations (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    connection_id TEXT,
    provider_id_snapshot TEXT NOT NULL,
    model_id_snapshot TEXT NOT NULL,
    title TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY(connection_id) REFERENCES provider_connections(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_chat_conversations_project_updated
    ON chat_conversations(project_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS chat_messages (
    id TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('user', 'assistant')),
    content TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK(status IN ('pending', 'streaming', 'completed', 'failed', 'cancelled', 'interrupted')),
    execution_id TEXT,
    generation_id TEXT,
    last_sequence INTEGER NOT NULL DEFAULT 0,
    input_tokens INTEGER,
    output_tokens INTEGER,
    estimated_cost_usd REAL,
    error_code TEXT,
    error_message TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(conversation_id, ordinal),
    FOREIGN KEY(conversation_id) REFERENCES chat_conversations(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chat_messages_conversation_ordinal
    ON chat_messages(conversation_id, ordinal);

CREATE TABLE IF NOT EXISTS chat_model_calls (
    id TEXT PRIMARY KEY NOT NULL,
    assistant_message_id TEXT NOT NULL,
    round_index INTEGER NOT NULL,
    provider_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    latency_ms INTEGER,
    input_tokens INTEGER,
    output_tokens INTEGER,
    estimated_cost_usd REAL,
    status TEXT NOT NULL,
    error_code TEXT,
    FOREIGN KEY(assistant_message_id) REFERENCES chat_messages(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS chat_tool_calls (
    id TEXT PRIMARY KEY NOT NULL,
    assistant_message_id TEXT NOT NULL,
    model_call_id TEXT NOT NULL,
    round_index INTEGER NOT NULL,
    tool_call_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    sanitized_arguments TEXT NOT NULL,
    result_summary TEXT,
    result_bytes INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT,
    error_code TEXT,
    error_message TEXT,
    FOREIGN KEY(assistant_message_id) REFERENCES chat_messages(id) ON DELETE CASCADE,
    FOREIGN KEY(model_call_id) REFERENCES chat_model_calls(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_chat_tool_calls_message_round
    ON chat_tool_calls(assistant_message_id, round_index);
