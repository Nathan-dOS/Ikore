//! Validation command profiles: developer-owned commands, detected presets
//! and the immutable snapshot a receipt binds to (§1 of the validation
//! contract).
//!
//! A profile names an executable and an argument vector; the host launches
//! it directly, never through a shell built from model output. Presets are
//! detected from the project's own manifests and only ever point at tools
//! already installed on the machine - nothing is downloaded or installed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::cargo_policy::{
    CARGO_CHECK_ARGUMENTS, CARGO_TEST_ARGUMENTS, CargoToolchainSnapshot, CargoValidationPolicyV1,
    installed_toolchain,
};

use super::super::{
    CheckKind, CompositionError, ErrorCode, Handle, Invariant, Sha256Hex, Uuid,
    ValidationCommandProfileV1, canonical_sha256,
};
use crate::orchestration::SchemaV1;

/// Default and maximum command timeouts (seconds) from the contract.
pub const DEFAULT_TIMEOUT_SECONDS: u32 = 180;
pub const MAX_TIMEOUT_SECONDS: u32 = 900;

/// Where a profile came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileOrigin {
    /// Detected from the project's manifests (`Cargo.toml`, ...).
    Preset,
    /// Configured by the developer.
    Custom,
}

/// The immutable, host-only snapshot of a profile as it was when a stage
/// was admitted: the contract profile plus the resolved executable identity.
/// Never carries secret values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValidationProfileSnapshot {
    pub profile: ValidationCommandProfileV1,
    pub origin: ProfileOrigin,
    /// Canonical absolute path of the executable that was resolved, or
    /// `null` when it could not be found (the check then reports
    /// `unavailable`; a missing tool is never installed by the host).
    pub resolved_executable: Option<String>,
    pub resolved_executable_sha256: Option<Sha256Hex>,
    /// Digest of the toolchain identity and the resolved executable bytes.
    pub toolchain_sha256: Sha256Hex,
    /// Digest of the profile fields the receipt binds to.
    pub profile_sha256: Sha256Hex,
    /// Absent for legacy/custom profiles, whose original interpretation is
    /// retained until a developer explicitly upgrades the profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_policy: Option<CargoValidationPolicyV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_toolchain: Option<CargoToolchainSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_unavailable_reason: Option<String>,
    /// Only host-selected installation/home paths and tool selection. Project
    /// variable values remain in the runner's in-memory environment snapshot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effective_environment: Vec<(String, String)>,
}

fn error(code: ErrorCode, message: impl Into<String>) -> CompositionError {
    CompositionError::new(Invariant::Sc011, code, message)
}

/// Resolves a profile into its immutable snapshot. Shell wrappers and
/// shortcuts are never launch targets; a direct executable is canonicalized
/// and hashed so the receipt proves which bytes ran.
pub fn resolve_profile(
    profile: &ValidationCommandProfileV1,
    origin: ProfileOrigin,
) -> Result<ValidationProfileSnapshot, CompositionError> {
    profile.validate()?;
    let profile_sha256 = canonical_sha256(profile, &[])?;
    let executable = find_executable(&profile.executable);
    let (resolved_executable, resolved_executable_sha256) = match executable {
        Some(path) => {
            reject_wrapper(&path)?;
            let sha = crate::terminals::hash_launch_files(&[&path])
                .map_err(|e| error(ErrorCode::ValidationUnavailable, e.to_string()))?;
            (
                Some(path.to_string_lossy().into_owned()),
                Some(Sha256Hex::new(sha)?),
            )
        }
        None => (None, None),
    };
    let toolchain_sha256 = Sha256Hex::of(
        format!(
            "{}\n{}",
            profile.toolchain_identity,
            resolved_executable_sha256
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("unresolved")
        )
        .as_bytes(),
    );
    Ok(ValidationProfileSnapshot {
        profile: profile.clone(),
        origin,
        resolved_executable,
        resolved_executable_sha256,
        toolchain_sha256,
        profile_sha256,
        cargo_policy: None,
        cargo_toolchain: None,
        cargo_unavailable_reason: None,
        effective_environment: Vec::new(),
    })
}

