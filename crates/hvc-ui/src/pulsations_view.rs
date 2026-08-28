use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px};
use hvc_core::orchestration::BaseState;
use hvc_core::persistence_service::{Pulsation, PulsationStage, PulsationStatus};

use crate::app_model::AppModel;
use crate::pulsations::{PIPELINE_STAGES, stage_label, status_label};
use crate::theme;

pub(crate) fn pulsations_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let has_project = model.workspace.read(cx).project_id.is_some();
    let feature_input = model.pulsation_feature_input.clone();
    let feature_name_empty = feature_input.read(cx).value().trim().is_empty();
    let queue = model
        .pulsations
        .read(cx)
        .coordination
        .as_ref()
        .map(|s| s.queue.clone())
        .unwrap_or_default();
    let (
        pulsations,
        active_id,
        active,
        artifacts,
        runtime_loading,
        artifacts_loading,
        awaiting_hitl,
        running_jobs,
        notice,
        error,
    ) = {
        let state = model.pulsations.read(cx);
        (
            state.pulsations.clone(),
            state.active_id(),
            state.active().cloned(),
            state.artifacts.clone(),
            state.runtime_loading,
            state.artifacts_loading,
            state.awaiting_hitl,
            state.running_jobs,
            state.notice.clone(),
            state.error.clone(),
        )
    };

    if !has_project {
        return empty_panel(
            "Nenhum projeto ativo",
            "Abra um workspace antes de criar ou acompanhar pulsações.",
        )
        .into_any_element();
    }

    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_4()
        .flex()
        .gap_3()
        .child(
            div()
                .w(px(292.0))
                .min_h_0()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .flex()
                .flex_col()
                .gap_3()
                .child(section_heading(
                    "PROJECT FLOW",
                    "Pulsations",
                    pulsations.len(),
                ))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme::text_muted())
                                .child("NOVA PULSAÇÃO"),
                        )
                        .child(feature_input)
                        .child(
                            div()
                                .id("create-pulsation")
                                .h(px(34.0))
                                .rounded_md()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if runtime_loading || feature_name_empty {
                                    theme::surface_raised()
                                } else {
                                    theme::accent()
                                })
                                .text_color(if runtime_loading || feature_name_empty {
                                    theme::text_muted()
                                } else {
                                    theme::canvas()
                                })
                                .when(!runtime_loading && !feature_name_empty, |button| {
                                    button.cursor_pointer().on_click(
                                        cx.listener(|model, _, _, cx| model.create_pulsation(cx)),
                                    )
                                })
                                .child(if runtime_loading {
                                    "Processando…"
                                } else {
                                    "Criar pulsação"
                                }),
                        ),
                )
                .child(
                    div()
                        .id("pulsation-list")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(pulsations.is_empty(), |list| {
                            list.child(
                                div()
                                    .p_3()
                                    .text_sm()
                                    .text_color(theme::text_muted())
                                    .child("Crie a primeira pulsação deste projeto."),
                            )
                        })
                        .children(pulsations.into_iter().map(|pulsation| {
                            let position = queue.iter().find(|p| p.pulsation_id == pulsation.id);
                            let label = position.map(|p| {
                                format!(
                                    "#{} · {}",
                                    p.sequence,
                                    blocking_label(p.blocking_reason.as_deref())
                                )
                            });
                            pulsation_card(pulsation, active_id.as_deref(), label, cx)
                        })),
                ),
        )
        .child(
            div()
                .id("pulsation-detail-scroll")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .overflow_y_scroll()
                .child(coordination_panel(model, cx))
                .when_some(active, |panel, active| {
                    panel
                        .child(pulsation_detail_header(active.clone(), runtime_loading, cx))
                        .child(pipeline(active.stage))
                        .child(preparation_panel(model, cx))
                        .child(artifact_preview_panel(model, cx))
                        .child(
                            div()
                                .grid()
                                .grid_cols(3)
                                .gap_2()
                                .my_4()
                                .child(fact("ESTÁGIO ATUAL", stage_label(active.stage)))
                                .child(fact("AGENTES ATIVOS", running_jobs.to_string()))
                                .child(fact("HITL PENDENTE", awaiting_hitl.to_string())),
                        )
                        .child(fact("CRIADA EM", active.created_at.clone()))
                        .child(fact("ÚLTIMA TRANSIÇÃO", active.updated_at.clone()))
                        .child(
                            div()
                                .mt_5()
                                .child(section_heading(
                                    "GENERATED OUTPUT",
                                    "Artefatos",
                                    artifacts.len(),
                                ))
                                .when(artifacts_loading, |section| {
                                    section.child(
                                        div()
                                            .py_3()
                                            .text_color(theme::text_muted())
                                            .child("Carregando artefatos…"),
                                    )
                                })
                                .when(!artifacts_loading && artifacts.is_empty(), |section| {
                                    section.child(
                                        div()
                                            .py_3()
                                            .text_color(theme::text_muted())
                                            .child("Nenhum artefato gerado até o momento."),
                                    )
                                })
                                .children(artifacts.into_iter().map(|artifact| {
                                    let pulse = artifact.pulsation_id.clone();
                                    let artifact_id = artifact.id.clone();
                                    div()
                                        .id(gpui::SharedString::from(format!(
                                            "artifact:{}",
                                            artifact.id
                                        )))
                                        .mt_2()
                                        .p_3()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(theme::border())
                                        .bg(theme::surface_raised())
                                        .cursor_pointer()
                                        .on_click(cx.listener(move |model, _, _, cx| {
                                            model.preview_artifact(
                                                pulse.clone(),
                                                artifact_id.clone(),
                                                cx,
                                            )
                                        }))
                                        .child(
                                            div()
                                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                                .child(artifact.name),
                                        )
                                        .child(
                                            div().text_xs().text_color(theme::text_muted()).child(
                                                format!(
                                                    "{} · {}",
                                                    artifact.kind,
                                                    artifact.relative_path.unwrap_or_else(|| {
                                                        "sem caminho no workspace".into()
                                                    })
                                                ),
                                            ),
                                        )
                                })),
                        )
                })
                .when(active_id.is_none(), |panel| {
                    panel.child(empty_panel(
                        "Nenhuma pulsação",
                        "Crie a primeira pulsação para visualizar pipeline e artefatos.",
                    ))
                })
                .when_some(notice, |panel, notice| {
                    panel.child(
                        div()
                            .mt_3()
                            .p_3()
                            .rounded_md()
                            .bg(theme::accent_soft())
                            .text_color(theme::accent())
                            .child(notice),
                    )
                })
                .when_some(error, |panel, error| {
                    panel.child(
                        div()
                            .mt_3()
                            .p_3()
                            .rounded_md()
                            .bg(theme::danger_soft())
                            .text_color(theme::danger())
                            .child(error),
                    )
                }),
        )
        .into_any_element()
}

