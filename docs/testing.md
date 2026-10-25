# Testing and validation

Run commands from the workspace root. `rust-toolchain.toml` pins Rust 1.98.0.
The desktop application and GPUI validation target Windows x64.

## Local commands

The aliases in [.cargo/config.toml](../.cargo/config.toml) use `--locked` and
two test threads per binary:

| Command | Selection |
|---|---|
| `cargo test-contracts` | Shared contracts and serialization compatibility |
| `cargo test-terminal` | Shared contracts and native terminal subsystem |
| `cargo test-core` | Shared contracts, terminal and core host integration |
| `cargo test-ui` | UI |
| `cargo test-editor` | Editor and consuming UI |
| `cargo test-support` | Harnesses, integration tests and consuming UI |
| `cargo test-all` | Complete workspace, including test-enabled examples and doctests |
| `cargo test-unit` | Workspace `lib` and `bin` tests (CI `unit` job) |
| `cargo test-integration` | `tests/` targets and the `pulsation_ui_fixture` example (CI `integration` job) |
| `cargo test-headless` | Crates without GPUI, run by the Docker harness |

`test-unit`, `test-integration` and `cargo test --locked --workspace --doc`
together select the same tests as `test-all`.

Use the relevant area during development. For a complete delivery, follow the
repository's checks:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test-all
```

`cargo test` without workspace/package selection uses only `default-members`
(`hvc-app`). Testing a package does not execute its dependencies' tests.
Confirm that a filter selects real tests before using it:

```powershell
cargo test --locked -p hvc-core --lib usage:: -- --list
cargo test --locked -p hvc-core --lib usage:: -- --test-threads=2
```

Aliases already contain `--`; use `cargo test-ui --list` without adding a second
separator. Keep each checkout's own `target/`. Compare timings only with the
same selection, toolchain, cache conditions and concurrency.

## Isolation

Use temporary directories, ephemeral SQLite databases, isolated keyrings and
controlled loopback providers. Opt-in tests requiring authenticated CLIs,
external services or paid quota retain their documented manual prerequisites.
Do not run ignored tests merely to increase the passing count.

For manual application checks, use an isolated profile:

```powershell
$env:HVC_DATA_DIR = "$PWD\target\dev-data"
cargo run -p hvc-app
```

## Complete gate and current evidence

```powershell
powershell -ExecutionPolicy Bypass -File scripts/gate-stage7.ps1
```

The gate includes structure, formatting, strict Clippy, workspace tests,
release build, Windows smoke and dependency audits. Skipping an audit does not
certify a complete passing gate.

The [2026-10-23 boundary validation](../quality/rust-boundaries-validation-2026-10-23.md)
records 1,421 passing tests, zero failures and 13 ignored tests across all 45
default workspace targets. Execution used three disjoint selections: the
workspace excluding core, core library tests and core doctests. The isolated
collectors-disabled selection additionally passed ten contract tests and 181
terminal tests, with one ignored test.

Strict workspace/all-target Clippy, formatting, crate boundaries and the
public app build without collectors passed. Nine RustSec findings remain
identical to the original lockfile under the same advisory database; binary
distribution policy remains blocking. See the dated report for release/smoke,
build measurements and the unverified local Docker execution. The
[2026-10-17 integration report](../quality/main-merge-validation-2026-10-17.md)
retains the earlier baseline and its then-current advisory count.

## Linux and Docker

The [Docker Compose services](../docker-compose.yml) run headless
contracts/terminal/core/editor/test-support tools. They are one-shot and behind profiles, so
`docker compose up` starts nothing:

```powershell
docker compose --profile tools build
docker compose --profile tools run --rm test-runner
docker compose --profile tools run --rm cargo-check
docker compose --profile tools run --rm clippy-check
docker compose --profile tools run --rm fmt-check
```

`test-runner` runs `cargo test-headless`. Linux validation has known gaps and
does not certify the Windows desktop. See the [quality index](../quality/README.md)
for evidence and the meaning of historical design references.

## Continuous integration

[`ci.yml`](../.github/workflows/ci.yml) runs on every pull request and push to
`main`: `fmt`, then `clippy`, `unit` and `integration` on `windows-latest`, plus
the Docker job `linux headless (docker)`, which stays informational until the
Linux failures are fixed. [`nightly.yml`](../.github/workflows/nightly.yml) runs
the complete Stage 7 gate, and [`security.yml`](../.github/workflows/security.yml)
runs `cargo audit` and the license and source audit. The jobs call the same
aliases as local runs, with no retries.

## Secret sanitization qualification

[Secret sanitization](secret-sanitization.md) describes the source/projection and
publication contracts. Relevant tests are in `secret_redactor`, `sanitization`,
CLI/Companion transport fixtures, composition and the review UI. The measured
scanner probe is hermetic:

```powershell
cargo run --locked -p hvc-core --example secret_sanitization_benchmark
```

The probe reports its build profile, sizes and timings. Whole-process peak memory
is measured separately; scanner timing is not a claim about provider latency or
the complete desktop application's memory.

## Integrator review boundary

[Review policy](review-policy.md) describes the reviewer's classified inputs,
evidence gaps, `REVIEW.md` and revision rounds. Pure rules are tested next to
`review_boundary`, `review_policy`, `line_diff`, `settings` and the recipes;
publication protection and base capture in `serial_tests`; the full pipeline in
`hvc-test-support/tests/composition_review.rs`, which also runs the reviewer
through an official CLI fixture and a Custom adapter; V51 in `serial_upgrade`;
the Review tab in the `hitl` and layout tests. Every run is hermetic:

```powershell
cargo test --locked -p hvc-test-support --test composition_review -- --test-threads=2
```

## Terminal extraction and feature boundaries

Run `scripts/check-crate-boundaries.ps1` to inspect all Cargo edges. Pure
terminal tests do not depend on core or test-support. Integration tests retain
the real host policies and migrations.

The integration harness enables credential collectors. Check the disabled
production graph independently of development targets:

```powershell
cargo check --locked -p hvc-app --no-default-features --bins
cargo tree -p hvc-app --no-default-features -e normal,build,features
cargo test --locked -p hvc-terminal --lib --no-default-features -- --test-threads=2
```

Check the terminal-to-zeroize optional edge, not global absence of zeroize:
other libraries can use zeroize independently. The full workspace tests exercise
the default collector configuration. See [ADR-031](adr/031-terminal-boundary.md).