/// Resolve the qualified Cargo profile against already installed real tools.
/// Proxies are never launched, including for a missing project toolchain pin.
/// Missing tools are retained as an unavailable snapshot, so the runner can
/// produce an inspectable receipt rather than losing project configuration.
pub async fn resolve_cargo_profile(
    profile: &ValidationCommandProfileV1,
    origin: ProfileOrigin,
    policy: &CargoValidationPolicyV1,
    root: &Path,
    cancellation: &CancellationToken,
) -> Result<ValidationProfileSnapshot, CompositionError> {
    profile.validate()?;
    policy.validate()?;
    // resolve_profile would hash whichever PATH proxy it found. The qualified
    // path instead binds Cargo, rustc and rustdoc from one installed toolchain.
    let mut snapshot = ValidationProfileSnapshot {
        profile: profile.clone(),
        origin,
        resolved_executable: None,
        resolved_executable_sha256: None,
        toolchain_sha256: Sha256Hex::of(b"cargo-toolchain-unavailable"),
        profile_sha256: canonical_sha256(&(profile, policy), &[])?,
        cargo_policy: Some(policy.clone()),
        cargo_toolchain: None,
        cargo_unavailable_reason: None,
        effective_environment: Vec::new(),
    };
    if cancellation.is_cancelled() {
        snapshot.cargo_unavailable_reason =
            Some("Cargo toolchain resolution was cancelled.".into());
        return Ok(snapshot);
    }
    let owned_root = root.to_path_buf();
    let owned_policy = policy.clone();
    let resolution =
        tokio::task::spawn_blocking(move || installed_toolchain(&owned_root, &owned_policy));
    let installed = tokio::select! {
        _ = cancellation.cancelled() => {
            snapshot.cargo_unavailable_reason = Some("Cargo toolchain resolution was cancelled.".into());
            return Ok(snapshot);
        }
        result = tokio::time::timeout(std::time::Duration::from_secs(10), resolution) => match result {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("The installed Cargo toolchain could not be inspected.".into()),
            Err(_) => Err("Inspecting the installed Cargo toolchain exceeded 10 seconds.".into()),
        },
    };
    match installed {
        Ok((toolchain, environment)) => {
            // A qualified preset uses the selected real Cargo, never a shell
            // proxy. An explicitly configured executable must identify those
            // same bytes rather than silently being substituted.
            if !matches!(profile.executable.as_str(), "cargo" | "cargo.exe") {
                let configured = Path::new(&profile.executable).canonicalize().ok();
                if configured.as_deref() != Some(Path::new(&toolchain.cargo.path)) {
                    snapshot.cargo_unavailable_reason = Some(
                        "The configured executable is not Cargo from the selected installed toolchain.".into(),
                    );
                    return Ok(snapshot);
                }
            }
            snapshot.resolved_executable = Some(toolchain.cargo.path.clone());
            snapshot.resolved_executable_sha256 = Some(toolchain.cargo.sha256.clone());
            snapshot.toolchain_sha256 = canonical_sha256(&toolchain, &[])?;
            snapshot.cargo_toolchain = Some(toolchain);
            snapshot.effective_environment = environment;
        }
        Err(reason) => snapshot.cargo_unavailable_reason = Some(reason),
    }
    Ok(snapshot)
}

use super::super::Validate;

