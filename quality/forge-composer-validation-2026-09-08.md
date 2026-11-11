# Forge Incubation composer and header validation

Design references: [public summaries](../docs/adr/README.md); [historical archive context](README.md#historical-design-references).

Date: 2026-09-08

## Scope

- Incubation reuses the Companion multiline `NativeTextInput`: Enter sends, Shift+Enter inserts a newline, text wraps, the input grows to six visible rows and then scrolls internally.
- `ForgeUi` owns the Submit subscription. Both Enter and the button use `ForgeAction::Send`; project reset replaces the input and its listener together.
- Admission rejects whitespace-only input, busy operations, a running turn for the selected feature, archived features and project mismatch. Existing core admission/revision checks remain authoritative.
- A successful admission carries its original feature id and draft through the asynchronous result. Clearing requires the same current selection and text. Failed admission preserves the draft.
- The header HV square and Hard Vibe Code label are removed with their reserved layout space. Native application title/icon and navigation controls remain unchanged.
- Title/capture input modes, core APIs, database schema and greenfield context collection/prompt behavior are unchanged by this patch.

## Validation boundary

The shared working directory contained concurrent, unrelated changes to the core, UI translation, runtime and documentation. Its first UI run passed 101 tests. Workspace-wide formatting and strict Clippy then reported unrelated changes (including `feature_forge/persistence.rs:64`, `needless_question_mark`). Those changes were preserved and excluded from this commit.

Release validation uses an isolated archive of code base `f804f99c3bd083024702aa4af3115b11dfbe3775` plus precisely the four UI file changes selected for this commit. The source snapshot is `target/forge-composer-validation`; build artifacts use the normal repository `target` directory. No real provider credentials or LLM calls are used by these tests.

## Automated evidence

| Check | Result |
| --- | --- |
| `cargo test -p hvc-ui --lib --locked --offline` with `RUST_TEST_THREADS=1` | PASS: 100 tests on the isolated commit candidate |
| `cargo fmt --all -- --check` | PASS on the isolated commit candidate |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | PASS on the isolated commit candidate |
| `cargo build --release --locked --offline` | PASS: optimized release, 4m 41s |

`forge_enter_and_button_share_admission_and_recreated_subscriptions` mounts the real Forge view with an isolated core. Shift+Enter only inserts a newline. Repeated Enter produces one admission, and the button reaches the same path. Deliberately stale revisions reject admission locally, preserve the draft and create no agent turn. Three ForgeUi reconstructions retain old input entities to verify their Submit events no longer act on the current model. The Companion draft remains unchanged.

`message_admission_and_acknowledgement_preserve_newer_drafts` covers empty/whitespace drafts, busy state, active response, archived feature, project mismatch and conditional clearing after acceptance. Failed turns allow another attempt. A changed draft or selection is not cleared by the older acknowledgement.

The existing shared component tests cover automatic wrapping, Unicode, six 24-pixel rows, scrolling, caret visibility, selection, resize, height reduction and IME suppression of Submit. Header/navigation geometry and Forge panels are checked at 960x640 and 1440x900. The first tab starts without a branding gap, and all tab/settings controls remain inside the header.

These are GPUI automated rendering/input tests. No new interactive Windows IME, accessibility, HiDPI or multi-monitor certification is claimed. The release executable is built from the isolated commit candidate, not from unrelated uncommitted changes in the shared working directory.

Cargo reports a dependency future-incompatibility notice for `proc-macro-error2 2.0.1`; strict Clippy and the release command nevertheless exit successfully.

## Release artifact

- Path: `C:\Dev\Hard-Vibe-Code\target\release\hard-vibe-code.exe`
- Size: 23983616 bytes
- SHA-256: `e96df55694352c7d2bcc921ac49a87683c33b2cc0d161d37ec526011a3612660`

## Greenfield diagnosis

For an actually empty repository, the reported absence of repository files, symbols and architectural sources is expected. It is compatible with the Feature Agent's existing greenfield guidance rather than a fixed application error. The idea, brief, recent messages and known features/tasks still provide planning context. This work deliberately leaves that collection and wording unchanged.

## Traceability

- `crates/hvc-ui/src/forge.rs`: multiline opt-in, subscription lifecycle, admission, draft acknowledgement and integration regressions.
- `crates/hvc-ui/src/forge_view.rs`: composer frame, shared send action and English keyboard hint.
- `crates/hvc-ui/src/shell.rs`: header branding removal.
- `crates/hvc-ui/src/layout_tests.rs`: header control geometry at minimum and wide sizes.
- Cerebro: `Arquitetura-UI-Tabs-e-Layout-Interno`, `Feature-Forge-Tasks-and-Batches-Implementation`, and `16-Forge-Composer-and-Header-Validation-2026-09-08`.
- Local command logs: `target/forge-composer-tests.log`, `target/forge-composer-clippy.log`, `target/forge-composer-release.log`.
