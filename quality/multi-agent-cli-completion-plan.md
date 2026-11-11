# Plano CLI-G — conclusão do painel multiagente CLI/API

**Data:** 2026-09-02  
**Branch:** `gpui-migration`  
**Baseline Git:** `2cdf27d` mais a fatia vertical CLI ainda não commitada no worktree  
**Estado:** implementação concluída; gates automatizados aprovados em 2026-09-03  
**Prioridade:** bloqueador do fechamento do MVP  
**Plataforma de corte:** Windows x64

## Objetivo

Concluir a feature de Agents e Terminais como caminho operacional principal do aplicativo. Ao final, os sete agentes canônicos e os slots livres deverão executar por Codex, Claude Code, Gemini CLI, OpenCode ou executável Custom, com estado ao vivo confiável, isolamento de segurança, fallback para API somente após autorização explícita, histórico auditável, retomada nativa de sessão e experiência visual pronta para uso diário.

Este plano sucede a primeira fatia vertical implementada na V20. Ele não autoriza exibição ou persistência de raciocínio interno do modelo. A interface mostrará somente atividade observável, ferramentas declaradas, stdout/stderr sanitizados e respostas visíveis.

## Resultado da implementação

| Bloco | Estado | Evidência principal |
|---|---|---|
| 1. Sincronização, cancelamento e timeout | concluído | coordenador serial, reducer ao vivo, deadline, limites e Job Object testado com descendente |
| 2. Adapters e fixtures | concluído | adapters Codex/Claude/Gemini/OpenCode/Custom, corpus versionado e executor hermético |
| 3. Segurança, autenticação e Windows | concluído | snapshot seguro, redaction pré-persistência, probes fail-closed, shim npm validado e threat model |
| 4. API Mode e QA visual | código concluído; validações físicas parciais | bindings/viewer no Agents, buffers limitados e matriz visual aprovada em 1024/1280/1440; limitações do host registradas no relatório |
| 5. Retomada nativa e FreeForm | concluído | resume por ID com fencing, V22, CRUD/arquivo/histórico e limite de oito FreeForm |

Os gates `fmt`, `clippy -D warnings`, suíte completa e build release passaram. Smoke com contas reais, resoluções maiores que a superfície física disponível, escalas e foco de teclado permanecem como validações externas pré-corte, documentadas em `quality/reports/multi-agent-cli-visual-qa-2026-09-03.md`; não são substituídas por uma aprovação fictícia.

## Definição de concluído

A feature estará concluída somente quando todos os itens abaixo forem verdadeiros:

- os sete agentes canônicos executam ponta a ponta por cada um dos quatro backends oficiais;
- Custom executa apenas um alvo direto validado, sem composição de comando pelo aplicativo;
- eventos de início, streaming, cancelamento, timeout e término aparecem no card correto e na ordem correta;
- o deadline do pipeline é obedecido e nenhum processo ou descendente permanece após cancelamento, timeout ou shutdown;
- cada backend possui adaptador, fixtures versionadas, parser de eventos, extração de resposta final, descoberta, autenticação e retomada de sessão;
- saída sensível é redigida antes de entrar no banco, no event bus ou na UI;
- API Mode permite configurar bindings e apresenta viewer por agente equivalente ao painel CLI;
- sessões nativas nunca atravessam silenciosamente projeto, slot, backend, modelo ou raiz do workspace;
- slots FreeForm são persistentes, limitados e usam o mesmo runtime seguro dos slots canônicos;
- upgrades V1→versão final preservam projetos, checkpoints e históricos existentes;
- todos os gates CLI-G1 a CLI-G5 e o gate final CLI-G passam.

## Escopo e não objetivos

### Incluído

- Codex, Claude Code, Gemini CLI, OpenCode e Custom;
- execução automática do pipeline, execução manual one-shot e continuação explícita;
- slots canônicos e FreeForm;
- configuração híbrida CLI/API por projeto e agente;
- Windows x64 como plataforma obrigatória do primeiro corte;
- histórico local completo dentro dos limites documentados de uma execução;
- compatibilidade aditiva com a V20.

### Não incluído no gate do MVP

- emulador de terminal PTY genérico;
- shell arbitrário, PowerShell, `cmd /c`, Bash ou composição de strings de comando;
- edição de código diretamente pelas CLIs dentro desta feature; o pipeline continua recebendo saída estruturada e o host continua responsável por validar e publicar artefatos;
- entrada por voz;
- exposição de chain-of-thought ou conteúdo oculto;
- OpenCode ACP como transporte padrão. O desenho aceitará um transporte ACP futuro, mas o fechamento usa `opencode run --session` por ser suficiente para continuidade.

## Estado atual resumido

