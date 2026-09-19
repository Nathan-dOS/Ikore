use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gpui::SharedString;
use hvc_core::AppCore;
use hvc_core::agent_runtime::{AgentRuntimeConfig, ProviderKeyStatus};
use hvc_core::llm::{
    DeleteProviderConnectionRequest, DeleteProviderRouteRequest, LlmResilienceSnapshot,
    ProviderConnection, ProviderProfile, ProviderRoute, ProviderRouteBinding,
    ResetLlmResilienceStateRequest, RouteTarget, TestProviderConnectionRequest,
    TestProviderConnectionResponse, UpsertProviderConnectionRequest,
    UpsertProviderRouteBindingRequest, UpsertProviderRouteRequest,
};
use hvc_core::persistence_service::{
    EmbeddingReadinessReport, PersistenceCommandError, ReindexWorkspaceEmbeddingsRequest,
    ReindexWorkspaceEmbeddingsResponse, UpdateAgentRuntimeConfigRequest,
};

use crate::errors::UiCommandError;

pub(crate) type SettingsFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, UiCommandError>> + Send + 'static>>;

/// Per-project composition settings (ADR-022) of the last active project:
/// the opt-in itself, the validation profiles with their enabled flag, the
/// presets the host detects from the project's manifests, and the specialty
/// overrides. Absent when no project is active.
#[derive(Debug, Clone)]
pub(crate) struct CompositionProjectSettings {
    pub(crate) project_id: String,
    pub(crate) project_name: String,
    pub(crate) settings: hvc_core::orchestration::composition::settings::CompositionSettings,
    pub(crate) profiles: Vec<(
        hvc_core::orchestration::composition::ValidationCommandProfileV1,
        bool,
    )>,
    pub(crate) presets: Vec<hvc_core::orchestration::composition::ValidationCommandProfileV1>,
    pub(crate) overrides: Vec<(
        hvc_core::orchestration::composition::ExecutorKind,
        hvc_core::terminals::SpecialtyOverride,
    )>,
}

#[derive(Debug, Clone)]
pub(crate) struct SettingsSnapshot {
    pub(crate) composition: Option<CompositionProjectSettings>,
    pub(crate) preparation: hvc_core::orchestration::PreparationSettings,
    pub(crate) config: AgentRuntimeConfig,
    pub(crate) profiles: Vec<ProviderProfile>,
    pub(crate) connections: Vec<ProviderConnection>,
    pub(crate) routes: Vec<ProviderRoute>,
    pub(crate) bindings: Vec<ProviderRouteBinding>,
    pub(crate) key_statuses: Vec<ProviderKeyStatus>,
    pub(crate) resilience: LlmResilienceSnapshot,
}

pub(crate) trait SettingsBackend: Send + Sync {
    fn update_preparation(
        &self,
        settings: hvc_core::orchestration::PreparationSettings,
    ) -> SettingsFuture<()>;
    fn update_composition(
        &self,
        project_id: String,
        settings: hvc_core::orchestration::composition::settings::CompositionSettings,
    ) -> SettingsFuture<()>;
    fn upsert_validation_profile(
        &self,
        project_id: String,
        profile: hvc_core::orchestration::composition::ValidationCommandProfileV1,
        enabled: bool,
    ) -> SettingsFuture<()>;
    fn delete_validation_profile(
        &self,
        project_id: String,
        check_ref: String,
    ) -> SettingsFuture<()>;
    fn upsert_specialty_override(
        &self,
        project_id: String,
        executor_kind: hvc_core::orchestration::composition::ExecutorKind,
        setting: hvc_core::terminals::SpecialtyOverride,
    ) -> SettingsFuture<()>;
    fn delete_specialty_override(
        &self,
        project_id: String,
        executor_kind: hvc_core::orchestration::composition::ExecutorKind,
    ) -> SettingsFuture<()>;
    fn snapshot(&self) -> SettingsFuture<SettingsSnapshot>;
    fn update_config(
        &self,
        request: UpdateAgentRuntimeConfigRequest,
    ) -> SettingsFuture<AgentRuntimeConfig>;
    fn upsert_connection(
        &self,
        request: UpsertProviderConnectionRequest,
    ) -> SettingsFuture<ProviderConnection>;
    fn delete_connection(&self, request: DeleteProviderConnectionRequest) -> SettingsFuture<()>;
    fn test_connection(
        &self,
        request: TestProviderConnectionRequest,
    ) -> SettingsFuture<TestProviderConnectionResponse>;
    fn upsert_route(&self, request: UpsertProviderRouteRequest) -> SettingsFuture<ProviderRoute>;
    fn delete_route(&self, request: DeleteProviderRouteRequest) -> SettingsFuture<()>;
    fn upsert_binding(
        &self,
        request: UpsertProviderRouteBindingRequest,
    ) -> SettingsFuture<ProviderRouteBinding>;
    fn reset_resilience(&self, request: ResetLlmResilienceStateRequest) -> SettingsFuture<usize>;
    fn reindex(
        &self,
        workspace_path: String,
        model: Option<String>,
    ) -> SettingsFuture<ReindexWorkspaceEmbeddingsResponse>;
    fn check_embedding_readiness(&self) -> SettingsFuture<EmbeddingReadinessReport>;
    fn pull_embedding_model(&self, model: Option<String>) -> SettingsFuture<()>;
}

