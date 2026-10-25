# Integrator review boundary, review policy and revision rounds — implementation and validation

Date: 2026-10-24. Implementation baseline: `6810d94`; branch
`feat/adr-032-review-policy`. Decision: [ADR-032](../docs/adr/032-integrator-review-boundary.md).
Behavior contract: [review policy](../docs/review-policy.md). Additive SQLite migration: V51.

This report qualifies the pipeline v4 (composed) Integrator's inputs, the
`REVIEW.md` policy channel and the human revision-round rule against hermetic
fixtures. It is not evidence that the reviewer finds more defects: review
quality is measured separately (planned explicit feedback and local evals).
No real credentials or paid model calls were used.

## Delivered behavior

- **Classified inputs (M1).** Each Integrator input carries its origin and
  class. Stage documents (Specification note, Test report) are not delivered or
  owned; candidates published by Code/Test are delivered as final files plus a
  bounded line diff of the sanitized base and candidate views; only baseline
  documents stay editable. Code evidence and Test coverage explanations arrive
  labelled as author claims. Host evidence is a table of check, status,
  freshness and limits, with earlier stages' checks named without a citable
  handle. Hashes, receipt identifiers and execution identifiers stay out of the
  prompt.
- **Base and candidate binding.** The first publication of each path records its
  base inside the publication transaction (bytes up to 256 KiB, otherwise only
  the hash; a replay after a crash keeps only the expected hash). Bytes are
  cleared when the pulsation ends. Candidates are bound to the published hash.
- **Evidence gaps.** Missing workspace, unreadable or hand-edited candidates,
  unreadable brief sources and checks not run, stale or unverifiable become
  explicit gaps. These downgrade a `supported` verdict to `unverified`; a gap
  never promotes a verdict, and a missing diff only informs. The Code execution
  is looked up by input revision and status, never by recency alone.
- **Manifest.** `integrator_review_inputs` records the canonical manifest per
  execution; its digest joins the Integrator's reuse key only.
- **Host-selected context over CLI.** The reviewer declares and enforces
  `HostContextOnly`: the CLI runs in an empty directory under the Companion
  hardening, a dispatch naming the reviewer without that scope is refused, and a
  Custom adapter parks the stage with `provider_configuration` before any
  invocation.
- **`REVIEW.md` (M2).** Resolved as a root-to-leaf cascade over the reviewed
  paths with per-file, file-count and total budgets and visible states,
  sanitized with the frozen policy, ranked below host gates, approved intent
  and `PROJECT_AGENT.md`. Publication refuses the name at any depth and case,
  and no stage owns it. The checkpoint flags a guide changed since the review.
- **Revision rounds (M4).** Human rounds (checkpoint and failure-path
  revisions), retries, technical attempts, extra invocations and Code candidate
  generations are counted from durable rows per input revision. At
  `revisionRoundThreshold` (default 3, range 1–10, Settings → Stage composition)
  every further revision needs Continue or Replan with a reason; Replan revises
  from the Specification and restarts the window. Decisions are stored in
  `pulsation_convergence_decisions` and as `convergence_decision` events.
- **Review tab and report.** The Integrator brief leads with "What the reviewer
  received"; legacy pulsations show that the policy does not apply; the decision
  bar shows the round count and, beyond the threshold, the two decisions
  instead of Request revision. The pulsation report gains a Review inputs
  section; reports written before keep parsing.

## Acceptance matrix

| Milestone | Criterion | Executed evidence |
|---|---|---|
| M1 | Origin, revision and class of each input are inspectable | `composition_review::the_integrator_reviews_classified_inputs_under_the_project_policy` (prompt sections, manifest classes, base row, no stage document delivered or owned); `recipes` unit test for stage documents, candidates and owned documents; `review_boundary` digest test |
| M1 | Author narrative is not taken as proof | Same integration test (labelled claims, Test report absent); `review_boundary::claims_are_labeled_and_bounded` |
| M1 | Missing context and invalid freshness prevent an undue conclusion | `composition_review::a_hand_edit_after_publication_is_a_gap_that_downgrades_the_verdict`; `review_boundary::gap_normalization_matrix`; `composition_recipes::the_integrator_receives_host_prechecks…` (check not run is a gap, no citable handle) |
| M1 | CLI reviewer reads host-selected context only | `composition_review::a_cli_integrator_reads_host_selected_context_only` (empty directory, read tools off); `composition_review::a_custom_cli_integrator_parks_before_any_invocation`; `contracts::unit_dispatch_preserves…` (scope wire format and default) |
| M2 | Guide, version and fallback visible; never an owned destination | Integration test above (`loaded` state, changed-since-review, no policy text kept); `review_policy` tests (cascade, every state, budget, root first); recipes unit test (Code never owns `REVIEW.md`) |
| M2 | An attempt to change the policy is refused by the host | `serial_tests::review_policy_files_are_never_published`; sanitized policy in the captured request; guide text cannot lift a downgrade (`gap_normalization_matrix` runs regardless of the guide) |
| M4 | Human revision and technical repair counted separately; restart keeps counts | `composition_review::revision_rounds_beyond_the_threshold_need_an_explicit_decision` (counts by kind, second service on the same database) |
| M4 | Reaching the threshold requires a decision, no silent loop | Same test (refusal without a decision, blank reason refused, Continue, Replan to Specification, window restart, two events); `hitl` and layout tests for the Review tab |
| — | V51 is additive and cascades with its pulsation | `serial_upgrade::v50_to_v51_adds_the_review_boundary_tables_without_rebuilding_existing_ones` and the bumped latest-version assertions |
| — | Base capture before replacement, replay and size limits | `serial_tests::publication_base_records_the_bytes_before_the_first_replacement` |
| — | Report section and compatibility | `reports::pulsation::tests::the_review_section_reports_inputs_gaps_and_rounds`; golden file unchanged |

