CREATE TABLE IF NOT EXISTS projects (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  workspace_path TEXT NOT NULL UNIQUE,
  is_archived INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  last_opened_at TEXT
);

CREATE TABLE IF NOT EXISTS pulsations (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  feature_name TEXT NOT NULL,
  stage TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS checkpoints (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT NOT NULL,
  stage TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS jobs (
  id TEXT PRIMARY KEY,
  pulsation_id TEXT,
  kind TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS agent_runs (
  id TEXT PRIMARY KEY,
  job_id TEXT NOT NULL,
  agent_name TEXT NOT NULL,
  status TEXT NOT NULL,
  input_tokens INTEGER,
  output_tokens INTEGER,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(job_id) REFERENCES jobs(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS logs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT,
  pulsation_id TEXT,
  level TEXT NOT NULL,
  message TEXT NOT NULL,
  created_at TEXT NOT NULL,
  FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE SET NULL,
  FOREIGN KEY(pulsation_id) REFERENCES pulsations(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS ui_sessions (
  project_id TEXT PRIMARY KEY,
  workspace_root TEXT NOT NULL,
  active_tab_path TEXT,
  workspace_view_mode TEXT NOT NULL,
  scroll_top REAL,
  updated_at TEXT NOT NULL,
  FOREIGN KEY(project_id) REFERENCES projects(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS ui_session_tabs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  project_id TEXT NOT NULL,
  tab_order INTEGER NOT NULL,
  relative_path TEXT NOT NULL,
  draft_content TEXT,
  base_revision TEXT,
  cursor_line INTEGER,
  cursor_column INTEGER,
  scroll_top REAL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(project_id, relative_path),
  FOREIGN KEY(project_id) REFERENCES ui_sessions(project_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_projects_last_opened_at
  ON projects(last_opened_at DESC);

CREATE INDEX IF NOT EXISTS idx_pulsations_project_id
  ON pulsations(project_id);

CREATE INDEX IF NOT EXISTS idx_checkpoints_pulsation_id
  ON checkpoints(pulsation_id);

CREATE INDEX IF NOT EXISTS idx_jobs_pulsation_id
  ON jobs(pulsation_id);

CREATE INDEX IF NOT EXISTS idx_agent_runs_job_id
  ON agent_runs(job_id);

CREATE INDEX IF NOT EXISTS idx_logs_project_created_at
  ON logs(project_id, created_at DESC);

CREATE INDEX IF NOT EXISTS idx_ui_session_tabs_project_order
  ON ui_session_tabs(project_id, tab_order);
