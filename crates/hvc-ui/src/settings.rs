use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use gpui::SharedString;
use hvc_core::AppCore;
use hvc_core::agent_runtime::{AgentRuntimeConfig, ProviderKeyStatus};
use hvc_core::llm::{
    ApiModelCatalogSnapshot, DeleteProviderConnectionRequest, DeleteProviderRouteRequest,
    LlmResilienceSnapshot, ProviderConnection, ProviderProfile, ProviderRoute,
    ProviderRouteBinding, ResetLlmResilienceStateRequest, RouteTarget,
    TestProviderConnectionRequest, TestProviderConnectionResponse, UpsertProviderConnectionRequest,
    UpsertProviderRouteBindingRequest, UpsertProviderRouteRequest,
};
use hvc_core::persistence_service::{
    EmbeddingReadinessReport, ListProjectsRequest, PersistenceCommandError, Project,
    ReindexWorkspaceEmbeddingsRequest, ReindexWorkspaceEmbeddingsResponse,
    UpdateAgentRuntimeConfigRequest,
};
use hvc_core::project_deletion::{ProjectDeletionOutcome, ProjectDeletionPreview};

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
    pub(crate) cli_observation_trial: Option<hvc_core::terminals::CliObservationTrial>,
    pub(crate) cli_observation_report: Option<hvc_core::terminals::CliObservationReport>,
}

#[derive(Debug, Clone)]
pub(crate) struct SettingsSnapshot {
    pub(crate) projects: Vec<Project>,
    pub(crate) pending_project_deletions: i64,
    pub(crate) composition: Option<CompositionProjectSettings>,
    pub(crate) preparation: hvc_core::orchestration::PreparationSettings,
    pub(crate) config: AgentRuntimeConfig,
    pub(crate) profiles: Vec<ProviderProfile>,
    pub(crate) connections: Vec<ProviderConnection>,
    /// Persisted API snapshots only; fetching is always a separate explicit action.
    pub(crate) api_model_catalogs: Vec<ApiModelCatalogSnapshot>,
    pub(crate) routes: Vec<ProviderRoute>,
    pub(crate) bindings: Vec<ProviderRouteBinding>,
    pub(crate) key_statuses: Vec<ProviderKeyStatus>,
    pub(crate) resilience: LlmResilienceSnapshot,
}

