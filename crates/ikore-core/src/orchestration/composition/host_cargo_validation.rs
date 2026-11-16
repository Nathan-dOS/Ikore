//! ADR-033 host-only policy, frozen admission context and executable evidence.
//! The existing model profile and receipt V1 JSON contracts remain unchanged.

use super::composition_persistence::{ExecutionRow, composition_invalid};
use super::{Connection, OptionalExtension, PersistenceService, PersistenceServiceError, params};
use crate::orchestration::composition::validation::candidate::{
    Candidate, capture_baseline_excluding, load_baseline,
};
use crate::orchestration::composition::validation::cargo_policy::is_qualified_cargo_profile;
use crate::orchestration::composition::validation::profile::ValidationProfileSnapshot;
use crate::orchestration::composition::validation::profile::{
    ProfileOrigin, resolve_cargo_profile, resolve_profile,
};
use crate::orchestration::composition::validation::runner::{
    ReceiptContext, effective_environment_digest,
};
use crate::orchestration::composition::validation::{
    CargoValidationPolicyV1, RunEvidenceDetailsV1,
};
use crate::orchestration::composition::{
    self as comp, Sha256Hex, Validate, ValidationCommandProfileV1,
};
use rusqlite::TransactionBehavior;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub(crate) fn cargo_cache_environment(
    root: &Path,
    project_id: &str,
    snapshot: &ValidationProfileSnapshot,
) -> Vec<(String, String)> {
    let project_key = Sha256Hex::of(project_id.as_bytes());
    vec![(
        "CARGO_TARGET_DIR".into(),
        root.join("cache")
            .join(project_key.as_str())
            .join(snapshot.toolchain_sha256.as_str())
            .join("target")
            .to_string_lossy()
            .into_owned(),
    )]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct FrozenValidationProfile {
    pub snapshot_id: comp::Uuid,
    pub snapshot: ValidationProfileSnapshot,
    pub environment_sha256: Sha256Hex,
}

impl PersistenceService {
    pub(crate) fn generated_validation_paths(
        db: &Connection,
        project_id: &str,
    ) -> Result<Vec<String>, PersistenceServiceError> {
        let mut statement = db.prepare(
            "SELECT DISTINCT a.relative_path FROM artifacts a JOIN pulsations p ON p.id=a.pulsation_id
             WHERE p.project_id=?1 AND a.relative_path IS NOT NULL
             AND a.kind IN ('specification','validation_report','intake_note','contract','integration_report') ORDER BY a.relative_path",
        )?;
        let paths = statement
            .query_map([project_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for path in &paths {
            comp::RelPath::new(path.clone()).map_err(|e| composition_invalid(&e))?;
        }
        Ok(paths)
    }

    /// Re-check current configuration, real tools and a fresh byte capture. This
    /// observes evidence only: it never executes Cargo or blesses an old run.
    pub(crate) async fn inspect_cargo_receipt_freshness(
        &self,
        root: &Path,
        project_id: &str,
        profile: &ValidationCommandProfileV1,
        policy: &CargoValidationPolicyV1,
        receipt: &comp::ValidationReceiptV1,
    ) -> Result<(super::composition_coordinator::CargoFreshness, String), PersistenceServiceError>
    {
        use super::composition_coordinator::CargoFreshness;
        let (old, details) = {
            let db = self.connection()?;
            let json = Self::policy_snapshot_json(&db, receipt.profile_snapshot_id.as_str())?;
            let old: ValidationProfileSnapshot = serde_json::from_value(json)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
            let details = Self::validation_evidence_on(&db, receipt.receipt_id.as_str())?;
            (old, details)
        };
        let policy_sha256 =
            comp::canonical_sha256(policy, &[]).map_err(|e| composition_invalid(&e))?;
        if details.as_ref().is_none_or(|d| {
            !d.executed()
                || d.policy_sha256 != policy_sha256
                || d.parser_version != policy.parser_version
        }) {
            return Ok((CargoFreshness::Unverifiable, "Qualified execution details are absent, inconclusive or refer to a different policy.".into()));
        }
        if old.cargo_policy.as_ref() != Some(policy)
            || old.profile != *profile
            || old.profile_sha256 != receipt.profile_sha256
            || old.toolchain_sha256 != receipt.toolchain_sha256
        {
            return Ok((
                CargoFreshness::Stale,
                "The approved command or Cargo input policy changed after the receipt.".into(),
            ));
        }
        let cancellation = CancellationToken::new();
        let current =
            resolve_cargo_profile(profile, ProfileOrigin::Preset, policy, root, &cancellation)
                .await
                .map_err(|e| composition_invalid(&e))?;
        if current.cargo_unavailable_reason.is_some() {
            return Ok((
                CargoFreshness::Unverifiable,
                "The current installed toolchain cannot be verified.".into(),
            ));
        }
        if current.toolchain_sha256 != receipt.toolchain_sha256
            || current.profile_sha256 != receipt.profile_sha256
        {
            return Ok((
                CargoFreshness::Stale,
                "The toolchain or command identity changed after the receipt.".into(),
            ));
        }
        let candidates_root = self
            .database_path()
            .parent()
            .ok_or_else(|| {
                PersistenceServiceError::Runtime("Validation data directory unavailable.".into())
            })?
            .join("stage_candidates");
        let context = ReceiptContext {
            stage_execution_id: receipt.stage_execution_id.clone(),
            parent_generation: receipt.parent_generation,
            profile_snapshot_id: receipt.profile_snapshot_id.clone(),
            extra_environment: cargo_cache_environment(&candidates_root, project_id, &current),
        };
        if effective_environment_digest(&current, &context) != receipt.environment_sha256 {
            return Ok((
                CargoFreshness::Stale,
                "The effective execution environment changed after the receipt.".into(),
            ));
        }
        let directory = tempfile::Builder::new()
            .prefix("integrator-cargo-")
            .tempdir_in(&candidates_root)
            .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
        let source = root.to_path_buf();
        let destination = directory.path().join("current");
        // The shared candidate may contain assets approved by other checks of
        // this same execution. Reconstruct that exact frozen union as well.
        let additional_inputs = Self::frozen_validation_context(
            &self.connection()?,
            receipt.stage_execution_id.as_str(),
        )?
        .map(|(_, profiles)| {
            profiles
                .into_iter()
                .filter_map(|p| p.snapshot.cargo_policy)
                .flat_map(|p| p.additional_inputs)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect()
        })
        .unwrap_or_else(|| policy.additional_inputs.clone());
        let excluded_paths = Self::generated_validation_paths(&self.connection()?, project_id)?;
        let candidate = tokio::task::spawn_blocking(move || {
            capture_baseline_excluding(&source, &destination, &additional_inputs, &excluded_paths)
        })
        .await
        .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
        match candidate {
            Ok(candidate)
                if candidate.candidate_sha256 == receipt.candidate_sha256
                    && candidate
                        .input_manifest_sha256(&profile.input_globs)
                        .map_err(|e| composition_invalid(&e))?
                        == receipt.input_manifest_sha256 =>
            {
                Ok((CargoFreshness::Fresh, String::new()))
            }
            Ok(_) => Ok((
                CargoFreshness::Stale,
                "Candidate inputs or declared assets changed after the receipt.".into(),
            )),
            Err(_) => Ok((
                CargoFreshness::Unverifiable,
                "Current candidate inputs could not be completely captured.".into(),
            )),
        }
    }

    pub fn get_validation_receipt(
        &self,
        receipt_id: &str,
    ) -> Result<Option<comp::ValidationReceiptV1>, PersistenceServiceError> {
        let json: Option<String> = self
            .connection()?
            .query_row(
                "SELECT receipt_json FROM host_validation_runs WHERE id=?1",
                [receipt_id],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|s| {
            serde_json::from_str(&s).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))
        })
        .transpose()
    }

    pub fn get_cargo_validation_policy(
        &self,
        project_id: &str,
        check_ref: &str,
    ) -> Result<Option<CargoValidationPolicyV1>, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        Self::cargo_validation_policy_on(&self.connection()?, &project_id, check_ref)
    }

    pub(crate) fn cargo_validation_policy_on(
        db: &Connection,
        project_id: &str,
        check_ref: &str,
    ) -> Result<Option<CargoValidationPolicyV1>, PersistenceServiceError> {
        // A profile edited through an older API cannot silently retain an approval
        // for a different version. Such a configuration must be reviewed again.
        let json: Option<String> = db.query_row(
            "SELECT p.policy_json FROM project_cargo_validation_policies p
             JOIN project_validation_profiles v ON v.project_id=p.project_id AND v.check_ref=p.check_ref
             WHERE p.project_id=?1 AND p.check_ref=?2 AND p.profile_version=v.profile_version",
            params![project_id, check_ref], |r| r.get(0),
        ).optional()?;
        json.map(|s| {
            serde_json::from_str(&s).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))
        })
        .transpose()
    }

    /// Saves command and approved input policy in one transaction. Changes to
    /// either increment the profile revision; toggling alone preserves it.
    pub fn save_cargo_validation_profile(
        &self,
        project_id: &str,
        mut profile: ValidationCommandProfileV1,
        policy: CargoValidationPolicyV1,
        enabled: bool,
    ) -> Result<ValidationCommandProfileV1, PersistenceServiceError> {
        let project_id = Self::normalize_project_id(project_id)?;
        profile.profile_version = profile.profile_version.max(2);
        profile.validate().map_err(|e| composition_invalid(&e))?;
        policy.validate().map_err(|e| composition_invalid(&e))?;
        if !is_qualified_cargo_profile(&profile) || profile.resolved_wrapper_hint().is_some() {
            return Err(PersistenceServiceError::Validation(
                "A Cargo policy requires a direct Cargo validation profile.".into(),
            ));
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if Self::find_project_by_id(&tx, &project_id, false)?.is_none() {
            return Err(PersistenceServiceError::NotFound(
                "Project not found to configure Cargo validation.".into(),
            ));
        }
        let previous: Option<String> = tx.query_row(
            "SELECT profile_json FROM project_validation_profiles WHERE project_id=?1 AND check_ref=?2",
            params![project_id, profile.check_ref.as_str()], |r| r.get(0),
        ).optional()?;
        if let Some(json) = previous {
            let old: ValidationCommandProfileV1 = serde_json::from_str(&json)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?;
            let old_policy =
                Self::cargo_validation_policy_on(&tx, &project_id, profile.check_ref.as_str())?;
            let mut comparable = profile.clone();
            comparable.profile_version = old.profile_version;
            profile.profile_version = if comparable == old && old_policy.as_ref() == Some(&policy) {
                old.profile_version
            } else {
                old.profile_version
                    .checked_add(1)
                    .filter(|v| *v <= i32::MAX as u32)
                    .ok_or_else(|| {
                        PersistenceServiceError::Validation(
                            "Validation profile version limit reached.".into(),
                        )
                    })?
            };
        }
        let now = Self::now_iso();
        tx.execute(
            "INSERT INTO project_validation_profiles(project_id,check_ref,profile_version,enabled,profile_json,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?6)
             ON CONFLICT(project_id,check_ref) DO UPDATE SET profile_version=excluded.profile_version,enabled=excluded.enabled,profile_json=excluded.profile_json,updated_at=excluded.updated_at",
            params![project_id, profile.check_ref.as_str(), profile.profile_version, enabled,
                serde_json::to_string(&profile).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?, now],
        )?;
        tx.execute(
            "INSERT INTO project_cargo_validation_policies(project_id,check_ref,profile_version,policy_json,updated_at)
             VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(project_id,check_ref) DO UPDATE SET profile_version=excluded.profile_version,policy_json=excluded.policy_json,updated_at=excluded.updated_at",
            params![project_id, profile.check_ref.as_str(), profile.profile_version,
                serde_json::to_string(&policy).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?, now],
        )?;
        tx.commit()?;
        Ok(profile)
    }

    pub fn get_validation_evidence(
        &self,
        receipt_id: &str,
    ) -> Result<Option<RunEvidenceDetailsV1>, PersistenceServiceError> {
        Self::validation_evidence_on(&self.connection()?, receipt_id)
    }

    pub(crate) fn validation_evidence_on(
        db: &Connection,
        receipt_id: &str,
    ) -> Result<Option<RunEvidenceDetailsV1>, PersistenceServiceError> {
        let json: Option<String> = db
            .query_row(
                "SELECT details_json FROM host_validation_run_details WHERE receipt_id=?1",
                [receipt_id],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|s| {
            serde_json::from_str(&s).map_err(|e| PersistenceServiceError::Runtime(e.to_string()))
        })
        .transpose()
    }

    pub(crate) fn insert_validation_evidence(
        db: &Connection,
        details: &RunEvidenceDetailsV1,
    ) -> Result<(), PersistenceServiceError> {
        db.execute(
            "INSERT INTO host_validation_run_details(receipt_id,details_json) VALUES(?1,?2)",
            params![
                details.receipt_id.as_str(),
                serde_json::to_string(details)
                    .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?
            ],
        )?;
        Ok(())
    }

    pub(crate) fn frozen_validation_context(
        db: &Connection,
        execution_id: &str,
    ) -> Result<Option<(Sha256Hex, Vec<FrozenValidationProfile>)>, PersistenceServiceError> {
        let row: Option<(String, String)> = db.query_row(
            "SELECT baseline_sha256,profiles_json FROM stage_cargo_validation_contexts WHERE execution_id=?1",
            [execution_id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        row.map(|(sha, json)| {
            Ok((
                Sha256Hex::new(sha).map_err(|e| composition_invalid(&e))?,
                serde_json::from_str(&json)
                    .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?,
            ))
        })
        .transpose()
    }

    pub(crate) fn insert_frozen_validation_context(
        db: &Connection,
        execution: &ExecutionRow,
        baseline_sha256: &Sha256Hex,
        profiles: &[FrozenValidationProfile],
    ) -> Result<(), PersistenceServiceError> {
        db.execute(
            "INSERT INTO stage_cargo_validation_contexts(execution_id,baseline_sha256,profiles_json,created_at) VALUES(?1,?2,?3,?4)",
            params![execution.id, baseline_sha256.as_str(), serde_json::to_string(profiles)
                .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?, Self::now_iso()],
        )?;
        Ok(())
    }

    /// Capture once before dispatch. A recovered execution must keep its original
    /// inputs and approved profiles, even when project settings changed meanwhile.
    pub(crate) async fn freeze_cargo_validation_context(
        &self,
        execution: &ExecutionRow,
        fence: &crate::orchestration::ExecutionFence,
        project: &super::Project,
        profiles: &[ValidationCommandProfileV1],
        cancellation: &CancellationToken,
    ) -> Result<(), PersistenceServiceError> {
        if profiles.is_empty() {
            return Ok(());
        }
        let context = Self::frozen_validation_context(&self.connection()?, &execution.id)?;
        let data_dir = self.database_path().parent().ok_or_else(|| {
            PersistenceServiceError::Runtime("Validation data directory unavailable.".into())
        })?;
        let candidates_root = data_dir.join("stage_candidates");
        let baseline_dir = candidates_root.join(&execution.id).join("baseline");
        if let Some((expected, _)) = context {
            let baseline = load_baseline(&baseline_dir).map_err(|e| composition_invalid(&e))?;
            if baseline.candidate_sha256 != expected {
                return Err(PersistenceServiceError::Validation("The frozen validation baseline is corrupt; it cannot be recaptured from the live checkout.".into()));
            }
            return Ok(());
        }
        let policies = {
            let db = self.connection()?;
            profiles.iter().map(|profile| {
                let policy = Self::cargo_validation_policy_on(&db, &project.id, profile.check_ref.as_str())?;
                if is_qualified_cargo_profile(profile) && policy.is_none() {
                    return Err(PersistenceServiceError::Validation(format!("Review and save the Cargo input policy for `{}` before enabling this profile.", profile.check_ref)));
                }
                Ok(policy)
            }).collect::<Result<Vec<_>, PersistenceServiceError>>()?
        };
        if policies.iter().all(Option::is_none) {
            return Ok(());
        }
        let additional: Vec<String> = policies
            .iter()
            .flatten()
            .flat_map(|policy| policy.additional_inputs.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let source = std::path::PathBuf::from(&project.workspace_path);
        let target = baseline_dir.clone();
        let excluded_paths = Self::generated_validation_paths(&self.connection()?, &project.id)?;
        let excluded_keys = excluded_paths
            .iter()
            .map(|path| comp::RelPath::new(path.clone()).map(|path| path.canonical_key()))
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
            .map_err(|e| composition_invalid(&e))?;
        let baseline: Candidate = tokio::task::spawn_blocking(move || {
            capture_baseline_excluding(&source, &target, &additional, &excluded_paths)
        })
        .await
        .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?
        .map_err(|e| composition_invalid(&e))?;
        let captured = baseline
            .manifest
            .iter()
            .map(|(path, sha)| {
                comp::RelPath::new(path.clone()).map(|path| (path.canonical_key(), sha))
            })
            .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
            .map_err(|e| composition_invalid(&e))?;
        for reference in &execution.plan.references {
            if let comp::ReferenceEntryV1::ExistingSource {
                relative_path,
                content_sha256,
                ..
            } = reference
            {
                let key = relative_path.canonical_key();
                if !excluded_keys.contains(&key)
                    && captured.get(&key).copied() != Some(content_sha256)
                {
                    return Err(PersistenceServiceError::Validation(
                        "A source changed between stage preparation and candidate capture, or was not captured; start a new execution after reviewing its inputs.".into(),
                    ));
                }
            }
        }
        let mut frozen = Vec::new();
        for (profile, policy) in profiles.iter().zip(policies) {
            if cancellation.is_cancelled() {
                return Err(PersistenceServiceError::Cancelled(
                    "Validation preparation cancelled before dispatch.".into(),
                ));
            }
            let snapshot = match policy {
                Some(policy) => {
                    resolve_cargo_profile(
                        profile,
                        ProfileOrigin::Preset,
                        &policy,
                        &baseline.dir,
                        cancellation,
                    )
                    .await
                }
                None => resolve_profile(profile, ProfileOrigin::Custom),
            }
            .map_err(|e| composition_invalid(&e))?;
            let (id, _) = Self::snapshot_policy(
                &self.connection()?,
                "validation_profile",
                Some(&project.id),
                &serde_json::to_value(&snapshot)
                    .map_err(|e| PersistenceServiceError::Runtime(e.to_string()))?,
            )?;
            let snapshot_id = comp::Uuid::new(id).map_err(|e| composition_invalid(&e))?;
            let context = ReceiptContext {
                stage_execution_id: comp::Uuid::new(execution.id.clone())
                    .map_err(|e| composition_invalid(&e))?,
                parent_generation: execution.parent_generation as u32,
                profile_snapshot_id: snapshot_id.clone(),
                extra_environment: cargo_cache_environment(
                    &candidates_root,
                    &project.id,
                    &snapshot,
                ),
            };
            let environment_sha256 = effective_environment_digest(&snapshot, &context);
            frozen.push(FrozenValidationProfile {
                snapshot_id,
                snapshot,
                environment_sha256,
            });
        }
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::check_execution(&tx, fence)?;
        Self::insert_frozen_validation_context(
            &tx,
            execution,
            &baseline.candidate_sha256,
            &frozen,
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::composition::validation::profile::detect_presets;

    fn project() -> (tempfile::TempDir, PersistenceService) {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname=\"fixture\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();
        let service = PersistenceService::initialize(&root.path().join("data")).unwrap();
        service.connection().unwrap().execute(
            "INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('cargo-project','Cargo',?1,?2,?2)",
            params![root.path().to_string_lossy(), PersistenceService::now_iso()],
        ).unwrap();
        (root, service)
    }

    #[test]
    fn cargo_policy_changes_version_atomically_and_survives_restart() {
        let (root, service) = project();
        let profile = detect_presets(root.path()).remove(0);
        let first = service
            .save_cargo_validation_profile(
                "cargo-project",
                profile,
                CargoValidationPolicyV1::default(),
                false,
            )
            .unwrap();
        assert!(!service.list_validation_profiles("cargo-project").unwrap()[0].1);
        let same = service
            .save_cargo_validation_profile(
                "cargo-project",
                first.clone(),
                CargoValidationPolicyV1::default(),
                true,
            )
            .unwrap();
        assert_eq!(same.profile_version, first.profile_version);
        let mut policy = CargoValidationPolicyV1::default();
        policy.additional_inputs.push("assets/*.ico".into());
        let next = service
            .save_cargo_validation_profile("cargo-project", same, policy.clone(), true)
            .unwrap();
        assert_eq!(next.profile_version, first.profile_version + 1);
        drop(service);
        let service = PersistenceService::initialize(&root.path().join("data")).unwrap();
        assert_eq!(
            service
                .get_cargo_validation_policy("cargo-project", "cargo_check")
                .unwrap(),
            Some(policy)
        );
        service
            .delete_validation_profile("cargo-project", "cargo_check")
            .unwrap();
        assert!(
            service
                .get_cargo_validation_policy("cargo-project", "cargo_check")
                .unwrap()
                .is_none()
        );
    }
}
