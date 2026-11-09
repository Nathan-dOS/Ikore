# Companion dock, decisions and feature starts — 2026-11-06

Status: implementation in progress; qualification pending. This document does
not certify the source tree, native behavior or performance until executed
evidence is added below.

## Source and scope

- Branch: `feat/companion-decision-dock` in its isolated worktree.
- Current candidate: `68b5c0522fa642664f114e4bbdf8628c81db41c0`.
- Baseline: `c42f2a77dafd30ee0023b342f9a5f4bd7f28cef2`, combining the existing
  Forge fixes at `ef2879a` with the current `main` documentation/CI changes.
- First delivery: corrected notifications/counts, per-conversation chat state,
  native Companion dock, durable drafts, read-only pending decisions and tools,
  lazy resolution conversations, richer Forge questions and Approve and start.
- Delegated review, automatic answer interpretation, web search and user-created
  agents are outside this delivery. Four human artifact reviews remain required.

## Regression scenarios added

### Forge command and questions

Tests were added before the corresponding implementation. Executed results are
recorded below; focused selections are not added to full-suite totals.

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

Coverage and executed selections are recorded below.
Required scenarios include concurrent streams, stale callbacks, focus/shortcut
isolation, restart, versioned drafts, project deletion, decision freshness,
zero-call card opening and preservation of existing domain guards.

### Flow guidance

Seven GPUI regressions cover unplanned approved features, blocked/reserved
tasks, current revisions and standalone tasks, stale task revisions/dependency
cycles, cached backend preview blockers, complete feature prerequisites and
project reconciliation. These tests target `flow::tests` and are included in
the complete suites recorded below. The snapshot computation is navigation guidance,
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

The first candidate is commit `4870c07d1d7e364c38e00aa4ced084ae1fadcfa7`.
Its Stage 7 run passed all nine checks, including the complete workspace suite:
2,137 passed, zero failed, 23 ignored,
zero filtered across 61 target summaries. This includes the 1,193 core and 465
UI tests; focused selections are not added to that total. The test gate step
took 1,726.871 seconds, including compilation. Release took 707.654 seconds;
Windows smoke closed gracefully in 1.793 seconds. Both audits passed, with zero
vulnerabilities. The timing/report files for this run are retained separately.

Before final qualification, a second route to duplicate composer mounting was
identified: a decision conversation could also be opened as a plain chat topic.
The bounded correction in `9935489` reuses the decision topic across opening,
delayed binding and legacy-layout restoration. Three regressions passed after
two were first reproduced failing; formatting and strict UI Clippy passed.
A repeated complete gate is required. The nine-check result above certifies
only `4870c07`; it does not certify that correction. The paired speed comparison
remains pending.

Native inspection of `9935489` confirmed canonical alias restoration, history
reuse, preserved Unicode draft, three windows at requested 1440x900 and two
with recoverable overflow at requested 960x640. Card scrolling, Expand, closing
and restart also worked. It exposed a keyboard regression: after hiding the
dock while its history button had focus, Ctrl+Shift+C could not reopen it until
a visible host control was focused. The repeated gate was interrupted to fix
this confirmed regression; no CLI test failure had been reported. That run
does not count as a complete gate. Focus regression tests and another complete
gate are required before qualification.

The focus correction in `29ed69a` retains the previous dock focus owner through
paint and recovers it to the persistent dock ancestor only when its child has
disappeared. It uses GPUI's focus-lost event, including while the window is
inactive, without activating the OS window. Seven focus regressions passed
(1.27 s), the complete dock group passed 30 tests (5.55 s), and strict UI Clippy
and formatting passed. Explicit blur, outside Chat focus and automatic opening
keep their existing behavior.

Native recheck of `29ed69a` at requested 960x640 passed repeated Ctrl+Shift+C
gestures without any intervening click, starting from a history button and a
composer, and after minimizing or closing a focused topic. Full Chat retained
its visible caret/focus when the dock was hidden and reopened. The exact Unicode
draft and counts of 3 conversations, 6 messages, 2 existing route executions and
0 decision bindings remained unchanged; no model submission was used. The app
closed gracefully with empty stderr. Binary SHA-256:
`6870c566b2e5c192cc0c8804f38bdd48b9170506e8fb05efbc2c4f7636735954`.
The gate for this source subsequently stopped at the English-source policy
check, recorded below. The older complete gate does not certify this source.