fn pulsation_card(
    pulsation: Pulsation,
    active_id: Option<&str>,
    queue_label: Option<String>,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let selected = active_id == Some(pulsation.id.as_str());
    let id = pulsation.id.clone();
    div()
        .id(gpui::SharedString::from(format!("pulsation:{id}")))
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(if selected {
            theme::accent()
        } else {
            theme::border()
        })
        .bg(if selected {
            theme::accent_soft()
        } else {
            theme::surface_raised()
        })
        .cursor_pointer()
        .on_click(cx.listener(move |model, _, _, cx| {
            model.select_pulsation(id.clone(), cx);
        }))
        .child(
            div()
                .flex()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(pulsation.feature_name),
                )
                .child(status_dot(pulsation.status)),
        )
        .child(
            div()
                .mt_1()
                .text_xs()
                .text_color(theme::text_muted())
                .child(format!(
                    "{} · {}",
                    stage_label(pulsation.stage),
                    status_label(pulsation.status, pulsation.terminal_reason.as_deref())
                )),
        )
        .when_some(queue_label, |card, label| {
            card.child(
                div()
                    .mt_1()
                    .text_xs()
                    .text_color(theme::warning())
                    .child(label),
            )
        })
        .into_any_element()
}

fn preparation_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    use hvc_core::orchestration::{DescriptionReviewState, PreSpecSkipReason, PreSpecState};
    use hvc_core::persistence_service::HitlDecision;
    let state = model.pulsations.read(cx);
    let Some(detail) = state.detail().cloned() else {
        return div().into_any_element();
    };
    let busy = state.action_busy;
    let artifact_review = state.hitl.expected_checkpoint.is_some();
    let review_busy = state.hitl.busy;
    let prep = &detail.preparation;
    let label = match prep.prespec_state {
        PreSpecState::NotScheduled => "Não agendada",
        PreSpecState::Pending => "Aguardando slot local",
        PreSpecState::Running => "Coletando contexto local",
        PreSpecState::Ready => "Pacote pronto para revalidação",
        PreSpecState::Skipped => "Preparação dispensada",
        PreSpecState::Stale => "Pacote obsoleto; contexto atual usado",
        PreSpecState::Failed => "Coleta falhou; contexto sob demanda",
        PreSpecState::Cancelled => "Preparação cancelada; contexto sob demanda",
    };
    let reason = prep.skip_reason.map(|r| match r {
        PreSpecSkipReason::Disabled => "Desativada nas configurações",
        PreSpecSkipReason::SkippedNoPredecessor => "Sem antecessora",
        PreSpecSkipReason::AmbiguousRequest => "Pedido ainda vago após dispensa humana",
        PreSpecSkipReason::NoMatches => "Sem matches no escopo limitado",
        PreSpecSkipReason::BaseReleased => "Base liberada; Specification não espera",
    });
    let mut panel = div()
        .mt_4()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Preparação · fora dos seis estágios"),
        )
        .child(format!("Revisão {} · {}", detail.input_revision, label))
        .when_some(reason, |p, r| {
            p.child(div().text_sm().text_color(theme::text_muted()).child(r))
        })
        .when_some(detail.context_disposition.clone(), |p, d| {
            p.child(format!("Contexto selecionado: {d}"))
        })
        .when_some(detail.diagnostic.clone(), |p, d| {
            p.child(div().text_sm().child(d))
        })
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("Código contextual não exige vault nem AST."),
        );
    use hvc_core::orchestration::LearningState;
    let learning_label = match detail.learning {
        LearningState::NotScheduled => "Não agendado",
        LearningState::Unavailable => "Histórico sem executor (não será reprocessado)",
        LearningState::Pending => "Aguardando prioridade e 5 s de silêncio na conexão",
        LearningState::Running => "Gerando proposta consultiva",
        LearningState::Preempted => "Interrompido pela demanda principal",
        LearningState::DeferredBudget => "Orçamento esgotado; aguarda autorização",
        LearningState::Disabled => "Desativado; orçamento preservado",
        LearningState::Publishing => "Publicando proposta",
        LearningState::Published => "Proposta publicada; confira o artefato abaixo",
        LearningState::Conflict => "Conflito de publicação; arquivos preservados",
        LearningState::Failed => "Falha opcional; pulsação concluída preservada",
        LearningState::Cancelled => "Relatório dispensado",
    };
    panel = panel
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(format!("Learning · {learning_label}")),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_muted())
                .child("Propostas não alteram regras nem reabrem Done."),
        );
    if let Some(run) = &detail.learning_run {
        panel = panel
            .child(format!(
                "Chamadas: {}/3 nesta versão · {} no histórico",
                run.budget_calls, run.total_calls
            ))
            .when_some(run.diagnostic.clone(), |panel, diagnostic| {
                panel.child(div().text_sm().child(diagnostic))
            });
        if matches!(
            detail.learning,
            LearningState::Failed | LearningState::DeferredBudget
        ) && run.artifact_id.is_none()
        {
            panel = panel.child(preparation_button(
                "learning-retry",
                "Autorizar até 3 novas chamadas",
                !busy,
                cx,
                |m, cx| m.learning_action(true, cx),
            ));
        }
        if !matches!(
            detail.learning,
            LearningState::Published | LearningState::Cancelled
        ) {
            panel = panel.child(preparation_button(
                "learning-dismiss",
                "Dispensar relatório",
                !busy,
                cx,
                |m, cx| m.learning_action(false, cx),
            ));
        }
    }
    if let Some(assessment) = &prep.assessment {
        panel = panel.child(format!(
            "TaskRefine: {} / 100 · threshold {} · {}",
            assessment.score.get(),
            assessment.threshold.get(),
            assessment.ruleset_version
        ));
        for contribution in &assessment.contributions {
            panel = panel.child(div().text_sm().child(format!(
                "+{} — {}",
                contribution.weight, contribution.reason
            )));
        }
    }
    if prep.description_review == DescriptionReviewState::Pending {
        panel=panel.child(div().text_color(theme::warning()).child("Aguardando você: revise o pedido ou confirme que deseja seguir. Isso não aprova a especificação."));
    }
    if detail.description_editable {
        panel = panel
            .child(model.description_review_input.clone())
            .child(preparation_button(
                "description-edit",
                "Editar pedido",
                !busy,
                cx,
                |m, cx| m.revise_description(cx),
            ));
    }
    if prep.description_review == DescriptionReviewState::Pending {
        panel = panel.child(preparation_button(
            "description-confirm",
            "Continuar para especificação",
            !busy,
            cx,
            |m, cx| m.confirm_description(cx),
        ));
    }
    if let Some(questions) = detail.questions {
        panel=panel.child(div().font_weight(gpui::FontWeight::SEMIBOLD).child("Specification aguarda suas respostas"))
            .child(div().text_sm().child("Perguntas produzidas pela Specification e apresentadas no Companion sem outra chamada LLM. Não há especificação completa para aprovar ainda."));
        let mut complete = true;
        for question in questions.questions {
            panel = panel
                .child(format!("[{}] {}", question.id, question.question))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme::text_muted())
                        .child(question.rationale),
                );
            if let Some((_, input)) = model
                .specification_answer_inputs
                .iter()
                .find(|(id, _)| id == &question.id)
            {
                complete &= !input.read(cx).value().trim().is_empty();
                panel = panel.child(input.clone());
            } else {
                complete = false;
            }
        }
        panel = panel
            .child(preparation_button(
                "specification-answer",
                "Enviar respostas e retomar Specification",
                !busy && complete,
                cx,
                |m, cx| m.answer_specification(cx),
            ))
            .child(preparation_button(
                "specification-conversation",
                "Ver conversa no Companion",
                true,
                cx,
                |m, cx| m.open_specification_conversation(cx),
            ));
    } else if artifact_review {
        panel = panel
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Revisão de artefatos · confira o conteúdo abaixo"),
            )
            .child(model.stage_feedback_input.clone())
            .child(preparation_button(
                "artifact-approve",
                "Aprovar estágio",
                !review_busy,
                cx,
                |m, cx| m.apply_hitl_decision(HitlDecision::Approve, cx),
            ))
            .child(preparation_button(
                "artifact-revise",
                "Solicitar revisão com feedback",
                !review_busy,
                cx,
                |m, cx| m.apply_hitl_decision(HitlDecision::RequestRevision, cx),
            ))
            .child(preparation_button(
                "artifact-reject",
                "Rejeitar estágio",
                !review_busy,
                cx,
                |m, cx| m.apply_hitl_decision(HitlDecision::Reject, cx),
            ));
    }
    panel.into_any_element()
}

