# Plan-quota fixtures (ADR-028)

Synthetic replies in the shapes observed on 2026-10-21 (spikes S1, S2, S4) with Codex 0.163.0
and Claude Code 2.1.302. Every identifier, address and number is invented; no reply body from
a real account is stored here.

- `codex/app-server-rate-limits.json` - `account/rateLimits/read` result of `codex app-server`.
- `codex/rollout-token-count.jsonl` - a rollout `token_count` record carrying `rate_limits`.
- `codex/usage-endpoint.json` - the Codex usage endpoint reply (only `plan_type` and `rate_limit` are read).
- `claude/usage-endpoint.json` - the Claude usage endpoint reply (only the named windows are read).
- `claude/rate-limit-event.jsonl` - a `rate_limit_event` frame of `--output-format stream-json`.