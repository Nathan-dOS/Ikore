-- Only bounded watchdog metadata is retained; never store prompts or CLI output.
CREATE TABLE companion_timeout_diagnostics (
    model_call_id TEXT PRIMARY KEY REFERENCES chat_model_calls(id) ON DELETE CASCADE,
    reason TEXT NOT NULL CHECK(reason IN ('cli_inactivity_timeout', 'cli_timeout')),
    cli_version TEXT,
    watchdog_diagnostic TEXT NOT NULL
);