fn preparation_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    cx: &mut Context<AppModel>,
    action: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .p_2()
        .rounded_md()
        .bg(theme::surface_raised())
        .text_sm()
        .opacity(if enabled { 1.0 } else { 0.5 })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .on_click(cx.listener(move |m, _, _, cx| action(m, cx)))
        })
        .child(label)
}

fn artifact_preview_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let Some((id, content)) = model.pulsations.read(cx).artifact_preview.clone() else {
        return div().into_any_element();
    };
    let pages = content.chars().count().max(1).div_ceil(8000);
    let page = model.artifact_preview_page.min(pages - 1);
    let text: String = content.chars().skip(page * 8000).take(8000).collect();
    div()
        .mt_3()
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::accent())
        .flex()
        .flex_col()
        .gap_2()
        .child(format!(
            "Conteúdo persistido · {id} · página {}/{}",
            page + 1,
            pages
        ))
        .child(
            div()
                .id("artifact-preview-scroll")
                .max_h(px(450.0))
                .overflow_y_scroll()
                .text_sm()
                .child(text),
        )
        .child(preparation_button(
            "artifact-prev",
            "Página anterior",
            page > 0,
            cx,
            |m, cx| {
                m.artifact_preview_page = m.artifact_preview_page.saturating_sub(1);
                cx.notify();
            },
        ))
        .child(preparation_button(
            "artifact-next",
            "Próxima página",
            page + 1 < pages,
            cx,
            |m, cx| {
                m.artifact_preview_page += 1;
                cx.notify();
            },
        ))
        .child(preparation_button(
            "artifact-copy",
            "Copiar conteúdo integral",
            true,
            cx,
            move |_, cx| cx.write_to_clipboard(gpui::ClipboardItem::new_string(content.clone())),
        ))
        .child(preparation_button(
            "artifact-close",
            "Fechar conteúdo",
            true,
            cx,
            |m, cx| {
                m.pulsations.update(cx, |p, _| p.artifact_preview = None);
                cx.notify();
            },
        ))
        .into_any_element()
}

