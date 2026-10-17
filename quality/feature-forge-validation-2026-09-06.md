# Feature Forge implementation and validation

Design references: [public summaries](../docs/adr/README.md); [historical archive context](README.md#historical-design-references).

Historical 2026-09-06 baseline. The advisory-only triage contract below was superseded by the explicitly confirmed V26 analysis authority on 2026-09-09; see [current synchronization evidence](cerebro-sync-validation-2026-09-09.md). Historical results remain unchanged.

## Baseline and scope

- Date: 2026-09-06.
- Code HEAD at start: `afac99605d81d4fe2afa377f3be767c992d7f4c6`, branch `gpui-migration`.
- Existing local UI, model-catalog and icon changes were preserved. Additional concurrent
  UI changes remained in the workspace; they were not reverted or claimed as Forge work.
- Tracked baseline patches were saved under the temporary directory
  `hvc-feature-forge-baseline-20260906-083504` before implementation.
- Starting schema V23; new schema V24. Legacy pipeline versions remain readable/executable.
- Accepted design: ADR-016 and Feature-Forge-Tasks-and-Batches-Implementation in the
  separate architecture repository. No commit or push was requested or performed.

## Delivered behavior

1. Global Novo separates multi-task backlog capture from feature conversation. Engine
   provides Tasks, Feature Forge, Pulsations and Vault. Conversion is explicit and
   preserves the original request. TaskRefine warnings remain local and advisory.
2. FeatureAgent is the eighth canonical configurable role. Its durable conversations,
   cancellation and startup recovery are independent of pulsations. The host gathers
   safe context, reuses Intake only for matching evidence and attaches observed source
   hashes. Messages/questions may be localized; generated artifacts remain English.
3. Forge exposes incubation, advisory four-filter triage, confirmed dependency graph,
   Current/Next/Horizon waves, foundation planning and delivery review. Priority uses
   impact × confidence ÷ effort with pending scores when ratings are absent. Current
   capacity defaults to three. Drag/drop has button and keyboard alternatives.
4. Task dispatch freezes revisions and approved feature scope into a hashed manifest.
   Dependencies must be done or included; a feature prerequisite can share the batch
   when every remaining approved task is selected. One six-stage pipeline handles the
   whole batch. Typed evidence references each task, criterion and output artifact.
5. Host integration completes tasks atomically after Integrator approval and publication
   reconciliation. Feature completion requires explicit developer acceptance. The UI
   exposes integrated evidence and navigation to the related pulsation. Failed work
   remains retryable after base reconciliation, preserving memberships and history.
6. V24 preserves constrained-table IDs, configuration versions, slots, indexes and
   history. Upgrade backups and foreign-key verification remain fail-closed. Approved
   planning notes use durable publication and preserve conflicting human content.

## Regression coverage

| Requirement | Executable coverage |
| --- | --- |
| Separate capture and explicit approval | `forge_batches::tests::task_capture_is_idempotent_and_does_not_enqueue_work`, `feature_approval_is_explicit_and_does_not_dispatch` |
| Greenfield and durable localized conversation | `feature_conversation_is_durable_idempotent_and_separate_from_execution` |
| Existing repository and cache invalidation | `existing_repository_intake_cache_is_reused_only_while_sources_match`, `unresolved_decisions_and_changed_sources_block_approval` |
| Generated language and question bound | `feature_agent_rejects_non_english_artifacts_and_excess_questions` |
| Task identity, conversion, cycles and capacity | `conversion_preserves_original_request_without_duplicate_work`, `approved_task_proposals_resolve_dependencies_and_rollback_cycles`, dependency/capacity tests |
| Mixed batch and immutable membership | `dispatch_freezes_mixed_sources_and_rejects_duplicate_reservations`, `stale_dispatch_and_tampered_manifest_are_rejected`, `feature_prerequisites_can_share_a_complete_atomic_batch` |
| Six stages and reviewed integration evidence | `complete_six_stage_batch_requires_approved_integration` |
| Failure, restart and late work | `failed_batch_requires_reconciliation_before_retry`, cancellation/restart/shutdown admission tests; existing serial publication and generation-fence suites |
| Upgrade and identity preservation | `serial_upgrade` V1/V9/V12/V20/V23 cases and `coordination_upgrade` |
| Official CLI transport and isolation | `feature_discovery_uses_four_official_cli_adapters_with_isolated_context_and_own_identity` |
| UI sizes and interaction | `layout_tests::forge_capture_separates_multiple_tasks_from_feature_discovery`, `forge_graph_waves_and_delivery_fit_supported_window_sizes` |
| Selection recovery | `navigation_restores_the_feature_and_forge_section_without_queueing_work` |

The targeted discovery suite found and corrected an unnecessary Intake call caused by
changed retrieval hints with identical evidence. The cache now ignores the retrieval
scope fingerprint while retaining actual evidence, extractor version and product scope.

## Executed validation

All final commands completed successfully:

```powershell
$env:RUST_TEST_THREADS='1'
cargo test --workspace --locked --offline --no-fail-fast
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
cargo build -p hvc-app --locked --offline
```

- Serial workspace suite: **499 passed, 0 failed, 0 ignored**, including 338 core
  tests, 88 UI tests, 21 Forge regressions, the populated V23→V24 upgrade and four
  official CLI-adapter fixtures. The remaining tests cover editor, orchestration,
  providers, recovery and supporting fixtures.
- Strict Clippy, formatting verification and application debug build: passed.
- Native application output: `target/debug/hard-vibe-code.exe`.
- Code and documentation `git diff --check`: passed. The new architecture and quality
  documents have no missing wiki-link targets.
- The existing `proc-macro-error2` future-compatibility notice remains informational.

An earlier full run hit the existing M5 benchmark's exact 128-file enumeration
assertion while validation builds were also active. Its isolated retry passed, then
this final complete run passed without concurrent validation compilation. The assertion
and production context budget were preserved; this report does not hide that intermittent
failure. Local raw logs are `quality/feature-forge-tests.log`,
`quality/feature-forge-m5-recheck.log`, `quality/feature-forge-clippy.log`,
`quality/feature-forge-fmt.log` and `quality/feature-forge-build.log`.

The build includes the current shared workspace. Scoped lint annotations preserve the
existing concurrent native-widget helper signatures; Forge does not redefine those APIs.

## Evidence boundaries

- Provider tests use deterministic simulated gateways and local transport fixtures.
  CLI adapters execute real child processes backed by fixture executables for Codex,
  Claude, Gemini and OpenCode; this is not an authenticated vendor-model smoke test.
- GPUI layout/interaction tests render all five Forge sections and capture controls at
  960×640 and 1440×900. They check geometry, scrolling and selection. These are automated
  rendered checks, not a claim of manual screenshot, physical HiDPI or screen-reader QA.
- Existing host tests cover late generations, cancellation, uncertain writes, publication
  recovery and integration rollback; they run together with the new batch regressions.
- Planning publication means approved scope, not delivered product functionality.