pub(crate) trait SettingsBackend: Send + Sync {
    fn cli_connections(
        &self,
        refresh: bool,
    ) -> SettingsFuture<hvc_core::terminals::CliConnectionsSnapshot>;
    /// Plan usage of the metered CLIs (ADR-028); `force` is the Refresh button.
    fn quota_overview(&self, force: bool) -> SettingsFuture<hvc_core::quota::QuotaOverview>;
    /// Credential collectors run only while the window is active (ADR-028 G6).
    fn set_quota_foreground(&self, foreground: bool);
    fn set_quota_endpoint(
        &self,
        backend: hvc_core::terminals::CliBackend,
        enabled: bool,
    ) -> SettingsFuture<()>;
    fn acknowledge_quota_disclosure(
        &self,
        backend: hvc_core::terminals::CliBackend,
    ) -> SettingsFuture<()>;
    fn preview_project_deletion(
        &self,
        project_id: String,
    ) -> SettingsFuture<ProjectDeletionPreview>;
    fn delete_project_permanently(
        &self,
        project_id: String,
        confirmation: String,
    ) -> SettingsFuture<ProjectDeletionOutcome>;
    fn update_preparation(
        &self,
        settings: hvc_core::orchestration::PreparationSettings,
    ) -> SettingsFuture<()>;
    fn update_composition(
        &self,
        project_id: String,
        settings: hvc_core::orchestration::composition::settings::CompositionSettings,
    ) -> SettingsFuture<()>;
    fn start_cli_observation_trial(
        &self,
        project_id: String,
    ) -> SettingsFuture<hvc_core::terminals::CliObservationTrial>;
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
    fn refresh_api_model_catalog(
        &self,
        connection_id: String,
    ) -> SettingsFuture<ApiModelCatalogSnapshot>;
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
    fn cli_connections(
        &self,
        refresh: bool,
    ) -> SettingsFuture<hvc_core::terminals::CliConnectionsSnapshot> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move { terminals.connections(refresh).await })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("CLI connection check interrupted: {error}"))
                })
        })
    }
    fn quota_overview(&self, force: bool) -> SettingsFuture<hvc_core::quota::QuotaOverview> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move { terminals.quota_overview(force).await })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("plan usage check interrupted: {error}"))
                })
        })
    }
    fn set_quota_foreground(&self, foreground: bool) {
        self.core
            .terminals()
            .quota_meter()
            .set_foreground(foreground);
    }
    fn set_quota_endpoint(
        &self,
        backend: hvc_core::terminals::CliBackend,
        enabled: bool,
    ) -> SettingsFuture<()> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || {
                    terminals
                        .quota_meter()
                        .set_endpoint_switch(backend, enabled)
                })
                .await
                .map_err(|error| UiCommandError::worker(error.to_string()))?
                .map_err(|error| UiCommandError::worker(error.to_string()))
        })
    }
    fn acknowledge_quota_disclosure(
        &self,
        backend: hvc_core::terminals::CliBackend,
    ) -> SettingsFuture<()> {
        let terminals = self.core.terminals();
        let runtime = self.core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || terminals.quota_meter().acknowledge_disclosure(backend))
                .await
                .map_err(|error| UiCommandError::worker(error.to_string()))?
                .map_err(|error| UiCommandError::worker(error.to_string()))
        })
    }
    fn preview_project_deletion(
        &self,
        project_id: String,
    ) -> SettingsFuture<ProjectDeletionPreview> {
        self.blocking(move |persistence| persistence.preview_project_deletion(&project_id))
    }

    fn delete_project_permanently(
        &self,
        project_id: String,
        confirmation: String,
    ) -> SettingsFuture<ProjectDeletionOutcome> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn_blocking(move || core.delete_project_permanently(&project_id, &confirmation))
                .await
                .map_err(|error| UiCommandError::worker(error.to_string()))?
                .map_err(command_error)
        })
    }
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

    fn start_cli_observation_trial(
        &self,
        project_id: String,
    ) -> SettingsFuture<hvc_core::terminals::CliObservationTrial> {
        self.blocking(move |persistence| persistence.start_cli_observation_trial(&project_id))
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
                            cli_observation_trial: persistence
                                .active_cli_observation_trial(&project.id)?,
                            cli_observation_report: persistence
                                .latest_cli_observation_report(&project.id)?,
                            project_id: project.id,
                            project_name: project.name,
                        }),
                        None => None,
                    };
                    Ok(SettingsSnapshot {
                        projects: persistence
                            .list_projects(ListProjectsRequest {
                                include_archived: true,
                            })?
                            .projects,
                        pending_project_deletions: persistence.pending_project_deletion_count()?,
                        composition,
                        preparation: persistence.get_preparation_settings()?,
                        config: persistence.get_agent_runtime_config()?.config,
                        profiles: persistence.list_provider_profiles().profiles,
                        connections: persistence.list_provider_connections()?.connections,
                        api_model_catalogs: persistence.list_api_model_catalogs()?,
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

    fn refresh_api_model_catalog(
        &self,
        connection_id: String,
    ) -> SettingsFuture<ApiModelCatalogSnapshot> {
        let core = Arc::clone(&self.core);
        let runtime = core.runtime_handle().clone();
        Box::pin(async move {
            runtime
                .spawn(async move {
                    core.persistence()
                        .refresh_api_model_catalog(connection_id)
                        .await
                })
                .await
                .map_err(|error| {
                    UiCommandError::worker(format!("catalog refresh task aborted: {error}"))
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

/// A page of the Settings modal. `ALL` is the sidebar order, top to bottom.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SettingsPage {
    #[default]
    General,
    Usage,
    Projects,
    Shortcuts,
    Connections,
    Routes,
    Embeddings,
    SecondBrain,
    Preparation,
    Composition,
    Timeouts,
    Labs,
    About,
}

/// Sidebar groups, ordered from personal preferences down to system plumbing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsGroup {
    General,
    AiModels,
    Pipeline,
    System,
}

impl SettingsGroup {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::AiModels => "AI & models",
            Self::Pipeline => "Pipeline",
            Self::System => "System",
        }
    }
}

impl SettingsPage {
    pub(crate) const ALL: [Self; 13] = [
        Self::General,
        Self::Usage,
        Self::Projects,
        Self::Shortcuts,
        Self::Connections,
        Self::Routes,
        Self::Embeddings,
        Self::SecondBrain,
        Self::Preparation,
        Self::Composition,
        Self::Timeouts,
        Self::Labs,
        Self::About,
    ];

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Usage => "usage",
            Self::Projects => "projects",
            Self::Shortcuts => "shortcuts",
            Self::Connections => "connections",
            Self::Routes => "routes",
            Self::Embeddings => "embeddings",
            Self::SecondBrain => "second-brain",
            Self::Preparation => "preparation",
            Self::Composition => "composition",
            Self::Timeouts => "timeouts",
            Self::Labs => "labs",
            Self::About => "about",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Usage => "Usage",
            Self::Projects => "Projects & data",
            Self::Shortcuts => "Keyboard shortcuts",
            Self::Connections => "Connections",
            Self::Routes => "Routes",
            Self::Embeddings => "Embeddings & RAG",
            Self::SecondBrain => "Second Brain",
            Self::Preparation => "Preparation & context",
            Self::Composition => "Stage composition",
            Self::Timeouts => "Agent timeouts",
            Self::Labs => "Labs",
            Self::About => "About",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::General => "How the workbench behaves around you.",
            Self::Usage => "Tokens recorded by Hard Vibe Code across projects and transports.",
            Self::Projects => {
                "Review local projects and permanently delete their repository and project data."
            }
            Self::Shortcuts => "Every keyboard shortcut, grouped by where it works.",
            Self::Connections => "API providers, CLI connections and subscription plan usage.",
            Self::Routes => "Ordered fallbacks across connections and the agents bound to them.",
            Self::Embeddings => "Vector search over your notes, served by a local Ollama.",
            Self::SecondBrain => "How far agents navigate the note graph and resolve code from it.",
            Self::Preparation => {
                "Refinement before Specification, the handoff budget and learning after a run."
            }
            Self::Composition => "Host-coordinated units, checks and overrides for this project.",
            Self::Timeouts => "Deadlines that stop hung or runaway agent runs.",
            Self::Labs => "Experimental switches. Each one says what it changes and can be reset.",
            Self::About => "Version, where your data lives and a snapshot of the runtime.",
        }
    }

    pub(crate) fn icon(self) -> crate::icons::Icon {
        use crate::icons::Icon;
        match self {
            Self::General => Icon::SlidersHorizontal,
            Self::Usage => Icon::ChartColumn,
            Self::Projects => Icon::Folder,
            Self::Shortcuts => Icon::Keyboard,
            Self::Connections => Icon::Plug,
            Self::Routes => Icon::Route,
            Self::Embeddings => Icon::Database,
            Self::SecondBrain => Icon::Brain,
            Self::Preparation => Icon::ListTodo,
            Self::Composition => Icon::Workflow,
            Self::Timeouts => Icon::Clock,
            Self::Labs => Icon::FlaskConical,
            Self::About => Icon::Info,
        }
    }

    pub(crate) fn group(self) -> SettingsGroup {
        match self {
            Self::General | Self::Usage | Self::Projects | Self::Shortcuts => {
                SettingsGroup::General
            }
            Self::Connections | Self::Routes | Self::Embeddings | Self::SecondBrain => {
                SettingsGroup::AiModels
            }
            Self::Preparation | Self::Composition | Self::Timeouts => SettingsGroup::Pipeline,
            Self::Labs | Self::About => SettingsGroup::System,
        }
    }

    /// Extra words a search should find this page by, beyond its label and description.
    pub(crate) fn keywords(self) -> &'static str {
        match self {
            Self::General => "preferences workflow layout accessibility",
            Self::Usage => "tokens cost billing input output chart",
            Self::Projects => "repository delete remove folder disk pulsations companion history",
            Self::Shortcuts => "keyboard hotkeys keybindings keys",
            Self::Connections => {
                "providers api keys openai anthropic ollama credentials cli codex claude gemini opencode antigravity plan quota usage 5h weekly"
            }
            Self::Routes => "routing fallback failover binding resilience cooldown",
            Self::Embeddings => "rag vectors ollama semantic search index",
            Self::SecondBrain => "adaptive navigation vault notes graph wikilinks",
            Self::Preparation => "taskrefine prespec vagueness budget learning brownfield",
            Self::Composition => "v4 composed batches units validation checks overrides",
            Self::Timeouts => "deadline inactivity ceiling streaming",
            Self::Labs => "experimental preview flags beta",
            Self::About => "version build data directory database diagnostics",
        }
    }

    /// Pages that show the provider configuration snapshot and wait for it to load.
    pub(crate) fn needs_snapshot(self) -> bool {
        !matches!(
            self,
            Self::General | Self::Usage | Self::Shortcuts | Self::Timeouts | Self::About
        )
    }
}

