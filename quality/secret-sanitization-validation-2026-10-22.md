# Deterministic secret sanitization — implementation and validation

Date: 2026-10-22. Implementation baseline: `adcf49b`; local working tree.
Policy: `hvc-secret-projection-v1`; additive SQLite migration: V49.

This report qualifies the host-provided boundaries and compiled detector catalog.
It is not evidence of universal secret detection or an operating-system sandbox
for native CLI tools. No real credentials or paid model calls were used.

## Delivered behavior

- A single `SecretRedactor` scans original UTF-8 bytes, consolidates overlaps and
  projects deterministic markers while preserving LF/CRLF. Sources, anchors and
  original hashes remain unchanged. Incomplete private keys protect through EOF.
- Host-issued `PreparedModelInput` binds delivered text to its frozen policy and
  source-scoped authorizations. API, CLI stdin, CLI snapshots and embeddings use
  prepared views, with final transport verification and policy-aware reuse.
  Final verification covers model identifiers, schema names/content and textual
  metadata, including embedding calls; authorizations remain confined to the
  verified prepared text slots of their source.
- Edits intersecting protected source intervals are refused. Disjoint anchors
  retain exact original bytes elsewhere. New markers cannot silently become
  published artifacts; durable guards are checked during acceptance and replay.
- Observable response/log text is buffered until completion and sanitization.
  Operational replies remain private in memory for exact parsing. Incomplete,
  cancelled, failed or over-limit observations omit unqualified text.
- SQLite observations, legacy read projections, viewers, review/copy and report
  exports are sanitized. JSON is inspected after decoding, including object keys
  and nested JSON strings. Unrecoverable legacy fragmentation is omitted.
- Local fixture authorizations bind project, exact path, complete original file
  hash, detector/version and selected occurrence, with a reason. The settings UI
  permits inspection, authorization and revocation without displaying values.
  Source changes invalidate authorization; ignored/sensitive paths remain excluded.

## Coverage matrix

| Source / consumer | Boundary and storage/export | Executed synthetic evidence |
|---|---|---|
| Agent system/user prompts, Forge, Learning, Visualizer, Brownfield, history, feedback and previous attempts | Router / ProtocolAdapter; request records and API result observations | Captured loopback requests; stale preparation refused; encoded JSON and Unicode; SSE completion and limits |
| Companion API / CLI messages and tools | Whole-source projection before read/search excerpts; source-bound proof for tool results | Same fixture value in another file/message stays masked; revocation; private key across read window; tool-call growth limits |
| Pipeline/manual/FreeForm CLI input and repository snapshots | Adapter stdin and sanitized files with original revision + view + policy fingerprint | All backend preparations; captured snapshot; exact source unchanged; stale receipt/resume invalidation |
| Composition context and edits | Original-coordinate segments, anchors and persisted source maps | Secret before/inside/across anchors; disjoint edit allowed; overlap and new marker refused |
| CLI stdout/stderr | Complete run/stream before terminal events, history pagination and export | Fragmented tokens, UTF-8 and pauses; separate streams; cancellation/limits; old marker plus incomplete legacy fragments omitted |
| API output / SSE / structured data | Complete response before observer and SQLite; raw operational parse remains separate | Completion-required streams for supported protocols; decoded keys/values; raw edit literals preserved |
| Validation output and retry diagnostics | Complete bounded process observation before receipt/failure context | Full/escaped-JSON credentials masked; truncated or failed buffer omitted |
| Semantic corpus and query | Preparation before excerpt; separate embedding policy revision | Captured modern/legacy endpoint requests; policy change dirties index without changing original revision hash |
| Pending candidate publication and recovery | Atomic policy check, original/desired hash proof and durable publication guard | Restart replay, tampered guard, changed authorization/policy, uncertain legacy marker write |
| Evidence/review/clipboard and HTML/JSON reports | Sanitized derivative with original identity retained | Nested Unicode-escaped JSON; typed source immutability; original IDs/hashes; HTML escaping and CSP retained |
| Local authorizations and counters | SQLite metadata and host-only settings UI | Exact scope, stale file, revoke, retry deduplication, project/pulsation isolation, preview path/project generation fence |

Transport coverage is demonstrated with fake providers/executables, not with
every authenticated third-party CLI version. The sources in the first row also
share the final router boundary; this does not claim a separate end-to-end paid
model execution for every product flow.

The API viewer deliberately omits legacy protocol-v1 token deltas across paints;
it cannot safely reconstruct text that an old event might already have rendered.
Structured metadata remains visible. Complete historical snapshots are separately
requalified on read; omitting live legacy deltas does not rewrite those originals.

## Detector contract

Detector identifiers currently use version 1. `private_key`, `bearer_token`,
`jwt_token`, `known_api_token`, `sensitive_variable`, `credential_url` and
`credential_path` identify the consolidated region, not a unique credential.
Overlaps count once. Existing generated markers are idempotent; mixed arbitrary
text beside a marker does not gain an exemption.

