# Native architecture canvas

Companion → Canvas (`Ctrl+0`) keeps project diagrams inside HVC. Create a drawing,
then use rectangles, diamonds, ellipses, straight arrows, lines and plain text.
Select shapes to move/resize them; double-click or Enter edits a label. The label
editor supports Unicode, IME and its own text Undo; Ctrl+Enter applies the label.

The surface shortcuts are V/H/R/D/O/A/L/T/E, Q to keep a tool active, Space-drag or
middle-drag to pan, Ctrl-wheel to zoom, and Ctrl+Z/Y to undo/redo complete gestures.
Tool keys do not apply while typing in an input. Shapes and connectors use a shared
geometry model for hit-testing and SVG export. Moving a bound shape moves its arrow
endpoints. Dashed rectangle boundaries record spatial containment without grouping
children for movement.

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
