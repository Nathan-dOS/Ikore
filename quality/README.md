# Quality evidence

This directory contains dated implementation reports, benchmarks, threat models
and planning documents. A plan is not an implementation report. A passing
historical check applies to the source and environment identified in that report.

## Starting points

| Topic | Public reference |
|---|---|
| Architecture and design | [Overview](../docs/architecture.md), [design summaries](../docs/adr/README.md) |
| Test commands and isolation | [Testing guide](../docs/testing.md) |
| Latest integrated baseline | [2026-10-17 merge](main-merge-validation-2026-10-17.md) |
| Contracts and terminal boundaries | [2026-10-23 implementation and validation](rust-boundaries-validation-2026-10-23.md), [test inventory](rust-boundaries-tests-2026-10-23.json) |
| Test workflow | [2026-10-15 validation](test-workflow-validation-2026-10-15.md) |
| CLI process boundary | [Threat model](cli-threat-model.md) |
| Synchronization and human recovery | [Synchronization report](cerebro-sync-validation-2026-09-09.md) |
| Context telemetry and Learning | [Validation](learning-context-optimization.md) |
| Deterministic secret sanitization | [2026-10-22 implementation and validation](secret-sanitization-validation-2026-10-22.md) |
| Integrator review boundary and revision rounds | [2026-10-24 implementation and validation](review-policy-validation-2026-10-24.md) |
| Dependency advisories | [2026-10-24 lockfile remediation and validation](dependency-advisories-2026-10-24.md) |
| Review corrections (editor revisions, check output, quota client, voice setup) | [2026-10-25 implementation and validation](review-corrections-2026-10-25.md) |
| Execution mandates and approval receipts (ADR-035 M1) | [2026-10-26 implementation and validation](mandates-attestation-m1-validation-2026-10-26.md) |
| Trust layers (ADR-035 M2) | [2026-10-26 implementation and validation](mandates-attestation-m2-validation-2026-10-26.md) |
| Signed local attestation (ADR-036 M3) | [2026-10-26 implementation and validation](mandates-attestation-m3-validation-2026-10-26.md) |
| Just-in-time scope grants (ADR-035 M4) | [2026-10-29 implementation and validation](mandates-attestation-m4-validation-2026-10-29.md) |
| Feature 19 closing: threat model, governance metrics, visual pass (ADR-035, ADR-036 M5) | [2026-10-29 closing validation](mandates-attestation-m5-validation-2026-10-29.md) |
| Repository hygiene | [2026-10-17 change record](repository-hygiene-2026-10-17.md) |

## Historical design references

"Cérebro" / "Cerebro" names the maintainers' private design archive. Old report
titles, document identifiers and commit hashes retain that provenance. Those
references are historical citations, not prerequisites for building, testing or
contributing. Public explanations are in [docs/](../docs/architecture.md) and
[docs/adr/](../docs/adr/README.md); the private archive is not required.

Reports covering navigation/input/Forge refer to ADR-014–019 and companion
design notes. Start with the [navigation summary](../docs/adr/README.md#adr-015-navigation)
and [Forge authority](../docs/adr/README.md#adr-016-forge-analysis-authority),
then follow the local dated reports for executed evidence.

## Paths and integrity

During the 2026-10-17 hygiene pass, personal checkout prefixes in reports were
replaced with `<repo>` or local relative links. `<repo>/target/...` identifies a
path relative to the checkout used for the original validation; it does not
claim that temporary evidence is shipped in Git. Substitute your checkout when
reusing a recorded command. Machine-readable reports carry a `pathSanitization`
note where string values were changed.

Recorded source hashes, Git blob IDs, commit IDs, commands' outcomes, timings
and validation dates were retained. Edited report files naturally have different
file hashes; use the original commits when exact original report bytes matter.

Deleting current files does not remove their older versions from Git. The
hygiene change preserves the existing history, including earlier personal paths
and archived images. Their historical publication review remains pending.
