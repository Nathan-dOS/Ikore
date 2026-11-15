//! The models the Companion can talk to, whatever serves them, and choosing one of them.
//!
//! The developer picks a model; the host picks the route. The route in use is kept when it
//! serves the model, and it stays visible in the composer, where it can be changed.

use std::collections::HashMap;

use gpui::Context;
use ikore_core::chat_service::{ChatExecutionTargetRequest, ChatTransportKind};
use ikore_core::llm::{
    ModelIdentity, ModelRoute, ModelSource, RouteOffer, VendorModels, models_by_vendor,
};
use ikore_core::terminals::{
    CliAvailabilityState, CliBackend, CliModelCatalogSnapshot, CliModelSelectionSource,
};

use crate::AppModel;
use crate::chat::ChatModel;

/// Every model the Companion can be pointed at, by vendor, each with its routes. A CLI offers
/// its whole catalog; an API connection answers the Companion with its default model only.
pub(crate) fn companion_models(
    chat: &ChatModel,
    catalogs: &HashMap<CliBackend, CliModelCatalogSnapshot>,
) -> Vec<VendorModels> {
    let cli: Vec<(CliBackend, Vec<String>)> = CliBackend::ALL
        .into_iter()
        .filter(|backend| *backend != CliBackend::Custom)
        .filter_map(|backend| {
            let catalog = catalogs.get(&backend)?;
            Some((
                backend,
                catalog
                    .models
                    .iter()
                    .filter_map(|entry| entry.model_id.clone())
                    .collect(),
            ))
        })
        .collect();
    let api: Vec<(&str, &str, [String; 1])> = chat
        .eligible_connections()
        .map(|connection| {
            (
                connection.id.as_str(),
                connection.provider_id.as_str(),
                [connection.default_model.clone()],
            )
        })
        .collect();
    models_by_vendor(
        cli.iter()
            .map(|(backend, models)| (ModelSource::Cli(*backend), models.as_slice()))
            .chain(api.iter().map(|(id, provider_id, models)| {
                (
                    ModelSource::Connection { id, provider_id },
                    models.as_slice(),
                )
            })),
    )
}

/// How a route reads in a list: `Codex CLI`, `API · OpenAI key`, `Gateway · OpenRouter`.
pub(crate) fn route_text(chat: &ChatModel, route: &ModelRoute) -> String {
    let connection = |id: &str| {
        chat.connections
            .iter()
            .find(|connection| connection.id == id)
            .map_or(id, |connection| connection.display_name.as_str())
            .to_owned()
    };
    match route {
        ModelRoute::Cli(backend) => format!("{} CLI", backend.label()),
        ModelRoute::Api { connection_id } => format!("API · {}", connection(connection_id)),
        ModelRoute::Gateway { connection_id } => {
            format!("Gateway · {}", connection(connection_id))
        }
    }
}

/// The model a target names, when it names one: the CLI's own default does not.
pub(crate) fn target_identity(
    chat: &ChatModel,
    target: &ChatExecutionTargetRequest,
) -> Option<ModelIdentity> {
    match target {
        ChatExecutionTargetRequest::Cli { backend, model, .. } => {
            ModelIdentity::of(model.as_deref()?, backend.as_str())
        }
        ChatExecutionTargetRequest::Api { connection_id } => {
            let connection = chat
                .connections
                .iter()
                .find(|connection| &connection.id == connection_id)?;
            ModelIdentity::of(&connection.default_model, &connection.provider_id)
        }
    }
}

/// The id `identity` has in the catalog of `backend`, when that CLI lists the model.
pub(crate) fn model_on_backend(
    catalogs: &HashMap<CliBackend, CliModelCatalogSnapshot>,
    identity: &ModelIdentity,
    backend: CliBackend,
) -> Option<String> {
    catalogs.get(&backend)?.models.iter().find_map(|entry| {
        let model_id = entry.model_id.as_ref()?;
        (ModelIdentity::of(model_id, backend.as_str()).as_ref() == Some(identity))
            .then(|| model_id.clone())
    })
}