| Área | Já existe | Falta para produção |
|---|---|---|
| Domínio | Configuração por projeto/agente, runs, eventos e V20 | máquina de estados fechada, metadados de capacidade e sessão |
| Processos | `tokio::process`, stdin, stdout/stderr, Job Object no Windows | deadline, limites totais, ordenação única, testes de descendentes |
| Backends | argumentos iniciais dos quatro CLIs e Custom | adaptadores independentes, parsers/fixtures e compatibilidade versionada |
| Segurança | ambiente reduzido, read-only e sanitização ANSI | redação de segredos, snapshot seguro, hardening de shims e probes |
| Fallback | decisão persistida Usar API/Tentar CLI/Cancelar | matriz completa de erros e recuperação visual consistente |
| UI CLI | sete cards, configuração, histórico e ações | reducer ao vivo, follow-tail, diagnósticos e catálogo de modelos |
| UI API | tabela de bindings e resumo de atividade | edição inline e viewer completo por agente |
| Sessões | coluna `native_session_id` reservada | captura, resume, fencing e UX de continuidade |
| FreeForm | colunas reservadas na V20 | domínio, repositório, runtime e UI |

## Invariantes não negociáveis

- **Sem fallback silencioso:** falha de CLI só usa API após autorização durável para a combinação pulsação + agente.
- **Cancelamento correto:** Stop automático cancela a pulsação inteira; Stop manual cancela somente a execução manual.
- **Sem shell construído:** prompts nunca entram em argumentos; são enviados por stdin. Custom não aceita scripts de shell.
- **Fail closed:** versão, autenticação, sandbox ou capacidade desconhecida desabilita Start e oferece diagnóstico.
- **Configuração isolada:** configurações, histórico, sessões e slots pertencem a um projeto.
- **Publicação pelo host:** a CLI não publica artefatos diretamente; saída estruturada passa pelos contratos e fencing existentes.
- **Sem segredos em repouso:** conteúdo redigido antes de persistência; chaves de API continuam no keyring.
- **Sem raciocínio oculto:** rótulo canônico da UI será “ATIVIDADE E SAÍDA”.
- **Histórico explícito:** exclusão exige confirmação e nunca apaga a sessão nativa do provedor sem uma segunda ação específica.
- **Migração aditiva:** não reconstruir tabelas referenciadas quando `ALTER TABLE` ou tabelas complementares resolverem.

## Arquitetura alvo

O módulo atual `terminals.rs` será separado progressivamente sem alterar a API pública de uma só vez:

```text
hvc-core/src/terminals/
├── mod.rs
├── domain.rs
├── repository.rs
├── discovery.rs
├── security.rs
├── executor.rs
├── session.rs
└── adapters/
    ├── mod.rs
    ├── codex.rs
    ├── claude.rs
    ├── gemini.rs
    ├── opencode.rs
    └── custom.rs
```

Contrato central proposto:

```rust
trait CliAdapter: Send + Sync {
    fn backend(&self) -> CliBackend;
    fn capabilities(&self, version: &CliVersion) -> CliCapabilities;
    fn discovery_probe(&self, target: &LaunchTarget) -> ProbeSpec;
    fn auth_probe(&self, target: &LaunchTarget) -> Option<ProbeSpec>;
    fn build_start(&self, request: &CliStartRequest) -> Result<CliProcessSpec, TerminalError>;
    fn build_resume(&self, request: &CliResumeRequest) -> Result<CliProcessSpec, TerminalError>;
    fn parse_frame(&self, stream: OutputStream, frame: &[u8]) -> Vec<NormalizedCliEvent>;
    fn finish(&self, state: AdapterRunState) -> Result<CliCompletion, TerminalError>;
}
```

`CliProcessSpec` continuará usando executável e vetor de argumentos separados. O executor não conhecerá flags específicas de fornecedor. O adapter não poderá abrir processo ou gravar no banco.

Todos os eventos de stdout, stderr e lifecycle passarão por um único `TerminalEventCoordinator`. Somente esse coordenador atribuirá sequência, aplicará redaction, persistirá e publicará no event bus.

---

# Bloco 1 — sincronização ao vivo, cancelamento e timeout

## CLI-1.1 — máquina de estados do run

- Formalizar transições permitidas:
  - `queued → starting → running`;
  - `running → completed | failed | cancelled | awaiting_fallback`;
  - `queued | starting | running → interrupted` somente em recovery/shutdown;
  - estados terminais não podem ser reabertos.
- Aplicar compare-and-set no repositório para impedir término duplicado ou tardio.
- Persistir `termination_reason`, `deadline_at`, versão da CLI, versão do adapter e contadores de bytes/eventos.
- Emitir evento de lifecycle em toda transição, inclusive `queued`, `starting`, `cancelled` e `timeout`.

## CLI-1.2 — reducer ao vivo da UI

