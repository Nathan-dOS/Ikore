# ADR-033: verifiable Rust/Cargo candidate validation

Date: 2026-10-25; integrated 2026-10-26. Status: implemented for M1/M2;
Windows qualification remains pending. See the
[verification report](../../quality/cargo-validation-2026-10-26.md).

## Context

The v4 runner already executes developer-owned commands against disposable
candidates and records receipts. The original Cargo presets do not yet provide
a complete workspace profile: preparation is not separate evidence, snapshots
are text-only, assets need explicit handling, and receipt freshness must account
for the effective profile, tools, policy and environment as well as source files.

The Integrator boundary, read-only `REVIEW.md` and convergence controls already
exist under ADR-032. Risk-based selection and documentation reconciliation are
separate work and do not block an explicitly approved validation profile.

## Decision

Extend the existing runner for the first Rust/Cargo increment. Version 2 presets
run workspace checks/tests with `--locked --offline`; detection suggests them
for developer review and does not enable execution, replace an existing profile,
download tools or install dependencies.

Capture a complete fenced baseline and manifest before unit dispatch. Build and
baseline comparison runs use independent physical copies, never hardlinks.
Resume and repair use the captured baseline rather than silently rereading the
live checkout. Selected binary assets are local command inputs, not LLM context.
Snapshot limits are 1 MiB per file, 64 MiB total and 10,000 files. Incomplete
capture and changed run inputs cannot support a successful result.

Keep `ValidationCommandProfileV1` and `ValidationReceiptV1` readable and add
versioned host-only `CargoValidationPolicyV1` and `RunEvidenceDetailsV1` sidecars,
with additive persistence in migration V55. Its third table,
`stage_cargo_validation_contexts`, durably binds each execution to its captured
baseline and frozen profiles, policies and tools so resume cannot silently adopt
changed project settings. Freeze policy and profile before dispatch. Observe
installed cargo/rustc/rustdoc and preparation outcomes without
installing a missing toolchain. Preparation and execution share the check's
deadline: 180 seconds by default for check, 900 for test, at most 900 seconds.
Retained logs remain bounded to 1 MiB, with at most 64 normalized diagnostics.

Separate process state, output interpretation and check result. Required output
that cannot be interpreted is unknown/error even when the process exits 0.
Unknown test totals remain unavailable, not zero. Compare relevant inputs before
and after execution, and require complete candidate/input, profile/command,
policy/parser, toolchain and environment bindings for reuse and freshness.

Extend ADR-032's existing host evidence projection, snapshots, UI and reports to
show preparation, interpretation, integrity and limitations consistently. Host
gates, sanitization and human checkpoints retain their authority.

## Consequences

This provides inspectable execution evidence for a trusted Cargo project. It is
not a security sandbox or a hermetic build: project code and build scripts run
with the runner's OS identity, and Cargo offline mode does not isolate arbitrary
child processes from the network or filesystem. Baseline copies consume bounded
disk space. More complete identity checks may reject previously reusable results.

Other ecosystems, dedicated security profiles and mutation testing remain
later increments. Existing risk-based selection is owned by ADR-034 and consumes
these receipts, including explicit unknown/partial/integrity limitations. Qualification must record actual
test results; Windows process control and GPUI smoke are separate from Linux
headless checks. No gate is declared passed by this design record.

See [Cargo candidate validation](../cargo-validation.md) for commands, evidence
states and operating limits.

## Integration update — 2026-10-26

Rebased on `456c91d`, which completes feature 10 in ADR-034. This decision now
uses migration V55 and review manifest schema 3, preserving ADR-034 risk fields,
Intake policy snapshots and high-risk acknowledgement. Executed failed evidence
remains negative evidence; inconclusive Cargo results do not satisfy coverage.
