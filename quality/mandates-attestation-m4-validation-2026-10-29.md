# Just-in-time scope grants (ADR-035 M4) — implementation and validation

Date: 2026-10-29. Branch `feat/mandates-layers-attestation`, implementation
`363ac1d` (and the unrelated fixture fix `9396f3f`) on top of the M3 commits (`dc1f3bb` … `8ed8f43`, see the
[M3 report](mandates-attestation-m3-validation-2026-10-26.md)). Decision:
[ADR-035](../docs/adr/035-execution-mandates.md). Migration V57. Nothing is
pushed.

This report qualifies scope requests, their grant at the failure checkpoint
and their display against hermetic fixtures (scripted model gateway,
temporary workspaces and databases). No real credentials, CLI sessions or
paid model calls were used. GitHub Actions was not used and is not evidence
here.

## Delivered behavior

- **Request.** A Code or Test unit that must change a delivered file it does
  not own fails with `scope_insufficient` and lists the file's refs in
  `relatedRefs`; the multi-unit Code and Test prompts say so in one sentence.
  A pure derivation (`orchestration::mandate::scope`) resolves each ref
  against the frozen plan and refuses, with a code, a ref that is not an
  existing file delivered to the unit, a role the stage never writes
  (production for Code, tests for Test), a protected name or `REVIEW.md`, a
  stage document, a file another stage or unit produced, a file another unit
  owns, and a file the unit already owns. It records the delivered digest of
  each accepted file and the risk level before and with them. Past eight
  accepted files, two requests in the stage's shared budget account or a
  retry the budget cannot reserve, the request is not offered and the
  diagnostic says why.
- **Checkpoint.** A grantable request is kept with the failure checkpoint the
  stage parks on (`model_reported_failure`, as before). The Review tab shows
  a "Scope request" card: unit and stage, each file with its digest and a
  "sensitive" mark, what was refused and why, the agent's reason (sanitized,
  labelled model-written, 400 characters at most), the risk before and after
  and the extra invocations left. "Grant and retry" (`g`) leads the decision
  bar, next to the unchanged Revise and Abort, and the decision explanation
  says what it does. The notice reads "Code needs write access to N file(s)".
- **Grant.** `ScopeGrantCommandV1` (new, versioned; `StageRecoveryCommandV1`
  is unchanged) runs in one immediate transaction: the freshness checks of
  the failure decision, the request still pending, the derivation re-derived
  to the same digest, and every file's current hash equal to the delivered
  one. A request that fails any of them becomes `stale` and nothing is
  granted. Otherwise a `scope_grant` mandate revision adds each file as an
  amendment of the unit, the receipt records `grant_scope` with the files,
  and the existing Retry resumes the job on the shared budget.
- **Next plan.** The stage's preparation reads its granted requests and adds
  each file to the unit's destinations, with the anchors its edits need;
  validated siblings are reused by their keys as before.
- **Decline and supersede.** Any other decision declines a pending request
  with that action; a revision of the stage supersedes a granted one, so its
  plans no longer give the unit the file.

## Acceptance matrix

| Criterion (implementation plan) | Executed evidence |
|---|---|
| A grant never adds a path the worker was not delivered, a protected path, a production path to a Test unit, or a path with another owner | `scope_tests::each_refusal_rule_names_its_code` (every refusal code), `a_mixed_request_keeps_only_the_admissible_files_and_their_delivered_bytes`, `a_test_unit_is_granted_test_files_only`; `scope_escalation::a_test_unit_asking_for_a_production_file_gets_no_request` |
| After the grant the pulsation stays in the stage, the shared budget pays the retry and validated units are reused without a call | `scope_escalation::a_granted_file_becomes_the_units_own_and_is_published` (two Code executions on one budget account, an extra reservation held, two `code_implementer` calls, the file published, no enforcement event) |
| A file changed after the request is refused without any change | `a_file_changed_after_the_request_is_not_granted` (the error names the file, the request is `stale`, the mandate keeps one revision, the checkpoint stays open and Abort still works) |
| At most two requests per stage execution, and the UI shows only what the host persisted | `the_request_is_offered_within_its_limits_and_the_budget_only` (second offered, third refused, nine files refused, eight fit, no budget refused); `a_revision_after_the_grant_supersedes_the_request` (a new budget account counts afresh); layout and state tests below |
| Risk delta | `the_risk_after_counts_the_granted_files`; the card's risk line in the layout test |
| Budget condition | `the_request_is_offered_within_its_limits_and_the_budget_only` |
| Crash between the grant commit and the dispatch | `a_restart_between_the_grant_and_the_dispatch_resumes_the_stage` (the application is closed, the grant leaves the job pending at generation 2, a restart with recovery finishes the pulsation and publishes the file) |
| A human revision after the grant supersedes the request | `a_revision_after_the_grant_supersedes_the_request` (the Test review sends Code back; the first request is `superseded`, the unit asks again under a new budget) |
| Decline | `another_decision_declines_the_request_with_its_action` |
| V57 | `v56_to_v57_adds_scope_requests_without_touching_parked_failures` (nothing rebuilt, including the failure rows; one request per checkpoint; closed stages, states and decisions; decided once; a grant names its revision; cascade) |
| UI: card layout, failure checkpoint keyboard, stale message | `a_scope_request_shows_its_files_and_leads_the_decisions_with_the_grant` (960 and 1440 px, a 260-character path, the grant first, no plain Retry); `a_stale_scope_request_says_so_and_offers_no_grant`; `granting_without_a_core_reports_why_and_keeps_the_checkpoint`; `failure_checkpoint` state tests; `every_bound_shortcut_on_the_page_is_a_registered_key_binding` (the `g` binding) |

