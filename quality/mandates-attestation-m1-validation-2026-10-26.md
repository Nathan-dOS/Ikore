# Execution mandates and approval receipts (ADR-035 M1) — implementation and validation

Date: 2026-10-26. Branch `feat/mandates-layers-attestation`: implementation
`8946570`, test completion `a9d16fe` and `e913361`, and the lock fix
`a85f46d`, on top of `585fe9e` (ADR summaries) and `2363b9d` (formatting of
`tool_scope_ab.rs`). Decision:
[ADR-035](../docs/adr/035-execution-mandates.md). Additive SQLite migration:
V55. Nothing is pushed.

This report qualifies the execution mandate, its enforcement points and the
approval receipts against hermetic fixtures (scripted model gateway, temporary
workspaces and databases). No real credentials, CLI sessions or paid model
calls were used. GitHub Actions was not used and is not evidence here. The
trust layers (M2), the signed attestation (ADR-036, M3) and the scope grant
(M4) do not exist yet.

## Delivered behavior

- **Issue.** Approving a pipeline v4 Specification issues an
  `ExecutionMandateV1` in the approval's transaction, before the candidates are
  accepted. Failing to derive it refuses the approval. A second approval of the
  Specification (after a revision back to it) appends a `reissued` revision; an
  approved CLI fallback that switches a role to the API appends a
  `transport_fallback` revision for that role only. Each revision is canonical
  JSON (sorted keys, integers only) with a SHA-256 digest and the previous
  revision's digest; a trigger refuses any update, and a read re-checks the
  digest (`mandate_integrity_failed`).
- **Content.** The intent (task revisions with digests, accepted Specification
  artifacts, the review-policy snapshot), the write envelope, the executor
  policies, one transport per agent role, the enabled check profiles with their
  digests, and the budget parameters `derive_budget` reads (per-stage output
  limits, context characters, extra invocations, units in flight). `expiresAt`
  is reserved and always null.
- **Envelope.** Explicit grants, each with the write classes that produced it:
  brief sources, paths named by the batch's tasks, clarification paths,
  Specification references, the stage notes and the default test file of each
  source path. It is closed over what v4 recipes let a stage own; the review
  policy file is never granted. Path identity is the v4 owner table's: `\` and
  `/` are equal and only ASCII case is folded, so a non-ASCII case variant is a
  different path and is refused.
- **Enforcement (Contract, Code, Test, Integrator).**
  - Acceptance: a stage output outside the envelope is a host-contract failure
    (`mandate_scope_violation`); nothing is accepted.
  - Publication: the outbox re-checks against the revision in force; a path it
    no longer grants becomes a `conflict` and the workspace is untouched.
  - Admission: a transport, executor policy or check profile that differs from
    the mandate parks the stage as `stale_input` with
    `mandate_capability_mismatch`, before any permit is reserved. Restoring the
    setting and choosing Retry readmits it.
  - Budget: admission uses the frozen parameters, not the live settings.
  - Refusals at publication and admission are kept in
    `mandate_enforcement_events`, at most 200 rows per pulsation (further
    refusals increment the newest row).
- **Receipts.** Every checkpoint resolution records an `ApprovalReceiptV1`:
  artifact review (approve, reject, request revision), path confirmation,
  clarification answers, stage failure and CLI fallback. It keeps the reviewed
  candidates' digests, the confirmed paths, the mandate revision in force, the
  risk and convergence acknowledgements, and free text only as SHA-256.
- **Preview.** The v4 Specification brief shows "What approving grants" before
  the decision: paths with the sensitive ones marked (first 64 drawn, the rest
  counted), agents and transports, checks, budget and the mandate's short
  digest. It is derived by the same function and is byte-identical to what the
  approval issues for the same state version.
- **Legacy and upgrade.** Pipelines v1–v3 get a `not_applied_legacy` revision
  with no envelope and keep their behavior; the brief draws no card. A
  pulsation already past its Specification when V55 runs has no mandate and is
  not checked.

## Differences from the accepted design

Recorded in the ADR summary's implementation notes and in the maintainers'
specification:

- The mandate governs only Contract, Code, Test and Integrator. A revision back
  to the Specification runs it on the live settings, and its new approval
  re-issues the mandate.
- A capability mismatch parks as `stale_input`, not `host_contract`: in v4 only
  `stale_input` always offers Retry.
- The mandate freezes the budget **parameters**, not per-stage numbers, because
  per-stage budgets are derived from the plan's units after the approval. There
  is no `mandate_budget_mismatch`.
- Write classes are origins of explicit grants; there is no separate
  `scope.derived` array. Transports are their own array, one per role.
- An acceptance refusal rolls back its transaction, so it is reported in the
  failure diagnostic, not in `mandate_enforcement_events`.
- `preflight_dismissal` acknowledgements exist in the contract but are not
  filled yet.

## Acceptance matrix

