# Pulsation attestations

Every pulsation that ends gets a signed local record of what it applied and
under which approvals, checks and models. This page describes the format, the
key, how the app checks a record and how to check one without the app. The
decision is [ADR-036](adr/036-pulsation-attestation.md).

## What it shows and what it does not

A valid signature shows that the statement has not changed since it was sealed
and which local key sealed it. It does not show who the developer is, that the
time is trusted, that the build was hermetic, or that no other process running
as the same user touched the machine. No SLSA level is claimed.

## Format

The statement is an [in-toto Statement v1](https://github.com/in-toto/attestation/blob/main/spec/v1/statement.md)
in a [DSSE v1](https://github.com/secure-systems-lab/dsse/blob/master/envelope.md)
envelope, signed with Ed25519.

- `_type`: `https://in-toto.io/Statement/v1`.
- `subject`: one entry per path the pulsation's main publications applied,
  with the SHA-256 of the applied bytes. When a path was published twice, the
  last applied publication counts. Subjects are ordered by lower-cased path.
- `predicateType`: `urn:hvc:pulsation-attestation:v1`.
- `predicate`: the blocks below. They hold identifiers, digests, enumerated
  states, relative paths and integers only. File content, prompts, stage
  documents, reason text, feature names, absolute paths and credentials are
  never included; a reason is kept as its SHA-256. Relative paths are kept as
  they are, so a stage note named after the feature (for example
  `features/token-expiry.md`) carries that name in its path.

| Block | What it records |
|---|---|
| `pulsation` | id, project id, pipeline version, outcome (`done`, `failed`, `rejected`, `aborted`), terminal reason, creation and end times |
| `builder` | app version, optional build id, OS, architecture, the key that sealed it |
| `intent` | task revisions with the digest of each draft, digests of the accepted Specification and Contract, the `REVIEW.md` snapshot digest, pre-flight findings |
| `authority` | mandate revisions (number, cause, digest) and checkpoint receipts (purpose, stage, decision, digest) |
| `execution` | per stage execution: generation, plan digest, recipe version, policy digest, status, budget; per attempt: executor, transport, backend, effective and observed model, dispatch, input and result digests, state |
| `evidence` | host check receipts, the Integrator manifest digest and gap count, the coverage summary and matrix digest, risk assessments, acknowledgements |
| `sanitization` | policy identities, total redactions, how many inputs and outputs redacted each category |
| `effects` | per subject: base state and digest, applied digest, artifact id, producing stage |
| `conformance` | whether the run stayed within an enforced mandate, enforcement events, scope grants |
| `usage` | token totals per provider and observed model, and whether they are complete; an unknown counter stays unknown |
| `sealing` | sealing time (local clock), deriver version |

The bytes signed are the canonical JSON of the statement: object keys sorted,
no insignificant whitespace, UTF-8. The DSSE signature covers the
pre-authentication encoding

```
DSSEv1 28 application/vnd.in-toto+json <byte length> <statement bytes>
```

The envelope's `keyid` is `hvc-ed25519:` followed by the SHA-256, in hex, of
the 32-byte public key.

## Sealing

The terminal transition records a seal intent in the same transaction. The
background lane seals it once none of the pulsation's main publications is
pending or being applied, so a restart in between only delays the seal. It
derives the statement, signs it and stores it in one database transaction.

- With no key yet, the first seal creates one.
- When the credential store cannot be read or written, or the key it holds is
  missing or not the registered one, the statement is stored `unsigned`. The
  card offers "Sign now"; a missing or replaced key is never replaced without
  "Create a new key" in Settings → Security.
- A statement over 1 MiB is stored as its digest only (`too_large`), never
  truncated.
- A derivation the host cannot complete is kept `failed` with its error; the
  card offers "Seal again".
- Settings → Security → "Seal pulsations that end" turns sealing off for
  pulsations that end from then on. Nothing is sealed retroactively, including
  pulsations that ended before the upgrade.

A stored attestation never changes, except that an unsigned one may be signed
later with every other column kept; a database trigger refuses anything else.
Attestations are deleted with their pulsation or project. Keys are global and a
key an attestation names cannot be deleted.

## The key

One Ed25519 key per installation. The private key, PKCS#8 in base64, lives in
the operating system's credential store (Windows Credential Manager, service
`ikore.runtime`, entry `attestation-signing-key-v1`). A key stored by a build
from before the rename to Ikore, under `hard-vibe-code.runtime`, moves to the
new service the first time it is read. The database
keeps only public keys, so a retired key keeps verifying what it signed.
"Create a new key" retires the active key and asks for confirmation first.
Private key material never reaches the database, logs, events, reports,
exports or models.

## Checking a record in the app

"Verify again" on the pulsation's Attestation card checks three things:

1. **Signature**: `valid`, `invalid`, `unsigned`, `unknown_key` or
   `too_large`.
2. **Re-derivation**: the deriver of the recorded version runs on the current
   rows and is compared with the signed statement: `matches`, `differs` (with
   the first block that differs) or `unavailable`.
3. **Workspace**: each subject's file now, `unchanged`, `modified` ("changed
   since the attestation"), `missing` or `unreadable`. This level informs; a
   change is never called tampering or attributed to anyone.

The last check is kept for display only. The pulsation report's Provenance
section repeats the state, key, digest and subjects with their workspace state.

## Saving and checking a record elsewhere

"Save attestation…" writes `<pulsation id>.intoto.jsonl` (one envelope per
line) and, next to it, `hvc-attestation-key-<first 8 hex of the key id>.pub.pem`.
"Copy commit trailer" copies

```
HVC-Pulsation: <pulsation id>
HVC-Attestation: sha256:<statement digest>
```

Nothing is written into the repository. Settings → Security → "Verify a file…"
checks a bundle against this machine's keys, or against the public key file
chosen with it, and says whether the key is known on this machine.

### With OpenSSL 3

The extraction below needs only Python 3's standard library. It writes the
pre-authentication encoding and the raw signature of the first envelope:

```python
import base64, json, sys

envelope = json.loads(open(sys.argv[1], encoding="utf-8").readline())
payload = base64.b64decode(envelope["payload"])
kind = envelope["payloadType"].encode()
pae = b"DSSEv1 %d %s %d %s" % (len(kind), kind, len(payload), payload)
open("pae.bin", "wb").write(pae)
open("sig.bin", "wb").write(base64.b64decode(envelope["signatures"][0]["sig"]))
open("statement.json", "wb").write(payload)
print(envelope["signatures"][0]["keyid"])
```

Then verify the signature with the saved public key:

```bash
openssl pkeyutl -verify -pubin -inkey hvc-attestation-key-0123abcd.pub.pem -rawin -in pae.bin -sigfile sig.bin
```

OpenSSL prints `Signature Verified Successfully`. The key id printed by the
script should be `hvc-ed25519:` followed by the SHA-256 of the public key's
last 32 bytes:

```bash
openssl pkey -pubin -in hvc-attestation-key-0123abcd.pub.pem -outform DER | tail -c 32 | sha256sum
```

`statement.json` is the signed statement; its SHA-256 is the digest in the
commit trailer.

### Test vector

`crates/hvc-core/src/attestation/fixtures/` holds a vector signed with a fixed
test seed that exists only in unit tests: `statement.json`, its digest
`statement.sha256`, the envelope `vector.intoto.jsonl`, the public key
`vector.pub.pem`, and the `pae.bin` and `sig.bin` the script above produces.
From that folder:

```bash
openssl pkeyutl -verify -pubin -inkey vector.pub.pem -rawin -in pae.bin -sigfile sig.bin
```

The unit test `the_test_vector_is_stable` fails when any of these bytes would
change; `HVC_UPDATE_GOLDEN=1` rewrites them after the change was reviewed.
