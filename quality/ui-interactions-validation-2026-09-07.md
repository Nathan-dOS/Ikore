# UI refresh, modal dismissal and Companion composer — 2026-09-07

## Findings and implementation

- Every Pulsations runtime snapshot previously called `refresh_forge` while Tasks or Feature Forge was visible. The 1.2-second poll set the same busy flag as mutations and inserted an `Atualizando` row into normal layout. Background refresh now retains rendered data, has its own in-flight state and never inserts that row.
- Forge notifications, explicit actions, view entry and broadcast-lag recovery request snapshots. Events during a pending read or mutation coalesce into one trailing read. Mutations stay serial, pending navigation retains the latest destination, and project/generation fences discard superseded results. An explicit mutation may supersede an old background read without waiting for it.
- `FeatureForgeService::view_snapshot` reads features, backlog, dependencies, navigation, evidence and selected conversation history in one SQLite read transaction, then the UI applies them together. A FeatureAgent completion cannot mix an older brief with a newer reply in this response.
- The existing Pulsations runtime poll remains. Its bridge into Forge now reacts only to newly observed Done/Failed transitions. This is event-based refresh with a targeted compatibility bridge, not a complete conversion of the pulsation runtime to events. A full conversion needs complete post-commit event coverage across the runtime state machine. Blocking synchronous database work on the GPUI thread would violate ADR-006, so reads remain on Tokio blocking workers.
- Capture and the shared model picker dismiss on a left click on the scrim. Dialog content stops propagation; the scrim consumes dismissal clicks so covered controls cannot be activated. Close and Escape remain supported. Dismissal preserves current input entities.
- The Companion opts into multiline `NativeTextInput`; form controls keep the existing single-line mode. Enter emits the same guarded send command as the button, Shift+Enter inserts a newline and active IME composition suppresses submit. Empty/invalid/unavailable/busy/streaming/disclosure restrictions remain in `send_chat_message`.
- Text wraps at the measured width, grows from one to six 24-pixel rows, then scrolls vertically within the field. Padding/borders add to the outer frame. Scroll position is visible through a thumb. Edits, vertical navigation and resize keep the cursor visible, and reducing text collapses the field again. Unicode hit testing and soft-wrap cursor affinity use the rendered row geometry. Paste normalizes CRLF/CR into newlines in multiline mode.

## Executed validation

| Check | Result |
| --- | --- |
| `$env:RUST_TEST_THREADS='1'; cargo test --workspace --locked --offline --no-fail-fast` | 510 passed, 0 failed, 1 opt-in authenticated test ignored |
| Included `hvc-ui` tests | 96 passed |
| Final Forge regressions after lint cleanup | 2 passed |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | Passed |
| `cargo fmt --all -- --check` | Passed |
| `cargo build --release -p hvc-app --locked --offline` | Pending final result |

The suite covers burst coalescing, command admission during reads, retained navigation,
unchanged runtime polls, terminal invalidation, project-scoped view snapshots, stable
task control geometry, modal content/outside clicks, click-through prevention,
Enter/Shift+Enter, duplicate-submit/streaming guards, Unicode, IME submit suppression,
long unbroken text, bounded height, scrolling, cursor navigation and resize. The
pre-existing single-line/masked-input regressions remain passing.

The first long-text regression exposed a soft-wrap cursor-affinity problem during
Up/Down navigation. Tracking the visual row and preferred column corrected it; the
regression and final workspace suite passed afterward.

## Scope of evidence

GPUI interaction tests execute against its visual test harness, including actual
keyboard/mouse dispatch and layout measurement. This run does not certify physical
Windows rendering, external provider behavior, accessibility, HiDPI, or multiple
monitors. The opt-in authenticated Codex smoke was not rerun for these UI changes.
The existing transitive `proc-macro-error2` future-incompatibility warning remains.

Existing unrelated worktree changes were retained. No commit or push was performed.

## Traceability

- `crates/hvc-ui/src/forge.rs`, `forge_view.rs`, `app_model.rs`: refresh ownership,
  lifecycle invalidation, capture and snapshot application.
- `crates/hvc-core/src/feature_forge/service.rs`, `runtime.rs`, `tests.rs`: coherent
  view snapshot and selected-history isolation.
- `crates/hvc-ui/src/agents_view.rs`, `layout_tests.rs`: modal dismissal and geometry.
- `crates/hvc-ui/src/text_input.rs`, `chat.rs`, `chat_view.rs`: composer mode,
  guarded submit subscription, text geometry and regressions.
- Cerebro: ADR-015, ADR-016, UI architecture, Forge implementation, Companion
  architecture/specification, UI engineering sequence and quality map link to
  `15-UI-Refresh-Modals-and-Composer-Validation-2026-09-07`.
