//! What the Agents canvas and the synthesis know about each agent (feature
//! 25, M1; before, the list of ADR-040 M7): the pipeline stages in their
//! order, the support agents and roles in neither list, the free-form
//! agents, and the archived ones apart.

use gpui::Rgba;
use ikore_core::llm::AgentRole;
use ikore_core::terminals::{
    AgentExecutionMode, CliAgentConfig, CliBackend, SessionSlot, TerminalRun, TerminalRunStatus,
    TerminalsSnapshot,
};

use crate::agents::AgentIdentifier;
use crate::animated_icon::AnimClip;
use crate::app_model::AppModel;
use crate::settings::SettingsSnapshot;
use crate::theme;

/// The pipeline stages, in the pipeline's order.
pub(super) const PIPELINE: [AgentRole; 6] = [
    AgentRole::IntakeRouter,
    AgentRole::TaskSpecifier,
    AgentRole::ContractAgent,
    AgentRole::BackendAgent,
    AgentRole::TestAgent,
    AgentRole::Integrator,
];

/// The agents beside the pipeline.
pub(super) const SUPPORT: [AgentRole; 5] = [
    AgentRole::FeatureAgent,
    AgentRole::Visualizer,
    AgentRole::LearningAgent,
    AgentRole::NotesScribe,
    AgentRole::DocumentationReconciler,
];

/// What the page shows about one agent.
#[derive(Debug, Clone)]
pub(super) struct AgentItem {
    pub(super) id: AgentIdentifier,
    pub(super) label: String,
    pub(super) color: Rgba,
    pub(super) execution_mode: AgentExecutionMode,
    /// The latest run.
    pub(super) run: Option<TerminalRun>,
    /// `CLI · claude`, or `API · {route}`.
    pub(super) transport: String,
    pub(super) fallbacks: usize,
    /// Composition units running now.
    pub(super) units: usize,
}

/// Animated icon clip for a canonical role based on its execution mode and pipeline position.
pub(super) fn canonical_clip(role: AgentRole, mode: AgentExecutionMode) -> AnimClip {
    if mode == AgentExecutionMode::ApiDirect {
        AnimClip::AgentsApi
    } else if PIPELINE.contains(&role) {
        AnimClip::AgentsTerminal
    } else {
        AnimClip::AgentsWand
    }
}

impl AgentItem {
    /// Failed, waiting for a fallback decision, or with a pending fallback.
    pub(super) fn needs_attention(&self) -> bool {
        self.fallbacks > 0
            || self.run.as_ref().is_some_and(|run| {
                matches!(
                    run.status,
                    TerminalRunStatus::Failed | TerminalRunStatus::AwaitingFallback
                )
            })
    }

    pub(super) fn clip(&self) -> AnimClip {
        match self.id {
            AgentIdentifier::Canonical(role) => canonical_clip(role, self.execution_mode),
            AgentIdentifier::FreeForm(_) => {
                if self.execution_mode == AgentExecutionMode::ApiDirect {
                    AnimClip::AgentsApi
                } else {
                    AnimClip::AgentsWand
                }
            }
            AgentIdentifier::PipelineStep { .. } => AnimClip::AgentsTerminal,
            AgentIdentifier::StageVerifier(_) => AnimClip::AgentsOracle,
        }
    }
}

/// Every agent of the project, grouped as the canvas lays them out.
#[derive(Debug, Clone, Default)]
pub(super) struct AgentItems {
    pub(super) stages: Vec<AgentItem>,
    /// Support agents, then roles in neither list (a newer build's).
    pub(super) support: Vec<AgentItem>,
    pub(super) free_form: Vec<AgentItem>,
    pub(super) archived: Vec<AgentItem>,
}

/// The transport badge: the CLI's backend, or the API's bound route when it
/// has one.
pub(super) fn transport(
    mode: AgentExecutionMode,
    backend: CliBackend,
    route: Option<String>,
) -> String {
    match (mode, route) {
        (AgentExecutionMode::CliProcess, _) => format!("CLI · {}", backend.as_str()),
        (AgentExecutionMode::ApiDirect, Some(route)) => format!("API · {route}"),
        (AgentExecutionMode::ApiDirect, None) => "API".to_owned(),
    }
}