pub(crate) struct CoreSettingsBackend {
    core: Arc<AppCore>,
}

impl CoreSettingsBackend {
    pub(crate) fn new(core: Arc<AppCore>) -> Self {
        Self { core }
    }
}

fn command_error(error: hvc_core::persistence_service::PersistenceServiceError) -> UiCommandError {
    UiCommandError::from(PersistenceCommandError::from(error))
}

impl CoreSettingsBackend {
    fn blocking<T: Send + 'static>(
        &self,
        operation: impl FnOnce(
            &hvc_core::persistence_service::PersistenceService,
        )
            -> Result<T, hvc_core::persistence_service::PersistenceServiceError>
        + Send
        + 'static,
    ) -> SettingsFuture<T> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || operation(core.persistence()))
                .await
                .map_err(|e| UiCommandError::worker(e.to_string()))?
                .map_err(command_error)
        })
    }
}

impl SettingsBackend for CoreSettingsBackend {
    fn update_preparation(
        &self,
        settings: hvc_core::orchestration::PreparationSettings,
    ) -> SettingsFuture<()> {
        let core = self.core.clone();
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().update_preparation_settings(settings))
                .await
                .map_err(|e| UiCommandError::worker(e.to_string()))?
                .map(|_| ())
                .map_err(command_error)
        })
    }
    fn update_composition(
        &self,
        project_id: String,
        settings: hvc_core::orchestration::composition::settings::CompositionSettings,
    ) -> SettingsFuture<()> {
        self.blocking(move |persistence| {
            persistence
                .update_composition_settings(&project_id, settings)
                .map(|_| ())
        })
    }
    fn upsert_validation_profile(
        &self,
        project_id: String,
        profile: hvc_core::orchestration::composition::ValidationCommandProfileV1,
        enabled: bool,
    ) -> SettingsFuture<()> {
        self.blocking(move |persistence| {
            persistence
                .upsert_validation_profile(&project_id, profile, enabled)
                .map(|_| ())
        })
    }
    fn delete_validation_profile(
        &self,
        project_id: String,
        check_ref: String,
    ) -> SettingsFuture<()> {
        self.blocking(move |persistence| {
            persistence.delete_validation_profile(&project_id, &check_ref)
        })
    }
    fn upsert_specialty_override(
        &self,
        project_id: String,
        executor_kind: hvc_core::orchestration::composition::ExecutorKind,
        setting: hvc_core::terminals::SpecialtyOverride,
    ) -> SettingsFuture<()> {
        self.blocking(move |persistence| {
            persistence.upsert_specialty_override(&project_id, executor_kind, setting)
        })
    }
    fn delete_specialty_override(
        &self,
        project_id: String,
        executor_kind: hvc_core::orchestration::composition::ExecutorKind,
    ) -> SettingsFuture<()> {
        self.blocking(move |persistence| {
            persistence.delete_specialty_override(&project_id, executor_kind)
        })
    }
    fn snapshot(&self) -> SettingsFuture<SettingsSnapshot> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    let persistence = core.persistence();
                    let composition = match persistence.get_last_active_project()?.project {
                        Some(project) => Some(CompositionProjectSettings {
                            settings: persistence.get_composition_settings(&project.id)?,
                            profiles: persistence.list_validation_profiles(&project.id)?,
                            presets: persistence.detect_validation_presets(&project.id)?,
                            overrides: persistence.list_specialty_overrides(&project.id)?,
                            project_id: project.id,
                            project_name: project.name,
                        }),
                        None => None,
                    };
                    Ok(SettingsSnapshot {
                        composition,
                        preparation: persistence.get_preparation_settings()?,
                        config: persistence.get_agent_runtime_config()?.config,
                        profiles: persistence.list_provider_profiles().profiles,
                        connections: persistence.list_provider_connections()?.connections,
                        routes: persistence.list_provider_routes()?.routes,
                        bindings: persistence.list_provider_route_bindings()?.bindings,
                        key_statuses: persistence.get_provider_key_statuses()?.providers,
                        resilience: persistence.get_llm_resilience_snapshot()?.snapshot,
                    })
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("worker de Settings interrompido: {error}"))
                })?
                .map_err(command_error)
        })
    }

    fn update_config(
        &self,
        request: UpdateAgentRuntimeConfigRequest,
    ) -> SettingsFuture<AgentRuntimeConfig> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().update_agent_runtime_config(request))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!(
                        "settings configuration worker aborted: {error}"
                    ))
                })?
                .map(|response| response.config)
                .map_err(command_error)
        })
    }

    fn upsert_connection(
        &self,
        request: UpsertProviderConnectionRequest,
    ) -> SettingsFuture<ProviderConnection> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().upsert_provider_connection(request))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("connection worker aborted: {error}"))
                })?
                .map(|response| response.connection)
                .map_err(command_error)
        })
    }

    fn delete_connection(&self, request: DeleteProviderConnectionRequest) -> SettingsFuture<()> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().delete_provider_connection(request))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("deletion worker aborted: {error}"))
                })?
                .and_then(|response| {
                    response.deleted.then_some(()).ok_or_else(|| {
                        hvc_core::persistence_service::PersistenceServiceError::NotFound(
                            "Connection not found.".to_string(),
                        )
                    })
                })
                .map_err(command_error)
        })
    }

    fn test_connection(
        &self,
        request: TestProviderConnectionRequest,
    ) -> SettingsFuture<TestProviderConnectionResponse> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move { core.persistence().test_provider_connection(request).await })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("connection test task aborted: {error}"))
                })?
                .map_err(command_error)
        })
    }

    fn upsert_route(&self, request: UpsertProviderRouteRequest) -> SettingsFuture<ProviderRoute> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().upsert_provider_route(request))
                .await
                .map_err(|error| UiCommandError::worker(format!("route worker aborted: {error}")))?
                .map(|response| response.route)
                .map_err(command_error)
        })
    }

    fn delete_route(&self, request: DeleteProviderRouteRequest) -> SettingsFuture<()> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().delete_provider_route(request))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("deletion worker aborted: {error}"))
                })?
                .and_then(|response| {
                    response.deleted.then_some(()).ok_or_else(|| {
                        hvc_core::persistence_service::PersistenceServiceError::NotFound(
                            "Route not found.".to_string(),
                        )
                    })
                })
                .map_err(command_error)
        })
    }

    fn upsert_binding(
        &self,
        request: UpsertProviderRouteBindingRequest,
    ) -> SettingsFuture<ProviderRouteBinding> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().upsert_provider_route_binding(request))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("binding worker aborted: {error}"))
                })?
                .map(|response| response.binding)
                .map_err(command_error)
        })
    }

    fn reset_resilience(&self, request: ResetLlmResilienceStateRequest) -> SettingsFuture<usize> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.persistence().reset_llm_resilience_state(request))
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("resilience worker aborted: {error}"))
                })?
                .map(|response| response.reset_count)
                .map_err(command_error)
        })
    }

    fn reindex(
        &self,
        workspace_path: String,
        model: Option<String>,
    ) -> SettingsFuture<ReindexWorkspaceEmbeddingsResponse> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move {
                    core.persistence()
                        .reindex_workspace_embeddings_for_workspace(
                            &workspace_path,
                            ReindexWorkspaceEmbeddingsRequest { model },
                        )
                        .await
                })
                .await
                .map_err(|error| UiCommandError::worker(format!("reindex task aborted: {error}")))?
                .map_err(command_error)
        })
    }

    fn check_embedding_readiness(&self) -> SettingsFuture<EmbeddingReadinessReport> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move { core.persistence().check_embedding_readiness().await })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("readiness check task aborted: {error}"))
                })?
                .map_err(command_error)
        })
    }

    fn pull_embedding_model(&self, model: Option<String>) -> SettingsFuture<()> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move { core.persistence().pull_embedding_model(model).await })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("model pull task aborted: {error}"))
                })?
                .map_err(command_error)
        })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsSection {
    #[default]
    Connections,
    Routes,
    Runtime,
    Usage,
}

