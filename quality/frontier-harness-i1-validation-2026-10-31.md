# Frontier Harness I1 validation

Date: 2026-10-31. Scope: I1/M2, stable prompt prefixes and qualified cache
measurement. Code commit: `a078d152cce6fbc4e7e14e9839d17059e30164c2`, based on
`c03d708da0f38053b80f2f5ac434ef50f3608915`, on branch
`feat/frontier-harness-i1`. The qualification below remains attributed to that
source. It was subsequently merged and published; see the integration record
at the end of this report.

## Resulting behavior

- Companion API notices are transient tail input. They preserve the system
  contract, prior history and complete tool-call/result pairs across Responses,
  Chat Completions and Anthropic payloads. CLI prompts put the fixed contract
  and response schema before history, with current notices and budgets in the
  suffix.
- Composed repair diagnostics follow the complete response contract. Stable
  prefixes are compared within the same executor and policy; author and repair
  roles retain their distinct contracts.
- Cache reuse uses paired, eligible ledger observations. Missing breakdowns
  remain unknown; invalid pairs, partial observations, zero input and unavailable
  usage retain explicit coverage. Grouping uses each accounting row's transport,
  provider and model identity, without inventing a model for missing metadata.
- Optional report metadata carries that coverage through the typed HTML
  boundary. Older reports still decode and serialize without the new fields.
  JSON, text and HTML share the cache-percentage rounding rule. Saturated totals
  are marked as inexact, and extreme stage counts do not overflow chart
  arithmetic. Runtime strings remain escaped and the report CSP permits no
  scripts or network access.

The change adds no dependency, database migration, tool permission or model
selection capability. I2 batch inspection is not implemented. M6b model
escalation remains deferred; M8 candidate normalization remains conditional on
its separate isolation qualification.

## Tests and complete gate

Twenty-two tests were added through the failing-test-first cycle. They cover
actual API dispatch capture and CLI prompt ordering, correction suffixes,
paired cache denominators and identity groups, missing/invalid measurements,
legacy HTML compatibility, escaping, overflow and shared rounding.

The final Stage 7 gate passed all nine steps on the code commit above. The
fresh suite ran **61 targets: 2,002 passed, 0 failed, 23 ignored and 0 filtered**.
The unchanged baseline had 1,980 passing tests and 23 ignored tests.

| Step | Result | Duration |
|---|---|---:|
| GPUI/Rust-only structure | Passed | 0.703 s |
| Crate dependency boundaries | Passed | 0.164 s |
| `cargo fmt` | Passed | 5.408 s |
| Clippy `-D warnings` | Passed | 26.195 s |
| `cargo test-all` | 2,002 passed; 0 failed | 1,380.354 s |
| GPUI release build | Passed | 586.352 s |
| Windows GPUI smoke | Window opened and graceful shutdown passed | 1.680 s |
| Rust licenses and sources | Passed | 1.964 s |
| Rust advisories | Exit 0; 0 vulnerabilities | 4.862 s |

Total gate time: 2,007.850 s. The advisory report records 14 `unmaintained`
and 2 `unsound` informational warnings, with RustSec revision
`2295466c4e9df26647fd67d8fa1c705ac2fc7f81`. No advisory step was skipped.
The timing record is `target/frontier-harness/i1-qualified-gate-timings.json`
in the validation checkout; temporary evidence under `target/` is not shipped
in Git.

## Offline fixture comparison

The baseline completed all **80/80 observations**: the same 20 fixtures across
four execution arms, including the held-out set. Fixture dataset SHA-256:
`786e5cc1da8409dc4985da0d27cea9c388d4c42fc82a82121ce1515c1f093c5e`.

The candidate also completed **80/80 observations correctly**, with zero
interventions. Paired comparison found no differences in provider calls,
rejected attempts, prompt characters, estimated tokens or per-call records.

| Arm | Calls before/after | Rejections before/after | Prompt characters before/after | Estimated input before/after |
|---|---:|---:|---:|---:|
| A | 125 / 125 | 0 / 0 | 1,476,661 / 1,476,661 | 360,201 / 360,201 |
| B | 125 / 125 | 5 / 5 | 876,218 / 876,218 | 213,087 / 213,087 |
| C | 145 / 145 | 5 / 5 | 1,031,936 / 1,031,936 | 252,581 / 252,581 |
| D | 145 / 145 | 5 / 5 | 1,031,936 / 1,031,936 | 252,581 / 252,581 |

Rejections are expected recovery cases. The change reorders stable blocks and
qualifies cache reporting without removing instructions. The
[paired comparison](frontier-harness-i1-benchmark-comparison-2026-10-31.json)
records descriptive p50/p95 durations for one run per revision; load was not
controlled, so those durations do not establish a causal speedup. The
[final gate record](frontier-harness-i1-gate-2026-10-31.json) preserves counts,
timings and advisory results outside disposable build output.

Fixture mode uses scripted replies over loopback. Tokens are estimated as
characters divided by four. It measures host orchestration and prompt size,
not real-model quality, provider cache hits, monetary savings or production
latency. No paid provider was used for this qualification.

## Presentation limits and reproduction

HTML checks covered deterministic rendering, legacy output, coverage text,
escaping and CSP. Browser inspection of the generated local HTML was blocked
by file-URL access policy, so presentation QA is static only. The native UI is
unchanged; its Windows release smoke passed. This is not a new physical visual
acceptance pass.

Run from the checkout root:

```powershell
cargo test-all
powershell -ExecutionPolicy Bypass -File scripts/gate-stage7.ps1 -TimingOutputPath target/frontier-harness/i1-qualified-gate-timings.json
cargo run --locked --offline -p hvc-test-support --example composition_benchmark -- --mode fixture --arm A,B,C,D --held-out --output-dir target/frontier-harness/after
```

The advisory gate requires the existing local `cargo-audit` tool and current
RustSec data, as documented in the [testing guide](../docs/testing.md).

## Integration and cleanup — 2026-10-31

The developer confirmed integration. Merge
[`6372f40`](https://github.com/Nathan-dOS/HARD-VC/commit/6372f40da194328ca1af617722f988c66c716139)
was pushed to `origin/main`, with the remote ref verified at that revision.
The merge tree exactly matched the qualified branch head `02b706a`; the four
changed Rust source blobs are identical to code commit `a078d15`. The passing
2,002-test suite and 80-observation benchmark remain the pre-merge qualification,
not a new execution on the merge.

The temporary worktree and local feature branch were removed and worktree
metadata was pruned. No remote feature branch existed. The pre-existing local
edit in `candidate_capture.rs` was restored byte for byte, with its diff verified;
it was excluded from publication and from this qualification. The temporary
preservation stash was removed. The [integration record](frontier-harness-i1-integration-2026-10-31.json)
contains revision, source identity and cleanup evidence.