- Enriquecer `TerminalEventEnvelope` com o snapshot mínimo do run: status, origem, pulsação, timestamps e backend.
- Fazer `AgentsModel` inserir um run desconhecido no primeiro evento e selecioná-lo quando ele se tornar ativo.
- Atualizar o status local sem depender de refresh completo.
- Tratar `Completed`, `Failed`, `Cancelled`, `Interrupted` e `AwaitingFallback` como término visual.
- Executar ressincronização persistida quando houver lacuna ou evento fora de ordem.
- Manter seleção histórica do usuário; auto-selecionar somente quando o run selecionado não estiver ativo ou quando o usuário estiver seguindo a execução atual.
- Implementar follow-tail com opção de pausar ao rolar para cima.

## CLI-1.3 — coordenador ordenado de eventos

- Substituir a persistência concorrente direta de stdout/stderr por um canal MPSC por execução.
- O coordenador atribui `sequence`, redige, limita, persiste e emite de forma serial.
- Drenar stdout e stderr antes do evento terminal.
- Persistência falhou: cancelar o processo e marcar run como `failed/persistence_error`; nunca continuar sem auditoria.
- Evento duplicado ou tardio deve ser idempotente.

## CLI-1.4 — deadline e limites

- Pipeline usa exatamente `LlmRequest.deadline`; não cria um timeout independente mais longo.
- Execução manual recebe default de 30 minutos e limite configurável entre 1 e 120 minutos.
- Probes usam timeout de 3 segundos e `kill_on_drop`.
- Limites iniciais por execução:
  - frame individual sanitizado: 64 KiB;
  - buffer sem newline: 256 KiB;
  - saída persistida total: 32 MiB;
  - máximo de 100.000 eventos;
  - ao atingir limite, registrar um único evento `output_limit`, encerrar a árvore e falhar de maneira diagnosticável.
- O limite deverá ser centralizado e poderá ser ajustado somente com benchmark registrado.

## CLI-1.5 — cancelamento e shutdown

- Diferenciar `user_cancel`, `pulse_cancel`, `timeout`, `shutdown`, `superseded` e `output_limit`.
- Cancelamento do usuário nunca abre fallback para API.
- Timeout e falha genuína podem abrir fallback explícito conforme matriz do Bloco 2.
- Windows: fechar Job Object com `KILL_ON_JOB_CLOSE` e aguardar processo raiz e descendentes.
- Shutdown do `AppCore`: cancelar, drenar eventos, aguardar tasks e então fechar o banco.
- Após restart, runs não terminais viram `interrupted`; retomada conversacional será decisão explícita do Bloco 5.

## Testes CLI-1

- run iniciado enquanto Agents já está aberto aparece e transmite no card correto;
- stdout/stderr intercalados preservam sequência total;
- cancelamento manual termina somente o run;
- Stop automático exige confirmação e cancela todos os jobs da pulsação;
- cancelamento não cria fallback;
- deadline encerra raiz e descendente;
- shutdown não deixa processo órfão;
- output limit é determinístico e não cresce o SQLite indefinidamente;
- evento terminal sempre aparece depois do último frame;
- lacuna de evento solicita snapshot persistido.

## Gate CLI-G1

- todos os testes CLI-1 passam no Windows;
- nenhum processo permanece 2 segundos após cancelamento, timeout ou shutdown;
- latência evento→modelo GPUI P95 ≤ 100 ms com 1.000 eventos locais;
- nenhuma execução visual permanece `Running` após estado persistido terminal.

---

# Bloco 2 — adaptadores e fixtures das quatro CLIs

## CLI-2.1 — contrato de capacidades

Cada adapter declarará, por versão detectada:

- saída `json` ou `jsonl`;
- structured output nativo ou instrução de schema no prompt;
- suporte a session ID e resume;
- níveis de thinking/effort aceitos;
- comando de autenticação não interativo;
- modo read-only efetivamente disponível;
- catálogo de modelos quando suportado;
- códigos de erro reconhecidos.

Capacidade ausente não será simulada. A UI ocultará ou desabilitará o controle e explicará a limitação.

## CLI-2.2 — CodexAdapter

- Manter `codex exec` não interativo, prompt por stdin, `--json`, `--output-schema`, approval `never` e sandbox `read-only`.
- Parsear eventos JSONL por tipo e extrair resposta final, uso quando disponível, erro e identificador nativo da sessão.
- Implementar `codex exec resume SESSION_ID` com prompt por stdin.
- Fixar fixtures por versão mínima suportada e versão corrente validada.
- Não usar `--last` no pipeline: sempre retomar por ID explícito e raiz conferida.

## CLI-2.3 — ClaudeAdapter

- Usar print mode, `stream-json`, `verbose`, schema nativo e permission mode `plan`.
- Restringir ferramentas ao conjunto aprovado para leitura; MCP permanece desabilitado no MVP.
- Parsear `system`, `assistant`, `user/tool_result`, partial messages, `result`, erro, usage e `session_id`.
- Implementar `--resume SESSION_ID -p` e autenticação via `claude auth status`.
- Validar `--effort` contra capacidades do modelo/versão.

