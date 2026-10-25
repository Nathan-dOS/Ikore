# ADR-034: risk-driven scrutiny, pre-flight guardian and documentation reconciliation

Date: 2026-10-25

Status: implemented (migrations V52 to V54); Linux headless validation in
`quality/feature-10-remaining-validation-2026-10-25.md`. Windows acceptance
and the T1 real-run measurement are pending.

## Context

ADR-032 gave the composed (pipeline v4) Integrator classified inputs, a
read-only `REVIEW.md` policy and a revision-round threshold. Four gaps remain:

- Intake, Contract and the two repairers declare no tools, yet a CLI worker
  still reads the sanitized workspace snapshot. Composition is opt-in and off
  by default, so projects without it get none of ADR-032.
- The Intake's per-task `risk` is only rendered and the notes' `risk` only
  widens context expansion. Neither selects checks, and the result does not
  say what was executed, what only the model judged and what nobody verified.
- Forge triage sees only the feature, not the project's decisions, so a
  conflict with an accepted decision surfaces after the expensive stages.
- Notes that list `source-files`/`test-files` silently go stale when that code
  changes outside a pulsation.

## Decision

**Tool policy (T1).** The host enforces every executor's declared tool
policy. The declarative `None` disappears. Each executor is either
host-context-only (CLI in an empty directory, custom adapters refused) or
honestly declared read-only scoped. A measurement decides which one per
executor. Earlier pipelines do not get ADR-032; the Forge says when the review
policy does not apply because composition is off.

**Risk (M3, v4 only).** A versioned risk policy combines the Intake risk, note
risk, sensitive paths and change traits in a pure function. The level is the
maximum signal; `unknown` stays visible and selects like `medium`. Projects may
add sensitive globs and required checks, never remove built-in ones. The level
selects the required checks among configured profiles; a missing profile is a
visible gap that does not downgrade verdicts. Low risk never removes a stage or
checkpoint. The Integrator shows a coverage matrix (`executed`, `model_only`,
`unverified`, `not_run`). Approving a high-risk pulsation with unverified or
unrun items needs an acknowledgement with a reason. `REVIEW.md` is captured at
Intake admission and that capture applies to the whole pulsation.

**Pre-flight guardian (M6).** Forge triage receives `PROJECT_AGENT.md` and the
accepted decision notes, selected, budgeted and sanitized by the host. It does
not make an extra model call. A blocking conflict prevents approval, automatic
approval included, until the developer adjusts the feature, proposes a decision
as a new Forge feature, or dismisses the warning with a reason. Only delivered
decisions may be cited, and a new feature revision invalidates the finding.

**Documentation reconciliation (M5).** The host keeps a baseline per
note-to-code link. Detection hashes files locally and sends nothing to a model.
On request only, a tool-less executor proposes anchored edits to that note
alone. Publication rechecks the note revision and the code hashes, and any
difference invalidates the proposal. The UI never attributes authorship or
claims a defect.

## Consequences

The listed executors may lose context; the measurement is recorded. Triage
prompts grow by at most 32 KiB of decisions. High-risk acknowledgement and
blocking conflicts add friction on purpose, and their value must be measured.
Additive migrations add risk, acknowledgement, pre-flight and reconciliation
tables; reconciliation base bytes stay local with per-file and per-project
caps. The one exception is V54: the new `documentation_reconciler` role needs
the three role-checked tables rebuilt, as V50 did for the Notes Scribe, with
every row kept. The Integrator manifest moves to schema version 2. Generated
documentation edits are English (ADR-013).
