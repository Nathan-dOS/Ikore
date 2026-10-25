# Feature 10 — remaining milestones validation (ADR-034) — 2026-10-25

Scope: T1 (tool policy enforced), M3 (risk scrutiny and coverage matrix), M6 (Forge pre-flight guardian) and M5 (documentation reconciliation, code → note), on branch `cli/dreamy-hamilton-s7shmz` from `main` `11af87e` (ADR-034 summary `cdaa2e4`).

Environment: Linux headless container (no Windows, no authenticated CLIs). GitHub Actions is out of use and is not evidence. Windows acceptance (`cargo test-all`, Clippy `-D warnings`, `scripts/gate-stage7.ps1`, `scripts/audit-advisories.ps1`) and the T1 real-run measurement stay with Nathan.

## Delivered

- **T1.** `ToolPolicy::None` is gone; Intake, Contract and the repairers declare `ReadOnlyScoped`, the scope the CLI already gave them. `UnitContextScope::required_for` is the one table, and the CLI transport refuses a divergent scope or an unknown specialty. The Forge batch composer says when composition is off.
- **M3 (V52).** Versioned risk policy, check selection, diff priority, `REVIEW.md` captured at Intake, the Integrator coverage matrix, the high-risk acknowledgement, the "Risk and coverage" card and report section, and the project's risk additions in Settings.
- **M6 (V53).** Decisions in the triage prompt, `architectureConflicts` in the strict schema, `forge_preflight_findings`, the host block on manual and automatic approval (`preflight_conflict_unresolved`, `preflight_stale`, `preflight_required`), Adjust / Propose a decision / Dismiss, the Triage "Architecture conformance" card, and dismissed conflicts in the Specification brief.
- **M5 (V54).** The `documentation_reconciler` role (role tables rebuilt as V50 did), baselines, blobs, signals and proposals; detection on watcher events, at project open and for finished pulsations; the on-request proposal with sanitized note anchors and line diffs; approve with expected hashes, edit, discard, ignore, silence; the status-bar indicator, the Vault list and the comparison.

Deviations from the specification are listed in the Cérebro specification, "Estado da implementação".

## Gate per milestone (Linux)

Each milestone closed with `cargo fmt --check`, `cargo clippy --locked` over the headless crates plus `hvc-ui` (all targets), the headless suite (`hvc-contracts`, `hvc-terminal`, `hvc-voice`, `hvc-core`, `hvc-editor`, `hvc-test-support`, two threads) and the `hvc-ui` lib and integration tests. Failures are compared by name with the known Linux list (28 headless REV-0001 failures plus the UI settings-search test that needs the Windows-only push-to-talk entry).