/// The route the host takes for `identity`: the one in use when it serves the model, otherwise
/// the best one that can run now. A CLI that is not ready is skipped.
pub(crate) fn preferred_route(
    chat: &ChatModel,
    catalogs: &HashMap<CliBackend, CliModelCatalogSnapshot>,
    identity: &ModelIdentity,
) -> Option<RouteOffer> {
    let vendors = companion_models(chat, catalogs);
    let model = vendors
        .iter()
        .flat_map(|group| &group.models)
        .find(|model| &model.identity == identity)?;
    let current = match chat.selected_transport {
        ChatTransportKind::Cli => Some(ModelSource::Cli(chat.selected_cli_backend)),
        ChatTransportKind::Api => {
            chat.selected_connection_id
                .as_ref()
                .map(|id| ModelSource::Connection {
                    id: id.as_ref(),
                    provider_id: "",
                })
        }
    };
    let ready = |route: &ModelRoute| match route {
        ModelRoute::Cli(backend) => chat
            .cli_availability
            .iter()
            .find(|item| item.backend == *backend)
            .is_none_or(|item| item.state == CliAvailabilityState::Available),
        ModelRoute::Api { .. } | ModelRoute::Gateway { .. } => true,
    };
    model.preferred(current, ready).cloned()
}

impl AppModel {
    /// Points the Companion at a model, whatever serves it. See [`preferred_route`] for the route.
    pub(crate) fn select_companion_model(
        &mut self,
        identity: &ModelIdentity,
        cx: &mut Context<Self>,
    ) {
        let offer = {
            let chat = self.chat.read(cx);
            if chat.target_change_pending {
                None
            } else {
                preferred_route(chat, &self.agents.read(cx).model_catalogs, identity)
            }
        };
        self.close_cli_model_picker(cx);
        let Some(offer) = offer else {
            return;
        };
        match offer.route {
            ModelRoute::Cli(backend) => {
                self.chat.update(cx, |chat, _| {
                    chat.selected_transport = ChatTransportKind::Cli;
                    chat.selected_cli_backend = backend;
                    chat.selected_cli_model = Some(offer.model_id);
                    chat.selected_cli_model_profile = None;
                    chat.selected_cli_model_source = CliModelSelectionSource::Catalog;
                });
                self.apply_chat_target_change(cx);
            }
            ModelRoute::Api { connection_id } | ModelRoute::Gateway { connection_id } => {
                self.set_chat_route_api(connection_id, cx);
            }
        }
    }

