# Forge Incubation, file paths and context budget — 2026-10-31

Status: implemented and validated in the scope recorded below.

## Reported failures

The Incubation composer's keyboard hint overlapped the multiline input's lower
edge. Separately, **Ask Feature Agent for a path** added a pending decision and
reopened Incubation but never started the promised agent turn. A subsequent
developer message failed the context-budget guard before reaching a provider.

Read-only local inspection found the configured limit was 16,000 characters.
The current brief was 6,792 characters and discovery selected its most recent six
turns by count, without budgeting their rendered prompt size. The earlier Codex
model-evidence correction was working: subsequent Feature Agent calls recorded
the selected and observed model successfully.

## Corrections and boundaries

- Composer layout resolves its final width before measuring multiline height,
  reserving separate space for the keyboard hint and send controls.
- The explicit path button calls `FeatureAgentRuntime::start_path_clarification`.
  It records the pending decision, reloads the current revision and starts one
  ordinary discovery turn naming the existing task. Replaying the same request
  neither reopens a resolved decision nor starts another turn. Repeated pending
  questions do not advance the feature revision or duplicate the decision.
- Discovery fits the character budget by omitting older conversation turns as
  whole entries, with an explicit omission count. The complete current feature,
  brief, newest developer message, repository evidence, Intake analysis and
  known work remain present. Required context that still cannot fit retains
  the existing `context_too_large` failure before a provider call.

The Feature Agent remains read-only product planning. A path proposal changes
the evolving brief through host validation; it does not write source files,
approve tasks or dispatch a pulsation. Persisted conversation history and the
configured limit remain unchanged. No production database or settings were
modified by this correction.

## Regression scenarios

| Layer | Scenario |
|---|---|
| UI geometry | Explicit newlines, wrapping and capped long drafts at narrow/wide widths keep the hint and send controls below the input and preserve the full draft |
| Unit/boundary | A repeated path decision is stored once; archived features cannot start a clarification turn |
| Runtime integration | An explicit path request reaches Feature Agent once, includes the pending question, persists a validated task-path proposal and creates no file or pulsation; replay is idempotent |
| Context boundary | Long history is omitted only in the prompt projection, newest input and authoritative context stay complete, persisted history remains intact |
| Context rejection | An oversized required prompt still fails before a provider call |

The current regression names are
`layout_tests::forge_multiline_composer_keeps_its_hint_below_the_input`,
`feature_forge::runtime::discovery_prompt_tests::discovery_prompt_keeps_exact_unicode_budget_and_omits_only_whole_old_rounds`,
`discovery_context_omits_old_rounds_without_losing_the_brief_or_latest_message`,
`discovery_context_never_truncates_required_scope_or_the_latest_developer_message`,
and the `path_clarification_tests` module. The geometry matrix includes 960-,
1,440- and 2,400-pixel widths and explicit, wrapped and capped long drafts,
including trailing blank lines. It compares the actual native input frame with
the hint as well as the caret and surrounding controls. UI test text is English,
consistent with the source-language guard; runtime context tests retain Unicode
developer messages.

## Native Windows comparison

Computer Use reproduced the original overlap in an isolated profile: one long
developer line wrapped inside the capped field, while its hint was painted on
the lower border. In the corrected build, the same Portuguese draft, a second
line and a trailing blank line kept the hint below the field. No message was
sent during this visual check. The final rebuilt executable also started and
closed using the isolated profile. The test windows, profile and binary copies
were cleaned up afterward.

## Execution

| Check | Result |
|---|---|
| Path clarification regressions | Three passed: dispatch and replay, pending-question deduplication, archived-feature rejection |
| Context regressions | Two runtime tests and the exact Unicode/character-boundary unit test passed |
| Composer geometry and UI source-language guard | Both focused tests passed |
| `cargo fmt --all -- --check` | Passed |
| `scripts/check-crate-boundaries.ps1` | Passed |
| `cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings` | Passed; environment-only incremental-cache warnings remain |
| `cargo test --locked -p hvc-core -j 2 -- --test-threads=2` | 1,122 passed, zero failed, seven optional tests ignored, including doctests |
| `cargo test --locked --workspace --exclude hvc-core -j 2 -- --test-threads=2` | 887 passed, zero failed, 16 optional tests ignored, including test-enabled examples and doctests |
| `cargo build --locked -p hvc-app -j 2` | Passed; refreshed `target/debug/hard-vibe-code.exe` |
| Native Windows startup, layout and close | Passed with isolated data |

The two disjoint test selections together cover the complete default workspace
suite: **2,009 passed, zero failed, 23 optional tests ignored**. Final logs are
`target/forge-followup-core-verified-2026-10-31.log`,
`target/forge-followup-workspace-quiet-2026-10-31.log`,
`target/forge-followup-clippy-verified-2026-10-31.log` and
`target/forge-followup-build-verified-2026-10-31.log`.

An earlier concurrent attempt returned `InactivityTimeout` from the existing
`progressive_cli_budget_outlives_api_deadline_without_an_active_viewer` test.
It then passed both isolated and in the final remaining selection, without a
timeout-policy or test change. The final selection runs without concurrent
builds, core tests or native-window checks. Earlier lint findings in the new
tests and the UI source-language fixture failure were corrected before this
validation. Interrupted or failed preliminary runs are not counted as passing.

The final debug executable's SHA-256 is
`da951ae52b030c99b03eb631a9a47e72d1458f1982fc411a142df782b203b700`.
No release/distribution qualification is claimed. Existing unrelated local
changes are outside this correction; no temporary worktree was created.