fn route_name(settings: Option<&SettingsSnapshot>, route_id: &str) -> Option<String> {
    settings
        .and_then(|settings| settings.routes.iter().find(|route| route.id == route_id))
        .map(|route| route.name.clone())
}

/// The route a stage's agent is bound to, by the same binding
/// `configure_agent_api` resolves: project, role and stage.
pub(super) fn stage_route(
    settings: Option<&SettingsSnapshot>,
    project_id: &str,
    role: AgentRole,
) -> Option<String> {
    let stage = ikore_core::terminals::canonical_agent(role).stage;
    settings
        .and_then(|settings| {
            settings.bindings.iter().find(|binding| {
                binding.project_id == project_id
                    && binding.agent_role == role.as_str()
                    && binding.stage.eq_ignore_ascii_case(stage)
            })
        })
        .and_then(|binding| route_name(settings, &binding.route_id))
}

/// The composition units of an agent running now.
fn running_units(runs: &[TerminalRun]) -> usize {
    runs.iter()
        .filter(|run| matches!(run.slot, SessionSlot::Ephemeral { .. }) && run.status.is_active())
        .count()
}

/// The agents of the snapshot, each with its latest run, transport, pending
/// fallbacks and running units.
pub(super) fn agent_items(
    model: &AppModel,
    snapshot: &TerminalsSnapshot,
    cx: &gpui::App,
) -> AgentItems {
    let agents = model.agents.read(cx);
    let settings = model.settings.read(cx).snapshot.as_ref();
    let canonical_item = |config: &CliAgentConfig, runs: &[TerminalRun]| AgentItem {
        id: AgentIdentifier::Canonical(config.agent_role),
        label: ikore_core::terminals::canonical_agent(config.agent_role)
            .name
            .to_owned(),
        color: theme::agent_color(config.agent_role),
        execution_mode: config.execution_mode,
        run: runs.first().cloned(),
        transport: transport(
            config.execution_mode,
            config.backend,
            stage_route(settings, &snapshot.project_id, config.agent_role),
        ),
        fallbacks: agents
            .fallbacks
            .iter()
            .filter(|fallback| fallback.agent_role == config.agent_role)
            .count(),
        units: running_units(runs),
    };
    let group = |roles: &[AgentRole]| {
        roles
            .iter()
            .filter_map(|role| {
                snapshot
                    .agents
                    .iter()
                    .find(|agent| agent.config.agent_role == *role)
            })
            .map(|agent| canonical_item(&agent.config, &agent.recent_runs))
            .collect::<Vec<_>>()
    };
    let slot_item = |slot: &ikore_core::terminals::FreeFormTerminalSnapshot| AgentItem {
        id: AgentIdentifier::FreeForm(slot.slot.id.clone()),
        label: slot.slot.label.clone(),
        color: theme::accent(),
        execution_mode: slot.slot.execution_mode,
        run: slot.recent_runs.first().cloned(),
        transport: transport(
            slot.slot.execution_mode,
            slot.slot.backend,
            agents
                .free_form_bindings
                .iter()
                .find(|binding| binding.slot_id == slot.slot.id)
                .and_then(|binding| route_name(settings, &binding.route_id)),
        ),
        fallbacks: 0,
        units: 0,
    };
    // A role in neither list (a newer build's) still shows, with the support agents.
    let mut support = group(&SUPPORT);
    support.extend(
        snapshot
            .agents
            .iter()
            .filter(|agent| {
                !PIPELINE.contains(&agent.config.agent_role)
                    && !SUPPORT.contains(&agent.config.agent_role)
            })
            .map(|agent| canonical_item(&agent.config, &agent.recent_runs)),
    );
    AgentItems {
        stages: group(&PIPELINE),
        support,
        free_form: snapshot.free_form_slots.iter().map(slot_item).collect(),
        archived: snapshot
            .archived_free_form_slots
            .iter()
            .map(slot_item)
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use ikore_core::llm::{LlmResilienceSnapshot, ProviderRoute, ProviderRouteBinding};

    use super::*;

    /// One route, `Primary`, bound to the Backend agent of `project` at `stage`.
    fn settings(stage: &str) -> SettingsSnapshot {
        SettingsSnapshot {
            projects: vec![],
            pending_project_deletions: 0,
            composition: None,
            preparation: Default::default(),
            config: Default::default(),
            profiles: vec![],
            connections: vec![],
            api_model_catalogs: vec![],
            routes: vec![ProviderRoute {
                id: "route-1".into(),
                name: "Primary".into(),
                strategy: "ordered".into(),
                max_attempts: 1,
                attempt_timeout_ms: 1,
                route_timeout_ms: 1,
                enabled: true,
                version: 1,
                targets: vec![],
                created_at: "now".into(),
                updated_at: "now".into(),
            }],
            bindings: vec![ProviderRouteBinding {
                project_id: "project".into(),
                agent_role: AgentRole::BackendAgent.as_str().into(),
                stage: stage.into(),
                route_id: "route-1".into(),
                enabled: true,
                version: 1,
                created_at: "now".into(),
                updated_at: "now".into(),
            }],
            key_statuses: vec![],
            resilience: LlmResilienceSnapshot {
                generated_at: "now".into(),
                states: vec![],
            },
            attestation: None,
        }
    }

    /// ADR-040 M7 (12.1): the badge names the CLI's backend, or the API's route.
    #[test]
    fn the_transport_badge_names_the_backend_or_the_route() {
        let cli = transport(AgentExecutionMode::CliProcess, CliBackend::Claude, None);
        assert_eq!(cli, "CLI · claude");
        let routed = transport(
            AgentExecutionMode::ApiDirect,
            CliBackend::Claude,
            Some("Primary".into()),
        );
        assert_eq!(routed, "API · Primary");
        let unbound = transport(AgentExecutionMode::ApiDirect, CliBackend::Claude, None);
        assert_eq!(unbound, "API");
    }

    /// The route is the project's binding for the role at its stage, whatever
    /// the stage's case; nothing else names it.
    #[test]
    fn a_stage_finds_its_route_by_project_role_and_stage() {
        let stage = ikore_core::terminals::canonical_agent(AgentRole::BackendAgent).stage;
        let settings = settings(&stage.to_uppercase());
        let found = stage_route(Some(&settings), "project", AgentRole::BackendAgent);
        assert_eq!(found.as_deref(), Some("Primary"));
        assert_eq!(
            stage_route(Some(&settings), "another", AgentRole::BackendAgent),
            None
        );
        assert_eq!(
            stage_route(Some(&settings), "project", AgentRole::TaskSpecifier),
            None
        );
        assert_eq!(stage_route(None, "project", AgentRole::BackendAgent), None);
        assert_eq!(route_name(Some(&settings), "route-2"), None);
    }

    #[test]
    fn agent_clips_match_role_and_execution_mode() {
        assert_eq!(
            canonical_clip(AgentRole::BackendAgent, AgentExecutionMode::CliProcess),
            AnimClip::AgentsTerminal
        );
        assert_eq!(
            canonical_clip(AgentRole::BackendAgent, AgentExecutionMode::ApiDirect),
            AnimClip::AgentsApi
        );
        assert_eq!(
            canonical_clip(AgentRole::FeatureAgent, AgentExecutionMode::CliProcess),
            AnimClip::AgentsWand
        );
        assert_eq!(
            canonical_clip(AgentRole::FeatureAgent, AgentExecutionMode::ApiDirect),
            AnimClip::AgentsApi
        );
        let free_form_cli = AgentItem {
            id: AgentIdentifier::FreeForm("slot-1".into()),
            label: "Free".into(),
            color: theme::accent(),
            execution_mode: AgentExecutionMode::CliProcess,
            run: None,
            transport: "CLI · claude".into(),
            fallbacks: 0,
            units: 0,
        };
        assert_eq!(free_form_cli.clip(), AnimClip::AgentsWand);
        let free_form_api = AgentItem {
            execution_mode: AgentExecutionMode::ApiDirect,
            ..free_form_cli
        };
        assert_eq!(free_form_api.clip(), AnimClip::AgentsApi);
    }
}
