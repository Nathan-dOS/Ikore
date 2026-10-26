# ADR-035: execution mandates, just-in-time scope grants and trust layers

Date: 2026-10-26

Status: accepted, not implemented yet. Planned migrations: V55 (mandates and
approval receipts) and V57 (scope requests).

## Context

Approving a pipeline v4 Specification already grants authority, but the grant
is spread over the frozen plan, unit ownership, executor policies, check
profiles and budget settings. No screen shows it before the click, and the
checkpoint row records the decision without a digest of what was approved.
Each stage reads its budget parameters from the live runtime settings at
admission. A Code worker that needs to change a delivered file it does not own
returns `scope_insufficient`, and the only ways out are a Retry with the same
result or a Specification revision that discards validated work.

Agents never write to the workspace: CLI runs use a read-only snapshot or an
empty directory, and every write is a host publication. A mandate is therefore
an explicit record of authority plus one more check at the existing write
points, not a sandbox.

## Decision

**Mandate.** Approving the Specification issues an immutable,
hash-chained `ExecutionMandateV1` in the same transaction. It records:

- the approved intent: task revisions, accepted artifact digests and the
  `REVIEW.md` snapshot;
- the write envelope, as host-derived write classes;
- each executor's edit operations, writable roles, tool policy, context scope
  and transport;
- the enabled check profiles;
- the budget parameters, which are frozen for the rest of the pulsation.

A revision is added on re-approval, on an approved CLI fallback and on a scope
grant. Models never see or edit the mandate. Failing to derive it refuses the
approval.

**Preview and receipts.** The Specification brief shows "What approving
grants", derived by the same function. Every checkpoint resolution records an
`ApprovalReceiptV1` with the reviewed artifact digests. Free text enters only as
a SHA-256 digest.

**Enforcement.** In v4 a path outside the envelope is refused at artifact
acceptance and again at publication, and a divergent executor, transport or
check profile is refused before a permit is reserved. Plan freezing already
prevents these cases, so a hit is a host defect.

**Trust layers.** A pure, versioned function derives four layers from
durable rows:

- Identity: agents, transports, effective and observed models, approvals;
- Scope: envelope, touched and refused paths, sensitive paths;
- Guardrails: tool policy, budget, checks, sanitization counts, revision
  rounds;
- Intent: criteria states, risk, acknowledgements.

States are `clear`, `attention`, `issue`, `not_applicable` and `unknown`;
`clear` means no issue the host can detect. The panel never offers a decision.

**Just-in-time scope grant.** A `scope_insufficient` whose `relatedRefs` resolve
to delivered, writable files becomes a scope request. It is all or nothing,
with at most 8 paths per request and 2 requests per stage execution. The
existing stage-failure checkpoint gains "Grant and retry", which in one
transaction:

1. checks freshness and file hashes;
2. adds a `scope_grant` revision;
3. runs the existing Retry, without renewing the shared budget and reusing
   validated sibling units.

Test units only receive test paths. The Companion stays read-only.

Pipelines v1 to v3 get receipts, partial layers and a "Mandate not applied
(legacy pipeline)" label.

## Consequences

Frozen budget parameters are the only happy-path behavior change. The
enforcement duplicates plan-level checks on purpose. A new `ScopeGrantCommandV1`
leaves `StageRecoveryCommandV1` unchanged. The migrations are additive, and the
rows are deleted with their project. Composition stays opt-in, so projects
without it get receipts and layers but no enforcement.
