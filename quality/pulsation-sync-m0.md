# Sincronização de pulsações — checkpoint M0

Data: 2026-08-27. Baseline: `a4b475e`. Estado: **M0 concluído; G0 aprovado**.

Este checkpoint entrega contratos, políticas puras e fixtures. **Não ativa a sincronização concorrente no aplicativo.** M1–M5 continuam pendentes; worker, migrations V1–V9 e UI de produção mantêm seu comportamento anterior.

## Escopo implementado

| Entrega | Evidência |
|---|---|
| Outcomes, revisão humana, configuração, preparação, publicação e snapshot tipados | [contracts.rs](../crates/ikore-core/src/orchestration/contracts.rs) |
| Captura imutável, SHA-256, caminhos relativos, evidências e contratos de coleta/extração | [context.rs](../crates/ikore-core/src/orchestration/context.rs) |
| Validação de geração/revisão, checkpoints, respostas, handoff e decisão de replay | [policy.rs](../crates/ikore-core/src/orchestration/policy.rs) |
| 30 testes unitários novos de contratos | [tests.rs](../crates/ikore-core/src/orchestration/tests.rs) |
| Bancos V9 isolados, projetos sem vault e estados legados | [pulsation_fixture.rs](../crates/ikore-test-support/src/pulsation_fixture.rs) |
| Provedor simulado controlável por semáforos | [controlled_gateway.rs](../crates/ikore-test-support/src/controlled_gateway.rs) |
| 8 testes novos de integração/caracterização | [orchestration_baseline.rs](../crates/ikore-test-support/tests/orchestration_baseline.rs) |

O módulo público `hvc_core::orchestration` não possui worker nem publicador. As funções de política retornam decisões/erros; quem as aplicar em M1 deverá fazê-lo dentro da transação/coordenador correspondente. Serde é usado nos contratos persistíveis e nos testes, não para criar uma API JSON/IPC.

## Fechamento dos itens M0

| Item | Resultado |
|---|---|
| M0.1 | Commit e worktree conferidos; somente os três documentos anteriores estavam não rastreados; migrations continuam V1–V9. |
| M0.2 | Baseline completa de 186 testes aprovada. Suites existentes caracterizam avanço até checkpoint, HITL, artifacts, cancelamento, recovery e compatibilidade. Novos testes usam o parser real com gateway simulado. |
| M0.3 | Contratos Rust criados para outcome/handoff, revisão, perguntas/respostas, preparação, outbox, snapshots e Settings. Não estão ligados às APIs de produção ainda. |
| M0.4 | Estados e políticas puros definidos; chaves/limites transacionais registrados abaixo para materialização em M1. |
| M0.5 | Interfaces independentes de pulsação/vault/GPUI, cobertura parcial, fontes imutáveis e identidade de fatos por projeto/conteúdo/extrator definidas e testadas. |
| M0.6 | Fixtures `TwoQueued`, `ThreeQueued`, `OneActive` e `MultipleActive`; segundo projeto independente; mesmo path em dois repositórios sem vault; artifacts legados com conteúdo nos outputs. |
| M0.7 | Relógio `ManualClock` existente reutilizável; semáforos para resposta atrasada/cancelamento; janelas de crash representadas por conteúdo esperado/desejado/observado. |

## Contratos para persistência e transições de M1/M2

Esta tabela especifica invariantes futuras. Nenhuma tabela/constraint abaixo foi criada neste marco.

| Entidade | Identidade/unicidade | Operação atômica exigida |
|---|---|---|
| Pulsação | ID existente; `(project_id, queue_sequence)` único e imutável | Criar sequência + revisão inicial + avaliação + Intake elegível |
| Proprietário/base | Uma linha por projeto; versão de estado e epoch distintos | Claim principal + propriedade; finalização + `Done` + epoch + liberação |
| Preparação | `(pulsation_id, input_revision)`; geração por espera/revisão | Invalidar avaliação/pacote antigo ao editar descrição |
| Job | ID existente; chave idempotente por intenção + revisão; geração por claim | Claim verifica estado atual; saída só é aceita se fence, status e cancelamento ainda permitem |
| Artifact | `(job_id, generation, output_position)` único | Conteúdo + estado de aceitação + evento + checkpoint/próximo trabalho |
| Publicação | Intenção idempotente por artifact/revisão/destino | Outbox no commit lógico; filesystem coordenado; ACK/replay por hash em transação posterior |
| Esclarecimento | Conjunto/checkpoint vinculados à revisão/geração de Specification | Criar perguntas/checkpoint juntos; respostas válidas retomam o mesmo estágio |
| Learning | `(pulsation_id, integration_epoch, report_version)` | Criar uma única intenção na finalização habilitada, sem bloquear a liberação |

