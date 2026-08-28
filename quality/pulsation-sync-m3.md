# Sincronização de pulsações — checkpoint M3

Data: 2026-08-28. Repositório: `C:\Dev\Hard-Vibe-Code`, branch `gpui-migration`.
Base: `66abe0d` (checkpoint M2). Implementação: `031420d`.
Decisão: ADR-007 e REGRAS de sincronização do cérebro do projeto.

Status: **M3 concluído; G3 aprovado**. Preparação continua desligada por default; não é autorização de rollout M5 nem onboarding Brownfield.

## Entrega

| Item | Implementação |
|---|---|
| M3.1 | `LexicalCodeContextService` reutiliza `RepositoryContext` para Specification sob demanda e PreSpec. A primeira tarefa recebe evidências de código sem vault, antecessora ou AST. Hash, linhas e snippet derivam dos mesmos bytes; cobertura e referências não resolvidas são explícitas. |
| M3.2 | TaskRefine local PT/EN, NFC Unicode, dicionário/corpus `task-refine-v1`, pesos fixos e score 0–100. Descrição original preservada, limite de 16.000 caracteres, revisão/dispensa tipadas e histórico por input. Settings persistidos por upsert; threshold default 40. |
| M3.3 | A lane background M2 executa Intake do sucessor imediato e depois PreSpec local, uma geração por revisão/espera. Não adquire propriedade do principal nem publica arquivos; pacote é artifact interno `code_context` sem path, de autoria exclusiva do host. |
| M3.4 | Consumo confere hash do artifact, versões, identidade, fontes e conjunto de candidatos por nova descoberta/leitura limitada. Reutiliza extração lexical somente com evidências idênticas e cobertura completa; pacote obsoleto/parcial usa coleta atual. Principal cancela preparação incompleta e não aguarda seu slot. |
| M3.5 | Specification pode retornar `needs_clarification` com perguntas identificadas. Checkpoint distinto, conversa Companion dedicada e histórico SQLite; respostas completas/versionadas retomam Specification em geração nova. Não há LLM para repetir perguntas nem aprovação de spec incompleta. |
| M3.6 | UI exibe avaliação, revisão do pedido, preparação, diagnósticos, perguntas/respostas, feedback de revisão e artifacts com conteúdo, inclusive sem path. Preserva fila/bloqueador/reconciliação M2; não acrescenta estágios fictícios. Settings permite ajustar preparação, threshold e orçamento de contexto. |
| M3.7 | Fluxos automatizados cobrem primeira tarefa, sucessor antecipado, pedido vago/dispensado/editado, consumo reutilizado/obsoleto, resultado tardio, kill switch, restart e perguntas/respostas via provider HTTP local real na borda de parsing/roteamento. |

## Persistência, segurança e lifecycle

- Migration aditiva **V12**; V1–V11 não foram alteradas. Backup `VACUUM INTO` com WAL/integridade antes do upgrade: `hard-vibe-code.pre-v12.<uuid>.db`. Fixtures de upgrade preservam histórico, checkpoints, artifacts e coordenação legados.
- `pulsation_description_revisions` preserva descrição/avaliação/dispensa por revisão. `specification_started` impede trocar o pedido depois do início de Specification. `pulsation_preparation` passa a identificar job, antecessora, consumidor e resultado da revalidação.
- `specification_conversations` vincula o Companion à pulsação. A conversa de esclarecimento não pode ser excluída pelo comando genérico de Chat, preservando a trilha de auditoria. Perguntas e respostas já persistidas são restauradas sem nova chamada.
- Ações humanas verificam revisão/checkpoint/versão; repetição retorna conflito ou é bloqueada na UI. Rascunhos de respostas sobrevivem ao polling do mesmo conjunto e não são reaproveitados para perguntas/revisões diferentes.
- Antecipação reutiliza reservas globais `main`/`background` e cancelamento do supervisor M2. Trabalho bloqueante de contexto é aguardado, inclusive quando cancelado. Resultado antigo não cancela nem substitui a preparação de uma descrição nova.
- Recovery preserva pacotes prontos e cancela coleta incompleta, que passa ao caminho sob demanda; não inicia um novo ciclo ilimitado de scans. Revisão humana pendente continua pendente após desligamento da preparação.
- Leitor aplica ignores, exclusão de gerados/binários e nomes/diretórios protegidos, incluindo `.env*` mesmo de exemplo, chaves e credenciais conhecidas. Rejeita symlinks/reparse points/junctions; leitura é limitada antes de alocar o conteúdo. Contexto recuperado é dado não confiável, não autorização para alterar políticas do host.
- Nenhum parser AST, resolvedor multilíngue, nova rota LLM, servidor, executores Git/testes ou mudança de provider foi acrescentado. `pipeline_version` continua 2; o pipeline mantém seis estágios.