Final native recheck at the requested 1440x900 size used debug SHA-256
`DB107163EC8AEFA0505511B90AA9139A6DF6B29585BA7B9D259198BF003CF349`.
Expand minimized only the matching conversation, its complete Unicode draft
appeared in full Chat, and reopening its topic displayed Continue in full Chat
without a second input. Other conversations stayed available. History offered
Reopen only for hidden topics. The isolated app closed gracefully with empty
stderr. This confirms the two exploratory visual fixes; physical DPI coverage
is still limited to the current display scale.

Record commands, source revision, counts, raw report paths and any limitations
here after execution. A sandbox/environment failure and a passing retry must be
reported separately; neither is a reason to suppress a regression.

The complete gate for `29ed69a` stopped at the English-source policy check:
two Unicode test-fixture literals contained raw language-specific characters.
The other 474 UI tests passed. No policy or allowlist was relaxed. Commit
`0c3cca5` represents those characters with Rust escapes, preserving the runtime
text. The guard then passed in isolation and in the complete suite. The
production portion of the only changed Rust file is byte-equivalent after
newline normalization to the natively inspected `29ed69a`; its SHA-256 is
`dc47239624e4ffd7b042db8f3a013917e73ed50c08625e3858cbf424569ddd5a`.

The complete Stage 7 gate for `0c3cca5` passed all nine checks in 1,942.096
seconds. Its suite passed 2,147 tests, with zero failures, 23 ignored and zero
filtered across 61 target summaries, including all 475 UI tests. The test step
took 1,474.821 seconds including compilation, release took 441.094 seconds, and
Windows smoke closed gracefully in 2.000 seconds. Both audits passed with zero
vulnerabilities. Raw logs, timings, source fingerprints and native equivalence
are retained in `quality/companion-qualification-2026-11-08/` so removing the
worktree's Cargo artifacts does not remove this evidence. The paired speed
comparison remains pending; this record does not certify that measurement yet.

## Performance and evidence limits

The `0c3cca5` release failed the three-stream preflight at requested 1440x900:
P50 6.21 ms, P95 10.4591 ms (limit 8.33 ms), and P99 14.5195 ms
(limit 16.67 ms). The report records 120 measured samples after 20 warmup
samples, 141 fixture frames and 423 conversation updates on a 164 Hz display.
An unchanged-source closed-dock diagnostic at the same size measured P95
6.557 ms and P99 6.7605 ms. These separate reports are retained in the
qualification package. The full paired speed gate has not run. Qualification
remains incomplete; the failed preflight is not replaced by a passing retry.

Commit `fd43c7e` retains measurements for unchanged message text pieces. The
local wrapper invalidates the full inherited text style and rem size, recreates
consumed selection highlights, and reads the current layout for hit testing.
HVC-GPUI-006 requires exact wrapping-width equality: the new narrow-to-intrinsic
regression failed before that vendor correction. Ten focused selectable-message
tests passed (five new), with zero failed/ignored and 470 filtered. They cover
streamed text, width/style/rem changes and complete Unicode/table/diff copy.
Two test-fixture issues from the first behavioral run were corrected and are
identified in the Stage 7 follow-up. Full parsing, message content, fixture
update frequency and latency limits are unchanged. A separate review found no
concrete correctness defect in the wrapper or width correction. This newer
production source requires another complete regression gate and fresh
performance evidence; identity reuse alone does not establish a speed gain.

The `fd43c7e` release built successfully in 8 minutes 18 seconds. Its first
three-stream preflight retained the same 141 frames, 423 conversation updates,
120 measured samples after 20 warmup samples, 1440x900 viewport and 164 Hz
display. P50 was 6.5485 ms, P95 8.4551 ms and P99 9.4659 ms. This is still
**failed** against the unchanged P95 limit of 8.33 ms; P99 remains below
16.67 ms. The report and build/test logs are archived separately. No unchanged
retry is substituted for this failed measurement. Further optimization and a
complete gate are required before integration.

