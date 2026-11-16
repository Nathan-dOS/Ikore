ALTER TABLE chat_conversations ADD COLUMN transport_kind TEXT NOT NULL DEFAULT 'api'
  CHECK(transport_kind IN ('api','cli'));
ALTER TABLE chat_conversations ADD COLUMN cli_backend TEXT
  CHECK(cli_backend IS NULL OR cli_backend IN ('codex','claude','gemini','opencode'));
ALTER TABLE chat_conversations ADD COLUMN model_profile TEXT;
ALTER TABLE chat_conversations ADD COLUMN model_source TEXT NOT NULL DEFAULT 'catalog'
  CHECK(model_source IN ('default','catalog','manual'));

ALTER TABLE chat_model_calls ADD COLUMN transport_kind TEXT NOT NULL DEFAULT 'api'
  CHECK(transport_kind IN ('api','cli'));
ALTER TABLE chat_model_calls ADD COLUMN cli_backend TEXT
  CHECK(cli_backend IS NULL OR cli_backend IN ('codex','claude','gemini','opencode'));
ALTER TABLE chat_model_calls ADD COLUMN model_profile TEXT;
ALTER TABLE chat_model_calls ADD COLUMN detected_cli_version TEXT;
ALTER TABLE chat_model_calls ADD COLUMN exit_code INTEGER;
ALTER TABLE chat_model_calls ADD COLUMN termination_reason TEXT;
ALTER TABLE chat_model_calls ADD COLUMN output_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE chat_model_calls ADD COLUMN redaction_count INTEGER NOT NULL DEFAULT 0;

ALTER TABLE cli_agent_configs ADD COLUMN model_profile TEXT;
ALTER TABLE cli_agent_configs ADD COLUMN model_source TEXT NOT NULL DEFAULT 'default'
  CHECK(model_source IN ('default','catalog','manual'));
ALTER TABLE terminal_slots ADD COLUMN model_profile TEXT;
ALTER TABLE terminal_slots ADD COLUMN model_source TEXT NOT NULL DEFAULT 'default'
  CHECK(model_source IN ('default','catalog','manual'));
ALTER TABLE terminal_runs ADD COLUMN requested_model_profile TEXT;
ALTER TABLE terminal_runs ADD COLUMN model_source TEXT NOT NULL DEFAULT 'default'
  CHECK(model_source IN ('default','catalog','manual'));
ALTER TABLE terminal_runs ADD COLUMN observed_model TEXT;

UPDATE cli_agent_configs
SET model_profile=NULL,
    model_source=CASE WHEN model IS NULL OR trim(model)='' THEN 'default' ELSE 'manual' END;
UPDATE terminal_slots
SET model_profile=NULL,
    model_source=CASE WHEN model IS NULL OR trim(model)='' THEN 'default' ELSE 'manual' END;
UPDATE terminal_runs
SET model_source=CASE WHEN model IS NULL OR trim(model)='' THEN 'default' ELSE 'manual' END;

CREATE TABLE cli_model_catalog_snapshots (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  backend TEXT NOT NULL CHECK(backend IN ('codex','claude','gemini','opencode')),
  adapter_contract_version INTEGER NOT NULL,
  state TEXT NOT NULL CHECK(state IN (
    'ready','stale','missing','auth_required','unsupported_version','failed'
  )),
  source TEXT NOT NULL CHECK(source IN ('preset','cli')),
  cli_version TEXT,
  diagnostic TEXT,
  refreshed_at TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(project_id,backend)
);

CREATE TABLE cli_model_catalog_entries (
  snapshot_id TEXT NOT NULL REFERENCES cli_model_catalog_snapshots(id) ON DELETE CASCADE,
  provider_id TEXT,
  model_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  context_window INTEGER,
  input_cost_usd_per_million REAL,
  output_cost_usd_per_million REAL,
  capabilities_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(capabilities_json)),
  profiles_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(profiles_json)),
  raw_metadata_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(raw_metadata_json)),
  position INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(snapshot_id,model_id)
);

CREATE INDEX idx_cli_model_catalog_entries_snapshot_position
  ON cli_model_catalog_entries(snapshot_id,position,model_id);

