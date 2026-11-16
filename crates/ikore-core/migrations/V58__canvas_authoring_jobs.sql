-- A proposal is not a grant. Only host admission starts a bounded authoring job.
CREATE TABLE companion_canvas_requests (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    conversation_id TEXT NOT NULL REFERENCES chat_conversations(id) ON DELETE CASCADE,
    assistant_message_id TEXT NOT NULL,
    source_sha256 TEXT NOT NULL CHECK(length(source_sha256)=64),
    source_explanation TEXT NOT NULL CHECK(length(source_explanation) BETWEEN 1 AND 16000),
    profile TEXT NOT NULL CHECK(profile IN ('architecture','bpmn')),
    language TEXT NOT NULL CHECK(length(language) BETWEEN 2 AND 35),
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 120),
    request_text TEXT NOT NULL CHECK(length(request_text) BETWEEN 1 AND 4000),
    state TEXT NOT NULL CHECK(state IN ('awaiting_authorization','declined','running','ready','failed','cancelled','interrupted')),
    request_revision INTEGER NOT NULL DEFAULT 1 CHECK(request_revision>0),
    job_id TEXT UNIQUE,
    action_id TEXT,
    context_nonce TEXT,
    target_snapshot_json TEXT CHECK(target_snapshot_json IS NULL OR (json_valid(target_snapshot_json) AND length(target_snapshot_json)<=32768)),
    target_binding_json TEXT CHECK(target_binding_json IS NULL OR json_valid(target_binding_json)),
    accepted_at INTEGER,
    expires_at INTEGER,
    stage_revision INTEGER NOT NULL DEFAULT 0 CHECK(stage_revision>=0),
    stage_scene_json TEXT CHECK(stage_scene_json IS NULL OR json_valid(stage_scene_json)),
    stage_sha256 TEXT,
    publication_sha256 TEXT,
    drawing_id TEXT,
    drawing_revision INTEGER,
    failure_code TEXT,
    failure_message TEXT,
    created_at INTEGER NOT NULL CHECK(created_at>=0),
    updated_at INTEGER NOT NULL CHECK(updated_at>=created_at),
    UNIQUE(project_id, assistant_message_id),
    CHECK(state!='running' OR (job_id IS NOT NULL AND action_id IS NOT NULL AND context_nonce IS NOT NULL
          AND target_snapshot_json IS NOT NULL AND target_binding_json IS NOT NULL
          AND accepted_at IS NOT NULL AND expires_at>accepted_at)),
    CHECK(state!='ready' OR (job_id IS NOT NULL AND drawing_id IS NOT NULL AND drawing_revision>0 AND publication_sha256 IS NOT NULL))
);
CREATE INDEX companion_canvas_requests_conversation
    ON companion_canvas_requests(project_id, conversation_id, created_at, id);
CREATE UNIQUE INDEX companion_canvas_requests_one_running
    ON companion_canvas_requests(conversation_id) WHERE state='running';
-- No FK to the source message or published drawing: audit and ready tombstones
-- survive deletion of either object and must never recreate a removed drawing.