Commit `a362f4f` removes cloned Markdown AST/text from the cached-message
render path and reserves buffer capacity. Other Markdown consumers retain
their owned-string path, without new text copies. Three additional tests cover
borrowed storage, moved owned storage and matching geometry across nine cases
spanning all supported block kinds. Thirteen focused tests passed with zero
failed/ignored and 470 filtered. Strict UI Clippy initially rejected a complex
test-fixture type; a local type alias corrected it without suppressing the lint.
Strict Clippy, the 13 tests, formatting and diff whitespace checks then passed.
Its release built successfully in 7 minutes 16 seconds. The first three-stream
preflight failed: P50 6.8889 ms, P95 12.1084 ms and P99 14.6862 ms. The workload,
viewport, display rate, warmup and limits match the previous attempts. A
separate same-source closed-dock diagnostic measured P50 6.0564 ms, P95
6.6614 ms and P99 6.957 ms. Reports, binary/source fingerprints and logs are
archived separately. This measurement does not demonstrate a gain from the
allocation refinement. Investigation now requires observed costs and sources
of variation before another optimization; no unchanged retry replaces this
failure. The complete and paired gates for this source remain pending.

Read-only bootstrap inspection found that initial core migrations complete
before the window opens, but first activation starts quota refresh and CLI
discovery while the frame probe is running. A fresh application data directory
does not bypass global CLI discovery. That establishes possible concurrent
work, not the cause of the observed P95. A temporary debug-only diagnostic will
record per-frame parse/build/layout/paint costs, text-layout cache constraints
and coarse startup spans, without changing workload, warmup, readiness or
thresholds. Its patch and source restoration proof must be retained; debug
timings do not qualify release performance.

The single instrumented debug run completed with all 141 fixture frames and
423 conversation updates. Across its 120 sampled frames, 29,340 message pieces
caused 352,080 text measurements: 176,040 hits and 176,040 misses, of which only
360 were cold. The other 175,680 misses changed wrapping constraints. Each
piece was measured 12 times per frame. In the last frame, 423 pieces caused
5,076 measurements and 2,538 misses, with only three cold pieces. The existing
single-state cache therefore loses useful measurements as layout alternates
constraints. This is the next correction target; parsing/allocation was not
the dominant observed cost.

Debug means were approximately 56 ms for root layout/prepaint, 1.16 ms for
parsing, 2.1 ms for message building including parsing, 10.9 ms for painting
and 0.27 ms for present. These instrumented debug numbers locate work; they
are not estimates of release latency. All 33,420 background-paint calls had
no background color, traversing 989,343 glyphs; 25,020 of 29,340 message paints
were outside the content mask. Quota discovery remained in progress during
the trace; causality for the release P95 has not been established.

The temporary patch, sidecar, analysis and restoration proof are archived in
`quality/companion-qualification-2026-11-08/diagnostic-a362f4f/`. Nine original
files were restored byte-for-byte, the temporary module was removed, and all
965 source hashes match the pre-instrumentation snapshot. The release binary
was not rebuilt by this diagnostic. The proposed correction is a small bounded
cache retaining exact wrapping/truncation measurements, with existing style,
rem, selection and hit-testing invariants preserved.

Commit `42219bb` implements HVC-GPUI-007: one active measurement plus up to
three saved states, with exact wrapping/truncation keys and least-recently-used
eviction. Boxed states move without copying inline glyph decorations. Bounds
are cleared on reuse and renewed during prepaint. Measurement counters and
read helpers are compiled only for `test`/`test-support`, so tests can observe
actual repeated measurements without adding release instrumentation.

