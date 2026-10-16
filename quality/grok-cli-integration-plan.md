# Plano Grok-G — integração do Grok CLI e do provedor xAI (API)

**Data:** 2026-09-21
**Branch:** `main` (a decidir: branch de feature dedicada antes do primeiro commit)
**Baseline Git:** `bf7fdbf`, mais o worktree em andamento de ADR-022/redesign de shell (não commitado)
**Estado:** plano — nenhuma linha de código escrita
**Prioridade:** definida pelo usuário
**Plataforma de corte:** Windows x64

## Objetivo

Adicionar um sexto backend CLI (`grok`) ao runtime multiagente já existente (Codex, Claude Code, Gemini CLI, OpenCode, Custom) e um novo provedor de API direta (`xai`) ao modo API, reaproveitando ao máximo a arquitetura de adapters, migrações, catálogo de modelos e UI já implementada e validada pelo [multi-agent-cli-completion-plan.md](multi-agent-cli-completion-plan.md) e pelo [cli-threat-model.md](cli-threat-model.md). Este plano não reabre nenhuma decisão desses dois documentos; ele estende ambos.

## Evidências coletadas (verificadas nesta máquina em 2026-09-21)

Diferente dos quatro backends existentes, o Grok CLI não tinha nenhuma fixture, documentação interna ou captura prévia neste repositório. Antes de desenhar o adapter, os seguintes fatos foram confirmados diretamente — parte pelo usuário, parte por execução real do binário nesta sessão (custo total incorrido: dois turnos triviais, ≈ US$ 0,065, ambos com `--max-turns 1 --disable-web-search --no-subagents`, `--cwd` apontando para um diretório de scratch fora do repositório):

- Binário: `grok.exe`, instalador oficial nativo (`irm https://x.ai/cli/install.ps1 | iex`), SHA-256 conferido contra `agent.exe` (mesma build). **Não é um wrapper npm/node** — é um executável nativo assindoável, diferente dos quatro backends atuais.
- Versão instalada: `grok 1.0.40 (eb1a2256660d) [stable]`. Única versão validada até aqui.
- Descoberta: `grok.exe` fica em `~/.grok/bin`, adicionado ao PATH de usuário pelo instalador. `find_executable(backend.as_str(), …)` já busca por nome genérico no PATH ([terminals.rs:2795](../crates/hvc-core/src/terminals.rs)) — nenhuma lógica nova de descoberta é necessária além de `CliBackend::Grok.as_str() == "grok"`.
- Modo headless: `-p/--single <PROMPT>` imprime e sai. **`stdin` não é o prompt** — só `-p`, `--prompt-file <path>` ou `--prompt-json <json>` aceitam conteúdo. Isso conflita com o invariante "prompt nunca em argv, sempre por stdin"; a resolução está na seção de decisões abaixo.
- `--output-format streaming-messages-json --include-partial-messages` produz **NDJSON no formato de wire da Anthropic Messages API**, eventos `stream_event` envolvendo `message_start` / `content_block_start` / `content_block_delta` (`thinking_delta`, `text_delta`) / `content_block_stop` / `message_delta` / `message_stop`, seguido de um evento `assistant` consolidado e um evento final `result` com `session_id`, `stop_reason`, `total_cost_usd`, `usage`. Capturado ao vivo nesta sessão (amostra completa arquivada no histórico da conversa, não neste repositório — recapturar como fixture no Bloco G2).
- `grok -p "..." --output-format json` (modo não-streaming) devolve um único objeto `{"text","stopReason","sessionId","requestId","thought","usage",...}`. Testado pelo usuário fora desta sessão.
- Modelo do turno de teste: `grok-4.7`, `contextWindow: 500000` (visível em `modelUsage`). `grok models` lista `grok-4.7` (default) e `grok-4.6`, sem metadados de preço/contexto — saída é **texto solto**, não JSON.
- Autenticação nesta máquina: sessão OAuth (`~/.grok/auth.json`, issuer `https://auth.x.ai`). Ordem de credencial do CLI: `api_key`/`env_key` do modelo em `config.toml` → sessão OAuth → `XAI_API_KEY` (alias `GROK_CODE_XAI_API_KEY`). Não existe `grok auth status`; `grok models` já imprime o estado de login e sai rápido, servindo como probe de auth de baixo custo.
- Resume: `-r/--resume <ID_OU_TITULO>` (aceita UUID explícito — compatível com o invariante do projeto). `-c/--continue` existe mas replica exatamente o padrão "latest/last/continue" que o projeto proíbe explicitamente no pipeline; **nunca deve ser usado**. `--session-id` só serve para forçar o UUID de uma sessão **nova**, nunca para retomar.
- `--reasoning-effort`/`--effort <EFFORT>` existe (mesmo nome de flag do Claude). O vocabulário aceito de valores não foi confirmado — ver item em aberto.
- Toolset padrão observado no evento `init`: `run_terminal_command, read_file, search_replace, list_dir, grep, kill_command_or_subagent, todo_write, get_command_or_subagent_output, spawn_subagent, scheduler_create/delete/list, monitor, search_tool, use_tool, workflow, enter_plan_mode, exit_plan_mode, ask_user_question, send_feedback, image_gen, image_edit, image_to_video, reference_to_video, write`. É uma superfície de ferramentas muito maior que a dos quatro backends atuais (que já são restringidos a um subconjunto de leitura).

