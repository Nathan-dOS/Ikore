# Native canvas implementation and validation

Status: basic feature validation completed in the isolated branch; integration
awaits developer approval. Started 2026-10-26; updated 2026-10-27.
Branch: `feat/native-architecture-canvas`, based on `15d596c`.
Worktree: `HardVibeCode/.worktrees/native-architecture-canvas`.
No commit, merge or push has been performed. The primary checkout and the
Mandates worktree were not modified by this implementation.

## Delivered scope

The native GPUI Canvas adds project drawings, rectangles, diamonds, ellipses,
straight arrows/lines and plain labels; selection, moving/resizing, gesture
history, styles, dark/light backgrounds, pan/zoom and spatial boundaries.
The existing native text editor supplies Unicode, IME and text undo for labels.
SQLite owns sanitized documents and optimistic revisions. A durable journal
mirrors deterministic JSON/SVG under `.hvc/drawings/`, with conflict preservation
and recovery after partial writes. Camera preferences do not change the scene
revision. Normal close/project switching preserve pending content.

Migration V57 is valid on this V55 baseline. Migration numbers must be reconciled
at integration: once V57 has been applied, adding V56 would fail refinery's missing-
migration validation. If Canvas lands first, keep V55 Cargo/V57 Canvas and number
the unpublished Mandates migrations V58/V59 or later free versions. If Mandates
lands first, assign Canvas a version after its final migration and rerun validation.
Never edit already applied migration history or disable refinery's checks. Existing
Mandates development databases must stay isolated until their transition is defined.
No existing migration was edited. Cargo manifests and lockfile are unchanged.
AI analysis, PNG, import, collaboration and advanced drawing tools are excluded.
Usage and limitations are documented in [native-canvas.md](../docs/native-canvas.md).

## Verification record

| Check | Result |
|---|---|
| Complete workspace suite, final source candidate | 1,655 passed, zero failures, 16 existing ignored tests; `cargo test-all` in `target/canvas-stage7-final.log`. |
| All UI library tests within that suite | 356 passed, zero failures. |
| Final Canvas interaction tests after focus correction | 20 passed, zero failures; `target/canvas-final-interactions.log`. |
| Preview argument/scene tests, explicit final-candidate run | 2 passed; `target/canvas-preview-tests.log`. |
| Qualified Cargo regressions with two test threads | 4 passed; `target/canvas-cargo-cache-isolation.log`; both runner cases also passed in the complete suite. |
| Core/UI dependency boundary | Passed; the script now checks core → GPUI/UI edges too. |
| Final complete Stage 7 orchestration | Passed all 9 steps in one invocation, with no skipped steps, in 1,009.842 seconds. |
| Source candidate after final gate | All 28 source hashes match the fingerprint recorded before execution. |
| Release and Windows smoke | Optimized app built; isolated database initialized; real window opened and closed gracefully, exit 0. |
| Rust advisories | Exit 0, zero vulnerabilities; 14 unmaintained and 2 unsound informational warnings inherited from the lockfile. |
| Physical Windows walkthrough | Native drawing, movement/bindings, handles, backgrounds, Unicode labels, immediate focus, Ctrl+Enter, Undo and autosave observed. Detailed limits below. |

The complete source candidate is identified by the per-file hashes in
[canvas-source-fingerprint-2026-10-27.json](canvas-source-fingerprint-2026-10-27.json).
Those hashes cover changed/untracked executable sources without creating a Git
commit. Final checks must apply to that candidate; later edits invalidate reuse.

The final [Stage 7 timing record](canvas-stage7-run-2026-10-27.json) has
`passed: true` and all nine steps approved. An [earlier setup failure](canvas-stage7-setup-failure-2026-10-27.json)
is preserved: cargo-audit was initially unavailable. The already installed 0.22.2
tool was copied into this worktree's local tool directory, the advisory script
passed, and the complete gate was then rerun after the final migration-order
documentation correction in the upgrade test. The
[qualification record](canvas-qualification-2026-10-27.json) binds the final gate,
source fingerprint and release executable hash. The
[advisory summary](canvas-advisories-2026-10-27.json) records its RustSec revision.