## CLI-2.4 — GeminiAdapter

- Usar modo não interativo, `stream-json` e approval mode `plan`.
- Parsear init, message delta, tool calls/results, error, stats e session UUID.
- Implementar `--resume SESSION_ID` somente após confirmar que a versão suporta retomada não interativa com o mesmo formato de saída.
- Classificar exit code 41 como autenticação necessária e demais códigos documentados em erros tipados.
- Thinking fica indisponível quando não existir flag estável equivalente.

## CLI-2.5 — OpenCodeAdapter

- Usar `opencode run --format json`, permissões read-only explícitas e download automático de LSP desabilitado.
- Parsear eventos JSON, saída final, tools, usage, erro e session ID.
- Implementar `--session SESSION_ID`; `--continue` não será usado pelo pipeline.
- Verificar autenticação com `opencode auth list` e modelos com `opencode models` quando a versão suportar saída analisável.
- Não habilitar `--thinking` para exibir conteúdo oculto; `variant` poderá representar effort quando suportado.

## CLI-2.6 — CustomAdapter

- Executável absoluto e argumentos exatos, sem expansão, interpolação ou shell.
- Prompt somente por stdin.
- stdout/stderr são texto visível; JSONL opcional apenas quando o usuário declarar o contrato.
- Structured output continua validado pelo host.
- Custom não oferece retomada nativa genérica; poderá declarar template de resume somente em uma evolução com contrato próprio e nova confirmação de risco.

## CLI-2.7 — fixtures versionadas

Estrutura prevista:

```text
crates/hvc-test-support/fixtures/cli/
├── codex/<version>/
├── claude/<version>/
├── gemini/<version>/
└── opencode/<version>/
```

Cada backend terá fixtures para:

- sucesso mínimo;
- streaming fragmentado em fronteiras UTF-8 arbitrárias;
- tool use e tool result;
- structured output válido e inválido;
- autenticação ausente;
- rate limit/indisponibilidade;
- flag ou versão incompatível;
- session ID e resume;
- stderr com exit zero e não zero;
- linha desconhecida preservada como evento observável;
- conteúdo com ANSI, OSC, controles e padrões de segredo.

As fixtures não poderão conter credenciais, paths de usuário ou transcripts reais não sanitizados.

## CLI-2.8 — executáveis-fixture

- Criar um helper Rust compilado no workspace de testes capaz de:
  - validar argv e stdin;
  - emitir stdout/stderr com atrasos controlados;
  - fragmentar UTF-8 e JSONL;
  - travar até cancelamento;
  - criar descendente;
  - retornar exit codes configurados;
  - simular sessão e resume.
- Testar o executor real, não uma função substituta `cfg(test)`.

## Matriz de fallback

| Falha | Retry automático | Oferecer API | Resultado |
|---|---:|---:|---|
| usuário cancelou | não | não | pulsação cancelada ou run manual cancelado |
| shutdown/preempção | recuperação existente | não | interrupted |
| timeout | não | sim | checkpoint explícito |
| autenticação ausente | não | sim | checkpoint + ação de login |
| versão incompatível | não | sim | checkpoint + atualização orientada |
| saída/schema inválido | não | sim | checkpoint com diagnóstico seguro |
| exit temporário conhecido | no máximo uma vez | sim depois | tentativa auditada |
| executable ausente | não | sim | checkpoint + instalação orientada |
| persistência/redaction falhou | não | não | fail closed |

## Gate CLI-G2

- 100% das fixtures passam em todos os adapters;
- executor-fixture prova stdin, streaming, cancelamento, timeout e descendentes;
- saída estruturada passa pelo mesmo schema/fencing usado pela API;
- smoke real opt-in conclui uma solicitação read-only em cada CLI instalada;
- nenhuma credencial real é necessária na CI obrigatória.

---

# Bloco 3 — segurança, autenticação e Windows

## CLI-3.1 — threat model

Documentar ameaças e controles para:

- prompt injection proveniente do workspace;
- leitura de `.env`, chaves, certificados e arquivos ignorados;
- vazamento em stdout/stderr, logs, banco ou screenshots;
- command injection por modelo, argumentos ou shims;
- processo descendente sobrevivente;
- troca de executável após configuração;
- sessão retomada no projeto errado;
- configuração do usuário que enfraquece sandbox/permissions;
- plugins, hooks, MCPs e arquivos de instrução descobertos automaticamente pelos CLIs.

## CLI-3.2 — redaction antes da persistência

