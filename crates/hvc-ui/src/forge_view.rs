use crate::{
    AppModel,
    forge::{ForgeAction, ForgeSection},
    theme,
};
use gpui::{
    AnyElement, Context, Div, Focusable, PathBuilder, Render, SharedString, Window, canvas,
    deferred, div, point, prelude::*, px, relative,
};
use hvc_core::feature_forge::*;

fn card() -> Div {
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface())
        .min_w_0()
}
fn label(value: impl Into<SharedString>) -> Div {
    div()
        .text_sm()
        .text_color(theme::text_muted())
        .child(value.into())
}
fn heading(value: impl Into<SharedString>) -> Div {
    div()
        .text_lg()
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .child(value.into())
}
pub(crate) fn button(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    action: ForgeAction,
    cx: &mut Context<AppModel>,
) -> AnyElement {
    let id = id.into();
    let keyboard_action = action.clone();
    div()
        .id(id.clone())
        .debug_selector(move || id.to_string())
        .px_3()
        .py_2()
        .rounded_md()
        .bg(theme::surface_raised())
        .text_color(theme::accent())
        .text_sm()
        .cursor_pointer()
        .tab_index(0)
        .hover(|s| s.bg(theme::accent_soft()))
        .on_key_down(
            cx.listener(move |model, event: &gpui::KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    model.forge_action(keyboard_action.clone(), cx);
                    if matches!(keyboard_action, ForgeAction::New(_)) {
                        focus_capture(model, window, cx);
                    }
                    cx.stop_propagation();
                }
            }),
        )
        .on_click(cx.listener(move |model, _, window, cx| {
            model.forge_action(action.clone(), cx);
            if matches!(action, ForgeAction::New(_)) {
                focus_capture(model, window, cx);
            }
        }))
        .child(title.into())
        .into_any_element()
}
fn focus_capture(model: &AppModel, window: &mut Window, cx: &Context<AppModel>) {
    let input = if model.forge.capture_feature {
        &model.forge.feature_title
    } else {
        &model.forge.drafts[0].title
    };
    input.read(cx).focus_handle(cx).focus(window);
}
fn status(model: &AppModel) -> Div {
    div()
        .debug_selector(|| "forge-status".into())
        .flex()
        .flex_col()
        .gap_2()
        .when_some(model.forge.notice.clone(), |d, s| d.child(label(s)))
        .when_some(model.forge.error.clone(), |d, s| {
            d.child(div().text_color(theme::danger()).child(s))
        })
}
pub(crate) fn forge_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    if model.workspace.read(cx).project_id.is_none() {
        return card()
            .child(heading("Abra um projeto para organizar tasks e features"))
            .into_any_element();
    }
    let mut tabs = div().flex().flex_wrap().gap_2();
    for section in ForgeSection::ALL {
        tabs = tabs.child(button(
            format!("forge-section-{}", section.id()),
            if model.forge.section == section {
                format!("● {}", section.label())
            } else {
                section.label().to_string()
            },
            ForgeAction::Section(section),
            cx,
        ));
    }
    let body = if model.forge.section == ForgeSection::Backlog {
        backlog_body(model, cx)
    } else {
        feature_body(model, cx)
    };
    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_4()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(heading("Forge"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(button(
                            "capture-feature",
                            "Nova feature",
                            ForgeAction::New(true),
                            cx,
                        ))
                        .child(button(
                            "new-tasks",
                            "Adicionar tasks",
                            ForgeAction::New(false),
                            cx,
                        ))
                        .child(button(
                            "refresh-forge",
                            "Atualizar",
                            ForgeAction::Refresh,
                            cx,
                        )),
                ),
        )
        .child(tabs)
        .child(status(model))
        .child(body)
        .into_any_element()
}

