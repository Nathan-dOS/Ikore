# Guided test recovery — I1

The first increment preserves the evidence behind a composed validation-failure
checkpoint. It keeps check results separate from agent coverage and human guidance.

When the developer requests a Code revision, the newly enqueued job receives the
failed checks' bounded diagnostics and the captured test evidence automatically.
The operator can leave the guidance field empty. The host still enforces the
approved requirements, file ownership, budgets and recovery targets. A later job
does not inherit the binding merely because it belongs to the same pulsation.

Migration V66 adds immutable `validation_failure_contexts` and
`validation_failure_recovery_jobs` sidecars. Old checkpoints remain readable
without invented snapshots. Failure evidence survives revision and restart;
deleting its owning checkpoint/project follows the existing cascade lifecycle.

The native failure panel shows the captured results, tests and limitations.
Model input fences diagnostics as tool output and preserved tests as either
repository content or agent-produced candidate content. Persistent identifiers
and hashes stay in metadata. Recovery evidence participates in the unit reuse key
and the existing prompt-budget checks. No extra model request or success-path
snapshot is introduced.

Capture is deliberately bounded: at most 16 failed checks, 32 recognized test
paths and 24,576 characters of complete sanitized test content, with at most 8,192
characters per test. Tests that changed after execution, cannot be read safely or
exceed a limit retain an explicit unavailable reason and their candidate digest.
Inline tests and external inputs may be absent. An original-byte digest does not
certify that redacted text is identical to the original. The model projection is
bounded to 16,384 characters separately and reports omitted sections. The local
JSON sidecar allows up to 2 MiB to accommodate escaping of bounded text.

Test applicability and the existing validation schedule remain unchanged. A
pulsation can have no applicable automated test. A required failed check still
blocks; historical evidence is never a pass or permission to weaken assertions.

Later increments will add guided choices for updating a stale test against an
already approved decision and revising behavior through Specification/Contract.
Those semantic recovery routes are not implemented by I1.

Execution status and exact commands are recorded in the
[I1 validation report](../quality/guided-test-recovery-i1-2026-11-11.md).
