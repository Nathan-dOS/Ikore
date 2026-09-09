# Pulsations review, status groups, and Workspace columns

Date: 2026-09-09

## Behavior and architecture

Pulsations uses the display order Running, Checkpoint, Planning, Idle, Failed, Done. Running remains first even when empty and starts expanded; other groups start collapsed and appear when populated. Entries within groups sort by creation timestamp descending, then id descending for deterministic ties. Selection and expansion choices survive polling and tab navigation. Runtime queue sequence and scheduling rules are unchanged. Group choices reset with the project model and are not serialized across application restarts.

Review defaults to the complete persisted output of the exact waiting checkpoint. Document tabs expose all candidate artifacts from its producer job, generation, input revision, and stage, in output order. An unrelated open Workspace tab cannot replace the document. The core validates the checkpoint id, pulsation, state version, purpose, and unique waiting checkpoint within a SQLite read transaction. The UI loads asynchronously, coalesces reads for the same checkpoint, and rejects late responses after selection/version/project changes. Missing legacy content reports an error with retry; it is never reconstructed or presented as a draft. Approval requires successfully loaded documents. Request revision and Reject remain available with their existing core admission rules.

The reader offers rendered Markdown, complete Markdown source, and full-content copy. A compact header, document tabs, optional feedback field, and fixed decision footer leave the document in its own scroll viewport. Review shortcuts explicitly exclude focused NativeTextInput fields, so typing feedback cannot approve, reject, or request revision. Review suppresses the duplicate pipeline header; other Pulsations sections retain it. The shared Markdown renderer now retains table headers and rows instead of silently dropping them. Raw HTML remains suppressed in rendered Markdown. Source mode presents inert text.

Workspace and Pulsations use a shared six-logical-pixel divider with a resize cursor, hover/drag highlight, mouse capture outside the divider, and double-click reset. The interaction is informed by [VS Code's custom layout](https://code.visualstudio.com/docs/configure/custom-layout). User widths are held by AppModel entities across navigation. Measured bounds never overwrite the preferred width on window resize. Nested percentage layouts resolve against the current parent width, with clipping even before the next measurement frame.

| Split | Default | Bounds in logical pixels |
| --- | --- | --- |
| Pulsations list / detail | 292 px list | List 220–480 px; detail at least 420 px |
| Workspace tree / editor | 220 px tree | Tree 160–400 px; editor area at least 480 px |
| Workspace source / preview | 60% / 40% | Source at least 240 px; preview at least 220 px; normally 20–80% |
| Workspace content / optional context | 78% / 22% | Content at least 480 px; context at least 180 px; normally 20–80% |

When the window cannot satisfy both minima, available width is apportioned between them. Panels stay inside the container and the preferred width is recovered when space returns. Dragging affects adjacent columns only. No persistence migration or new dependency is introduced by this change.

NativeEditor clips text, selection, and caret together. Horizontal wheel/Shift+wheel and caret tracking preserve access to long lines. Unicode hit testing and IME bounds use the same shaped text offsets; fractional vertical scroll includes partial rows. Horizontal extent is computed from visible lines to preserve virtualization.

## Validation

| Command | Result |
| --- | --- |
| `cargo test -p hvc-ui -p hvc-editor --lib --locked --offline` with `RUST_TEST_THREADS=1` | PASS: 130 UI tests and 15 editor tests |
| `cargo test -p hvc-core hitl_review --locked --offline` with `RUST_TEST_THREADS=1` | PASS: 3 checkpoint document regressions |
| `cargo test -p hvc-core persistence_service::serial::tests --lib --locked --offline` with `RUST_TEST_THREADS=1` | PASS: 28 checkpoint/acceptance/publication tests |
| `cargo fmt --all -- --check` | PASS |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS |
| `cargo build -p hvc-app --release --locked --offline` | PASS: final optimized build, 3m 35s |
| `scripts/smoke-windows.ps1` | PASS: isolated data directory, window created, graceful shutdown, exit code 0 |

GPUI rendering/input regressions verify group defaults and navigation retention, list/detail selection stability, long Unicode lines, IME geometry, fractional scroll, divider capture/release/reset, resize bounds, and complete document copy independent of Workspace. Combined review/Workspace geometry is checked at 960×640 and 1440×900; decision buttons remain inside the footer and the document reader retains at least 100 logical pixels of height. Core fixtures verify full long documents, pathless artifacts, exclusion of another generation, stale/cross-pulsation/resolved checkpoints, and missing legacy content.

Initial validation uses the shared working tree, which already contains unrelated changes. Those changes are preserved. Validation performs no real provider call or production checkpoint decision. Automated GPUI tests are not interactive Windows visual inspection or physical HiDPI/multi-monitor certification. Local commits were subsequently requested; no push was requested or performed.

## Release artifact and commit isolation

The user subsequently requested commits. Task-specific changes are extracted into `target/pulsations-commit-validation` from HEAD; unrelated pre-existing changes remain outside the commit. Additional candidate validation is recorded below before committing. The executable was built and smoke-tested from the shared working tree, including its pre-existing changes.

- Executable: `C:\Dev\Hard-Vibe-Code\target\release\hard-vibe-code.exe`
- Size: 24,269,312 bytes.
- SHA-256: `cc8c85ea56245889205a0a9cb1f5d3e187b2f74085486b08fc5fffba19719cf4`.
- Native smoke database: `target/smoke-data/57325b955d394b5dbfbf8418b09a214f/hard-vibe-code.db`.
- Isolated commit candidate: 112 UI tests, 15 editor tests, and 28 checkpoint/acceptance/publication tests pass with `RUST_TEST_THREADS=1`; workspace formatting and strict all-targets Clippy also pass. These checks run against HEAD plus only this task's changes. Existing feedback rendering outside the redesigned reader is retained in that candidate; later local notification changes stay outside the commit.

## Traceability

- `crates/hvc-ui/src/pulsations.rs`, `pulsations_view.rs`: display order, groups, review layout.
- `crates/hvc-ui/src/hitl.rs`, `review_view.rs`, `app_model.rs`: checkpoint-scoped loading, reader state, and decisions.
- `crates/hvc-core/src/orchestration/serial.rs`, `serial_tests.rs`, `persistence_service.rs`: typed read API and exact producer validation.
- `crates/hvc-ui/src/split_pane.rs`, `workspace_view.rs`, `native_editor.rs`, `layout_tests.rs`: column layout, viewport bounds, input behavior, and rendering regressions.
- `crates/hvc-editor/src/markdown.rs`: complete table parsing.
- Cerebro design references: `Arquitetura-UI-Tabs-e-Layout-Interno`, `Arquitetura-Rust-Core-e-Modulos`, ADR-015, ADR-017, and the task-specific ADR-019.
