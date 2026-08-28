# Sincronização de pulsações — validação M5

Data: 2026-08-28. Branch `gpui-migration`. Base M4: `c627837`. Implementação: `8167313`.
ADR-007 / REGRAS de sincronização do cérebro do projeto.

Status: **implementação e validação automatizada entregues; G5 aberto**. Benchmark concluído, rollout não autorizado; smoke visual ainda pendente. Preparação permanece desligada por default.
Esta avaliação não habilita onboarding Brownfield nem remove a política de distribuição.

## Implementação

- Instrumentação `context_collected`, transacional e protegida pela revisão/geração: tempo monotônico em microssegundos, fase, disposição, cobertura, arquivos/bytes. Não contém paths, snippets, pedidos ou diagnósticos recuperados. Sem migration nova; V1–V13 intactas.
- Benchmark executa supervisor, router, parser, handoff, HITL e integração reais, com provedor HTTP exclusivamente loopback, credenciais em memória e workspace/SQLite temporários. Não usa o fallback de estágios `cfg(test)`.
- Cenários: primeira tarefa sem fila; fila A/B/C; alteração do corpo durante espera, preservando tamanho/mtime. A e B concluem os seis estágios por APIs tipadas; C chega à primeira Specification aceita. Integração no-op explícita, sem executar código/Git. Nenhuma transição de workflow do benchmark é escrita por SQL.
- Trabalho equivalente: duas chamadas no cenário sem fila; 14 chamadas na fila em ambos os modos. Learning e embeddings desligados **somente nas fixtures comparativas**, com testes próprios de prioridade/custo/erro. Não confundir tokens simulados com cobrança ou economia real.
- Teste de Learning em chamada HTTP bloqueada por barreira: demanda principal preempta, kill switch cancela, resposta tardia não publica, orçamento permanece consumido, `Done` e revisão humana pendente são preservados.

### Falhas encontradas e corrigidas durante M5

1. **Disputa de escrita SQLite no início do estágio.** A fila real reproduziu `Code/planning` com job falho `database is locked`. A transação deferred lia antes de obter a reserva de escrita; outra conexão do background podia impedir o upgrade. Início e transição de falha do worker agora usam `BEGIN IMMEDIATE`, antes das leituras, preservando as validações de versão. Não foi adicionado retry que pudesse repetir efeito aceito.
2. **Extensão de arquivo como falso match.** `auth.rs` selecionava arquivos não relacionados pelo token `rs`, preenchia o limite de 12 fontes e impedia reuso por cobertura parcial. O ranking lexical usa diretório/stem sem extensão, mantendo prioridade do path exato e símbolos no código. Política `rsp12-v2` invalida pacotes antigos. Teste com 21 arquivos comprova uma fonte selecionada e revalidação reutilizável.

## Reprodução do benchmark

```powershell
cargo run --release --locked --offline -p hvc-test-support --example pulsation_benchmark -- quality/pulsation-benchmark-m5.json 12
```

Um aquecimento excluído por modo/cenário, 12 pares contrabalanceados; variante inicial alternada. Mesmos 128 arquivos e hash de fixture em todos os pares, dois workers Tokio, provedor simulado com 10 ms, janela HITL fixa de 1.500 ms. A janela não é ampliada quando a preparação perde a oportunidade; misses permanecem na amostra.

O atraso preexistente de 160 ms por estágio do runtime é mantido nos dois modos. Antecipar Intake retira esse estágio do intervalo após a base; portanto uma redução nesse intervalo não pode ser atribuída exclusivamente ao reuso do coletor. A medição usa o core de produção no executável otimizado de test-support, não tempo de desenho da janela GPUI.

