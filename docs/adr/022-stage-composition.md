# ADR-022: host-coordinated stage composition

Public summary of the implemented v4 boundary at `bee361b` (2026-10-17).
The six outer stages and the human checkpoint contract remain in place.
Legacy v1-v3 pulsations retain their original execution path.

The Rust host freezes the plan, references, owned destinations, execution
profile and budget. Workers return bounded semantic results. The host parses,
validates, aggregates and publishes; a successful worker does not independently
approve the stage or prove that a check ran.

## Contract files

- [Composition module](../../crates/hvc-core/src/orchestration/composition/mod.rs)
- [Versioned schema](../../crates/hvc-core/src/orchestration/composition/fixtures/stage-composition-v1.schema.json)
- [Schema examples](../../crates/hvc-core/src/orchestration/composition/fixtures/stage-composition-v1.examples.json)
- [Plan validation](../../crates/hvc-core/src/orchestration/composition/plan.rs)
- [Payload validation](../../crates/hvc-core/src/orchestration/composition/validate.rs)
- [Exact edits](../../crates/hvc-core/src/orchestration/composition/edits.rs)
- [Host aggregation](../../crates/hvc-core/src/orchestration/composition/aggregate.rs)

The schema/examples are available in this repository. Historical validation
digests identify the original copies; this documentation does not change them.

## Invariant registry

The identifiers below match `Invariant` in the source. These descriptions are
public summaries of the contract, not a second schema implementation.

| ID | Boundary |
|---|---|
| SC-001 | Reject unknown versions/properties; preserve legacy semantics |
| SC-002 | Approved frozen scope owns task and criterion identities |
| SC-003 | The host owns IDs, hashes, paths and content identity |
| SC-004 | Existing, planned, candidate and unavailable references remain distinct |
| SC-005 | Enforce each executor's stage, payload, write and tool policy |
| SC-006 | Validate graph, ownership, dependency closure and capacity before dispatch |
| SC-007 | Project only necessary, scoped context |
| SC-008 | Strict parsing, payload validation and artifact-language checks |
| SC-009 | Exact internal edits preserve untouched bytes; workers never publish |
| SC-010 | Deterministic aggregation requires coverage and resolvable evidence |
| SC-011 | Execution claims require host receipts for the exact candidate/profile |
| SC-012 | Required failed/unavailable checks and blockers prevent advancement |
| SC-013 | Durable budgets are shared across units, retries and fallback |
| SC-014 | Freeze effective profiles and permitted targets |
| SC-015 | Check parent/child fences before acceptance without invalidating siblings |
| SC-016 | Bound corrections and reuse; retain original provenance |
| SC-017 | Cancellation, revision and Abort stop dispatch and reject late results |
| SC-018 | Aggregate acceptance creates publication intent; replay remains authoritative |
| SC-019 | Preserve legacy state/history and explicitly admit v4 capabilities |
| SC-020 | Count actual usage once, including failed calls |
| SC-021 | UI/recovery uses relevant versions and cannot approve children independently |
| SC-022 | Test/Integrator cannot automatically repair production |
| SC-023 | Trusted-project checks are host-controlled, without an OS-sandbox guarantee |
| SC-024 | Keep proposed design, implementation and executed evidence distinguishable |

## Evidence and limits

The 2026-11-02 destination-planning refinement freezes host-resolved file
candidates and their origin in the batch, separately from approved task prose.
Bounded repository reads and approved scope supply conventional paths;
developer overrides are optional and validated. Existing files and proposed
destinations keep their separate reference roles. These candidates do not
change Specification review, ownership, mandates or publication authority.
Older manifests with no candidates keep their preventive path checkpoint
compatibility. Dispatch reserves half the composed prompt capacity
`max(64000, 4 * context_chars_limit)` and excludes duplicated feature backlog
drafts when measuring the model-facing manifest. See the
[2026-11-02 validation record](../../quality/forge-destinations-validation-2026-11-02.md)
for current check status.

Host-controlled checks run against a disposable candidate of a trusted project.
The process controls do not create an operating system sandbox. Publication of
multiple files uses durable intent, replay and reconciliation rather than an
atomic filesystem transaction.

The [composition benchmark](../../quality/stage-composition-benchmark-2026-09-13.md)
and [integration report](../../quality/main-merge-validation-2026-10-17.md)
record specific executed checks. They do not establish universal cost savings
or certify later source changes.
