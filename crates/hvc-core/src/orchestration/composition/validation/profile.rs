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
    })
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
        profile_version: 1,
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

/// Presets the host detects from the project's manifests. Only Cargo in v1:
/// its dependency cache is global (`CARGO_HOME`), so a disposable copy of
/// the sources builds offline without touching the live checkout. Node
/// projects need `node_modules` inside the candidate and are left to an
/// explicit developer profile.
pub fn detect_presets(root: &Path) -> Vec<ValidationCommandProfileV1> {
    let mut presets = Vec::new();
    if root.join("Cargo.toml").is_file() {
        presets.push(preset(
            "cargo_check",
            CheckKind::Compile,
            true,
            "cargo",
            &[
                "check",
                "--offline",
                "--message-format=json",
                "--color=never",
            ],
            &[
                "Cargo.toml",
                "Cargo.lock",
                "**/*.rs",
                "**/Cargo.toml",
                "build.rs",
            ],
            "cargo",
            DEFAULT_TIMEOUT_SECONDS,
        ));
        presets.push(preset(
            "cargo_test",
            CheckKind::Test,
            true,
            "cargo",
            &["test", "--offline", "--no-fail-fast", "--color=never"],
            &[
                "Cargo.toml",
                "Cargo.lock",
                "**/*.rs",
                "**/Cargo.toml",
                "build.rs",
                "tests/**",
            ],
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
        assert!(presets[0].argv.contains(&"--offline".to_owned()));
        assert_eq!(presets[1].check_ref.as_str(), "cargo_test");
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
        profile.executable = "definitely-not-installed-hvc-tool".into();
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
}