fn backlog_body(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut list = div()
        .id("task-backlog")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_2();
    for task in model
        .forge
        .snapshot
        .tasks
        .iter()
        .filter(|t| t.state != "superseded")
    {
        let selected = model.forge.selected_tasks.contains(&task.id);
        let source = task
            .draft
            .feature_id
            .as_ref()
            .and_then(|id| model.forge.snapshot.features.iter().find(|f| &f.id == id))
            .map(|f| f.title.clone())
            .unwrap_or_else(|| "Task avulsa".into());
        let mut row = card()
            .child(heading(task.draft.title.clone()))
            .child(label(format!(
                "{source} · {:?} · {}",
                task.draft.kind, task.state
            )))
            .child(label(task.draft.description.clone()));
        for criterion in &task.draft.criteria {
            row = row.child(label(format!("□ {}", criterion.description)));
        }
        if !task.draft.dependencies.is_empty() {
            row = row.child(label(format!(
                "Pré-requisitos: {}",
                task.draft
                    .dependencies
                    .iter()
                    .map(|id| model
                        .forge
                        .snapshot
                        .tasks
                        .iter()
                        .find(|t| &t.id == id)
                        .map(|t| t.draft.title.clone())
                        .unwrap_or_else(|| id.clone()))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        if let Some(pulse) = task
            .active_pulsation_id
            .as_ref()
            .or(task.completed_pulsation_id.as_ref())
        {
            row = row.child(button(
                format!("task-pulsation-{}", task.id),
                "Ver pulsação e evidências",
                ForgeAction::OpenPulsation(pulse.clone()),
                cx,
            ));
        }
        let assessment = hvc_core::orchestration::task_refine::assess(
            &task.draft.description,
            task.revision,
            40.try_into().expect("bounded threshold"),
        );
        if assessment.score.get() >= assessment.threshold.get() {
            row = row.child(label("A descrição pode precisar de mais detalhes:"));
            for contribution in assessment.contributions {
                row = row.child(label(contribution.reason));
            }
        }
        if let Some(evidence) = model
            .forge
            .snapshot
            .integration_evidence
            .iter()
            .find(|e| e.task_id == task.id)
        {
            row = row
                .child(label(format!("Evidência integrada: {}", evidence.summary)))
                .child(label(format!(
                    "Critérios: {}",
                    evidence.criterion_ids.join(", ")
                )));
        }
        if task.state == "ready"
            || (task.state == "queued"
                && task.active_pulsation_id.as_ref().is_some_and(|id| {
                    model.pulsations.read(cx).pulsations.iter().any(|p| {
                        &p.id == id
                            && p.status == hvc_core::persistence_service::PulsationStatus::Failed
                    })
                }))
        {
            row = row.child(button(
                format!("select-task-{}", task.id),
                if selected {
                    "✓ Selecionada"
                } else {
                    "Selecionar para pulsação"
                },
                ForgeAction::ToggleTask(task.id.clone()),
                cx,
            ));
        }
        if task.draft.feature_id.is_none() && task.state == "ready" {
            row = row.child(button(
                format!("edit-task-{}", task.id),
                "Editar task",
                ForgeAction::EditTask(task.id.clone()),
                cx,
            ));
        }
        if task.draft.feature_id.is_none()
            && task.state == "ready"
            && TaskService::suggest_feature(&task.draft)
        {
            row = row
                .child(label(
                    "Este pedido pode se beneficiar de uma conversa de feature.",
                ))
                .child(button(
                    format!("convert-task-{}", task.id),
                    "Levar ideia à Feature Forge",
                    ForgeAction::SuggestFeature(task.id.clone()),
                    cx,
                ));
        }
        list = list.child(row);
    }
    if model.forge.snapshot.tasks.is_empty() {
        list = list.child(label(
            "Adicione bugs e pequenos ajustes ou aprove uma feature para criar suas tasks.",
        ));
    }
    let mut selected = card()
        .child(heading("Compor pulsação"))
        .child(label(
            "Selecione tasks para executar e integrar como um único lote.",
        ))
        .child(model.forge.batch_title.clone());
    for task in model
        .forge
        .snapshot
        .tasks
        .iter()
        .filter(|t| model.forge.selected_tasks.contains(&t.id))
    {
        selected = selected.child(label(format!("• {}", task.draft.title)));
    }
    selected = selected
        .child(label(format!(
            "{} tasks selecionadas",
            model.forge.selected_tasks.len()
        )))
        .child(button(
            "dispatch-batch",
            "Enviar lote para execução",
            ForgeAction::Dispatch,
            cx,
        ));
    div()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .flex()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .gap_3()
                .child(button(
                    "retry-forge-publications",
                    "Reconciliar notas de planejamento",
                    ForgeAction::RetryPublications,
                    cx,
                ))
                .child(list),
        )
        .child(
            div()
                .w(px(310.0))
                .min_h_0()
                .id("batch-composer")
                .overflow_y_scroll()
                .child(selected),
        )
        .into_any_element()
}

#[derive(Clone)]
struct FeatureDrag {
    id: String,
    title: String,
}
impl Render for FeatureDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        card().child(self.title.clone())
    }
}

fn feature_body(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut list = div()
        .id("feature-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_2();
    for f in &model.forge.snapshot.features {
        let selected = model.forge.selected_feature.as_ref() == Some(&f.id);
        list = list.child(
            div()
                .id(SharedString::from(format!("feature-{}", f.id)))
                .p_3()
                .rounded_md()
                .bg(if selected {
                    theme::accent_soft()
                } else {
                    theme::surface_raised()
                })
                .cursor_pointer()
                .on_click(cx.listener({
                    let id = f.id.clone();
                    move |m, _, _, cx| m.forge_action(ForgeAction::SelectFeature(id.clone()), cx)
                }))
                .on_drag(
                    FeatureDrag {
                        id: f.id.clone(),
                        title: f.title.clone(),
                    },
                    |drag, _, _, cx| cx.new(|_| drag.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(f.title.clone())
                        .child(label(format!(
                            "{} · {}",
                            state(f.state),
                            f.evaluation
                                .score()
                                .map(|s| format!("score {s:.1}"))
                                .unwrap_or_else(|| "sem score".into())
                        ))),
                ),
        );
    }
    let detail_content = if model.forge.section == ForgeSection::Waves {
        waves(model, cx)
    } else if let Some(f) = model.forge.feature() {
        match model.forge.section {
            ForgeSection::Incubation => incubation(model, f, cx),
            ForgeSection::Triage => triage(model, f, cx),
            ForgeSection::Dependencies => dependencies(model, f, cx),
            _ => delivery(model, f, cx),
        }
    } else {
        card().child(heading("Transforme uma ideia em um plano executável")).child(label("Capture a ideia para conversar com o Feature Agent, avaliar alternativas e preparar suas tasks.")).child(button("forge-first-feature","Nova feature",ForgeAction::New(true),cx)).into_any_element()
    };
    let detail = div()
        .id("forge-detail")
        .debug_selector(|| "forge-detail".into())
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_y_scroll()
        .child(detail_content);
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .gap_3()
        .child(
            div()
                .w(px(245.0))
                .min_h_0()
                .flex()
                .flex_col()
                .gap_3()
                .child(list),
        )
        .child(detail)
        .into_any_element()
}
fn state(s: FeatureState) -> &'static str {
    match s {
        FeatureState::Incubating => "Incubação",
        FeatureState::InTriage => "Triagem",
        FeatureState::Approved => "Aprovada",
        FeatureState::Planned => "Planejada",
        FeatureState::Executing => "Em execução",
        FeatureState::ReadyForReview => "Revisar entrega",
        FeatureState::Done => "Concluída",
        FeatureState::Archived => "Arquivada",
    }
}
fn incubation(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let mut panel = card()
        .child(heading(f.title.clone()))
        .child(label(format!(
            "{} · revisão {} · contexto {}",
            state(f.state),
            f.revision,
            f.brief.coverage
        )))
        .child(label(f.original_idea.clone()));
    for turn in &model.forge.history {
        panel = panel.child(card().child(label("Você")).child(turn.user_message.clone()));
        if let Some(reply) = &turn.assistant_message {
            panel = panel.child(card().child(label("Feature Agent")).child(reply.clone()));
        }
        if let Some(error) = &turn.error {
            panel = panel.child(div().text_color(theme::danger()).child(error.clone()));
        }
        if turn.status == "running" {
            panel = panel
                .child(label("O Feature Agent está analisando o contexto…"))
                .child(button(
                    format!("cancel-feature-turn-{}", turn.id),
                    "Cancelar resposta",
                    ForgeAction::CancelTurn(turn.id.clone()),
                    cx,
                ));
        }
    }
    panel = panel.child(model.forge.message.clone()).child(button(
        "send-feature-message",
        "Enviar mensagem",
        ForgeAction::Send,
        cx,
    ));
    if !f.brief.objective.is_empty() {
        panel = panel.child(brief(f)).child(button(
            "approve-feature",
            "Aprovar escopo e criar tasks",
            ForgeAction::Approve,
            cx,
        ));
    }
    panel.into_any_element()
}
fn brief(f: &Feature) -> Div {
    let mut d = card().child(heading("Especificação proposta"));
    for (title, value) in [
        ("Objetivo", &f.brief.objective),
        ("Público", &f.brief.audience),
        ("Comportamento", &f.brief.behavior),
        ("Escopo", &f.brief.scope),
        ("Fora do escopo", &f.brief.exclusions),
    ] {
        d = d.child(label(title)).child(value.clone());
    }
    for decision in &f.brief.decisions {
        d = d.child(label(format!("Decisão: {decision}")));
    }
    for pending in &f.brief.pending_decisions {
        d = d.child(
            div()
                .text_color(theme::warning())
                .child(format!("Pendente: {pending}")),
        );
    }
    for alternative in &f.brief.alternatives {
        d = d.child(label(format!("Alternativa: {alternative}")));
    }
    for c in &f.brief.criteria {
        d = d.child(label(format!("Aceite: {}", c.description)));
    }
    for t in &f.brief.tasks {
        d = d.child(label(format!(
            "Task proposta: {} — {}",
            t.title, t.description
        )));
    }
    for s in &f.brief.sources {
        d = d.child(label(format!(
            "Fonte consultada: {}",
            s.relative_path.as_str()
        )));
    }
    d
}
fn triage(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let mut p = card().child(heading("Triagem e decisão")).child(label(
        "A avaliação orienta a prioridade. Você decide se a feature deve avançar.",
    ));
    for (i, name) in [
        "Valor e necessidade",
        "Alinhamento com o produto",
        "Viabilidade e engenharia",
        "Usabilidade e adoção",
    ]
    .into_iter()
    .enumerate()
    {
        p = p.child(label(name)).child(model.forge.filters[i].clone());
    }
    p = p
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(button(
                    "rating-impact",
                    format!("Impacto: {}", model.forge.impact),
                    ForgeAction::Rating(0),
                    cx,
                ))
                .child(button(
                    "rating-confidence",
                    format!("Confiança: {}", model.forge.confidence),
                    ForgeAction::Rating(1),
                    cx,
                ))
                .child(button(
                    "rating-effort",
                    format!("Esforço: {}", model.forge.effort),
                    ForgeAction::Rating(2),
                    cx,
                )),
        )
        .child(label(
            "Clique em uma nota para alternar de 1 a 5. Score = impacto × confiança ÷ esforço.",
        ))
        .child(button(
            "structural-level",
            format!(
                "Nível: {}",
                ["Fundação", "Domínio", "Interface", "Otimização"][model.forge.level as usize]
            ),
            ForgeAction::Level,
            cx,
        ))
        .child(button(
            "save-evaluation",
            "Salvar avaliação",
            ForgeAction::Evaluate,
            cx,
        ))
        .child(model.forge.rationale.clone());
    if f.state == FeatureState::Archived {
        p = p.child(button(
            "reopen-feature",
            "Reabrir feature",
            ForgeAction::Reopen,
            cx,
        ));
    } else {
        p = p
            .child(button(
                "archive-feature",
                "Arquivar com justificativa",
                ForgeAction::Archive,
                cx,
            ))
            .child(button(
                "triage-approve",
                "Aprovar escopo e tasks",
                ForgeAction::Approve,
                cx,
            ));
    }
    p.into_any_element()
}
fn dependencies(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let mut p=card().child(heading("Dependências confirmadas")).child(label("As setas apontam da feature para seus pré-requisitos. Adicionar uma relação exige sua confirmação e justificativa.")).child(model.forge.rationale.clone());
    for edge in model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter(|d| d.feature_id == f.id)
    {
        let pre = model
            .forge
            .snapshot
            .features
            .iter()
            .find(|x| x.id == edge.prerequisite_id);
        p = p.child(
            card()
                .child(format!(
                    "{} → {}",
                    f.title,
                    pre.map(|x| x.title.as_str())
                        .unwrap_or("Feature indisponível")
                ))
                .child(label(edge.rationale.clone()))
                .child(button(
                    format!("remove-edge-{}", edge.prerequisite_id),
                    "Remover relação",
                    ForgeAction::Dependency(edge.prerequisite_id.clone(), false),
                    cx,
                )),
        );
    }
    p = p.child(heading("Confirmar um pré-requisito"));
    for other in model.forge.snapshot.features.iter().filter(|x| {
        x.id != f.id
            && !model
                .forge
                .snapshot
                .dependencies
                .iter()
                .any(|e| e.feature_id == f.id && e.prerequisite_id == x.id)
    }) {
        p = p.child(button(
            format!("add-edge-{}", other.id),
            format!("Depende de {} ({})", other.title, state(other.state)),
            ForgeAction::Dependency(other.id.clone(), true),
            cx,
        ));
    }
    if let Some(turn) = model
        .forge
        .history
        .iter()
        .rev()
        .find(|t| t.status == "completed")
    {
        for edge in &turn.suggested_dependencies {
            if model
                .forge
                .snapshot
                .dependencies
                .iter()
                .any(|d| d.feature_id == f.id && d.prerequisite_id == edge.prerequisite_id)
            {
                continue;
            }
            if let Some(pre) = model
                .forge
                .snapshot
                .features
                .iter()
                .find(|x| x.id == edge.prerequisite_id)
            {
                p = p.child(
                    card()
                        .child(label(format!(
                            "Sugestão pendente: {} → {}",
                            f.title, pre.title
                        )))
                        .child(label(edge.rationale.clone()))
                        .child(button(
                            format!("accept-suggestion-{}", pre.id),
                            "Confirmar dependência sugerida",
                            ForgeAction::ConfirmSuggestion(
                                edge.prerequisite_id.clone(),
                                edge.rationale.clone(),
                            ),
                            cx,
                        )),
                );
            }
        }
    }
    card()
        .child(feature_graph(model, cx))
        .child(p)
        .into_any_element()
}
fn feature_graph(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut counts = [0usize; 4];
    let nodes = model
        .forge
        .snapshot
        .features
        .iter()
        .filter(|f| f.state != FeatureState::Archived)
        .map(|f| {
            let level = usize::from(f.structural_level.min(3));
            let row = counts[level];
            counts[level] += 1;
            (
                f.clone(),
                10.0 + level as f32 * 200.0,
                42.0 + row as f32 * 82.0,
            )
        })
        .collect::<Vec<_>>();
    let edges = model
        .forge
        .snapshot
        .dependencies
        .iter()
        .filter_map(|e| {
            let (_, x, y) = nodes.iter().find(|(f, _, _)| f.id == e.feature_id)?;
            let (_, tx, ty) = nodes.iter().find(|(f, _, _)| f.id == e.prerequisite_id)?;
            Some(if x == tx {
                (
                    *x + 90.0,
                    if y < ty { *y + 60.0 } else { *y - 3.0 },
                    *tx + 90.0,
                    if y < ty { *ty - 3.0 } else { *ty + 60.0 },
                )
            } else {
                (
                    if x < tx { *x + 180.0 } else { *x },
                    *y + 26.0,
                    if x < tx { *tx - 3.0 } else { *tx + 183.0 },
                    *ty + 26.0,
                )
            })
        })
        .collect::<Vec<_>>();
    let height = (counts.into_iter().max().unwrap_or(1) as f32 * 82.0 + 58.0).max(200.0);
    let mut graph = div().relative().w(px(800.0)).h(px(height)).child(
        canvas(
            move |_, _, _| edges,
            |bounds, edges, window, _| {
                for (x, y, tx, ty) in edges {
                    let mut path = PathBuilder::stroke(px(2.0));
                    let a = point(bounds.left() + px(x), bounds.top() + px(y));
                    let b = point(bounds.left() + px(tx), bounds.top() + px(ty));
                    path.move_to(a);
                    path.line_to(b);
                    let angle = (ty - y).atan2(tx - x);
                    for offset in [-0.5f32, 0.5] {
                        path.move_to(b);
                        path.line_to(point(
                            b.x - px((angle + offset).cos() * 10.0),
                            b.y - px((angle + offset).sin() * 10.0),
                        ));
                    }
                    if let Ok(path) = path.build() {
                        window.paint_path(path, theme::accent());
                    }
                }
            },
        )
        .absolute()
        .size_full(),
    );
    for (i, title) in ["Fundação / Onda 0", "Domínio", "Interface", "Otimização"]
        .iter()
        .enumerate()
    {
        graph = graph.child(
            label(*title)
                .absolute()
                .left(px(10.0 + i as f32 * 200.0))
                .top(px(8.0)),
        );
    }
    for (f, x, y) in nodes {
        graph = graph.child(
            div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(180.0))
                .child(button(
                    format!("graph-node-{}", f.id),
                    f.title,
                    ForgeAction::SelectFeature(f.id),
                    cx,
                )),
        );
    }
    div()
        .id("feature-graph")
        .debug_selector(|| "feature-graph".into())
        .min_w_0()
        .h(px(280.0))
        .overflow_scroll()
        .child(graph)
        .into_any_element()
}
fn waves(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut columns = div().flex().gap_2().min_w_0();
    for (wave, title) in [
        (Wave::Current, "Onda Atual"),
        (Wave::Next, "Próxima Onda"),
        (Wave::Horizon, "Horizonte"),
    ] {
        let mut column = card().flex_1().child(heading(title));
        for f in model
            .forge
            .snapshot
            .features
            .iter()
            .filter(|f| f.wave == Some(wave) && f.state != FeatureState::Archived)
        {
            column = column.child(
                div()
                    .id(SharedString::from(format!("wave-feature-{}", f.id)))
                    .on_drag(
                        FeatureDrag {
                            id: f.id.clone(),
                            title: f.title.clone(),
                        },
                        |drag, _, _, cx| cx.new(|_| drag.clone()),
                    )
                    .child(
                        card()
                            .child(f.title.clone())
                            .child(label(state(f.state)))
                            .children(
                                [
                                    (Wave::Current, "Atual"),
                                    (Wave::Next, "Próxima"),
                                    (Wave::Horizon, "Horizonte"),
                                ]
                                .into_iter()
                                .filter(|(w, _)| *w != wave)
                                .map(|(w, name)| {
                                    button(
                                        format!("move-{}-{w:?}", f.id),
                                        name,
                                        ForgeAction::Move(f.id.clone(), w),
                                        cx,
                                    )
                                }),
                            ),
                    ),
            );
        }
        columns = columns.child(
            column
                .id(SharedString::from(format!("wave-{wave:?}")))
                .debug_selector(move || format!("wave-{wave:?}"))
                .min_h(px(180.0))
                .on_drop(cx.listener(move |m, drag: &FeatureDrag, _, cx| {
                    m.forge_action(ForgeAction::Move(drag.id.clone(), wave), cx)
                })),
        );
    }
    let mut p=card().child(heading("Planejamento em ondas")).child(label(format!("Capacidade da Onda Atual: {} features. Arraste uma feature para uma onda ou use os botões.",model.forge.snapshot.current_wave_limit))).child(model.forge.wave_limit.clone()).child(button("set-wave-limit","Atualizar capacidade",ForgeAction::SetLimit,cx)).child(columns);
    for f in model.forge.snapshot.features.iter().filter(|f| {
        f.wave.is_none()
            && f.approved_revision == Some(f.revision)
            && f.state != FeatureState::Archived
    }) {
        p = p.child(
            card()
                .child(f.title.clone())
                .child(button(
                    format!("plan-current-{}", f.id),
                    if f.brief.foundation {
                        "Planejar fundação / Onda 0"
                    } else {
                        "Planejar na Onda Atual"
                    },
                    ForgeAction::Move(f.id.clone(), Wave::Current),
                    cx,
                ))
                .child(button(
                    format!("plan-next-{}", f.id),
                    "Planejar na Próxima Onda",
                    ForgeAction::Move(f.id.clone(), Wave::Next),
                    cx,
                )),
        );
    }
    p.into_any_element()
}
fn delivery(model: &AppModel, f: &Feature, cx: &mut Context<AppModel>) -> AnyElement {
    let tasks = model
        .forge
        .snapshot
        .tasks
        .iter()
        .filter(|t| {
            t.draft.feature_id.as_ref() == Some(&f.id) && t.feature_revision == f.approved_revision
        })
        .collect::<Vec<_>>();
    let done = tasks.iter().filter(|t| t.state == "done").count();
    let mut p = card()
        .child(heading("Entrega da feature"))
        .child(label(format!("{done}/{} tasks integradas", tasks.len())))
        .child(button(
            "compose-feature-tasks",
            "Selecionar tasks desta feature",
            ForgeAction::SelectFeatureTasks(f.id.clone()),
            cx,
        ));
    for t in tasks {
        p = p.child(
            card()
                .child(t.draft.title.clone())
                .child(label(t.state.clone()))
                .children(
                    t.draft
                        .criteria
                        .iter()
                        .map(|c| label(format!("□ {}", c.description))),
                ),
        );
    }
    for evidence in &model.forge.snapshot.integration_evidence {
        if let Some(task) = model.forge.snapshot.tasks.iter().find(|t| {
            t.id == evidence.task_id
                && t.draft.feature_id.as_ref() == Some(&f.id)
                && t.feature_revision == f.approved_revision
        }) {
            p = p.child(
                card()
                    .child(heading(format!("Revisão integrada: {}", task.draft.title)))
                    .child(label(evidence.summary.clone()))
                    .child(label(format!(
                        "Critérios cobertos: {}",
                        evidence.criterion_ids.join(", ")
                    ))),
            );
        }
    }
    for t in &model.forge.snapshot.tasks {
        if t.draft.feature_id.as_ref() == Some(&f.id)
            && let Some(pulse) = t
                .active_pulsation_id
                .as_ref()
                .or(t.completed_pulsation_id.as_ref())
        {
            p = p.child(button(
                format!("delivery-evidence-{}", t.id),
                format!("Evidências: {}", t.draft.title),
                ForgeAction::OpenPulsation(pulse.clone()),
                cx,
            ));
        }
    }
    p = p
        .child(heading("Revisão final dos critérios da feature"))
        .children(
            f.brief
                .criteria
                .iter()
                .map(|c| label(format!("□ {}", c.description))),
        )
        .child(label(
            "Confirme a entrega após revisar as evidências e publicações das pulsações vinculadas.",
        ))
        .child(button(
            "complete-feature",
            "Confirmar critérios e concluir feature",
            ForgeAction::Complete,
            cx,
        ));
    p.into_any_element()
}
pub(crate) fn capture_overlay(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let mut content = card()
        .debug_selector(|| "forge-capture-content".into())
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .w(px(720.0))
        .max_w(relative(0.9))
        .max_h(relative(0.9))
        .child(heading("Novo trabalho"))
        .child(
            div()
                .flex()
                .gap_2()
                .child(button(
                    "capture-tasks-option",
                    "Adicionar tasks",
                    ForgeAction::New(false),
                    cx,
                ))
                .child(button(
                    "capture-feature-option",
                    "Nova feature",
                    ForgeAction::New(true),
                    cx,
                ))
                .child(button("close-capture", "Fechar", ForgeAction::Close, cx)),
        )
        .child(status(model));
    let mut body = div()
        .id("capture-body")
        .debug_selector(|| "forge-capture-body".into())
        .min_h_0()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap_3();
    if model.forge.capture_feature {
        body = body
            .child(label(
                "Converse sobre a funcionalidade antes de definir sua execução.",
            ))
            .child(model.forge.feature_title.clone())
            .child(model.forge.idea.clone())
            .child(button(
                "create-feature",
                "Criar feature e iniciar conversa",
                ForgeAction::CreateFeature,
                cx,
            ));
    } else {
        for (i, draft) in model.forge.drafts.iter().enumerate() {
            body = body.child(
                card()
                    .child(label(format!("Task {}", i + 1)))
                    .child(draft.title.clone())
                    .child(draft.description.clone())
                    .child(draft.criteria.clone())
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(button(
                                format!("draft-kind-{i}"),
                                format!("Tipo: {:?}", draft.kind),
                                ForgeAction::Kind(i),
                                cx,
                            ))
                            .child(button(
                                format!("remove-draft-{i}"),
                                "Remover",
                                ForgeAction::RemoveDraft(i),
                                cx,
                            )),
                    ),
            );
        }
        for (i, draft) in model.forge.drafts.iter().enumerate() {
            body = body.child(label(format!("Pré-requisitos da task {}", i + 1)));
            for t in model.forge.snapshot.tasks.iter().filter(|t| {
                t.state != "superseded"
                    && model
                        .forge
                        .editing_task
                        .as_ref()
                        .is_none_or(|(id, _)| id != &t.id)
            }) {
                body = body.child(button(
                    format!("task-dependency-{i}-{}", t.id),
                    format!(
                        "{} {}",
                        if draft.dependencies.contains(&t.id) {
                            "✓"
                        } else {
                            "+"
                        },
                        t.draft.title
                    ),
                    ForgeAction::TaskDependency(i, t.id.clone()),
                    cx,
                ));
            }
        }
        if model.forge.editing_task.is_none() {
            body = body.child(button(
                "add-task-draft",
                "+ Outra task",
                ForgeAction::AddDraft,
                cx,
            ));
        }
        body = body.child(button(
            "save-task-drafts",
            "Salvar tasks no backlog",
            ForgeAction::SaveTasks,
            cx,
        ));
    }
    content = content.child(body);
    deferred(
        div()
            .debug_selector(|| "forge-capture-overlay".into())
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .occlude()
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|model, _, _, cx| {
                    model.forge_action(ForgeAction::Close, cx);
                    cx.stop_propagation();
                }),
            )
            .on_key_down(cx.listener(|model, event: &gpui::KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    model.forge_action(ForgeAction::Close, cx);
                    cx.stop_propagation();
                }
            }))
            .bg(gpui::rgba(0x000000aa))
            .child(content),
    )
    .with_priority(20)
    .into_any_element()
}