Three new behavioral regressions failed against the single-state cache and
passed after the correction. Repeated four-constraint cycles now retain four
measurements rather than growing to 16; the three-constraint truncation cycle
retains three rather than growing to 12. Tests cover fifth-key eviction,
Unicode hit testing, ellipsis and full style/rem invalidation. All 16 focused
tests passed, with zero failed/ignored and 470 filtered; formatting, vendor
Rust 2024 formatting, strict UI Clippy and whitespace checks passed. A separate
review found no concrete LRU/key/bounds defect. The release preflight and
complete/paired gates for this source remain pending. No background-paint,
readiness, workload, warmup or threshold change was included.

The `42219bb` release built successfully in 8 minutes 57 seconds. Its first
three-stream preflight again failed the unchanged P95 limit: P50 6.1556 ms,
P95 9.9154 ms and P99 11.7028 ms. The report confirms 141 fixture frames,
423 conversation updates, 120 samples after 20 warmup samples, 1440x900 and
164 Hz. The cache tests establish reuse and bounded retention; this failed
release run does not qualify speed. The build, raw report and binary/source
fingerprints are archived with the other 2026-11-08 attempts.

The 2026-11-09 continuation targets the background traversal already observed
in the trace. The narrow correction will preserve `paint_layer` and its scene
ordering/replay behavior, returning inside that closure only when every
background color is absent. A `Some` color, including transparent, must retain
the original path. Behavioral drawing and ordering tests precede the change.
The new qualification artifacts use `quality/companion-qualification-2026-11-09/`;
the earlier failed evidence remains unchanged.

Commit `d71a573` contains HVC-GPUI-008/009. Background painting skips metrics
and glyph work only for all-`None` backgrounds, inside the existing layer.
Foreground painting skips traversal only when a conservative envelope proves
the old glyph visibility predicate cannot pass. It repeats wrap Y additions,
includes all font heights, preserves `Some` underline/strike and falls through
for invalid/non-finite inputs. Layers, order and replay remain intact.

The background RED passed five and failed one; its GREEN passed six. The
foreground RED passed seven and failed three; its GREEN passed 12. The complete
GPUI library suite then passed all 82 tests, with zero failed/ignored/filtered;
the 12 focused tests are included in that total. These vendor tests are
separate from the default application workspace suite.

The temporary test harness preserved all 794 package identities while adding
only required graph edges, then restored both manifests and the lock byte for
byte. Root verification independently compared each restored file to its
backup. Logs, harness diff, lock snapshots, identity/restoration proofs and
executed source hash are retained in
`quality/companion-qualification-2026-11-09/`. The redundant raw Cargo metadata
dump is omitted; its dependency identities can be recomputed from the retained
lock snapshots. Source `line.rs` SHA-256:
`8db9faf989f682116d7c6bfa461d9fdfaea668d18133fb6c4148299dd53e8fd7`.

With the original workspace restored, formatting and strict UI Clippy passed.
The complete UI library suite passed 486 tests in 98.83 seconds, with zero
failed/ignored/filtered. The independent painting review found no concrete
correctness defect. The release preflight, full application gate, current
native inspection and paired speed qualification remain pending for this
source. No workload, cadence, warmup, readiness or limit changed.

The `d71a573` release built in 8 minutes 04 seconds. Its first three-stream
preflight failed: P50 6.1209 ms, P95 12.3584 ms and P99 15.0495 ms. The valid
fixture retained 141 frames, 423 updates, 120 measured samples after 20 warmup,
1440x900 and 164 Hz. Functional tests and skipped-work assertions do not
establish release speed acceptance. The failed report and binary/source
fingerprints are retained in the 2026-11-09 package.

The next diagnostic will sample CPU by thread of the exact launched release
process, distinguishing its UI thread from workers and initialization. It is
a separate instrumented observation, not a qualifying retry or replacement of
the failed preflight. The official script, release binary, source, workload
and latency limits remain unchanged.

Compare the frozen baseline and candidate with the same fixtures, toolchain,
profile and concurrency, excluding compilation and warmup. Record calibration
noise before evaluating the candidate. Check the happy-path call count and
transition writes, the closed dock, and controlled concurrent streaming.