- A saída útil é o checkpoint Specification aceito pelo host, não o primeiro byte de uma resposta potencialmente inválida.
- Medição monotônica por observação do epoch já persistido e do checkpoint, polling de 2 ms mais latência de consulta. O commit da base está entre iniciar a aprovação e observar o epoch. O relatório expõe também o intervalo conservador aprovação→saída, sem fingir precisão no instante do commit. Sem fila, a base já está pronta e o intervalo inclui criação/TaskRefine.
- B e C têm percentis separados, para não esconder a invalidação de B com o reuso de C. Método nearest-rank; 12 observações por posição são exploratórias, não certificação de SLO/P95 de produção.
- Tempos de jobs não são somados para calcular caminho crítico. Tempos de coleta/preparação e bytes são contadores separados. O cache de disco não é purgado; o contrabalanceamento reduz viés, mas não comprova cold-start.
- Resultado não é extrapolável a providers remotos, repositórios grandes, parsers AST ou inteligência semântica. Não há estimativa de custo financeiro nem promessa percentual.

## Matriz T01–T22

Os nomes abaixo são testes executáveis; caminhos sob `crates/hvc-core/src/orchestration/` salvo indicação.

| Caso | Evidência automatizada |
|---|---|
| T01 | `tests::preserves_exactly_the_six_existing_stage_wire_values`; testes de aliases/bindings do router |
| T02 | `serial_tests::failed_outcome_never_publishes_or_advances`, `stale_revision_generation_or_version_rolls_back_all_acceptance_effects`, `failure_inside_acceptance_rolls_back_output_and_checkpoint` |
| T03 | `crash_after_replace_before_ack_replays_desired_hash`, `human_change_after_uncertain_write_is_preserved_and_review_recorded` |
| T04 | `unapproved_candidates_wait_and_pathless_content_reaches_handoff_exactly`; `normative_content_is_never_silently_truncated_to_fit_the_budget` |
| T05 | `user_cancel_is_terminal_for_running_pending_and_accepted_publications`, `shutdown_requeues_only_unaccepted_current_execution_and_fences_late_result` |
| T06 | test-support `coordination_upgrade`: V9 múltiplos proprietários e upgrade V10 preservando checkpoint |
| T07 | `fifo_abc_equal_timestamps_owner_holds_hitl_without_slot_or_retry`, `concurrent_claims_share_one_global_slot_and_cancel_keeps_live_reservation`, `a_at_hitl_does_not_block_different_project` |
| T08 | `artifact_review_does_not_write_before_approval`, `integrator_done_waits_for_publication_ack`, `publication_waits_for_main_slot_but_not_for_optional_background` |
| T09 | `main_claim_does_not_wait_for_inflight_local_read_or_accept_its_late_result`, `optional_background_reservation_cannot_block_integration_or_next_specification` |
| T10 | corpus `task_refine`; `description_review_is_local_versioned_unicode_and_not_silently_confirmed`, `threshold_edges_apply_only_to_next_assessment` |
| T11 | `superseded_preparation_cannot_change_the_new_description_and_recovery_does_not_rescan`; testes de fences e clique HITL obsoleto |
| T12 | `first_request_without_vault_uses_exact_bytes_and_unresolved_references`, `context_rejects_cross_project_evidence`; test-support `preparation_workflow` |
| T13 | `completed_package_is_consumed_or_invalidated_by_body_and_candidate_changes`; test-support `pulsation_m5` |
| T14 | `bounded_budget_marks_partial_and_stable_ranking_prioritizes_explicit_paths`, `only_immediate_successor_intake_and_one_local_preparation_are_admitted` |
| T15 | `ignores_secrets_examples_generated_binary_and_narrative_even_when_explicitly_hinted`, `junctions_are_never_followed_by_code_collection`, `windows_junction_cannot_escape_publication_root`; contratos de snippets/hashes |
| T16 | `clarification_checkpoint_survives_restart_and_answers_cannot_approve_artifacts`, `revision_goes_to_correct_stage_and_old_click_cannot_approve_new_checkpoint`; test-support `preparation_workflow` com provider real simulado |
| T17 | `three_preemptions_defer_and_explicit_versioned_retry_preserves_total_usage`, `frozen_input_is_bounded_and_recurrence_requires_two_distinct_pulses`; test-support `learning_worker` |
| T18 | `delta_queue_coalesces_changes_deletions_and_model_switches`, `late_vector_is_fenced_by_current_bytes_then_new_delta_can_run`, `disable_during_call_fences_response_and_reenable_resumes_same_hash` |
| T19 | `unavailable_provider_has_no_automatic_retry_loop_and_manual_retry_is_coordinated`, `proposed_outbox_replay_and_conflict_never_hold_next_pulse`; Learning tardio/erro sem reabrir Done |
| T20 | testes GPUI de perguntas, revisão, artifacts sem path, criação/cancelamento e generation; reconciliação `effects_require_explicit_review_with_fresh_hashes_and_keep_human_bytes`; smoke visual abaixo |
| T21 | `kill_switch_cancels_unconsumed_context_but_keeps_main_progress_possible`, `real_inflight_learning_yields_to_foreground_or_kill_switch_without_late_publication`, `disable_recovery_and_dismiss_fence_late_reports_without_resetting_budget` |
| T22 | `real_worker_matches_work_and_fences_stale_context_in_all_benchmark_scenarios`, percentis nearest-rank e benchmark release; decisão de rollout abaixo |

