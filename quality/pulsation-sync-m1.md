# Sincronização de pulsações — checkpoint M1

Design references: [public summaries](../docs/adr/README.md); [historical archive context](README.md#historical-design-references).

Data: 2026-08-27. Repositório: `C:\Dev\Hard-Vibe-Code`, branch `gpui-migration`.
Base: `de6ab80` (M0). Implementação M1: `f4bc196`. Decisão: ADR-007 e REGRAS RSP-01–22 no cérebro.

## Entrega

Integridade do pipeline existente, ainda com um worker serial. Não foram implementados o supervisor M2, PreSpec, TaskRefine, perguntas na UI, Learning ou onboarding Brownfield.

| Item | Implementação e evidência |
|---|---|
| M1.1 | Migration aditiva V10: sequência por projeto, revisões/versões, proprietário/base, jobs, conteúdo/proveniência, aceitação, outbox, preparação e esclarecimentos. Backup `VACUUM INTO` antes do upgrade, incluindo WAL, com `integrity_check`; V1–V9 não alteradas. |
| M1.2 | Catálogo único dos seis estágios/roles, alias `Specification`, normalização conservadora dos bindings. Colisões continuam visíveis e bloqueiam a rota até um save versionado explícito. |
| M1.3 | `statusUpdate` adaptado para `StageOutcome`. Output, métricas, artifacts, evento, checkpoint/próximo job e encerramento lógico são aceitos em uma transação IMMEDIATE, com fence de revisão/geração/estado. Falha do modelo não publica artifacts. |
| M1.4 | Handoff explícito persistido, conteúdo sem path via `read_artifact_content`, hashes e conteúdo aprovado íntegro; orçamento insuficiente é erro, não truncamento da especificação/contrato. Bases de publicação são hashes separados do conteúdo normativo. |
| M1.5 | Outbox, candidatos sem escrita antes da aprovação, paths protegidos, rejeição de traversal/reparse points/junctions, arquivo temporário sincronizado, replace atômico, ACK e replay pelo hash desejado. Próximo job aguarda publicações necessárias. |
| M1.6 | Cancelamento terminal inclui jobs pendentes; motivos separados. Recovery não chama LLM para resultado já aceito, não ressuscita cancelamento e reconcilia publicação incerta. Cada unidade write+ACK e o cancelamento compartilham serialização SQLite, sem lock sobre await. |
| M1.7 | HITL usa ID de checkpoint e versão esperados na API e GPUI. Spec→Spec, Contract→Contract, Test/Integrator→Code. Feedback e invalidação posterior preservam os artifacts anteriores aplicáveis. Esclarecimentos têm contrato/tabelas, mas não são emitidos pelo runtime antes da UI M3. |
| M1.8 | Testes reais de transação/publicador e fixtures de upgrade; falhas injetadas antes do commit e depois da escrita/antes do ACK; resposta real de provider simulada em loopback e invalidada antes da aceitação. |

## Evidências de G1

- `crates/hvc-core/src/orchestration/serial_tests.rs`: rollback de output/artifacts/checkpoint; replay sem duplicação; cancelamento e resultado tardio; conteúdo sem path; revisão; ACK final; arquivo humano; corrupção; produtor superado; junction real no Windows.
- `crates/hvc-test-support/tests/serial_upgrade.rs`: upgrade V9 com WAL/backup restaurável, IDs/outputs/checkpoints preservados, ordem `(created_at,id)`, projetos isolados, proprietário legado, colisão de bindings e histórico indisponível sem conteúdo inventado.
- `crates/hvc-test-support/tests/serial_worker.rs`: worker, roteador, parser e publisher reais, sem fallback sintético. Fixture local usa o perfil Ollama em modo de roteamento legado, resposta HTTP e validação de schema reais; não acessa modelo local nem credenciais reais. Um callback determinístico invalida a revisão entre resposta e aceitação. Saída válida publica uma vez; falha/saída antiga não publica nem altera a revisão nova.
- Testes existentes de parser, contratos, lifecycle, UI e demais crates permanecem na regressão.

## Compatibilidade e limites deliberados

1. Pulsations existentes conservam `pipeline_version=1`; novas recebem `2`. Não há Learning retroativo.
2. O parser novo preserva bytes; somente o adaptador histórico reproduz `trim()` do V1. Correspondência ausente/ambígua não inventa conteúdo. Arquivo legado divergente não é regravado pelo upgrade.
3. Um proprietário legado é registrado; múltiplos avançados são diagnosticados sem escolher nem perder trabalho. **Admissão exclusiva/FIFO, bloqueio efetivo por base e reconciliação interativa são M2**, não garantias já ativadas neste marco.
4. `Done` no caminho serial exige aprovação final e ACKs. Incremento de epoch/liberação de propriedade/intenção Learning fazem parte da barreira coordenada M2/M4.
5. Conflito encerra a pulsação com diagnóstico e registra `needs_review`; não cria um novo checkpoint inacessível por UI. Startup continua abrindo diante de conflito já registrado; erro de banco não confirmado continua sendo erro. Não há rollback automático de arquivos.
6. O host só substitui arquivo existente com base em um hash previamente publicado pelo host e capturado no início da etapa. Um destino novo deve estar ausente. Não autoriza overwrite de código humano arbitrário; captura de fontes/contexto para evoluções futuras é M3/Brownfield.
7. Hash/replace implementam detecção otimista de edições externas, não uma transação global com editores externos nem uma defesa contra um processo local malicioso trocando diretórios simultaneamente. Symlinks/junctions existentes são recusados, inclusive nos ancestrais da raiz.
8. A unidade atômica é um arquivo. Vários arquivos não têm rollback conjunto; falha parcial exige revisão. O teste de crash injeta abort no banco após o publicador real; não equivale a ensaio de perda de energia do hardware.
9. Não há dependência nova, AST, tabela Brownfield, LanceDB, novo runtime, pool independente ou alteração automática de configuração de provider do usuário.

## Validação

Validação final executada após a última alteração de código:

| Comando | Resultado |
|---|---|
| `cargo fmt --all -- --check` | Aprovado |
| `cargo clippy --workspace --all-targets --locked --offline --quiet -- -D warnings` | Aprovado, zero warnings do workspace |
| `cargo test --workspace --locked --offline --quiet` | Aprovado: 247 testes (144 core, 14 editor, 19 test-support, 14 integrações e 56 UI) |
| `cargo build --release --locked --offline -p hvc-app` | Aprovado; binário release GPUI recompilado após a alteração final |
| `powershell -ExecutionPolicy Bypass -File scripts/smoke-windows.ps1` | Aprovado em diretório isolado: janela encontrada, banco V10 inicializado, shutdown gracioso, exit code 0 |
| `git diff --check` | Aprovado antes do commit de código e novamente no fechamento documental |

Não foi executado o gate completo stage7/auditorias de dependências nem o benchmark de ativação M5. O Cargo repetiu o aviso preexistente de incompatibilidade futura do `proc-macro-error2 v2.0.1`; não houve warning do Clippy do workspace. Testes usam bancos/workspaces temporários e o smoke remove seu diretório ao finalizar. O perfil real do aplicativo não foi migrado para validar esta entrega.

## Próximo marco

M2: usar as primitivas agora verificadas para admissão FIFO, propriedade por projeto, barreira/epoch, supervisor e reconciliação utilizável. Preparação e perguntas continuam desligadas até M3; rollout/ganho medido continua M5.
