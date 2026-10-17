# Main branch integration — 2026-10-17

## Scope and provenance

This integration joins `main` at `33373a560026c8cda296b3d987efdcef51416bbc`
with `feat/adr-024-review-brief` at
`dccd257140ea4dd976bd5e0bfd2a8b7880196b92`, preserving both histories.
The feature includes the focused test workflow, Review Brief M0–M6,
Visualizer, and the pulsation report (canonical ADR-027).

The initial tips of `chore/test-workflow` and
`integration/review-brief-m2-base` are already ancestors of the feature tip.
They require no separate cherry-picks.
The former `main` is retained as `backup/main-before-merge-2026-10-17`;
older backups and existing worktrees are retained.

## Conflict resolution

| File | Resolution |
|---|---|
| `feature_forge/tests.rs` | Keep the request capture and regression for progressive CLI budgets from main; apply current rustfmt. |
| `terminals/adapters/claude.rs` | Retain main's synthetic API error regressions and the feature's other changes. |
| `tests/cli_executor.rs` | Retain the isolated CLI discovery fixtures and synthetic authentication regression from main. |
| `forge_view.rs` | Retain selectable, copyable host notices from main; apply current rustfmt. |

The automatically merged paths were also reviewed for preservation of Claude
error classification, generation attestation, Companion errors, Antigravity
restrictions, Forge budgets, and selectable Companion/Forge messages.
Formatting covers the eight Rust files requiring normalization after the merge.
`Cargo.toml`, `Cargo.lock`, and `rust-toolchain.toml` are identical between
the two parents; this integration changes no dependency versions.

## Validation

The executable source candidate before adding this report is Git tree
`1e0fd323341572197166eea30550604d87aae136`.
Validation uses Windows, Rust/Cargo 1.98.0, this checkout's own `target/`,
and the existing Stage 7 gate without skipped stages.

The first, sandboxed gate stopped during test linking with MSVC LNK1114
(access denied while replacing a generated UI import library). It did not
execute the tests. The native rerun uses two build jobs and the existing
two test threads per binary; the executable source is unchanged.

- GPUI/Rust structure: passed.
- `cargo fmt --all -- --check`: passed after formatting.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test-all`: 1,228 passed, zero failed, 10 existing opt-in cases ignored,
  zero filtered. Core: 783 passed / 1 ignored; UI: 302 passed.
- Release build: passed (853.890 s, initially empty release target).
- Windows smoke: passed (isolated database, window found, graceful shutdown,
  exit 0).
- Rust licenses and sources: technical check passed. The existing distribution
  policy remains blocking (`distributionAllowed: false`).
- RustSec audit: failed, exit 1, with the same seven previously recorded
  vulnerabilities in `quinn-proto 0.11.17` and `rustls 0.23.43`.

The complete native gate took 1,721.987 s and remains **failed** because of
the dependency audit. Its seven other stages passed. The fresh RustSec
database revision is `64f92cdc64401fab47baa10e2e11fe382aa57155`.
No advisory was suppressed and no dependency upgrade was included in this merge.
The machine-readable snapshot is
[`main-merge-validation-2026-10-17.json`](main-merge-validation-2026-10-17.json).

Raw logs and timings are kept locally under
`target/merge-main-2026-10-17/`. The existing `cargo-audit 0.22.2` binary
from the test-workflow checkout is reused under `target/tools/`.

## Preserved local work

The pre-existing, untracked `CONTRIBUTING.md` remains outside this merge.
The sibling Cerebro checkout's untracked Kanban and open-source planning
documents are also preserved. Their SHA-256 values are recorded locally in
`target/merge-main-2026-10-17/preserved-untracked-files.json`.

The corresponding documentation integration is recorded in Cerebro at
`Documentos/04-Qualidade-e-Testes/Main-Branch-Integration-Validation-2026-10-17.md`.
