# Focused test workflow — Windows validation, 2026-10-15

Design references: [public summaries](../docs/adr/README.md); [historical archive context](README.md#historical-design-references).

Base: ADR-024 M1 `7ee6ef58273c0c379082020567aa015a09dd150d`, implemented and validated in the separate `HARD-VC-test-workflow` checkout on `chore/test-workflow`. Source hashes and full measurements are preserved in [the JSON snapshot](test-workflow-validation-2026-10-15.json). The Git commit introducing these files identifies the delivered patch.

## Delivered behavior

- Five locked Cargo aliases: `test-ui` (UI), `test-editor` (editor + UI), `test-support` (support + UI), `test-core` (core), and `test-all` (workspace). All use two test threads per binary. Package selection does not execute dependency tests; focused filters remain development feedback.
- `CoreTestHarness` keeps a background multithread Tokio runtime with two workers per fixture. The two discovery-cache tests use a private injected scan and check one call for a fresh cache and two for an expired cache, without installed-CLI probes. Public application APIs and production discovery behavior are preserved.
- Stage 7 retains fmt, strict Clippy, complete tests, release, isolated Windows smoke and both audits. It reports each step's duration/result and writes JSON even on failure (`-TimingOutputPath`, default `target/stage7/timings.json`). UTF-8 with BOM preserves labels under Windows PowerShell 5.1.

## Executed evidence

Windows 11 Pro 10.0.26300, 12 logical processors, approximately 32 GiB RAM; Rust/Cargo 1.98.0 MSVC. Each checkout uses its own target. Registry packages were already downloaded when the new target was compiled.

| Measurement | Seconds | Result |
|---|---:|---|
| Empty-target test compilation, `--no-run --timings` | 256.882 | passed |
| Warm `--no-run` | 5.224 | passed |
| `cargo test-all`, warm run 1 | 460.293 | 943 passed, 0 failed, 2 ignored |
| `cargo test-all`, warm run 2 | 412.033 | 943 passed, 0 failed, 2 ignored |
| `cargo test-all`, warm run 3 | 390.592 | 943 passed, 0 failed, 2 ignored |
| Full-suite median | 412.033 | same Rust sources and two threads |
| `cargo test-ui`, warm | 42.135 | 209 passed, 0 failed |
| Two deterministic discovery tests, harness-reported | 0.610 | 2 passed, 612 filtered |

Actual alias listings matched UI **209**, editor + UI **224**, support + UI **316**, core **614**, and workspace **945** cases, including default ignored cases. The workspace contains 24 integration executables and the example marked `test = true`; four doctest targets ran with zero cases. The two retained ignored cases are the manual prompt printer and authenticated Codex smoke. No new cases were ignored and no retries were added.

The historically sensitive compression case `never_compresses_normative_content`, batch case `complete_six_stage_batch_requires_approved_integration` and `pulsation_m5` integration target passed in all three standalone complete runs after the worker limit. Real-process fixtures, temporary directories, dynamic ports and existing global-state locks were retained.

The UI and full-suite measurements have different scopes. They quantify local feedback cost, not acceleration of identical coverage. The first use of another package selection can compile a distinct dependency-feature variant. The first Clippy check took 105.551 seconds; the final gate below reused that cache. Installation of local `cargo-audit` 0.22.2 (dev build, no default features; Cargo-reported build 137 seconds) is excluded from test measurements.

## Complete Stage 7 gate with initially cold release build

PowerShell 5.1, without `-SkipAdvisories` or `-EnforceDistribution`; debug/Clippy caches warm, release target initially empty. **The complete gate failed only at the RustSec audit (exit 1, seven dependency advisories).** The other seven steps passed. The pre-existing distribution policy also remains blocking and separate from this infrastructure change.

| Step | Seconds | Result |
|---|---:|---|
| estrutura exclusivamente GPUI/Rust | 0.693 | passed |
| cargo fmt | 3.215 | passed |
| Clippy | 1.266 | passed |
| testes Rust | 381.949 | passed |
| build release GPUI | 471.057 | passed |
| smoke Windows GPUI | 1.309 | passed |
| licenças e fontes Rust | 1.377 | passed |
| advisories Rust | 20.448 | FAILED, exit 1 |

Gate-reported total: **881.423 seconds**; process wall time: **881.877 seconds**. The final attempt again passed **943 tests**, with zero failures and the same two ignored cases. Release startup used a new `HVC_DATA_DIR`, confirmed the window/database and closed gracefully. Audit results and dependency count are in the JSON snapshot.

Control-flow QA checked success, native exit 7, PowerShell exception propagation, JSON after failure and preservation of the original error if reporting fails. Parsing, labels and step control were checked on PowerShell 5.1 and 7.6.5. An earlier gate attempt was deliberately interrupted to fix source encoding; it is not counted as a completed run or a test failure. Rust sources were unchanged after the three benchmark runs.

## Three warmed Stage 7 gate invocations

After the initial release build, the unchanged gate was invoked three more times without skipping advisories. Warm runs 1 and 2 passed their first seven steps, including 943 tests with zero failures and two existing ignored cases, then failed fetching the advisory database. Their old on-disk summary was stale and is not counted as fresh security evidence. An isolated native audit after resumption fetched the database successfully and confirmed the same seven findings. Warm run 3 stopped at the test stage with 612 core tests passed, one failed and one ignored; later stages were not reached. These planned measurement samples are separate from the three standalone suite runs above and are not retries for approval.

| Step | Run 1 seconds | Run 2 seconds | Run 3 seconds | Median seconds |
|---|---:|---:|---:|---:|
| estrutura exclusivamente GPUI/Rust | 0.623 | 7.032 | 0.591 | 0.623 |
| cargo fmt | 3.232 | 7.600 | 3.123 | 3.232 |
| Clippy | 1.263 | 10.856 | 1.203 | 1.263 |
| testes Rust | 378.759 | 413.699 | 156.722 (FAILED) | n/a |
| build release GPUI | 1.109 | 8.613 | not reached | n/a |
| smoke Windows GPUI | 1.001 | 1.259 | not reached | n/a |
| licenças e fontes Rust | 1.275 | 1.330 | not reached | n/a |
| advisories Rust | 0.202 (FAILED) | 20.206 (FAILED) | not reached | n/a |
| Gate process wall time | 387.966 | 471.885 | 162.981 | n/a |
| Cargo test preparation (inside test step) | 1.090 | 5.740 | 1.040 | n/a |

Cargo test preparation is included in the test step and must not be added again. Runner-reported test time and individual operational reports are retained for each sample in the JSON. Only stages completed successfully in all three samples have a comparable median. No whole-gate median is certified: the third test execution has partial coverage and stops before later stages. Release build, smoke and audit costs remain visible rather than being attributed to test parallelism.

## Existing dependency-security blocker

The `Cargo.lock` Git blob is identical to M1 (`eb8c44c12925532e4219627b1fa43dcc9c002463`); this patch changes no dependency manifests or versions. The audit database revision is `edb58f5974b83eb8302332821cb6d389777aa351`. Seven findings concern two already locked packages:

| Advisory | Locked package | Patched versions reported by RustSec |
|---|---|---|
| [RUSTSEC-2026-0338](https://rustsec.org/advisories/RUSTSEC-2026-0338.html) | quinn-proto 0.11.17 | >=0.11.18 |
| [RUSTSEC-2026-0340](https://rustsec.org/advisories/RUSTSEC-2026-0340.html) | quinn-proto 0.11.17 | >=0.11.18 |
| [RUSTSEC-2026-0343](https://rustsec.org/advisories/RUSTSEC-2026-0343.html) | quinn-proto 0.11.17 | >=0.11.18 |
| [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html) | rustls 0.23.43 | >=0.23.45 |
| [RUSTSEC-2026-0346](https://rustsec.org/advisories/RUSTSEC-2026-0346.html) | rustls 0.23.43 | >=0.23.46 |
| [RUSTSEC-2026-0348](https://rustsec.org/advisories/RUSTSEC-2026-0348.html) | rustls 0.23.43 | >=0.23.46 |
| [RUSTSEC-2026-0349](https://rustsec.org/advisories/RUSTSEC-2026-0349.html) | rustls 0.23.43 | >=0.23.46 |

These findings are recorded for a separate security/dependency update and must be reassessed with the real enabled features and supported platforms. No advisory was ignored or downgraded. Focused test commands are available for M2; the final quality gate remains red until the dependency findings are resolved. Passing tests do not override that result.

## Existing intermittent language-validation defect

The third warm gate failed `persistence_service::forge_batches::tests::feature_prerequisites_can_share_a_complete_atomic_batch` during fixture approval, before the batch assertions. The error was `agent-authored feature plan must be English; detected non-English marker da+de`. Plan publication inserts a random feature UUID into YAML frontmatter, then the prose validator tokenizes that metadata as natural language. A valid UUID such as `000000da-0000-4000-8000-000000de0000` contains both weak Portuguese markers. The failure occurs in unchanged publication/language code; the fixture uses `PersistenceService` directly and bypasses the modified `CoreTestHarness`.

The relevant test, publication, service and language-guard files are identical to M1; their Git blobs are recorded in the JSON. This test passed in the previous six complete runs (three standalone suites, the initial gate and two warm gates) and failed in the seventh. This observation does not erase the three successful benchmark samples. No retry, ignored case, fixture-ID substitution or production-validator change was used to obtain artificial approval. Track a production fix that separates opaque metadata from prose and adds a deterministic regression for a marker-containing UUID, then revalidate the full gate.

## M2 handoff

During Review Brief work, use `cargo test-ui`, `cargo test --locked -p hvc-core --lib review_brief -- --test-threads=2`, and `cargo test --locked -p hvc-test-support --test review_brief -- --test-threads=2` as appropriate. Check a filter with `--list`; zero selected cases do not prove coverage. Use explicit `cargo test` for Cargo flags because aliases already contain the harness delimiter.

Finish M2 with fmt, strict Clippy and `cargo test-all`; run the complete Stage 7 gate for the final delivery. New tests in these crates are included automatically. Refresh the inventory for M2 rather than treating this dated snapshot as validation of later code. The Cerebro runbook, strategy, inventory, map, backlog and delivery report document the same workflow. `nextest`, automatic Git-impact selection, remote runners and crate subdivision are deferred pending a measured need.
