# Trust layers (ADR-035 M2) — implementation and validation

Date: 2026-10-26. Branch `feat/mandates-layers-attestation`, implementation
`3fd7803` on top of the M1 commits (`8946570` … `c054af8`, see the
[M1 report](mandates-attestation-m1-validation-2026-10-26.md)). Decision:
[ADR-035](../docs/adr/035-execution-mandates.md). No migration. Nothing is
pushed.

This report qualifies the derivation of the four trust layers, their reader
and their display against hermetic fixtures (scripted model gateway,
temporary workspaces and databases). No real credentials, CLI sessions or paid
model calls were used. GitHub Actions was not used and is not evidence here.
The attestation footer arrives with ADR-036 (M3).

## Delivered behavior

- **Derivation.** A pure function (`orchestration::layers::derive_layers`,
  policy version 1) turns the host's facts into four layers, each with a
  state, a headline, up to 50 items (the rest counted) and the reasons that
  set the state. States are `clear` (nothing the host can detect, never
  "safe"), `attention`, `issue` (a grave fact already recorded; it blocks
  nothing new), `not_applicable` and `unknown` (the layer's main record does
  not exist yet). It reads no clock, file or database and sorts its inputs, so
  the same rows give the same snapshot.
- **Rules (v1).**

  | Layer | Attention | Issue |
  |---|---|---|
  | Identity | A completed run reported a model other than the request (the host's `verify_model`), or none; an approved CLI fallback moved a role to the API; a v4 pulsation past its Specification has no mandate | Never |
  | Scope | A sensitive path (ADR-034 globs) is granted or written; a unit result wrote outside its own destinations | The mandate refused a write or dispatch; a publication is in conflict; the stored mandate no longer matches its digest |
  | Guardrails | Less than 25% of an extra (repair) pool is left while the stage still runs; a usage record is partial; a required check is unavailable, not run, cancelled or a runner error; revision rounds are at the threshold minus one or beyond | The stage parked with its repair or retry budget spent and a unit still waiting; a required check failed or timed out |
  | Intent | Risk is high or unknown; a coverage row is unverified or rests on the model only; a criterion expects an executed check the Specification does not name | The Integrator found a criterion contradicted |

  Pipelines v1–v3 show Identity and Guardrails with what exists; Scope says
  "Not enforced (legacy pipeline)." and Intent "Criteria states unavailable
  (legacy pipeline).", both `not_applicable`.
- **Reader.** The detail snapshot reads the layers of the open pulsation only,
  in the same transaction as its composed execution and on that connection
  (the M1 lock lesson). A rejected answer keeps no model on its attempt, so the
  reader falls back to what the API router and the CLI recorded. Exhaustion is
  the waiting stage failure of the latest execution with class
  `budget_exhausted` or `retry_budget_exhausted`; the unit keeps its own error
  code.
- **Display.**
  - The Layers panel leads the pulsation Overview: one row per layer with an
    icon, a word and a colour, and the headline; a row opens to its reasons and
    items, with "N more" past 50. A note says what `clear` means.
  - The composed execution moved under a collapsed "Execution details".
  - The Contract, Test and Integrator briefs show a compact card; the
    Specification keeps its "What approving grants" card as its Scope, and the
    Integrator's Intent points at "Risk and coverage" instead of repeating it.
  - The header readout carries a dot in the worst flagged layer's colour, and
    its tooltip names the layer.
  - Nothing in these surfaces decides anything: rows open and close, nothing
    else.
- **Report.** A "Layers" section with the snapshot at render time; reports
  written before it still parse.

## Acceptance matrix

| Criterion (implementation plan) | Executed evidence |
|---|---|
| Every state comes from a documented host rule | `orchestration::layers` table tests: `identity_flags_a_model_other_than_the_request_or_none_reported`, `identity_flags_a_transport_fallback_and_a_missing_mandate`, `scope_follows_the_mandate_and_what_was_written`, `scope_without_a_mandate_says_why`, `guardrails_flags_an_extra_pool_below_a_quarter_while_the_stage_runs` (exactly 25% is clear, 24% is attention, a settled stage and a pool without a limit are clear), `guardrails_rates_required_checks_by_their_result` (every check status), `guardrails_flags_rounds_near_the_threshold_and_partial_usage`, `intent_follows_risk_coverage_and_verdicts`; every snapshot in those tests asserts that a flagged layer has a reason and only then |
| The same rows give the same snapshot | `the_same_facts_in_any_order_give_the_same_snapshot` (equal values and equal JSON after reversing every input list) |
| Legacy pulsations render `not_applicable` without error | `a_legacy_pulsation_marks_scope_and_intent_not_applicable`, `layers_persistence::tests::a_legacy_pulsation_reads_without_error` |
| An empty pulsation, 64 tasks and the item limit | `a_new_pulsation_is_unknown_and_flags_nothing`; `a_layer_keeps_fifty_items_and_counts_the_rest` (64 criteria, 257 grants, 200 publications) |
| Reader over real rows | `a_composed_pulsation_reads_its_mandate_writes_and_refusals`, `only_the_open_pulsation_carries_its_layers`, `a_tampered_mandate_is_a_scope_issue_and_the_detail_still_loads`, `the_detail_reads_its_layers_while_another_connection_writes` |
| From Intake to the Integrator | `trust_layers::the_layers_follow_a_composed_pulsation_from_specification_to_the_end`: before the approval Scope and Intent are `unknown`; after it Scope is in attention for the fixture's sensitive `src/auth.rs`; Intent turns to attention at Test (risk high) and at the Integrator (a verdict backed by the model only); nothing becomes an issue and Identity stays clear |
| Divergent observed model | `a_model_other_than_the_one_requested_puts_identity_in_attention` (the host refuses the answer and parks Code; Identity names both models and the flow-bar dot names Identity) |
| Budget near the end | `a_stage_that_spent_its_repair_budget_is_a_guardrails_issue` (two empty Code answers: 0% of the extra output allowance left and an exhausted, waiting unit) |
| Required check failing | `a_failed_required_check_is_a_guardrails_issue` (the fixture check fails: Guardrails issue, item `required · failed`) |
| No decision control | By construction: the panel, the card and the dot have no handler other than opening and closing a row; `the_layers_panel_leads_the_overview_and_opens_a_layer` opens and closes a row |
| UI: narrow width, long path, more than 50 items | `the_layers_panel_leads_the_overview_and_opens_a_layer` (960 and 1440 px, a 260-character path, items 0–49 and "N more", nothing past the panel); `the_header_marks_the_worst_layer_only_when_one_needs_a_look`; `the_integrator_brief_shows_the_layers_card`; `a_specification_brief_draws_no_layers_card` |
| Report golden updated, older reports readable | `the_layers_section_lists_each_layer_and_older_reports_still_parse`; `rendering_is_pure_and_matches_the_golden_file` with the regenerated golden (the word diff only adds the navigation entry and the section) |
| Under 5 ms for 64 tasks | `layers_benchmark` (below) |

## Findings during the implementation

No product defect. Two test findings:

- The header-dot test hung: its absence check opened a second window over the
  Pulsations view, whose split panes re-measure themselves forever across two
  windows (a limitation the layout tests already document). The check now runs
  on Home, where the header is the same.
- The "different model" scenario showed that the host refuses an API answer
  whose model differs from the request and parks Code instead of going on. The
  test now waits for that failure checkpoint, and the reader takes the
  observed model from the router's record because the refused attempt keeps
  none.

## Cost

`cargo run --locked --release -p hvc-core --example layers_benchmark` builds a
pulsation at the batch limits in a temporary database (64 tasks and criteria
in an approved Specification, 257 granted paths, 200 publications, 64 units
with two attempts each, 128 usage records) and times 200 reads after one
warm-up, with the release profile of the workspace:

| Read | p50 | p95 | max |
|---|---:|---:|---:|
| Inside an open transaction, as the detail reads it | 1.70 ms | 2.15 ms | 3.04 ms |
| `pulsation_layers` on a fresh connection | 4.90 ms | 5.69 ms | 7.54 ms |

The detail path is within the plan's 5 ms target. The standalone call pays for
opening a connection, where SQLite parses the whole schema on the first
statement (about 3 ms of the difference); the report and explicit callers use
it, the polled detail does not.

Two changes came from the measurement: the reader matches granted and written
paths against the sensitive globs in one pass, and the built-in glob set is
compiled once per process instead of on every read. Before them the read
inside a transaction took p50 2.6 ms and p95 5.5 ms (release without LTO, on
the same machine).

## Validation results

Environment: Windows 11 Pro 10.0.26300, Rust/Cargo 1.98.0, `--locked`,
`CARGO_BUILD_JOBS=6`, in the branch's own worktree.

The Stage 7 gate (`scripts/gate-stage7.ps1`) on `3fd7803` (2026-10-26
20:45–21:20 local time, 2,095 s) **passed**:

| Gate step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | PASS | 0.920 s |
| crate dependency boundaries | PASS | 0.285 s |
| cargo fmt | PASS | 6.022 s |
| Clippy (`-D warnings`) | PASS | 1.978 s |
| Rust tests: 54 targets, **1,617 passed, 0 failed, 16 ignored** | PASS | 1,520.488 s |
| GPUI release build | PASS | 554.413 s |
| Windows GPUI smoke (window opened, graceful shutdown, exit 0) | PASS | 1.946 s |
| Rust licenses and sources (560 packages for `x86_64-pc-windows-msvc`, none denied) | PASS | 2.257 s |
| Rust advisories (`cargo audit` exit 0, 0 vulnerabilities; 14 `unmaintained` and 2 `unsound` informational warnings; RustSec `3279c99`) | PASS | 6.278 s |

Against the M1 gate (`a85f46d`: 53 targets, 1,588 passed): one more target
(`trust_layers`) and 29 more passing tests, all of them new (15 in
`orchestration::layers`, 5 in `layers_persistence`, 1 report test, 4 in
`trust_layers` and 4 layout tests); the 16 ignored tests are the same opt-in
ones. The RustSec database moved to a newer commit; the warnings are the same
crates as before. The supply-chain summary still reports
`distributionAllowed: false` for the existing GPUI binary blocker.

`scripts/benchmark-ui-latency.ps1 -EnforceThresholds` on the gate's release
binary (idle scenario, 120 samples after 20 warm-up samples): frame-callback
interval p95 7.00 ms and p99 7.24 ms against the 8.33 ms and 16.67 ms
thresholds, **passed**.

`Cargo.toml` files and `Cargo.lock` are unchanged.

## Limits

- Hermetic fixtures only. No real CLI or provider run, and no Linux headless
  run of this branch.
- The panel, the card and the dot are covered by GPUI layout tests, not by a
  manual visual pass in the running application; that pass belongs to M5.
- `benchmark-ui-latency.ps1` opens the application on an empty data
  directory, so it shows that the shell did not get slower, not the cost of a
  pulsation with layers.
- `clear` only reports what the host can detect from its own rows. The layers
  read model identities and check results the host recorded; they do not
  re-verify them.
