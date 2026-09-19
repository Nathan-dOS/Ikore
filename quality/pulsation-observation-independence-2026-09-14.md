# Pulsation observation independence — 2026-09-14

## Findings and implemented contract

Baseline: `beabe6a`. Investigation used current code and a read-only connection to the local application database. The TaskSpecifier/OpenCode run ending at 2026-09-14 08:23 local time stopped at approximately 240 seconds. Both traditional and v4 request constructors passed the role base as the caller deadline; TerminalService used that value as the watchdog ceiling. This is a confirmed timeout-budget defect. The database contains no view-navigation history, so it cannot establish that the Pulsations view caused that failure.

- Both stage dispatchers now carry a CLI-specific initial policy and absolute deadline. API request/route/attempt limits remain independent. Explicit smaller CLI ceilings and callers without the override remain bounded.
- Transport reads own activity; recognized stdout transitions own reasoning. Audit drain and stderr bookkeeping cannot create semantic delivery. The absolute ceiling remains effective under continuing output.
- A bounded channel feeds a sequential blocking audit worker, retaining ordering, redaction, usage recording and persist-before-publish semantics. Database/worker failures signal process cancellation. Final completion follows drain; diagnostics capture termination timing before that drain.
- Periodic and manual runtime snapshots share one running read and one pending request. Their task is separate from mutations. Revision-keyed details and coalesced usage avoid repeated reads; stale completions cannot replace a newer project, selection or filter.
- Lag recovery invalidates scoped observations rather than restoring the session. Hidden auxiliary panels load on entry; runtime counts and review notices continue across all tabs.
- One main slot, one background slot, v4 child caps, provider priorities, admission fences, budgets and HITL checkpoints remain in force. No database migration or new OS process-priority setting.

## Validation

| Check | Result |
| --- | --- |
| Core unit suite (`cargo test --offline -p hvc-core --lib`) | 562 passed, 1 ignored, before unrelated concurrent provider edits |
| UI unit/layout suite (`cargo test --offline -p hvc-ui --lib --quiet`) | 162 passed, including current concurrent Settings tests; subsequent added real-core test recorded below |
| CLI executor | 9 passed, 1 ignored |
| Composition CLI | 1 passed |
| Composition parallel | 3 passed |
| Composition pipeline | 1 passed |
| Orchestration baseline | 9 passed |
| Real core + all eight GPUI views | Pending final verification |
| Native visible/unfocused/minimized smoke | Pending native tool availability |
| Scoped formatting, diff review and Clippy | Pending final verification |

The new core regressions exercise SQLite write-lock contention on a single-thread Tokio runtime, an unconsumed/lagged event bus, audit persistence failure, old queued events not renewing activity, reasoning plus stderr, and continuous activity reaching the real ceiling. Real subprocess fixtures exercise completion after the API/initial allowance and termination at a deliberately shorter CLI ceiling. Existing process-tree cancellation coverage also runs.

UI regressions cover unchanged artifact revisions, overlapping snapshot requests and selection changes, per-resource coalescing, stale usage, and repeated navigation across Agents, Pulsations, Forge, Workspace, Chat, Vault, Logs and Settings. The real-core test repeats a scripted local-provider pulsation beneath each of these views, requiring the same four explicit fixture approvals.

Reproduction logs are under `target/tab-independence-*.log`. `cargo run --offline -p hvc-ui --example pulsation_observation` launches a disposable native instance using CoreTestHarness and a loopback scripted provider. Only its own synthetic checkpoints are auto-approved; it never uses installed CLI credentials. The fixture writes `target/tab-independence-native-report.json`; completion by itself does not prove focus/minimize state.

## Qualification boundaries

No authenticated external-provider reliability, model quality, lower paid latency or absence of every possible tab-related performance problem is claimed. The defect above and regressions are established independently of the original anecdotal correlation. Baseline-versus-patch native latency benchmarking has not been performed. Concurrent provider/catalog/Settings work in this checkout is outside this patch and is preserved. No commit, push or replacement of the running user instance was requested.
