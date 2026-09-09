# Token usage accounting — 2026-09-09

## Delivered behavior

`Settings → Usage` displays generation tokens recorded by HVC across projects.
The default is all projects, API + CLI, all time. Filters select one project,
API or CLI, and the last 7 or 30 UTC calendar days. Cards show input, output
and total tokens; a chronological chart and model table expose the breakdown.
Model attribution distinguishes observed, requested and unknown models.

This feature does not query account-wide billing, tokenize text, estimate absent
usage, or make additional model requests. Embeddings are outside generation usage.
Deleting conversations, terminal history, runs or projects preserves the ledger.
Deleted project identifiers remain filterable. The panel refreshes on entry,
manual refresh and coalesced generation completion/failure events. SQLite reads
run on a Tokio blocking worker; generations discard superseded results.

## Accounting contract

`V28__token_usage_records.sql` adds a content-free ledger without cascading
foreign keys. `(source_kind, source_id, unit_key)` identifies an accounting unit.
Snapshots replace the same unit rather than adding copies. API attempts and
Companion model calls own separate units, including retries, repairs and tool
rounds. Terminal runs own CLI units; Companion CLI uses its model-call identity,
so the same operation is never also counted as a terminal run. Connection tests
are included globally, without a project.

| Transport | Native evidence | Normalization |
| --- | --- | --- |
| Responses API | `usage` in JSON / response SSE envelope | Input includes cached input; output includes reasoning. |
| Chat Completions API | `usage`, including the final usage-only SSE chunk | Requests `stream_options.include_usage` for supported streaming profiles. Cache/reasoning remain breakdowns. |
| Anthropic Messages API | Initial message and final delta usage | Input is uncached + cache read + cache creation. Cumulative output snapshots replace previous snapshots. |
| Codex CLI | `turn.completed.usage` | Input/output per invocation turn; an unfinished started turn remains unavailable. |
| Claude CLI | `result.modelUsage`, falling back to `result.usage` | Per-model snapshot takes precedence over global totals. Cache components are included once; fallback is partial because subagent coverage is unknown. |
| Gemini CLI | `result.stats.models`, falling back to aggregate stats | Per-model snapshot takes precedence; aggregate-only evidence is partial. |
| OpenCode CLI | `step_finish.part.tokens` | Stable part ID deduplicates steps; add cache to uncached input and reasoning to non-reasoning output. Missing IDs retain one partial unit. |
| Custom / missing events | No supported usage contract | Unavailable, never an estimate or fabricated zero. |

Known values from incomplete calls contribute to totals, with partial coverage
shown explicitly. Missing values remain SQL NULL; a supplier-reported zero is
stored as zero. Optional cache/reasoning fields stay unavailable when absent.
API cancellation retains the last in-memory snapshot; CLI evidence is persisted
as frames arrive. Abrupt API process termination before final persistence may
lose that call's usage. These counters are observability, not a billing ledger.

The migration imports existing `llm_attempts` and API `chat_model_calls` once.
Legacy agent runs are imported only when neither routed nor terminal evidence
exists. Message/run projections are excluded. Historical values are partial
because cache normalization and coverage cannot be reconstructed reliably.
CLI history is not reconstructed; collection begins with this version.

## Data handling

Only counters, source identifiers, provider/model identifiers, transport,
project identity, timestamps and coverage are added. Provider-owned unit keys
are hashed; accepted model identifiers are bounded and pass the existing secret
redactor. Prompts, responses, tool arguments and credentials are not copied into
this ledger. Existing cost calculations retain their original contracts.

## Validation

Tests use isolated
databases, loopback HTTP and synthetic CLI executables. Authenticated provider
smoke is opt-in and was not run for this feature.

| Check | Result |
| --- | --- |
| Serial `cargo test --workspace --locked --offline --quiet` | 642 passed, 0 failed, 2 ignored (manual prompt inspection and authenticated Codex smoke). |
| Final targeted `cargo test -p hvc-core -p hvc-ui usage:: --locked --offline` | 17 passed, including malformed/large CLI frames and populated panel geometry at 960×640. |
| Loopback protocol and V27→V28 integration tests | 2 passed in the workspace suite. |
| Synthetic CLI process and Companion regressions | 7 passed, 1 opt-in authenticated smoke ignored. |
| `cargo build -p hvc-app --locked --offline` | Passed. |
| Clippy with `-D warnings -A clippy::collapsible_if` | Passed; strict Clippy is blocked by a pre-existing nested `if` in `persistence_service.rs`. |
| Formatting | Feature files formatted. Workspace-wide check still reports pre-existing formatting in `orchestration/coordination_tests.rs`, `orchestration/failure.rs` and `persistence_service.rs`. |
| `git diff --check` | Passed. |

The existing transitive `proc-macro-error2` future-compatibility warning remains.
Unrelated lint/formatting changes were not mixed into this feature.

Coverage includes migration deduplication/deletion retention, idempotent API/CLI
snapshots, missing versus zero, normalized cache/reasoning, model provenance,
project/transport/UTC filtering, interrupted SSE, large CLI result frames,
identifier redaction, CLI resume and Companion summaries, persistence failure
classification, and GPUI panel mounting/stale response rejection.

Native inspection was attempted with an isolated database and the current debug
build. The hidden HVC test window was not returned as a targetable window by
Computer Use, so physical Windows rendering is not certified by this run.

## Code and source trace

- `crates/hvc-core/src/usage.rs`, `usage/api.rs`, `usage/cli.rs`: ledger and parsing.
- `crates/hvc-core/src/llm/{adapter,streaming,router}.rs`: routed generation and connection tests.
- `crates/hvc-core/src/chat_service.rs`, `terminals.rs`: Companion, pipeline, manual and FreeForm CLI capture.
- `crates/hvc-ui/src/usage.rs`, `settings_view.rs`, `shell.rs`, `app_model.rs`, `choices.rs`: panel, filters and refresh.
- `crates/hvc-core/src/usage/tests.rs`, `crates/hvc-test-support/tests/token_usage.rs`, `tests/cli_executor.rs`: regressions.
- [Codex non-interactive output](https://developers.openai.com/codex/noninteractive/).
- [Claude result and per-model usage scopes](https://code.claude.com/docs/en/agent-sdk/cost-tracking).
- [Gemini headless output](https://geminicli.com/docs/cli/headless/).
- [OpenCode session token normalization](https://github.com/anomalyco/opencode/blob/dev/packages/opencode/src/session/session.ts).

Unrelated local changes are retained. No commit or push was requested or performed.
