# Canvas AI authoring source publication — 2026-10-29

The selected Canvas source was committed and pushed to `Nathan-dOS/HARD-VC` main
as [`e7dfe4e10691c2e7101839f89a0917d227fc4c9b`](https://github.com/Nathan-dOS/HARD-VC/commit/e7dfe4e10691c2e7101839f89a0917d227fc4c9b).
Remote main was verified after the push. The maintainer approved this source
publication after reviewing the completed implementation and validation. No task
branch or worktree was created.

This publication includes the native Canvas refinements, consent-bound AI
authoring, semantic architecture/process editing and their supporting tests and
technical evidence. Pre-existing voice/Whisper changes were excluded from the
selected tree, preserved locally and restored after the source commit.

## Qualification of the published source

The publication candidate was isolated from unrelated local voice work before
validation. The qualified index tree was
`ce651c87810c7ceb0be4265db713c442b0390e97`, based on `92f6a0c`.
All **884 source SHA-256 values and Git blob identities** were verified against
the selected index and subsequently against the source commit, with **zero drift**.
The [qualification record](canvas-ai-publication-qualification-2026-10-29.json)
and [source fingerprint](canvas-ai-publication-source-fingerprint-2026-10-29.json)
identify this exact source selection.

The complete `cargo test-all` run passed **1,778 tests, zero failed, 16 existing
ignored and zero filtered**, across **55 target summaries**, in **2,023.458
seconds**. The adopted Stage 7 gate passed **all nine checks** in **2,933.376
seconds**, from 10:46:58 to 11:35:52 UTC on 2026-10-29. Exact timestamps remain in
the [gate record](canvas-ai-publication-stage7-run-2026-10-29.json).

| Check | Result | Duration (s) |
|---|---|---:|
| GPUI/Rust structure | PASS | 1.473 |
| Crate dependency boundaries | PASS | 0.291 |
| Formatting | PASS | 8.281 |
| Strict workspace/all-target Clippy | PASS | 75.208 |
| Complete workspace tests | PASS | 2,023.458 |
| Release application build | PASS | 806.453 |
| Windows GPUI startup/shutdown smoke | PASS | 4.951 |
| Technical license/source audit | PASS | 5.573 |
| RustSec advisory audit | PASS | 7.603 |

The native smoke closed gracefully with exit 0 and removed its isolated profile.
The qualified release executable's SHA-256 is
`24544db91b7a7f964af4fb33f447638d5de1ebef00341aea2431aab47eb8bbfa`.
That executable was used for qualification; no binary release was published.
See the [complete test record](canvas-ai-publication-test-results-2026-10-29.json).

## Earlier combined-checkout evidence

The [original workflow report](canvas-ai-workflow-validation-2026-10-28.md)
retains the earlier 1,782-test, 887-source qualification, which included preserved
local voice changes. Its counts, source hashes and release hash describe that
historical combined checkout. They are not the qualification of the selected
source commit. The separate 1,778-test/884-source run above qualifies the Canvas
selection published in `e7dfe4e`. The two runs overlap and must not be added.

## Preservation and scope

Before the documentation follow-up, the local restoration audit verified:

- 103 original files against literal backups, including line-ending restoration;
- all 887 combined-checkout source hashes after local work was restored;
- all 884 source blobs against the published commit;
- restoration of 16 paths covering pre-existing voice/Whisper work, including
  mixed files whose Canvas portions alone were selected for publication;
- removal of this task's temporary preservation stash after verification.

The restoration journal is local evidence under the ignored
`target/canvas-ai-publication-2026-10-29/` directory. It is not a prerequisite
shipped with the public source. Voice/Whisper source and documentation remain
local changes outside the published Canvas selection. The separate private
design repository is outside this HARD-VC push.

## Qualification limits and distribution policy

The runtime source is the completed persisted chat answer. The optional document
reference remains a pure contract; a document attachment/resolver workflow is not
exposed. API evidence uses loopback fixtures, and CLI evidence uses isolated
fixture executables. Authenticated providers, physical display/DPI/IME behavior
and dense generated-diagram performance remain unqualified. This is a restricted
typed BPMN profile, without a full conformance, XML interchange or execution claim.

The publication consists of source and technical evidence. It includes no release
binary, model weights, audio, installed runtime or build cache. The existing
binary-distribution hold remains unchanged; passing technical audits does not
replace that policy or its required legal decision. The
[implementation guide](../docs/canvas-ai-authoring.md) documents authority,
staging, publication, recovery and manual semantic editing.
