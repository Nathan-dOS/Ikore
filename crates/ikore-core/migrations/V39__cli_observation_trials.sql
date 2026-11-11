-- A project-scoped, explicitly enabled observation window for automatic
-- Pulsation CLI runs. The experiment never applies to manual/Companion runs.
CREATE TABLE cli_observation_trials (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  state TEXT NOT NULL CHECK(state IN ('active','closing','completed','cancelled')),
  target_runs INTEGER NOT NULL DEFAULT 20 CHECK(target_runs = 20),
  admitted_runs INTEGER NOT NULL DEFAULT 0 CHECK(admitted_runs BETWEEN 0 AND 20),
  settled_runs INTEGER NOT NULL DEFAULT 0 CHECK(settled_runs BETWEEN 0 AND 20),
  started_at TEXT NOT NULL,
  closed_at TEXT,
  completed_at TEXT
);

CREATE UNIQUE INDEX idx_cli_observation_trials_one_open
  ON cli_observation_trials(project_id)
  WHERE state IN ('active','closing');

ALTER TABLE terminal_runs
  ADD COLUMN observation_trial_id TEXT REFERENCES cli_observation_trials(id) ON DELETE SET NULL;

CREATE INDEX idx_terminal_runs_observation_trial
  ON terminal_runs(observation_trial_id, created_at);

CREATE TABLE cli_observation_samples (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id TEXT NOT NULL REFERENCES terminal_runs(id) ON DELETE CASCADE,
  observed_at TEXT NOT NULL,
  elapsed_ms INTEGER NOT NULL CHECK(elapsed_ms >= 0),
  user_cpu_100ns INTEGER,
  kernel_cpu_100ns INTEGER,
  peak_process_memory_bytes INTEGER,
  peak_job_memory_bytes INTEGER
);

CREATE INDEX idx_cli_observation_samples_run_time
  ON cli_observation_samples(run_id, observed_at);
