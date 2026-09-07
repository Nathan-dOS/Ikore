# Tabs UI implementation and validation

Implementation date: 2026-09-05
Validation completed: 2026-09-06
Design: ADR-015, in the separate Cerebro documentation repository.

## Delivered behavior

- Agent groups Agents and Logs; Engine groups Vault and Pulsations; Companion groups Chat and Workspace. Settings opens independently from the header gear.
- Each header tab remembers its last view in memory. Session restoration derives the header tab from the existing persisted `active_view`; a restored project starts fresh history for the other tabs. The legacy sidebar flag remains readable but has no rendered control.
- Agents renders a scrollable list and one selected detail in both CLI and API modes. Canonical, active FreeForm, and archived FreeForm controls remain reachable. Pending fallback counts appear in the list; decisions remain with the selected canonical agent.
- The shared Agents/Companion model picker uses an absolute `deferred` layer with mouse occlusion, search focus, Escape/Close dismissal, and a wrapping header. View or mode changes dismiss stale pickers.
- Pulsations keeps the header and pipeline outside the section scroll. Overview, Preparation, and Artifacts have separate content. Changing the selected pulse resets to Overview; ordinary refreshes retain the section. Pending preparation actions have an indicator. Artifact previews retain paging and full-content copy.
- Settings separates Connections, Routes, and Runtime. Creation actions stay reachable when no item exists; selected or explicitly new records expose their forms. Initial automatic selection hydrates the matching fields, while refresh of the same selection preserves draft text.
- Logs has collapsible routing metrics and events. Large routing groups start collapsed; the newest 50 events are shown first, with increments of 50 and the existing 200-event retention bound.
- Companion uses a compact CLI configuration grid and a toggle for API tool activity. CLI conversations do not expose the internal API tool panel.
- Vault provides independent graph scrolling and a document preview capped at 400 pixels.

## Regression evidence

- `layout_tests.rs`: rendered navigation at 960 x 640, last-view restoration after Settings, one selected CLI/API agent, modal geometry and click occlusion, Escape dismissal, creation-form reachability, and fixed Pulsations headers.
- `agents.rs`: selection survives snapshots and recovers from missing agents/slots.
- `app_model.rs`: all seven views synchronize navigation; restored sessions derive consistent header tabs.
- `settings.rs`: automatic selection fills connection/route fields and refresh preserves same-selection drafts.
- `logs.rs`: event retention is distinct from the incremental display limit.
- `agents_ui_fixture.rs`: the completed pulse can be read through the UI's runtime-snapshot API with valid coordination data and zero scheduled jobs. This catches a missing queue sequence in fixture data.
- Existing UI tests retain cancellation, HITL, disclosure, redaction, streaming, project isolation, and editor/session coverage.

GPUI 0.2.2 retains debug selectors in reused frames. Tests check absence in a fresh window and use the active window for geometry and pointer interactions; vendored GPUI was not changed.

## Validation commands

```powershell
$env:RUST_TEST_THREADS='1'
cargo test -p hvc-ui --locked --offline
cargo test -p hvc-test-support --example agents_ui_fixture --locked --offline
cargo fmt --all -- --check
cargo clippy -p hvc-ui -p hvc-app -p hvc-test-support --all-targets --locked --offline -- -D warnings
cargo build -p hvc-app --release --locked --offline
```

All commands above passed: 83 UI tests, one fixture test, formatting, strict Clippy, and the optimized release build. The final fixture correction was followed by another fixture test and Clippy run. Cargo reported an existing future-compatibility notice for `proc-macro-error2`; it did not fail these checks.

An earlier debug build passed. A later debug rebuild could not replace the executable because another task's process held it open. That process was preserved; final native inspection used `target/release/hard-vibe-code.exe` instead.

## Native fixture

```powershell
cargo run -p hvc-test-support --example agents_ui_fixture --locked --offline -- target/tabs-ui-qa-NEW
$env:HVC_DATA_DIR='C:\Dev\Hard-Vibe-Code\target\tabs-ui-qa-NEW\data'
.\target\release\hard-vibe-code.exe
```

The fixture requires a new directory. Manual fixture models no longer claim verified profiles. It includes active/archived FreeForm agents and a completed pulse with a pathless English artifact and a valid queue sequence; the pulse has zero jobs. No real workflow needs to run to inspect the sections.

Initial native inspection used a debug window configured to 1024 x 768 with `HVC_QA_WINDOW_SIZE`. Final inspection used the release at its default 1280 x 820 client size; that environment override is debug-only. The final corrected profile was `target/tabs-ui-qa-20260905d`.

Observed in the native application:

- Header tabs and Settings navigation worked; Agents displayed one selected detail in CLI and API modes.
- The model-picker title and Close button remained inside the overlay; Close dismissed it without shifting the detail. Escape dismissal was also exercised during the earlier native pass and in the final rendered regressions.
- Settings automatically populated connection and route fields. Category changes and Runtime scrolling kept the navigation controls accessible.
- All three Pulsations sections opened. The pathless artifact preview loaded, and scrolling reached paging, full-content copy, and close controls while the header and pipeline stayed fixed.
- Logs independently hid routing metrics and events. The fixture had no LLM run metrics/events; incremental event display is covered by automated regression.
- Companion showed the optional API activity panel, then removed both its toggle and panel in CLI mode. The CLI configuration occupied a compact two-column grid.
- Vault displayed the tree, graph, and document inspector with their separate regions.

No agent workflow or conversation was started. The isolated QA windows were closed after inspection. No schema, runtime policy, adapter compatibility gate, or production workspace was changed by the UI implementation.

## Scope of validation

This is a UI and fixture change. The historical full-workspace baseline is not a new result from this run. Multi-monitor/HiDPI and the complete accessibility/keyboard matrix remain separate physical QA work; the tested keyboard behavior is model-picker Escape dismissal.

Concurrent changes to core CLI discovery/model catalogs and application icons were preserved. They are not part of this UI delivery, although the release build includes the current workspace. No commit or push was performed.
