# Canvas interaction and grid refinement — 2026-10-27

Status: implemented, measured and validated on 2026-10-27 (America/Sao_Paulo).
Working directly on `main`, based on `92f6a0c`, at the maintainer's request. No branch,
worktree, commit or push is created for this revision. Pre-existing Whisper changes
remain in the checkout; validation runs against that combined working tree. The
Mandates worktree is untouched. Cerebro documentation stays local.

## Changes and contracts

- Dot grid: native quads share a single GPUI paint layer instead of inserting an
  ordering-tree entry per dot. Minimum spacing is 24 logical pixels; at most 4,096
  visible dots. Power-of-two subsampling preserves a world-origin anchor during
  pan and zoom. This is bounded batching, not a raster cache.
- Tools: SVG glyph plus shortcut letter for Select, Pan, Box, Decision, Ellipse,
  Arrow, Line, Text and Erase; full name and shortcut on hover. Lock has an open or
  closed padlock plus Q. New custom glyphs are original embedded SVG drawings;
  the remaining glyphs reuse the app's existing icon inventory.
- Drawing: new elements default to deterministic Sketch outlines; new rectangles
  are rounded. Clean is available. Arrows/lines support Straight, Curve left and
  Curve right, relative to their ordered endpoints; direction follows the drag.
  Sketch arrowheads are open strokes. There is no routing or intermediate-point
  editor. Core vector paths are shared with SVG; curves affect labels, hit-testing,
  culling, fit and bound endpoints.
- Text: one font family, A−/A+ from 8 to 96 units for all labels. Enter applies
  without appending a newline, Shift+Enter inserts one newline, Ctrl+Enter remains
  compatible, Escape cancels. Local editor history and composition are preserved;
  unfinished composition or invalid text cannot silently switch to another label.
- Storage: optional JSON fields `stroke_rendering`, `connector_curve` and
  `label_font_size` preserve the old defaults when absent. Default visual fields
  and absent font overrides remain omitted from canonical serialization. Current
  builds read older scenes; older readers reject new fields. No SQL migration,
  dependency, manifest or lockfile change is needed.

## Verification

The focused native core run passed 42 tests; focused Canvas UI passed 31 tests.
Both example fixtures passed explicitly (7 grid benchmark tests, including its 4
imported grid tests, plus 2 Canvas preview tests). These overlapping runs must not
be added to the full-workspace total. Both examples were rerun after the final
gate: all 9 tests passed again. The final full workspace passed **1,678 tests,
zero failures, 16 existing ignored cases and zero filtered cases**, across 54 target
summaries, with two test threads. UI accounts for 368 of those approvals.

All nine technical gate checks passed in one invocation, in **1,675.002 seconds**.
The full-suite step took 1,140.378 seconds. The run ended at 00:26 UTC on October 28,
which is still October 27 in the maintainer's timezone.

| Gate check | Seconds | Result |
|---|---:|---|
| GPUI/Rust-only structure | 0.643 | Passed |
| Crate boundaries | 0.116 | Passed |
| Formatting | 4.777 | Passed |
| Clippy, warnings denied | 6.810 | Passed |
| Full Rust suite | 1,140.378 | Passed |
| Release application build | 512.484 | Passed |
| Windows smoke | 2.603 | Passed; graceful close, exit 0 |
| Licenses and Rust sources | 1.847 | Passed; 560 packages |
| Rust advisories | 5.320 | Passed; zero vulnerabilities |

The advisory report retains 14 unmaintained and 2 unsound informational notices
already present in the pinned graph. No distribution policy was changed.

Evidence: [gate timings](canvas-refinement-stage7-run-2026-10-27.json),
[test results](canvas-refinement-test-results-2026-10-27.json),
[qualification and binary hashes](canvas-refinement-qualification-2026-10-27.json),
[supply chain](canvas-refinement-supply-chain-2026-10-27.json) and
[advisories](canvas-refinement-advisories-2026-10-27.json).

One full gate run stopped at the unrelated CLI integration test
`companion_cli_uses_existing_cli_auth_without_hvc_credentials_and_persists_only_text`:
its 30-second idle assertion timed out because discovery still reached installed
CLIs outside the fixture. Only Codex/Claude had fixture executables; the real
Antigravity authentication probe permits 45 seconds. The sole correction outside
Canvas isolates discovery by providing the existing `cli_fixture` executable under
all five backend names, as a neighboring test already does. Production code,
timeouts and behavioral assertions are unchanged. The isolated test then passed
in 4.42 seconds (`cli-isolation-after.log`). The complete CLI target then passed
all 15 enabled cases, with 6 existing authenticated cases ignored, in 48.13 seconds.
The final gate also passed all 15 enabled CLI cases. No timeout or assertion was relaxed.