## Limites e decisões operacionais

1. **Preparação desligada:** `orchestration.preparation_enabled=false`. Novas descrições não ficam bloqueadas por TaskRefine nessa configuração; a coleta normal de Specification e a experiência de esclarecimentos M3 continuam disponíveis. Ligar/desligar não dispensa avisos humanos já pendentes. Threshold alterado só vale para a próxima avaliação.
2. **Intake antecipado é somente leitura:** paths de suas saídas são removidos antes da criação de intenções de publicação. Intake normal conserva o comportamento M2. Se este já publicou efeitos, editar a descrição é recusado: cancelar/reconciliar e criar outro pedido. Não há rollback implícito de arquivos anteriores à spec.
3. **Orçamento por coleta:** uma enumeração, até 2.000 arquivos elegíveis, 16 MiB lidos, 1 MiB por arquivo, 12 arquivos no pacote, 32 KiB de snippets e 2 s cooperativos. Erro de leitura consome orçamento conservador. Diretório inacessível, limite de profundidade/tempo e seleção truncada são diagnósticos de cobertura, não prova de inexistência de código/dependências.
4. **Reuso conservador:** compara os hashes de todos os candidatos capturados no escopo limitado, não só os snippets selecionados. Mudança de corpo, inclusão, exclusão ou rename invalida; tamanho/mtime/epoch iguais não bastam. Revalidação ainda lê arquivos; o trabalho economizado é a extração/ranking quando as evidências são idênticas. Ganho de latência aguarda benchmark M5.
5. **Handoff:** artifacts aprovados, feedback e esclarecimentos permanecem integrais. Somente evidência lexical opcional pode ser reduzida para caber no orçamento. Respostas que excedem o orçamento são recusadas antes de encerrar as perguntas; a UI permite resumir ou ajustar o limite (2.000–80.000 caracteres). Não é um limite exato de tokens totais do provider.
6. **Conhecimento parcial:** coleta lexical não interpreta integralmente o projeto nem resolve imports; narrativas do vault continuam separadas. Fontes podem mudar externamente após a observação e permanecem sujeitas às barreiras de conflito existentes. Não há snapshot atômico de todo o repositório.
7. **Artifacts na UI:** conteúdo persistido é somente leitura, paginado em blocos de até 8.000 caracteres, com cópia integral. O editor de workspace continua limitado a `.md`/`.txt`; visualizar artifact não aplica código.
8. **M4/M5:** Learning aparece como indisponível e não executa. Cache documental, embeddings delta, retirada da reindexação pré-estágio existente, Learning consultivo, matriz manual completa e rollout/benchmark permanecem pendentes. O recurso opcional não foi ligado no perfil real.

## Evidência automatizada

