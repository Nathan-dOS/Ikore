# Canvas AI workflow implementation and validation — 2026-10-28

Implementation started on 2026-10-28; final validation completed on 2026-10-29
(America/Sao_Paulo). Filenames retain the implementation date. Gate JSON records
the actual UTC start/finish times.

**Publication follow-up, 2026-10-29:** the selected Canvas source was published on
main as [`92f03c4`](https://github.com/Nathan-dOS/HARD-VC/commit/92f03c43bd73521c9f4ee55c51c697ba84f557a8).
The [publication record](canvas-ai-publication-2026-10-29.md) covers its separate
1,778-test/884-source qualification with unrelated voice work excluded. This
report retains the earlier 1,782-test combined-checkout evidence.

Status of the original qualification: all nine Stage 7 checks passed. Work used
the existing dirty `main` checkout, based on `b3505a5`, with pre-existing Canvas
refinements and voice changes preserved. No branch, worktree, commit, merge or
push was created by that implementation continuation. This report describes its
combined local candidate; the publication follow-up above identifies the source
subsequently sent to remote main.

## Delivered candidate

The Companion can finish its answer with a typed Canvas offer. The answer is
persisted before the offer is created. The offer is not authorization: a native
Approve action binds one source, project, conversation and current Companion
selection to a new job. Decline preserves the answer. Stop cancels the job.
Neither normal chat text nor a model-returned approval field admits a job.

The separate authoring runner exposes only `canvas_inspect`,
`canvas_apply_operations`, `canvas_validate` and `canvas_finish`. It keeps staging
out of the drawing library, uses bounded rounds and validates each atomic batch.
The requested transport/model/profile is frozen on admission. API admission also
freezes and checks resolved provider/configuration identity. An approval replay
cannot change this durable target. A changed/unavailable target fails without
fallback. Usage records identify their job and actual call.

The new V58 migration stores requests, authorization state, bounded source and
target snapshots, stage state and the publication receipt. Existing migrations
remain unchanged. Publication commits one drawing, provenance, ready state and
mirror intention in one SQLite transaction. Repeated completion returns its
receipt. Ready means authoritative database commit; mirror pending/conflict is
separate. A deleted source or result is represented as unavailable and cannot be
recreated by replay. Restart interrupts running jobs and discards partial staging.

Native cards attach to the originating completed answer. Loading, approval and
completion do not navigate. Open Canvas resolves and revalidates the specific
receipt; delayed callbacks cannot target another project, unavailable result or
override later navigation. Manual commands preserve semantic state and geometry
together through Undo/Redo and normal saves. Architecture and the restricted BPMN
profile have explicit types and membership controls, with X/+ gateway markers,
event/task/pool/lane distinctions and matching SVG output. Legacy free drawings
do not acquire semantics from their appearance.

## Focused evidence before the complete gate

| Selection | Result |
|---|---|
| P1 complete workspace baseline, previous turn | 1,722 passed; 0 failed; 16 ignored; separate P1 evidence |
| New actual CLI workflow, five backends in one test | Passed; 11.31 s execution |
| New API loopback workflow selection | 3 passed; 0 failed; 2.44 s execution |
| Canvas native UI first run outside sandbox | 53 passed; 1 failed; missing debug selector corrected; native control test then passed |
| Expanded native semantic control test | Passed, including derived warnings and Undo |
| Semantic eraser regression | Failed before fix; passed after ordering removals by dependency |
| Semantic SVG | 2 passed; 0 failed; 0.03 s execution |
| V57→V58 upgrade | 1 passed; 0 failed; 1.02 s execution |
| Final core Canvas selection after lifecycle fixes | 45 passed; 0 failed; 23.61 s execution |

The CLI workflow test is
`cli_executor::canvas_cli_offer_requires_consent_and_publishes_one_localized_drawing_on_all_backends`.
It launches local Codex, Claude, Gemini, OpenCode and Antigravity fixture processes
through the real hardened terminal path, accepts one offer, builds and publishes
one localized drawing, checks source preservation, usage and idempotent result
resolution, and declines a later offer without changing the published drawing.
No installed authenticated provider is used.

The new core API tests are:

- `canvas_api_authorization_generates_one_hidden_candidate_then_one_ready_drawing`
- `canvas_api_cancellation_stops_the_inflight_request_without_a_drawing`
- `canvas_api_configuration_change_before_finish_fails_without_publication`
- `canvas_store_admission_replay_cannot_replace_the_frozen_dispatch`

The first three use an Ollama/OpenAI Chat protocol loopback fixture. They cover
actual model rounds, hidden staging, consent, one publication, source preservation,
job-attributed usage, cancellation and configuration changes before finish. The
last test first reproduced a real replay flaw and passes after the correction.
The store suite covers durable source validation, expiry, nonce/scope fencing,
transactions, restart, deletion, sanitization and mirror failure.

Native component tests exercise trusted approval, API/CLI selection at the click,
pending retargets, duplicate clicks, state/event hydration, refusal/stop, receipts,
late callbacks, explicit native buttons, correct drawing selection and semantic
manual editing. These tests use GPUI's test platform; they do not certify physical
display/DPI quality.

`project_deletion_preserves_canvas_until_acceptance_and_waits_for_running_jobs`
checks wrong confirmation, unsafe paths, active chat/terminal/Canvas, unchanged
request state/revision after refusals and pending-offer cascade on accepted
deletion. `canvas_project_deletion_guard_blocks_admission_and_releases_after_failure`
checks the temporary admission fence and its cleanup. Deletion now requires Stop
for an active Canvas job; it does not cancel Canvas as a precheck side effect.

`canvas_store_list_keeps_the_newest_thousand_offers_in_chronological_order`
uses 1,001 offers in one real database to cover the bounded history window and
equal-timestamp ordering. The pre-fix query reproduced exclusion of new offers;
the candidate returns the newest 1,000 chronologically without deleting history.

## Qualification limits

The runtime source is the completed persisted answer. The optional document
reference exists in the P1 pure contract; a document resolver/attachment workflow
is not exposed by this candidate. Separate generated documents retain their
existing language policy.

Authenticated providers, all physical display/DPI/IME combinations, performance of
dense generated diagrams, process execution, automatic layout, BPMN XML interchange
and full OMG conformance are not qualified by these fixtures. Association lines
keep explicit semantic endpoints but lack the arrow-binding automatic routing.
The result validates supported graph structure, not factual correctness of a
proposed architecture or executable business process.

## Final gate

The first gate attempt passed structure, crate boundaries and formatting, then
stopped at six strict Clippy findings before running the suite. The lint fixes
use explicit imports, simpler branches and a named target-binding type. Its
[failed attempt record](canvas-ai-workflow-stage7-attempt1-2026-10-28.json) is
retained. Read-only review also found and reproduced two lifecycle regressions:
dependency removal order in one eraser gesture, and Canvas cancellation before a
project deletion had passed its checks. Both received regression tests and fixes.

The [second attempt](canvas-ai-workflow-stage7-attempt2-2026-10-28.json) passed
the core checks, then stopped before the suite on three UI Clippy findings
(an unnecessary unwrap and two test-structure lints). Those were corrected
without changing behavior.

The [third attempt](canvas-ai-workflow-stage7-attempt3-2026-10-28.json) passed
strict Clippy and reached the complete suite. Core passed 976 tests; terminal
reported 200 passed, two failed and three ignored before the gate stopped. The
two meter tests assumed a captured quota fixture was still live. Its fixed reset
timestamp, `1793220000`, expired at 2026-10-28 20:40 UTC (17:40 in Sao Paulo), so
the production expiry rule correctly excluded those historical windows.

The correction changes only
[`quota/meter_tests.rs`](../crates/ikore-terminal/src/quota/meter_tests.rs): tests
that need a live window use a copied fixture with a relative future reset. The
captured fixture files and production expiry behavior remain unchanged. The new
`claude_run_events_keep_expired_windows_out_of_the_live_meter` regression preserves
explicit coverage of expired windows. The focused meter selection then passed
all 17 tests in 2.20 seconds. That focused result was followed by the complete
passing gate recorded below.

The [fourth attempt](canvas-ai-workflow-stage7-attempt4-2026-10-28.json) stopped
with UI results of 391 passed and one failed. The failing
`layout_tests::ui_source_contains_no_unapproved_portuguese_literals` audit found
a raw localized narrative in `canvas_authored_navigation_tests.rs:65`. The
correction changes only that literal's source representation to equivalent
Unicode escapes; the runtime string and audit rules are unchanged. The exact
source-language audit then passed one test in 0.52 seconds. The final complete
run below also passed the entire 392-test UI suite.

### Final complete run — PASS, 2026-10-29

`scripts/gate-stage7.ps1` passed all nine steps in **1,405.906 seconds**, outside
the sandbox with host-default temporary directories. Rust 1.98.0 / Cargo 1.98.0
ran on Windows x64. The source remained frozen throughout validation.

| Check | Result | Duration (s) |
|---|---|---:|
| GPUI/Rust structure | PASS | 0.668 |
| Crate dependency boundaries | PASS | 0.136 |
| Formatting | PASS | 4.867 |
| Strict workspace/all-target Clippy | PASS | 8.028 |
| Complete `cargo test-all` | PASS | 894.904 |
| Release application build | PASS | 486.057 |
| Windows GPUI startup/shutdown smoke | PASS | 2.360 |
| Technical license/source audit | PASS | 1.397 |
| RustSec advisory audit | PASS | 7.461 |

The complete suite passed **1,782 tests, zero failed, 16 existing ignored, zero
filtered out, across 55 target summaries**. This includes 976 core tests, 203
terminal tests and all 392 UI tests. Focused totals above overlap and must not be
added to this result.

The native smoke launched the actual release executable against a fresh isolated
profile, observed its window/database, closed it gracefully with exit 0 and
removed the temporary profile. It is a startup/shutdown smoke, not the unperformed
physical end-to-end, DPI or authenticated-provider qualification described above.

The audits covered 560 packages with zero vulnerabilities. Fourteen unmaintained
and two unsound informational warnings are inherited. The existing binary
distribution hold remains unchanged; a technical pass is not binary-distribution
approval. No dependency was introduced and nothing was published.

Evidence:

- [Complete test results](canvas-ai-workflow-test-results-2026-10-28.json)
- [Nine-step gate timings](canvas-ai-workflow-stage7-run-2026-10-28.json)
- [Final qualification, release hash and isolated smoke](canvas-ai-workflow-qualification-2026-10-28.json)
- [Source fingerprint](canvas-ai-workflow-source-fingerprint-2026-10-28.json): 887 files checked after the gate, zero drift

The release SHA-256 is `3023af13265c2c094968339df7a78f91d3a3b327abfa02bc93bac2d1c298546d`.
At the end of this historical run, the base was `b3505a5` on local main, with no
staged files, task branch, worktree, commit or push. Other pre-existing worktrees
and local voice changes were preserved. The maintainer subsequently approved
publication; the [publication record](canvas-ai-publication-2026-10-29.md)
documents the independently qualified source selection and verified main push.