The [source/license audit](canvas-supply-chain-2026-10-27.json) passed its technical
checks across 560 packages. The repository's existing binary-distribution hold is
still in effect under [distribution-policy.json](distribution-policy.json);
source publication is already approved by that policy.

## Regressions and corrections

- Canvas pane roots needed explicit height inside the shared split pane. The
  initial empty surface was reproduced by both layout and pointer tests, and in
  the real window. Tests now require usable geometry at 960×640 and 1280×800.
- A Unicode test uses escaped source literals so it exercises user-owned text
  without violating the existing English UI-source guard.
- Save acknowledgements cannot replace an in-progress pointer preview or title
  input. Refresh re-arms a dirty autosave, and stale sync snapshots cannot mark a
  newer revision saved, erase a newly created drawing or resurrect a deleted one.
  Five deterministic GPUI regressions cover these cases.
- Boundaries created after their children now paint behind them. Hit-testing,
  GPUI and SVG share the containment-aware order.
- The session active-view validator accepts `canvas`. Project restoration also
  starts mirror recovery when another view is active.
- Ctrl+Enter in a label cannot dispatch the global New work flow. Label focus is
  deferred until pointer dispatch completes, with generation/context/focus guards;
  typing works immediately after double-click and a canceled request cannot steal
  search focus. Both regressions run against native editor bindings.
- Normalized element order no longer produces a false secret-protection warning.
  Real title/label redaction has a separate quiet notice. Connector labels clear
  the segment by six world units; GPUI, SVG and export bounds share that rule.
- Existing base revision `c42afb0` had stopped rejecting truncated custom-check
  output. The failing `oversized_output_is_capped_and_marked_truncated_without_deadlock`
  regression required restoring that fail-closed condition. No parser was relaxed.
- The initial full suite also reported unsupported qualified-Cargo stderr. The
  original failure lacked the actual line; its assertion now includes sanitized
  stderr. An isolated native rerun passed. The subsequent Stage 7 run reproduced
  the failure in `cargo_check`: stderr contained two `Blocking waiting for file
  lock on package cache` status lines, followed by successful Checking/Finished
  output. The path-only Cargo fixtures had private build targets but shared the
  user's package cache. Both qualified-Cargo tests now explicitly create and pass
  a private temporary `CARGO_HOME` as well. This is test-harness isolation; the
  production parser and its rejection of unknown output remain unchanged.
  Four filtered tests passed with two threads, and both runner cases then passed
  in the complete workspace suite. The original cache-contention failure log is
  retained at `target/canvas-stage7-before-cache-isolation.log`.
- A Windows-only read-only-attribute operation triggered a cross-platform Clippy
  warning. A narrow allowance documents the platform-specific branch; Unix still
  sets mode 0644. The existing materialization test now checks that a newly staged
  file remains writable.

Sandboxed MSVC linking failed with LNK1114 while replacing generated libraries.
Native validation runs use this worktree's own target directory and two Cargo
build jobs. A sandboxed candidate-promotion failure was also not treated as test
evidence. No target cache is shared with another worktree.

## Coverage and qualification limits

Domain/service tests cover schemas, bounded invalid input, sanitization, XML,
geometry, bindings, containment, deterministic outputs, project scoping, revisions,
partial mirror recovery, external conflicts, unavailable roots and safe deletion.
External integration tests exercise AppCore and populated V55→V57 migration,
including project purge with pending mirrors. UI tests cover navigation, focus,
input/undo, gesture lifecycle, zoom extremes and asynchronous acknowledgements.
Windows ran 20 domain and 14 service cases; the additional managed-directory
symlink service case is Unix-only and is not counted as executed on Windows.
Per-target counts and the 16 existing ignored cases are recorded in
[canvas-test-results-2026-10-27.json](canvas-test-results-2026-10-27.json).

The disposable `canvas_preview` example supplies deterministic mixed scenes with
`--elements 100` and `--elements 500`. The existing UI probe measures intervals
between GPUI callbacks, not compositor presentation or actual input latency.
Machine context is recorded in
[canvas-benchmark-machine-2026-10-27.json](canvas-benchmark-machine-2026-10-27.json).
GPU/DPI/platform observations must not be generalized into a universal FPS claim.

