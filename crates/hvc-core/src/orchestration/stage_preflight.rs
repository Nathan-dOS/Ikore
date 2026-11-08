//! Before a pulsation is queued, check that every stage can reach a model.
//!
//! "Haiku Teste 1" (2026-09-26) passed Intake and Specification, waited for an
//! 11-minute review, and then its Contract failed in 2 s: the role ran on the
//! API route `Legacy default`, whose only target sent `gpt-4.1-mini` to a
//! connection that had become Gemini on 2026-09-23. The configuration was
//! already broken when the pulsation started. The check mirrors the runtime's
//! choices (`cli_agent_configs`, then the role's route and its enabled
//! targets) and blocks only what is certain to fail. Anything it cannot prove
//! stays a warning.
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::llm::{AgentRole, SqliteLlmRepository, model_family_conflict};
use crate::terminals::{CliAvailability, CliAvailabilityState, CliBackend};

use super::catalog::STAGES;

/// One stage that cannot, or might not, reach its model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StagePreflightIssue {
    pub stage: String,
    pub role: String,
    /// True when the stage is certain to fail: the pulsation must not start.
    pub blocking: bool,
    pub message: String,
}

/// The last CLI discovery, if any, and whether it is still fresh. A stale
/// snapshot can only warn: the developer may have fixed the CLI since.
pub type CachedDiscovery<'a> = Option<(&'a [CliAvailability], bool)>;

pub fn check_stages(
    db: &Connection,
    llm: &SqliteLlmRepository,
    project_id: &str,
    discovery: CachedDiscovery<'_>,
) -> Result<Vec<StagePreflightIssue>, rusqlite::Error> {
    let mut issues = Vec::new();
    for (_, stage, role) in STAGES {
        let label = stage_label(stage);
        let configured: Option<(String, String, Option<String>)> = db
            .query_row(
                "SELECT execution_mode,backend,custom_executable FROM cli_agent_configs WHERE project_id=?1 AND agent_role=?2",
                params![project_id, role.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let found = match configured {
            Some((mode, backend, executable)) if mode == "cli_process" => {
                check_cli(&backend, executable.as_deref(), discovery)
            }
            _ => check_api(db, llm, project_id, role, stage)?,
        };
        issues.extend(
            found
                .into_iter()
                .map(|(blocking, detail)| StagePreflightIssue {
                    stage: stage.to_owned(),
                    role: role.as_str().to_owned(),
                    blocking,
                    message: format!("{label} ({}): {detail}", role.as_str()),
                }),
        );
    }
    Ok(issues)
}

/// One sentence for a refused dispatch, naming every blocked stage and the fix.
pub fn blocking_message(issues: &[StagePreflightIssue]) -> Option<String> {
    let blocked = issues
        .iter()
        .filter(|issue| issue.blocking)
        .map(|issue| issue.message.as_str())
        .collect::<Vec<_>>();
    (!blocked.is_empty()).then(|| {
        format!(
            "Pulsation not started, because a stage cannot reach its model. {} Fix the route in Settings → Routes, or switch the role in Agents.",
            blocked.join(" ")
        )
    })
}

fn stage_label(stage: &str) -> &str {
    if stage == "SPECIFICATION" {
        "Specification"
    } else {
        stage
    }
}

fn check_cli(
    backend: &str,
    executable: Option<&str>,
    discovery: CachedDiscovery<'_>,
) -> Vec<(bool, String)> {
    let Some(backend) = CliBackend::parse(backend) else {
        return vec![(true, format!("the CLI backend `{backend}` is unknown."))];
    };
    if backend == CliBackend::Custom {
        return if executable.is_some_and(|path| !path.trim().is_empty()) {
            Vec::new()
        } else {
            vec![(true, "the custom CLI has no executable.".into())]
        };
    }
    // Without a snapshot nothing is known; the launch still verifies the CLI.
    let Some((items, fresh)) = discovery else {
        return Vec::new();
    };
    let Some(item) = items.iter().find(|item| item.backend == backend) else {
        return Vec::new();
    };
    let reason = match item.state {
        CliAvailabilityState::Available | CliAvailabilityState::Probing => return Vec::new(),
        CliAvailabilityState::Missing => "is not installed",
        CliAvailabilityState::AuthRequired => "needs a login",
        CliAvailabilityState::Unsupported
        | CliAvailabilityState::UnsupportedVersion
        | CliAvailabilityState::CapabilityMissing => "is not supported in this version",
        CliAvailabilityState::ProbeFailed => "could not be checked",
    };
    let detail = item
        .diagnostic
        .as_deref()
        .map(|diagnostic| format!(" ({diagnostic})"))
        .unwrap_or_default();
    let blocking = fresh && item.state != CliAvailabilityState::ProbeFailed;
    vec![(
        blocking,
        format!("the {} CLI {reason}{detail}.", backend.label()),
    )]
}

fn check_api(
    db: &Connection,
    llm: &SqliteLlmRepository,
    project_id: &str,
    role: AgentRole,
    stage: &str,
) -> Result<Vec<(bool, String)>, rusqlite::Error> {
    let route = match llm.resolve_route_on(db, project_id, role, stage) {
        Ok(route) => route,
        Err(error) => return Ok(vec![(true, format!("its API route is unusable: {error}"))]),
    };
    let targets = match llm.resolved_targets_on(db, &route.id) {
        Ok(targets) => targets,
        Err(error) => {
            return Ok(vec![(
                true,
                format!("the API route `{}` cannot be read: {error}", route.name),
            )]);
        }
    };
    if targets.is_empty() {
        return Ok(vec![(
            true,
            format!("the API route `{}` has no enabled target.", route.name),
        )]);
    }
    let mut problems = Vec::new();
    for target in &targets {
        let connection = &target.connection;
        let name: String = db
            .query_row(
                "SELECT display_name FROM provider_connections WHERE id=?1",
                [connection.id.as_str()],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or_else(|| connection.id.to_string());
        let model = &target.target.model_id;
        if !connection.enabled {
            problems.push(format!("the connection `{name}` is disabled"));
        } else if let Some(owner) = model_family_conflict(connection.provider_id.as_str(), model) {
            problems.push(format!(
                "it sends the {owner} model `{model}` to the {} connection `{name}`",
                connection.provider_id.as_str()
            ));
        }
    }
    let usable = targets.len() - problems.len();
    Ok(if problems.is_empty() {
        Vec::new()
    } else if usable == 0 {
        vec![(
            true,
            format!(
                "the API route `{}` has no usable target: {}.",
                route.name,
                problems.join("; ")
            ),
        )]
    } else {
        problems
            .into_iter()
            .map(|problem| {
                (
                    false,
                    format!(
                        "a target of the API route `{}` will fail and fall back: {problem}.",
                        route.name
                    ),
                )
            })
            .collect()
    })
}
