# Agent exchange M2 validation: durable exchange log and doorbell (ADR-037)

Date: 2026-10-30. Source: branch `feat/agent-exchange`, M2 at `3d7ab74`,
gated on `133424b` (M2 plus the M1 report). The load rehearsal was rewritten
in `2ddcb7c`, and `d230cda` (M4) trimmed the append path. Compared with the
[baseline before M1](agent-exchange-baseline-2026-10-29.md).

This report approves no distribution.

## What M2 changed

- **Log (V62).** `exchange_streams` and `exchange_items` are additive. An item
  references its domain row (`payload_table`, `payload_id`) and never copies
  content. It is written in the transaction of its domain write and is
  append-only (a trigger refuses updates). Each stream has its own sequence;
  `global_seq` orders the whole database, and a project reads its items
  through `(project_id, global_seq)`.
- **Receipts.** An append is `recorded`, `duplicate` (the idempotency key
  exists, nothing is written) or `fenced` (the execution fence moved, nothing
  is written, and the caller's domain write aborts).
- **Writers.** In the domain transaction: pulsation transitions (named
  `checkpoint_opened`, `checkpoint_resolved`, `stage_failure_resolved` or
  `pulsation_transition`), stage executions, unit attempts, publication
  outcomes, CLI run states in both crates, finished Companion answers,
  drawings and resolved Scribe turns. Forge turns, documentation
  reconciliation and canvas requests span several transactions, so they record
  right after their last commit.
- **Doorbell.** A core thread reads the log every 250 ms and publishes
  `AppEvent::Exchange` for each new item, in order, with the project's
  previous item. No writer needs the event bus, and no doorbell announces an
  uncommitted item.
- **UI follower.** In-order doorbells advance the cursor. A gap or a lagged
  broadcast reads the range after the cursor in pages of 500 and reloads only
  the affected surfaces. A cursor ahead of the newest item, or more than 20
  pages behind, is a reset and asks for the full resync. Pulsation polling
  drops to every 5 s while the doorbell is healthy.
- **Terminal gaps.** `list_events` reads an indexed range, with up to 64
  earlier events as redaction context, when the run has a sanitization
  receipt; the Agents panel re-reads only the missing events. Legacy runs
  without a receipt still read the whole history.
- **Event inventory.** Every `AppEvent` variant has a class (durable with its
  stream kind, own sequence, ephemeral or doorbell); a new variant does not
  compile without one.

## Stage 7 gate

`scripts/gate-stage7.ps1` passed on `133424b` in 2,724.8 s, in the second
worktree, incremental over the M1 gate.

| Step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | Passed | 0.7 s |
| Crate dependency boundaries | Passed | 0.2 s |
| `cargo fmt` | Passed | 7.7 s |
| Clippy `-D warnings` | Passed | 67.4 s |
| Rust tests (`cargo test-all`) | 60 targets, 1,956 passed, 0 failed, 22 ignored | 2,036.6 s |
| GPUI release build | Passed | 604.1 s |
| Windows GPUI smoke | Window opened, graceful shutdown, exit 0 | 1.8 s |
| Rust licenses and sources | Passed | 1.4 s |
| Rust advisories | `cargo audit` exit 0, 0 vulnerabilities; 14 `unmaintained` and 2 `unsound` informational warnings | 4.6 s |

Against M1: 25 more passing tests and 2 more ignored ones (the load rehearsal
and the ranged re-read measurement, both opt-in). Binary distribution stays
blocked by `zed-55470`.

## Pulsation transitions

`exchange_baseline_pulsation_transitions`: 1,000 non-terminal transitions and
200 terminal ones, each one `IMMEDIATE` transaction with its commit, in the
debug test profile. The numbers of the baseline report (p95 11.41 ms and 3.97 ms) were
measured under other load and are not comparable: in an idle session the same
code gives about 3.2 ms and 3.6 ms. So each build was compiled the same way
(`cargo test -p hvc-core --lib --no-run` in the gate worktree) and the
binaries were run alternately on the same machine, three runs each.

| Session | Build | Non-terminal p95 | Terminal p95 |
|---|---|---:|---:|
| 1 | Baseline code (`dd9a321`) | 3.17 ms | 3.56 ms |
| 1 | M2 (`2ddcb7c`) | 3.91 ms (+23%) | 4.49 ms (+26%) |
| 2 | Baseline code (`dd9a321`) | 3.35 ms | 3.81 ms |
| 2 | M2 (`2ddcb7c`) | 3.81 ms (+14%) | 4.36 ms (+15%) |
| 2 | Final (`9526c77`) | 3.76 ms (+12%) | 4.42 ms (+16%) |

Values are the median of the three runs' p95. The append costs 0.4 to 0.9 ms
per transition at p95: under the 15 ms absolute cap, **over the 10% relative
ceiling** of the plan. Trimming one statement per append (`d230cda`) made no
measurable difference; the cost is the extra pages each commit writes (the
item, its three indexes, the stream and the AUTOINCREMENT counter) under the
rollback journal. This meets the ADR's own review criterion for transition
latency, so the decision is the developer's: accept the cost, or ask for a
deeper change such as fewer indexes or another journal mode.

## Load rehearsal

`exchange_m2_load_slow_subscriber`, opt-in, on `9526c77`: 100,000 terminal
events written in bursts of 2,000 and 1,000 pulsation transitions, with a
subscriber slower than the event bus. The bus lagged 14 times and the
terminal run had 7 gaps; the follower re-read 99,943 events by range, saw all
1,000 transitions and never reset. No full reload happened. (The first run,
on `2ddcb7c`: 23 lags, 10 gaps, 99,940 events re-read, 0 resets.)

## Terminal re-read

`exchange_m2_terminal_reread`: one page of 500 events at 100 cursors spread
over a 100,000-event run, on `9526c77`: p50 16.8 ms, **p95 23.0 ms** (ceiling
50 ms), max 48.3 ms. Before M2 a page cost about 1.6 s at any cursor,
because the whole run was read and redacted first.

## Rerunning

```powershell
cargo test --locked -p hvc-core --lib exchange:: -- --test-threads=2
cargo test --locked -p hvc-core --lib exchange_writers_tests -- --test-threads=2
cargo test --locked -p hvc-core --lib exchange_baseline_pulsation_transitions -- --ignored --nocapture --test-threads=1
cargo test --locked -p hvc-core --lib exchange_m2_ -- --ignored --nocapture --test-threads=1
```

Timings depend on the machine and on background load. Compare builds side by
side on the same machine, alternating runs, as above.
