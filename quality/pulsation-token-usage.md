# Tokens per pulsation — 2026-09-10

## Delivered behavior

`Pulsations → Overview` shows all-time input, output and total generation tokens
for the selected pulsation, with a model breakdown. Its query is independent of
the Settings filters. `Settings → Usage` adds `Tokens per pulsation`, ordered by
reported total descending, with expandable model rows. The existing project,
transport and UTC period filters apply to that comparison. `Usage without
pulsation` accounts for the remaining generation usage in the same snapshot.

All new interface text is English. Empty history, supplier-reported zero,
unavailable counters and partial coverage remain distinct. Model tables scroll
horizontally within narrow panels. Selecting another pulsation clears the old
summary; refreshes retain the existing card geometry. Reads run on a blocking
worker, reject obsolete selection/generation results, and refresh on entry,
selection, runtime snapshots, manual refresh and coalesced completion events.

## Attribution and retention

V29 adds nullable `pulsation_id` and a pulsation/time index to the V28 ledger,
without a foreign key or cascading deletion. API routing copies the request's
pulsation ID; terminal accounting copies the run's ID. Companion and connection
tests remain unassigned. The existing accounting-unit key and native counter
normalization are unchanged; cache and reasoning breakdowns are not added twice.

Totals include every explicitly linked attempt, retry, reexecution, failed or
cancelled call with reported evidence, and associated Learning calls. Learning
can increase the total after a pulsation reaches a terminal state. No extra
provider request, tokenization, cost estimate or billing integration is added.

The migration attributes existing ledger records through their API execution,
terminal run, or legacy agent job. It does not import additional counters. A
previously deleted source cannot be reconstructed and remains unassigned. Later
source/project deletion retains both counters and attribution. The current
pulsation name is resolved when reading; a missing pulsation is displayed as
`Deleted pulsation` with its durable ID. Names and prompt content are not copied
into the ledger.

`UsageQuery` adds an optional pulsation filter; `UsageSummary` adds per-pulsation
totals/models and unassigned totals. All aggregates use the same SQLite read
transaction. Model breakdowns are grouped in one query, avoiding a query per
pulsation and avoiding joins that multiply accounting units.

## Validation

Targeted accounting tests cover two projects/pulsations, API and CLI, retries,
models, time/transport/project filters, idempotency, unassigned usage, deleted
pulsations, and missing/partial/zero counters. V28-to-V29 upgrade tests verify all
three attribution sources, unrecoverable history, repeated initialization and
retention after source/project deletion. Existing native API/CLI accounting tests
remain applicable.

GPUI tests cover independent Settings filters, rapid selection changes, stale
results after reset, read failure, expanded model rows, refresh geometry and
960×640 panel bounds. These are automated layout checks, not native visual QA.

See also [base usage implementation](token-usage-implementation.md) and
[CLI threat model](cli-threat-model.md). Documentation follows ADR-020.

## Verification results

- Targeted core usage tests: 17 passed.
- Targeted GPUI usage tests: 4 passed, including pulsation selection/reset and 960x640 layout coverage.
- Usage integration tests: 3 passed, including V28-to-V29 attribution and deletion retention.
- Strict Clippy reports the existing `collapsible_if` at `persistence_service.rs:4686`, outside this change. Clippy for core/UI and all targets passed with only that rule allowed (`-D warnings -A clippy::collapsible_if`).
- The existing transitive `proc-macro-error2` future-compatibility warning remains.

### Final workspace and release verification

- All workspace test groups were verified: 659 passed, 2 opt-in/manual tests ignored. The initial serial workspace run exposed two upgrade assertions still expecting schema 28; they now expect 29. The affected upgrade group and groups not reached by that run were executed separately and passed. Already-passing groups were not unnecessarily repeated.
- Core: 435 passed / 1 ignored; UI: 145 passed. The three usage integration tests passed, as did serial upgrades, serial worker, failure checkpoints and workspace flow. Workspace doc tests passed (no runnable cases).
- Focused rustfmt checks and `git diff --check` passed. Unrelated existing changes were preserved.
- `cargo build -p hvc-app --release --locked --offline` passed.
- Executable: `C:/Dev/Hard-Vibe-Code/target/release/hard-vibe-code.exe` (24,567,296 bytes).
- SHA-256: `340f1a39c165a860e8725930642209689f5460cd2f46338fdcc7f27ad2e5be53`.

No commit or push was created for this extension.
