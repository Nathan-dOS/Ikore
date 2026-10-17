# Sincronização de pulsações — checkpoint M4

Design references: [public summaries](../docs/adr/README.md); [historical archive context](README.md#historical-design-references).

Data: 2026-08-28. Repositório: `C:\Dev\Hard-Vibe-Code`, branch `gpui-migration`.
Base: `84c2163` (checkpoint M3). Implementação: `98afb91`.
Decisão: ADR-007 e REGRAS de sincronização do cérebro do projeto.

Status: **M4 concluído; G4 aprovado**. M5 e onboarding Brownfield não fazem parte desta entrega.

## Entrega

| Item | Implementação |
|---|---|
| M4.1 | `SharedDocumentCache` compartilhado por AppCore/runtime, isolado por raiz canônica. Conteúdo SHA-256 invalida o parsing; candidatos YAML/textuais carregam o hash selecionado, conferido novamente antes do prompt. `.txt` continua no fallback e não vira nota Markdown na UI. |
| M4.2 | Removida a reindexação incondicional pré-estágio. Publicações Markdown em `knowledge/obsidian/` sinalizam reconciliação; o supervisor detecta mudanças externas e coalesce deltas por raiz/path/modelo. Reindexação manual só registra intenção, sem scan ou rede fora do supervisor. |
| M4.3 | Inclusões/alterações, exclusões sem chamada LLM, troca/desativação de modelo, provider indisponível e vetores tardios são tratados. Hash, modelo, geração e ponteiro do delta protegem a aceitação. Vetor ausente/obsoleto não elimina candidato textual válido. |
| M4.4 | Role `learning_agent` com binding `Learning`, fora de `PulsationStage`. Input congelado com IDs de evidência, outputs aceitos, feedback, eventos, métricas e até 20 resumos anteriores do projeto. Schema estrito e validação de recorrência por duas pulsações distintas. |
| M4.5 | Demanda principal por conexão cancela Learning cooperativamente; admissão exige 5 s sem demanda, usando relógio injetado. Uma tentativa de provider por invocação, até um retry transitório e três chamadas por versão, incluindo preempções. Esgotamento persistente em `deferred_budget`. |
| M4.6 | Relatório aceito e outbox de classe background são persistidos juntos; host determina `knowledge/obsidian/learning/<pulse>-<epoch>.md`, com `type: learning_report`, `status: proposed` e proveniência. Exclusão do contexto normativo vale por tipo ou diretório, inclusive após mover uma proposta. UI mostra estado, consumo, diagnóstico, conteúdo e ações versionadas. |
| M4.7 | Falha, dispensa, desativação, conflito de nota ou orçamento esgotado não alteram `Done`, proprietário, epoch ou liberação da próxima pulsação. Outbox consultiva não participa da barreira principal. |

## Persistência e limites operacionais

- Migration aditiva **V13**: `learning_runs`, `embedding_sync_state`, `embedding_deltas` e classe da outbox. V1–V12 permanecem intactas. Backup com WAL/integridade antes do upgrade: `hard-vibe-code.pre-v13.<uuid>.db`. Intenções históricas `unavailable` não são convertidas nem reexecutadas.
- Preparação permanece **off** por default. Learning mantém o default aprovado **on somente para novas conclusões v2**, sem backfill ao habilitar. Embeddings não são ativados, modelos não são baixados e configurações de Chat/provider/concurrency não são alteradas automaticamente. Nenhum perfil real foi modificado durante a validação.
- Cache reaproveita parsing e relações, não um snapshot eterno dos arquivos: ainda enumera e lê hashes na recuperação. Catálogo de até 10.000 documentos, leitura individual de até 1 MiB e prefixo textual pesquisável de 8.192 caracteres por nota. Registro de raízes remove entradas ociosas a partir de oito, preservando caches em uso. Não há promessa de ganho de latência antes de M5.
- Leitura documental reutiliza `RepositoryContext`, ignores e exclusões; o publicador confere paths/ancestrais e rejeita symlink/junction. Notas inválidas/inacessíveis não se tornam contexto normativo. Hash do arquivo inteiro e conteúdo efetivamente lido são conferidos antes de usar uma seleção antiga.
- Reconciliação vetorial por lotes de até 128 notas, 8 MiB e 2 s cooperativos, com cursor persistido; enumeração do catálogo ocorre antes desse orçamento de leitura. Mudanças externas são verificadas a cada 30 s elegíveis. A aceitação de um lote confere a revisão do scan antes de substituir sinalização/deltas mais recentes. Não se presume exclusão a partir de lote parcial ou erro de I/O.
- Deltas concluídos não repetem embeddings sem mudança. Falha do provider fica local ao job; retry depende de alteração ou reindexação explícita. O pedido manual de retry persiste até o término do catálogo, inclusive entre lotes. Vetores de modelos antigos são preservados em suas próprias chaves e não são usados pelo modelo atual; garbage collection não faz parte de M4.
- Learning recebe até 16.000 caracteres operacionais, saída de até `min(max_output_tokens, 1.600)` e timeout do runtime. Trechos longos/descartados registram omissões; os limites de consulta (16 artifacts, 24 eventos, 20 resumos) também restringem o escopo. Ausência de achados é válida; citar evidência não equivale a comprovação semântica automática da recomendação.
- A reserva persistente da tentativa é conservadora: cancelamento/crash após a reserva pode consumir orçamento mesmo sem resposta faturável. Corrida de admissão `background_deferred` é devolvida ao orçamento. Preempção não penaliza a saúde do provider. O router não faz failover/retries ocultos de Learning dentro da mesma invocação.
- **Retry explícito cria nova versão**, com job novo e até três chamadas, somente quando não há relatório já aceito. A versão esgotada/falha conserva contadores e histórico; `total_calls` da versão nova carrega o consumo acumulado. Clique antigo/duplicado é recusado. Desligar/ligar ou reiniciar não renova orçamento.
- Conflito de relatório preserva arquivo humano e conteúdo consultivo persistido. A UI permite dispensar a publicação sem remover arquivos. Não há promoção automática a ADR/regra nem overwrite após conflito.
- Continua um executor principal e um background, com futures/tarefas bloqueantes aguardadas. Deltas podem pertencer só ao projeto/workspace, sem pulsação fictícia: ponto de extensão para manutenção Brownfield futura, não onboarding implementado.

## Evidências automatizadas

- `workspace_service::document_cache_tests`: reuso real de parsing entre clones/aliases canônicos, invalidação por bytes com tamanho igual, exclusão, isolamento de raízes, `.txt`, ignores e exclusão de propostas/deprecated como origem ou ponte de grafo.
- `agent_runtime::m4_context_tests`: vetor obsoleto não aumenta ranking, candidato sem vetor permanece, mudança entre seleção e leitura não entra no prompt e erro de embeddings conserva contexto textual.
- `orchestration/indexing_tests.rs`: coalescência, mudança/exclusão/modelo, resposta tardia controlada por barreiras, falha sem loop de retry, desativação em voo, retomada, propostas não indexadas e reindexação manual aguardando o único slot do supervisor.
- `llm/priority.rs`: relógio manual comprova 5 s, borda de 4.999 ms, isolamento entre conexões e sinal de preempção durante demanda principal, sem sleeps reais.
- `orchestration/learning_tests.rs`: intenção idempotente, ausência de backfill, input congelado/limitado, recorrência, três preempções, um retry transitório, nova versão explícita, dispensa/kill switch/recovery, publicação/replay e conflito que não seguram a próxima pulsação.
- `hvc-test-support/tests/learning_worker.rs`: router/parser e HTTP loopback reais, fora do fallback `cfg(test)`. Uma chamada, limite de 1.600 tokens, input limitado, proposta aceita e evidência inventada rejeitada sem reabrir `Done`. Credenciais somente em memória.
- Upgrade V12→V13 verifica backup restaurável e preservação de intenções históricas; regressões V9/V10 continuam. Teste GPUI adicional cobre ação Learning com geração exibida, clique duplicado e dispensa tipada.

## Gate

| Verificação | Resultado |
|---|---|
| Baseline M3 | 288 testes; worktree inicial limpo em `84c2163` |
| `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets --locked --offline --quiet -- -D warnings` | Aprovados; zero warnings do workspace |
| `cargo test --workspace --locked --offline --quiet` | **309 aprovados**: 195 core, 14 editor, 19 test-support, 62 UI e 19 integrações; zero falhas/ignorados |
| `cargo build --release --locked --offline -p hvc-app` / `scripts/smoke-windows.ps1` | Aprovados: janela encontrada, banco temporário inicializado, shutdown gracioso e exit code 0 |
| `git diff --check` | Aprovado |
| Auditoria técnica de licenças/fontes | Aprovada: 549 pacotes; nenhuma licença negada/ausente ou fonte em revisão |
| RustSec | Exit 0, zero vulnerabilidades; avisos preexistentes: 10 `unmaintained`, 2 `unsound`, 1 `yanked`; base `6420e39260b3d771b049954cf5d52b57e2118da4` |
| Distribuição externa | Bloqueio preexistente `zed-55470` mantido em `quality/distribution-policy.json` |

Nenhuma dependência/versão foi acrescentada ou atualizada; `Cargo.lock` permanece inalterado. Aviso de incompatibilidade futura de `proc-macro-error2 2.0.1` continua preexistente. Auditorias executadas pelos scripts individuais, sem liberar distribuição externa. Relatórios locais em `target/stage7/`. Smoke executado com instância oculta e dados temporários removidos pelo próprio script; nenhum dado do usuário foi removido. Esse smoke não substitui a matriz manual M5.

## Próximo marco

**M5:** matriz de regressão/manual, benchmark comparável e ativação controlada. Medir custo real de enumeração/revalidação, latência, reuso e preempções antes de prometer benefício ou alterar defaults. AST, centralidade, fronteira e adoção Brownfield continuam posteriores.