## Escopo

### Incluído

- `CliBackend::Grok` no runtime de terminais (mesmo tratamento dos cinco backends existentes: descoberta, auth probe, versão, execução automática de pipeline, execução manual, FreeForm, resume nativo por ID, catálogo de modelos).
- `xai` como `ProviderProfile` do modo API (protocolo OpenAI-compatible, mesmo caminho genérico de Groq/DeepSeek/Mistral/OpenRouter).
- Migração de banco widening todo `CHECK(backend IN (...))`/`CHECK(cli_backend IN (...))` existente.
- Fixtures versionadas, threat-model e ADR do cérebro atualizados.

### Não incluído nesta entrega

- Ferramentas de imagem/vídeo do Grok (`image_gen`, `image_edit`, `image_to_video`, `reference_to_video`) — irrelevantes para o pipeline de código e explicitamente excluídas do allowlist de tools.
- Subagentes nativos do Grok (`spawn_subagent`) — o app já tem seu próprio modelo de subagentes; `--no-subagents` sempre presente, igual à decisão já tomada para os outros backends quanto a MCP/hooks de terceiros.
- `grok worktree`/`grok clone`/`grok dashboard`/modo TUI interativo — fora do modo headless usado pelo pipeline.
- Suporte a `--prompt-json` (conteúdo multimodal) — v1 usa só texto, como os outros backends.

## Decisões arquiteturais

### 1. Prompt por arquivo temporário, não por stdin — única exceção ao invariante existente

O invariante "prompts nunca entram em argumentos; são enviados por stdin" (multi-agent-cli-completion-plan.md, Invariantes não negociáveis) não é satisfazível literalmente pelo Grok CLI: `-p` exige o prompt como argumento, e stdin é ignorado. A opção que preserva a *propriedade de segurança* por trás do invariante (o conteúdo do prompt nunca aparece em texto plano na linha de comando, visível via `tasklist`/Process Explorer/ETW/log de auditoria para qualquer outro processo ou usuário na máquina) é usar `--prompt-file <path>`: o argumento em si é só um caminho de arquivo, nunca o conteúdo.

Mecanismo (espelha o que já existe para `schema_file` em [terminals.rs:3944](../crates/hvc-core/src/terminals.rs) e [terminals.rs:4599](../crates/hvc-core/src/terminals.rs)):

- Novo campo `prompt_file: Option<PathBuf>` em `CliInvocationRequest` (ao lado de `schema_file`).
- Nos pontos de montagem de `CliInvocationRequest` (os mesmos dois locais que hoje materializam `schema_file` para Codex), quando `backend == CliBackend::Grok`: escrever `request.prompt` num `tempfile::NamedTempFile` fora do workspace, manter o guard vivo até o processo terminar (mesmo padrão de `schema_guard`), preencher `prompt_file` com o caminho.
- `GrokAdapter::build_process_spec` usa `request.prompt_file` para montar `--prompt-file <path>` em vez de escrever em `stdin_payload` (que fica vazio para Grok).
- **Risco residual novo, não presente nos outros cinco backends:** o prompt (que pode conter trechos de código proprietário) toca o disco, ainda que por um arquivo de vida curta apagado por `Drop` mesmo em pânico/erro. Isso precisa de uma linha nova na tabela de ameaças do [cli-threat-model.md](cli-threat-model.md) (Bloco G8) e, se possível, ACL explícita restringindo o arquivo ao usuário atual no Windows (a confirmar na implementação — `schema_file` hoje não faz isso, então é uma barra mais alta que o precedente).
- Resume (`-r <uuid>`) usa o mesmo mecanismo para o prompt de continuação — nunca `-c/--continue`.