- Centralizar `SecretRedactor` antes de `TerminalRepository::append_event` e antes do event bus.
- Reutilizar padrões já existentes e cobrir tokens Bearer, API keys conhecidas, PEM, credenciais URL, variáveis sensíveis e paths de keyring.
- Persistir apenas contagem e categorias de redaction, nunca o valor original.
- Aplicar também a diagnósticos, versão/probes e payloads de erro.
- Se o redactor falhar, cancelar e não persistir o frame original.

## CLI-3.3 — visão segura do workspace

- Default: executar os backends oficiais contra um snapshot temporário read-only produzido pelo host.
- O snapshot reutiliza as regras de `RepositoryContext`: exclui segredos, ignorados, binários, artefatos gerados e paths que escapem da raiz.
- Preservar paths relativos necessários ao modelo e registrar hash do manifesto do snapshot.
- Acesso ao workspace real será opt-in por projeto, com aviso persistente e nova confirmação quando a raiz mudar.
- Plugins, hooks, MCP e memória/configuração de projeto dos fornecedores ficam desabilitados quando houver flag suportada; incapacidade de desabilitar aparece na matriz de capacidade.
- Custom é tratado como executável local confiado pelo usuário e recebe aviso mais forte; ainda usa o snapshot seguro por padrão.

## CLI-3.4 — resolução segura no Windows

- Representar lançamento como `LaunchTarget::NativeExe` ou `LaunchTarget::TrustedNpmShim`.
- Preferir binário nativo assinado/identificável quando existir.
- Para `.cmd`/`.bat` de pacote oficial:
  - não encaminhar texto por concatenação para `cmd.exe`;
  - resolver o wrapper conhecido para `node.exe` + entrypoint JavaScript canônico;
  - conferir caminho absoluto, package esperado e formato permitido;
  - rejeitar wrapper desconhecido ou alterado.
- Custom continuará rejeitando `.cmd`, `.bat`, `.ps1` e atalhos ambíguos.
- Canonicalizar executável antes do spawn e comparar identidade/timestamp com a descoberta; mudança exige novo probe.
- Job Object continua obrigatório; teste deve comprovar término do neto do processo.

## CLI-3.5 — descoberta, cache e autenticação

- Executar probes em paralelo, limitados por semáforo, com cache de 5 minutos e botão Atualizar.
- Probes usam ambiente reduzido, stdin nulo, stdout/stderr limitados e kill on timeout.
- Estados: `missing`, `probing`, `available`, `auth_required`, `unsupported_version`, `capability_missing`, `probe_failed`.
- Verificações:
  - Codex: `login status`;
  - Claude: `auth status` em formato estruturado;
  - Gemini: probe headless seguro e classificação dos exit codes documentados, sem disparar login interativo;
  - OpenCode: `auth list`, com seleção explícita do provedor quando necessário.
- Nunca abrir browser de login automaticamente. A UI poderá abrir a documentação ou copiar o comando após ação do usuário.

## CLI-3.6 — versões suportadas

- Parser semântico tolerante a prefixos, mas rejeitando saída vazia/desconhecida.
- Tabela versionada `CliCompatibilityRule` dentro de cada adapter.
- Guardar `detected_version`, `adapter_contract_version` e capabilities no run.
- Suporte inicial: versão mínima comprovada pelas fixtures e versão corrente validada no gate; versões mais novas desconhecidas entram como `capability_missing` até probe de contrato passar.
- O aplicativo não executará automaticamente comando de atualização.

## CLI-3.7 — Custom hardening

- Validar arquivo existente, regular, absoluto, canônico e executável.
- Limitar quantidade e tamanho de argumentos; proibir NUL e argumentos reservados pelo host.
- Alterar executável ou argumentos zera `custom_risk_ack` por compare-and-set.
- Mostrar hash SHA-256, caminho canônico e data de modificação na confirmação.
- Revalidar identidade imediatamente antes do spawn para reduzir TOCTOU.

## Testes CLI-3

- corpus de segredos prova zero valor original no SQLite, eventos e mensagens de erro;
- snapshot exclui `.env`, `.git`, certificados, chaves privadas, ignorados e junction escape;
- shim oficial conhecido resolve; shim alterado, path com metacaractere e wrapper arbitrário falham fechados;
- timeout de probe não deixa processo;
- descoberta paralela respeita limite e cache;
- todas as combinações de auth aparecem no estado correto;
- troca de executável invalida capability e confirmação Custom;
- sessão ou executável de outro projeto é rejeitado.

## Gate CLI-G3

- threat model revisado e sem risco crítico/alto aberto;
- zero segredo do corpus em banco/log/event bus;
- Windows shim, Job Object e junction tests passam em Windows x64;
- versão ou capacidade desconhecida nunca habilita Start;
- modo CLI pode ser desligado globalmente por kill switch sem impedir API Mode.

---

# Bloco 4 — API Mode completo e QA visual

## CLI-4.1 — configuração API dentro de Agents