- `orchestration/task_refine.rs` e corpus JSON: 17 exemplos PT/EN, score 100 alcançável, NFC composto/decomposto e falsos matches por substring. Limiares 0/40/100, revisão Unicode, dispensa e desativação também são cobertos nas APIs de persistência.
- `orchestration/code_context.rs`: cinco testes de leitura sem vault, integridade CRLF/Unicode, snippets/referências observadas, orçamento/ranking, vazios/indisponibilidade/cancelamento e exclusões, incluindo junction Windows.
- `orchestration/preparation_tests.rs`: nove testes com SQLite/filesystem reais. Incluem sucessor imediato A/B/C, zero chamadas no job local, zero publicações antecipadas, geração única, pacote reutilizado versus obsoleto, alteração com tamanho e mtime preservados, mudança de descrição, shutdown/recovery, kill switch e preparação em voo controlada por barreiras.
- Os testes de esclarecimento rejeitam aprovação de spec incompleta, versão antiga, respostas incompletas e clique repetido; verificam nova geração, perguntas/respostas no handoff e somente duas mensagens Companion para um conjunto respondido.
- `hvc-test-support/tests/preparation_workflow.rs`: teste integrado fora do fallback `cfg(test)`, usando provider HTTP loopback, parser e roteador reais. Primeiro pedido sem vault recebe código atual, Specification pergunta, restart conserva o checkpoint e respostas retomam o estágio. São três respostas de provider: Intake, Specification com perguntas e Specification após respostas; Companion/recovery não criam chamadas adicionais.
- `hvc-ui/src/app_model.rs`: dois testes GPUI adicionais de ações tipadas, escopo de revisão/perguntas, preservação de rascunho, bloqueio de aprovação indevida, clique duplicado e visualização de conteúdo sem path. Regressões M0–M2 e upgrades continuam na suíte completa.

## Validação

| Verificação | Resultado |
|---|---|
| Baseline | M2: 270 testes; worktree inicial limpo, HEAD `66abe0d` |
| `cargo fmt --all -- --check` | Aprovado |
| `cargo clippy --workspace --all-targets --locked --offline --quiet -- -D warnings` | Aprovado no código final, sem warnings do workspace |
| `cargo test --workspace --locked --offline --quiet` | **288 aprovados**: 177 core, 14 editor, 19 test-support, 61 UI e 17 integrações (271 unitários + 17 integrações); zero falhas/ignorados |
| `cargo build --release --locked --offline -p hvc-app` | Aprovado, inclusive após o último caso de regressão |
| `powershell -ExecutionPolicy Bypass -File scripts/smoke-windows.ps1` | Janela encontrada, banco temporário inicializado, shutdown gracioso, exit code 0 |
| `git diff --check` | Aprovado no fechamento de código/documentação |
| `scripts/audit-supply-chain.ps1` | Aprovado tecnicamente: 549 pacotes, nenhuma licença negada/ausente ou fonte em revisão |
| `scripts/audit-advisories.ps1` | Exit 0; zero vulnerabilidades reportadas; 10 avisos `unmaintained`, 2 `unsound`, 1 `yanked`; RustSec `6420e39260b3d771b049954cf5d52b57e2118da4` |
| Política de distribuição | Continua bloqueada por `zed-55470`, conforme `quality/distribution-policy.json`; decisão preexistente não alterada |

ICU normalizer 2.3 tornou-se dependência direta para NFC, com features mínimas; já integrava o lockfile transitivamente. Não houve inclusão de pacotes, atualização de versões ou download de gramáticas/modelos. O aviso de incompatibilidade futura de `proc-macro-error2 2.0.1` continua preexistente.

Comandos do gate foram executados individualmente, não pelo wrapper `gate-stage7.ps1`. Relatórios locais: `target/stage7/supply-chain.json` e `target/stage7/advisories/`. Testes/smoke não usaram o perfil/keyring/provider real. O smoke comprova inicialização/encerramento, não toda a matriz manual M5. Dados temporários do smoke foram removidos pelo próprio script; nenhum dado do usuário foi removido.

## Próximo marco

**M4:** cache documental compartilhado e validado por hash, embeddings delta/coalescentes e Learning consultivo com orçamento e prioridade inferiores ao principal. Reusar o supervisor e a outbox existentes; não criar Learning retroativo, não promover relatórios a regras e não confundir manutenção com a fila de tarefas do usuário.