## Validation results

Baseline before the change (`6810d94`, same machine): 1,436 passed, 0 failed,
15 ignored across all targets except the `hvc-core`/`hvc-terminal` doctests,
which the concurrent start of this work invalidated; those passed separately on
the clean `main` checkout (2 passed).

With this change, the complete workspace suite (`cargo test --locked --workspace
--no-fail-fast -- --test-threads=2`) recorded **1,467 passed, 0 failed and
15 explicitly ignored tests across 51 result targets** (2026-10-24 20:14–20:35
local time): the 1,438 of the baseline plus 29 new tests. Formatting
(`cargo fmt --all -- --check`) and strict Clippy (`cargo clippy --workspace
--all-targets --locked -- -D warnings`) passed.

The Stage 7 gate (`scripts/gate-stage7.ps1`, 2026-10-24 20:36–21:02) ran
against the same tree:

| Gate step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | PASS | 9.966 s |
| crate dependency boundaries | PASS | 0.201 s |
| cargo fmt | PASS | 4.802 s |
| Clippy | PASS | 7.283 s |
| Rust tests (1,467 passed, 15 ignored, 51 targets) | PASS | 996.555 s |
| GPUI release build | PASS | 521.194 s |
| Windows GPUI smoke | PASS | 2.406 s |
| Rust licenses and sources | PASS | 1.660 s |
| Rust advisories | NOT EXECUTED: `cargo-audit` is not installed on this machine | 0.149 s |

After the manual QA fixes below, formatting and strict Clippy passed again and
the complete suite was re-run (2026-10-24 21:28–21:48 local time): **1,468
passed, 0 failed and 15 ignored across 51 result targets**, one more test than
the gated run (the shared gap rule). Those fixes touch no build or dependency
configuration; the gated release build predates them.

The **overall gate is not approved**. The advisory step could not run, so this
report makes no new advisory claim. `Cargo.toml` files and `Cargo.lock` are
unchanged from `6810d94`, so the 9 dependency advisories recorded on
2026-10-22 still stand until a re-run proves otherwise. The existing GPUI
binary-distribution policy is unchanged. Neither blocker is waived.

## Manual QA in the application

`review_brief_fixture --stage integrator --review-policy` parks a v4 pulsation
at its Integrator checkpoint with a project `REVIEW.md`; the release and debug
`hard-vibe-code.exe` were opened on its data directory and captured with
`PrintWindow`. The Brief leads with "What the reviewer received": two
candidates, two stage documents not delivered, five author claims, host-selected
context only, `REVIEW.md` loaded with its short digest, `src/auth.rs` modified
with a diff and `tests/auth_test.rs` created. The round line ("Revision rounds:
0 of 3 · 6 technical attempt(s)") sits above the decision bar. The recorded
manifest, the base rows (`src/auth.rs` present, created files absent) and the
report's Review inputs section matched.

The QA found two problems, both fixed before the final run:

- The fixture had been unable to reach the Test checkpoint since the secret
  sanitization (reproduced on clean `6810d94`): its source declared
  `validate_token(token: &Token, …)`, which the redactor treats as a sensitive
  assignment, so the edit guard refused the scripted replacement. The fixture
  now names the parameter `candidate`.
- A `check_stale` gap with no mapped criterion was labelled "limits verdicts"
  under a summary saying the host had downgraded verdicts, although it could not
  change any. Gaps now share one rule (`EvidenceGap::limits_verdicts`) in the
  prompt, the Review tab and the report; such a gap reads "no criterion depends
  on it".

## Limits

- Pipeline v4 only; earlier pipelines are unchanged. Intake, Contract and the
  repairers keep their declarative `ToolPolicy::None`.
- Exceptions for synthetic fixtures do not extend to the guide or the diffs:
  those values are masked.
- No delta-only review, risk-driven selection (M3) or documentation
  reconciliation (M5). The threshold default and the diff budgets are
  hypotheses to calibrate with measured review outcomes.
- Losing the CLI reviewer's read tools may reduce context; prompt growth from
  files plus diffs can reach the existing prompt budget on large changes.
- Transport coverage uses fixture executables, not every authenticated CLI
  version. The existing dependency advisories and binary-distribution policy
  are unchanged.