/// What a connection test found, in the provider-neutral words a developer acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectionTestOutcome {
    Connected,
    /// The provider answered, but with an empty reply.
    ConnectedWithWarnings,
    NoApiKey,
    AuthenticationFailed,
    BillingProblem,
    PermissionDenied,
    ModelNotFound,
    RateLimited,
    TimedOut,
    Unreachable,
    ProviderUnavailable,
    InvalidResponse,
    Failed,
}

impl ConnectionTestOutcome {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::ConnectedWithWarnings => "Connected with warnings",
            Self::NoApiKey => "No API key",
            Self::AuthenticationFailed => "Authentication failed",
            Self::BillingProblem => "Billing problem",
            Self::PermissionDenied => "Permission denied",
            Self::ModelNotFound => "Model not found",
            Self::RateLimited => "Rate limited",
            Self::TimedOut => "Timed out",
            Self::Unreachable => "Unreachable",
            Self::ProviderUnavailable => "Provider unavailable",
            Self::InvalidResponse => "Invalid response",
            Self::Failed => "Test failed",
        }
    }

    /// What to do next, shown in the result's "i".
    pub(crate) fn advice(self) -> &'static str {
        match self {
            Self::Connected => "The provider answered with the default model.",
            Self::ConnectedWithWarnings => {
                "The provider answered, but the reply was empty. Check the default model."
            }
            Self::NoApiKey => "Paste the provider's API key and save the connection.",
            Self::AuthenticationFailed => {
                "The provider rejected the key. Paste a new one and save the connection."
            }
            Self::BillingProblem => "The provider account needs credit or a payment method.",
            Self::PermissionDenied => {
                "The key is valid but not allowed to use this model or endpoint."
            }
            Self::ModelNotFound => "The default model does not exist for this provider or key.",
            Self::RateLimited => "The provider is throttling this key. Wait and test again.",
            Self::TimedOut => "No answer within 30 seconds. Check the endpoint and network.",
            Self::Unreachable => "The endpoint could not be reached. Check the URL and network.",
            Self::ProviderUnavailable => "The provider reported an outage. Try again later.",
            Self::InvalidResponse => "The provider's answer was not in the expected format.",
            Self::Failed => "The test did not complete; the detail says why.",
        }
    }

    pub(crate) fn tone(self) -> StatusTone {
        match self {
            Self::Connected => StatusTone::Saved,
            Self::ConnectedWithWarnings | Self::RateLimited => StatusTone::Working,
            _ => StatusTone::Failed,
        }
    }

    /// Classifies a failed test from the error's kind (when the core reports it) or, failing
    /// that, from its code and HTTP status.
    pub(crate) fn from_error(error: &UiCommandError) -> Self {
        let details = error.details.as_ref();
        let kind = details
            .and_then(|details| details.get("kind"))
            .and_then(|kind| kind.as_str());
        let status = details
            .and_then(|details| details.get("httpStatus"))
            .and_then(|status| status.as_u64());
        match kind {
            Some("MissingApiKey") => return Self::NoApiKey,
            Some("Authentication") => return Self::AuthenticationFailed,
            Some("Billing") => return Self::BillingProblem,
            Some("Permission") => return Self::PermissionDenied,
            Some("ModelNotFound") => return Self::ModelNotFound,
            Some("RateLimited") => return Self::RateLimited,
            Some("Timeout") => return Self::TimedOut,
            Some("Network") => return Self::Unreachable,
            Some("ProviderUnavailable") => return Self::ProviderUnavailable,
            Some("InvalidResponse" | "OutputSchema") => return Self::InvalidResponse,
            _ => {}
        }
        match (error.code.as_str(), status) {
            ("missing_api_key", _) => Self::NoApiKey,
            ("provider_timeout", _) => Self::TimedOut,
            (_, Some(401)) => Self::AuthenticationFailed,
            (_, Some(402)) => Self::BillingProblem,
            (_, Some(403)) => Self::PermissionDenied,
            (_, Some(404)) => Self::ModelNotFound,
            (_, Some(429)) => Self::RateLimited,
            (_, Some(500..=599)) => Self::ProviderUnavailable,
            ("provider_blocked", _) => Self::AuthenticationFailed,
            _ => Self::Failed,
        }
    }
}

