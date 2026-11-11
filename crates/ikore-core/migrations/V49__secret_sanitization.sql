-- Host-owned policy and metadata only: never store a matched secret or its standalone digest.
CREATE TABLE project_secret_redaction_exceptions (
 id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 relative_path TEXT NOT NULL,
 detector_id TEXT NOT NULL,
 detector_version INTEGER NOT NULL,
 original_file_sha256 TEXT NOT NULL,
 start_byte INTEGER NOT NULL CHECK(start_byte >= 0),
 end_byte INTEGER NOT NULL CHECK(end_byte > start_byte),
 reason TEXT NOT NULL CHECK(length(reason) BETWEEN 1 AND 1000),
 created_at TEXT NOT NULL,
 UNIQUE(project_id,relative_path,detector_id,detector_version,original_file_sha256,start_byte,end_byte)
);
CREATE TABLE secret_sanitization_receipts (
 call_id TEXT PRIMARY KEY,
 project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 pulsation_id TEXT REFERENCES pulsations(id) ON DELETE CASCADE,
 policy_identity TEXT NOT NULL,
 input_redaction_count INTEGER NOT NULL CHECK(input_redaction_count >= 0),
 categories_json TEXT NOT NULL,
 examined_bytes INTEGER NOT NULL CHECK(examined_bytes >= 0),
 duration_ms INTEGER NOT NULL CHECK(duration_ms >= 0),
 exception_count INTEGER NOT NULL DEFAULT 0 CHECK(exception_count >= 0),
 created_at TEXT NOT NULL
);
CREATE INDEX secret_receipts_pulsation ON secret_sanitization_receipts(pulsation_id);
CREATE TABLE secret_sanitization_source_maps (
 call_id TEXT NOT NULL REFERENCES secret_sanitization_receipts(call_id) ON DELETE CASCADE,
 source_ref TEXT NOT NULL,
 original_sha256 TEXT NOT NULL,
 view_sha256 TEXT NOT NULL,
 policy_identity TEXT NOT NULL,
 spans_json TEXT NOT NULL,
 PRIMARY KEY(call_id,source_ref)
);
CREATE TABLE secret_publication_guards (
 artifact_id TEXT PRIMARY KEY REFERENCES artifacts(id) ON DELETE CASCADE,
 project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
 pulsation_id TEXT NOT NULL REFERENCES pulsations(id) ON DELETE CASCADE,
 expected_sha256 TEXT,
 desired_sha256 TEXT NOT NULL,
 policy_identity TEXT NOT NULL,
 created_at TEXT NOT NULL
);
-- Original revision hashes stay intact; semantic vectors are qualified separately.
ALTER TABLE workspace_note_embeddings ADD COLUMN sanitization_policy_version TEXT NOT NULL DEFAULT '';
ALTER TABLE embedding_sync_state ADD COLUMN sanitization_policy_version TEXT NOT NULL DEFAULT '';
-- Evidence from the logical final API output, never reconstructed from markers.
ALTER TABLE llm_route_executions ADD COLUMN output_redaction_count INTEGER NOT NULL DEFAULT 0 CHECK(output_redaction_count >= 0);
ALTER TABLE llm_route_executions ADD COLUMN output_redaction_categories_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(output_redaction_categories_json));