fn blocking_label(reason: Option<&str>) -> &'static str {
    match reason {
        Some("waiting_for_base") => "Aguardando base anterior",
        Some("waiting_for_user") => "Aguardando você",
        Some("needs_review" | "legacy_owner_selection") => "Reconciliação necessária",
        Some("publishing") => "Conferindo publicações",
        _ => "Fila persistida",
    }
}

fn coordination_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let state = model.pulsations.read(cx);
    let Some(snapshot) = state.coordination.clone() else {
        return div().into_any_element();
    };
    let active = state.active_id();
    let blocked = snapshot.pipeline.base_state == BaseState::NeedsReview;
    let input = model.reconciliation_input.clone();
    let can_confirm = blocked
        && !state.reconciliation_busy
        && !input.read(cx).value().trim().is_empty()
        && (snapshot.owner_candidates.is_empty()
            || active
                .as_ref()
                .is_some_and(|id| snapshot.owner_candidates.contains(id)));
    let candidates: Vec<_> = snapshot
        .owner_candidates
        .iter()
        .filter_map(|id| state.pulsations.iter().find(|p| &p.id == id).cloned())
        .collect();
    div().mb_3().p_3().rounded_md().border_1().border_color(if blocked {theme::warning()} else {theme::border()})
        .child(div().text_sm().child(format!("Base #{} · {} · proprietário: {}",snapshot.pipeline.base_epoch,
            match snapshot.pipeline.base_state {BaseState::Ready=>"consistente",BaseState::Publishing=>"publicação em andamento",BaseState::NeedsReview=>"revisão necessária"},
            snapshot.pipeline.active_pulsation_id.as_deref().unwrap_or("livre"))))
        .when(blocked, |panel| panel
            .child(div().mt_2().text_color(theme::warning()).child(snapshot.pipeline.blocking_reason.unwrap_or_default()))
            .child(div().mt_2().text_sm().child("Confira os arquivos no seu editor externo; o editor do app abre .md/.txt. Adotar a base atual não desfaz alterações nem declara sucesso. Escolha qual trabalho avançado retomar; os demais continuarão preservados e bloqueados."))
            .children(candidates.into_iter().map(|p| {
                let id=p.id.clone();
                div().id(gpui::SharedString::from(format!("resume:{}",p.id))).mt_2().p_2().rounded_md()
                    .bg(if active.as_deref()==Some(p.id.as_str()) {theme::accent_soft()} else {theme::surface_raised()})
                    .cursor_pointer().on_click(cx.listener(move |m,_,_,cx| m.select_pulsation(id.clone(),cx)))
                    .child(format!("Retomar: {} · {}",p.feature_name,stage_label(p.stage)))
            }))
            .children(snapshot.publications.into_iter().map(|p| {
                let path=p.relative_path.clone();
                let openable=path.to_ascii_lowercase().ends_with(".md") || path.to_ascii_lowercase().ends_with(".txt");
                div().id(gpui::SharedString::from(format!("review:{}",p.artifact_id))).mt_2().p_2().bg(theme::surface_raised())
                    .when(openable,|row|row.cursor_pointer().on_click(cx.listener(move |m,_,_,cx| m.open_workspace_file(path.clone(),cx))))
                    .child(div().text_sm().child(format!("{} · {}",p.relative_path,p.state)))
                    .child(div().text_xs().child(format!("Hash proposto: {}",p.desired_sha256.unwrap_or_else(||"indisponível".into()))))
                    .child(div().text_xs().child(format!("Hash observado: {}",p.observed_sha256.unwrap_or_else(||"ausente ou ilegível".into()))))
                    .when_some(p.diagnostic,|row,error|row.child(div().text_xs().text_color(theme::danger()).child(error)))
                    .when_some(p.observation_error,|row,error|row.child(div().text_xs().text_color(theme::danger()).child(error)))
            }))
            .child(div().mt_2().child(input))
            .child(div().mt_2().flex().gap_2()
                .child(div().id("refresh-base-review").p_2().cursor_pointer().bg(theme::surface_raised())
                    .on_click(cx.listener(|m,_,_,cx|m.refresh_pulsations(cx))).child("Atualizar revisão"))
                .child(div().id("confirm-base-review").p_2().bg(if can_confirm {theme::accent_soft()} else {theme::surface_raised()})
                    .when(can_confirm,|b|b.cursor_pointer().on_click(cx.listener(|m,_,_,cx|m.reconcile_project_base(cx))))
                    .child("Revisei: adotar base atual"))))
        .into_any_element()
}