| Criterion (implementation plan) | Executed evidence |
|---|---|
| Same rows and state version give the same bytes; the preview equals the issued mandate | `mandate::tests::the_same_inputs_give_the_same_bytes_and_digest`, `generated_envelopes_grant_exactly_the_named_paths` (200 generated batches); `mandate_persistence::tests::approving_the_specification_issues_the_previewed_mandate_and_a_receipt`; `execution_mandate::a_composed_pulsation_runs_inside_the_mandate_its_specification_approval_issued` |
| Each v4 Specification approval creates exactly one revision; a re-approval chains | Same tests; `approving_the_specification_again_reissues_on_the_chain`; `a_reissue_chains_to_the_previous_revision_and_drops_its_amendments`; `a_rejection_gets_a_receipt_and_no_mandate` |
| A derivation failure refuses the approval | `execution_mandate::an_approval_whose_mandate_cannot_be_derived_is_refused_and_changes_nothing` (missing payload: `mandate_derivation_failed`, no revision, no receipt, same waiting checkpoint and state version) |
| The preview is readable while the host writes | `the_preview_reads_while_another_connection_writes` (20 previews against a writer in a tight loop, none near the 5 s busy timeout) |
| No path outside the mandate reaches `applied` | `a_stage_write_outside_the_enforced_mandate_is_refused_at_acceptance`, `a_publication_outside_the_revision_in_force_is_a_conflict_and_an_event`, `a_write_inside_the_mandate_passes_both_checks`; the end-to-end test checks every applied publication against the issued envelope, with no enforcement event |
| Path identity: case, separators, dot segments, Unicode, reserved names | `paths_match_by_windows_identity`, `unicode_and_reserved_names_fail_closed`, `a_batch_at_the_limits_grants_every_path_it_names` |
| Every checkpoint resolution has a receipt with exact digests | Artifact review: the two `mandate_persistence` approval tests and the end-to-end test (4 checkpoints, risk acknowledgement kept); stage failure: `a_transport_changed_after_the_approval_parks_the_stage_until_restored`; CLI fallback: `cli_failure_requires_explicit_api_authorization_and_requeues_same_job`; clarification: `clarification_checkpoint_survives_restart_and_answers_cannot_approve_artifacts` (answers kept as a digest); path confirmation: `a_foundation_task_with_no_confirmed_path_parks_before_specification_and_resolves_with_a_hint`; canonical form: `receipts_are_canonical_and_keep_only_digests_of_text` |
| Capabilities and budget come from the mandate | `the_budget_frozen_at_the_approval_ignores_a_later_settings_change` (live output cap raised to 40,000; Code keeps the frozen limit); `a_transport_changed_after_the_approval_parks_the_stage_until_restored` (nothing dispatched, one `capability_mismatch` event, Retry after restoring); `the_budget_and_capabilities_come_from_the_mandate_in_force`; `a_role_must_keep_its_approved_transport`, `an_executor_must_keep_its_approved_policy`, `a_stage_runs_exactly_the_approved_checks_that_apply_to_it`; transport fallback: `an_api_fallback_amends_the_transport_of_its_role`, `a_transport_fallback_changes_only_that_role_and_chains` |
| Legacy shows the label and keeps its behavior | `a_legacy_mandate_records_the_approval_without_an_envelope`, `a_legacy_mandate_records_without_refusing`; `layout_tests::a_legacy_specification_review_draws_no_grants_card`; the existing v1–v3 suites pass unchanged |
| Write sources are classified; background writers stay outside the pulsation's mandate | `every_write_source_is_classified`, `writes_are_checked_only_for_stage_output_under_an_enforced_mandate` |
| Append-only, integrity, bounded events | `revisions_are_append_only_and_verified_against_their_digest`, `enforcement_events_are_bounded_per_pulsation` |
| V55 is additive; rows go with their project | `serial_upgrade::v54_to_v55_adds_mandates_and_receipts_without_touching_running_pulsations` and the latest-version assertions bumped to 55; `project_deletion::tests::deletes_one_repository_and_its_history_but_retains_attributed_usage` |
| Limits: 64 tasks, 200+ paths, a 260-character path | `a_batch_at_the_limits_grants_every_path_it_names` (64 tasks, 257 grants, under 2 s); `enforcement_events_are_bounded_per_pulsation` (205 refusals in 200 rows) |
| The card: 64 paths, sensitive mark, both widths | `layout_tests::the_specification_brief_shows_what_approving_grants` (70 paths at 960 and 1440 px: paths 0–63 drawn, the rest counted, no overflow). The card has no focusable element, so the brief's keyboard order is unchanged |

## Defects found during the implementation

**Stages before the mandate.** The first version applied the mandate to every
stage after the approval. When a Contract conflict sent the pulsation back to
the Specification,
`composition_failures::a_worker_diagnosed_contract_conflict_parks_with_gated_targets_and_supersedes_on_revision`
timed out: admission refused the re-run Specification with
`mandate_capability_mismatch: mandate revision 1 records no transport for role
task_specifier`. The mandate now governs only the stages it was issued for
(`mandate_governs`), at admission, acceptance and budget derivation alike.