#[derive(Debug)]
pub(crate) struct SettingsModel {
    pub(crate) usage: crate::usage::UsageState,
    pub(crate) settings_section: SettingsSection,
    pub(crate) creating_connection: bool,
    pub(crate) creating_route: bool,
    pub(crate) snapshot: Option<SettingsSnapshot>,
    pub(crate) loading: bool,
    pub(crate) busy: bool,
    pub(crate) selected_connection_id: Option<SharedString>,
    pub(crate) selected_route_id: Option<SharedString>,
    pub(crate) notice: Option<SharedString>,
    pub(crate) error: Option<UiCommandError>,
    pub(crate) generation: u64,
    pub(crate) connection_provider_id: SharedString,
    pub(crate) route_strategy: SharedString,
    pub(crate) binding_agent_role: SharedString,
    pub(crate) binding_stage: SharedString,
    pub(crate) route_max_attempts: u32,
    pub(crate) route_attempt_timeout_ms: u64,
    pub(crate) route_timeout_ms: u64,
    pub(crate) route_targets: Vec<RouteTarget>,
    pub(crate) embedding_readiness: Option<EmbeddingReadinessReport>,
    pub(crate) embedding_readiness_loading: bool,
    /// Specialty-override form (composition settings card).
    pub(crate) composition_override_kind: SharedString,
    pub(crate) composition_override_mode: SharedString,
    pub(crate) composition_override_backend: SharedString,
    pub(crate) composition_override_thinking: SharedString,
}

