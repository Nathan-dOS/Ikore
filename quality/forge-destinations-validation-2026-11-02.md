# Forge automatic destinations and batch admission — 2026-11-02

Status: implemented and validated for the debug application and the complete
default Windows workspace suite. No release/distribution qualification is claimed.

## Reported behavior and cause

A developer-selected batch containing one Feature Agent task failed with
`Batch exceeds the context budget`. The composer also requested explicit file
paths even though the project already had a registered repository root.
Earlier approval changes appeared to require an application restart.

The dispatch guard counted the complete serialized manifest, including every
draft in the feature brief, against half the global context setting. With the
default 16,000-character setting, that meant an 8,000-character manifest limit.
Selecting fewer tasks retained the whole feature backlog in that measurement.
Stage prompts already omitted those duplicated drafts. Legacy handoff
accounting still charged them, and composed dispatch did not use its larger
existing prompt capacity. Approval itself does not certify dispatch capacity.

The path field concerned destination files relative to the workspace, not the
registered repository directory. Requiring routine product-planning tasks to
name files in their prose caused avoidable clarification and approval cycles.

## Implementation and boundaries

- A bounded host resolver reads repository inventory, supported manifests and
  approved scope. Explicit task paths and optional manual overrides take
  precedence over conventional existing or proposed file candidates. Unknown
  implementation stacks receive `docs/architecture.md` and an uncertainty
  notice. The resolver performs no model call and creates no files.
- The immutable manifest freezes `taskDestinations` (`taskId`, `paths`,
  `origin`: `manual`, `task_text`, `repository_convention`) and
  `destinationNotices`. Approved task drafts and feature revisions remain
  unchanged. Stage input collection propagates candidates through the
  existing source/planned-destination distinction. Candidates grant no
  publication or mandate authority.
- Inventory is bounded to 1,000 entries and depth eight; manifest content is
  bounded to 32 KiB. Each task receives one to eight paths, with at most 256
  path entries per batch. A truncated tree does not establish an empty
  repository; greenfield detection does not depend on old retrieval coverage.
- Overrides must name files within the workspace. Roots, directories,
  traversal, protected/private/generated paths and symbolic links or Windows
  reparse points are rejected, including a future leaf behind a linked parent.
- Dispatch and legacy handoffs share the prompt's manifest projection. Only
  duplicated `features[].brief.tasks` drafts are omitted; selected tasks,
  criteria and full approved scope remain. Admission counts Unicode
  characters and reserves half the applicable capacity: global context for
  v3; `max(64000, 4 * context_chars_limit)` for v4. The existing error prefix
  remains compatible and diagnostics report required/allowed amounts.
- The composer explains automatic resolution and retains optional file
  overrides. It no longer sends routine file-path questions to Feature Agent.
  The runtime clarification API remains compatible. Approval and wave changes
  have an explicit regression scenario for refreshing ready tasks without a
  restart.

Older manifests default the new arrays to empty and retain the preventive
path checkpoint behavior. Existing execution history, developer approval,
dispatch, Specification review and publication checks retain their authority.
The production project and its database are not test fixtures.

## Regression matrix

| Layer | Scenario | Source |
|---|---|---|
| Unit | Static greenfield, planning documents, unknown-stack notice, Next/React/Node/Rust conventions, existing relevant files, manifest-only TypeScript and Rust workspace roots | `feature_forge/destinations_tests.rs` |
| Boundary | Empty/truncated inventories, optional override precedence, roots/directories/traversal/private/policy paths and symlink/reparse containment | Destination tests and dispatch regressions |
| Unit/boundary | Unselected backlog omitted from measurement; complete approved scope/criteria preserved; exact Unicode limits; v3/v4 capacities; required overflow rejected | `feature_forge/batch_budget_tests.rs` |
| Integration | Frozen candidates and notices, unchanged approved drafts/revisions, replay, older manifest defaults and handoff accounting | Forge dispatch and orchestration tests |
| Pipeline integration | Candidate propagation as existing sources or proposed destinations; existing preventive checkpoints for legacy manifests | `composition_path_confirmation` |
| Native UI | Automatic guidance, optional file overrides, absence of routine path-request action and approval/wave refresh without restart | `forge::tests` |

