//! Model identity: the model a developer chooses, apart from the route that serves it.
//!
//! Configuration stays keyed by transport: a CLI backend, an API connection. This layer is
//! derived from it, so `gpt-5` reads as one model whether a CLI subscription, an API key or a
//! gateway serves it. The route stays visible, because it decides the tools and hardening of
//! the run, native resume, and what pays for the call.

use std::collections::BTreeMap;

use crate::terminals::CliBackend;

/// Who makes a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ModelVendor {
    OpenAi,
    Anthropic,
    Google,
    DeepSeek,
    Mistral,
    XAi,
    Meta,
    Alibaba,
    /// A name no known vendor publishes: a local build, a router alias.
    Other,
}

impl ModelVendor {
    /// In the order lists show them.
    pub const ALL: [Self; 9] = [
        Self::OpenAi,
        Self::Anthropic,
        Self::Google,
        Self::DeepSeek,
        Self::Mistral,
        Self::XAi,
        Self::Meta,
        Self::Alibaba,
        Self::Other,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Google => "google",
            Self::DeepSeek => "deepseek",
            Self::Mistral => "mistral",
            Self::XAi => "xai",
            Self::Meta => "meta",
            Self::Alibaba => "alibaba",
            Self::Other => "other",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Google => "Google",
            Self::DeepSeek => "DeepSeek",
            Self::Mistral => "Mistral",
            Self::XAi => "xAI",
            Self::Meta => "Meta",
            Self::Alibaba => "Alibaba",
            Self::Other => "Other",
        }
    }

    /// The vendor a model name belongs to. The first-party families come from the same table
    /// that detects a model sent to the wrong provider.
    fn of_name(name: &str) -> Option<Self> {
        if let Some(provider) = super::profiles::model_family(name) {
            return Self::of_server(provider);
        }
        const MORE_FAMILIES: [(ModelVendor, &[&str]); 3] = [
            (ModelVendor::XAi, &["grok-"]),
            (ModelVendor::Meta, &["llama", "meta-llama"]),
            (ModelVendor::Alibaba, &["qwen", "qwq"]),
        ];
        MORE_FAMILIES
            .iter()
            .find(|(_, family)| family.iter().any(|prefix| name.starts_with(prefix)))
            .map(|(vendor, _)| *vendor)
    }

    /// The vendor an aggregator names before the slash, as in `openai/gpt-5`.
    fn of_namespace(namespace: &str) -> Option<Self> {
        match namespace {
            "openai" => Some(Self::OpenAi),
            "anthropic" => Some(Self::Anthropic),
            "google" | "gemini" => Some(Self::Google),
            "deepseek" => Some(Self::DeepSeek),
            "mistral" | "mistralai" => Some(Self::Mistral),
            "x-ai" | "xai" => Some(Self::XAi),
            "meta" | "meta-llama" => Some(Self::Meta),
            "qwen" | "alibaba" => Some(Self::Alibaba),
            _ => None,
        }
    }

    /// The vendor of an API provider or a CLI that serves one vendor's models only. Aggregators,
    /// local servers and the multi-vendor CLIs have none.
    pub fn of_server(provider_or_backend: &str) -> Option<Self> {
        match provider_or_backend {
            "openai" | "openai_legacy" | "codex" => Some(Self::OpenAi),
            "anthropic" | "claude" => Some(Self::Anthropic),
            "gemini" => Some(Self::Google),
            "deepseek" => Some(Self::DeepSeek),
            "mistral" => Some(Self::Mistral),
            _ => None,
        }
    }
}

/// A model as the developer thinks of it: who makes it and what it is called, whatever lists it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelIdentity {
    pub vendor: ModelVendor,
    /// Lower case, without the `models/` prefix of Gemini or the namespace of an aggregator. A
    /// model of no known vendor keeps its whole id: nothing says what its namespace means.
    pub model: String,
}

