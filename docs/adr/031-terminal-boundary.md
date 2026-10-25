# ADR-031: terminal subsystem and shared contracts

Date: 2026-10-23

Numbering: recorded as ADR-030 on 2026-10-23 and renumbered on 2026-10-24,
because ADR-030 is the secret sanitization decision. The 2026-10-23
validation report keeps its original label.

## Context

The CLI runtime, its SQL repository, provider adapters, process isolation and
subscription quota were implemented inside hvc-core. Moving that module without
separating its host dependencies would create a reverse dependency on core's
orchestration, workspace, sanitization and usage services.

The Google Rust Style Guide prompted this review. Its Chromium-specific flat
layout and small-crate rules are not a requirement for this Cargo application.
The extraction establishes an explicit responsibility boundary; compile-time
improvement must be measured rather than assumed.

## Decision

Introduce two internal packages. hvc-contracts owns shared values for roles,
execution targets, response formats, stage vocabulary, timeout policies, usage
and redaction results. hvc-terminal owns terminal services and repository,
adapters, discovery/probes, model catalogs, process management, stream auditing,
session recovery, observation and subscription quota. The dependency direction
is core -> terminal -> contracts, with an additional core -> contracts edge.

Keep orchestration, migrations, workspace access, global usage persistence and
sanitization authority in core. Application requests and prepared-input proofs
remain core types. Typed host capabilities supply the CLI subsystem with safe
projections, prepared snapshots and usage recorders; no global service registry
or runtime plugin loading is introduced. The existing application event bus
implements the terminal event sink.

The host rejects a prepared-input candidate of another concrete type or one
bound to different prompts. It returns data from the validated concrete proof,
never caller-supplied getters. Digest integrity does not establish authority.
The constructor permitting authorized ranges remains private to core and no
proof can be deserialized. Inputs without a proof receive normal redaction.
Workspace snapshots retain the original-content hashes, projected bytes and
policy identity used by the existing resume fingerprint.

Reexport shared types through their established core paths. Keep core facades
for service/repository/catalog constructors and for application-specific request
conversion. These facades delegate execution to the terminal package. Persisted
formats, error codes, migrations and user-facing behavior remain compatible.

The quota collectors feature is implemented by terminal and forwarded through
core, UI and app. Dependencies opt out of defaults so a production build with
credential collectors disabled actually excludes those collectors.

## Quality and validation

Workspace lints cover unwrap usage, private wildcard imports, documentation
Markdown and undocumented unsafe blocks. Tests can unwrap fixture setup;
production external failures must be returned with context. Existing GPUI
preludes, module facades and formatter defaults are retained. Safety comments
describe actual ownership, lifetimes, buffers and Windows concurrency contracts.
RAII guards warn when discarded without being retained.

Pure tests live with their owning package. Tests exercising real migrations,
host authorization, orchestration and the application boundary use core or the
integration harness, without terminal depending back on either package. The
Cargo graph check includes normal, build and development dependencies.

Validate default and disabled collector builds separately: the integration
harness enables collectors, so a workspace-wide no-default-features test alone
does not establish the disabled production graph. Preserve process-tree cleanup,
timeouts, bounded streams, redaction, usage deduplication and recovery behavior.
Record compile timings with equivalent package selection, concurrency and cache
conditions; do not equate fewer selected tests with a faster build.

Executed evidence and remaining platform/advisory limits are recorded in the
[2026-10-23 validation report](../../quality/rust-boundaries-validation-2026-10-23.md).

## Consequences

Terminal changes have their own ownership and test selection. Shared stage
vocabulary removes the former role -> orchestration -> persistence dependency.
The host boundary requires explicit adapters and compatibility facades. Those
adapters must preserve error propagation and authorization, and should remain
small. Internal events keep exhaustive matches; no blanket non-exhaustive,
boolean-to-enum or shared-ownership migration accompanies this extraction.

Storage, pipeline and context are not extracted by this decision.