CREATE TABLE companion_agent_output_documents (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  stable_reference TEXT NOT NULL,
  source_kind TEXT NOT NULL CHECK(source_kind IN (
    'agent_run_output','terminal_event','artifact'
  )),
  source_id TEXT NOT NULL,
  agent_role TEXT,
  backend TEXT,
  model TEXT,
  status TEXT NOT NULL,
  authoritative INTEGER NOT NULL DEFAULT 1 CHECK(authoritative IN (0,1)),
  title TEXT NOT NULL,
  content TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(project_id,stable_reference)
);

CREATE VIRTUAL TABLE companion_agent_output_fts USING fts5(
  title,
  content,
  content='companion_agent_output_documents',
  content_rowid='id',
  tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER companion_agent_output_documents_ai AFTER INSERT ON companion_agent_output_documents BEGIN
  INSERT INTO companion_agent_output_fts(rowid,title,content)
  VALUES(new.id,new.title,new.content);
END;

CREATE TRIGGER companion_agent_output_documents_ad AFTER DELETE ON companion_agent_output_documents BEGIN
  INSERT INTO companion_agent_output_fts(companion_agent_output_fts,rowid,title,content)
  VALUES('delete',old.id,old.title,old.content);
END;

CREATE TRIGGER companion_agent_output_documents_au AFTER UPDATE ON companion_agent_output_documents BEGIN
  INSERT INTO companion_agent_output_fts(companion_agent_output_fts,rowid,title,content)
  VALUES('delete',old.id,old.title,old.content);
  INSERT INTO companion_agent_output_fts(rowid,title,content)
  VALUES(new.id,new.title,new.content);
END;

CREATE INDEX idx_companion_outputs_project_created
  ON companion_agent_output_documents(project_id,created_at DESC);
CREATE INDEX idx_companion_outputs_project_source
  ON companion_agent_output_documents(project_id,source_kind,source_id);

INSERT OR IGNORE INTO companion_agent_output_documents(
  project_id,stable_reference,source_kind,source_id,agent_role,backend,model,
  status,authoritative,title,content,created_at,updated_at
)
SELECT
  p.project_id,
  'agent-run-output:' || o.id,
  'agent_run_output',
  o.id,
  r.agent_name,
  r.provider,
  r.model,
  r.status,
  CASE WHEN r.status='completed' THEN 1 ELSE 0 END,
  r.agent_name || ' agent output',
  CASE WHEN o.structured_output IS NOT NULL AND trim(o.structured_output)<>''
       THEN o.structured_output ELSE o.raw_output END,
  o.created_at,
  o.created_at
FROM agent_run_outputs o
JOIN agent_runs r ON r.id=o.agent_run_id
JOIN jobs j ON j.id=o.job_id
JOIN pulsations p ON p.id=COALESCE(o.pulsation_id,j.pulsation_id);

INSERT OR IGNORE INTO companion_agent_output_documents(
  project_id,stable_reference,source_kind,source_id,agent_role,backend,model,
  status,authoritative,title,content,created_at,updated_at
)
SELECT
  p.project_id,
  'artifact:' || a.id,
  'artifact',
  a.id,
  a.producer_stage,
  NULL,
  NULL,
  a.acceptance,
  CASE WHEN a.acceptance='accepted' THEN 1 ELSE 0 END,
  a.name,
  COALESCE(a.content,''),
  a.created_at,
  a.updated_at
FROM artifacts a
JOIN pulsations p ON p.id=a.pulsation_id
WHERE a.content IS NOT NULL AND trim(a.content)<>'';

INSERT OR IGNORE INTO companion_agent_output_documents(
  project_id,stable_reference,source_kind,source_id,agent_role,backend,model,
  status,authoritative,title,content,created_at,updated_at
)
SELECT
  r.project_id,
  'terminal-event:' || e.run_id || ':' || e.sequence,
  'terminal_event',
  e.id,
  r.agent_role,
  r.backend,
  r.model,
  r.status,
  CASE WHEN r.status='completed' THEN 1 ELSE 0 END,
  COALESCE(r.free_form_label,r.agent_role,'Terminal') || ' terminal output',
  e.content,
  e.created_at,
  e.created_at
FROM terminal_events e
JOIN terminal_runs r ON r.id=e.run_id
WHERE e.kind IN ('message','tool_result','stdout');
