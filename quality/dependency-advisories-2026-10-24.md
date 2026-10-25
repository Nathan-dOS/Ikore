# Dependency advisories — lockfile remediation and validation

Date: 2026-10-24. Baseline: `main` at `e8f4644`; branch
`fix/dependency-advisories`. Machine-readable evidence:
[dependency-advisories-2026-10-24.json](dependency-advisories-2026-10-24.json).

Since 2026-10-15 every validation report recorded the same 9 RustSec
vulnerabilities, the only failure of the Stage 7 gate's advisory step (latest:
[review policy, 2026-10-24](review-policy-validation-2026-10-24.md)). This change
resolves all 9 with a `Cargo.lock`-only update. No `Cargo.toml`, vendored GPUI
source or application code changed.

## Change

Six registry packages move to the newest semver-compatible release. Each
`[[package]]` entry changes only its `version` and `checksum`; dependency lists
and every other entry are unchanged (12 changed lines).

| Package | Before | After | Advisories resolved | Patched range (RustSec) |
|---|---|---|---|---|
| `quinn-proto` | 0.11.17 | 0.11.19 | RUSTSEC-2026-0338, -0340, -0343 | `>= 0.11.18` |
| `rustls` | 0.23.43 | 0.23.46 | RUSTSEC-2026-0285, -0346, -0348, -0349 | `>= 0.23.45` (0285), `>= 0.23.46` (others) |
| `rustls-webpki` | 0.103.15 | 0.103.16 | RUSTSEC-2026-0382 | `>= 0.103.16, < 0.104.0-alpha.1` |
| `webpki-roots` | 1.0.9 | 1.0.11 | RUSTSEC-2026-0381 | `>= 1.0.10` |
| `tokio` | 1.53.1 | 1.54.1 | informational `unsound` RUSTSEC-2026-0386 | `>= 1.53.3` |
| `chacha20` | 0.10.1 (yanked) | 0.10.2 | informational `yanked` | — |

`tokio` and `chacha20` were not gate failures. They are included because a
semver-compatible `cargo update -p` reaches a release outside the warning.
RUSTSEC-2026-0386 affects the Unix-only `AsyncFd` constructors.

### Who pulls the vulnerable crates

- `rustls`, `rustls-webpki`, `webpki-roots`: `hyper-rustls 0.27.9`,
  `tokio-rustls 0.26.4` and `reqwest 0.12.28` (used by `hvc-core`,
  `hvc-terminal`, `hvc-voice` and `jsonschema 0.30.0`), and `zed-reqwest
  0.12.15-zed` via `gpui_http_client 0.2.2` → vendored `gpui 0.2.2`
  (`third_party/gpui`). All requirements are caret ranges that already allowed
  the patched releases, so no upstream pin blocked the fix.
- `quinn-proto`: locked as an optional dependency of `quinn 0.11.11`, required by
  `reqwest 0.12.28` and `zed-reqwest 0.12.15-zed` for HTTP/3.
  `cargo tree -i quinn-proto --target all` (also with `--all-features`) prints
  nothing: it is not compiled into any current target. `cargo audit` reads
  `Cargo.lock`, so it still failed the gate.

### Why the lockfile was not taken verbatim from `cargo update`

`cargo update -p quinn-proto -p rustls -p rustls-webpki -p webpki-roots -p tokio
-p chacha20` (cargo 1.98.0) reached the patched versions, but it also re-pointed
unrelated dependency edges that accept a version range. Examples: `windows-sys
0.61.2` to 0.48.0, 0.52.0, 0.59.0 or 0.60.2 under `errno`, `quinn-udp`,
`rustix`, `socket2`, `stacker`, `tempfile`, `uds_windows` and `winapi-util`, and
`getrandom 0.4.3` to 0.3.4 under `tempfile`. The choice was not reproducible:
two identical runs from the same baseline wrote different `windows-sys` edges.
`--precise` showed the same effect.

The committed lockfile is the baseline with only the six entries' `version` and
`checksum` taken from cargo's own resolution. Cargo accepts it unchanged:
`cargo metadata --locked` and `cargo tree --locked --target all -e
normal,build,dev` succeed without rewriting the file, as do the `--locked` build,
Clippy and test runs below. Any later `cargo update` may re-pick those range
edges; that is cargo behaviour, not part of this fix.

