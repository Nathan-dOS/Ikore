# Threat model — runtime CLI multiagente

Design references: [public summaries](../docs/adr/README.md); [historical archive context](README.md#historical-design-references).

**Revisão:** 2026-09-03  
**Escopo:** Codex, Claude Code, Gemini CLI, OpenCode, Antigravity CLI e Custom no Windows x64<br>
**Estado:** implementado e revisado para o gate CLI-G3

## Fronteiras de confiança

- O workspace, seus arquivos, configurações de fornecedor e instruções são entrada não confiável.
- A saída da CLI também é entrada não confiável, mesmo quando o executável foi descoberto como oficial.
- O banco local e o event bus só podem receber conteúdo depois de sanitização e redaction.
- O keyring é a única fronteira autorizada para segredos de API; o painel Agents nunca persiste a chave em configuração ou histórico.
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
