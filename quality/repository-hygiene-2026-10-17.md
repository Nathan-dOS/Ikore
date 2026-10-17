# Repository hygiene — 2026-10-17

Source baseline: `bee361b4a6ce059d6292cb9794b43ca358863793` (107 commits
reachable from HEAD). The existing history is preserved. This change is a
working-tree cleanup; it does not erase earlier versions from Git.

## Changes

- Archived all nine root planning documents in the private design archive.
  Each original was copied and compared by SHA-256 before removal. Existing
  canonical design documents were not overwritten.
- Archived the five files from `Interfaces/` and the unused `Icone-Logo.png`
  under the same procedure. No image from that archive was added to public
  documentation. Provenance review remains pending, including historical copies.
- Removed the seven-file baseline's personal checkout references: one planning
  document was archived, two Markdown references now use public relative links,
  and four JSON reports use `<repo>` for their checkout prefix.
- Added public architecture, testing and design summaries. Current README,
  composition-fixture comments, agent workflow and GPUI patch references now
  resolve within the repository. Sixteen dated reports explain their historical
  design citations through the public quality index. Their provenance remains.
- Expanded ignore rules for environment variants, local databases, editor/agent
  state, CI cache and archived prototype paths; `.env.example`, `Cargo.lock`
  and the shared `.agent/` skill remain eligible for version control.

## Validation scope

The hygiene checks verify archive checksums, absence of the original root
documents/prototype files, personal-path removal, local documentation links,
JSON parsing and preservation of original report data after the documented
path substitution. Git refs and the pre-existing untracked contribution guide
and Kanban document are compared with the captured baseline.

The only Rust change is three documentation-comment lines. No runtime logic,
dependency, migration, contract fixture or test changed. Runtime tests are not
repeated for this documentation cleanup. The previous full runtime results
remain in the [integration report](main-merge-validation-2026-10-17.md).

Executed hygiene checks:

- All 15 archived originals match their recorded SHA-256 and byte lengths,
  including the Git blobs staged for the private archive commit. Archive-specific
  attributes disable text normalization so future checkouts retain those bytes.
- No personal checkout prefix remains in current versioned files or the new
  public documentation.
- All 54 current versioned JSON files parse; the four sanitized reports match
  their original data after only the documented prefix substitution and note.
- All 178 checked local links and 27 Markdown heading targets resolve, including
  the private archives' new indexes.
- Recorded hexadecimal identifiers in the edited historical Markdown reports
  are unchanged. Before the hygiene commits, both repositories' HEADs and refs
  matched the captured baseline. The pre-existing untracked contribution guide
  and Kanban document retain their original bytes.
- `cargo fmt --all -- --check` and `git diff --check` pass. Ignore-rule probes
  retain the example environment, lockfile, shared skill and runtime icon.

## Publication limits

The earlier format-pattern scan covered 104 commits and reported synthetic
test values. It is not a new dedicated scan of this 107-commit baseline.
The planned full-history `gitleaks` check remains pending.

Existing commit hashes, author metadata and historical contents remain
accessible. Archiving assets outside the current tree does not resolve their
historical publication rights. License selection, dependency advisories,
distribution policy, CI setup and repository visibility are separate pending
work. This cleanup does not certify the repository for publication.
