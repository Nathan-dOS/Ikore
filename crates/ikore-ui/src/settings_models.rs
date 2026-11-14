//! Settings › Models & routes: each model once, by vendor, with every route that serves it.
//!
//! Nothing is configured here. The page is derived from what the CLIs and the API connections
//! list, so it changes when a catalog is read again or a connection is added. Configuration
//! stays where it was: connections in Connections, agents in Agents, the Companion in its composer.

use std::collections::HashMap;

use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px};
use ikore_core::llm::{
    ModelIdentity, ModelRoute, ModelSource, RoutedModel, VendorModels, models_by_vendor,
};
use ikore_core::terminals::{CliAvailabilityState, CliBackend};

use crate::app_model::AppModel;
use crate::settings::SettingsPage;
use crate::settings_ui::{PageContent, note, row_with, section, stacked_row};
use crate::theme;

/// Models of one vendor that get a row; the rest are counted.
const VENDOR_ROWS: usize = 24;

/// What the catalogs say about a model beyond its name.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct ModelFacts {
    context_window: Option<u64>,
    /// Effort levels, in the order the first catalog that names them lists them.
    efforts: Vec<String>,
}

impl ModelFacts {
    fn learn(&mut self, context_window: Option<u64>, efforts: &[String]) {
        self.context_window = self.context_window.or(context_window);
        for effort in efforts {
            if !self.efforts.contains(effort) {
                self.efforts.push(effort.clone());
            }
        }
    }

    fn line(&self) -> Option<String> {
        let mut parts = Vec::new();
        if let Some(window) = self.context_window {
            parts.push(format!(
                "{} context",
                crate::agents_view::format_context_window(window)
            ));
        }
        if !self.efforts.is_empty() {
            parts.push(format!("effort: {}", self.efforts.join(", ")));
        }
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

/// How a route reads on the page, and whether a run could take it now.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RouteLabel {
    text: String,
    usable: bool,
}

/// Everything the page shows, read from the models before any element is built.
#[derive(Debug, Default)]
struct ModelsOverview {
    vendors: Vec<VendorModels>,
    facts: HashMap<ModelIdentity, ModelFacts>,
    routes: HashMap<ModelRoute, RouteLabel>,
    /// One line per source: what it is and how many models it lists.
    sources: Vec<String>,
}

fn overview(model: &AppModel, cx: &gpui::App) -> ModelsOverview {
    let agents = model.agents.read(cx);
    let settings = model.settings.read(cx);
    let chat = model.chat.read(cx);
    let mut overview = ModelsOverview::default();

    let cli_state = |backend: CliBackend| {
        settings
            .cli_connections
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .connections
                    .iter()
                    .find(|connection| connection.availability.backend == backend)
            })
            .map(|connection| connection.availability.state)
            .or_else(|| {
                chat.cli_availability
                    .iter()
                    .find(|item| item.backend == backend)
                    .map(|item| item.state)
            })
    };
    let mut cli: Vec<(CliBackend, Vec<String>)> = Vec::new();
    for backend in CliBackend::ALL {
        if backend == CliBackend::Custom {
            continue;
        }
        let state = cli_state(backend);
        let ready = state.is_none_or(|state| state == CliAvailabilityState::Available);
        overview.routes.insert(
            ModelRoute::Cli(backend),
            RouteLabel {
                text: match state.filter(|_| !ready) {
                    Some(state) => format!(
                        "{} CLI · {}",
                        backend.label(),
                        crate::chat_view::cli_availability_label(state)
                    ),
                    None => format!("{} CLI", backend.label()),
                },
                usable: ready,
            },
        );
        let Some(catalog) = agents.model_catalogs.get(&backend) else {
            overview
                .sources
                .push(format!("{} CLI: catalog not read yet", backend.label()));
            continue;
        };
        let mut models = Vec::new();
        for entry in &catalog.models {
            let Some(model_id) = &entry.model_id else {
                continue;
            };
            if let Some(identity) = ModelIdentity::of(model_id, backend.as_str()) {
                overview
                    .facts
                    .entry(identity)
                    .or_default()
                    .learn(entry.context_window, &entry.profiles);
            }
            models.push(model_id.clone());
        }
        overview
            .sources
            .push(format!("{} CLI: {} models", backend.label(), models.len()));
        cli.push((backend, models));
    }

    let mut api: Vec<(String, String, Vec<String>)> = Vec::new();
    if let Some(snapshot) = &settings.snapshot {
        for connection in snapshot.connections.iter().filter(|item| item.enabled) {
            let usable = connection.blocked_reason.is_none();
            let label = |kind: &str| RouteLabel {
                text: format!(
                    "{kind} · {}{}",
                    connection.display_name,
                    if usable { "" } else { " · blocked" }
                ),
                usable,
            };
            overview.routes.insert(
                ModelRoute::Api {
                    connection_id: connection.id.clone(),
                },
                label("API"),
            );
            overview.routes.insert(
                ModelRoute::Gateway {
                    connection_id: connection.id.clone(),
                },
                label("Gateway"),
            );
            let mut models = Vec::new();
            let catalog = snapshot
                .api_model_catalogs
                .iter()
                .find(|catalog| catalog.connection_id == connection.id);
            for entry in catalog.into_iter().flat_map(|catalog| &catalog.models) {
                if let Some(identity) = ModelIdentity::of(&entry.model_id, &connection.provider_id)
                {
                    overview
                        .facts
                        .entry(identity)
                        .or_default()
                        .learn(entry.context_window, &entry.reasoning_efforts);
                }
                models.push(entry.model_id.clone());
            }
            overview.sources.push(if models.is_empty() {
                format!("{}: its default model only", connection.display_name)
            } else {
                format!("{}: {} models", connection.display_name, models.len())
            });
            // A connection serves its default model whether its catalog lists it or not.
            if !models.contains(&connection.default_model) {
                models.push(connection.default_model.clone());
            }
            api.push((
                connection.id.clone(),
                connection.provider_id.clone(),
                models,
            ));
        }
    }

    overview.vendors = models_by_vendor(
        cli.iter()
            .map(|(backend, models)| (ModelSource::Cli(*backend), models.as_slice()))
            .chain(api.iter().map(|(id, provider_id, models)| {
                (
                    ModelSource::Connection { id, provider_id },
                    models.as_slice(),
                )
            })),
    );
    overview
}

