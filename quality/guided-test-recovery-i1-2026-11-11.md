# Guided test recovery I1 — implementation validation

Date: 2026-11-11, America/Sao_Paulo. Source base: `ee20e14`.
Branch: `feat/guided-test-recovery-i1`. No additional worktree was created.

Status: I0/I1 implemented and automated qualification passed; integration awaits
the developer's branch-closure decision. Semantic Test/Specification/Contract
recovery routes remain planned.

The branch is based on the preexisting Ikore renaming branch, seven commits ahead
of local `main`. Integration must account for that dependency explicitly.

## Delivered behavior

- Immutable, bounded validation-failure evidence stored with the checkpoint.
- Candidate test hashes/content retained independently of superseded artifacts.
- Human revision binds evidence to the exact newly enqueued job. Empty guidance
  still delivers actionable check observations, with correct provenance.
- Native evidence card and recovery-aware reuse keys; existing authority and
  optional/no-test-applicable policies remain unchanged.
- Additive V66 migration; older failures receive no fabricated evidence.

## Execution record

| Check | Observed result |
|---|---|
| Initial workspace baseline inside sandbox | Build failed before test execution: Windows linker LNK1114, access denied replacing existing example `.lib` files. |
| Same workspace selection outside sandbox, `-j 2` | Original compiled tests passed; doctests failed because source was changed before the run finished, mixing new source and old core libraries. This run does not qualify I1 or a clean baseline. The final gate must run sequentially against the final source. |
| New integration tests before implementation | 0 passed, 2 failed: missing validation context and snapshot table, as expected. |
| Recovery integration, including the later job-isolation assertion | 2 passed, 0 failed in the focused rerun and complete workspace gate. |
| Initial recovery unit selection | 2 passed, 1 failed: containment assertion assumed injected text began at the fence. Updated to verify actual open/injection/close positions. |
| Prompt-size boundary | RED observed (oversized model projection); after bounded projection, all 4 recovery units passed, 0 failed, 0.05 s. |
| Strict Clippy, all workspace targets | Passed, exit 0. Cargo reports an existing future-compatibility notice for `proc-macro-error2`, outside Clippy errors. |
| Upgrade suite | 34 passed, 0 failed, including populated V65-to-V66 compatibility. |
| Native failure-evidence layout | 1 passed, 0 failed; recovery actions remain accessible at 960×640. |
| SQLite WAL at 512/4096/65536-byte page sizes | Passed after updating the expected final schema from 65 to 66; all existing integrity assertions retained. |
| Formatting | `cargo fmt --all -- --check`: exit 0. |
| Complete final workspace | **2,203 passed, 0 failed, 23 preexisting ignored, 0 filtered; 62 targets**, exit 0. Includes integration tests, test-enabled examples and doctests. |

The first complete I1 attempt stopped in core with 1,224 passed, one failed and
seven ignored. `migrated_wal_databases_preserve_snapshots_rollback_and_reopen_at_page_size_limits`
still asserted final migration 65. The intended V66 schema change requires 66;
that expectation was updated while retaining snapshot, rollback, reopen,
page-size, checksum, integrity and foreign-key assertions. The corrected focused
SQLite test and the complete workspace then passed. Failed log:
`target/guided-test-recovery-full.log`; final successful log:
`target/guided-test-recovery-full-rerun.log`.

Logs are under this checkout's `target/guided-test-recovery-*.log`. They are local
execution artifacts, not source-controlled fixtures. The final command was
`cargo test --locked --workspace -j 2 -- --test-threads=2`.

The [machine-readable record](guided-test-recovery-i1-2026-11-11.json) binds the
final result to 15 changed Rust/SQL source and test files, with zero drift during
the gate. Their sorted `path NUL SHA256(file bytes) NUL` manifest is
`988282cf08dd7d87e419466a56e6544cb9676751d369e6373fcc95a503336dcc`.
There are eight new test functions and one updated existing SQLite test. Focused
runs overlap the complete suite and are not added to its total.

No authenticated provider run, release build or physical native-app smoke has
been performed for I1 at this point. GPUI tests alone do not establish those gates.
