# ADR-032: Integrator evidence boundary, review policy and convergence

Date: 2026-10-24

## Context

The composed (pipeline v4) Integrator received the Test coverage report and the
Specification note both as sources and as editable destinations. The report
mixes the Test author's narrative with the host's check table, and the note is
the approved specification, which only a human revision may change. The
reviewer saw no base or diff, received the host pre-checks as prose, and missing
context passed silently. Over a CLI it could read the whole workspace although
its policy declared no tools. Projects had no review guide for the reviewer and
no limit on human revision rounds.

## Decision

The host classifies every Integrator input: approved intent, candidate files
published by Code/Test, baseline context, stage documents (not delivered),
author claims taken from the typed Code/Test payloads and labelled as such,
host evidence and the read-only review policy. The reviewer may edit only
baseline documents, never a candidate or a stage document.

The first publication of each path in a pulsation records its base. Candidates
are bound to the published hash; a later hand edit or an unreadable file is an
evidence gap. The reviewer receives a bounded line diff of the sanitized base
and candidate views. What the host cannot establish becomes an explicit gap.
Gaps that invalidate evidence downgrade a `supported` verdict to `unverified`;
no gap promotes a verdict, and a missing diff does not downgrade because the
complete candidate is delivered. The canonical input manifest is persisted per
execution and its digest joins the Integrator's reuse key.

The composed Integrator reads host-selected context only: a CLI reviewer runs in
an empty directory with the Companion hardening, and a custom CLI adapter parks
the stage before any permit is spent. `REVIEW.md` files cascade from the
project root to the reviewed paths, within per-file, file-count and total
budgets, with visible states. They rank below host gates, approved intent and
`PROJECT_AGENT.md`, are sanitized like any other source, and no pipeline stage
may publish one.

Human revision rounds per input revision are counted separately from technical
attempts. After `revisionRoundThreshold` rounds (default 3, a hypothesis to
evaluate) every further revision needs an explicit decision with a reason:
continue, or replan from the Specification. Decisions are durable.

Earlier pipelines are unchanged and show that the review policy does not apply.

## Consequences

The reviewer gains a diff, structured evidence and explicit gaps, and loses its
CLI read tools; review quality must be measured rather than assumed. Larger
changes produce larger Integrator prompts. Changing the policy digest
invalidates the reuse of Integrators parked at the upgrade. Migration V51 adds
three tables; base bytes stay local and are cleared when the pulsation ends.

See [review policy](../review-policy.md) and the validation report in
`quality/`.