GPUI probes measure animation-frame callback intervals, not compositor
presentation. Scripted model replies and character-derived token estimates
test orchestration; they do not establish the quality or speed of real models.

## Current full gate and native inspection (2026-11-09)

The frozen `d71a573` full Stage 7 run completed eight of nine checks in
1,762.757 seconds. The complete workspace suite passed 2,158 tests, with zero
failed, 23 ignored and zero filtered across 61 targets. UI 486 and core 1,193
are included. Tests took 1,694.065 seconds including compilation; incremental
release took 16.299 seconds; the native smoke closed gracefully in 1.893 seconds.
Format, boundaries, strict Clippy and the technical supply-chain check passed.
The advisory check failed on `libsqlite3-sys 0.31.0`, RUSTSEC-2026-0459 /
CVE-2026-119375. This is a newly detected qualification blocker, not a claim
that the renderer introduced the vulnerability. No advisory suppression or
threshold change is permitted. The archived log, timings, audit and integrity
proof are in `companion-qualification-2026-11-09/stage7-d71a573/`; all 965
source hashes still matched the frozen snapshot when the gate finished.

Native inspection on the normal debug build of `d71a573` passed at 1440x900
and 960x640 client sizes. It covered three/two visible topics, recoverable
overflow, complete multiline/Unicode selection, decision controls, repeated
dock shortcuts without an intervening click, Expand and exact durable draft
restoration. No decision or message was submitted. Before/after counts stayed
at three conversations, six messages, two route executions and zero decision
bindings; the complete saved draft row remained identical. Both stderr logs
were empty; the 960 run explicitly returned exit zero. The binary hash was
unchanged. Evidence: `companion-qualification-2026-11-09/native-d71a573.json`.
This covers the current physical display scale; IME and other physical DPI
settings were not exercised by this inspection.

The external thread diagnostic completed without source or binary changes.
During the last approximately 544 ms of visible work, the UI used 515.625 ms
of CPU, with 15.625 ms in other sampled threads. This supports further UI
investigation, without attributing individual late frames to specific functions.
The diagnostic did not qualify speed or replace the failed official preflight.
Raw sampling, uncertainty and integrity records are retained in
`companion-qualification-2026-11-09/thread-diagnostic-d71a573/`.

Commit `5111c6117da219248f72e4844f3ebc593235ff33` upgrades the bundled
SQLite chain. `Cargo.lock` resolves `rusqlite 0.41.1`, `libsqlite3-sys 0.39.1`,
`refinery 0.10.0`, `refinery-core 0.10.1` and `refinery-macros 0.10.0`.
The `fallible_uint` feature preserves checked unsigned conversions. The exact
focused command was:
`cargo test --locked -p hvc-core --lib sqlite_runtime_tests -- --test-threads=2`.
The corrected RED passed the WAL behavior test and failed only the linked-engine
security floor (SQLite 3.48.0). The final GREEN passed all three tests in 2.40
seconds, with zero failed/ignored and 1,200 filtered. It checks the linked
SQLite security floor, real WAL reader snapshots, commits, rollback, migrations,
checksum preservation and reopen at 512/4096/65536-byte page limits, Unicode,
integrity and foreign keys. The third test covers zero/the signed integer
limit and rejects negative reads or unsigned overflow. It does not reproduce
the malicious-file leak; the version floor and audit enforce a patched engine.

The standalone advisory audit passed with zero vulnerabilities using RustSec
`d9cb059f3217db87aeccd190b81eaffe8cc29cb9`; informational warnings remain
recorded and no advisory was suppressed. An initial reserved `sqlite_` fixture
name, unavailable `refinery ^0.10.1` resolution and compilation without
`fallible_uint` are retained as fixture/resolution/compile diagnostics, rather
than additional application runtime failures. Eight raw logs, both audit JSON
files and a verified SHA-256 manifest are retained under
`companion-qualification-2026-11-09/sqlite-5111c61/`. Root's
`source-5111c61.json` captured 966 files with no source diff. After the gate,
all 966 hashes were verified unchanged before the next correction.
The gate attempt failed after 78.940 seconds: GPUI/Rust structure, crate
boundaries and formatting passed, then Clippy stopped with E0308 at
`hvc-test-support/src/core_harness.rs:77`. The upgraded `Target::Version`
requires `i32`; the harness supplied `u32`. The workspace test suite and later
gate steps did not start. The immutable gate log, timings and post-gate
source verification are archived alongside the SQLite evidence, with all 12
archived file hashes verified. The subsequent bounded migration-target
correction is recorded below; the failed attempt does not qualify `5111c61`.
These three focused tests are separate from the earlier `d71a573` suite,
whose eight-of-nine result and advisory failure remain unchanged.