- `Completed` nos quatro estágios com HITL continua exigindo revisão; `NeedsClarification` só é permitido em Specification; `Failed` não transporta artifacts ou pedido de avanço.
- Revisão em Specification/Contract volta ao mesmo estágio; em Test/Integrator volta a Code. São políticas testadas, ainda não conectadas ao handler legado.
- Base `ready` permite elegibilidade; `publishing` e `needs_review` impedem a próxima Specification. Aprovação sozinha não libera uma base não reconciliada.
- Preparação tem estados próprios (`not_scheduled` a `cancelled`), sem estender `PulsationStage`. Relatório e indexação não participam da barreira obrigatória.
- `user_cancel` não é retomável; shutdown/preempção exigem revisão atual e orçamento disponível. A classificação completa e recovery transacional serão implementados em M1/M2.
- Uma única pulsação legada além de Intake será retida como proprietária. Múltiplas exigirão reconciliação, não descarte silencioso. Fixtures preservam ambos os casos para o teste de migração.

IDs/revisões vindos de banco devem ser convertidos com checagem de faixa ao materializar o schema SQLite. Os DTOs não substituem validação de request, constraints ou controle transacional.

## Cobertura nova e seus limites

- Fence alterado em projeto, pulsação, input, job, geração ou versão é rejeitado. Status encerrado/cancelado também rejeita resultado correspondente.
- Descrição usa contagem de caracteres Unicode; score aceita somente 0–100. O algoritmo TaskRefine e seu corpus linguístico continuam em M3.
- Snapshot mantém bytes Unicode/CRLF; hash e snippet derivam da mesma captura. Handoff rejeita artifact superado, não publicado, corrompido, de outra revisão ou de estágio posterior.
- Limites insuficientes geram erro sem truncar artifacts normativos. Isso não é um tokenizer nem substitui o orçamento total do prompt em M1/M3.
- Identidade de fatos diferencia projeto, conteúdo, extrator, política e contexto de resolução. Sem vault é válido; referência não resolvida não inventa destino.
- `publication_replay_decision` distingue escrever, reconhecer escrita já aplicada e conflito. O teste de janela de crash simula os bytes; **não comprova um publicador/outbox real ainda inexistente**.
- O provedor controlado entra no caminho real de `execute_stage`, inclusive parsing/validação de schema. `call_count == 1` impede um falso positivo via fallback sintético. O timeout de 5 s é apenas proteção contra travamento; semáforos determinam a ordem.
- Não foram executados benchmarks de espera/PreSpec/AST/embeddings. Medir benefício da antecipação depende dos marcos posteriores.

## Achado de compatibilidade para M1

O [parser legado de artifacts](../crates/ikore-core/src/agent_runtime.rs#L922) aplica `trim()` ao conteúdo, enquanto `structured_output` mantém o texto original. O novo teste de caracterização comprova a diferença usando uma quebra de linha final.

Ao migrar conteúdo durável/hashes, M1 deve distinguir bytes gerados dos bytes aceitos/publicados pelo legado. Não calcular o hash de um texto e usar outro no replay, nem reescrever automaticamente um arquivo existente para “normalizar” a diferença. Este marco não modificou o parser.

## Validação executada

Ambiente: Windows, Rust 1.98.0, Cargo offline, dependências travadas. Dados temporários e gateway de fixture; nenhuma chamada paga ou keyring real nos testes novos.

| Comando | Resultado |
|---|---|
| `cargo test --workspace --locked --offline` antes das mudanças | 186 testes aprovados |
| `cargo test --workspace --locked --offline` após M0 | 224 testes aprovados: 215 unitários + 9 de integração |
| `cargo clippy --workspace --all-targets --locked --offline -- -D warnings` | Aprovado |
| `cargo fmt --all -- --check` | Aprovado |
| `git diff --check` | Aprovado para alterações rastreadas |

Incremento: 38 testes (30 contratos + 8 integração). Permanece o aviso preexistente de compatibilidade futura de `proc-macro-error2 2.0.1`, sem erro de Clippy.

O gate completo de release/smoke/auditorias da Etapa 7 não foi reexecutado neste M0 de contratos sem mudança de UI/lifecycle. Ele continua exigido na liberação M5; este resultado não é autorização para distribuição nem ativação do novo runtime.

## Alterações deliberadamente pequenas no código existente

- Exportação do módulo `orchestration` pelo core.
- `PulsationStage::next` e `requires_checkpoint` passam a `pub(crate)` para a política reutilizar as mesmas regras, sem duplicar o catálogo e sem mudar comportamento.
- `hvc-test-support` usa `tokio-util` já resolvido no workspace para o cancelamento do gateway de fixture; o lockfile ganhou somente essa referência, sem pacote/versão nova.
- Nenhuma migration, parser AST, configuração do usuário ou fluxo GPUI foi alterado. Nenhum commit foi criado.

## Próximo marco

**M1 — integridade e persistência serial:** materializar schema aditivo, ligar `StageOutcome`/handoff ao runtime, tornar checkpoint/aceitação atômicos, implementar publicador/outbox e recovery seletivo. Reconfirmar a próxima migration livre antes de começar e usar as fixtures V9 deste marco.

O gate G0 está fechado; G1–G5 não foram antecipados.