## Advisory audit

`cargo-audit` 0.22.2 (`target/tools/cargo-audit`) with RustSec database commit
`21d909e4c8bd4e14c7aea58bf980c881a0053ab1` (1,352 advisories, updated
2026-10-24T10:30:13+02:00). A second run that fetched the database again found
no newer commit.

| Lockfile | Exit | Vulnerabilities | Unmaintained | Unsound | Yanked |
|---|---:|---:|---:|---:|---:|
| `main` `e8f4644` | 1 | 9 | 14 | 3 | 1 |
| this branch | 0 | **0** | 14 | 2 | 0 |

### Remaining informational warnings (not gate failures)

None of these has a semver-compatible fix inside the current graph. All arrive
through upstream crates, mostly the GPUI/zed dependency tree. They are recorded,
not waived. The JSON lists each one with its lockfile dependents.

- `unsound`: RUSTSEC-2026-0183 and RUSTSEC-2026-0184 in `git2 0.20.4`, patched
  only in `>= 0.21.0` (semver-incompatible). It is locked solely as a dependency
  of `gpui_util 0.2.2` and is not in the active graph (`cargo tree -i git2`
  finds no match).
- `unmaintained` (14): `async-std` (`zed-async-tar`), `instant` (`fastrand
  1.9.0`), `paste` (`metal`, `pulp`, `rav1e`), `proc-macro-error2`
  (`stacksafe-macro`), `rustls-pemfile` (`zed-reqwest`), `rustybuzz` 0.14.1 and
  0.20.1 (`cosmic-text`, `usvg`), `ttf-parser` 0.20.0, 0.21.1 and 0.25.1
  (`cosmic-text`, `fontdb`, `rustybuzz`), `unsafe-libyaml` (`serde_yaml`),
  `zbus_names`, `zvariant` and `zvariant_derive` (`zbus`, `oo7`).

## Validation

Native Windows 11, pinned toolchain (cargo 1.98.0), fresh worktree target
directory, `CARGO_BUILD_JOBS=6`.

| Check | Result | Time |
|---|---|---|
| `cargo fmt --all -- --check` | PASS | 5 s |
| `cargo clippy --workspace --all-targets --locked -j 6 -- -D warnings` | PASS | 144 s |
| `cargo test --locked --workspace --no-fail-fast -j 6 -- --test-threads=2` | PASS: 1,468 passed, 0 failed, 15 ignored across 51 targets | 1,405 s |

The counts equal those of the last recorded run on `main` ([review policy](review-policy-validation-2026-10-24.md)). Clippy printed only the existing future-incompatibility note for `proc-macro-error2`.

Stage 7 gate (`scripts/gate-stage7.ps1`, 2026-10-24 22:42–22:56 local time):

| Gate step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | PASS | 8.554 s |
| crate dependency boundaries | PASS | 0.245 s |
| cargo fmt | PASS | 4.695 s |
| Clippy | PASS | 6.883 s |
| Rust tests | STOPPED by hand after 28 of 51 targets (1,077 passed, 0 failed, 12 ignored) | — |
| GPUI release build | NOT EXECUTED | — |
| Windows GPUI smoke | NOT EXECUTED | — |
| Rust licenses and sources | NOT EXECUTED | — |
| Rust advisories | NOT EXECUTED in the gate (the standalone audit above passed) | — |

The gate was stopped by hand during its test step, so this report does **not**
approve the overall gate. The standalone suite above had already passed on the
same tree, and no test failed in the gate before the stop. The release build,
the Windows smoke test, the license/source audit and the gate's own advisory
step did not run. Run `scripts/gate-stage7.ps1` to completion before merging this
branch into `main`.

## Limits

- The binary-distribution policy (`quality/distribution-policy.json`) is
  unchanged. This report covers the technical gate's advisory step only.
- The rustls fixes change TLS behaviour (stricter handshake message boundaries,
  no secrets in `Debug`, bounded Brotli certificate decompression, a
  ServerHello downgrade guard). This validation used the hermetic suite only.
  It made no TLS connection to an external server and called no provider
  endpoint.
- A newer RustSec database can add advisories. This result applies to database
  commit `21d909e`.