/// The last test of one connection, kept for the session.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConnectionTestResult {
    pub(crate) outcome: ConnectionTestOutcome,
    pub(crate) latency_ms: Option<u64>,
    pub(crate) model: String,
    pub(crate) detail: String,
    /// Wall-clock `HH:MM` of the test.
    pub(crate) at: String,
}

impl ConnectionTestResult {
    /// "Connected · 412 ms · 08:31"
    pub(crate) fn summary(&self) -> String {
        let mut parts = vec![self.outcome.label().to_string()];
        if let Some(latency) = self.latency_ms {
            parts.push(format!("{latency} ms"));
        }
        parts.push(self.at.clone());
        parts.join(" · ")
    }
}

/// Outcome of the last Settings operation, shown in the modal's status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusTone {
    Working,
    Saved,
    Failed,
}

#[derive(Debug)]
pub(crate) struct SettingsModel {
    pub(crate) cli_connections: Option<hvc_core::terminals::CliConnectionsSnapshot>,
    pub(crate) cli_connections_loading: bool,
    pub(crate) cli_connections_error: Option<UiCommandError>,
    pub(crate) usage: crate::usage::UsageState,
    pub(crate) deletion_preview: Option<ProjectDeletionPreview>,
    pub(crate) deletion_project_id: Option<String>,
    pub(crate) deletion_loading: bool,
    pub(crate) deletion_busy: bool,
    pub(crate) deletion_generation: u64,
    pub(crate) page: SettingsPage,
    /// Connections/Routes drill-in: the detail of the selected (or new) item is open.
    pub(crate) connection_detail: bool,
    pub(crate) route_detail: bool,
    pub(crate) status_line: Option<(StatusTone, SharedString)>,
    /// Last test per connection id, and the connection being tested now.
    pub(crate) connection_tests: std::collections::HashMap<String, ConnectionTestResult>,
    pub(crate) testing_connection: Option<String>,
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
            cli_connections: None,
            cli_connections_loading: false,
            cli_connections_error: None,
            usage: Default::default(),
            deletion_preview: None,
            deletion_project_id: None,
            deletion_loading: false,
            deletion_busy: false,
            deletion_generation: 0,
            page: SettingsPage::General,
            connection_detail: false,
            route_detail: false,
            status_line: None,
            connection_tests: Default::default(),
            testing_connection: None,
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
        let previous = self.snapshot.as_ref();
        let connection_ids: Vec<&str> =
            snapshot.connections.iter().map(|c| c.id.as_str()).collect();
        let route_ids: Vec<&str> = snapshot.routes.iter().map(|r| r.id.as_str()).collect();
        let (connection, creating_connection, keep_connection_detail) = reconcile_selection(
            self.selected_connection_id.as_ref().map(|id| id.as_ref()),
            self.creating_connection,
            &connection_ids,
            previous.map(|p| {
                p.connections
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect::<Vec<_>>()
            }),
        );
        let (route, creating_route, keep_route_detail) = reconcile_selection(
            self.selected_route_id.as_ref().map(|id| id.as_ref()),
            self.creating_route,
            &route_ids,
            previous.map(|p| p.routes.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()),
        );
        self.selected_connection_id = connection.map(Into::into);
        self.creating_connection = creating_connection;
        self.connection_detail &= keep_connection_detail;
        self.selected_route_id = route.map(Into::into);
        self.creating_route = creating_route;
        self.route_detail &= keep_route_detail;
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