The source inventory includes 31 Windows destination cases (plus one Unix-only
symlink case), six budget/handoff cases, four path-confirmation integrations,
two Rust multi-binary repository-reader cases, two native UI cases and one JSX
Code/Test ownership case. Bootstrap cases cover manifest-only Rust, custom
Cargo entrypoints, React, Next, Node and standalone Node initialization.
Existing related modules receive conventional test destinations, including
adjacent test/spec and `__tests__` layouts. These are source counts, not passing
results; execution results belong in the following table.

## Execution

| Check | Result |
|---|---|
| Destination unit and boundary regressions | Passed: 31 Windows cases; the Unix-only symlink case is outside this platform |
| Batch budget and legacy handoff regressions | Passed: six cases |
| Forge dispatch/replay/compatibility integration | Passed in the 1,165-test core binary |
| Composed pipeline destination propagation | Passed: four path-confirmation integrations; six-stage pipeline and scope/mandate integrations also passed |
| Native UI regressions | Passed: both new cases, within 416 passing UI tests |
| `cargo fmt --all -- --check` | Passed |
| `scripts/check-crate-boundaries.ps1` | Passed |
| Strict workspace/all-target Clippy | Passed with `-D warnings` (55.51 seconds) |
| Complete workspace suite, including integrations/examples/doctests | 2,059 passed; zero failed; 23 optional tests ignored; zero filtered; 61 target summaries |
| Application debug build | Passed in 1 minute 49 seconds |
| Isolated native Windows startup/close smoke | Passed: actual GPUI window, graceful shutdown and exit code 0 |

The machine-readable [validation record](forge-destinations-validation-2026-11-02.json)
records totals, source hashes and binary identity. Final logs are
`target/forge-destinations-workspace-final-verified.log`,
`target/forge-destinations-inventory-verified.log`,
`target/forge-destinations-clippy-verified.log`,
`target/forge-destinations-fmt.log`, `target/forge-destinations-boundaries.log`,
`target/forge-destinations-build.log` and `target/forge-destinations-smoke-final.log`.
The final debug binary is 111,133,184 bytes with SHA-256
`7c4aed7ab1664845668f48dbdfc28b4fa57501a0ea6b4d0be60d034d9db6c58c`.
No production project or database was changed. Existing local icon and source
comment edits were preserved and excluded from the task changes; validation
used that working checkout. No worktree was created. Authenticated provider
quality and release/distribution remain outside this qualification. Existing
incremental-cache and proc-macro-error2 future-compatibility warnings remain.

## Preliminary runs

- The new empty-repository dispatch regression first failed at the old explicit
  path guard, reproducing the reported failure before integration.
- The first broad core attempt ran under the restricted sandbox. Loopback
  connections failed with Windows error 10013; it was interrupted and is not
  counted as a passing check. The elevated workspace run subsequently passed
  those Canvas and API fixture cases.
- The first elevated workspace run stopped at the core binary: 1,164 passed,
  one failed and seven ignored. The failing resolution fixture had dropped
  its temporary workspace before dispatch. Retaining its TempDir fixes the
  fixture without weakening destination validation. The final complete rerun passed; the preliminary failure remains recorded here.
- A preliminary copied integration executable exposed an outdated exact-path
  assertion: the inferred companion `tests/handler.rs` must accompany the
  existing module. The assertion now verifies both existing and planned refs;
  the four-test integration target passed in 6.15 seconds. A compilation-only
  rerun was interrupted to apply this fixture correction before executing
  another full suite. Both preliminary native UI regressions passed. Temporary
  executable copies were removed; their logs remain.
- Initial strict Clippy found a cloned-reference slice in a new test; the
  fixture was corrected. The final strict run passed.

- The first full run after the lifetime fix passed core and earlier integrations,
  then failed prompt inventory: the host file-candidate rows matched a generic
  agent-task prefix. A narrow host-row grammar and two negative/positive tests
  corrected only the test-support classification, preserving production prompt
  trust labels. Six inventory tests then passed, including the complete run.
- The old smoke selected a debug console and reported CTRL_C termination.
  The smoke now selects the vendored GPUI `Zed::Window` class for the child PID
  and requires exit code zero. The updated binary then passed. Isolated profiles
  and preliminary executable copies were removed; logs remain.