**Lock cycle in the preview** (fixed in `a85f46d`). The derivation read the
runtime settings through a second connection while the preview's read
transaction was open. The database uses SQLite's rollback journal: a writer
waiting to commit holds the pending lock until that read ends, and the second
connection waits for the writer, so the preview failed with `database is
locked` when the 5 s busy timeout expired. In the application the "What
approving grants" card would fail to load while a pulsation or background job
wrote. It surfaced after the gate on `a9d16fe` had passed:
`a_composed_pulsation_runs_inside_the_mandate_its_specification_approval_issued`
failed in 2 of 3 runs once a fourth test ran beside it. The regression test
reproduced it before the fix (60 previews took 454 s and the last failed with
`DatabaseBusy`). The derivation now reads the settings on the transaction's
connection (`runtime_config_from`). With the fix, no preview failed or came
near the busy timeout in 5 runs of the regression test; 60 previews still took
about 9 s against the tight-loop writer, which is ordinary contention, so the
committed test runs 20. `execution_mandate` passed 8 of 8 consecutive runs. The other
places that read the settings inside a transaction hold an immediate (write)
transaction, where no other connection can take the pending lock, so the
cycle cannot form there.

## Validation results

Environment: Windows 11 Pro 10.0.26300, Rust/Cargo 1.98.0, `--locked`,
`CARGO_BUILD_JOBS=6`, in the branch's own worktree.

Baseline before the change (Etapa 0, `9ebea09` plus the formatting fix, same
machine): 52 result targets, **1,551 passed, 0 failed, 16 ignored**; the Stage 7
gate passed, with 0 vulnerabilities and 14 `unmaintained` plus 2 `unsound`
informational advisory warnings.

The Stage 7 gate (`scripts/gate-stage7.ps1`) on `a85f46d`
(2026-10-26 17:57–18:27 local time, 1,804 s) **passed**:

| Gate step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | PASS | 0.694 s |
| crate dependency boundaries | PASS | 0.192 s |
| cargo fmt | PASS | 4.678 s |
| Clippy (`-D warnings`) | PASS | 1.552 s |
| Rust tests: 53 targets, **1,588 passed, 0 failed, 16 ignored** | PASS | 1,287.551 s |
| GPUI release build | PASS | 500.884 s |
| Windows GPUI smoke (window opened, graceful shutdown, exit 0) | PASS | 1.764 s |
| Rust licenses and sources (560 packages for `x86_64-pc-windows-msvc`, none denied) | PASS | 2.011 s |
| Rust advisories (`cargo audit` exit 0, 0 vulnerabilities; 14 `unmaintained` and 2 `unsound` informational warnings; RustSec `a78282a`) | PASS | 4.728 s |

Against the baseline: one more target (`execution_mandate`) and 37 more
passing tests, all of them new (18 unit tests in `orchestration::mandate`,
12 in `mandate_persistence`, 4 in `execution_mandate`, 1 in `serial_upgrade`
and 2 layout tests); the 16 ignored tests are the same opt-in ones. The
informational warnings are the baseline's: `git2 0.20.4` (RUSTSEC-2026-0183,
RUSTSEC-2026-0184) and 14 unmaintained transitive crates. The supply-chain
summary still reports `distributionAllowed: false` for the existing GPUI
binary blocker; this change does not touch it.

Earlier gate runs on the same implementation:

- **Run 1** failed one test:
  `composition_cli::claude_haiku_selection_parks_the_pulsation_for_subagent_mismatch_or_missing_identity`
  hit its 30 s wait while its v4 Code stage was still running. That stage now
  passes the mandate checks at admission. In isolation the test passed 3 times
  out of 3, in about 25 s each, and it passed in every later full run, so this
  is recorded as a thin timing margin under load rather than a regression. The
  margin was not measured on the baseline.
- **Run 2** passed every step except the advisory audit: `cargo-audit` is
  looked up under each checkout's `target/tools/cargo-audit`, and the new
  worktree had none. Tests: 53 targets, 1,584 passed, 0 failed, 16 ignored.
  After `cargo-audit` 0.22.2 was copied from the main checkout,
  `scripts/audit-advisories.ps1` passed on its own: exit 0, 0 vulnerabilities,
  the same 14 + 2 informational warnings, RustSec commit `a78282a`.
- `a9d16fe` added tests that complete the plan's M1 list: one new unit test,
  one new persistence test, receipt assertions in three existing tests and V55
  rows in the project-deletion test.
- **Run 3** on `a9d16fe` passed every step (2026-10-26 16:54–17:31 local
  time): 53 targets, 1,586 passed, 0 failed, 16 ignored. The lock cycle above
  was already in that tree and did not show in that run.
- `e913361` added the derivation-failure test and `a85f46d` the lock fix with
  its regression test.

`Cargo.toml` files and `Cargo.lock` are unchanged from the baseline.

## Limits

- Hermetic fixtures only. No real CLI or provider run, and no Linux headless
  run of this branch.
- The "What approving grants" card is covered by GPUI layout tests, not by a
  manual visual pass in the running application; that pass belongs to M5.
- The receipts and mandates are local SQLite rows. Until the attestation (M3)
  signs them, they show what the host recorded, not proof against someone who
  can edit the database.
- A pulsation already past its Specification at upgrade time has no mandate.
- `preflight_dismissal` acknowledgements are not recorded yet.
