# Agent exchange closing validation (ADR-037 M6)

Date: 2026-10-30. Source: branch `feat/agent-exchange` at `4e396ad`, the last
code commit of the feature. Earlier milestones:
[M1](agent-exchange-m1-validation-2026-10-30.md),
[M2](agent-exchange-m2-validation-2026-10-30.md) and
[M3 and M4](agent-exchange-m3-m4-validation-2026-10-30.md).

This report approves no distribution.

## What M6 closed

- **Reviewer contract.** `review_finding` and `claim_under_verification` stay
  reserved for the multi-reviewer feature. A new test checks that they exist
  and that no crate produces them (`reserved_review_kinds_exist_and_nothing_produces_them`).
- **Threat model.** [Agent exchange section](cli-threat-model.md#agent-exchange--2026-10-30):
  new prompt inputs, the provenance line as a defense and its limit, and each
  control with its test.
- **Documentation.** The [architecture overview](../docs/architecture.md#agent-exchange)
  has an Agent exchange section, and the
  [ADR-037 summary](../docs/adr/037-agent-exchange.md) is marked implemented,
  except the ACP transport.
- **Two gaps closed.** Orphan snapshot blobs are removed in the transaction
  that deletes runs; before, nothing called the cleanup. A pulsation whose
  digest cannot be stored no longer blocks older ones or a search, and a
  digest stays within its cap after sanitization.

## Stage 7 gate

`scripts/gate-stage7.ps1` passed on `4e396ad` in 2,251.6 s, in the second
worktree, incremental over the M3 and M4 gate.

| Step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | Passed | 0.7 s |
| Crate dependency boundaries | Passed | 0.1 s |
| `cargo fmt` | Passed | 7.7 s |
| Clippy `-D warnings` | Passed | 40.1 s |
| Rust tests (`cargo test-all`) | 61 targets, 1,980 passed, 0 failed, 23 ignored | 1,505.6 s |
| GPUI release build | Passed | 688.2 s |
| Windows GPUI smoke | Window opened, graceful shutdown, exit 0 | 2.0 s |
| Rust licenses and sources | Passed | 2.0 s |
| Rust advisories | `cargo audit` exit 0, 0 vulnerabilities; 14 `unmaintained` and 2 `unsound` informational warnings; RustSec `2295466` | 5.1 s |

Against the baseline before M1 (59 targets, 1,913 passed, 17 ignored): two
more targets (`prompt_input_inventory` and `reserved_exchange_kinds`), 67 more
passing tests and 6 more ignored ones, all opt-in measurements. Against the M3
and M4 gate: the new target and 2 more passing tests.

## Measurements

The plan's closing report: prompt growth, transition latency, and manifest
and blob sizes, plus the other ceilings of the feature. All on `4e396ad`.

| Measure | Result | Ceiling | Verdict |
|---|---|---|---|
| Fixture benchmark (`composition_benchmark --held-out`) | 80/80 correct; calls, rejections and interventions equal to the baseline; every prompt identical to M1 | parity | met |
| v4 prompt growth over the baseline | +915 characters at the median, +1,234 at most | +1,000 / +1,700 | met |
| Legacy prompt growth over the baseline | +341 at the median, +497 at most | +600 | met |
| Pulsation transition p95, against the baseline code | +12% non-terminal, +16% terminal (+0.4 to +0.6 ms); an earlier session gave +23% and +26% | +10%, and at most 15 ms | over the relative ceiling, **cost accepted**; under 15 ms |
| Terminal re-read, page of 500 at 100,000 events | p95 23.0 ms | 50 ms | met |
| Load rehearsal | 14 lags and 7 terminal gaps, all re-read; 0 resets | no full reload | met |
| `search_pulsations` over 1,000 digests | p95 11.3 ms | 50 ms | met |
| Manifest and blobs for this repository | 1,025 files; about 252 KB of manifest rows; 144 blobs, 1 MiB | 1 MiB of blobs per run | within budget |

Details: [M2](agent-exchange-m2-validation-2026-10-30.md#pulsation-transitions)
and [M3 and M4](agent-exchange-m3-m4-validation-2026-10-30.md#measurements).
The developer accepted the transition cost on 2026-10-30.

## Not done

- **ACP transport (M5).** The parity gate needs adapters that the user
  installs; none was installed or run. A static reading of the adapters'
  published source found two blockers to resolve first: the Claude adapter
  loads the user's, the project's and local settings (hooks, MCP servers,
  permission rules and the initial mode), and token counts comparable to the
  usage ledger exist only in each adapter's own metadata. No ACP code or
  dependency was added.
- **Manual visual pass.** M3 and M4 add no new visual component: the resume
  preview uses the existing header notice, and the two tools render through
  the existing Companion tool activity. A pass in the real app is still owed:
  resume a manual CLI session after editing a file and read the notice; ask
  the Companion about an earlier pulsation and check that the answer cites
  `pulsation:<id>`.
- Binary distribution stays blocked by `zed-55470`.

## Rerunning

```powershell
cargo test --locked -p hvc-test-support --test reserved_exchange_kinds
cargo test --locked -p hvc-core --lib exchange_baseline_pulsation_transitions -- --ignored --nocapture --test-threads=1
cargo test --locked -p hvc-core --lib exchange_m2_ exchange_m4_ -- --ignored --nocapture --test-threads=1
$env:HVC_EXCHANGE_BASELINE_WORKSPACE = '<workspace to snapshot>'
cargo test --locked -p hvc-core --lib exchange_baseline_snapshot_manifest -- --ignored --nocapture --test-threads=1
cargo run --locked -p hvc-test-support --example composition_benchmark -- --mode fixture --held-out --output-dir <dir>
```