### 2. Formato de saída

- Pipeline/execução automática e manual: `--output-format streaming-messages-json --include-partial-messages --verbatim`. Por ser wire-compatible com a Anthropic Messages API, os helpers genéricos já existentes em [adapters/mod.rs](../crates/hvc-core/src/terminals/adapters/mod.rs) (`find_session_id`, `find_model_id`, `find_finish_reason`, `extract_visible_text`, a heurística `is_delta` que já testa `event_type(&value) == "stream_event"`) funcionam sobre os eventos do Grok sem alteração — só o `classify` fechado em `normalize_frame` precisa ser escrito, espelhando quase 1:1 o de [claude.rs](../crates/hvc-core/src/terminals/adapters/claude.rs).
- Execução com saída estruturada (schema): `--json-schema <schema-inline>` (implica `--output-format json`, então não combinar com `streaming-messages-json` nesse caso — igual à forma como Codex/Claude tratam schema hoje). `--json-schema` do Grok aceita JSON **inline** como argumento, exatamente como o do Claude — provável candidato a reaproveitar `SchemaFlavor::Canonical` (a decidir empiricamente: Claude precisa remover `$schema` antes de enviar; não há confirmação ainda de que o Grok tenha a mesma limitação — ver item em aberto).

### 3. `extract_visible_text` já protege contra vazamento de raciocínio — verificado, não assumido

O invariante "sem raciocínio oculto" (rótulo canônico "ATIVIDADE E SAÍDA") foi checado campo a campo contra a captura real: em todo `content_block_delta` do tipo `thinking_delta`, o texto do raciocínio fica sob a chave `thinking`, que não é uma das cinco chaves prioritárias de `extract_visible_text` (`text`, `content`, `message`, `output`, `result` — [terminals.rs:3427](../crates/hvc-core/src/terminals.rs)) e, por ser uma string-folha sob chave não-prioritária, a recursão genérica não a alcança. No bloco `assistant` consolidado, `content` é um array com um bloco `{"type":"thinking","thinking":"...","signature":"..."}` seguido de `{"type":"text","text":"..."}`; a busca por array retorna o primeiro `find_map` que produzir `Some`, e o bloco `thinking` não produz nada pela mesma razão, então o bloco `text` é o que aparece. No modo `--output-format json` (não-streaming), o campo irmão é `thought` (nome diferente) e nunca é alcançado porque `text` já casa primeiro no mesmo objeto. Ou seja: **nenhuma mudança de código é necessária aqui**, mas isso deve virar um teste de regressão explícito no Bloco G2 (`captures_no_thinking_or_thought_text`), porque é uma propriedade essencial que hoje só existe por como as chaves calham de se chamar — uma mudança futura do formato do Grok pode quebrá-la silenciosamente.

### 4. Catálogo de modelos: caminho declarativo, não live-parse

`grok models` devolve texto solto sem metadados de preço/contexto/capacidades, e a xAI publica um catálogo pequeno e estável (2 modelos hoje). Isso é o mesmo perfil de Codex/Claude/Gemini (catálogo JSON versionado e curado em [assets/cli-model-catalog-v1.json](../crates/hvc-core/assets/cli-model-catalog-v1.json)), não o do OpenCode (catálogo agregando múltiplos provedores de terceiros, por isso only ele faz parsing ao vivo de `opencode models --verbose`). Grok entra como quarto backend com catálogo declarativo:

- `CliModelCatalogAssetBackends` ganha um campo `grok` ([model_catalog.rs:34](../crates/hvc-core/src/terminals/model_catalog.rs)).
- `declarative_models()` ganha `CliBackend::Grok => &backends.grok.models` ([model_catalog.rs:87](../crates/hvc-core/src/terminals/model_catalog.rs)).
- `cli-model-catalog-v1.json` ganha a chave `"grok"` com `grok-4.7` (`contextWindow: 500000`, confirmado) e `grok-4.6` (contexto a confirmar). `profiles` (efforts aceitos) começa vazio até o item em aberto #2 ser resolvido.
- `detect_cli_default_model` ([model_catalog.rs:703](../crates/hvc-core/src/terminals/model_catalog.rs)) ganha um branch para `~/.grok/config.toml`, chave `model` — mesma técnica read-only/capped já usada para Codex.