The first sandboxed core run had 6 filesystem failures, including Windows error 5
and missing exports after denied replacement. The identical 42-test run passed
with native filesystem access. No persistence implementation was weakened to work
around sandbox permissions. An intermediate Clippy error in the new benchmark's
Render return type was corrected; final gate Clippy passed with `-D warnings`.
An earlier build also overlapped an unfinished agent edit and was discarded; it
does not qualify the final working tree.

Another gate attempt stopped at MSVC LNK1114 when replacing an old generated
`pulsation_observation.lib`. Removing only that fixture's generated `.lib`/`.exp`
allowed recompilation; sources and application data were preserved. Historical
failure logs/timings remain beside the final run.

A later full-suite run reached the UI with every earlier target passing, then
reported one source-language audit failure among its 368 UI cases. Five new Unicode
test literals used Portuguese characters directly. They now use the same Unicode
escapes as the existing fixtures, preserving the exact runtime strings and the
language audit unchanged. This changes only test source spelling; production and
benchmark source hashes are unchanged. The source fingerprint includes the fixed
fixture. The complete UI then passed all 368 tests in 92.82 seconds, including the
unchanged language audit. The final gate repeated the complete UI and workspace
successfully on that same fixed source.

Raw local logs: `target/canvas-refinement-2026-10-27/`.
[Source fingerprints](canvas-refinement-source-fingerprint-2026-10-27.json) identify
the 28 source/asset files in this revision. Final hash checks found no drift in
those files or the six raw measurement reports. Pre-existing Whisper work remains
unstaged and was checked independently against its preserved manifest/backups.

## Measurement method

`canvas_grid_benchmark` opens a disposable native window with no application
database or project. Compare `legacy`, `optimized` and `off` with the same window,
zoom, pan, samples and warmup. It records:

- CPU elapsed around grid insertion only, excluding scene finish, GPU and compositor;
- interval between GPUI animation render callbacks;
- raw samples, logical canvas dimensions, display scale factor and calculated dot
  counts (not a GPU draw counter).

Zoom 1 preserves the original spacing, isolating batching. Zoom 0.5 also includes
the reduced density. The same release binary ran six cases with a 1280×820 logical
canvas, scale factor 1, animated pan, 500 warmup frames and 1,000 samples per case.
Ryzen 5 5600, approximately 32 GiB visible RAM, Windows 11 Pro; display inventory
includes an RTX 5060 Ti at 2560×1440/164 Hz. Other desktop workloads were uncontrolled;
no Canvas Cargo build ran during measurement.

| Zoom | Grid | Expected dots | CPU insertion p95 (ms) | Callback p95 (ms) | Callback p99 (ms) |
|---|---|---:|---:|---:|---:|
| 100% | Legacy | 1,802–1,855 | 1.1668 | 8.2337 | 8.7691 |
| 100% | Optimized | 1,802–1,855 | 0.0693 | 8.1035 | 8.6320 |
| 100% | Off | 0 | 0.0001 | 8.0952 | 8.6877 |
| 50% | Legacy | 7,208–7,383 | 5.8469 | 8.2010 | 8.6388 |
| 50% | Optimized | 1,802–1,855 | 0.0694 | 7.9579 | 8.5143 |
| 50% | Off | 0 | 0.0001 | 7.9948 | 8.6354 |

The grid's CPU insertion p95 decreased by **94.06%** at zoom 1 and **98.81%** at
zoom 0.5. The second comparison combines batching and density reduction. Callback
intervals stayed close to the grid-off cases: these data establish lower grid CPU
cost and more headroom, not a comparable percentage gain in displayed FPS. The
fixture does not measure complete diagram performance, input-to-photon latency or
compositor presentation/FPS.

[Comparison](canvas-grid-comparison-2026-10-27.json) records the exact binary hash,
machine/source records, six raw sample files and their SHA-256 hashes. Release
fixture compilation reported 13 min 13 s; build time is outside the sample windows.

```powershell
cargo build --release --locked --offline -p hvc-ui --example canvas_grid_benchmark --example canvas_preview
.\target\release\examples\canvas_grid_benchmark.exe --mode optimized --zoom 1 --width 1280 --height 820 --samples 1000 --warmup 500 --pan animated --report target/grid-optimized-z1.json
```

## Native walkthrough

The disposable debug Canvas preview confirmed embedded glyphs/letters and the Text
hover, new rounded Sketch rectangle by drag, immediate Unicode label input,
Shift+Enter adding the second line, Enter applying without another newline, A+
changing the label from 16 to 18, an open-stroke arrow and Curve left, grid off/on,
Pan with the grid enabled, Saved status and normal window close. Credential
endpoint collectors were disabled only in that preview process. The walkthrough
uses a temporary database and is not a frame-rate measurement. Its binary hash and
scope are in `canvas-refinement-walkthrough-2026-10-27.json`.

Automated GPUI tests additionally cover history, invalid sizes and composition
guards. This does not certify OS IME candidate windows or every display/DPI
configuration.