fn reject_wrapper(path: &Path) -> Result<(), CompositionError> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "cmd" | "bat" | "ps1" | "lnk" | "vbs" | "js"
    ) {
        return Err(error(
            ErrorCode::ValidationUnavailable,
            format!(
                "`{}` is a shell wrapper or shortcut; a validation profile must name a direct executable.",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Locates an executable: an absolute/relative path is canonicalized as is;
/// a bare name is searched on `PATH` (with `PATHEXT`-style `.exe` on
/// Windows) and in `~/.cargo/bin`. Directories and wrappers are skipped.
pub fn find_executable(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.components().count() > 1 || direct.is_absolute() {
        return direct.canonicalize().ok().filter(|path| path.is_file());
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path));
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        candidates.push(PathBuf::from(home).join(".cargo").join("bin"));
    }
    let names: Vec<String> = if cfg!(windows) && !name.to_ascii_lowercase().ends_with(".exe") {
        vec![format!("{name}.exe"), name.to_owned()]
    } else {
        vec![name.to_owned()]
    };
    for directory in candidates {
        for file in &names {
            let path = directory.join(file);
            if path.is_file()
                && reject_wrapper(&path).is_ok()
                && let Ok(canonical) = path.canonicalize()
            {
                return Some(canonical);
            }
        }
    }
    None
}

fn preset(
    check_ref: &str,
    kind: CheckKind,
    required: bool,
    executable: &str,
    argv: &[&str],
    input_globs: &[&str],
    toolchain_identity: &str,
    timeout_seconds: u32,
) -> ValidationCommandProfileV1 {
    ValidationCommandProfileV1 {
        schema_version: SchemaV1,
        // Stable identity per preset so a re-detection keeps the same id.
        profile_id: Uuid::new(preset_uuid(check_ref)).expect("preset uuid"),
        profile_version: 2,
        check_ref: Handle::new(check_ref).expect("preset handle"),
        kind,
        required,
        executable: executable.to_owned(),
        argv: argv.iter().map(|s| (*s).to_owned()).collect(),
        cwd_relative_path: ".".into(),
        input_globs: input_globs.iter().map(|s| (*s).to_owned()).collect(),
        environment_names: Vec::new(),
        toolchain_identity: toolchain_identity.to_owned(),
        timeout_seconds,
        max_log_bytes: super::super::MAX_LOG_BYTES as u32,
    }
}

/// A stable UUID-shaped identity derived from the preset name.
fn preset_uuid(name: &str) -> String {
    let digest = Sha256Hex::of(format!("stage-composition-preset:{name}").as_bytes());
    let hex = digest.as_str();
    format!(
        "{}-{}-4{}-8{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}

/// Cargo v2 presets are proposals only: detection does not save, enable or
/// overwrite an existing profile. Their stable identities let the UI offer an
/// explicit upgrade, including the broader workspace selection and locked
/// dependency requirement. Other ecosystems remain custom profiles.
pub fn detect_presets(root: &Path) -> Vec<ValidationCommandProfileV1> {
    let mut presets = Vec::new();
    if root.join("Cargo.toml").is_file() {
        presets.push(preset(
            "cargo_check",
            CheckKind::Compile,
            true,
            "cargo",
            CARGO_CHECK_ARGUMENTS,
            &["**/*"],
            "cargo",
            DEFAULT_TIMEOUT_SECONDS,
        ));
        presets.push(preset(
            "cargo_test",
            CheckKind::Test,
            true,
            "cargo",
            CARGO_TEST_ARGUMENTS,
            &["**/*"],
            "cargo",
            MAX_TIMEOUT_SECONDS,
        ));
    }
    presets
}

/// Whether a check of this kind applies to a stage (§3 schedule).
pub fn applies_to_stage(kind: CheckKind, stage: super::super::Stage) -> bool {
    use super::super::Stage;
    match stage {
        Stage::Code => matches!(kind, CheckKind::Compile | CheckKind::Lint),
        Stage::Test => matches!(kind, CheckKind::Compile | CheckKind::Test),
        Stage::Integrator => matches!(kind, CheckKind::Document | CheckKind::Custom),
        Stage::Intake | Stage::Specification | Stage::Contract => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_presets_are_detected_only_with_a_manifest_and_have_stable_identity() {
        let root = tempfile::tempdir().unwrap();
        assert!(detect_presets(root.path()).is_empty());
        std::fs::write(root.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let presets = detect_presets(root.path());
        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].check_ref.as_str(), "cargo_check");
        assert_eq!(presets[0].kind, CheckKind::Compile);
        assert_eq!(presets[0].profile_version, 2);
        assert!(presets[0].argv.contains(&"--offline".to_owned()));
        assert!(presets[0].argv.contains(&"--locked".to_owned()));
        assert!(presets[0].argv.contains(&"--workspace".to_owned()));
        assert!(presets[0].argv.contains(&"--all-targets".to_owned()));
        assert_eq!(presets[0].input_globs, ["**/*"]);
        assert_eq!(presets[1].check_ref.as_str(), "cargo_test");
        assert!(
            presets[1]
                .argv
                .contains(&"--message-format=json".to_owned())
        );
        assert!(
            presets[1]
                .argv
                .ends_with(&["--".into(), "--test-threads=2".into()])
        );
        assert_eq!(
            presets[0].profile_id,
            detect_presets(root.path())[0].profile_id
        );
        assert_ne!(presets[0].profile_id, presets[1].profile_id);
        for profile in &presets {
            profile.validate().unwrap();
        }
    }

    #[test]
    fn a_wrapper_is_never_a_launch_target_and_a_missing_tool_is_unresolved() {
        let root = tempfile::tempdir().unwrap();
        let wrapper = root.path().join("tool.cmd");
        std::fs::write(&wrapper, "@echo off\r\n").unwrap();
        let mut profile = preset(
            "custom_check",
            CheckKind::Custom,
            false,
            wrapper.to_str().unwrap(),
            &[],
            &["**/*"],
            "custom",
            10,
        );
        let error = resolve_profile(&profile, ProfileOrigin::Custom).unwrap_err();
        assert_eq!(error.code, ErrorCode::ValidationUnavailable);
        profile.executable = "definitely-not-installed-ikore-tool".into();
        let snapshot = resolve_profile(&profile, ProfileOrigin::Custom).unwrap();
        assert!(snapshot.resolved_executable.is_none());
        assert!(snapshot.resolved_executable_sha256.is_none());
        // The snapshot still binds the profile and a toolchain digest.
        assert_eq!(
            snapshot.profile_sha256,
            canonical_sha256(&profile, &[]).unwrap()
        );
    }

    #[test]
    fn a_direct_executable_is_canonicalized_and_hashed() {
        let exe = std::env::current_exe().unwrap();
        let profile = preset(
            "self_check",
            CheckKind::Custom,
            false,
            exe.to_str().unwrap(),
            &["--version"],
            &["**/*"],
            "self",
            10,
        );
        let snapshot = resolve_profile(&profile, ProfileOrigin::Custom).unwrap();
        assert!(snapshot.resolved_executable.is_some());
        assert!(snapshot.resolved_executable_sha256.is_some());
        assert_ne!(snapshot.toolchain_sha256, Sha256Hex::of(b""));
    }

    #[test]
    fn legacy_snapshots_remain_readable_without_qualified_cargo_fields() {
        let profile = preset(
            "custom_check",
            CheckKind::Custom,
            false,
            "definitely-not-installed-ikore-tool",
            &[],
            &["**/*"],
            "custom",
            10,
        );
        let snapshot = resolve_profile(&profile, ProfileOrigin::Custom).unwrap();
        let serialized = serde_json::to_value(&snapshot).unwrap();
        assert!(serialized.get("cargoPolicy").is_none());
        assert!(serialized.get("effectiveEnvironment").is_none());
        let restored: ValidationProfileSnapshot = serde_json::from_value(serialized).unwrap();
        assert_eq!(restored, snapshot);
        assert!(restored.cargo_toolchain.is_none());
    }

    #[test]
    fn a_legacy_edit_counter_does_not_upgrade_a_cargo_command_or_its_input_scope() {
        use super::super::cargo_policy::is_qualified_cargo_profile;

        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let original = detect_presets(root.path()).remove(0);
        assert!(is_qualified_cargo_profile(&original));
        let mut legacy = original.clone();
        legacy.profile_version = 7;
        legacy.argv.retain(|arg| arg != "--locked");
        assert!(!is_qualified_cargo_profile(&legacy));
        let mut scoped = original.clone();
        scoped.input_globs = vec!["**/*.rs".into()];
        assert!(!is_qualified_cargo_profile(&scoped));
        let mut external = original;
        external
            .argv
            .extend(["--manifest-path".into(), "/outside/Cargo.toml".into()]);
        assert!(!is_qualified_cargo_profile(&external));
    }

    #[tokio::test]
    async fn cancelled_toolchain_resolution_preserves_the_approved_policy_without_launching() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        let profile = detect_presets(root.path()).remove(0);
        let policy = CargoValidationPolicyV1::default();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let snapshot = resolve_cargo_profile(
            &profile,
            ProfileOrigin::Preset,
            &policy,
            root.path(),
            &cancellation,
        )
        .await
        .unwrap();
        assert_eq!(snapshot.cargo_policy, Some(policy.clone()));
        assert!(snapshot.resolved_executable.is_none());
        assert!(snapshot.cargo_toolchain.is_none());
        assert!(snapshot.effective_environment.is_empty());
        assert!(
            snapshot
                .cargo_unavailable_reason
                .as_deref()
                .unwrap()
                .contains("cancelled")
        );
        assert_eq!(
            snapshot.profile_sha256,
            canonical_sha256(&(&profile, &policy), &[]).unwrap()
        );
    }
}