    pub(crate) fn api_model_catalog(&self) -> Option<&ApiModelCatalogSnapshot> {
        let id = self.selected_connection_id.as_ref()?;
        let snapshot = self.snapshot.as_ref()?;
        let provider_id = self.connection()?.provider_id.as_str();
        snapshot
            .api_model_catalogs
            .iter()
            .find(|item| item.connection_id == id.as_ref() && item.provider_id == provider_id)
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

/// Selection after a snapshot, for one list (connections or routes). Returns the selected id,
/// whether a "New …" form is still being filled, and whether an open detail may stay open.
///
/// While creating, nothing is auto-selected — the next Save would otherwise update the first item
/// instead of creating one — until an id appears that the previous snapshot did not have, which
/// is the item just created. Otherwise an invalid selection falls back to the first item, and a
/// detail that was showing a deleted item closes.
fn reconcile_selection(
    selected: Option<&str>,
    creating: bool,
    ids: &[&str],
    previous: Option<Vec<&str>>,
) -> (Option<String>, bool, bool) {
    if creating {
        let created = previous
            .as_ref()
            .and_then(|previous| ids.iter().find(|id| !previous.contains(id)));
        return match created {
            Some(id) => (Some((*id).to_owned()), false, true),
            None => (None, true, true),
        };
    }
    match selected {
        Some(id) if ids.contains(&id) => (Some(id.to_owned()), false, true),
        Some(_) => (ids.first().map(|id| (*id).to_owned()), false, false),
        None => (ids.first().map(|id| (*id).to_owned()), false, true),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gpui::{AppContext as _, TestAppContext};
    use hvc_core::agent_runtime::AgentRuntimeConfig;
    use hvc_core::llm::{
        ApiModelCatalogEntry, ApiModelCatalogSnapshot, ApiModelCatalogState, LlmResilienceSnapshot,
        ProviderConnection, ProviderRoute, ProviderRouteBinding, RoutingMode,
        TestProviderConnectionRequest, UpsertProviderConnectionRequest,
    };
    use hvc_core::persistence_service::UpdateAgentRuntimeConfigRequest;
    use hvc_test_support::{CoreTestHarness, ScriptedHttpServer, ScriptedResponse};

    use super::{CoreSettingsBackend, SettingsBackend, SettingsModel, SettingsSnapshot};
    use crate::AppModel;

    #[test]
    fn a_new_item_form_survives_refreshes_until_the_created_item_appears() {
        use super::reconcile_selection;
        // Creating: a refresh with the same ids keeps the form empty instead of selecting the
        // first item (whose Save would then update it).
        assert_eq!(
            reconcile_selection(None, true, &["a", "b"], Some(vec!["a", "b"])),
            (None, true, true)
        );
        // The created item appears: it is selected and the detail stays open on it.
        assert_eq!(
            reconcile_selection(None, true, &["a", "b", "c"], Some(vec!["a", "b"])),
            (Some("c".into()), false, true)
        );
        // Not creating: a valid selection is kept, a missing one falls back to the first item.
        assert_eq!(
            reconcile_selection(Some("b"), false, &["a", "b"], Some(vec!["a", "b"])),
            (Some("b".into()), false, true)
        );
        assert_eq!(
            reconcile_selection(None, false, &["a"], None),
            (Some("a".into()), false, true)
        );
        // The item open in the detail was deleted: fall back and close the detail.
        assert_eq!(
            reconcile_selection(Some("b"), false, &["a"], Some(vec!["a", "b"])),
            (Some("a".into()), false, false)
        );
    }

    #[test]
    fn renders_connections_routes_and_bindings() {
        let mut model = SettingsModel::default();
        let generation = model.begin(true);
        model.apply_snapshot(
            generation,
            SettingsSnapshot {
                projects: Vec::new(),
                pending_project_deletions: 0,
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
                api_model_catalogs: Vec::new(),
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
    fn api_catalog_selection_only_fills_the_connection_draft(cx: &mut TestAppContext) {
        let harness = CoreTestHarness::new();
        let core = harness.core();
        let backend = Arc::new(CoreSettingsBackend::new(Arc::clone(&core)));
        let verify_backend = backend.clone();
        let mut snapshot = core.runtime_handle().block_on(backend.snapshot()).unwrap();
        let connection_id = snapshot
            .connections
            .iter()
            .find(|connection| connection.provider_id == "openai_legacy")
            .unwrap()
            .id
            .clone();
        let persisted_connection_id = connection_id.clone();
        let saved_model = snapshot
            .connections
            .iter()
            .find(|connection| connection.id == connection_id)
            .unwrap()
            .default_model
            .clone();
        snapshot.api_model_catalogs.push(ApiModelCatalogSnapshot {
            connection_id: connection_id.clone(),
            provider_id: "openai_legacy".into(),
            state: ApiModelCatalogState::Ready,
            source: "provider_api".into(),
            diagnostic: None,
            refreshed_at: "now".into(),
            models: vec![ApiModelCatalogEntry {
                model_id: "gpt-api-fixture".into(),
                display_name: "GPT API fixture".into(),
                context_window: Some(128),
                max_output_tokens: Some(16),
                capabilities: vec![],
                reasoning_efforts: vec![],
                raw_metadata: serde_json::json!({}),
            }],
        });
        let app = cx.new(|cx| AppModel::new_for_test_with_settings_backend(backend, cx));
        app.update(cx, |model, cx| {
            model
                .settings
                .update(cx, |settings, _| settings.snapshot = Some(snapshot));
            model.select_settings_connection(connection_id, cx);
            assert!(
                model
                    .choice_options("settings-api-model", cx)
                    .iter()
                    .any(|choice| choice.value == "gpt-api-fixture")
            );
            model.select_settings_api_model("gpt-api-fixture".into(), cx);
            assert_eq!(
                model.settings_connection_model_input.read(cx).value(),
                "gpt-api-fixture"
            );
            assert_ne!(
                saved_model, "gpt-api-fixture",
                "the persisted connection was not saved by selection"
            );
        });
        let persisted = core
            .runtime_handle()
            .block_on(verify_backend.snapshot())
            .unwrap();
        assert_eq!(
            persisted
                .connections
                .iter()
                .find(|connection| connection.id == persisted_connection_id)
                .map(|connection| connection.default_model.clone()),
            Some(saved_model),
            "leaving the draft without Save does not change the connection"
        );
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            cx.run_until_parked();
            if cx.read(|cx| app.read(cx).settings.read(cx).error.is_some()) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
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

    #[test]
    fn a_rejected_key_is_reported_as_an_authentication_failure_end_to_end() {
        use super::ConnectionTestOutcome;
        let rejected = || ScriptedResponse::json(401, r#"{"error":{"message":"invalid api key"}}"#);
        let server = ScriptedHttpServer::start([rejected(), rejected(), rejected()]);
        let harness = CoreTestHarness::new();
        let core = harness.core();
        let backend = CoreSettingsBackend::new(Arc::clone(&core));
        let connection = core
            .runtime_handle()
            .block_on(backend.upsert_connection(connection_request(
                "connection-rejected",
                Some(server.url("")),
                None,
            )))
            .unwrap();

        let error = core
            .runtime_handle()
            .block_on(backend.test_connection(TestProviderConnectionRequest {
                connection_id: connection.id,
            }))
            .unwrap_err();

        assert_eq!(
            error
                .details
                .as_ref()
                .and_then(|details| details.get("kind"))
                .and_then(|kind| kind.as_str()),
            Some("Authentication"),
            "{error:?}"
        );
        assert_eq!(
            ConnectionTestOutcome::from_error(&error),
            ConnectionTestOutcome::AuthenticationFailed
        );
    }

    #[test]
    fn connection_test_outcomes_fall_back_to_the_code_and_http_status() {
        use super::{ConnectionTestOutcome as Outcome, UiCommandError};
        use serde_json::json;
        let error = |code: &str, details: Option<serde_json::Value>| UiCommandError {
            code: code.into(),
            message: "failed".into(),
            details,
            remediation: None,
        };
        let cases = [
            (
                error("llm", Some(json!({"kind": "RateLimited"}))),
                Outcome::RateLimited,
            ),
            (
                error("llm", Some(json!({"kind": "Network"}))),
                Outcome::Unreachable,
            ),
            (
                error("llm", Some(json!({"kind": "OutputSchema"}))),
                Outcome::InvalidResponse,
            ),
            (error("missing_api_key", None), Outcome::NoApiKey),
            (error("provider_timeout", None), Outcome::TimedOut),
            (
                error("llm", Some(json!({"httpStatus": 402}))),
                Outcome::BillingProblem,
            ),
            (
                error("llm", Some(json!({"httpStatus": 404}))),
                Outcome::ModelNotFound,
            ),
            (
                error("llm", Some(json!({"httpStatus": 503}))),
                Outcome::ProviderUnavailable,
            ),
            // A kind the test does not explain still uses the status.
            (
                error(
                    "llm",
                    Some(json!({"kind": "ModelMismatch", "httpStatus": 401})),
                ),
                Outcome::AuthenticationFailed,
            ),
            (error("io_error", None), Outcome::Failed),
        ];
        for (error, expected) in cases {
            assert_eq!(Outcome::from_error(&error), expected, "{error:?}");
        }
    }

    #[test]
    fn a_connection_test_summary_reads_outcome_latency_and_time() {
        use super::{ConnectionTestOutcome, ConnectionTestResult, StatusTone};
        let connected = ConnectionTestResult {
            outcome: ConnectionTestOutcome::Connected,
            latency_ms: Some(412),
            model: "gpt-4.1".into(),
            detail: "ok".into(),
            at: "08:31".into(),
        };
        assert_eq!(connected.summary(), "Connected · 412 ms · 08:31");
        assert_eq!(connected.outcome.tone(), StatusTone::Saved);
        let failed = ConnectionTestResult {
            outcome: ConnectionTestOutcome::NoApiKey,
            latency_ms: None,
            ..connected
        };
        assert_eq!(failed.summary(), "No API key · 08:31");
        assert_eq!(failed.outcome.tone(), StatusTone::Failed);
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
