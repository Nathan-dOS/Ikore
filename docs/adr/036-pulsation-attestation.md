# ADR-036: signed local pulsation attestation

Date: 2026-10-26

Status: accepted and implemented (M3, migration V56). Validation:
`quality/mandates-attestation-m3-validation-2026-10-26.md`. Format, key and
verification steps: [attestations](../attestation.md).

Implementation notes from M3: sealing runs from the durable intent alone; the
background lane picks the oldest pending intent whose publications settled, so
there is no separate job row for recovery to close. The key sits behind the
existing credential-store abstraction, which tests replace with a memory store.
A derivation the host cannot complete is kept `failed` with its error until
"Seal again". A re-derivation reuses the builder and sealing time the statement
recorded, so the same rows give the same bytes on a later build.

## Context

After a pulsation the host holds strong local evidence:

- receipts for every host check;
- effective and observed model identity;
- the Integrator manifest, coverage matrix and risk assessments;
- each published path's base and applied digests.

None of it leaves the application in a verifiable form, and the pulsation
report is a derived view, not a record. At the same time there is no hermetic
build, no operating-system sandbox and no trusted clock, and a secret in the
keyring is readable by any process of the same user. The attestation must not
claim more than that.

## Decision

**Format.** An in-toto Statement v1 in a DSSE v1 envelope, signed with Ed25519,
with predicate type `urn:hvc:pulsation-attestation:v1`. Subjects are the applied
paths with their published SHA-256. No SLSA level is claimed.

**Content.** The predicate binds the pulsation and builder, the approved intent,
the mandate revisions and approval receipts (ADR-035), stage executions with
attempts and models, host receipts, manifest, coverage, risk and
acknowledgements, sanitization counters, per-path effects, mandate conformance
and usage. It holds only identifiers, digests, enumerated states, relative paths
and integers. It never holds file contents, prompts, stage documents, free text,
absolute paths or credentials.

**Keys.** One Ed25519 key per installation, through `ring` 0.17. `ring`,
`base64` and `zeroize` are already in the dependency graph through rustls, so
the graph does not change. The PKCS#8 key is stored in the operating system
keyring (Windows Credential Manager), and public keys are kept in SQLite. Keys
are rotated from Settings, and retired keys keep verifying. If the keyring is
unavailable, the attestation stays `unsigned` and can be signed later. Private
key material never reaches the database, logs, events, reports, exports or
models.

**Sealing.** Every pulsation that reaches `done` or `failed` gets a durable
intent in its terminal transition. The background lane waits for pending
publications, derives the statement and stores an immutable row in one
transaction. Statements
over 1 MiB are recorded as `too_large`, never truncated. Sealing is on by
default and can be turned off in Settings.

**Verification.** There are three levels:

- the signature;
- re-derivation from the current rows, with versioned derivers kept forever;
- the current workspace state of each subject.

The workspace level is informational and never claims tampering or
authorship.

**Export.** The envelope is exported as `.intoto.jsonl` and the public key as
`.pub.pem`, with external verification through OpenSSL 3. The application also
offers a copyable commit trailer (`HVC-Pulsation`, `HVC-Attestation`). Nothing
is written into the repository.

## Consequences

The signature proves integrity since sealing and which local key sealed the
statement. It does not prove a person's identity, a trusted time, a hermetic
build or the absence of a malicious process running with the user's
privileges; that last point is an accepted residual risk. The migration is
additive. Attestations are deleted with their pulsation or project, while keys
are global. Every predicate change needs a new schema and deriver version.