impl ModelIdentity {
    /// The identity of `model_id` as `served_by` lists it; `served_by` is a provider id or a CLI
    /// backend id. The name decides the vendor, then the namespace an aggregator gave it, and the
    /// server only when it serves a single vendor (`sonnet` on the Claude CLI). `default`, the
    /// CLI's own choice, names no model.
    pub fn of(model_id: &str, served_by: &str) -> Option<Self> {
        let id = model_id.trim().to_ascii_lowercase();
        let id = id.strip_prefix("models/").unwrap_or(&id);
        if id.is_empty() || id == "default" {
            return None;
        }
        let (namespace, name) = match id.rsplit_once('/') {
            Some((namespace, name)) => (namespace.rsplit('/').next(), name),
            None => (None, id),
        };
        let vendor = ModelVendor::of_name(name)
            .or_else(|| namespace.and_then(ModelVendor::of_namespace))
            .or_else(|| ModelVendor::of_server(served_by));
        Some(match vendor {
            Some(vendor) => Self {
                vendor,
                model: name.to_owned(),
            },
            None => Self {
                vendor: ModelVendor::Other,
                model: id.to_owned(),
            },
        })
    }
}

/// What serves a model, and so who pays and what the agent may do. Lists put the best first: a
/// CLI subscription, then a key of the model's own vendor, then a gateway.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ModelRoute {
    /// A CLI the developer signs in to: the subscription pays.
    Cli(CliBackend),
    /// An API connection of the model's own vendor.
    Api { connection_id: String },
    /// A connection that serves other vendors' models: an aggregator, or a local server.
    Gateway { connection_id: String },
}

impl ModelRoute {
    const fn rank(&self) -> u8 {
        match self {
            Self::Cli(_) => 0,
            Self::Api { .. } => 1,
            Self::Gateway { .. } => 2,
        }
    }

    /// The route goes through `source`: that CLI, or that connection.
    pub fn is_from(&self, source: ModelSource<'_>) -> bool {
        match (self, source) {
            (Self::Cli(backend), ModelSource::Cli(other)) => *backend == other,
            (
                Self::Api { connection_id } | Self::Gateway { connection_id },
                ModelSource::Connection { id, .. },
            ) => connection_id == id,
            _ => false,
        }
    }
}

/// A route that serves a model, and the id the model has there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteOffer {
    pub route: ModelRoute,
    /// As the source spells it: what a run on this route has to ask for.
    pub model_id: String,
}

/// A place that lists models: the catalog of a CLI, or an API connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource<'a> {
    Cli(CliBackend),
    Connection { id: &'a str, provider_id: &'a str },
}

impl ModelSource<'_> {
    fn served_by(&self) -> &str {
        match self {
            Self::Cli(backend) => backend.as_str(),
            Self::Connection { provider_id, .. } => provider_id,
        }
    }

    fn route(&self, vendor: ModelVendor) -> ModelRoute {
        match self {
            Self::Cli(backend) => ModelRoute::Cli(*backend),
            Self::Connection { id, provider_id } => {
                let connection_id = (*id).to_owned();
                if ModelVendor::of_server(provider_id) == Some(vendor) {
                    ModelRoute::Api { connection_id }
                } else {
                    ModelRoute::Gateway { connection_id }
                }
            }
        }
    }
}

/// One model and every route that serves it, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedModel {
    pub identity: ModelIdentity,
    pub routes: Vec<RouteOffer>,
}

impl RoutedModel {
    /// The route the host would take: the one through `current` when it serves the model, so
    /// choosing a model never moves the developer off a route that works; otherwise the best
    /// usable one.
    pub fn preferred(
        &self,
        current: Option<ModelSource<'_>>,
        usable: impl Fn(&ModelRoute) -> bool,
    ) -> Option<&RouteOffer> {
        current
            .and_then(|current| {
                self.routes
                    .iter()
                    .find(|offer| offer.route.is_from(current))
            })
            .filter(|offer| usable(&offer.route))
            .or_else(|| self.routes.iter().find(|offer| usable(&offer.route)))
    }
}

/// The models of one vendor, by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorModels {
    pub vendor: ModelVendor,
    pub models: Vec<RoutedModel>,
}