The catalog includes the existing AI/Slack token formats, GitHub classic and
fine-grained token prefixes, documented GitLab prefixes, AWS access-key IDs,
Stripe keys, SendGrid, sensitive assignments/JSON/YAML, private PEM/OpenSSH keys,
password-bearing URLs and local credential paths. Prefix recognition has
conservative minimum lengths; it does not validate provider-issued credentials.
UUIDs, isolated public certificates and Twilio key SIDs are negatives. Twilio
secret values require sensitive assignment or authorization context. See the
[public behavior contract](../docs/secret-sanitization.md) and the official
[GitLab prefix table](https://docs.gitlab.com/security/tokens/#token-prefixes).

| Detector / version | Recognition bounds | Explicit negative cases |
|---|---|---|
| `private_key` / 1 | PEM/OpenSSH private-key header through matching footer or EOF | Public certificate, ordinary text |
| `bearer_token` / 1 | Case-insensitive Bearer context, at least 8 allowed token characters | Short label/example |
| `jwt_token` / 1 | `eyJ` header, bounded base64url-shaped header/payload; signature may be empty | Short or malformed non-JWT identifier |
| `known_api_token` / 1 | Documented prefixes; conservative minimum payloads: commonly 12, Google 20, GitLab cookie 16, SendGrid 16/24, AWS exactly 16 after prefix | Short prefix examples, UUID, Twilio SID, unrelated cookie name |
| `sensitive_variable` / 1 | Sensitive named assignment or JSON/YAML field; quoted values including spaces/escapes; YAML literal/folded block through dedent/EOF | Empty value, unrelated field, intact generated marker |
| `credential_url` / 1 | Password in scheme://user:password@host; retain nonsecret URL structure | URL without password |
| `credential_path` / 1 | Existing absolute private-key/credential file path patterns | SSH `.pub` path, ordinary project source path |

The positive/negative corpus is in `secret_redactor::tests`; decoded JSON
assignment context and aggregate output limits are qualified separately by
`llm::adapter::tests`. These bounds are recognition rules, not authoritative
provider credential validators. Custom GitLab prefixes need sensitive variable,
Bearer or another covered context; default-prefix detection cannot identify them.

## Limits and performance

Each logical text/response has a 32 MiB ceiling; existing lower limits remain.
The scanner permits at most 65,536 detection candidates. Exceeding a ceiling
fails before dispatch/export or emits only a neutral observational omission.
This candidate ceiling is conservative when several detectors overlap.

The [machine-readable measurements](secret-sanitization-benchmark-2026-10-22.json)
record five scanner samples per size and a sequential snapshot-shaped workload.
Measurements use a debug build and are not a release-performance guarantee or
full application memory profile. Functional tests have no latency thresholds.

| Input | Scanner p50 | Scanner p95 (5 samples) |
|---|---:|---:|
| 1 KiB | 0.183 ms | 0.261 ms |
| 64 KiB | 9.606 ms | 10.077 ms |
| 1 MiB | 151.651 ms | 152.529 ms |
| 32 MiB | 5,762.666 ms | 5,868.860 ms |

The 64 × 1 MiB sequential workload took 11.825 s. The probe's Windows process
peak working set was 109,998,080 bytes (about 104.9 MiB), sampled every 10 ms and
including the executable/runtime. These measurements were collected while
workspace tests were also running; they are coarse local cost evidence. The
snapshot-shaped workload measures sequential projection, not filesystem
materialization, and the 32 MiB case probes scanner/response size, not HTTP latency.

## Validation results

The final mandatory Stage 7 gate ran from `2026-10-22T23:12:35.6467792Z` to
`2026-10-22T23:30:58.6938031Z` against the frozen
[source fingerprint](secret-sanitization-source-fingerprint-2026-10-22.json) (53 changed/added crate files).
It executed the repository script without skipping advisories. Formatting,
strict Clippy, all Rust tests, release build and the isolated Windows GPUI smoke
test passed. The full run recorded **1,362 passed, 0 failed and
13 explicitly ignored tests** across 39 result targets. The four final
payload-metadata regressions also passed as targeted checks before this run.

| Gate step | Result | Duration |
|---|---|---:|
| estrutura exclusivamente GPUI/Rust | PASS | 0.750 s |
| cargo fmt | PASS | 5.658 s |
| Clippy | PASS | 26.657 s |
| testes Rust | PASS | 1049.468 s |
| build release GPUI | PASS | 11.684 s |
| smoke Windows GPUI | PASS | 1.473 s |
| licenças e fontes Rust | PASS | 1.853 s |
| advisories Rust | FAIL (existing advisories) | 5.327 s |

The **overall gate is not approved** because RustSec reports
9 existing dependency advisories. Dependency manifests and
`Cargo.lock` remain unchanged from `adcf49b`. The audit used database commit
`640bf5a9f06e66d8721e8772766531fe77c8630d`; the affected existing packages are
`quinn-proto`, `rustls`, `rustls-webpki` and `webpki-roots`. The
[dated advisory evidence](secret-sanitization-advisories-2026-10-22.json)
records individual advisories and informational warnings separately.

The license/source audit passed technically for 551 resolved packages.
Repository policy still blocks binary distribution pending the existing GPUI
upstream/legal disposition (`zed-55470`). Neither existing blocker is waived.

The [machine-readable checks](secret-sanitization-checks-2026-10-22.json)
record per-target test counts, gate results and durations, the release binary's
complete-file checksum and the qualified source fingerprint. Raw traces remain
in ignored local `target/` files; these aggregate artifacts are retained in
`quality/`. Source bytes were checked against the fingerprint after the gate.

## Adoption and remaining qualification limits

M0–M4 form the integrated protection increment; M5 adds the compiled catalog,
local exception management and counters. M6 records the actual checks above.
New sources from features 10/17/08 must use the same preparation boundary before
being enabled. Detector changes require a new policy/version and corpus update.

Original operational artifacts and historical evidence are not destructively
rewritten. A sanitized export's bytes do not share the original evidence hash;
review identifies the displayed projection and preserves the original identity.
Publication maps follow execution retention and remain while publication is
pending. Unknown encodings, arbitrary obfuscation, unrecognized formats, native
CLI file access and private repository corpora are outside this qualification.