impl Default for SettingsModel {
    fn default() -> Self {
        Self {
            usage: Default::default(),
            settings_section: SettingsSection::Connections,
            creating_connection: false,
            creating_route: false,
            snapshot: None,
            loading: false,
            busy: false,
            selected_connection_id: None,
            selected_route_id: None,
            notice: None,
            error: None,
            generation: 0,
            connection_provider_id: "openai".into(),
            route_strategy: "priority".into(),
            binding_agent_role: "task_specifier".into(),
            binding_stage: "Specification".into(),
            route_max_attempts: 3,
            route_attempt_timeout_ms: 45_000,
            route_timeout_ms: 120_000,
            route_targets: Vec::new(),
            embedding_readiness: None,
            embedding_readiness_loading: false,
            composition_override_kind: "code_implementer".into(),
            composition_override_mode: "api_direct".into(),
            composition_override_backend: "codex".into(),
            composition_override_thinking: "default".into(),
        }
    }
}

impl SettingsModel {
    pub(crate) fn begin(&mut self, loading: bool) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.loading = loading;
        self.busy = !loading;
        self.error = None;
        self.generation
    }

    /// Background snapshot read for the shell chrome; see `AgentsModel::begin_quiet`.
    pub(crate) fn begin_quiet(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.generation
    }

    pub(crate) fn in_flight(&self) -> bool {
        self.loading || self.busy
    }

    pub(crate) fn apply_snapshot(&mut self, generation: u64, snapshot: SettingsSnapshot) -> bool {
        if generation != self.generation {
            return false;
        }
        self.loading = false;
        self.busy = false;
        if self.selected_connection_id.as_ref().is_none_or(|selected| {
            !snapshot
                .connections
                .iter()
                .any(|item| item.id == selected.as_ref())
        }) {
            self.selected_connection_id = snapshot
                .connections
                .first()
                .map(|item| item.id.clone().into());
        }
        if self.selected_route_id.as_ref().is_none_or(|selected| {
            !snapshot
                .routes
                .iter()
                .any(|item| item.id == selected.as_ref())
        }) {
            self.selected_route_id = snapshot.routes.first().map(|item| item.id.clone().into());
        }
        self.creating_connection = false;
        self.creating_route = false;
        self.snapshot = Some(snapshot);
        true
    }

    pub(crate) fn fail(&mut self, generation: u64, error: UiCommandError) -> bool {
        if generation != self.generation {
            return false;
        }
        self.loading = false;
        self.busy = false;
        self.error = Some(error);
        true
    }

    pub(crate) fn connection(&self) -> Option<&ProviderConnection> {
        let id = self.selected_connection_id.as_ref()?;
        self.snapshot
            .as_ref()?
            .connections
            .iter()
            .find(|item| item.id == id.as_ref())
    }

    pub(crate) fn route(&self) -> Option<&ProviderRoute> {
        let id = self.selected_route_id.as_ref()?;
        self.snapshot
            .as_ref()?
            .routes
            .iter()
            .find(|item| item.id == id.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{AppContext as _, TestAppContext};
    use hvc_core::agent_runtime::AgentRuntimeConfig;
    use hvc_core::llm::{
        LlmResilienceSnapshot, ProviderConnection, ProviderRoute, ProviderRouteBinding,
        RoutingMode, TestProviderConnectionRequest, UpsertProviderConnectionRequest,
    };
    use hvc_core::persistence_service::UpdateAgentRuntimeConfigRequest;
    use hvc_test_support::{CoreTestHarness, ScriptedHttpServer, ScriptedResponse};

    use super::{CoreSettingsBackend, SettingsBackend, SettingsModel, SettingsSnapshot};
    use crate::AppModel;

    #[test]
    fn renders_connections_routes_and_bindings() {
        let mut model = SettingsModel::default();
        let generation = model.begin(true);
        model.apply_snapshot(
            generation,
            SettingsSnapshot {
                composition: None,
                preparation: Default::default(),
                config: AgentRuntimeConfig::default(),
                profiles: Vec::new(),
                connections: vec![ProviderConnection {
                    id: "connection-1".into(),
                    provider_id: "openai".into(),
                    display_name: "Principal".into(),
                    endpoint_url: None,
                    default_model: "gpt-4.1-mini".into(),
                    enabled: true,
                    state: "closed".into(),
                    blocked_reason: None,
                    cooldown_until: None,
                    max_concurrency: 2,
                    version: 1,
                    has_api_key: true,
                    chat_eligible: true,
                    created_at: "now".into(),
                    updated_at: "now".into(),
                }],
                routes: vec![ProviderRoute {
                    id: "route-1".into(),
                    name: "Default".into(),
                    strategy: "fallback".into(),
                    max_attempts: 2,
                    attempt_timeout_ms: 1_000,
                    route_timeout_ms: 2_000,
                    enabled: true,
                    version: 1,
                    targets: Vec::new(),
                    created_at: "now".into(),
                    updated_at: "now".into(),
                }],
                bindings: vec![ProviderRouteBinding {
                    project_id: "project-1".into(),
                    agent_role: "task_specifier".into(),
                    stage: "Specification".into(),
                    route_id: "route-1".into(),
                    enabled: true,
                    version: 1,
                    created_at: "now".into(),
                    updated_at: "now".into(),
                }],
                key_statuses: Vec::new(),
                resilience: LlmResilienceSnapshot {
                    generated_at: "now".into(),
                    states: Vec::new(),
                },
            },
        );

        let snapshot = model.snapshot.as_ref().unwrap();
        assert_eq!(snapshot.connections.len(), 1);
        assert_eq!(snapshot.routes.len(), 1);
        assert_eq!(snapshot.bindings.len(), 1);
    }

    #[test]
    fn openrouter_connection_uses_keyring_metadata_and_fixed_endpoint() {
        let harness = CoreTestHarness::new();
        let core = harness.core();
        let backend = CoreSettingsBackend::new(Arc::clone(&core));
        let connection = core
            .runtime_handle()
            .block_on(backend.upsert_connection(UpsertProviderConnectionRequest {
                id: Some("openrouter-connection".into()),
                provider_id: "openrouter".into(),
                display_name: "OpenRouter fixture".into(),
                endpoint_url: Some("https://attacker.invalid/custom".into()),
                default_model: "openrouter/free".into(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: Some("sk-or-v1-secret".into()),
                version: None,
            }))
            .expect("OpenRouter connection should save");

        assert_eq!(connection.provider_id, "openrouter");
        assert_eq!(connection.endpoint_url, None);
        assert_eq!(connection.default_model, "openrouter/free");
        assert!(connection.has_api_key);
        assert!(connection.chat_eligible);
        assert!(
            harness
                .credentials()
                .contains("connection:openrouter-connection")
        );
        let serialized = serde_json::to_string(&connection).unwrap();
        assert!(!serialized.contains("sk-or-v1-secret"));
        assert!(!serialized.contains("credential_ref"));
        let snapshot = core.runtime_handle().block_on(backend.snapshot()).unwrap();
        assert!(
            snapshot
                .connections
                .iter()
                .any(|item| item.id == connection.id && item.has_api_key)
        );
        assert!(
            !serde_json::to_string(&snapshot.connections)
                .unwrap()
                .contains("sk-or-v1-secret")
        );
        for entry in std::fs::read_dir(harness.data_dir()).unwrap() {
            let path = entry.unwrap().path();
            if path.is_file()
                && path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("hard-vibe-code.db")
            {
                let bytes = std::fs::read(path).unwrap();
                assert!(
                    !bytes
                        .windows(b"sk-or-v1-secret".len())
                        .any(|window| window == b"sk-or-v1-secret")
                );
            }
        }
    }

    #[gpui::test]
    fn openrouter_selector_populates_default_and_saves_a_secret_free_connection(
        cx: &mut TestAppContext,
    ) {
        let harness = CoreTestHarness::new();
        let core = harness.core();
        let backend = Arc::new(CoreSettingsBackend::new(Arc::clone(&core)));
        let snapshot = core.runtime_handle().block_on(backend.snapshot()).unwrap();
        let profile = snapshot
            .profiles
            .iter()
            .find(|profile| profile.id == "openrouter")
            .unwrap();
        assert_eq!(profile.default_model, "openrouter/free");
        assert!(!profile.endpoint_configurable);
        let app = cx.new(|cx| AppModel::new_for_test_with_settings_backend(backend.clone(), cx));
        app.update(cx, |model, cx| {
            model
                .settings
                .update(cx, |settings, _| settings.snapshot = Some(snapshot));
            model.new_settings_connection(cx);
            assert_eq!(
                model.settings_connection_model_input.read(cx).value(),
                "gpt-4.1-mini"
            );
            assert!(
                model
                    .choice_options("settings-provider", cx)
                    .iter()
                    .any(|choice| choice.value == "openrouter")
            );
            model.select_settings_provider("openrouter".into(), cx);
            assert_eq!(
                model.settings_connection_model_input.read(cx).value(),
                "openrouter/free"
            );
            model
                .settings_connection_model_input
                .update(cx, |input, cx| {
                    input.set_value("nex-agi/nex-n2.5-mini:free", cx)
                });
            model.select_settings_provider("openrouter".into(), cx);
            assert_eq!(
                model.settings_connection_model_input.read(cx).value(),
                "nex-agi/nex-n2.5-mini:free"
            );
            model.new_settings_connection(cx);
            assert_eq!(
                model.settings_connection_model_input.read(cx).value(),
                "openrouter/free"
            );
            model
                .settings_connection_name_input
                .update(cx, |input, cx| input.set_value("OpenRouter UI fixture", cx));
            model
                .settings_connection_endpoint_input
                .update(cx, |input, cx| {
                    input.set_value("https://attacker.invalid", cx)
                });
            model
                .settings_connection_secret_input
                .update(cx, |input, cx| input.set_value("sk-or-v1-ui-fixture", cx));
            model.save_settings_connection(cx);
            assert!(
                model
                    .settings_connection_secret_input
                    .read(cx)
                    .value()
                    .is_empty()
            );
        });
        for _ in 0..500 {
            cx.run_until_parked();
            if app.read_with(cx, |model, cx| {
                !model.settings.read(cx).busy && !model.settings.read(cx).loading
            }) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let snapshot = core.runtime_handle().block_on(backend.snapshot()).unwrap();
        let saved = snapshot
            .connections
            .iter()
            .find(|item| item.display_name == "OpenRouter UI fixture")
            .expect("connection should save from the UI");
        assert_eq!(saved.provider_id, "openrouter");
        assert_eq!(saved.default_model, "openrouter/free");
        assert_eq!(saved.endpoint_url, None);
        assert!(saved.has_api_key && saved.chat_eligible);
        assert!(
            !serde_json::to_string(saved)
                .unwrap()
                .contains("sk-or-v1-ui-fixture")
        );
    }

    #[test]
    fn openrouter_rejects_uncatalogued_model_before_writing_credentials() {
        let harness = CoreTestHarness::new();
        let core = harness.core();
        let backend = CoreSettingsBackend::new(Arc::clone(&core));
        let before = harness.credentials().set_call_count();
        let error = core
            .runtime_handle()
            .block_on(backend.upsert_connection(UpsertProviderConnectionRequest {
                id: Some("invalid-openrouter-model".into()),
                provider_id: "openrouter".into(),
                display_name: "Invalid model".into(),
                endpoint_url: None,
                default_model: "unknown/model".into(),
                enabled: Some(true),
                max_concurrency: Some(1),
                api_key: Some("sk-or-v1-must-not-save".into()),
                version: None,
            }))
            .unwrap_err();
        assert!(error.presentable().contains("curated catalog"));
        assert_eq!(harness.credentials().set_call_count(), before);
        assert!(
            !harness
                .credentials()
                .contains("connection:invalid-openrouter-model")
        );
    }

    #[test]
    fn updates_routing_mode_through_core() {
        let harness = CoreTestHarness::new();
        let core = harness.core();
        let backend = CoreSettingsBackend::new(Arc::clone(&core));
        let response = core.runtime_handle().block_on(backend.update_config(
            UpdateAgentRuntimeConfigRequest {
                provider: None,
                model: None,
                temperature: None,
                max_output_tokens: None,
                timeout_ms: None,
                max_retries: None,
                context_chars_limit: None,
                max_context_files: None,
                context_strategy: None,
                embedding_enabled: None,
                embedding_provider: None,
                embedding_model: None,
                embedding_top_k: None,
                routing_v2_mode: Some(RoutingMode::Shadow),
                adaptive_context_mode: None,
                dynamic_context_policy: None,
            },
        ));

        assert_eq!(response.unwrap().routing_v2_mode, RoutingMode::Shadow);
        let snapshot = core.runtime_handle().block_on(backend.snapshot()).unwrap();
        assert_eq!(snapshot.config.routing_v2_mode, RoutingMode::Shadow);
    }

    #[test]
    fn readiness_check_reflects_the_fixed_gateways_installed_models() {
        let harness = CoreTestHarness::new();
        let core = harness.core();
        let backend = CoreSettingsBackend::new(Arc::clone(&core));

        let report = core
            .runtime_handle()
            .block_on(backend.check_embedding_readiness())
            .unwrap();
        assert!(report.ollama_reachable);
        assert!(!report.model_installed);

        harness
            .embeddings()
            .set_installed_models(vec![report.model.clone()]);
        let report = core
            .runtime_handle()
            .block_on(backend.check_embedding_readiness())
            .unwrap();
        assert!(report.model_installed);
        assert!(report.embedding_call_ok);
    }

    #[test]
    fn pull_embedding_model_forwards_the_gateways_failure() {
        let harness = CoreTestHarness::new();
        let core = harness.core();
        harness.embeddings().fail_pull_with("model not found");
        let backend = CoreSettingsBackend::new(Arc::clone(&core));

        let error = core
            .runtime_handle()
            .block_on(backend.pull_embedding_model(Some("nomic-embed-text".into())))
            .unwrap_err();
        assert!(error.presentable().contains("model not found"));
    }

    #[test]
    fn tests_selected_provider_connection() {
        let server = ScriptedHttpServer::start([ScriptedResponse::json(
            200,
            r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
        )]);
        let harness = CoreTestHarness::new();
        let core = harness.core();
        let backend = CoreSettingsBackend::new(Arc::clone(&core));
        let connection = core
            .runtime_handle()
            .block_on(backend.upsert_connection(connection_request(
                "connection-test",
                Some(server.url("")),
                None,
            )))
            .unwrap();

        let response = core
            .runtime_handle()
            .block_on(backend.test_connection(TestProviderConnectionRequest {
                connection_id: connection.id,
            }))
            .unwrap();

        assert!(response.reachable);
        assert_eq!(response.provider_id, "ollama");
        assert_eq!(
            server
                .wait_for_requests(1, std::time::Duration::from_secs(1))
                .len(),
            1
        );
    }

    #[gpui::test]
    fn automatic_selection_hydrates_forms_and_refresh_preserves_same_selection_drafts(
        cx: &mut TestAppContext,
    ) {
        let harness = CoreTestHarness::new();
        let backend = Arc::new(CoreSettingsBackend::new(harness.core()));
        let app = cx.new(|cx| AppModel::new_for_test_with_settings_backend(backend, cx));
        app.update(cx, |model, cx| model.refresh_settings(cx));
        for _ in 0..500 {
            cx.run_until_parked();
            if app.read_with(cx, |model, cx| !model.settings.read(cx).loading) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        app.read_with(cx, |model, cx| {
            let settings = model.settings.read(cx);
            let connection = settings
                .connection()
                .expect("automatically selected connection");
            assert_eq!(
                model.settings_connection_name_input.read(cx).value(),
                connection.display_name
            );
            assert_eq!(
                model.settings_connection_model_input.read(cx).value(),
                connection.default_model
            );
            assert_eq!(
                settings.connection_provider_id.as_ref(),
                connection.provider_id
            );
            let route = settings.route().expect("automatically selected route");
            assert_eq!(model.settings_route_name_input.read(cx).value(), route.name);
            assert_eq!(settings.route_targets.len(), route.targets.len());
        });
        app.update(cx, |model, cx| {
            model
                .settings_connection_name_input
                .update(cx, |input, cx| input.set_value("Unsaved local draft", cx));
            model.refresh_settings(cx);
        });
        for _ in 0..500 {
            cx.run_until_parked();
            if app.read_with(cx, |model, cx| !model.settings.read(cx).loading) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        app.read_with(cx, |model, cx| {
            assert_eq!(
                model.settings_connection_name_input.read(cx).value(),
                "Unsaved local draft"
            )
        });
    }

    #[gpui::test]
    fn writes_secret_once_and_clears_input(cx: &mut TestAppContext) {
        let harness = CoreTestHarness::new();
        let core = harness.core();
        let backend = Arc::new(CoreSettingsBackend::new(Arc::clone(&core)));
        let connection = core
            .runtime_handle()
            .block_on(backend.upsert_connection(connection_request(
                "secret-target",
                Some("http://127.0.0.1:11434".into()),
                None,
            )))
            .unwrap();
        let snapshot = core.runtime_handle().block_on(backend.snapshot()).unwrap();
        let before = harness.credentials().set_call_count();
        let app = cx.new(|cx| AppModel::new_for_test_with_settings_backend(backend.clone(), cx));
        app.update(cx, |model, cx| {
            model.settings.update(cx, |settings, _| {
                settings.snapshot = Some(snapshot);
                settings.selected_connection_id = Some(connection.id.clone().into());
                settings.connection_provider_id = "ollama".into();
            });
            model
                .settings_connection_name_input
                .update(cx, |input, cx| input.set_value("Local", cx));
            model
                .settings_connection_model_input
                .update(cx, |input, cx| input.set_value("fixture-model", cx));
            model
                .settings_connection_endpoint_input
                .update(cx, |input, cx| {
                    input.set_value("http://127.0.0.1:11434", cx)
                });
            model
                .settings_connection_secret_input
                .update(cx, |input, cx| input.set_value("super-secret", cx));
            model.save_settings_connection(cx);
            assert!(
                model
                    .settings_connection_secret_input
                    .read(cx)
                    .value()
                    .is_empty()
            );
        });
        for _ in 0..100 {
            cx.run_until_parked();
            if harness.credentials().set_call_count() > before {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        assert_eq!(harness.credentials().set_call_count(), before + 1);
        assert!(harness.credentials().contains("connection:secret-target"));
        cx.read(|cx| {
            assert!(
                app.read(cx)
                    .settings_connection_secret_input
                    .read(cx)
                    .value()
                    .is_empty()
            )
        });
    }

    #[gpui::test]
    fn clears_secret_input_after_persistence_failure(cx: &mut TestAppContext) {
        let harness = CoreTestHarness::new();
        let core = harness.core();
        let backend = Arc::new(CoreSettingsBackend::new(Arc::clone(&core)));
        let snapshot = core.runtime_handle().block_on(backend.snapshot()).unwrap();
        harness.credentials().fail_with("keyring unavailable");
        let app = cx.new(|cx| AppModel::new_for_test_with_settings_backend(backend.clone(), cx));
        app.update(cx, |model, cx| {
            model.settings.update(cx, |settings, _| {
                settings.snapshot = Some(snapshot);
                settings.connection_provider_id = "ollama".into();
            });
            model
                .settings_connection_name_input
                .update(cx, |input, cx| input.set_value("Local", cx));
            model
                .settings_connection_model_input
                .update(cx, |input, cx| input.set_value("fixture-model", cx));
            model
                .settings_connection_endpoint_input
                .update(cx, |input, cx| {
                    input.set_value("http://127.0.0.1:11434", cx)
                });
            model
                .settings_connection_secret_input
                .update(cx, |input, cx| input.set_value("must-disappear", cx));
            model.save_settings_connection(cx);
            assert!(
                model
                    .settings_connection_secret_input
                    .read(cx)
                    .value()
                    .is_empty()
            );
        });
        for _ in 0..100 {
            cx.run_until_parked();
            if cx.read(|cx| app.read(cx).settings.read(cx).error.is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        cx.read(|cx| {
            let model = app.read(cx);
            assert!(
                model
                    .settings_connection_secret_input
                    .read(cx)
                    .value()
                    .is_empty()
            );
            assert!(model.settings.read(cx).error.is_some());
        });
    }

    fn connection_request(
        id: &str,
        endpoint_url: Option<String>,
        api_key: Option<String>,
    ) -> UpsertProviderConnectionRequest {
        UpsertProviderConnectionRequest {
            id: Some(id.to_string()),
            provider_id: "ollama".to_string(),
            display_name: "Ollama fixture".to_string(),
            endpoint_url,
            default_model: "fixture-model".to_string(),
            enabled: Some(true),
            max_concurrency: Some(1),
            api_key,
            version: None,
        }
    }
}