## Findings during the implementation

- **The plan's column on `stage_failure_requests` broke six upgrade tests.**
  `ALTER TABLE ... ADD COLUMN` rewrites the table's stored SQL, and every
  earlier upgrade test checks that no table changed. The request already
  names its checkpoint (unique), so V57 keeps the link there and changes no
  existing table.
- **One grant per unit in practice.** Every Retry is a new execution, so the
  two-request limit counts per shared budget account. The retry a grant
  starts takes the unit's single extra invocation, so a second request from
  the same unit is refused for budget before the limit applies; a second
  request in a stage comes from another unit.
- **A date in a quota fixture expired (not M4).** The first gate run on
  `363ac1d` failed two `hvc-terminal` meter tests: the Claude rate-limit
  fixture said both windows reset in October 2026, and since the weekly reset
  passed on 2026-10-28 the meter reads the frame as expired. The fixture's
  reset times are in 2100 now (`9396f3f`, test data only); the main branch
  carries the same fixture and fails the same way.
- **Granted files come from the requests, not the mandate.** Reading the
  amendments from the mandate would keep granting a file after a revision,
  because only a new Specification approval re-issues the mandate. The plan
  reads the requests in state `granted`, and a revision supersedes them.

## Validation results

Environment: Windows 11 Pro 10.0.26300, Rust/Cargo 1.98.0, `--locked`,
`CARGO_BUILD_JOBS=6`, in the branch's own worktree.

The Stage 7 gate (`scripts/gate-stage7.ps1`) on `9396f3f` (2026-10-29
08:24–09:00 local time, 2,189 s) **passed**:

| Gate step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | PASS | 0.826 s |
| crate dependency boundaries | PASS | 0.210 s |
| cargo fmt | PASS | 11.472 s |
| Clippy (`-D warnings`) | PASS | 35.521 s |
| Rust tests: 56 targets, **1,667 passed, 0 failed, 17 ignored** | PASS | 1,497.176 s |
| GPUI release build | PASS | 630.620 s |
| Windows GPUI smoke (window opened, graceful shutdown, exit 0) | PASS | 2.545 s |
| Rust licenses and sources (560 packages for `x86_64-pc-windows-msvc`, none denied) | PASS | 2.062 s |
| Rust advisories (`cargo audit` exit 0, 0 vulnerabilities; 14 `unmaintained` and 2 `unsound` informational warnings; RustSec `40f5be6`) | PASS | 7.986 s |

Against the M3 gate (`f98c5a2`: 55 targets, 1,648 passed, 17 ignored): one
more target (`scope_escalation`) and 19 more passing tests, all of them new
(7 in `mandate::scope_tests`, 6 in `scope_escalation`, 1 upgrade test,
3 layout tests and 2 `failure_checkpoint` tests); the ignored tests are the
same opt-in ones. The run before it, on `363ac1d`, stopped at the two quota
meter tests described above. The RustSec database moved to a newer commit;
the warnings are the same crates as before. The supply-chain summary still
reports `distributionAllowed: false` for the existing GPUI binary blocker.

`scripts/benchmark-ui-latency.ps1 -EnforceThresholds` on the gate's release
binary (idle scenario, 120 samples after 20 warm-up samples): frame-callback
interval p95 6.74 ms and p99 7.26 ms against the 8.33 ms and 16.67 ms
thresholds, **passed**.

`Cargo.toml` files and `Cargo.lock` are unchanged.

## Limits

- Hermetic fixtures only. No real CLI or provider run, and no Linux headless
  run of this branch.
- Only files already delivered to the unit can be granted; new paths, partial
  grants and budget amendments stay outside M4, as the plan says.
- The card and the decision bar are covered by GPUI layout tests, not by a
  manual visual pass in the running application; that pass belongs to M5.
- The Companion stays read-only: it can explain a request when asked, and
  has no action on it.
