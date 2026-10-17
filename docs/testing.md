# Testing and validation

Run commands from the workspace root. `rust-toolchain.toml` pins Rust 1.98.0.
The desktop application and GPUI validation target Windows x64.

## Local commands

The aliases in [.cargo/config.toml](../.cargo/config.toml) use `--locked` and
two test threads per binary:

| Command | Selection |
|---|---|
| `cargo test-core` | Core |
| `cargo test-ui` | UI |
| `cargo test-editor` | Editor and consuming UI |
| `cargo test-support` | Harnesses, integration tests and consuming UI |
| `cargo test-all` | Complete workspace, including test-enabled examples and doctests |

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

The [2026-10-17 integration report](../quality/main-merge-validation-2026-10-17.md)
records 1,228 passing tests and ten existing ignored tests. The complete gate
failed on seven RustSec findings; distribution policy also remains blocking.
Those are dated results, not validation of later changes.

## Linux and Docker

The existing [Docker Compose services](../docker-compose.yml) run headless
core/editor/test-support tools:

```powershell
docker compose --profile tools build
docker compose --profile tools run --rm test-runner
docker compose --profile tools run --rm clippy-check
docker compose --profile tools run --rm fmt-check
```

Linux validation has known gaps and does not certify the Windows desktop.
GitHub Actions workflows are still planned at this baseline; the repository
does not yet contain `.github/workflows/`. See the [quality index](../quality/README.md)
for evidence and the meaning of historical design references.