## Gates e smoke GPUI

| Verificação | Resultado |
|---|---|
| `cargo test --workspace --locked --offline --quiet` | **313 aprovados**, zero falhas/ignorados; 196 core, 14 editor, 41 test-support (20 unitários + 21 integração), 62 UI. Reexecução posterior da integração `pulsation_m5` aprovada após separar métricas B/C. |
| `cargo clippy --workspace --all-targets --locked --offline --quiet -- -D warnings` | Aprovado |
| `cargo fmt --all -- --check` | Aprovado |
| `git -c core.excludesFile= -c core.autocrlf=false diff --check` | Aprovado |
| `cargo build --release --locked --offline -p hvc-app` | Aprovado; aviso preexistente de futura incompatibilidade `proc-macro-error2 2.0.1` |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/audit-supply-chain.ps1` | 549 pacotes, zero licenças negadas/metadata ausente/source review; gate técnico aprovado. Distribuição permanece bloqueada por `zed-55470`. |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/audit-advisories.ps1 -OutputDirectory target/m5/advisories` | Zero vulnerabilidades; avisos informativos preexistentes 10 unmaintained, 2 unsound, 1 yanked. RustSec `6420e39260b3d771b049954cf5d52b57e2118da4`. |
| Smoke visual GPUI e shutdown nativo gracioso | **Pendente**: a execução oculta inicializou DB/lock isolados, mas não expôs janela ao controle de aplicativos. Instância de teste encerrada; não contar como smoke aprovado. Solicitada autorização para janela visível. O script Win32 de smoke não foi combinado com a skill de controle de aplicativos nesta sessão. |

Ambiente: Windows `10.0.26200.0`, x86_64; Intel Core Ultra 7 258V, 8 núcleos/8 processadores lógicos. Rust detalhado e modo release registrados no JSON comparativo. SHA-256 do executável GPUI: `6d16efb9023a6f075a18c675ac7ca27019bf167313173e4ad85c9e9a354b5436`.

O smoke pendente inclui criação/revisão/perguntas/aprovações, artifacts, conflito e reconciliação de base, cancelamento, Learning, troca de projeto e restart. Testes automatizados não substituem essa evidência visual nem permitem declarar G5 aprovado.

Fixture reproduzível (novo diretório, nunca perfil real):

```powershell
cargo run --release --locked --offline -p hvc-test-support --example pulsation_ui_fixture -- target/m5-ui-NOVO
# Iniciar hard-vibe-code.exe com HVC_DATA_DIR apontando para target/m5-ui-NOVO/data.
# Fechar o app antes de criar STOP no diretório da fixture.
```

A fixture oferece dois projetos, revisão de pedido pendente, resposta de esclarecimento para tarefas contendo `M5 Perguntas` (responder `JWT assinado`), conflito preservando `notes/conflict.md` para `M5 Conflito`, e Learning consultivo após Done. Apenas configuração/semeadura; o supervisor pertence exclusivamente ao aplicativo.

## Decisão de ativação

**Manter `preparation_enabled=false`.** O ensaio mostra melhora de mediana em filas, mas não sustenta ativação geral: há piora observada da cauda no cenário com edição e a validação visual não foi concluída. Os defaults independentes de Learning/embeddings permanecem inalterados. Sem downgrade, troca de provider/modelo ou flexibilização de hashes/HITL para obter números melhores.

