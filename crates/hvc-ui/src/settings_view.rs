use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};

use crate::app_model::AppModel;
use crate::theme;

pub(crate) fn settings_panel(model: &AppModel, cx: &mut Context<AppModel>) -> AnyElement {
    let connection_name = model.settings_connection_name_input.clone();
    let connection_model = model.settings_connection_model_input.clone();
    let connection_endpoint = model.settings_connection_endpoint_input.clone();
    let connection_secret = model.settings_connection_secret_input.clone();
    let route_name = model.settings_route_name_input.clone();
    let embedding_model = model.settings_embedding_model_input.clone();
    let (
        snapshot,
        loading,
        busy,
        selected_connection,
        selected_route,
        provider_id,
        route_strategy,
        binding_role,
        binding_stage,
        route_limits,
        route_target_count,
        notice,
        error,
    ) = {
        let settings = model.settings.read(cx);
        (
            settings.snapshot.clone(),
            settings.loading,
            settings.busy,
            settings.selected_connection_id.clone(),
            settings.selected_route_id.clone(),
            settings.connection_provider_id.clone(),
            settings.route_strategy.clone(),
            settings.binding_agent_role.clone(),
            settings.binding_stage.clone(),
            (
                settings.route_max_attempts,
                settings.route_attempt_timeout_ms,
                settings.route_timeout_ms,
            ),
            settings.route_targets.len(),
            settings.notice.clone(),
            settings.error.clone(),
        )
    };

    let Some(snapshot) = snapshot else {
        return div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme::text_muted())
            .child(if loading {
                "Carregando configuração LLM…"
            } else {
                "Configuração indisponível. Use Recarregar para tentar novamente."
            })
            .into_any_element();
    };

    let mode = snapshot.config.routing_v2_mode.as_str();
    let strategy = snapshot.config.context_strategy.as_str();
    let selected_profile = snapshot
        .profiles
        .iter()
        .find(|profile| profile.id == provider_id.as_ref());
    let endpoint_allowed = selected_profile.is_some_and(|profile| profile.endpoint_configurable);

    div()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .p_3()
        .flex()
        .gap_3()
        .child(
            div()
                .w(px(330.0))
                .min_h_0()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .flex()
                .flex_col()
                .gap_3()
                .child(section_header("PROVIDERS", "Conexões", snapshot.connections.len()))
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .id("settings-connections-scroll")
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(snapshot.connections.is_empty(), |list| {
                            list.child(empty("Nenhuma conexão configurada."))
                        })
                        .children(snapshot.connections.into_iter().map(|connection| {
                            let id = connection.id.clone();
                            let selected = selected_connection.as_ref().is_some_and(|selected| selected.as_ref() == id);
                            div()
                                .id(SharedString::from(format!("settings-connection:{id}")))
                                .p_3()
                                .rounded_md()
                                .border_1()
                                .border_color(if selected { theme::accent() } else { theme::border() })
                                .bg(if selected { theme::accent_soft() } else { theme::surface_raised() })
                                .cursor_pointer()
                                .on_click(cx.listener(move |model, _, _, cx| {
                                    model.select_settings_connection(id.clone(), cx);
                                }))
                                .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(connection.display_name))
                                .child(div().text_xs().text_color(theme::text_muted()).child(format!(
                                    "{} · {} · key {}",
                                    connection.provider_id,
                                    connection.default_model,
                                    if connection.has_api_key { "salva" } else { "ausente" }
                                )))
                                .child(div().text_xs().text_color(if connection.enabled { theme::accent() } else { theme::warning() }).child(connection.state))
                        })),
                )
                .child(section_header("ROUTING", "Rotas", snapshot.routes.len()))
                .child(
                    div()
                        .max_h(px(190.0))
                        .id("settings-routes-scroll")
                        .overflow_y_scroll()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .when(snapshot.routes.is_empty(), |list| list.child(empty("Nenhuma rota configurada.")))
                        .children(snapshot.routes.into_iter().map(|route| {
                            let id = route.id.clone();
                            let selected = selected_route.as_ref().is_some_and(|selected| selected.as_ref() == id);
                            div()
                                .id(SharedString::from(format!("settings-route:{id}")))
                                .p_2()
                                .rounded_md()
                                .border_1()
                                .border_color(if selected { theme::accent() } else { theme::border() })
                                .cursor_pointer()
                                .on_click(cx.listener(move |model, _, _, cx| model.select_settings_route(id.clone(), cx)))
                                .child(format!("{} · {} · {} alvo(s)", route.name, route.strategy, route.targets.len()))
                        })),
                ),
        )
        .child(
            div()
                .id("settings-detail-scroll")
                .flex_1()
                .min_w_0()
                .min_h_0()
                .p_4()
                .rounded_lg()
                .border_1()
                .border_color(theme::border())
                .bg(theme::surface())
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div().flex().items_center().justify_between()
                        .child(div().text_xl().font_weight(gpui::FontWeight::BOLD).child("Runtime & LLM routing"))
                        .child(action("settings-refresh", if loading { "Carregando…" } else { "Recarregar" }, !loading && !busy, cx, |model, cx| model.refresh_settings(cx))),
                )
                .when_some(notice, |panel, notice| panel.child(banner(notice, false)))
                .when_some(error, |panel, error| panel.child(banner(error.presentable().into(), true)))
                .child(
                    div().grid().grid_cols(3).gap_2()
                        .child(toggle("settings-routing-mode", "ROUTING V2", mode, !busy, cx, |model, cx| model.cycle_routing_mode(cx)))
                        .child(toggle("settings-context-strategy", "CONTEXTO", strategy, !busy, cx, |model, cx| model.toggle_context_strategy(cx)))
                        .child(toggle("settings-embeddings", "EMBEDDINGS", if snapshot.config.embedding_enabled { "on" } else { "off" }, !busy, cx, |model, cx| model.toggle_embeddings(cx))),
                )
                .child(
                    card("RAG / YAML Graph")
                        .child(div().text_sm().text_color(theme::text_muted()).child(format!(
                            "Provider: {} · top K: {} · {} key(s) no keyring",
                            snapshot.config.embedding_provider.as_str(), snapshot.config.embedding_top_k,
                            snapshot.key_statuses.iter().filter(|status| status.has_api_key).count()
                        )))
                        .child(embedding_model)
                        .child(div().flex().gap_2()
                            .child(action("settings-embedding-top-k", "Alternar top K", !busy, cx, |model, cx| model.cycle_embedding_top_k(cx)))
                            .child(action("settings-reindex", "Reindexar workspace", !busy, cx, |model, cx| model.reindex_embeddings(cx)))),
                )
                .child(
                    card("Conexão selecionada / nova")
                        .child(action("settings-new-connection", "Nova conexão", !busy, cx, |model, cx| model.new_settings_connection(cx)))
                        .child(div().flex().gap_2()
                            .child(toggle("settings-provider", "PROVIDER", provider_id.clone(), !busy, cx, |model, cx| model.cycle_settings_provider(cx)))
                            .child(div().flex_1().child(connection_name)))
                        .child(connection_model)
                        .when(endpoint_allowed, |section| section.child(connection_endpoint))
                        .when(!endpoint_allowed, |section| section.child(div().text_xs().text_color(theme::text_muted()).child("Endpoint fixo pelo perfil selecionado.")))
                        .child(connection_secret)
                        .child(div().flex().gap_2()
                            .child(action("settings-save-connection", "Salvar conexão", !busy, cx, |model, cx| model.save_settings_connection(cx)))
                            .child(action("settings-test-connection", "Testar", !busy && selected_connection.is_some(), cx, |model, cx| model.test_settings_connection(cx)))
                            .child(action("settings-delete-connection", "Excluir", !busy && selected_connection.is_some(), cx, |model, cx| model.delete_settings_connection(cx)))),
                )
                .child(
                    card("Rota selecionada / nova")
                        .child(action("settings-new-route", "Nova rota", !busy, cx, |model, cx| model.new_settings_route(cx)))
                        .child(route_name)
                        .child(toggle("settings-route-strategy", "ESTRATÉGIA", route_strategy, !busy, cx, |model, cx| model.cycle_route_strategy(cx)))
                        .child(div().text_sm().text_color(theme::text_muted()).child(format!(
                            "Limites: {} tentativa(s) · {} ms/tentativa · {} ms/rota · {} alvo(s)",
                            route_limits.0, route_limits.1, route_limits.2, route_target_count
                        )))
                        .child(div().flex().gap_2()
                            .child(action("settings-route-limits", "Alternar limites", !busy, cx, |model, cx| model.cycle_route_limits(cx)))
                            .child(action("settings-target-up", "Alvo ↑", !busy && selected_connection.is_some(), cx, |model, cx| model.move_selected_route_target(-1, cx)))
                            .child(action("settings-target-down", "Alvo ↓", !busy && selected_connection.is_some(), cx, |model, cx| model.move_selected_route_target(1, cx))))
                        .child(div().flex().gap_2()
                            .child(action("settings-save-route", "Salvar rota", !busy && selected_connection.is_some(), cx, |model, cx| model.save_settings_route(cx)))
                            .child(action("settings-delete-route", "Excluir", !busy && selected_route.is_some(), cx, |model, cx| model.delete_settings_route(cx)))),
                )
                .child(
                    card("Binding exato")
                        .child(div().text_sm().child(format!("Agente: {binding_role} · estágio: {binding_stage}")))
                        .child(div().text_xs().text_color(theme::text_muted()).child(format!("{} binding(s) persistido(s)", snapshot.bindings.len())))
                        .child(div().flex().gap_2()
                            .child(action("settings-binding-role", "Alternar agente", !busy, cx, |model, cx| model.cycle_binding_role(cx)))
                            .child(action("settings-binding-stage", "Alternar estágio", !busy, cx, |model, cx| model.cycle_binding_stage(cx)))
                            .child(action("settings-save-binding", "Vincular rota", !busy && selected_route.is_some(), cx, |model, cx| model.save_settings_binding(cx)))),
                )
                .child(
                    card("Resiliência")
                        .child(div().text_sm().text_color(theme::text_muted()).child(format!(
                            "{} estado(s) · snapshot {}",
                            snapshot.resilience.states.len(), snapshot.resilience.generated_at
                        )))
                        .child(action("settings-reset-resilience", "Limpar cooldowns", !busy, cx, |model, cx| model.reset_settings_resilience(cx))),
                )
        )
        .into_any_element()
}