fn pulsation_detail_header(
    pulsation: Pulsation,
    busy: bool,
    cx: &mut Context<AppModel>,
) -> impl IntoElement {
    let busy = busy
        || matches!(
            pulsation.status,
            PulsationStatus::Done | PulsationStatus::Failed
        );
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap_3()
        .child(
            div()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child("PULSAÇÃO SELECIONADA"),
                )
                .child(
                    div()
                        .text_2xl()
                        .font_weight(gpui::FontWeight::BOLD)
                        .child(pulsation.feature_name),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(pulsation.id),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(status_pill(
                    pulsation.status,
                    pulsation.terminal_reason.as_deref(),
                ))
                .child(
                    div()
                        .id("cancel-pulsation-jobs")
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(if busy {
                            theme::surface_raised()
                        } else {
                            theme::danger_soft()
                        })
                        .text_color(if busy {
                            theme::text_muted()
                        } else {
                            theme::danger()
                        })
                        .when(!busy, |button| {
                            button.cursor_pointer().on_click(
                                cx.listener(|model, _, _, cx| {
                                    model.cancel_active_pulsation_jobs(cx)
                                }),
                            )
                        })
                        .child("Cancelar pulsação"),
                ),
        )
}

fn pipeline(current_stage: PulsationStage) -> impl IntoElement {
    let current = PIPELINE_STAGES
        .iter()
        .position(|stage| *stage == current_stage)
        .unwrap_or_default();
    div()
        .mt_5()
        .flex()
        .items_start()
        .gap_1()
        .children(
            PIPELINE_STAGES
                .into_iter()
                .enumerate()
                .map(|(index, stage)| {
                    let complete = index < current;
                    let active = index == current;
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .size_7()
                                .rounded_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .bg(if complete || active {
                                    theme::accent_soft()
                                } else {
                                    theme::surface_raised()
                                })
                                .text_color(if complete || active {
                                    theme::accent()
                                } else {
                                    theme::text_muted()
                                })
                                .child(if complete {
                                    "✓".into()
                                } else {
                                    (index + 1).to_string()
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(if active {
                                    theme::text()
                                } else {
                                    theme::text_muted()
                                })
                                .child(stage_label(stage)),
                        )
                }),
        )
}

fn status_dot(status: PulsationStatus) -> impl IntoElement {
    div().size_2().rounded_full().bg(status_color(status))
}

fn status_pill(status: PulsationStatus, terminal_reason: Option<&str>) -> impl IntoElement {
    div()
        .px_3()
        .py_1()
        .rounded_full()
        .bg(theme::surface_raised())
        .text_color(status_color(status))
        .child(status_label(status, terminal_reason))
}

fn status_color(status: PulsationStatus) -> gpui::Rgba {
    match status {
        PulsationStatus::Done => theme::accent(),
        PulsationStatus::Failed => theme::danger(),
        PulsationStatus::Checkpoint => theme::warning(),
        PulsationStatus::Idle | PulsationStatus::Planning | PulsationStatus::Running => {
            gpui::rgb(0x73b7ff)
        }
    }
}

fn section_heading(eyebrow: &'static str, title: &'static str, count: usize) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .child(
                    div()
                        .text_xs()
                        .text_color(theme::text_muted())
                        .child(eyebrow),
                )
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                ),
        )
        .child(
            div()
                .min_w(px(24.0))
                .h(px(24.0))
                .px_2()
                .rounded_full()
                .bg(theme::surface_raised())
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .child(count.to_string()),
        )
}

fn fact(label: &'static str, value: impl Into<String>) -> impl IntoElement {
    div()
        .mt_2()
        .p_3()
        .rounded_md()
        .bg(theme::surface_raised())
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(div().mt_1().child(value.into()))
}

fn empty_panel(title: &'static str, description: &'static str) -> impl IntoElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .text_lg()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div()
                .text_color(theme::text_muted())
                .text_sm()
                .child(description),
        )
}