### Resultados medidos

[Dados brutos e metadados](C:/Dev/Hard-Vibe-Code/quality/pulsation-benchmark-m5.json), código `8167313feb7796ef873b5d11a828d3a584280530`, working diff de código vazio, Rust 1.98.0 / LLVM 22.1.8, release. **78 execuções**: seis aquecimentos excluídos + 72 medidas. Hash único dos 128 arquivos: `afeee15ddae5cd9ed914050bd92c209e2a931520c2724463d7464afcd6ca02cf`. Nenhuma compilação ou outra instância HVC estava concorrendo com a medição; o desktop e seus serviços não foram isolados como um laboratório.

Tempos em milissegundos, base observada → Specification aceita; 12 observações por célula/posição:

| Cenário / pulsação | Off P50 | On P50 | Off P95 | On P95 |
|---|---:|---:|---:|---:|
| Sem fila / A | 1.056,7 | 1.070,8 | 1.745,9 | 1.883,6 |
| Fila A/B/C / B | 1.114,7 | 786,1 | 2.150,8 | 1.800,3 |
| Fila A/B/C / C | 1.142,9 | 602,6 | 1.864,0 | 1.698,9 |
| Edição durante espera / B | 1.074,1 | 663,5 | 1.219,4 | 1.701,0 |
| Edição durante espera / C | 1.164,1 | 573,2 | 2.038,5 | 1.831,8 |

O P95 observado de B com edição aumentou **39,5%**; considerando o intervalo conservador desde iniciar a aprovação, o P95 passa de **1.640,1 para 1.899,8 ms**. São sinais do ensaio, não uma estimativa causal/estatisticamente certificada: nearest-rank com n=12 torna P95 o máximo observado. Isso exige investigação/repetição, não ocultação da amostra nem afirmação de ganho universal. Sem fila, não há benefício de antecipação.

| Coleta/revalidação de Specification | Off P50/P95 (ms) | On P50/P95 (ms) |
|---|---:|---:|
| Fila / B | 49,3 / 66,6 | 37,9 / 55,5 |
| Edição / B | 48,9 / 65,6 | 61,3 / 69,4 |

- Das **48 preparações** medidas: 36 reutilizadas e 12 invalidadas por edição (todas as B do cenário de edição). Todas estavam prontas ao liberar a base; nenhum miss/skip de oportunidade nesse ensaio. As 12 tarefas sem fila por variante usaram contexto sob demanda, sem job PreSpec. Nenhum pacote parcial, conflito de publicação ou aceitação de corpo antigo.
- Preparação adicionou **48 coletas** ao trabalho total. Por execução A/B/C, os contadores acumulados leem 1.382.112 bytes off contra 2.303.520 bytes on: três coletas/revalidações obrigatórias contra cinco incluindo duas antecipadas. Reuso não dispensa reler hashes/candidatos.
- Coleta antecipada: P50 **54,0 ms**, P95 **67,1 ms** nas 48 preparações. Esse custo ocorre antes da liberação da base; não foi subtraído artificialmente nem somado ao caminho crítico.
- Chamadas equivalentes: **2** sem fila e **14** por fila, on/off; **780 chamadas sintéticas** incluindo aquecimento, zero provider externo. Tokens de saída simulados iguais. Entrada simulada por 12 execuções em cada cenário de fila: 143.364 off e 144.108 on; não há economia de tokens demonstrada.
- Learning/embeddings estavam desligados nas variantes comparativas: suas preempções/conflitos são zero por construção, não evidência de custo zero quando habilitados. Prioridade, orçamento, cancelamento e respostas tardias são cobertos separadamente por `learning_worker` e testes determinísticos M4/M5.

Próximos passos para fechar G5: autorizar e executar o smoke visual isolado; investigar a variabilidade/cauda com edição (incluindo revalidação, contenção SQLite, acordar do scheduler e ruído do host) e repetir a comparação com amostra maior antes de propor ativação. Não promover a hipótese de causa a diagnóstico comprovado. Nenhum desses pontos autoriza antecipar onboarding Brownfield.