    /// The id of the panel's model on `backend`, so that changing the route keeps the model
    /// when the new CLI lists it.
    pub(crate) fn panel_model_on(&self, backend: CliBackend, cx: &gpui::App) -> Option<String> {
        let chat = self.chat.read(cx);
        let identity = target_identity(chat, &chat.panel_target()?)?;
        model_on_backend(&self.agents.read(cx).model_catalogs, &identity, backend)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ikore_core::llm::{ModelVendor, ProviderConnection};
    use ikore_core::terminals::{
        CliAvailability, CliCapabilities, CliModelCatalogEntry, CliModelCatalogSource,
        CliModelCatalogState, CliOutputFormat,
    };

    fn catalog(backend: CliBackend, models: &[&str]) -> CliModelCatalogSnapshot {
        CliModelCatalogSnapshot {
            project_id: "project".into(),
            backend,
            adapter_contract_version: 2,
            state: CliModelCatalogState::Ready,
            source: CliModelCatalogSource::Cli,
            cli_version: None,
            diagnostic: None,
            refreshed_at: "now".into(),
            models: models
                .iter()
                .map(|id| CliModelCatalogEntry {
                    provider_id: None,
                    model_id: Some((*id).into()),
                    display_name: (*id).into(),
                    context_window: None,
                    input_cost_usd_per_million: None,
                    output_cost_usd_per_million: None,
                    capabilities: Vec::new(),
                    profiles: Vec::new(),
                    raw_metadata: Default::default(),
                })
                .collect(),
        }
    }

    fn connection(id: &str, provider: &str, model: &str, eligible: bool) -> ProviderConnection {
        ProviderConnection {
            id: id.into(),
            provider_id: provider.into(),
            display_name: format!("{id} name"),
            endpoint_url: None,
            default_model: model.into(),
            enabled: true,
            state: "active".into(),
            blocked_reason: None,
            cooldown_until: None,
            max_concurrency: 1,
            version: 1,
            has_api_key: true,
            chat_eligible: eligible,
            created_at: "now".into(),
            updated_at: "now".into(),
        }
    }

    fn availability(backend: CliBackend, state: CliAvailabilityState) -> CliAvailability {
        CliAvailability {
            backend,
            state,
            executable_path: None,
            version: None,
            diagnostic: None,
            install_url: "https://example.invalid",
            install_command: "install",
            quota: Vec::new(),
            capabilities: CliCapabilities {
                output_format: CliOutputFormat::JsonLines,
                structured_output: true,
                native_resume: true,
                thinking_levels: vec![],
                auth_probe: true,
                read_only: true,
                model_catalog: true,
                action_frames: false,
                reasoning_blocks: false,
            },
        }
    }

    fn fixture() -> (ChatModel, HashMap<CliBackend, CliModelCatalogSnapshot>) {
        let mut chat = ChatModel::default();
        chat.connections = vec![
            connection("key", "openai", "gpt-5", true),
            connection("router", "openrouter", "anthropic/claude-sonnet-4-5", true),
            // A connection the Companion cannot use offers nothing.
            connection("blocked", "openai", "gpt-4.1", false),
        ];
        let catalogs = HashMap::from([
            (
                CliBackend::Codex,
                catalog(CliBackend::Codex, &["gpt-5", "gpt-5-codex"]),
            ),
            (
                CliBackend::Opencode,
                catalog(CliBackend::Opencode, &["openai/gpt-5"]),
            ),
        ]);
        (chat, catalogs)
    }

    fn named(vendor: ModelVendor, model: &str) -> ModelIdentity {
        ModelIdentity {
            vendor,
            model: model.into(),
        }
    }

    #[test]
    fn a_cli_offers_its_catalog_and_a_connection_its_default_model() {
        let (chat, catalogs) = fixture();
        let listed: Vec<String> = companion_models(&chat, &catalogs)
            .iter()
            .flat_map(|group| &group.models)
            .map(|model| {
                format!(
                    "{}: {}",
                    model.identity.model,
                    model
                        .routes
                        .iter()
                        .map(|offer| route_text(&chat, &offer.route))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                "gpt-5: Codex CLI, OpenCode CLI, API · key name",
                "gpt-5-codex: Codex CLI",
                "claude-sonnet-4-5: Gateway · router name",
            ]
        );
    }

    #[test]
    fn a_target_names_a_model_and_a_cli_may_list_it_under_another_id() {
        let (chat, catalogs) = fixture();
        let on_key = ChatExecutionTargetRequest::Api {
            connection_id: "key".into(),
        };
        let gpt = named(ModelVendor::OpenAi, "gpt-5");
        assert_eq!(target_identity(&chat, &on_key), Some(gpt.clone()));
        let default = ChatExecutionTargetRequest::Cli {
            backend: CliBackend::Codex,
            model: None,
            model_profile: None,
        };
        assert_eq!(target_identity(&chat, &default), None);
        assert_eq!(
            model_on_backend(&catalogs, &gpt, CliBackend::Opencode).as_deref(),
            Some("openai/gpt-5")
        );
        assert_eq!(model_on_backend(&catalogs, &gpt, CliBackend::Claude), None);
        assert_eq!(
            model_on_backend(
                &catalogs,
                &named(ModelVendor::Anthropic, "claude-sonnet-4-5"),
                CliBackend::Codex
            ),
            None
        );
    }

    #[test]
    fn the_route_in_use_is_kept_and_a_cli_that_is_not_ready_is_skipped() {
        let (mut chat, catalogs) = fixture();
        let gpt = named(ModelVendor::OpenAi, "gpt-5");
        let route = |chat: &ChatModel| {
            preferred_route(chat, &catalogs, &gpt).map(|offer| route_text(chat, &offer.route))
        };
        // On the key, which serves the model: it stays.
        chat.selected_transport = ChatTransportKind::Api;
        chat.selected_connection_id = Some("key".into());
        assert_eq!(route(&chat).as_deref(), Some("API · key name"));
        // On a CLI that does not list it: the best route, a subscription first.
        chat.selected_transport = ChatTransportKind::Cli;
        chat.selected_cli_backend = CliBackend::Claude;
        assert_eq!(route(&chat).as_deref(), Some("Codex CLI"));
        // A CLI that needs a login is passed over for the next route that can run.
        chat.cli_availability = vec![
            availability(CliBackend::Codex, CliAvailabilityState::AuthRequired),
            availability(CliBackend::Opencode, CliAvailabilityState::Available),
        ];
        assert_eq!(route(&chat).as_deref(), Some("OpenCode CLI"));
        assert_eq!(
            preferred_route(
                &chat,
                &catalogs,
                &named(ModelVendor::Google, "gemini-2.5-pro")
            ),
            None
        );
    }
}
