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
the complete Stage 7 gate on demand and on pushes to `main` that change
`Cargo.lock`, `third_party/` or `scripts/`; it has no daily schedule because a run
costs about two hours of Windows minutes. [`security.yml`](../.github/workflows/security.yml)
runs `cargo audit` and the license and source audit, including a weekly run. The
jobs call the same aliases as local runs, with no retries.

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

## Tool policy, risk, pre-flight and documentation drift

ADR-034 tests sit next to their rules: the scope table in `hvc-contracts`
`contracts`, the declared policies in `composition::policy`, the CLI refusal
in `hvc-terminal` `terminals::tests`; risk and coverage in
`composition::risk`, `review_boundary`, `review_policy` and
`hvc-test-support/tests/composition_review.rs`; the Forge pre-flight in
`feature_forge::preflight` and the Forge tests (`persistence_service::forge_batches`);
documentation reconciliation in `doc_reconciliation` (links, contract and an
integration suite with a scripted gateway); V52, V53 and V54 in
`serial_upgrade`; the screens in the `forge`, `doc_drift`, `hitl` and layout
tests:

```powershell
cargo test --locked -p hvc-core --lib doc_reconciliation feature_forge::preflight forge_batches -- --test-threads=2
cargo test --locked -p hvc-test-support --test serial_upgrade -- --test-threads=2
```

`documentation_reconciler_runs_companion_scoped_under_the_reconciliation_origin`
in `cli_executor`, like its Notes Scribe twin, needs the Windows `.exe` CLI
fixtures and fails on Linux before the run starts.

## Execution mandates

ADR-035 M1 keeps its pure rules in `orchestration::mandate` (derivation,
canonical digests, the write, transport, executor and check checks, and a
generated-envelope property test) and its host transactions in
`persistence_service::mandate_persistence` (acceptance and publication refusals,
receipts, the preview, the transport amendment, the frozen budget). The full v4
pipeline runs in `hvc-test-support/tests/execution_mandate.rs`; V55 is in
`serial_upgrade`; the grants card in the layout tests. The receipts of the CLI
fallback, clarification and path-confirmation decisions are asserted in those
flows' own tests, and the project-deletion test covers the V55 rows:

```powershell
cargo test --locked -p hvc-core --lib mandate -- --test-threads=2
cargo test --locked -p hvc-test-support --test execution_mandate -- --test-threads=2
```

## Trust layers

ADR-035 M2 keeps the state rules in `orchestration::layers` (one table test per
trigger and threshold edge, legacy and new pulsations, order independence, the
50-item cut) and the reader in `persistence_service::layers_persistence` (a
composed pulsation's mandate, writes and refusals, a tampered mandate, the
detail read under a concurrent writer). `hvc-test-support/tests/trust_layers.rs`
follows a v4 pulsation from the Specification to the end and provokes a model
other than the one requested, a spent repair budget and a failed required
check. The panel, the brief card and the header dot are in the layout tests,
and the report's golden file carries the Layers section. The cost probe builds
a pulsation at the batch limits and times reading and deriving the layers:

```powershell
cargo test --locked -p hvc-core --lib layers -- --test-threads=2
cargo test --locked -p hvc-test-support --test trust_layers -- --test-threads=2
cargo run --locked --release -p hvc-core --example layers_benchmark
```

## Editor revisions, check output and voice setup

The workspace revision is a SHA-256 over the resolved path and the bytes read,
so a same-size, same-mtime external edit is a conflict. `workspace_service`
tests cover that edit, another file or root with identical content, a change
while the temporary file is written, a concurrent creation and a deleted file.
The runner fixture `exit-leaving-child` exits while a child keeps its output
open; the receipt must end within the drain bounds and never pass. Quota HTTP
and voice setup tests use loopback servers that never answer or stall the body,
plus a runtime check cancelled mid-run:

```powershell
cargo test --locked -p hvc-core --lib workspace_service::tests -- --test-threads=2
cargo test --locked -p hvc-test-support --test composition_validation -- --test-threads=2
cargo test --locked -p hvc-terminal --lib quota::http -- --test-threads=2
cargo test-voice
```

On Linux the process tree is not contained, so the timeout test that checks
the grandchild stopped fails there by design; it no longer hangs the run.

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