/// Groups what every source lists into vendors and models, each model with its routes. A model
/// two sources list under different spellings (`gpt-5`, `openai/gpt-5`) is one model.
pub fn models_by_vendor<'a>(
    sources: impl IntoIterator<Item = (ModelSource<'a>, &'a [String])>,
) -> Vec<VendorModels> {
    let mut routed: BTreeMap<ModelIdentity, Vec<RouteOffer>> = BTreeMap::new();
    for (source, models) in sources {
        for model_id in models {
            let Some(identity) = ModelIdentity::of(model_id, source.served_by()) else {
                continue;
            };
            let route = source.route(identity.vendor);
            let routes = routed.entry(identity).or_default();
            // A source that lists one model under two spellings offers it once.
            if routes.iter().all(|offer| offer.route != route) {
                routes.push(RouteOffer {
                    route,
                    model_id: model_id.clone(),
                });
            }
        }
    }
    let mut vendors: Vec<VendorModels> = Vec::new();
    // The map is ordered by vendor, then by model name.
    for (identity, mut routes) in routed {
        routes.sort_by_key(|offer| offer.route.rank());
        let model = RoutedModel { identity, routes };
        match vendors.last_mut() {
            Some(group) if group.vendor == model.identity.vendor => group.models.push(model),
            _ => vendors.push(VendorModels {
                vendor: model.identity.vendor,
                models: vec![model],
            }),
        }
    }
    vendors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(vendor: ModelVendor, model: &str) -> Option<ModelIdentity> {
        Some(ModelIdentity {
            vendor,
            model: model.into(),
        })
    }

    #[test]
    fn a_model_is_the_same_model_whatever_lists_it() {
        let gpt = identity(ModelVendor::OpenAi, "gpt-5");
        assert_eq!(ModelIdentity::of("gpt-5", "codex"), gpt);
        assert_eq!(ModelIdentity::of(" GPT-5 ", "openai"), gpt);
        assert_eq!(ModelIdentity::of("openai/gpt-5", "openrouter"), gpt);
        assert_eq!(
            ModelIdentity::of("openrouter/openai/gpt-5", "opencode"),
            gpt
        );
        assert_eq!(
            ModelIdentity::of("models/gemini-2.5-flash", "gemini"),
            identity(ModelVendor::Google, "gemini-2.5-flash")
        );
        // The name wins over the server: a gateway is not the vendor of what it serves.
        assert_eq!(
            ModelIdentity::of("claude-sonnet-4-5", "antigravity"),
            identity(ModelVendor::Anthropic, "claude-sonnet-4-5")
        );
        assert_eq!(
            ModelIdentity::of("qwen2.5-coder:7b", "ollama"),
            identity(ModelVendor::Alibaba, "qwen2.5-coder:7b")
        );
        assert_eq!(
            ModelIdentity::of("x-ai/grok-code-fast", "openrouter"),
            identity(ModelVendor::XAi, "grok-code-fast")
        );
    }

    #[test]
    fn the_server_names_the_vendor_only_when_it_serves_one() {
        // An alias the Claude CLI accepts carries no family in its name.
        assert_eq!(
            ModelIdentity::of("sonnet", "claude"),
            identity(ModelVendor::Anthropic, "sonnet")
        );
        // An aggregator's namespace does, when the name does not.
        assert_eq!(
            ModelIdentity::of("anthropic/sonnet-latest", "openrouter"),
            identity(ModelVendor::Anthropic, "sonnet-latest")
        );
        // Nothing says who makes these: they keep their whole id.
        assert_eq!(
            ModelIdentity::of("my-finetune", "ollama"),
            identity(ModelVendor::Other, "my-finetune")
        );
        assert_eq!(
            ModelIdentity::of("openrouter/free", "openrouter"),
            identity(ModelVendor::Other, "openrouter/free")
        );
        // The CLI's own default names no model.
        assert_eq!(ModelIdentity::of("default", "codex"), None);
        assert_eq!(ModelIdentity::of("  ", "codex"), None);
    }

    #[test]
    fn models_group_by_vendor_with_their_routes_best_first() {
        let own = |models: &[&str]| models.iter().map(ToString::to_string).collect::<Vec<_>>();
        let codex = own(&["gpt-5", "gpt-5-codex", "default"]);
        let claude = own(&["claude-sonnet-4-5", "sonnet"]);
        let key = own(&["gpt-5", "gpt-4.1-mini"]);
        let router = own(&["anthropic/claude-sonnet-4-5", "openai/gpt-5", "my/local"]);
        let connection = |id, provider_id| ModelSource::Connection { id, provider_id };
        // The gateway is listed first: the order of the sources does not order the routes.
        let vendors = models_by_vendor([
            (connection("router", "openrouter"), router.as_slice()),
            (connection("key", "openai"), key.as_slice()),
            (ModelSource::Cli(CliBackend::Claude), claude.as_slice()),
            (ModelSource::Cli(CliBackend::Codex), codex.as_slice()),
        ]);
        let route = |route: &ModelRoute| match route {
            ModelRoute::Cli(backend) => format!("cli {}", backend.as_str()),
            ModelRoute::Api { connection_id } => format!("api {connection_id}"),
            ModelRoute::Gateway { connection_id } => format!("gateway {connection_id}"),
        };
        let listed: Vec<String> = vendors
            .iter()
            .flat_map(|group| {
                group.models.iter().map(move |model| {
                    format!(
                        "{} {}: {}",
                        group.vendor.label(),
                        model.identity.model,
                        model
                            .routes
                            .iter()
                            .map(|offer| route(&offer.route))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
            })
            .collect();
        // Each route keeps the id the model has there.
        let gpt = &vendors[0].models[1];
        assert_eq!(
            gpt.routes
                .iter()
                .map(|offer| offer.model_id.as_str())
                .collect::<Vec<_>>(),
            ["gpt-5", "gpt-5", "openai/gpt-5"]
        );
        assert_eq!(
            listed,
            [
                "OpenAI gpt-4.1-mini: api key",
                "OpenAI gpt-5: cli codex, api key, gateway router",
                "OpenAI gpt-5-codex: cli codex",
                "Anthropic claude-sonnet-4-5: cli claude, gateway router",
                "Anthropic sonnet: cli claude",
                "Other my/local: gateway router",
            ]
        );
    }

    #[test]
    fn the_preferred_route_keeps_the_current_one_and_skips_what_cannot_run() {
        let codex = ModelRoute::Cli(CliBackend::Codex);
        let key = ModelRoute::Api {
            connection_id: "key".into(),
        };
        let model = RoutedModel {
            identity: identity(ModelVendor::OpenAi, "gpt-5").unwrap(),
            routes: vec![
                RouteOffer {
                    route: codex.clone(),
                    model_id: "gpt-5".into(),
                },
                RouteOffer {
                    route: key.clone(),
                    model_id: "gpt-5".into(),
                },
            ],
        };
        let on_codex = ModelSource::Cli(CliBackend::Codex);
        let on_key = ModelSource::Connection {
            id: "key",
            provider_id: "openai",
        };
        let chosen = |current, usable: &dyn Fn(&ModelRoute) -> bool| {
            model
                .preferred(current, usable)
                .map(|offer| offer.route.clone())
        };
        // Nothing chosen yet: the subscription comes before the key.
        assert_eq!(chosen(None, &|_| true), Some(codex.clone()));
        // The developer is on the key: choosing the model does not move the route.
        assert_eq!(chosen(Some(on_key), &|_| true), Some(key.clone()));
        // A route that does not serve this model is no reason to keep it.
        let elsewhere = ModelSource::Cli(CliBackend::Claude);
        assert_eq!(chosen(Some(elsewhere), &|_| true), Some(codex.clone()));
        // A CLI that is signed out is skipped, and so is a current route that stopped working.
        let signed_in = |route: &ModelRoute| *route != codex;
        assert_eq!(chosen(None, &signed_in), Some(key.clone()));
        assert_eq!(chosen(Some(on_codex), &signed_in), Some(key));
        assert_eq!(chosen(None, &|_| false), None);
    }
}