### 5. Auth probe

`auth_probe_args()` retorna `Some(&["models"])`. É rápido, sem custo de turno, e a saída já contém o texto de estado de login confirmado pelo usuário ("You are logged in with grok.com."/estado de erro em caso de falha). Evita gastar um turno de modelo só para checar auth, ao contrário de disparar `-p`.

### 6. Ferramentas e modo de permissão do pipeline

`--permission-mode plan --tools read_file,list_dir,grep,search_tool --no-subagents --disable-web-search`, mesma postura read-only que Claude (`--tools Read,Glob,Grep`) e Gemini (`--approval-mode plan`) já aplicam. **Isto precisa de validação real na implementação** (Bloco G1): confirmar que `--permission-mode plan` combinado com um allowlist restrito realmente impede `write`/`search_replace`/`run_terminal_command`/`spawn_subagent`, e não é só uma sugestão de prompt. Enquanto não validado, tratar como não confirmado no threat model (mesma disciplina de "capacidade não pode ser simulada").

### 7. Launch target no Windows — nenhum código novo

`grok.exe` tem extensão `.exe` nativa. A função de hardening de lançamento ([terminals.rs:6060-6084](../crates/hvc-core/src/terminals.rs)) só desvia para `resolve_trusted_npm_shim` quando a extensão é `.cmd`/`.bat`; qualquer outra extensão (inclusive `.exe`) já cai no caminho de binário nativo genérico (hash SHA-256 + `spec.executable = executable`), o mesmo caminho que `Custom` usa hoje. **Nenhum branch novo é necessário em `resolve_trusted_npm_shim` nem em `secure_launch_target`** — só é preciso confirmar isso com um teste dedicado (`grok_native_exe_never_routes_through_npm_shim_resolution`), já que é uma propriedade que depende de comportamento existente, não de código novo.

### 8. Modo API: provedor OpenAI-compatible declarativo

A API da xAI é compatível com o formato de chat completions da OpenAI. Isso cai diretamente no padrão já usado por Groq/DeepSeek/Mistral/OpenRouter/Gemini em [llm/profiles.rs](../crates/hvc-core/src/llm/profiles.rs) (`ProviderProtocol::OpenaiChat`, cliente HTTP genérico, sem código de provedor dedicado). Adição proposta:

```rust
profile(
    "xai",
    "xAI (Grok)",
    ProviderProtocol::OpenaiChat,
    "https://api.x.ai/v1/chat/completions",
    AuthStyle::Bearer,
    "<default_model a confirmar>",
    false,      // local
    true,       // json_object — a confirmar contra a documentação da API
    true,       // json_schema — a confirmar contra a documentação da API
    vec![],     // forbidden_parameters — a confirmar (Groq só descobriu os seus por tentativa real)
),
```

mais uma entrada em [assets/llm-model-catalog-v1.json](../crates/hvc-core/assets/llm-model-catalog-v1.json) (preço/capacidades por `provider_id`+`model_id`). **Os IDs de modelo da API quase certamente não são os mesmos do CLI** (`grok-4.7`/`grok-4.6` parecem apelidos internos do CLI; a API pública da xAI historicamente usa nomes versionados próprios, ex. `grok-4`, `grok-4-fast`, `grok-code-fast-1`). Não assumir equivalência — ver item em aberto #4.

## Migração de banco (V38)

Toda tabela com `CHECK(backend IN (...))` ou `CHECK(cli_backend IN (...))` foi enumerada aplicando as 37 migrações existentes numa base sqlite de verificação (`sqlite3` local, nesta sessão). O padrão de ampliação de `CHECK` já usado neste projeto (V24, V31, V32, V37) é sempre reconstrução de tabela: `CREATE TABLE <t>_v38 (... CHECK ampliado ...)`, `INSERT INTO <t>_v38 SELECT ... FROM <t>`, `DROP TABLE <t>`, `ALTER TABLE <t>_v38 RENAME TO <t>`. Nove tabelas precisam disso:

