# Rust contracts and terminal boundary — implementation report

Date: 2026-10-23. Baseline: `325ddbdd392be36188fac1ffec549235ea340a1c`;
implementation in the local working tree. [Source fingerprints](rust-boundaries-sources-2026-10-23.json)
identify changed code and configuration.

The selected [Google Rust Style Guide](https://google.github.io/styleguide/rustguide.html)
ideas are adapted to this Cargo/GPUI application: explicit ownership boundaries,
error handling, resource guards, imports, documentation and unsafe contracts.
The existing advisory and distribution blockers remain outside this refactor.

## Delivered

- [hvc-contracts](../crates/hvc-contracts/src/lib.rs) owns shared stage/role
  vocabulary, execution targets, response formats, dispatch attribution,
  timeout policies, usage values and redaction results. Persisted identifiers
  and wire formats retain their existing semantics.
- [hvc-terminal](../crates/hvc-terminal/src/lib.rs) owns CLI services and SQL
  repository, adapters, discovery, model catalog, process isolation, watchdogs,
  stream/usage tracking, recovery and quota. The dependency direction is
  `core -> terminal -> contracts`, with a separate `core -> contracts` edge.
  The terminal package has no core, UI or test-support dependency, including
  development dependencies.
- The [core host adapter](../crates/hvc-core/src/terminals_host.rs) retains
  sanitization authority, workspace selection, authorization receipts and the
  global usage ledger. It validates the concrete prepared-input proof against
  the prompts and returns the validated concrete value. Caller-provided getters
  cannot grant authorization. Core facades preserve existing application APIs;
  migrations remain owned by core.
- All seven workspace packages inherit the four workspace lints for unwrap
  usage, wildcard imports, documentation Markdown and undocumented unsafe
  blocks. Test harness allowances explain their scope. Changed production call sites return external
  failures with context; invariant `expect` calls state their assumptions.
  RAII data-directory and process-job guards carry `must_use`, and Windows
  unsafe operations document ownership, lifetimes and buffer contracts.
- The [chat runtime fallback](../crates/hvc-core/src/chat_service.rs) handles
  thread/runtime creation failure through normal turn finalization: persisted
  failure or cancellation, terminal events, active-turn cleanup and waiter
  notification.
- [Cargo graph enforcement](../scripts/check-crate-boundaries.ps1), test
  aliases, feature forwarding, CI and Docker configuration include the new
  packages. Default and disabled credential-collector graphs have distinct
  checks. [ADR-030](../docs/adr/030-terminal-boundary.md) records the boundary
  and its authority model.

## Coverage inventory

Source comparison confirms **all 123 original test functions are retained
exactly once**: 84 terminal, 12 recovery, 11 catalog and 16 quota tests. Their
final homes are 116 tests in terminal, five host tests in core and two public
integration tests in test-support. This is a transfer inventory, not the total
workspace test count.

There are **19 additional test functions**:

| Area | Added | Behavior covered |
|---|---:|---|
| [Contracts](../crates/hvc-contracts/tests/contracts.rs) | 10 | Stage aliases/order/checkpoints, every role identifier, target serialization, response schemas, dispatch attribution, timeout budgets and unknown usage values |
| [Core terminal host](../crates/hvc-core/src/terminals_host_tests.rs) | 6 | Forged proofs, changed prompts, spoofed getters, occurrence-scoped authorization, fixture/schema parity and production redaction categories |
| [Collectors disabled](../crates/hvc-terminal/src/quota/meter_tests.rs) | 1 | Local sources remain available without credentials, endpoint collectors or first-use disclosure |
| [Chat fallback](../crates/hvc-core/src/chat_service.rs) | 1 | Runtime startup failure finalizes pending turns and releases waiters, including cancellation |
| [Public host integration](../crates/hvc-test-support/tests/terminal_host_integration.rs) | 1 | Stage dispatch retains core preflight blockers after a login-required recheck |

Two [data-directory guard doctests](../crates/hvc-core/src/app_core.rs) cover a
retained guard (`no_run`, compile only) and rejection of a discarded guard under
`deny(unused_must_use)` (`compile_fail`). Existing manual/network/live-provider
tests retain their explicit ignore conditions; ignored tests are not passes.

## Executed validation

**1,421 passed, zero failed and 13 ignored**, covering all 45 default workspace
targets. The [machine-readable inventory](rust-boundaries-tests-2026-10-23.json)
checks target coverage, duplicates, per-crate/per-kind counts and ignored names.
The three completed selections were:

```powershell
cargo test --locked --offline --workspace --exclude hvc-core -- --test-threads=2
cargo test --locked --offline -p hvc-core --lib -- --test-threads=2
cargo test --locked --offline -p hvc-core --doc
```

This is aggregate workspace validation across disjoint selections. Earlier
interrupted `cargo test-all` attempts are not reported as successful invocations.

| Crate | Passed | Ignored |
|---|---:|---:|
| hvc-app | 0 | 0 |
| hvc-contracts | 10 | 0 |
| hvc-core | 743 (741 unit + 2 doctests) | 1 |
| hvc-editor | 17 | 0 |
| hvc-terminal | 197 | 3 |
| hvc-test-support | 134 | 9 |
| hvc-ui | 320 | 0 |

By runner: 1,297 unit/binary tests, 119 integration tests, three example tests
and two doctests passed. Ignored authenticated CLI/endpoint tests were not run.
Native tests use an explicit fixture host; real redaction authority and migrated
schema checks live in core and the public integration harness. The stream
coalescing test now checks ordered content and logical boundaries across valid
clock-driven chunks, while still rejecting per-frame storage and private partial
JSON exposure. Production batching logic matches the original.

| Additional check | Result |
|---|---|
| Isolated contracts + terminal, collectors disabled | 10 + 181 passed; zero failures, one ignored |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --workspace --all-targets --locked --offline -j4 -- -D warnings` | Passed |
| Crate-boundary graph guard | Passed |
| `cargo check --locked --offline -p hvc-app --no-default-features --bins -j4` | Passed |
| Windows release build in an isolated target | Passed |
| Isolated Windows smoke | Window and database created; graceful shutdown; exit 0 |

The default release output was locked by an application instance already open
before this implementation. Cargo's attempt to replace that file returned 101.
The successful release command reused a copied local cache in a separate target:

```powershell
cargo build --locked --offline --release -p hvc-app -j4 --target-dir target/release-check
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-windows.ps1 -BinaryPath target/release-check/release/hard-vibe-code.exe
```

The tested artifact is `<repo>/target/release-check/release/hard-vibe-code.exe`.
The smoke used a temporary profile and removed its database after shutdown.
[Command evidence](rust-boundaries-validation-2026-10-23.json) records both the
blocked default-copy attempt and the successful isolated build and smoke.

## Existing blockers and platform limits

The advisory comparison used the same RustSec database revision
`84d943a6deeb8cc6c6f632531aaf4f1c62275239` for the original and current lockfiles.
The [validation JSON](rust-boundaries-validation-2026-10-23.json) records **nine
vulnerabilities in each, with identical advisory ID, package and version tuples**
(`unchanged: true`); both audit commands returned exit code 1. They comprise three
`quinn-proto` advisories, four `rustls` advisories, one `rustls-webpki` advisory and
one `webpki-roots` advisory. All 776 external package versions and checksums are
unchanged. The only new local packages are contracts and terminal.

The supply-chain report records `technicalPassed: true` for the Windows graph,
with no denied licenses, missing license metadata or source-review entries.
The existing `zed-55470` legal blocker remains `distribution_blocking` for
binaries, so `distributionAllowed` remains false under the approved
[distribution policy](distribution-policy.json).

`docker compose config --quiet` passed. Linux container execution was unavailable
because the Docker Linux engine pipe was absent and the stopped Docker service
could not be started with the available OS permissions. Container tests and
Docker image execution are therefore **not verified in this local run**.

## Build measurements

Both frozen sources used Rust 1.98.0 on Windows x64 (12 logical CPUs), separate
empty Cargo targets, `CARGO_INCREMENTAL=1` and the same command:

```powershell
cargo check --locked --offline -p hvc-ui --lib -j4 --timings
```

The selection includes the UI and its real dependency graph. Before sources
come from the archived baseline revision; after sources match all 180 changed
code/configuration fingerprints. All 44 checks completed with exit 0. Frozen
sources were restored byte for byte and the validated workspace sources remained
unchanged.

| Measurement | Before (s) | After (s) | Observed change |
|---|---:|---:|---:|
| Cold check (one sample) | 138.615 | 117.669 | -15.1% |
| Warm check (median of three) | 1.261 | 1.242 | -1.5% |
| Terminal edit (median of three) | 7.839 | 8.344 | +6.4% |
| Core edit (median of three) | 7.627 | 7.088 | -7.1% |
| UI edit (median of three) | 2.696 | 2.711 | +0.5% |

Smaller elapsed time is faster; a negative percentage means a shorter check.
Cold has one observation per version. Warm and incremental rows report medians
of three paired repetitions; they do not establish statistical significance.

The terminal probe changes `CLI_OUTPUT_CEILING_FACTOR` from 2 to 3 in the old
core module or new terminal package. The core probe changes
`PREVIOUS_ATTEMPT_MAX_CHARS` from 60,000 to 60,001; the UI probe changes
`CANVAS_RGB` from `0x06080b` to `0x06080c`. Each probe runs only in its frozen
snapshot, restores original bytes and checks the restored baseline before the
next repetition. Reset durations are recorded and excluded from the table.

Registry, OS file caches and previous incremental states were retained. These
fixed synthetic edits give a local comparison; other edits, hardware, CPU
frequency and background work can produce different timings. The separation
establishes module ownership and focused test selection. The measurements do
not promise faster checks for every change or measure application runtime.

[Benchmark evidence](rust-boundaries-benchmark-2026-10-23.json) includes all
samples, reset checks, flags, source-manifest hashes and hashes of raw logs and
timing HTML. Those raw artifacts remain under `<repo>/target/rust-guide-benchmarks/`.

## Remaining qualification

The contracts/terminal extraction, retained host authorization, lint enforcement,
guard checks, feature forwarding and test transfer are complete. Follow-up work
is to qualify the Docker/Linux execution when its engine is available, remediate
the nine existing RustSec findings, and resolve the approved policy's binary
distribution blocker. Authenticated provider/endpoint probes remain explicit
opt-in checks. No binary publication is certified by this report.