fn section_header(kicker: &'static str, title: &'static str, count: usize) -> impl IntoElement {
    div()
        .flex()
        .items_end()
        .justify_between()
        .child(
            div()
                .child(div().text_xs().text_color(theme::accent()).child(kicker))
                .child(
                    div()
                        .text_lg()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(title),
                ),
        )
        .child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(count.to_string()),
        )
}

fn card(title: &'static str) -> gpui::Div {
    div()
        .p_3()
        .rounded_lg()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .flex()
        .flex_col()
        .gap_2()
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(title))
}

fn empty(message: &'static str) -> impl IntoElement {
    div()
        .p_3()
        .text_sm()
        .text_color(theme::text_muted())
        .child(message)
}

fn banner(message: SharedString, error: bool) -> impl IntoElement {
    div()
        .p_3()
        .rounded_md()
        .bg(if error {
            theme::danger_soft()
        } else {
            theme::accent_soft()
        })
        .text_color(if error {
            theme::danger()
        } else {
            theme::accent()
        })
        .child(message)
}

fn action(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .bg(if enabled {
            theme::accent()
        } else {
            theme::surface()
        })
        .text_color(if enabled {
            theme::canvas()
        } else {
            theme::text_muted()
        })
        .when(enabled, |button| {
            button
                .cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(label)
}

fn toggle(
    id: &'static str,
    label: &'static str,
    value: impl Into<SharedString>,
    enabled: bool,
    cx: &mut Context<AppModel>,
    handler: impl Fn(&mut AppModel, &mut Context<AppModel>) + 'static,
) -> impl IntoElement {
    let value = value.into();
    div()
        .id(id)
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(theme::border())
        .bg(theme::surface_raised())
        .when(enabled, |item| {
            item.cursor_pointer()
                .on_click(cx.listener(move |model, _, _, cx| handler(model, cx)))
        })
        .child(div().text_xs().text_color(theme::text_muted()).child(label))
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(value))
}