- Tabela com headers explícitos: Agente, Modo, Rota, Conexão, Provedor, Modelo, Key, Fallbacks e Status.
- Editar binding por agente sem sair da tela:
  - selecionar/criar rota;
  - selecionar conexão e modelo;
  - visualizar cadeia de fallback;
  - abrir o fluxo seguro de cadastro de chave no keyring;
  - testar conexão;
  - salvar com versão otimista.
- Reutilizar serviços e validações de Settings; não duplicar armazenamento de segredo.
- Link “Configuração avançada” navega para Settings preservando agente/rota selecionados.

## CLI-4.2 — viewer API por agente

- Reutilizar o componente visual de atividade dos cards CLI.
- Mostrar lifecycle de rota, tentativas, provider/model, tool events permitidos, token output visível, conclusão e falha.
- Associar evento ao projeto, pulsação, job, agente e execução.
- Persistir viewer API no histórico unificado ou apresentar link para o log persistido correspondente.
- Toggle interrompe somente a renderização/coleta auxiliar da UI; não interfere na execução.
- Texto permanente: “Exibe somente atividade e saída visível; raciocínio interno não é coletado.”

## CLI-4.3 — UX dos cards CLI

- Autoscroll/follow-tail com indicador de novas linhas.
- Filtros por `activity`, `tool`, `stdout`, `stderr`, `warning` e `error`.
- Copiar evento, copiar diagnóstico e exportar histórico redigido.
- Mostrar origem, duração, exit code, versão, modelo, executável e ID curto de sessão.
- Links oficiais de instalação clicáveis e botão para copiar comando; nunca executar instalação automaticamente.
- Botão Atualizar diagnóstico sem bloquear o restante da tela.
- Catálogo de modelos por adapter quando disponível; manter “Modelo customizado” como fallback validado.
- Controles indisponíveis durante run mostram a razão, não apenas aparência disabled.

## CLI-4.4 — responsividade e acessibilidade

- Layouts alvo: 1 coluna, 2 colunas e expansão até 4 colunas conforme largura útil.
- Ações principais permanecem visíveis com nomes, paths e erros longos.
- Navegação completa por teclado, foco visível e ordem previsível.
- Status não depende apenas de cor; sempre inclui texto/ícone.
- Validar escala Windows 100%, 125% e 150%, temas claro/escuro e contraste.
- Testar Unicode, IME, seleção e cópia de saída.

## CLI-4.5 — QA visual reproduzível

Criar cenários GPUI com banco e eventos determinísticos:

- todos idle;
- sete estados diferentes simultaneamente;
- streaming intenso;
- CLI ausente/auth necessária/versão incompatível;
- fallback pendente;
- API Mode com e sem key;
- Custom aguardando confirmação;
- histórico longo e erro longo;
- sessão retomável;
- slots FreeForm do Bloco 5.

Capturar e revisar nas resoluções 1024×768, 1280×720, 1440×900, 1920×1080 e 2560×1440. O relatório deve registrar screenshots, escala, tema e resultado de cada cenário.

## Gate CLI-G4

- configuração de binding e key pode ser concluída sem sair de Agents;
- viewer API mostra streaming completo por agente sem conteúdo oculto;
- zero overflow que esconda Start/Stop/fallback nas resoluções alvo;
- abertura da view com discovery em cache P95 ≤ 100 ms;
- atualização de 1.000 eventos mantém interação responsiva e uso de memória limitado;
- checklist manual de teclado, escala e temas aprovado.

---

# Bloco 5 — retomada nativa e slots FreeForm

## CLI-5.1 — SessionSlot como identidade

Introduzir o contrato:

```rust
pub enum SessionSlot {
    Canonical { agent_role: AgentRole },
    FreeForm { id: String },
}
```

- APIs de start, cancel, events, history e resume recebem `project_id + SessionSlot`.
- `AgentRole` deixa de ser a chave universal do runtime.
- Slots canônicos continuam fixos e ordenados.
- Slots FreeForm têm ID UUID, label, backend, modelo, thinking, política de sessão e versão.
- Índices parciais impedem mais de um run ativo por slot.

## CLI-5.2 — política de sessão

```rust
pub enum SessionPolicy {
    FreshPerRun,
    ResumePerPulsation,
    ResumeExplicit,
}
```

- Pipeline usa `FreshPerRun` por padrão.
- `ResumePerPulsation` só retoma a sessão do mesmo projeto, slot, pulsação, backend, modelo e fingerprint de workspace.
- Runs manuais e FreeForm usam `ResumeExplicit`: a UI oferece “Nova sessão” ou uma sessão compatível escolhida.
- Retry de CLI não retoma automaticamente depois de entrega incerta do prompt; exige prova do adapter ou começa nova tentativa explicitamente registrada.
- Trocar backend/modelo ou alterar raiz invalida a sessão para resume.

## CLI-5.3 — captura e retomada por backend

