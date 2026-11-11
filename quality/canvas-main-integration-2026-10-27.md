# Native Canvas main integration

Date: 2026-10-27. The developer explicitly approved commit, main merge, push and
cleanup after reviewing the completed Canvas validation.

## Published implementation

- Feature commit: `54c6142169dc403231d5e5bba15f537bdfd2da87`.
- Main merge: `36b2b1251474cc493c2bb59bf46f4c59c714d1af`.
- Origin main was updated and its SHA was independently verified after push.
- The merge tree is `d1c35e8b3716c5b4969ebd8e4f8910de489e8a13`, exactly the
  qualified feature commit's tree. All 28 source files also matched the validated
  worktree through Git's clean filters and committed blob IDs.
- This follow-up changes integration documentation only; it does not change the
  executable sources, dependencies or migrations that passed validation.

The [validation report](canvas-basic-validation-2026-10-27.md),
[qualification record](canvas-qualification-2026-10-27.json) and
[complete gate record](canvas-stage7-run-2026-10-27.json) retain the evidence:
1,655 tests passed, zero failed, 16 existing ignored; two additional fixture tests
passed. All nine gate steps passed, including release, Windows smoke and technical
audits. Cargo audit reported zero vulnerabilities.

Release callback p95/p99 were 6.7732/7.1737 ms for 100 elements and
13.2121/14.8072 ms for 500. The larger scene did not meet the probe's stricter
8.33 ms p95 target; four of its 1,000 intervals exceeded 16.7 ms. These are GPUI
callback observations, not compositor FPS or input latency. See the
[performance record](canvas-performance-2026-10-27.json).

## Preexisting local work

The primary checkout contained 16 modified/new Whisper-related files before
integration. Literal backups and hashes were saved under the ignored local
`target/canvas-integration-2026-10-27/` directory before any preservation operation.
Only the four overlapping files were temporarily stashed: README, AppCore,
candidate capture and UI layout tests.

After the merge, those changes were reapplied and left unstaged. The sole conflict
was between comments around the same Windows permission adjustment. The local
Whisper comment and the new Canvas writable-file regression assertion were both
preserved. The 12 files outside that overlap remained byte-identical to their
backups. Whisper changes were not included in the published Canvas commits and
are outside this validation's scope.

The preservation stash was dropped only after restoration was checked. The local
backup, restoration journal, source verification and four final validation logs
remain in the ignored integration directory. No binary artifact was published;
the existing binary-distribution hold remains unchanged.

## Migration order and cleanup

Main now contains V55 Cargo and V57 Canvas. The Mandates worktree was not modified.
Its future unpublished migrations must use V58 or higher free versions; V56 cannot
be inserted after an applied V57. Existing migration identities and refinery's
history checks remain unchanged.

The Canvas worktree, its Cargo target directory and its local temporary branch
were removed after the verified main push. No remote feature branch was created.
Other worktrees and branches were preserved.

The five Canvas documentation files in Cerebro were committed locally only, as
explicitly requested. Its nine preexisting unpublished commits were not pushed.
