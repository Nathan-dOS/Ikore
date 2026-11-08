# Companion dock, decisions and feature starts — 2026-11-06

Status: implementation in progress; qualification pending. This document does
not certify the source tree, native behavior or performance until executed
evidence is added below.

## Source and scope

- Branch: `feat/companion-decision-dock` in its isolated worktree.
- Baseline: `c42f2a77dafd30ee0023b342f9a5f4bd7f28cef2`, combining the existing
  Forge fixes at `ef2879a` with the current `main` documentation/CI changes.
- First delivery: corrected notifications/counts, per-conversation chat state,
  native Companion dock, durable drafts, read-only pending decisions and tools,
  lazy resolution conversations, richer Forge questions and Approve and start.
- Delegated review, automatic answer interpretation, web search and user-created
  agents are outside this delivery. Four human artifact reviews remain required.

## Regression scenarios added

### Forge command and questions

Tests were added before the corresponding implementation. Execution is pending.

- `approve_and_start_preview_is_read_only_and_covers_wave_capacity`
- `approve_and_start_preserves_approval_and_resumes_the_same_request_after_wave_failure`
- `approve_and_start_rejects_changed_input_and_stale_revision_without_dispatch`
- `approve_and_start_never_moves_or_dispatches_unfinished_prerequisites`
- `approve_and_start_concurrent_replay_produces_one_pulsation`
- `approve_and_start_rechecks_model_configuration_and_preserves_partial_progress`
- `dispatch_preflight_reads_routes_and_connections_from_its_transaction`
- `start_preview_does_not_block_a_worker_waiting_to_commit`
- `structured_feature_questions_keep_legacy_responses_readable_and_validate_indexes`
- `discovery_question_details_are_persisted_without_an_additional_model_call`
- GPUI: `selecting_a_question_option_only_fills_the_composer_for_the_current_turn`

The command stores durable progress and frozen task selections in the existing
`forge_commands` table; no additional migration is required for this subsystem.
The UI presents a read-only composite preview in Incubation and Triage, retains
the separate approval action and reports partial completion if dispatch fails.

### Dock, conversations and pending decisions

Coverage and executed selections will be recorded by the integration owner.
Required scenarios include concurrent streams, stale callbacks, focus/shortcut
isolation, restart, versioned drafts, project deletion, decision freshness,
zero-call card opening and preservation of existing domain guards.

### Flow guidance

Seven GPUI regressions cover unplanned approved features, blocked/reserved
tasks, current revisions and standalone tasks, stale task revisions/dependency
cycles, cached backend preview blockers, complete feature prerequisites and
project reconciliation. These tests target `flow::tests`; execution remains
pending until recorded below. The snapshot computation is navigation guidance,
not a substitute for dispatch-time admission.

## Executed checks

Speed-report validation executed on Windows PowerShell 5.1.26100.9711:
`scripts/tests/gate-speed-tests.ps1` passed all 27 metric/schema/parity scenarios.
The gate script passed PowerShell syntax inspection. The schema validator also
accepted the existing baseline's 80 observations with fixture digest
`786e5cc1da8409dc4985da0d27cea9c388d4c42fc82a82121ce1515c1f093c5e`.
These are contract checks, not execution of the paired performance gate.

The first complete baseline run outside the filesystem/network sandbox passed
2,059 tests (23 ignored, 61 target summaries), formatting, strict Clippy, crate
boundaries, release build, Windows smoke and supply-chain checks. The ninth
check failed on RUSTSEC-2026-0433 in `pulldown-cmark 0.13.5`. The candidate pins
the patched `0.13.6`; its standalone advisory check passed with zero
vulnerabilities. This is not a claim that the baseline passed all nine checks.

Candidate preflight exposed three obsolete core test expectations and a real
SQLite nested-reader deadlock in Forge start preview. The core fixtures now
exercise the expanded read-only tool list and strict/adapted question schema.
The preview reads configuration/routes from its existing connection, with a
concurrent-writer regression; no guard or timeout was relaxed. The repeated UI
target passed the affected Forge cases and 458 tests, with one remaining test
failure in the simulated keyboard gesture. These partial runs do not qualify
the candidate; the final full gate must pass below.

Exploratory native inspection used an isolated, offline review fixture and a
debug binary with SHA-256
`FBB8FFF77BC94251418F225FEEF7E2E80B47289FCB6653A179CBA72BF4BAC6BA`.
At 1440x900 it displayed three conversations; at 960x640 after restart it
displayed two, retained overflow and restored the exact Unicode draft. Opening
the host decision left the database counts unchanged: 3 chat conversations,
6 messages, 2 existing route executions and 0 decision-conversation bindings.
The inspection found duplicate composer layout on Expand and a redundant
Reopen entry. Those findings require the corrected source and native recheck;
this exploratory binary is not the final qualified artifact. The display's
current physical scale was used; other physical DPI scales were not exercised.

On 2026-11-08, five focused native tests passed individually: shutdown after
delayed Forge admission (3.77 s), retained Chat Send/Create admissions (0.06 s),
projected-description badge (0.02 s), Reopen visibility (0.09 s), and single
composer geometry/navigation/Expand (0.14 s). The geometry test checks absence
in a fresh render because GPUI's harness retains historical debug bounds.
Seven focused `approve_and_start` core tests also passed, including the
deterministic revision-change race. These selections overlap the final suite;
their counts must not be added to the complete regression total.

Pending: complete workspace tests; formatting; strict Clippy; crate boundaries;
release build; Windows smoke; supply-chain/advisory checks; speed comparison;
physical inspection at supported window sizes and display scales.

Record commands, source revision, counts, raw report paths and any limitations
here after execution. A sandbox/environment failure and a passing retry must be
reported separately; neither is a reason to suppress a regression.

## Performance and evidence limits

Compare the frozen baseline and candidate with the same fixtures, toolchain,
profile and concurrency, excluding compilation and warmup. Record calibration
noise before evaluating the candidate. Check the happy-path call count and
transition writes, the closed dock, and controlled concurrent streaming.

GPUI probes measure animation-frame callback intervals, not compositor
presentation. Scripted model replies and character-derived token estimates
test orchestration; they do not establish the quality or speed of real models.

## Integration and cleanup

Pending. After all required qualification passes, obtain the developer's
required confirmation before merging, pushing and removing this delivery's
worktree and temporary branches. Preserve preexisting branches and Cérebro edits.