- Capturar o ID somente de evento estruturado reconhecido pelo adapter.
- Persistir `native_session_id`, `session_fingerprint`, run pai e razão de continuidade.
- Comandos por ID explícito:
  - Codex: `codex exec resume SESSION_ID`;
  - Claude: `claude --resume SESSION_ID -p`;
  - Gemini: `gemini --resume SESSION_ID` em modo não interativo comprovado;
  - OpenCode: `opencode run --session SESSION_ID`.
- Nunca usar “latest/last/continue” no pipeline, pois pode selecionar conversa de outro contexto.
- Sessão ausente ou incompatível produz decisão “Iniciar nova” ou “Cancelar”; não faz fallback silencioso.
- O aplicativo não tenta reanexar um PID após restart. Ele marca o run como interrompido e pode continuar a conversa pelo ID nativo.

As formas de resume acima devem ser conferidas novamente contra documentação e fixtures no momento da implementação. Em 2026-09-02, a documentação oficial confirma resume por ID para os quatro backends.

## CLI-5.4 — slots FreeForm

- Botão “+ Novo agente livre” na área separada dos sete canônicos.
- Máximo inicial de 8 slots FreeForm ativos/configurados por projeto.
- Criar, renomear, reordenar, arquivar e excluir com confirmação.
- Mesmo seletor de backend/modelo/thinking e mesmo painel de atividade.
- Execução manual e continuidade nativa; não participa do pipeline automático.
- Custom disponível com o mesmo hardening e nova confirmação por slot.
- Exclusão do slot remove somente metadados/histórico HVC após confirmação; exclusão da sessão no fornecedor é ação separada e opt-in.

## CLI-5.5 — V22 e recovery

- Criar `terminal_slots` e relacionar runs por `slot_id` preservando compatibilidade com `agent_role` da V20.
- Backfill determinístico dos sete slots canônicos por projeto.
- Adicionar relação run pai/filho e fingerprint de sessão.
- Recovery mantém sessões compatíveis disponíveis e runs ativos como `interrupted`.
- Upgrade repetido é idempotente e preserva FKs de pulsação, jobs, agent runs e checkpoints.

## Testes CLI-5

- resume por ID usa a sessão correta para cada backend;
- `last/latest/continue` nunca aparece no argv do pipeline;
- sessão de outro projeto, raiz, slot, backend ou modelo é rejeitada;
- restart permite continuação explícita sem reviver o PID antigo;
- prompt não é duplicado em retry de entrega incerta;
- sete slots canônicos são backfilled uma única vez;
- FreeForm CRUD, ordenação, limite, histórico e cancelamento;
- exclusão de slot não apaga sessão nativa por acidente;
- migrações V1→V22 e V20→V22 preservam todos os dados.

## Gate CLI-G5

- quatro CLIs retomam sessão por ID em fixture e smoke opt-in;
- fencing de sessão tem cobertura completa;
- FreeForm funciona com backend oficial e Custom;
- recovery após restart é determinístico;
- nenhum run ativo duplicado por slot.

---

## Migrações previstas

### V21 — runtime seguro e observável

- metadados de versão/capacidade do adapter;
- deadline, termination reason e contadores;
- categorias/contagem de redaction;
- fingerprint do executável e do snapshot do workspace;
- índices de runs ativos e histórico;
- event kind de lifecycle/timeout/output limit, preferencialmente por nova coluna `event_type_version` ou tabela complementar para evitar reconstrução arriscada.

### V22 — sessões e slots

- `terminal_slots`;
- relação run→slot;
- run pai/continuação;
- fingerprint de sessão;
- política de sessão e estado arquivado do slot;
- backfill dos canônicos.

Cada migração deverá ter testes a partir da versão imediatamente anterior e a partir de fixtures legadas V1/V9/V19/V20. `PRAGMA foreign_key_check` deve retornar zero.

## Sequência de implementação e checkpoints

| Ordem | Checkpoint | Dependência | Entrega principal |
|---:|---|---|---|
| 0 | CLI-0 | — | congelar contratos, threat model inicial e helper de processo |
| 1 | CLI-1 | CLI-0 | lifecycle ao vivo, ordenação, deadline, cancelamento e V21 base |
| 2 | CLI-2A | CLI-1 | trait de adapter + Codex e Claude |
| 3 | CLI-2B | CLI-2A | Gemini, OpenCode, Custom e matriz de fixtures |
| 4 | CLI-3 | CLI-2B | redaction, snapshot seguro, auth, versão e Windows shims |
| 5 | CLI-4A | CLI-1/2 | API Mode editável e viewer unificado |
| 6 | CLI-4B | CLI-3/4A | responsividade, acessibilidade, performance e QA visual |
| 7 | CLI-5A | CLI-2/3 | SessionSlot, políticas, resume e V22 |
| 8 | CLI-5B | CLI-5A | FreeForm, recovery e QA integrado |
| 9 | CLI-G | todos | release build, smoke Windows, auditoria e documentação final |

