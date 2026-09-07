# Companion and text input correction — 2026-09-07

## Reproduced problems

- Codex CLI 0.153.4 rejected the screenshot's `gpt-5.2-codex` model for the current ChatGPT account. Its error and `turn.failed` JSONL events were on stdout with empty stderr. HVC discarded those events when constructing the error and showed only `Codex CLI exited with status Some(1)`.
- The Companion's native Codex output schema had a union at the root and an unconstrained argument object. These violate the [Structured Outputs contract](https://developers.openai.com/api/docs/guides/structured-outputs).
- `NativeTextInput` used a shrinking 36-pixel frame, a fixed text paint offset, no visible caret and no horizontal scroll. Long values continued to exist but their end was not visible. Mouse clicks always moved to the end.
- The Companion sidebar placed backend/model/profile controls in two narrow columns, causing labels and model names to overlap or wrap excessively.
- During validation, the user reported that OpenCode worked. This is user-observed evidence; the task's authenticated provider smoke specifically exercised Codex.

## Changes

- Decode only Codex error messages from known JSONL error events, including nested provider JSON. Bound and sanitize the decoded text and redact secrets again before returning it to Chat. Never render event JSON or reasoning. Explain how to recover from a provider model rejection or obsolete catalog selection by creating a new conversation.
- Give Codex a closed root `response` object with nested `anyOf` envelopes. Derive closed, fully required tool argument objects from the existing HVC schemas, representing optional arguments as nullable. Normalize only declared optional nulls back to omission before existing host validation. Keep the canonical host envelope, other CLI schemas, version gates, temporary workspace and conversation immutability unchanged.
- Refresh versioned model presets from the installed Codex metadata inspected on 2026-09-07, retaining `CLI default` first. Presets do not guarantee account entitlement. Update the catalog revision fixtures.
- Keep every shared single-line text input at 40 logical pixels with a centered 24-pixel line, prevent flex shrink, render caret/selection, scroll horizontally to follow editing/navigation/resize, and use the same viewport for Unicode/masked hit testing and IME bounds.
- Stack the three Companion CLI selection controls vertically.

## Validation

| Check | Result |
| --- | --- |
| Serial workspace: `cargo test --workspace --locked --offline` with `RUST_TEST_THREADS=1` | **503 passed**, zero failures, one opt-in authenticated test ignored |
| Actual Codex: `authenticated_codex_companion_smoke --ignored --nocapture --test-threads=1` | **Passed** in 73.79 seconds: CLI default final response, then a real HVC `list_repository_tree` round over synthetic `src/lib.rs` |
| Latest ChatService checks: `cargo test -p hvc-core --lib chat_service::tests --locked --offline` | **13 passed**, including obsolete-catalog remediation |
| Shared input regressions | **4 passed**, including long Unicode text, Home/End/arrows, deletion/replacement, narrow resize, flex pressure, and masked Unicode coordinates |
| Strict static analysis: `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | **Passed** |
| Formatting: `cargo fmt --all -- --check` | **Passed** |
| Whitespace: `git diff --check` | **Passed** |
| Debug: `cargo build -p hvc-app --locked --offline` | **Passed** |
| Release: `cargo build -p hvc-app --release --locked --offline` | **Passed** |
| Native UI, final debug build, isolated profile, 1280-by-820 client | **Passed**: long Unicode draft, end/caret visibility, Home back to start, selection, clearing the unsent test draft, and stacked CLI selectors |

The initial full-suite run found a stale catalog fixture revision and a JSON string assertion sensitive to property order. Both were corrected; the final complete workspace run passed. The full output is in `quality/companion-input-tests-2026-09-07.log`.

The provider smoke uses temporary databases and synthetic workspaces, asserts that HVC credentials are not consulted, and checks both final text and a host-owned tool round. It remains ignored by default because it requires the CLI's existing login and network access. No production conversation target, CLI authentication or Git history was changed by this task.

Native inspection initially failed to find a hidden test window. Launching the final debug build with the same isolated `HVC_DATA_DIR` exposed a targetable window. The draft was never sent, was cleared after the check, and the QA window was closed normally. The temporary profile remains under `target/companion-input-qa-20260907`; it was not deleted. Physical multi-monitor, additional DPI scales and broad accessibility certification remain outside this focused check.

One release attempt could not replace the executable while a QA instance was running. After it closed, the final release build succeeded. Final SHA-256 hashes:

- `target/debug/hard-vibe-code.exe`: `9C3AAE4AE02D10E20F29D08622C0CD8478C5C3CE79FFB27932E2182957888A64`
- `target/release/hard-vibe-code.exe`: `3E6857FD52B187505CA24BD43A3B1646638E3C6E61C1FDE92A35930D465C1BB2`

## Documentation trace

The Cerebro repository records this correction in ADR-014, the Companion architecture, the UI layout architecture, the quality map and `14-Companion-and-Text-Input-Validation-2026-09-07.md`.

## Use after rebuilding

Restart the rebuilt application. For the failed Codex conversation, choose `CLI default` or an available current model, then create a new conversation. The old conversation retains its original model and history. The reported working OpenCode transport remains available. Single-line inputs reveal overflowing text as the caret moves; they do not discard it.