The real disposable window was inspected at the default 1280×820 client size
(1282×850 captured outer window). Drawing a rectangle exposed eight resize handles;
moving a connected node moved its arrow endpoints. Dark/light appearance, multiline
Unicode text, label-local focus, Ctrl+Enter and scene Undo were exercised. The
final label edit reached Saved before normal close. The walkthrough used temporary
application data, with no Canvas model calls. Existing account/quota UI may still
be present because the example uses the full AppModel.

The physical walkthrough is not an exhaustive E2E certification of OS IME candidate
windows, high-DPI configurations, external SVG viewers or every failure path.
Automated geometry/input and real SQLite/filesystem integration tests supply those
specific code-level checks; they do not establish physical visual equivalence.

After sampling completed, the release 100-element scene also exercised immediate
label focus, Unicode/multiline replacement, local Ctrl+Z/Ctrl+Y and Ctrl+Enter.
Undo restored the initial label inside the open editor, Redo restored the new text,
and applying it closed the overlay without opening the global flow dialog. The
500-element scene was panned with H and pointer drag, then closed normally.

Release measurements used the repository's optimized LTO profile, a 1280×820
logical client window and 2,000 callback intervals per run, discarding 1,000 warmup
intervals and reporting the remaining 1,000. The native windows were explicitly
activated. No Canvas Cargo build ran during sampling; other desktop workloads
were not controlled. The release example includes the GPUI test-support feature
variant; this characterizes the fixture, not compositor presentation by the
production executable on every machine.

| Elements | p95 | p99 | p95/p99 ≤16.7 ms | Strict probe p95 ≤8.33 ms / p99 ≤16.67 ms |
|---|---|---|---|---|
| 100 | 6.7732 ms | 7.1737 ms | Met | Passed |
| 500 | 13.2121 ms | 14.8072 ms | Met | Failed on p95 |

The [performance record](canvas-performance-2026-10-27.json) links the raw samples,
launch/activation context, fixture executable hash and machine inventory. The
500-element report keeps `passed: false` under the existing stricter probe; its
threshold was not changed. Both p95 and p99 fit the planning budget. In the
500-element case, 4 of 1,000 intervals exceeded 16.7 ms and the maximum was
18.9069 ms; the 100-element maximum was 8.0341 ms with no interval above 16.7 ms.
This does not establish presented FPS or measured pan/drag/save latency.

Preliminary callback measurements used the unoptimized debug example, 1280×800
client size, 600 observed intervals with 200 warmup intervals (400 reported),
no Canvas Cargo build running and a fitted mixed scene. Other desktop workloads
were not controlled:

| Elements | Composition | p95 | p99 | Existing probe thresholds |
|---|---|---|---|---|
| 100 | 45 shapes, 33 arrows, 11 boundaries, 11 texts; 89 labels | 54.2104 ms | 59.0697 ms | Failed |
| 500 | 224 shapes, 166 arrows, 55 boundaries, 55 texts; 445 labels | 141.3816 ms | 147.3807 ms | Failed |

Raw observations are preserved in `canvas-callback-100-debug-2026-10-27.json` and
`canvas-callback-500-debug-2026-10-27.json`. These results do not meet the 16.7 ms
planning target. The probe's own stricter thresholds are p95 ≤8.33 ms and p99
≤16.67 ms. It measures idle full-AppModel render callback intervals with animation
requested; it does not measure presented FPS, pan/drag latency or save latency.
The release observations above are the current characterization. The debug data is
retained as preliminary evidence with a different window size and warmup count;
no direct speedup ratio or universal FPS guarantee is inferred.

The final `scripts/gate-stage7.ps1` invocation approved architecture, crate
boundaries, formatting, Clippy with warnings denied, full tests, release build,
Windows smoke, source/license audit and advisories. Earlier failed attempts remain
historical evidence. No commit, merge, push or worktree cleanup has been performed;
the branch remains active for review and integration approval.
