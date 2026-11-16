# Threat model — runtime CLI multiagente

Design references: [public summaries](../docs/adr/README.md); [historical archive context](README.md#historical-design-references).

**Revisão:** 2026-09-03  
**Escopo:** Codex, Claude Code, Gemini CLI, OpenCode, Antigravity CLI e Custom no Windows x64<br>
**Estado:** implementado e revisado para o gate CLI-G3

## Fronteiras de confiança

- O workspace, seus arquivos, configurações de fornecedor e instruções são entrada não confiável.
- A saída da CLI também é entrada não confiável, mesmo quando o executável foi descoberto como oficial.
- O banco local e o event bus só podem receber conteúdo depois de sanitização e redaction.
- O keyring é a única fronteira autorizada para segredos de API; o painel Agents nunca persiste a chave em configuração ou histórico. Única exceção, definida no ADR-028: o host pode ler, somente em memória e sob as garantias G1–G10, o token de acesso que a própria CLI de assinatura (Claude Code, Codex) guarda no seu diretório padrão, para consultar o uso do plano. Veja "Plan-quota meter and CLI credential reads" abaixo.
- Custom é código local explicitamente confiado pelo usuário, mas continua limitado a executável direto e snapshot seguro.

## Ameaças e controles

| Ameaça | Impacto | Controle implementado | Evidência automatizada |
|---|---|---|---|
| Prompt injection e instruções de fornecedor no workspace | leitura indevida, mudança de comportamento | snapshot temporário read-only produzido pelo host; `.codex`, `.claude`, `.gemini`, `.opencode`, MCP/hooks e arquivos sensíveis são excluídos | `safe_cli_workspace_excludes_secrets_ignored_files_and_vendor_configuration` e testes de `RepositoryContext` |
| Leitura de `.env`, chaves, certificados, ignorados ou escape da raiz | exfiltração local | regras centralizadas de contexto, canonicalização, exclusão de segredos/ignorados/binários e bloqueio de symlink/junction que escape da raiz | corpus de repository context e snapshot seguro |
| Segredo em stdout/stderr, erro, banco, event bus ou viewer | exposição em repouso ou na tela | `SecretRedactor` roda no coordenador antes de persistir/publicar; API viewer redige antes de armazenar no modelo; só contagem e categorias são mantidas | corpus de redaction, round-trip SQLite e `api_activity_is_project_scoped_redacted_and_bounded` |
| Command injection por prompt, modelo ou argumentos | execução arbitrária | `Command` recebe executável e `Vec<String>` separados; prompt somente por stdin; limites/NUL/reservados para Custom; nenhuma string de shell é composta | testes de argv/stdin dos adapters e executor-fixture |
| Shim npm adulterado ou wrapper arbitrário | execução de payload diferente | `.cmd/.bat` oficial é aceito somente se pequeno, contiver forwarding esperado e apontar para entrypoint conhecido; lançamento é convertido para `node.exe + entrypoint`; Custom rejeita wrappers | `trusted_npm_shim_resolves_to_node_and_tampering_is_rejected` e `custom_rejects_shell_scripts` |
| Troca do executável após configuração | TOCTOU/payload substituído | canonicalização e SHA-256 no launch; descoberta é invalidada por mudança de identidade; alteração Custom zera confirmação de risco | `custom_identity_change_requires_a_fresh_explicit_acknowledgement` |
| Processo filho/neto sobrevivente | consumo de recurso e execução fora do app | Job Object do Windows com `KILL_ON_JOB_CLOSE`, cancel token, deadline e espera de encerramento | matriz `cli_executor`, incluindo árvore de processos |
| Probe preso ou ruidoso | DoS e processo órfão | timeout, `kill_on_drop`, stdin nulo e limites estritos de stdout/stderr | `probe_reader_aborts_when_output_exceeds_the_bound` e matriz de discovery |
| Sessão retomada no contexto errado | vazamento entre projeto/agente | resume somente por ID explícito; fencing inclui projeto, slot, backend, modelo, snapshot do workspace e sessão nativa | `explicit_resume_fencing_rejects_workspace_backend_model_and_session_changes` e fixtures de resume |
| Configuração do usuário enfraquecer permissões | escrita não autorizada | adapters oficiais fixam modo não interativo/read-only; versão exige semver e faixa explícita por backend; capacidade desconhecida falha fechada; argumentos reservados não podem ser sobrescritos | `semantic_compatibility_is_backend_specific_and_fails_closed`, testes por adapter e matriz de versão/capacidade |
| Saída hostil ou ilimitada | escape visual, banco infinito, travamento | remoção de ANSI/OSC/controles, frame de 64 KiB, buffer de 256 KiB, 32 MiB/100.000 eventos por run e buffers visuais de 500/200 itens | sanitizer, output-limit executor e testes de 1.000 eventos da UI |
| Fallback silencioso para API | custo ou efeito não autorizado | checkpoint durável e decisão explícita; cancelamento, shutdown, persistência e redaction nunca abrem fallback | testes de fallback e cancelamento |

## Riscos residuais aceitos para o MVP

- CLIs oficiais são dependências externas e podem mudar formatos/flags. O controle é falhar fechado em versão ou capacidade desconhecida e atualizar fixtures antes de ampliar suporte.
- O usuário pode confiar deliberadamente em um executável Custom malicioso. A confirmação exibe identidade, é invalidada após mudança e o runtime fornece snapshot read-only; não existe sandbox perfeito para código nativo confiado pelo usuário.
- O snapshot reduz exposição do workspace, mas a própria saída autorizada da tarefa pode conter informação sensível não reconhecida pelo corpus. Exportações permanecem marcadas como redigidas e devem ser tratadas como dados privados.
- Validação com instalações e contas reais dos quatro fornecedores é opt-in e não pertence à CI hermética, para que credenciais reais nunca sejam requisito do gate automatizado.

## Decisão de risco

Nenhum risco crítico ou alto conhecido permanece aberto no código coberto pelo runtime hermético. Os riscos residuais acima são de severidade média ou menor, têm comportamento fail-closed e não autorizam fallback, shell ou acesso cruzado silencioso.

Exception: the plan-quota credential read (ADR-028) carries a policy risk — provider terms and client identification — that has not been assessed, so this statement does not cover it until ADR-028 G10 is completed.

## Forge analysis and failure recovery — 2026-09-09

The confirmed ADR-016 extension permits developer-started triage to approve eligible scope/create tasks and dependency analysis to apply agent-origin edges. Host revision/cancellation/active-turn checks, approval savepoints, project membership, cycle checks and preservation of human relations constrain these mutations. Misleading analysis can now affect approved planning work, so the evaluation and graph remain reviewable before explicit dispatch. Archive, waves, HITL and final acceptance retain developer gates.

Stage-failure resolution validates the unique waiting checkpoint, purpose, state/input revision and persisted decision permissions transactionally. Retry preserves the spent attempts budget and advances generation. Rejected output is untrusted plain text and may contain private run content; it is distinct from the content-free usage ledger. LF/CRLF migration reconciliation recognizes only a computed equivalent checksum; arbitrary drift still fails.

See [synchronization evidence](cerebro-sync-validation-2026-09-09.md).

## Token usage accounting — 2026-09-09

The V28 usage ledger stores counters and bounded metadata only. Raw model output,
tool content and credentials are excluded; provider-owned deduplication keys are
hashed and model labels pass the existing secret redactor. Capturing usage adds
no provider requests or CLI flags that broaden execution permissions. Unknown
formats remain unavailable, interrupted evidence is partial, and deletion of
conversation/run history intentionally retains counters. Supplier counters are
untrusted observability data, not independently verified billing evidence.
See [implementation and validation](token-usage-implementation.md).

## Pulsation attribution — 2026-09-10

V29 stores the host-owned pulsation ID alongside existing counters, without copying names or content. Attribution survives source deletion; names are resolved on read. No new provider call or execution capability is introduced. Previously deleted source relations remain unassigned. See [validation](pulsation-token-usage.md).

## Antigravity CLI — 2026-10-14

`agy` 1.3.5 is the sixth official backend. Verified on this machine before any code was written:

- **Writes by default.** A headless `agy` refuses shell commands (`denied_actions`) but created a file with `write_to_file` without asking. Read-only is therefore enforced by configuration, not by an adapter flag: every launch points `--gemini_dir` at a host-owned root whose `antigravity-cli/settings.json` denies `write_file(*)`, `command(*)`, `unsandboxed(*)` and `execute_url(*)`; the Companion also denies `read_file(*)`. A real run against that root answered "Matches user-configured deny rule" and left the workspace untouched.
- **Developer configuration stays out.** The private root replaces `~/.gemini` wholesale (MCP servers, plugins, rules, permission grants, skills). The sign-in lives in the OS keyring and survives. Pipeline and Companion launches get a fresh root per run; manual and FreeForm runs share one root beside the database because `--conversation` resumes from it.
- **Prompt stays off argv.** `-p` only takes the prompt as an argument, so the adapter uses `--input-format stream-json` and writes one NDJSON message on stdin.
- **No self-update mid-pulsation.** Launches set `AGY_CLI_DISABLE_AUTO_UPDATE=true` (`1` is ignored by 1.3.5); versions above the validated ceiling are gated like Gemini.
- **Auth probe.** `-p /usage --output-format json` runs in the developer's own root (an empty root sends `/usage` to the model as text). It spends no model turn and also yields the quota shown on the agent card; quota labels are bounded and sanitized.

Residual risk accepted by Nathan (2026-10-14): the deny rules are `agy`'s own permission engine, an external dependency that could change between versions; a pulsation's writes would still land only in the read-only snapshot, and publication happens only from the JSON `artifacts` array. `--gemini_dir` is an undocumented flag found in the binary; `agy` rejects unknown flags, so a version that drops it fails the launch instead of running unconfined.

## Connections and subscription usage — 2026-10-17

Settings lists global CLI connection checks independently of the active project and API
configuration. Antigravity reuses its existing `/usage` probe. Codex uses its CLI's stdio
`app-server` protocol: `initialize`, `initialized`, then `account/rateLimits/read`. The host
does not create a thread, start a model turn or read credential files for this query.
The CLI still initializes its own local state and authenticates through its normal configuration.

The executable uses the existing secured launcher, reduced environment, temporary working
directory, hidden Windows process and Job Object. The exchange has a 20-second deadline and
a shared 64 KiB stdout budget. Stderr is discarded, and protocol errors become a generic
unavailable message; raw replies and account fields are never logged or persisted.
Only bounded plan/group labels, validated percentages and valid reset timestamps enter the
connection snapshot. Multi-bucket limits take precedence over the legacy single bucket;
missing durations do not imply a five-hour or weekly window. Expired windows are labeled
as awaiting refresh and have no current percentage or bar. Other CLIs explicitly report
when their connection check cannot provide subscription usage.

Hermetic evidence: `codex_usage_initializes_then_reads_limits_without_creating_a_thread`,
`codex_usage_rejects_oversized_and_error_replies`, the `codex_plan_usage_*` parser tests,
`plan_windows_past_their_reset_are_never_shown_as_current_usage`, and the Connections layout
test at 960 and 1440 pixels. `codex_plan_usage_live_read` is an opt-in authenticated check.

## Plan-quota meter and CLI credential reads — 2026-10-21

ADR-028 adds a meter for the 5-hour and weekly plan windows of Claude Code, Codex and Antigravity. Antigravity uses the existing `/usage` probe and reads no credential. Codex prefers the `app-server` query described above, then the `rate_limits` records of its rollouts. Two collectors may read a credential: the provider usage endpoint for Claude (primary) and for Codex (last resort). This is the only place the host reads another application's token.

Rollout reads are bounded tail reads (256 KiB of the newest rollouts, lines up to 64 KiB) that parse only `rate_limits` records; no session content is kept, and `CODEX_HOME` is honored.

Trust boundaries added:

- The CLI's credential file is a secret owned by that CLI. The Claude Code binary lists it among sensitive paths (next to `~/.ssh/**` and `~/.aws/**`); HVC's host read happens outside any agent workspace.
- Any process running as the same Windows user can already read these files. The added risk is HVC's own outbound use of the token and its handling in memory, not a new file-access capability.
- Provider responses are untrusted input, like CLI output.

| Threat | Impact | Control | Automated evidence |
|---|---|---|---|
| Token reaches a log, event, error, SQLite row, snapshot or the UI model | credential exposure at rest or on screen | token held in a redacting, zeroize-on-drop wrapper; no `Debug`/`Display` of the value; secret redactor extended to JWT-shaped tokens; snapshot table has no secret or identifier column (G2) | `quota_token_never_reaches_logs_events_database_or_errors`, `jwt_shaped_tokens_are_redacted`, `quota_snapshots_store_no_secrets_or_account_identifiers` |
| Token sent to an unintended host (configurable URL, redirect, URL built from CLI output) | credential exfiltration | compile-time constant hosts, TLS with validation, no redirects followed (G4) | `quota_collectors_send_only_to_pinned_hosts_and_refuse_redirects` |
| HVC refreshes or rewrites the CLI credential | developer signed out of the CLI, or a corrupted store | no call to a provider token endpoint, no write to CLI files; expired token reports `SignInExpired` (G3) | `quota_never_refreshes_or_writes_cli_credentials`, `quota_expired_token_reports_sign_in_expired_without_a_request` |
| Agent, prompt injection, MCP or workspace snapshot reaches the token | agent-visible credential | collectors run only in the host; token never in prompts, CLI environment, argv, snapshots or Companion tools; vendor configuration stays excluded from workspaces (G5) | existing `safe_cli_workspace_excludes_secrets_ignored_files_and_vendor_configuration` plus `quota_token_is_absent_from_cli_environment_and_argv` |
| Wrong credential store or over-reading fields | broader secret access than intended | read only the CLI's own file under `CLAUDE_CONFIG_DIR`/`~/.claude` or `CODEX_HOME`/`~/.codex`; deserialize only the needed fields; never touch an OS keyring entry; keyring-stored credentials report `CredentialStoreNotFile` (G1) | `quota_credentials_are_read_from_the_cli_directory_only`, `quota_ignores_refresh_and_id_tokens_and_api_keys` |
| Hostile, oversized or changed provider response | crash, memory growth, wrong numbers | bounded body, typed parse, unknown fields ignored, out-of-range or unparsable values make the source `Unavailable(UnexpectedResponse)` (G8) | `quota_response_shape_changes_fail_closed` |
| Excess requests, rate limiting or account flagging | blocked endpoint, annoyed provider | 55 s cache, doubling backoff to 15 min, `429`/`403` honored, polling only while visible and signed in (G6) | `quota_backoff_doubles_to_the_cap_and_honors_429` |
| Developer unaware that HVC reads a token | consent gap | a one-time first-use disclosure names the file read and the host contacted, with a Turn off button; Settings shows the active source per CLI and an off switch; `HVC_DISABLE_QUOTA_ENDPOINTS=1` kill switch (G7) | `quota_kill_switch_disables_credential_collectors_only`, `quota_first_use_disclosure_names_the_file_and_host_and_can_turn_it_off` |
| Stale or invented numbers drive a decision | wrong pre-flight advice | `Stale` state with age, windows past `resets_at` not shown as current, no interpolation across sources; projections labeled as estimates (G8) | `quota_window_past_its_reset_is_not_current`, `quota_projection_is_withheld_with_too_little_data` |
| Pre-flight quota check changes execution on its own | silent provider switch or API spend | advisory only: opens the existing human checkpoint, never blocks silently, never falls back (ADR-010, ADR-021) | `preflight_quota_warning_opens_a_human_checkpoint_and_never_switches_provider` |
| History table keeps sensitive data or grows without bound | privacy and disk | no token, account id, email or body stored; 30-day retention plus run-boundary samples, pruned automatically | `quota_retention_prunes_after_30_days_and_keeps_run_boundary_samples` |
| Impersonating the official client; provider terms | account action, policy conflict, reputational cost | **not a technical control.** Honest identification is tried first; any impersonation is an explicit, recorded maintainer decision; provider terms are checked before the repository opens; credential collectors sit behind a Cargo feature (G9, G10) | none; tracked as an open item in ADR-028 |

Residual risks:

- The token is in process memory while a request is built and sent. A memory-reading attacker with the same privileges, or a crash dump, could expose it. Mitigations are a short lifetime, zeroize on drop and no token in panic or error text. Severity: medium.
- The provider endpoints are undocumented and can change or disappear without notice. The meter fails closed and degrades to the next source; the Claude bar then relies on run-time events. Severity: low to medium.
- The Codex endpoint may reject a `rustls` client. This costs availability, not security. Severity: low.
- **Provider terms and client identification are not assessed.** Until ADR-028 G10 is completed, this item has no severity and must not be described as "no known high risk".
- Qualification with real accounts is opt-in and outside the hermetic CI, as for every CLI backend.

## Notes Scribe — 2026-10-22

Decision: [ADR-029 public summary](../docs/adr/README.md#adr-029-companion-notes). The Notes Scribe is a canonical role that turns a developer capture into a structured note. It reads only the text the host gives it.

| Threat | Impact | Control | Automated evidence |
|---|---|---|---|
| Prompt injection inside a capture | the agent leaves its note | no tools, no repository, no workspace; captures are data in the prompt; the host applies only a closed, validated contract to the one note at its admitted revision | `refinement_rewrites_only_this_note_with_a_bounded_conversation`, `the_validator_enforces_every_rule_the_prompt_states` |
| CLI run reaches the project workspace or developer configuration | reads beyond the capture | role-scoped Companion hardening: empty temporary working directory, the Companion launch restrictions, `notes` run origin; the Custom backend is refused | `notes_scribe_runs_companion_scoped_under_the_notes_origin` |
| A secret pasted into a capture or refinement | secret stored and sent to a provider | the secret redactor runs before storage and before the prompt; only the count is kept and shown | `secrets_are_redacted_before_storage_and_before_the_prompt` |
| A failed or cancelled call loses the developer's text | data loss | the capture is committed before any call; failures keep it for a retry; a restart marks running turns interrupted | `a_provider_failure_keeps_the_capture_for_a_retry`, `a_restart_interrupts_running_turns_and_fails_unstructured_notes` |
| Project deletion races a running note turn | orphaned writes | deletion refuses while a note turn runs; notes and turns go with the project | `refuses_a_running_note_turn_and_purges_companion_notes` |

Residual risk: the note text follows the developer's language, so the host's English guard does not apply to it (ADR-029 decision 5). The language check is advisory and a mismatch is accepted with a notice. Severity: low; a note feeds no agent.

## Voice dictation — 2026-10-23, amended 2026-10-29

Decision: [ADR-029 public summary](../docs/adr/README.md#adr-029-companion-notes).
HVC downloads and runs the selected native CPU speech runtime and model:
NVIDIA NeMo-Speech.cpp 0.2.0 with Nemotron 3.5 ASR 0.6B Q8_0, or whisper.cpp
1.9.5 (build `b5454`, commit `d1be6fde11ac6e0407606b4e42fe72d34add8037`) with
Whisper large-v3-turbo Q5_0. The latter replaces Python Whisper medium and
requires no Python, PyTorch, CUDA or FFmpeg. HVC records the microphone while
the global shortcut is held and turns the transcript into a voice note. The
recognizer is not an agent: it receives one WAV, a model path and host-owned
arguments, with no tools or repository. The resulting text enters the Notes
capture path; no resident speech service runs.

Both Windows x64 CPU archives are source-pinned in
[`hvc-voice` runtime definitions](../crates/ikore-voice/src/runtime.rs):

| Runtime archive | Bytes | SHA-256 |
|---|---:|---|
| [NeMo-Speech.cpp 0.2.0](https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.2.0/nemo-speech-0.2.0-windows-x86_64-cpu.zip) | 5,528,418 | `82c80451086e86194aba9af19f800da77cd66d1c28d993d0efceffd6cdb49bb8` |
| [whisper.cpp 1.9.5 / b5454](https://github.com/ggml-org/whisper.cpp/releases/download/b5454/whisper-bin-x64.zip) | 8,928,640 | `6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb` |

Model URLs, revisions, sizes and SHA-256 values are likewise pinned. The
[native Whisper qualification record](whisper-cpp-validation-2026-10-29.md)
lists its model pin and executed evidence; the
[2026-10-26 report](whisper-medium-validation-2026-10-26.md) is historical
Python/CUDA evidence and does not qualify native CPU inference.

| Threat | Impact | Control | Automated evidence |
|---|---|---|---|
| A tampered or substituted runtime archive or model | arbitrary code runs as the user | URL, byte size and SHA-256 are pinned in the source; the download streams through the digest with a size cap and a mismatch deletes the file before anything is unpacked; the archive is unpacked into a staging folder by `%SystemRoot%\System32\tar.exe` (absolute path), and either binary must report its pinned version before installation; NeMo additionally passes `--json doctor` | `a_runtime_archive_with_another_digest_is_never_unpacked`, `a_digest_mismatch_leaves_nothing_behind`, `a_size_mismatch_is_refused`, `the_pinned_releases_are_https_and_complete`, `native_whisper_artifacts_are_pinned_to_verified_native_sources` |
| A half-written or swapped model is used | wrong or failing transcription | a model counts as ready only with its digest marker and the exact pinned size; partial `.part` files are removed on failure or cancel | `a_model_without_its_marker_or_with_another_size_is_not_ready`, `a_verified_model_download_lands_with_its_marker_and_progress` |
| Recorded audio or temporary transcription persists | private speech or transcript kept on disk | samples stay in memory while recording (120 s cap); WAV and native Whisper JSON use host-owned UUID names in `voice\tmp`; drop guards attempt removal on success, failure, timeout, cancel and dropped futures; the next startup retries only matching 32-character lowercase hexadecimal UUID `.wav`/`.json` files, leaving unrelated files untouched; audio is never stored in SQLite or sent to a network service | `scratch_audio_is_removed_however_the_transcription_ends`, `transcript_output_is_removed_on_success_error_and_dropped_future`, `a_crash_left_transcript_is_swept_without_touching_other_json`, `the_startup_sweep_removes_only_leftover_scratch_recordings` |
| The microphone records without the developer knowing | unintended capture | recording happens only while the shortcut is physically held, push-to-talk is off by default and registers only after setup, a start earcon plays at key-down, and taps under 0.5 s are discarded with a visible message | `settings_default_off_and_round_trip`, `taps_silence_and_a_missing_project_store_nothing_and_say_so` |
| A spoken secret is transcribed | secret stored or sent to the Notes Scribe | the transcript enters the same capture path as typed text, so the secret redactor runs before storage and before any prompt | `a_dictation_becomes_a_voice_note_in_the_open_project`, `secrets_are_redacted_before_storage_and_before_the_prompt` |
| The runtime hangs or opens windows | stuck dictation, console flashes | one process per utterance with `CREATE_NO_WINDOW`, stdin closed and kill on drop; Nemotron has a two-minute transcription timeout, native Whisper ten minutes; cancellation drops the command future and kills the process | `transcription_output_and_errors_are_read_from_the_runtime_json`, `cancellation_and_timeout_stop_a_running_native_check` |
| Native output is missing, malformed or excessive | invalid text accepted or memory growth | native Whisper requires its JSON file, caps it at 2 MiB, validates segment offsets and language, and derives duration from the input PCM rather than padded model output; no shell string is composed and Whisper CPU, Portuguese and transcription are fixed by the host | `a_success_exit_without_json_and_an_oversized_output_are_rejected`, `silence_is_valid_but_wrong_or_malformed_cli_output_is_rejected`, `pcm_duration_accepts_ancillary_chunks_but_rejects_truncated_and_wrong_formats`, `cli_arguments_keep_paths_as_separate_values_and_force_local_portuguese_cpu` |
| The global shortcut collides with another application | shortcut stolen or silently missing | `RegisterHotKey` fails visibly and the Settings page shows the error; the developer can choose another of 40 combinations | `every_combination_round_trips_through_its_stored_form`; manual `push_to_talk_reports_the_press_and_the_release` (ignored: sends global keystrokes) |

Residual risk: installed runtimes are trusted at each run through their install
markers, not re-hashed, so a process already running as the user could replace
`nemo-speech.exe`, `whisper-cli.exe` or an adjacent native library under
`%LOCALAPPDATA%`. Such a process already has the user's privileges. Severity:
low. A crash or filesystem error can defer scratch deletion until a later
startup; repeated failures are not proof of secure erasure. Recognition errors
also remain possible; the developer can review the resulting note. The
downloaded runtime/model licenses still apply, and these checks do not change
the project's existing binary-distribution disposition.

## Execution mandates, scope grants and attestation — 2026-10-29

Decisions: [ADR-035 public summary](../docs/adr/README.md#adr-035-execution-mandates-scope-grants-and-trust-layers) and [ADR-036 public summary](../docs/adr/README.md#adr-036-signed-local-pulsation-attestation). Approving the Specification issues an execution mandate: the host's record of what each stage may write, with which transport, checks and budget. A Code or Test unit can ask for a delivered file it does not own, and only the developer can grant it. When a pulsation ends, the host seals a signed record of what it did ([attestation format](../docs/attestation.md)). Agents never write a mandate, a grant or a record.

Trust boundaries added:

- A mandate, a grant and a sealed record are host data derived from SQLite rows. Model output is an input to the derivation, never authority: a scope request is a list of refs the host resolves against the frozen plan.
- The signing key is an Ed25519 secret in the Windows Credential Manager (`hard-vibe-code.runtime`, entry `attestation-signing-key-v1`). SQLite and exports hold only public keys.
- An exported record leaves the machine. It holds identifiers, digests, states and counters, with no file content.

| Threat | Impact | Control | Automated evidence |
|---|---|---|---|
| A stage writes outside what was approved | changes the developer never authorized | writes are checked against the mandate in force when output is accepted (refused, nothing stored or written) and again at publication (a conflict and a bounded enforcement event, nothing written) | `a_stage_write_outside_the_enforced_mandate_is_refused_at_acceptance`, `a_publication_outside_the_revision_in_force_is_a_conflict_and_an_event`, `enforcement_events_are_bounded_per_pulsation`, `a_composed_pulsation_runs_inside_the_mandate_its_specification_approval_issued` |
| Prompt injection makes a unit ask for a sensitive or unrelated file | a secret or a protected file in the unit's scope | only an existing file already delivered to the unit can be asked for; protected names, `REVIEW.md`, stage documents, another stage's or unit's files and the wrong role are refused with a code; at most 8 files and 2 requests per budget account; the card marks sensitive files and shows the risk before and after; nothing is granted without the developer | `each_refusal_rule_names_its_code`, `a_mixed_request_keeps_only_the_admissible_files_and_their_delivered_bytes`, `the_request_is_offered_within_its_limits_and_the_budget_only`, `the_risk_after_counts_the_granted_files`, `a_test_unit_asking_for_a_production_file_gets_no_request` |
| Model output forges an approval, a mandate or a grant | an agent widens its own authority | mandates are issued only from a Specification approval checkpoint and amended only by host events (transport fallback, scope grant); revisions are append-only and chained by digest; the agent's reason is sanitized, capped at 400 characters and labelled model-written; the grant command carries the state version and derivation digest the developer saw | `approving_the_specification_issues_the_previewed_mandate_and_a_receipt`, `revisions_are_append_only_and_verified_against_their_digest`, `a_reissue_chains_to_the_previous_revision_and_drops_its_amendments`, `a_pending_scope_request_is_granted_with_what_the_developer_saw`, `a_scope_request_shows_its_files_and_leads_the_decisions_with_the_grant` |
| A file changes between the request and the grant | the unit edits bytes the developer did not see | the grant re-derives the request in one immediate transaction and requires each file's current hash to equal the delivered one; otherwise the request becomes `stale` and nothing is granted | `a_file_changed_after_the_request_is_not_granted`, `a_stale_scope_request_says_so_and_offers_no_grant` |
| A crash between the grant and the retry | a grant without its run, or a run without its grant | the grant, the mandate revision, the receipt and the job reset commit together; recovery resumes the pending job | `a_restart_between_the_grant_and_the_dispatch_resumes_the_stage` |
| SQLite rows or files changed after sealing | a record that no longer matches what happened | a trigger allows only `unsigned` → `signed`; verification checks the signature, re-derives the statement from the current rows and names the first block that differs, and hashes each subject in the workspace | `tampering_with_rows_files_or_the_envelope_is_detected`, `an_attestation_never_changes_except_to_be_signed`, `the_deriver_reads_each_block_from_its_rows` |
| An exported record is altered | a forged history shown to a reviewer | DSSE over canonical JSON with Ed25519; any byte change breaks the signature; the file can be checked here or with OpenSSL and the exported public key | `an_exported_attestation_verifies_here_and_elsewhere`, `any_change_to_the_envelope_breaks_the_signature`, `pae_follows_the_dsse_specification`, `the_test_vector_is_stable` |
| A valid record is replayed onto another pulsation | another run's history passes as this one's | the statement names its pulsation and project; verification re-derives from the rows of the pulsation being checked, and the file check reports the pulsation id the record names | `a_record_replayed_on_another_pulsation_is_detected`, `an_exported_attestation_verifies_here_and_elsewhere` |
| The export leaks content or secrets | private code, prompts, reasons or paths leave the machine | the statement keeps digests, states and counters only; reasons are kept as digests; no workspace path, task text or key material; the export notice lists what the file holds; oversized statements are kept as a digest | `the_statement_carries_no_content_names_or_key_material`, `a_statement_over_the_size_cap_is_kept_as_a_digest` |
| The key is lost, replaced or printed | records signed by an unknown key, or a leaked key | the key never reaches SQLite, logs, `Debug` output or an export; a missing key is never replaced without the developer; a replaced key is a mismatch until a new key is created; rotation keeps every public key; without a keyring the statement is kept unsigned and signed later | `a_key_round_trips_through_its_stored_secret_and_never_prints_it`, `a_missing_key_is_never_replaced_without_asking`, `a_replaced_key_is_a_mismatch_until_a_new_key_is_created`, `rotation_keeps_one_active_key_and_every_public_key`, `without_a_keyring_the_statement_is_kept_unsigned_and_signed_later` |
| A sealing failure blocks the application | a stuck lane or a lost record | sealing is an intent written with the terminal transition; only `SQLITE_BUSY`/`SQLITE_LOCKED` is retried, any other error marks the intent failed with a diagnostic and the lane continues; a restart resumes pending intents | `a_database_error_other_than_busy_leaves_the_queue`, `a_derivation_that_cannot_succeed_is_kept_failed_until_retried`, `a_restart_between_the_end_and_the_seal_resumes_it` |
| The local clock is wrong or set back | misleading `endedAt` and `sealedAt` times | **not a technical control.** Times come from the local clock and are recorded as such; there is no trusted timestamp in v1 | none |

Residual risks:

- **A process running as the same Windows user can read the key from the Credential Manager and sign any statement.** A record proves that it was signed on a machine and account that held the key, not that HVC produced it. Hardware-bound keys (TPM or Windows Hello) are a later step. Severity: medium for a record shown to someone else; low on the developer's own machine, where such a process already has the user's privileges.
- The same process can edit the SQLite file, drop the trigger and seal a fabricated history with the key. Verification then reports a valid record. The record does not defend against the account that owns it.
- Timestamps are self-asserted by the local clock. Severity: low; verification never checks a time against a trusted clock, so a reader should treat the times as claims.
- A grant only widens a unit's write set to files it was already delivered, so a file the workspace sanitization keeps out of the workspace cannot be asked for, and protected names are refused by rule. Within those files, the developer's reading of the card is the control. Severity: low.

## Agent exchange — 2026-10-30

Decision: [ADR-037 public summary](../docs/adr/037-agent-exchange.md). Every piece of content the host did not write reaches a model under a provenance line; the host keeps a durable exchange log; an explicit CLI session resume works after workspace edits and tells the agent what changed; the Companion can search and read digests of finished pulsations. No new process, network access or write permission is added. The ACP transport was not enabled: its gate was not run, and no ACP code or dependency was added.

Trust boundaries added:

- The provenance line and the legend are host text. The line says who wrote an item; it grants nothing. Agent output, tool output and repository content stay data, even when the developer approved them.
- The exchange log stores references to domain rows (`payload_table`, `payload_id`), never content. The UI doorbell carries ids and sequence numbers only.
- A resume notice is host text built from sanitized snapshot content. A pulsation digest is host-derived from SQLite rows, sanitized with a receipt, and labelled per section.

| Threat | Impact | Control | Automated evidence |
|---|---|---|---|
| Text planted in a file, a check's output or another agent's output poses as an instruction | an agent follows an injected instruction | every item the host did not write carries a `> provenance:` line outside its fence; the legend says only host and human items may instruct and that an approved item still cannot change the rules; every prompt section is classified by an executable inventory | `prompt_input_inventory_classifies_every_prompt_the_pipelines_render`, `an_unclassified_section_fails_the_inventory`, `a_labelled_section_or_item_without_its_line_fails_the_inventory`, `the_legend_is_the_specified_text` |
| Content forges a host provenance line or closes its own fence | data poses as host text | the fence is longer than any backtick run in the content, so a forged line stays inside it | `the_fence_outlasts_every_backtick_run`, `a_forged_host_line_stays_inside_the_fence` |
| A stale writer records after its execution moved on, or a write is replayed | the log contradicts the domain state | an append with a moved fence writes nothing and aborts the domain write; a repeated idempotency key writes nothing; items are append-only; a stream out of step with its items fails closed | `a_moved_fence_writes_nothing_and_aborts_the_domain_write`, `a_repeated_key_is_a_duplicate_that_writes_nothing`, `items_are_append_only`, `a_stream_out_of_step_with_its_items_fails_closed` |
| A resume notice carries a secret from a changed file | a secret reaches the CLI provider | snapshot blobs keep the sanitized projection (text files up to 32 KiB, 1 MiB per run); hunks compare sanitized content; the notice goes through the prompt's sanitization; more than 40 changed files or 8,000 characters refuse the resume | `a_resume_notice_carries_only_sanitized_hunks`, `a_manual_run_keeps_its_manifest_and_small_sanitized_blobs`, `the_budget_drops_the_largest_hunks_then_refuses`, `more_than_forty_changes_refuse_the_resume_with_the_count` |
| The workspace changes between the resume preview and the launch | the agent is told about changes that are not the ones it sees | the child run stores the digest of the change list; the launch re-compares the parent's manifest with the real snapshot and refuses when the list differs | `a_resume_starts_only_on_the_snapshot_its_notice_described`, `the_canonical_list_ignores_content` |
| A stale-read notice in the Companion leaks file content | content reaches the provider without a read | the notice lists paths only, at most 50, and only files read in earlier turns of the same conversation | `a_turn_warns_when_a_file_read_earlier_changed` |
| A pulsation digest carries a secret or injected text into Companion answers | a secret reaches the provider; injected text gains weight | digests are derived by the host from rows, redacted with a receipt, labelled per section, and returned as `agent_output`; the tools write nothing on a model's request | `a_stored_digest_is_sanitized_receipted_announced_and_idempotent`, `a_digest_is_deterministic_with_each_sections_provenance`, `a_question_about_a_past_choice_reaches_its_approved_decision` |
| A Companion conversation reads another project's pulsation | cross-project disclosure | a reference must belong to the conversation's project; otherwise the answer is "Unknown pulsation reference." | `pulsation_search_and_read_find_an_approved_decision` |
| Reserved reviewer kinds are emitted before their contract exists | content enters prompts with an unreviewed trust meaning | `review_finding` and `claim_under_verification` exist in the contract and no crate produces them | `kinds_are_listed_once_and_two_are_reserved`, `reserved_review_kinds_exist_and_nothing_produces_them` |

Residual risks:

- **A provenance line reduces the chance that a model obeys injected text; it does not prevent it.** The controls that bound the damage are unchanged: the read-only snapshot, host-owned publication, and no Companion tool that writes. Severity: medium, as before.
- A digest quotes agent output the developer approved. An injection that passed that approval stays in later Companion answers, labelled `agent_output`. Severity: low.
- Hunks in a resume notice show sanitized content of files the agent already had in its session's snapshot; the redaction coverage is that of workspace sanitization. Severity: low.
