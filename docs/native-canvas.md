# Native architecture canvas

Companion → Canvas (`Ctrl+0`) keeps project diagrams inside HVC. Create a drawing,
then use rectangles, diamonds, ellipses, arrows, lines and plain text.
Select shapes to move/resize them; double-click or Enter edits a label. The label
editor supports Unicode, IME and its own text Undo. Enter applies without adding a
line; Shift+Enter inserts a line. Ctrl+Enter remains an Apply shortcut, and Escape
cancels. All labels use one font family; A−/A+ changes their size from 8 to 96 units,
including labels inside shapes and above connectors. There is no font-family picker.

The surface shortcuts are V/H/R/D/O/A/L/T/E, Q to keep a tool active, Space-drag or
middle-drag to pan, Ctrl-wheel to zoom, and Ctrl+Z/Y to undo/redo complete gestures.
Tool keys do not apply while typing in an input. Shapes and connectors use a shared
geometry model for hit-testing and SVG export. Moving a bound shape moves its arrow
endpoints. Dashed rectangle boundaries record spatial containment without grouping
children for movement.

Each tool button shows an SVG icon and its shortcut letter. Hover shows the full
tool name and shortcut. New elements use Sketch strokes, and new rectangles have
rounded corners. Clean strokes remain available. Sketch introduces a small, stable
variation in the outline rather than randomness on every frame. Arrow tips use an
open pen-like stroke. Arrows and lines offer Straight, Curve left and Curve right;
the bend is relative to their ordered endpoints, so drawing in another direction
also changes the orientation. Bound endpoints still follow their target shapes.
Curved labels follow the curve midpoint. Hit-testing, visual bounds and SVG export
use the shared curve geometry. Intermediate-point editing and automatic routing
remain future work.

The optional dot grid stays anchored to world coordinates. It uses one GPUI paint
layer, a minimum spacing of 24 logical screen pixels and at most 4,096 dots in the
visible area. Zooming out or enlarging the viewport skips alternate world grid
lines to respect those bounds. This groups native quads for scene ordering; it is
not a raster cache.

Gesture Undo/Redo stays in memory, with at most 100 snapshots and an estimated
32 MiB budget. Loading another drawing or restarting clears that history. The label
editor has separate text history while the label is being edited.

SQLite owns each drawing, scoped by project and optimistic revision. Autosave waits
600 ms after a completed edit. Camera preferences are stored separately and do not
change the content revision. The project mirror contains `.hvc-canvas.json` and
`.svg` files under `.hvc/drawings/`, with stable UUID-based names. Renaming a drawing
changes its title, not its filenames. JSON/SVG are one-way exports; external edits
are not imported automatically and conflicting files are preserved.
Copying/cloning the exported files into another workspace does not restore drawings
into another HVC database; native import is future work.

The UI refinement adds optional element fields to the scene JSON:
`label_font_size`, `stroke_rendering` and `connector_curve`. Missing fields preserve
the previous label size, Clean strokes and Straight connectors. Default visual
fields and an absent label-size override are omitted when serialized, so legacy
scenes keep their prior representation. The current reader accepts those older
scenes; older readers reject unknown new fields. This is not forward compatibility
with older builds. The SQLite schema and its V57 migration do not change.

The status distinguishes saved content from pending or conflicting project files.
Retry completes recoverable writes; Export copy creates a separate drawing and new
managed outputs. A copy does not clear the original conflict: restore or remove the
conflicting file and Retry to resolve the original drawing's pending outputs.
Each file replacement is atomic individually; the JSON/SVG pair is recoverable,
not a filesystem transaction. Hash checks do not provide an operating-system CAS.
Project switching and normal window close wait for pending content
commits. Failed saves preserve the draft. A forced process exit before a commit can
lose the last gesture. Committed mirror intentions replay on the next project load.

Recognizable secrets are redacted before persistence/export using the host's existing
detector. The canvas writer is restricted to its managed directory; the general
Markdown/text editor's extension policy is unchanged. Core has no GPUI dependency.
No new Cargo dependency is required. AI analysis, PNG export, import, freehand,
rotation, multi-selection and collaboration are future work.

Run `cargo test-all` for regression coverage and the Stage 7 gate for final checks.
`cargo run --locked --offline -p hvc-ui --example canvas_preview` opens a disposable
native walkthrough using temporary data and no model calls.

The UX/grid revision was implemented and validated directly on main on 2026-10-27,
without a new branch, worktree, commit or push. The complete
workspace suite passed 1,678 tests with zero failures, 16 existing ignored tests,
zero filtered tests and 54 target summaries in 1,140.378 seconds. The full UI suite
passed 368 tests. Focused checks passed 42 core tests, 31 Canvas UI tests and 9
example-fixture tests; these overlapping runs must not be added to the workspace
total. The CLI integration
target also passed its 15 enabled cases, with 6 existing authenticated cases ignored,
after isolating installed-CLI discovery in its fixture. Five Portuguese sample
literals in Canvas UI tests use equivalent Unicode escapes to comply with the
existing UI-language audit; runtime text and Unicode regression coverage remain
the same, and the audit was not relaxed. The final source fingerprint was updated.
All nine Stage 7 checks passed in one final run, including the release-app build
(512.484 seconds), Windows smoke (2.603 seconds, graceful close and exit 0), and
technical license/source and advisory audits. The audits covered 560 packages and
reported zero vulnerabilities; 14 unmaintained and 2 unsound warnings are inherited.
The existing binary-distribution hold remains, and this revision publishes nothing.
Detailed counts are in the
[workspace test record](../quality/canvas-refinement-test-results-2026-10-27.json)
and [final gate record](../quality/canvas-refinement-stage7-run-2026-10-27.json).
The smoke and walkthrough do not certify every display/DPI setting or operating-system
IME candidate window.

The release grid comparison used an otherwise empty 1280×820 logical-pixel native
canvas, display scale 1, animated pan, 500 warmup callbacks and 1,000 samples per
case. The p95 CPU time to insert the grid changed as follows:

| Zoom | Legacy | Optimized | Reduction |
|---|---:|---:|---:|
| 100% | 1.1668 ms | 0.0693 ms | 94.06% |
| 50% | 5.8469 ms | 0.0694 ms | 98.81% |

The 100% case keeps the old dot density, isolating batching; 50% also includes the
lower density. GPUI callback p95 stayed around 8 ms, close to grid-off. This measures
grid-insertion CPU and callback intervals, not compositor FPS or a 500-element
diagram. Other desktop workload was uncontrolled. The
[comparison and raw-sample hashes](../quality/canvas-grid-comparison-2026-10-27.json)
and [validation record](../quality/canvas-refinement-validation-2026-10-27.md) preserve
the method, limits and completed validation.

## Canvas AI authoring candidate — 2026-10-28

The local main candidate adds explicit Companion offers, developer consent,
hidden generation and an **Open Canvas** result button, plus shared semantic
validation for generated and manually edited architecture/process drawings.
The [authoring implementation](canvas-ai-authoring.md) describes V58, target
freezing, recovery and the restricted BPMN profile. On 2026-10-29, local technical
qualification passed all nine gate checks. The complete workspace regression
passed 1,782 tests, with zero failures and 16 existing ignored tests. No task branch
or worktree was created, and no commit or push was performed. Authenticated
providers and physical display/DPI/IME checks
remain unqualified; the optional document resolver is not exposed. The
[workflow validation record](../quality/canvas-ai-workflow-validation-2026-10-28.md)
records evidence and qualification limits.
