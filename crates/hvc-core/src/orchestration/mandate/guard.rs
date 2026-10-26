//! Checks against the mandate in force (ADR-035 M1). Pure: the persistence
//! layer reads the mandate and the live values and turns a refusal into a
//! stage failure or a publication conflict.
//!
//! In pipeline v4 the frozen plan already keeps every write inside its
//! owners' destinations, so a refused write here is a host defect to
//! investigate, not an expected path.

use crate::orchestration::composition::{CheckKind, ExecutorKind};

use super::contract::{CheckCapability, ExecutionMandateV1};
use super::derive::executor_capabilities;

/// Every place that writes into a project workspace (inventory of Etapa 0).
/// Only stage output is a pulsation write: the other sources have their own
/// human approval, or are the developer, and never pass through the mandate.
/// Adding a source means classifying it here, or this does not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteSource {
    /// A stage's accepted artifact (`serial.rs`, outbox `job_class='main'`).
    PipelineStage,
    /// Learning report or knowledge-graph patch (`learning.rs`, background).
    BackgroundLearning,
    /// Brownfield component note (`component_notes.rs`, background).
    BackgroundComponent,
    /// Feature Forge publication (`feature_forge/publication.rs`).
    ForgePublication,
    /// Documentation reconciliation proposal (ADR-034 M5).
    DocReconciliation,
    /// The developer's own edit in the editor or the Vault.
    DeveloperEdit,
    /// Workspace bootstrap and stage-folder repair.
    HostBootstrap,
}

impl WriteSource {
    pub const ALL: [Self; 7] = [
        Self::PipelineStage,
        Self::BackgroundLearning,
        Self::BackgroundComponent,
        Self::ForgePublication,
        Self::DocReconciliation,
        Self::DeveloperEdit,
        Self::HostBootstrap,
    ];

    pub const fn checked_by_mandate(self) -> bool {
        match self {
            Self::PipelineStage => true,
            Self::BackgroundLearning
            | Self::BackgroundComponent
            | Self::ForgePublication
            | Self::DocReconciliation
            | Self::DeveloperEdit
            | Self::HostBootstrap => false,
        }
    }

    /// The source of a row of the publication outbox.
    pub fn of_publication(job_class: &str, kind: &str) -> Self {
        match (job_class, kind) {
            ("background", "component") => Self::BackgroundComponent,
            ("background", _) => Self::BackgroundLearning,
            _ => Self::PipelineStage,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteVerdict {
    /// No mandate governs the write: another source, a legacy pipeline or a
    /// pulsation from before the mandate existed.
    NotChecked,
    Allowed,
    Refused(String),
}

pub fn check_write(
    mandate: Option<&ExecutionMandateV1>,
    source: WriteSource,
    path: &str,
) -> WriteVerdict {
    if !source.checked_by_mandate() {
        return WriteVerdict::NotChecked;
    }
    let Some(mandate) = mandate.filter(|mandate| mandate.enforcement.is_enforced()) else {
        return WriteVerdict::NotChecked;
    };
    if mandate.grants_path(path) {
        WriteVerdict::Allowed
    } else {
        WriteVerdict::Refused(format!(
            "mandate_scope_violation: `{path}` is outside the write envelope of mandate revision {}.",
            mandate.revision
        ))
    }
}

fn transport_label(transport: &str, backend: Option<&str>) -> String {
    match backend {
        Some(backend) => format!("{transport}/{backend}"),
        None => transport.to_owned(),
    }
}

/// A role must run on the transport the approval recorded, or on the one an
/// approved CLI fallback moved it to.
pub fn check_transport(
    mandate: &ExecutionMandateV1,
    role: &str,
    transport: &str,
    backend: Option<&str>,
) -> Result<(), String> {
    if !mandate.enforcement.is_enforced() {
        return Ok(());
    }
    let Some(approved) = mandate.transport_for(role) else {
        return Err(format!(
            "mandate_capability_mismatch: mandate revision {} records no transport for role `{role}`.",
            mandate.revision
        ));
    };
    if approved.transport == transport && approved.backend.as_deref() == backend {
        return Ok(());
    }
    Err(format!(
        "mandate_capability_mismatch: role `{role}` was approved on {} and would now run on {}. Restore the agent's transport in Settings, or request a revision of the Specification to approve the new one.",
        transport_label(&approved.transport, approved.backend.as_deref()),
        transport_label(transport, backend)
    ))
}

/// An executor must still run under the policy the approval recorded. Only
/// a host upgrade in the middle of a pulsation can change it.
pub fn check_executor(mandate: &ExecutionMandateV1, kind: ExecutorKind) -> Result<(), String> {
    if !mandate.enforcement.is_enforced() {
        return Ok(());
    }
    let current = executor_capabilities()
        .into_iter()
        .find(|executor| executor.kind == kind);
    if mandate.executor(kind) == current.as_ref() {
        return Ok(());
    }
    Err(format!(
        "mandate_capability_mismatch: the host policy of `{}` changed after mandate revision {} was issued. Request a revision of the Specification to approve it.",
        kind.as_str(),
        mandate.revision
    ))
}

/// The checks a stage runs must be the approved ones: every approved check
/// that applies to the stage is still enabled with the same profile, and no
/// other check was enabled for it.
pub fn check_checks(
    mandate: &ExecutionMandateV1,
    applies: impl Fn(CheckKind) -> bool,
    current: &[CheckCapability],
) -> Result<(), String> {
    if !mandate.enforcement.is_enforced() {
        return Ok(());
    }
    let approved = mandate
        .capabilities
        .checks
        .iter()
        .filter(|check| applies(check.kind))
        .collect::<Vec<_>>();
    let mut changed = Vec::new();
    for check in &approved {
        if !current.iter().any(|now| {
            now.check_ref == check.check_ref && now.profile_sha256 == check.profile_sha256
        }) {
            changed.push(check.check_ref.clone());
        }
    }
    for now in current {
        if !approved
            .iter()
            .any(|check| check.check_ref == now.check_ref)
        {
            changed.push(now.check_ref.clone());
        }
    }
    if changed.is_empty() {
        return Ok(());
    }
    changed.sort();
    changed.dedup();
    Err(format!(
        "mandate_capability_mismatch: validation profile(s) {} changed after mandate revision {} was issued. Restore them in Settings, or request a revision of the Specification to approve the change.",
        changed
            .iter()
            .map(|check| format!("`{check}`"))
            .collect::<Vec<_>>()
            .join(", "),
        mandate.revision
    ))
}
