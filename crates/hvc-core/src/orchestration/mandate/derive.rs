//! Pure derivation of mandate revisions. The persistence layer gathers the
//! inputs in the approving transaction; nothing here reads a clock, a file or
//! the database, so the brief's preview and the issued revision agree byte for
//! byte on the same inputs.
//!
//! The write envelope is every path a v4 stage can come to own, computed from
//! the same sources `collect_stage_inputs` and the recipes use: the frozen
//! batch (brief sources, task paths), clarification answers, the approved
//! Specification's references, the host-rendered stage notes and the derived
//! test destination. A file a stage publishes later is a source for the next
//! stage, and it entered the envelope the same way, so the envelope is closed
//! over the pulsation's own publications.

use std::collections::BTreeMap;

use crate::feature_forge::BatchManifest;
use crate::orchestration::ContentSha256;
use crate::orchestration::composition::policy::{policy_for, recipe_policy_sha256};
use crate::orchestration::composition::recipes::{
    SourceFile, StagePayloadRecord, default_test_destination, explicit_paths, role_for_path,
    specification_of,
};
use crate::orchestration::composition::references::vault_note_path;
use crate::orchestration::composition::review_policy::is_review_policy_path;
use crate::orchestration::composition::{ExecutorKind, RelPath, SourceRole, canonical_json};

use super::contract::{
    BudgetParameters, CheckCapability, Enforcement, ExecutionMandateV1, ExecutorCapability,
    IssuedBy, MANDATE_SCHEMA_VERSION, MandateCapabilities, MandateCause, MandateIntent,
    MandateScope, PROTECTED_RULES_VERSION, ProtectedRules, TaskRevisionRef, TransportCapability,
    WRITE_CLASS_POLICY_VERSION, WriteClass, WriteGrant, path_key,
};

/// Everything a first approval (or a re-approval) of the Specification needs,
/// gathered by the host in the approving transaction.
#[derive(Debug, Clone)]
pub struct IssueInputs<'a> {
    pub pulsation_id: &'a str,
    pub project_id: &'a str,
    pub enforcement: Enforcement,
    /// The pulsation's feature name; stage notes and the default test file
    /// are named after it.
    pub title: &'a str,
    pub issued_by: IssuedBy,
    /// The latest revision, when this approval re-issues the mandate.
    pub previous: Option<(&'a ExecutionMandateV1, &'a str)>,
    pub manifest: Option<&'a BatchManifest>,
    /// Paths clarification answers planned or named as modified.
    pub clarification_paths: &'a [String],
    /// The Specification payload the approval accepts (v4 only).
    pub specification: Option<&'a StagePayloadRecord>,
    /// Content digests of the artifacts the approval accepts.
    pub accepted_artifact_sha256s: &'a [String],
    pub review_policy_snapshot_sha256: Option<String>,
    pub project_sensitive_sha256: Option<String>,
    pub transports: Vec<TransportCapability>,
    pub checks: Vec<CheckCapability>,
    pub budget: BudgetParameters,
}

/// Revision 1 on a first approval, the next revision with cause `reissued`
/// on a later one. Amendments of an earlier revision are not inherited: the
/// new Specification is a new approval.
pub fn derive_issued(inputs: &IssueInputs<'_>) -> Result<ExecutionMandateV1, String> {
    let (revision, previous_sha256, cause) = match inputs.previous {
        Some((previous, sha)) => (
            previous.revision + 1,
            Some(sha.to_owned()),
            MandateCause::Reissued,
        ),
        None => (1, None, MandateCause::Issued),
    };
    let grants = if inputs.enforcement.is_enforced() {
        derive_grants(inputs)?
    } else {
        Vec::new()
    };
    let task_revisions = match inputs.manifest {
        Some(manifest) => task_revisions(manifest)?,
        None => Vec::new(),
    };
    let mut transports = inputs.transports.clone();
    transports.sort_by(|a, b| a.role.cmp(&b.role));
    let mut checks = inputs.checks.clone();
    checks.sort_by(|a, b| a.check_ref.cmp(&b.check_ref));
    Ok(ExecutionMandateV1 {
        schema_version: MANDATE_SCHEMA_VERSION,
        pulsation_id: inputs.pulsation_id.to_owned(),
        project_id: inputs.project_id.to_owned(),
        revision,
        previous_sha256,
        cause,
        issued_by: inputs.issued_by.clone(),
        enforcement: inputs.enforcement,
        intent: MandateIntent {
            task_revisions,
            specification_sha256: digest_of_set(inputs.accepted_artifact_sha256s)?,
            review_policy_snapshot_sha256: inputs.review_policy_snapshot_sha256.clone(),
        },
        scope: MandateScope {
            grants,
            write_class_policy_version: WRITE_CLASS_POLICY_VERSION,
            protected_rules: ProtectedRules {
                builtin_version: PROTECTED_RULES_VERSION,
                project_sha256: inputs.project_sensitive_sha256.clone(),
            },
        },
        capabilities: MandateCapabilities {
            executors: executor_capabilities(),
            transports,
            recipe_policy_sha256: recipe_policy_sha256()
                .map_err(|error| error.message)?
                .as_str()
                .to_owned(),
            checks,
        },
        budget: inputs.budget.clone(),
        expires_at: None,
    })
}