| Milestone | fmt | Clippy warnings (Linux baseline with `hvc-ui`: 15) | Headless passed / failed / ignored | UI passed / failed | New failures |
|---|---|---|---|---|---|
| Baseline `11af87e` | ok | 11 headless only | 1117 / 28 / 13 | not run | — |
| T1 | ok | 15 | 1121 / 28 / 13 | 325 / 1 | 0 |
| M3 | ok | 16 → 15 (`level_from_db` removed after the run) | 1140 / 28 / 13 | 327 / 1 | 0 |
| M6 | ok | 16 → 15 (`items_after_test_module` on a test helper, fixed after the run) | 1153 / 28 / 13 | 329 / 1 | 0 |
| M5 (final, includes M6's brief item) | ok | 15 | 1176 / 29 / 13 | 332 / 1 | 0 (the 29th is the new Windows-only CLI test, see below) |

The Clippy warnings are the Linux-only ones of the baseline (`cfg(windows)` code seen as dead or needlessly mutable on Linux); no warning comes from the new code.

## Tests added

68 tests, by file:

### crates/hvc-contracts/tests/contracts.rs
- every_composed_specialty_has_one_required_context_scope
### crates/hvc-core/src/doc_reconciliation/contract.rs
- edits_replace_whole_anchors_on_the_exact_bytes
- the_contract_refuses_what_the_prompt_forbids
- a_developer_edit_keeps_the_marker_and_size_rules
### crates/hvc-core/src/doc_reconciliation/links.rs
- links_are_explicit_normalized_and_filtered
- a_note_with_hundreds_of_links_is_derived_in_one_pass
### crates/hvc-core/src/doc_reconciliation/tests.rs
- a_first_observation_never_signals_and_a_change_opens_one_signal_per_pair
- the_scan_at_project_open_finds_changes_made_while_the_app_was_closed
- a_removed_file_is_reported_and_a_removed_link_or_note_drops_its_rows
- renaming_a_note_keeps_its_baselines_and_signals
- links_outside_the_workspace_symlinks_and_vault_notes_are_never_tracked
- ignore_advances_the_baseline_and_silence_stops_the_pair
- a_proposal_is_made_only_on_request_and_publishes_on_the_expected_bytes
- a_concurrent_change_invalidates_the_proposal_without_writing
- edit_and_reject_keep_the_developer_in_charge
- a_refused_answer_gets_one_corrective_call_and_then_reports_the_failure
- the_request_is_sanitized_and_its_gaps_stay_visible
- a_pulsation_that_also_published_the_note_advances_the_baseline
- base_bytes_are_bounded_per_file_and_released_when_unreferenced
- the_indicator_text_never_names_an_author_or_a_defect
- workspace_events_only_compare_tracked_files_and_never_rebuild_the_links
### crates/hvc-core/src/feature_forge/preflight.rs
- decisions_are_selected_agent_first_then_linked_then_by_path
- budgets_never_truncate_and_report_what_was_left_out
- the_digest_follows_the_files_not_their_order
- links_come_from_wikilinks_and_the_section_is_sanitized
- conflicts_cite_only_delivered_decisions_once_and_briefly
- only_no_blocking_conflict_or_a_dismissal_allows_approval
### crates/hvc-core/src/feature_forge/tests.rs
- the_forge_snapshot_reports_whether_new_batches_are_composed
- triage_reads_the_accepted_decisions_and_a_blocking_conflict_stops_approval
- adjusting_or_proposing_a_decision_keeps_the_approval_blocked
- a_changed_decision_or_a_new_revision_needs_a_new_triage
- an_advisory_conflict_or_a_project_without_decisions_never_blocks
- a_conflict_citing_a_decision_that_was_not_sent_fails_the_triage
- decisions_are_sanitized_before_they_reach_the_triage
- a_dismissed_conflict_reaches_the_specification_brief_of_the_batch
### crates/hvc-core/src/orchestration/composition/policy.rs
- every_declared_tool_policy_is_the_scope_the_transport_requires
### crates/hvc-core/src/orchestration/composition/review_boundary.rs
- risk_orders_the_diff_and_reports_missing_checks_without_downgrading
- a_version_one_manifest_still_reads
### crates/hvc-core/src/orchestration/composition/review_policy.rs
- the_intake_capture_keeps_every_guide_within_its_limits
- the_capture_reports_guides_left_out_at_its_limits
### crates/hvc-core/src/orchestration/composition/risk.rs
- a_note_declares_its_own_risk_in_front_matter
- the_level_is_the_highest_signal_and_unknown_stays_visible
- notes_interfaces_and_size_raise_the_level
- the_same_inputs_and_policy_give_the_same_assessment_in_any_order
- within_a_pulsation_the_level_never_falls
- a_project_only_adds_to_the_builtin_policy
- the_level_selects_required_checks_among_configured_profiles
- sensitive_files_take_the_diff_budget_first
- the_coverage_matrix_separates_executed_model_only_and_unverified
### crates/hvc-core/src/orchestration/composition/settings.rs
- risk_policy_additions_are_optional_and_validated
### crates/hvc-core/src/reports/pulsation.rs
- the_risk_section_reports_the_level_and_the_coverage
### crates/hvc-terminal/src/terminals/tests.rs
- the_documentation_reconciler_route_follows_the_feature_agent_but_never_custom
- a_composed_unit_runs_only_with_the_scope_its_specialty_requires
### crates/hvc-test-support/tests/cli_executor.rs
- documentation_reconciler_runs_companion_scoped_under_the_reconciliation_origin
### crates/hvc-test-support/tests/composition_review.rs
- a_high_risk_review_shows_its_coverage_and_needs_an_acknowledgement
- the_review_policy_is_the_one_captured_at_intake
- a_projects_risk_additions_persist_and_an_invalid_one_is_refused
### crates/hvc-test-support/tests/serial_upgrade.rs
- v51_to_v52_adds_the_risk_scrutiny_tables_without_rebuilding_existing_ones
- v52_to_v53_adds_the_forge_preflight_findings_without_rebuilding_existing_ones
- v53_to_v54_adds_the_reconciler_role_and_the_reconciliation_tables
### crates/hvc-ui/src/doc_drift.rs
- the_comparison_opens_edits_and_closes_without_reaching_the_core
- the_indicator_counts_notes_with_open_changes_only
### crates/hvc-ui/src/forge.rs
- the_batch_composer_says_when_the_review_policy_needs_composition
- preflight_decisions_act_on_the_current_blocking_finding_only
### crates/hvc-ui/src/hitl.rs
- the_risk_status_belongs_to_its_checkpoint
### crates/hvc-ui/src/layout_tests.rs
- a_high_risk_integrator_review_shows_its_coverage_and_asks_for_a_reason
- forge_triage_shows_the_conformance_card_and_holds_approval_on_a_blocking_conflict
- documentation_drift_shows_the_indicator_the_list_and_the_comparison

## Findings during validation

- **Strict triage schema (M6).** The triage reply schema is strict, so `architectureConflicts` is required. The existing triage fixtures did not send it and seven Forge tests failed; the fixtures now send an empty list. A reply without the field is refused like any other off-schema reply.
- **GPUI debug bounds (M6).** The vendored GPUI never clears `debug_bounds` between frames, so a selector stays "present" after its element stops rendering. Tests that assert absence draw the state in a fresh window.
- **Clippy `items_after_test_module` (M6).** The new UI test helper was placed after the test module; it moved before it.
- **Brownfield hot path (M5).** The first M5 version re-derived every link from the Vault index on each note event inside `record_workspace_change`, and read files inside its write transaction. Brownfield maintenance publishes component notes in a row and calls that path synchronously, and `brownfield_bf5` hit its 30-second watchdog. The workspace hook now answers untracked paths with one read on the connection it already holds. Note link changes are applied by the next overview or scan, and every file read and hash happens before the short write transaction. `brownfield_bf5` passed three times standalone (32 to 65 s) and in the final gate; `workspace_events_only_compare_tracked_files_and_never_rebuild_the_links` pins the design.
- **Older upgrade checks (M5).** The V51 → V52 and V52 → V53 tests upgrade to the latest version and asserted that no table was rebuilt. V54 rebuilds the three role-checked tables on purpose, so those checks leave exactly them out; the V54 test proves their rows survive and that nothing else changed.
- **Binary names (M5).** The repository rules never let a binary-named file (`.bin`, images) be a link, as for any context. Bytes that are not UTF-8 under a text name are tracked by hash only, with the `content_unavailable` gap in a proposal.
- **Windows-only CLI test (M5).** `documentation_reconciler_runs_companion_scoped_under_the_reconciliation_origin` mirrors `notes_scribe_runs_companion_scoped_under_the_notes_origin`, already in the known Linux list. Both need the `.exe` CLI fixtures and fail on Linux before the run starts ("Codex is not ready"). It belongs to the Windows acceptance.

## Windows acceptance (2026-10-25)

`scripts/gate-stage7.ps1` on `cli/dreamy-hamilton-s7shmz` (`e60c31e`), Windows 11, `CARGO_BUILD_JOBS=6`: passed, exit 0. Structure, crate boundaries, `cargo fmt`, Clippy `-D warnings` (92 s), `cargo test-all` (1,689 s, 0 failures), release build (1,021 s), Windows smoke, licenses and sources, and advisories (0 vulnerabilities) all green. Timings are in `target/stage7/timings.json`. The first attempt failed in step 1 only because `cargo tree` printed "Updating crates.io index" on stderr under PowerShell 5.1; a `cargo fetch --locked` beforehand fixed it.

## T1 real-run measurement (D3)

Harness: `crates/hvc-test-support/tests/tool_scope_ab.rs` (paid, `#[ignore]`). Real `claude` CLI (Haiku) for Intake, Contract, Code and Test; Specification and Integrator scripted. One task (`expiry_task`, the shared fixture), pulsation driven to `done` approving each artifact review. Arm A: current tables (`Workspace`). Arm B: `HostContextOnly` for `intake_classifier`, `contract_author`, `code_repairer`, `test_repairer` in `policy.rs` and `UnitContextScope::required_for` (throwaway edit, reverted; two binaries built from the same tree and run side by side). Three paired runs.

| Arm | Run | Outcome | Intake first-attempt ok | Contract first-attempt ok | Retries | CLI runs | Input tokens | Output tokens | Wall s |
|---|---|---|---|---|---|---|---|---|---|
| A workspace | 1 | done | 1/1 | 1/1 | 0 | 4 | 47,655 | 4,394 | 63 |
| A workspace | 2 | done | 1/1 | 1/1 | 0 | 4 | 48,818 | 4,069 | 62 |
| A workspace | 3 | done | 1/1 | 1/1 | 0 | 4 | 60,705 | 5,044 | 66 |
| B host context | 1 | done | 1/1 | 1/1 | 0 | 4 | 33,651 | 3,581 | 62 |
| B host context | 2 | done | 1/1 | 1/1 | 0 | 4 | 41,033 | 4,349 | 63 |
| B host context | 3 | done | 1/1 | 1/1 | 0 | 4 | 33,564 | 3,984 | 64 |

Token columns cover the whole pulsation (the unchanged Code and Test runs included). Mean input tokens: A 52,393, B 36,083.

Reading:
- **Intake and Contract:** first-attempt acceptance 3/3 and 3/3 in both arms, no retries, every run reached `done`. No measure worsens, so by the D3 rule (a tie favors `HostContextOnly`) they qualify. The sample is small and one easy task: it shows no regression, not a margin.
- **Repairers:** not exercised. No check failed in any of the six runs, so no `code_repairer` or `test_repairer` attempt exists. There is no evidence either way; they stay `ReadOnlyScoped`.
- **Not applied:** the tables on this branch are unchanged. Moving Intake and Contract is a one-line change per executor in both tables and needs another gate run.
- A second `intake_classifier` attempt row appears in both arms alike (ok 1/2); it is the same in A and B.

## Pending

- Manual QA screenshots of the new surfaces, as in ADR-032.
- Decision on moving Intake and Contract to `HostContextOnly`; a repair-provoking fixture if the repairers are to be measured.
