# Secret sanitization

The Rust host prepares a separate view of context before API requests, CLI stdin,
CLI workspace snapshots and embedding calls. `SecretRedactor` is the single,
cached detector. Its catalog covers recognizable provider tokens, sensitive
assignments, credential URLs, private keys and credential paths. UUIDs, public
certificates and Twilio key SIDs alone are not classified as secrets.

Final verification also covers model identifiers, schema names/content and
textual metadata, including embedding calls; source authorizations remain
confined to their verified prepared text slots.

This reduces accidental exposure within the covered catalog. It does not prove
that arbitrary credentials, encodings or obfuscated values are absent. Native
CLI tools remain available: a filtered snapshot is not an OS read sandbox.

## Local fixture authorizations

Open **Settings → Stage composition → Secret protection** for the active
project. Inspect an exact relative file path, select an occurrence, provide a
reason and authorize it as a synthetic fixture. Inspection shows categories and
byte ranges, never matched values. Existing authorizations can be removed and
become inactive when the file or detector changes.

An authorization binds the project, exact path, whole-file revision, detector
version and occurrence. It applies only to that source's prepared projection.
The same value in another file, user message, error or output remains protected.
Sensitive/ignored paths stay excluded. Repository instructions, comments and
model tools cannot create an authorization or disable protection.

## Editing and review

Original source bytes, source hashes and anchors remain local and unchanged.
A model sees a derived view with `<REDACTED_SECRET:CATEGORY_INDEX>` markers.
Anchor operations outside protected regions remain valid; operations intersecting
protected regions and whole-file replacements of protected sources are refused.
New placeholders cannot silently become published code. Resolve a blocked edit
locally and prepare it again; the host never reconstructs a secret by replacing a
model's placeholder.

Operational model replies remain private in memory for exact parsing. Activity,
diagnostics, review previews, clipboard and evidence exports use sanitized
projections. Review labels distinguish the view from the original approved
artifact. Original artifact identifiers and hashes continue to identify the
operational artifact, not the displayed bytes.

JSON evidence exports carry `originalEvidenceSha256` and
`sanitizedContentSha256` with an explicit `hashScope`. The latter hashes the
compact canonical data envelope before the sanitization metadata is appended;
it is not the pretty-printed file's checksum. HTML reports whose data changed
carry hidden metadata for the original and projected typed report, explicitly
excluding HTML bytes. These hashes identify complete evidence units, never
individual detected values.

## Output, persistence and recovery

Text waits until successful completion and sanitization; lifecycle, progress and
usage continue live. Incomplete streams, cancellation, output limits and failed
sanitization omit unqualified text. JSON is inspected after decoding, including
strings nested in strings and free object keys.

The host limits each textual unit to 32 MiB and detection to 65,536 candidate
regions, retaining smaller channel limits. Private-key headers without a closing
footer protect the remainder of the source. Original-source scanning precedes
excerpts. Bounded original serialization may occur privately for export evidence
hashes; only the sanitized derivative is exported. A truncated log cannot serve
as evidence of a complete scan.

SQLite V49 stores local authorizations, input receipts, source maps needed for
editing and publication guards without matched values or standalone secret
hashes. Operational originals may retain untouched source credentials. Historical
rows are requalified when read or exported, without destructive migration of
originals. Fragmented legacy streams that cannot be safely reconstructed are
omitted. Publication policy and hash evidence are checked again during recovery.
The viewer omits API token-delta events from legacy protocol v1 because text
already rendered in an earlier paint cannot be safely reassembled. It retains
structured status/usage metadata; complete history snapshots use the qualified
read projection.

Policy identity participates in reuse and resume. CLI snapshots also bind
whole-file original revisions. Embedding vectors qualify their policy version
separately from original note hashes and reindex through the existing worker.
Pulsation details show masked input/output occurrences, examined logical inputs
and applied fixture exceptions. Rechecks and retries retaining the same logical
call/execution ID, and history refreshes, do not manufacture new counts. A new
logical execution is a new examined input even if its text is identical.
Legacy observations without
qualifying evidence are not retrospectively counted as detected secrets.

## Validation

See the [dated validation report](../quality/secret-sanitization-validation-2026-10-22.md)
and [scanner measurements](../quality/secret-sanitization-benchmark-2026-10-22.json).
Tests use synthetic credentials, temporary profiles and loopback providers.
No live credentials or paid model requests are needed.

Any detector change must update its policy/version and positive/negative corpus.
Any new external context source must adopt and test the preparation boundary
before it is enabled.