/// The next revision after an approved CLI fallback moved `transport.role`
/// to another transport. Everything else is carried over unchanged.
pub fn amend_transport(
    previous: &ExecutionMandateV1,
    previous_sha256: &str,
    transport: TransportCapability,
    issued_by: IssuedBy,
) -> ExecutionMandateV1 {
    let mut next = previous.clone();
    next.revision = previous.revision + 1;
    next.previous_sha256 = Some(previous_sha256.to_owned());
    next.cause = MandateCause::TransportFallback;
    next.issued_by = issued_by;
    next.capabilities
        .transports
        .retain(|existing| existing.role != transport.role);
    next.capabilities.transports.push(transport);
    next.capabilities
        .transports
        .sort_by(|a, b| a.role.cmp(&b.role));
    next
}

/// The capability every executor kind has under the current host policies.
pub fn executor_capabilities() -> Vec<ExecutorCapability> {
    let mut executors = ExecutorKind::ALL
        .iter()
        .map(|kind| {
            let policy = policy_for(*kind);
            ExecutorCapability {
                kind: *kind,
                edit_operations: policy.allowed_edit_operations.to_vec(),
                writable_roles: policy.writable_roles.to_vec(),
                tool_policy: policy.tools,
                context_scope: policy.tools.context_scope(),
            }
        })
        .collect::<Vec<_>>();
    executors.sort_by_key(|executor| executor.kind);
    executors
}

/// Digest of a set of digests: order-independent, duplicates kept once.
/// Each frozen task with the digest of its canonical draft, by task id. The
/// mandate and the attestation (ADR-036) record the same digests.
pub fn task_revisions(manifest: &BatchManifest) -> Result<Vec<TaskRevisionRef>, String> {
    let mut revisions = Vec::new();
    for task in &manifest.tasks {
        let draft = serde_json::to_value(&task.draft).map_err(|error| error.to_string())?;
        let canonical = canonical_json(&draft).map_err(|error| error.message)?;
        revisions.push(TaskRevisionRef {
            task_id: task.id.clone(),
            revision: task.revision,
            sha256: ContentSha256::of(canonical.as_bytes()).as_str().to_owned(),
        });
    }
    revisions.sort_by(|a, b| a.task_id.cmp(&b.task_id));
    Ok(revisions)
}

pub fn digest_of_set(digests: &[String]) -> Result<String, String> {
    let mut sorted = digests.to_vec();
    sorted.sort();
    sorted.dedup();
    let canonical = canonical_json(&serde_json::Value::from(sorted)).map_err(|e| e.message)?;
    Ok(ContentSha256::of(canonical.as_bytes()).as_str().to_owned())
}

fn add_grant(grants: &mut BTreeMap<String, WriteGrant>, path: &RelPath, origin: WriteClass) {
    // A review policy is never a destination of any stage (ADR-032).
    if is_review_policy_path(path.as_str()) {
        return;
    }
    let entry = grants
        .entry(path_key(path.as_str()))
        .or_insert_with(|| WriteGrant {
            path: path.as_str().to_owned(),
            origins: Vec::new(),
            unit_key: None,
            amendment_revision: None,
        });
    if !entry.origins.contains(&origin) {
        entry.origins.push(origin);
        entry.origins.sort();
    }
}

fn derive_grants(inputs: &IssueInputs<'_>) -> Result<Vec<WriteGrant>, String> {
    let mut grants: BTreeMap<String, WriteGrant> = BTreeMap::new();
    let add = &mut grants;
    if let Some(manifest) = inputs.manifest {
        for feature in &manifest.features {
            for source in &feature.brief.sources {
                if let Ok(path) = RelPath::new(source.relative_path.as_str()) {
                    add_grant(add, &path, WriteClass::BriefSource);
                }
            }
        }
        for task in &manifest.tasks {
            let text = format!("{} {}", task.draft.title, task.draft.description);
            for path in explicit_paths(&text) {
                add_grant(add, &path, WriteClass::TaskPath);
            }
        }
    }
    for raw in inputs.clarification_paths {
        if let Ok(path) = RelPath::new(raw.as_str()) {
            add_grant(add, &path, WriteClass::ClarificationPath);
        }
    }
    if let Some(record) = inputs.specification
        && let Some(spec) = specification_of(record)
    {
        let handles = spec
            .existing_file_refs
            .iter()
            .chain(&spec.planned_file_refs)
            .chain(
                spec.unit_proposals
                    .iter()
                    .flat_map(|proposal| &proposal.destination_refs),
            );
        for handle in handles {
            if let Some(path) = record.path_of(handle) {
                add_grant(add, path, WriteClass::SpecificationReference);
            }
        }
    }
    for folder in ["features", "tests"] {
        let path = vault_note_path(folder, inputs.title).map_err(|error| error.message)?;
        add_grant(add, &path, WriteClass::StageDocument);
    }
    // `default_test_destination` takes the extension of the first source the
    // Test stage receives; any source-role path of the envelope may be that
    // one, so each one's destination is granted.
    let sources = grants
        .values()
        .filter_map(|grant| RelPath::new(grant.path.as_str()).ok())
        .filter(|path| role_for_path(path) == SourceRole::Source)
        .collect::<Vec<_>>();
    for source in sources {
        if let Some(destination) =
            default_test_destination(inputs.title, &[SourceFile::baseline(source, "")])
        {
            add_grant(&mut grants, &destination, WriteClass::TestDestination);
        }
    }
    Ok(grants.into_values().collect())
}
