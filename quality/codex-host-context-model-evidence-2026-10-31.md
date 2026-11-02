# Codex automatic model verification — 2026-10-31

## Failure and correction

A read-only inspection of the local Feature Agent run on 2026-10-31 found
`model_unverified`, `messageId=item_0`, an explicit selected model
`gpt-5.6-luna`, Codex CLI 0.163.0 and no matching persisted session rollout.
The CLI was signed in. Notes Scribe runs had the same failure.

Automatic host-context-only calls reused `Hardening::Companion`, which added
`--ephemeral`. This suppressed the rollout required by the existing model
verification path for public Codex items that omit `model`. The
[official non-interactive documentation](https://learn.chatgpt.com/docs/non-interactive-mode)
confirms that flag disables session rollout persistence and shows the public
model-less item shape.

`Hardening::HostContextOnly` now preserves the same tool/configuration
restrictions and temporary workspace while retaining Codex's rollout. Actual
Companion rounds remain ephemeral; Pipeline retains its read tools. Custom
remains refused for host-context-only calls. No selected model, provider,
credential, production database or model-verification rule was changed.

## Coverage

- Unit/boundary: actual Companion versus HostContextOnly across every official
  backend; Codex rollout retention, Gemini empty tools and Custom rejection.
- Integration: Feature discovery, Notes Scribe and Documentation Reconciler
  fixtures reject `--ephemeral`; Companion fixtures require it.
- Existing model-less item audits continue to cover current session/turn
  binding, persisted `codex_turn_context` evidence and model mismatch rejection.
- Opt-in native smoke: pipeline and the three automatic roles send synthetic
  identity prompts and assert selected/observed model equality plus
  `evidence_source=codex_turn_context` in isolated SQLite.

## Execution

All checks below passed on Windows. The complete default suite ran with access
to local fixture processes and loopback services; authenticated probes used
isolated databases and synthetic prompts.

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed |
| `scripts/check-crate-boundaries.ps1` | Passed |
| `cargo clippy --workspace --all-targets --locked -j 2 -- -D warnings` | Passed; environment-only incremental-cache hard-link warnings were emitted |
| `cargo test-all` | 2,002 passed, zero failed, 23 opt-in tests ignored; includes unit, integration, boundary and doctests |
| `cargo test --locked -p hvc-test-support --test cli_executor authenticated_codex_turn_context_model_smoke -- --ignored --exact --nocapture` | Passed; all four real Codex probes verified the requested model and `codex_turn_context` evidence, in 25.59 s |
| `cargo build --locked -p hvc-app -j 2` | Passed; updated `target/debug/hard-vibe-code.exe` |
| `scripts/smoke-windows.ps1 -BinaryPath target/debug/hard-vibe-code.exe` | Passed; window and isolated database detected, WM_CLOSE completed within five seconds; script reported `gracefulShutdown=true`, process exit code `-1073741510` |

Logs are under `target/codex-model-evidence-*-2026-10-31.log`. The debug
executable's SHA-256 is
`a9f4e9734be2acd6e5b63cac8b758a1d96d585167336230760dd7973f0db031f`.
The release executable and distribution/audit gate were not rebuilt or qualified.

The checkout also contains a pre-existing unrelated `candidate_capture.rs`
comment change, preserved throughout this task and excluded from the correction
commit. No temporary worktree was created. Integration and push remain subject
to the maintainer's branch-completion approval.
