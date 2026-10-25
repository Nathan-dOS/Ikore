# Review policy and the Integrator's inputs

This page describes what the final reviewer of a composed (pipeline v4)
pulsation receives, how a project can guide it with `REVIEW.md`, and how the
host limits repeated revision rounds. The decisions are
[ADR-032](adr/032-integrator-review-boundary.md) and, for risk scrutiny and the
Intake capture of `REVIEW.md`, [ADR-034](adr/034-risk-preflight-and-doc-reconciliation.md).
Pipelines before v4 are
unchanged; their Integrator checkpoint says that the review policy does not
apply.

A favorable review does not certify that the change is free of defects. Host
checks and the developer's approval remain required.

## What the reviewer receives

| Section | Content | Editable by the reviewer |
|---|---|---|
| Tasks, requirements, decisions, interfaces | The approved intent | No |
| Risk scrutiny | The level the host assessed, its reasons and unknown inputs | No |
| Review policy | The `REVIEW.md` cascade, sanitized | Never |
| Host evidence | Status, freshness and limits of every Code/Test check | No |
| Evidence gaps | What the host could not establish, with affected criteria | No |
| Changes (base → candidate) | Bounded line diff of the sanitized views | No |
| Candidate files | Final content of files Code/Test published in this pulsation | No |
| Context files | Brief, task and clarification files the pulsation did not change | Documents only |
| Author claims | Code evidence and Test coverage explanations, labelled as narrative | No |

The Specification note and the Test report are not delivered: their normative
content already arrives typed, and the report mixes the Test author's narrative
with the host's check table. Hashes, receipt identifiers and execution
identifiers never enter the prompt; they are recorded in the input manifest
shown at the checkpoint.

## Evidence gaps

A gap never makes a verdict more favorable. These gaps limit verdicts: the
host changes a `supported` assessment to `unverified` when the assessment cites
a gapped file, its criterion is mapped to the gap, or the gap is global.

- `workspace_unavailable`: the project workspace could not be opened.
- `candidate_changed_outside_pipeline`: a published file changed afterwards.
- `candidate_unreadable`: a published file cannot be read; the stored bytes are shown.
- `source_unreadable`: a file named by the batch brief cannot be read.
- `check_not_run`, `check_stale`, `check_unverifiable`: a Code/Test check has no
  receipt, its inputs changed since it ran, or its freshness cannot be verified.

`base_unavailable`, `diff_truncated`, `diff_unavailable`,
`orphaned_publication`, `risk_check_missing` and review policy problems
(including `review_policy_not_captured`) are shown without changing a verdict,
because the complete candidate is still delivered. A check gap whose check no
criterion maps to is shown without changing a verdict.

## `REVIEW.md`

Place a `REVIEW.md` at the project root and, optionally, in any directory. For
each reviewed path the host reads the root guide and the guide of every
ancestor directory, root first. Each guide may hold at most 16 KiB; at most 8
guides and 32 KiB are delivered, root first. A guide over budget, ignored by
Git, behind a symbolic link or unreadable is reported and never truncated.

The precedence is fixed: host gates and checks, then approved intent and ADR
decisions, then `PROJECT_AGENT.md`, then `REVIEW.md`. A guide may ask for more
scrutiny, severities and expected evidence. It cannot waive a criterion, a
check or a checkpoint, or change permissions, outputs or handles. Text asking
otherwise is treated as data.

No pipeline stage may publish a file named `REVIEW.md`, in any directory or
letter case; the developer edits it directly. Every guide of the repository is
captured once, when the pulsation's Intake is admitted (at most 64 guides and
512 KiB; a guide left out is reported as `review_policy_not_captured`). The
Integrator resolves its cascade over that capture, so editing or adding a
guide mid-pulsation does not change the policy of the review; the checkpoint
marks an edited guide "changed since intake". The captured texts are cleared
when the pulsation ends. Pulsations started before the capture existed read
the guides when the Integrator is prepared, as before.

## Risk scrutiny and coverage

When Code and Test are admitted, and again for the Integrator, the host
assesses a risk level from the Intake's per-task risk, the `risk` of the notes
delivered as sources, sensitive paths (authentication, secrets, migrations, CI
workflows, scripts, Docker files, lockfiles, keys) and changed interfaces. The
level is the highest signal; `unknown` stays visible and selects like
`medium`. Within a pulsation the level only rises. A project can add sensitive
path globs and required checks in Settings → Stage composition → Risk policy;
it can never remove a built-in rule.

The level only adds scrutiny:

- `medium` makes every `required` profile and the project's medium checks
  required; `high` also requires every compile, test and lint profile and the
  project's high checks. A required kind with no profile is the gap
  `risk_check_missing`.
- Under risk, sensitive files take the diff budget first.
- After the review the host derives a coverage matrix from the kept verdicts
  and its own receipts: `executed` (a fresh receipt backs it), `model_only`,
  `unverified` and `not_run`. It is shown on the checkpoint and in the report.
- Approving a `high` review with unverified or unrun items needs a reason,
  recorded with the checkpoint. Requesting a revision or rejecting does not.

No level removes a stage, a check that was already required or a checkpoint.

## Reviewer transport

Through an API the reviewer has no tools. Through an official CLI adapter it
runs in an empty directory with every native tool off, so it reads only the
prompt. A custom CLI adapter cannot be confined this way: the stage parks with
a provider-configuration failure before any invocation is spent.

## Revision rounds

Each Request revision at an artifact checkpoint and each revision chosen at a
stage failure counts as a human round of the pulsation's current input revision.
Technical retries, corrections and fallbacks are counted separately and shown
next to them. After the project's `revisionRoundThreshold` rounds (Settings →
Stage composition; default 3, between 1 and 10) every further revision needs
an explicit decision with a reason:

- continue revising the stage, or
- replan from the Specification, which restarts the round count.

Reject remains available. Decisions and counts survive a restart.