| Tabela | CHECK atual | Ação |
|---|---|---|
| `cli_agent_configs` | `backend IN ('codex','claude','gemini','opencode','custom')` NOT NULL | reconstruir, adicionar `'grok'` |
| `terminal_slots` | idem | reconstruir |
| `terminal_runs` | idem, nullable | reconstruir |
| `pulse_execution_overrides` | idem, nullable condicional | reconstruir |
| `cli_backend_attempts` | idem NOT NULL, parte da PK | reconstruir |
| `composition_specialty_overrides` | idem, nullable condicional | reconstruir |
| `chat_conversations` | `cli_backend IN ('codex','claude','gemini','opencode')` (sem `custom`) | reconstruir, adicionar `'grok'` |
| `chat_model_calls` | idem | reconstruir |
| `cli_model_catalog_snapshots` | `backend IN ('codex','claude','gemini','opencode')` (sem `custom`) | reconstruir, adicionar `'grok'` |

`companion_agent_output_documents.backend` é `TEXT` sem `CHECK` — não precisa de migração. `cli_fallback_requests` não tem coluna `backend` (só `agent_role`, que casualmente contém a string `'backend_agent'` como valor de enum) — não precisa de migração.

**Numeração:** `V37__ephemeral_composition_execution_slots.sql` está no working tree como arquivo não rastreado (`git status` no início desta conversa) — ainda não commitado. Se `V37` for commitado antes desta entrega, a migração do Grok é `V38`. Se o usuário preferir dobrar a ampliação do `backend` CHECK de `terminal_slots`/`terminal_runs` dentro do próprio `V37` (que já reconstrói exatamente essas duas tabelas por outro motivo), evita-se uma reconstrução dupla dessas duas — mas as outras 7 tabelas ainda precisam de uma migração própria. Recomendação: manter `V37` focado no que já é (slots de composição efêmeros) e abrir `V38` dedicado só a "adicionar backend grok", para manter cada migração de propósito único, como o próprio histórico do projeto já pratica.

Cada tabela reconstruída preserva `PRAGMA foreign_key_check = 0` e testes a partir de V1/V20/V36 (padrão já exigido pelo projeto para toda migração).

## Blocos de trabalho

### Bloco G0 — contratos e decisões
- Congelar as decisões da seção acima (usuário confirma os itens em aberto).
- Adicionar `CliBackend::Grok` ao enum ([terminals.rs:251](../crates/hvc-core/src/terminals.rs)): variante, `ALL` (vira `[Self; 6]`), `as_str()` → `"grok"`, `label()` → `"Grok"`, `parse()`.
- Rodar `cargo check --workspace` imediatamente após o passo acima e **usar os erros de match não-exaustivo como checklist autoritativa** dos pontos de código a tocar — é mais confiável do que qualquer grep manual (28 arquivos hoje referenciam `CliBackend`; nem todos têm um match exaustivo sobre ele, mas os que têm serão sinalizados pelo compilador).