Não iniciar um checkpoint se o gate anterior estiver vermelho. CLI-4A pode ser desenvolvido depois que o contrato de eventos de CLI-1 estiver congelado, mas só fecha após CLI-3.

## Estratégia de commits

1. `docs(cli): define completion gates and threat model`
2. `refactor(cli): extract runtime domains and adapter contract`
3. `fix(cli): serialize lifecycle events and enforce deadlines`
4. `test(cli): add hermetic process executor matrix`
5. `feat(cli): add codex and claude adapters`
6. `feat(cli): add gemini opencode and custom adapters`
7. `feat(cli): redact output and harden windows launch targets`
8. `feat(agents): complete api routing and activity viewer`
9. `test(agents): record responsive visual qa gate`
10. `feat(cli): resume native sessions by explicit id`
11. `feat(agents): add persistent freeform slots`
12. `test(cli): record final CLI-G release gate`

Commits devem permanecer pequenos o suficiente para revisão e bisect. Migração e código que depende dela entram no mesmo checkpoint ou em commits consecutivos que sempre compilam.

## Matriz de testes final

| Camada | Obrigatório |
|---|---|
| Unidade | domínio, transições, parser, redactor, argv, capability e fencing |
| Integração hermética | executor real + helper child/grandchild + SQLite + event bus |
| Migração | V1/V9/V19/V20→final, idempotência e FK check |
| UI GPUI | reducer, gaps, ações, bindings, viewer, histórico, sessões e FreeForm |
| Visual | resoluções, escalas, temas, textos longos e sete estados simultâneos |
| Performance | discovery, event→paint, 1.000/100.000 eventos e histórico grande |
| Segurança | corpus de segredos, path escape, junction, shim, TOCTOU e Custom |
| Smoke real | quatro CLIs instaladas, auth controlada, start/resume/cancel read-only |
| Release | build release, startup/shutdown e kill switch CLI |

## Comandos do gate final

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
cargo build --workspace --release
```

Além desses comandos:

- executar matriz Windows de processos e shims;
- executar QA visual e anexar relatório em `quality/reports/`;
- executar auditorias de dependências/licenças já adotadas pelo projeto;
- executar smoke com banco e workspace isolados;
- verificar que o kill switch muda todos os agentes para API sem alterar configurações persistidas;
- confirmar zero processo órfão e zero segredo do corpus nos artefatos de teste.

## Critérios de bloqueio do release

Qualquer item abaixo bloqueia o fechamento do MVP:

- fallback para API sem clique explícito;
- processo órfão após Stop, timeout ou shutdown;
- card exibindo estado diferente do SQLite;
- segredo não redigido em banco, log ou UI;
- versão/capacidade desconhecida tratada como disponível;
- sessão retomada com fingerprint incompatível;
- Custom iniciando sem confirmação atual;
- quebra de upgrade ou foreign key;
- ação principal inacessível em uma resolução alvo;
- teste hermético ou smoke Windows vermelho;
- P0 ou P1 conhecido sem decisão formal.

## Rollback e operação segura

- Adicionar kill switch global `cli_runtime_enabled`, default ligado somente após CLI-G3.
- Quando desligado, nenhum processo CLI novo inicia; runs existentes recebem cancelamento coordenado.
- Configurações e histórico permanecem preservados.
- Pipeline usa API apenas conforme configuração normal; falha CLI já aberta continua exigindo decisão explícita.
- Migrações são forward-only; rollback operacional desliga o runtime, não remove tabelas.
- Diagnóstico deve exportar somente metadados e saída redigida.

## Documentação de fechamento

Ao concluir CLI-G:

- atualizar o documento de produto `03-MVP-Multi-Agent-CLI-Painel-Terminais.md` para **Concluída**;
- substituir exemplos de flags antigos pela matriz gerada pelos adapters;
- documentar instalação, login, versões suportadas, Custom e recuperação;
- publicar threat model e relatório visual;
- registrar versões reais usadas no smoke, sem credenciais ou paths pessoais;
- documentar explicitamente que atividade visível não é chain-of-thought.

## Referências primárias verificadas em 2026-09-02

- Codex CLI: <https://developers.openai.com/codex/cli/reference/>
- Claude Code CLI: <https://code.claude.com/docs/en/cli-usage>
- Gemini CLI configuration: <https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/configuration.md>
- Gemini session management: <https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/session-management.md>
- OpenCode CLI: <https://dev.opencode.ai/docs/cli/>
- OpenCode permissions: <https://opencode.ai/docs/permissions>

As referências são informativas, não substituem fixtures. Cada adapter deverá registrar a versão observada e falhar fechado quando a capacidade efetiva não corresponder ao contrato testado.
