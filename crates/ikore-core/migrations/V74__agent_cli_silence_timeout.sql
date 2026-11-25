-- NULL follows the agent role's default; override is measured in milliseconds.
ALTER TABLE cli_agent_configs ADD COLUMN cli_silence_timeout_ms INTEGER
  CHECK(cli_silence_timeout_ms IS NULL OR cli_silence_timeout_ms BETWEEN 30000 AND 900000);