### Bloco G1 — `GrokAdapter`
- Novo `crates/hvc-core/src/terminals/adapters/grok.rs`, registrado em [adapters/mod.rs](../crates/hvc-core/src/terminals/adapters/mod.rs) (`mod grok;`, `static GROK`, braço em `adapter()`, braço em `compatibility_rule()` com `minimum: (1,0,40), maximum_exclusive: (1,1,0)` — únicos números validados até aqui).
- `capabilities`: `CliOutputFormat::JsonLines`, `structured_output: true`, `native_resume: version_supports_contract(...)`, `thinking_levels: [Low, Medium, High]` (mesmo trio do Claude, dado que a flag é a mesma `--effort`/`--reasoning-effort` — sujeito ao item em aberto #2), `auth_probe: true`, `read_only: true` (sujeito à validação da decisão #6), `model_catalog: true`.
- `build_process_spec`: materializa `--prompt-file`, `--output-format streaming-messages-json --include-partial-messages --verbatim --permission-mode plan --tools read_file,list_dir,grep,search_tool --no-subagents --disable-web-search`; `-r <uuid>` quando `resume_session_id` presente (nunca `-c`, nunca `--session-id`); `--model`; `--effort <level>` quando thinking_level ≠ Default ou `model_profile` presente; `--json-schema <inline>` quando schema nativo pedido (força `--output-format json`, removendo os flags de streaming nesse ramo).
- `normalize_frame`: `classify` fechado espelhando [claude.rs:76-108](../crates/hvc-core/src/terminals/adapters/claude.rs) (system/init → System; result → Completed; error → Error; stream_event com `delta.type` contendo "tool" → ToolUse; assistant com bloco `tool_use` → ToolUse; resto → Message).
- `auth_probe_args`: `Some(&["models"])`.
- `classify_failure`: reaproveitar `classify_failure_text` como baseline; confirmar (Bloco G2) se o exit code de auth ausente tem um código fixo reconhecível, como o `41` do Gemini, para classificar sem depender só de texto de stderr.

### Bloco G2 — fixtures
- `crates/hvc-test-support/fixtures/cli/grok/current/`: `success.jsonl`, `tools.jsonl`, `unknown.jsonl`, `hostile.txt`, `models.json` — mesmo formato dos outros quatro backends, capturado por execução real controlada (custo mínimo, `--max-turns` baixo), nunca por invenção de payload.
- Teste dedicado `captures_no_thinking_or_thought_text` sobre a fixture `success.jsonl`, fixando a propriedade descrita na decisão #3.
- Teste `grok_prompt_never_touches_argv` — grep no `Vec<String>` de `spec.args` garantindo que o texto do prompt não aparece ali (só o caminho do arquivo temporário aparece).
- Teste de tool-use real (turno com `list_dir`/`read_file`) para fixar `tools.jsonl` — só deve ser executado contra um diretório de scratch isolado, nunca contra o workspace do app.

### Bloco G3 — descoberta e Windows
- Confirmar via teste que `grok.exe` cai no caminho de binário nativo (decisão #7), sem tocar `resolve_trusted_npm_shim`.
- `install_guidance(CliBackend::Grok)` ([terminals.rs:3327](../crates/hvc-core/src/terminals.rs)) → `("https://x.ai/cli", "irm https://x.ai/cli/install.ps1 | iex")` (comando literal do instalador oficial no Windows; nunca executado automaticamente pelo app).
- Ordenação de descoberta ([terminals.rs:2822-2828](../crates/hvc-core/src/terminals.rs)): adicionar `CliBackend::Grok => 5`.

### Bloco G4 — migração V38
- SQL descrito acima, com teste a partir de V1/V20/V36/V37.
- `sqlite3` local (já disponível nesta máquina) para validar `PRAGMA foreign_key_check` antes de commitar.

### Bloco G5 — catálogo de modelos
- `cli-model-catalog-v1.json` + `CliModelCatalogAssetBackends.grok` + `declarative_models()` (decisão #4).
- `assets/llm-model-catalog-v1.json` para o modo API (decisão #8), pendente dos IDs de modelo reais da API.
- Atualizar o teste `versioned_fixtures_cover_all_four_cli_model_contracts` ([model_catalog.rs:917](../crates/hvc-core/src/terminals/model_catalog.rs)) — nome e contagem de backends passam a cobrir cinco.

### Bloco G6 — modo API (xAI)
- Entrada em `profiles()` ([llm/profiles.rs:103](../crates/hvc-core/src/llm/profiles.rs)) conforme decisão #8.
- `SchemaFlavor::for_cli_backend` ([composition/schema.rs:75-82](../crates/hvc-core/src/orchestration/composition/schema.rs)) ganha o braço `CliBackend::Grok` — o compilador força isso assim que o enum crescer; decisão de qual flavor (`Canonical` por padrão, `Claude`-like se o Grok também rejeitar `$schema`) fica para quando o Bloco G1 testar `--json-schema` de verdade.

### Bloco G7 — UI (`hvc-ui`)
- `choices.rs:30-36` — `BACKENDS` vira `[CliBackend; 6]`, inclui `CliBackend::Grok`.
- `terminals.rs (llm)`/`app_model.rs`/`agents_view.rs` — nenhuma mudança hardcoded por backend individual foi encontrada nesses arquivos além de checagens `== CliBackend::Custom`; a listagem de opções já itera `CliBackend::ALL`/`BACKENDS` genericamente. Confirmar com `cargo check` (Bloco G0) se algum outro ponto exige um braço explícito.
- Ícone/cor do card do Grok na UI — a definir com o usuário (Bloco G0), já que não há convenção de ícone por vendor documentada até aqui além do texto do `label()`.

### Bloco G8 — documentação
- Ampliar [cli-threat-model.md](cli-threat-model.md) com uma seção datada `## Grok CLI — prompt materializado em arquivo temporário — <data>`, cobrindo o risco novo da decisão #1 (nunca criar uma tabela de ameaças nova — seguindo a mesma convenção de emenda datada já usada nesse arquivo em 2026-09-09/2026-09-10).
- No cérebro (`<home>\Documents\Nathan\HardVibeCode\aplicativo\Projeto Hard-Code`), emendar o ADR que já documenta os backends CLI/API (a localizar no Bloco G0 — provavelmente o mesmo ADR estendido pelo ADR-022/redesign de shell) com uma seção datada, em vez de abrir ADR novo, seguindo a convenção já confirmada pelo usuário.

### Bloco G9 — gate final
Mesmos comandos do gate CLI-G original:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
cargo build --workspace --release
```

mais: smoke opt-in com a conta real já autenticada nesta máquina (start/resume/cancel read-only), confirmação de que nenhum processo `grok.exe` órfão sobrevive a cancelamento/timeout/shutdown, e confirmação de que o arquivo temporário do prompt não sobrevive ao processo (nem em caminho de erro).

## Itens em aberto que precisam confirmação antes ou durante a implementação

1. **ACL do arquivo temporário do prompt** (decisão #1): reaproveitar `tempfile::NamedTempFile` sem hardening extra (igual ao `schema_file` hoje) ou adicionar ACL restrita ao usuário atual no Windows? Dado que o prompt pode conter código proprietário, a segunda opção é mais segura e relativamente barata de implementar.
2. **Vocabulário de `--reasoning-effort`/`--effort`** do Grok: `low`/`medium`/`high` (igual ao Claude) é uma suposição pelo nome da flag ser idêntico, não uma confirmação. Precisa de um teste real antes do Bloco G1 fechar `thinking_levels`.
3. **`--json-schema` do Grok aceita `$schema` de draft-2020-12** ou precisa da mesma remoção que o Claude faz? Só descobrível testando.
4. **IDs de modelo da API xAI** (`grok-4.7`/`grok-4.6` são nomes do CLI; a API pública provavelmente usa nomes diferentes) e se `json_object`/`json_schema`/`tool_calling` são suportados na API tal como descrito na decisão #8 — verificar contra `GET https://api.x.ai/v1/models` com uma chave real e a documentação oficial da xAI antes de fechar `profiles()` e `llm-model-catalog-v1.json`. Isso exige uma `XAI_API_KEY` — o usuário deve rodar essa checagem no próprio terminal; esta sessão não deve manipular a chave.
5. **Exit code de autenticação ausente** do Grok em modo headless — o usuário só confirmou o shape do erro (`{"type":"error","message":"..."}`, exit 1) para um prompt de teste; não há confirmação de um código fixo equivalente ao `41` do Gemini.
6. **Nome/branch de trabalho**: seguir a branch `main` direto ou abrir uma branch de feature dedicada (`feat/grok-cli-backend`), dado que `main` já tem um volume grande de mudanças não commitadas de outra entrega (ADR-022/composição v4, redesign de shell)?
7. **Ordem de entrega**: CLI e API juntos num único checkpoint, ou CLI primeiro (Blocos G0–G5, G7–G9) e API como uma entrega G6 separada depois?

## Sequência recomendada

| Ordem | Checkpoint | Depende de | Entrega |
|---:|---|---|---|
| 0 | G0 | itens em aberto 1–3, 6 resolvidos | enum, compilação exaustiva como checklist |
| 1 | G1 | G0 | `GrokAdapter` completo |
| 2 | G2 | G1 | fixtures reais + testes de segurança (thinking, argv) |
| 3 | G3 | G1 | descoberta e confirmação do launch target nativo |
| 4 | G4 | — (paralelizável com G1–G3) | migração V38 |
| 5 | G5 | G2, G4 | catálogo declarativo CLI |
| 6 | G6 | item em aberto 4 resolvido | provedor xAI no modo API |
| 7 | G7 | G0 | UI |
| 8 | G8 | G1–G7 | threat model + ADR do cérebro |
| 9 | G9 | tudo | gate final |

## Estratégia de commits sugerida

1. `feat(terminals): add grok as a sixth cli backend enum variant`
2. `feat(terminals): implement grok adapter with prompt-file materialization`
3. `test(terminals): add grok fixtures and thinking-leak regression coverage`
4. `feat(terminals): migrate backend check constraints to include grok (V38)`
5. `feat(terminals): add grok to the declarative cli model catalog`
6. `feat(llm): register xai as an openai-compatible provider profile`
7. `feat(ui): surface grok in backend selection and agent cards`
8. `docs(cli): extend threat model and cérebro adr with the grok integration`

Commits pequenos o bastante para revisão e bisect, como já é a prática deste projeto.