/// The models of a vendor in the order the page lists them: the ones a subscription or a key of
/// the vendor serves first, then the ones only a gateway reaches. Names keep their order in each.
fn listed(group: &VendorModels) -> Vec<&RoutedModel> {
    let direct =
        |model: &&RoutedModel| !matches!(model.routes.first(), Some(ModelRoute::Gateway { .. }));
    group
        .models
        .iter()
        .filter(direct)
        .chain(group.models.iter().filter(|model| !direct(model)))
        .collect()
}

fn route_chip(label: &RouteLabel) -> AnyElement {
    div()
        .flex_shrink_0()
        .px_2()
        .py(px(2.0))
        .rounded_full()
        .border_1()
        .border_color(if label.usable {
            theme::border_strong()
        } else {
            theme::border()
        })
        .text_xs()
        .text_color(if label.usable {
            theme::text()
        } else {
            theme::text_subtle()
        })
        .child(label.text.clone())
        .into_any_element()
}

pub(crate) fn models_page(
    model: &AppModel,
    hl: Option<&str>,
    cx: &mut Context<AppModel>,
) -> PageContent {
    let overview = overview(model, cx);
    let refreshing = model.agents.read(cx).catalogs_refreshing;
    let has_project = model.workspace.read(cx).project_id.is_some();

    let mut sources = div().w_full().min_w_0().flex().flex_col().gap_1();
    for line in &overview.sources {
        sources = sources.child(
            div()
                .text_sm()
                .text_color(theme::text_muted())
                .child(line.clone()),
        );
    }
    sources = sources
        .child(
            div()
                .mt_2()
                .child(crate::settings_view::action(
                    "settings-refresh-cli-catalogs",
                    if refreshing {
                        "Reading CLI catalogs…"
                    } else {
                        "Read CLI catalogs"
                    },
                    has_project && !refreshing,
                    cx,
                    |model, cx| model.refresh_cli_model_catalogs(cx),
                )),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme::text_subtle())
                .child(if has_project {
                    "CLI catalogs belong to the open project. An API catalog is refreshed from its connection, in Connections."
                } else {
                    "Open a project to read the CLI catalogs. An API catalog is refreshed from its connection, in Connections."
                }),
        );

    let mut page = PageContent::new(SettingsPage::Models.label())
        .description(SettingsPage::Models.description())
        .section(
            "Sources",
            section(
                "Sources",
                None,
                vec![stacked_row("models-sources", hl, sources)],
            ),
        );
    if overview.vendors.is_empty() {
        return page.section(
            "Models",
            section(
                "Models",
                None,
                vec![note(
                    "No model is listed yet. Add a connection, or read the CLI catalogs.",
                )],
            ),
        );
    }
    for group in &overview.vendors {
        let models = listed(group);
        let mut rows: Vec<AnyElement> = models
            .iter()
            .take(VENDOR_ROWS)
            .map(|model| {
                let chips = div()
                    .max_w(px(380.0))
                    .flex()
                    .flex_wrap()
                    .justify_end()
                    .gap_1()
                    .children(
                        model
                            .routes
                            .iter()
                            .filter_map(|route| overview.routes.get(route))
                            .map(route_chip),
                    );
                row_with(
                    &format!("model:{}:{}", group.vendor.id(), model.identity.model),
                    SharedString::from(model.identity.model.clone()),
                    None,
                    overview
                        .facts
                        .get(&model.identity)
                        .and_then(ModelFacts::line)
                        .map(SharedString::from),
                    false,
                    chips,
                )
            })
            .collect();
        if models.len() > VENDOR_ROWS {
            rows.push(note(format!(
                "{} more {} models are listed by these sources.",
                models.len() - VENDOR_ROWS,
                group.vendor.label()
            )));
        }
        page = page.section(
            group.vendor.label(),
            section(group.vendor.label(), None, rows),
        );
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    fn routed(model: &str, routes: Vec<ModelRoute>) -> RoutedModel {
        RoutedModel {
            identity: ModelIdentity::of(model, "openai").unwrap(),
            routes,
        }
    }

    #[test]
    fn a_vendor_lists_what_it_serves_directly_before_what_only_a_gateway_reaches() {
        let gateway = || ModelRoute::Gateway {
            connection_id: "router".into(),
        };
        let group = VendorModels {
            vendor: ikore_core::llm::ModelVendor::OpenAi,
            models: vec![
                routed("gpt-4.1", vec![gateway()]),
                routed("gpt-5", vec![ModelRoute::Cli(CliBackend::Codex), gateway()]),
                routed("gpt-5-mini", vec![gateway()]),
                routed(
                    "gpt-5-nano",
                    vec![ModelRoute::Api {
                        connection_id: "key".into(),
                    }],
                ),
            ],
        };
        let names: Vec<&str> = listed(&group)
            .iter()
            .map(|model| model.identity.model.as_str())
            .collect();
        assert_eq!(names, ["gpt-5", "gpt-5-nano", "gpt-4.1", "gpt-5-mini"]);
    }

    #[test]
    fn the_facts_of_a_model_merge_what_each_catalog_says() {
        let mut facts = ModelFacts::default();
        assert_eq!(facts.line(), None);
        facts.learn(None, &["low".into(), "high".into()]);
        facts.learn(Some(400_000), &["high".into(), "xhigh".into()]);
        // The first catalog that names a window keeps it.
        facts.learn(Some(128_000), &[]);
        assert_eq!(
            facts.line().as_deref(),
            Some("400k context · effort: low, high, xhigh")
        );
    }
}