Commit `68b5c0522fa642664f114e4bbdf8628c81db41c0` uses `i32::try_from` for
the fixture migration target and retains the `DatabaseVersion` V1..V13 bound.
The exact focused command was
`cargo test --locked -p hvc-test-support --lib core_harness::tests -- --test-threads=2`.
All four selected tests passed in 2.25 seconds, with zero failed/ignored and
20 filtered. They include two new and two existing tests. The new cases run
all thirteen supported fixture targets, checking the exact maximum version
and migration count, and reject 0/14/`u32::MAX` before conversion. The existing
isolation and upgrade cases also passed. These selections are not added to
historical full-suite totals. Harness/format logs and the readable commit
diff are preserved with SHA-256 verification under
`companion-qualification-2026-11-09/sqlite-harness-68b5c05/`.
Root's `source-68b5c05.json` records 966 files with no source diff at capture.
The complete workspace suite in the new gate passed 2,163 tests, with zero
failed, 23 ignored and zero filtered across 61 targets. This includes core
1,196 passed/7 ignored (714.63 seconds), UI 486 passed/zero ignored (115.77
seconds), and the `hvc-test-support` library's 24 passed/zero ignored (15.78
seconds). These subsets and the focused selections are not added to the total.
The Rust test gate step took 1,955.407 seconds including compilation. The
complete technical gate finished successfully: all nine checks passed with
exit zero, in 2,617.720 seconds. Release took 624.686 seconds; Windows smoke
closed gracefully in 1.886 seconds; the technical license/source audit took
1.875 seconds and the advisory audit 6.335 seconds, with zero vulnerabilities.
Informational warnings remain recorded. Final log, timings, supply-chain and
advisory reports are archived under
`companion-qualification-2026-11-09/stage7-68b5c05/`, with SHA-256 verification
of all five artifacts. All 966 frozen source hashes were rechecked after the
gate with zero drift. The release artifact is 37,889,024 bytes, SHA-256
`a788a0f42e85ff26caa0ba0d53489092c9c71507c97ce9190d80821d0de10852`.
Native reopening of the old-profile copy passed using that exact release.
The SQLite 3.48.0 profile reopened under 3.54.1 with identical counts of three
conversations, six messages, two route executions and zero decision bindings.
The complete draft-row hash and all 64 migration-history rows (name/version/
checksum) stayed identical. Integrity returned `ok`, foreign-key violations
were zero, the original profile was unchanged and stderr was empty. Alt+F4
was observed closing the app with exit zero. At the default 1280x820 client
viewport, formatted history and the durable Unicode draft rendered without
submitting a message or decision. Evidence is retained in
`companion-qualification-2026-11-09/native-sqlite-68b5c05.json`. This reopened
the existing database file; it did not exercise an old-engine live WAL pair.
The current physical display scale was used; IME and a real provider were not
exercised. The failed `d71a573` official speed preflight remains unchanged,
and no speed result is attributed to `68b5c05`. Speed and paired qualification
remain pending.

Renderer retention remains a proposal: wrapping parity at fractional DPI must
be demonstrated before adopting cached paragraph views. Speed qualification
and delivery remain pending.

## Integration and cleanup

Pending. After all required qualification passes, obtain the developer's
required confirmation before merging, pushing and removing this delivery's
worktree and temporary branches. Preserve preexisting branches and Cérebro edits.
