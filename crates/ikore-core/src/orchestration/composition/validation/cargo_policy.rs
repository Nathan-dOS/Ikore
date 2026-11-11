//! Host-owned settings for the qualified Cargo presets. These settings never
//! grant a model command authority or widen its repository-reading policy.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::super::{
    CheckKind, CompositionError, ErrorCode, Invariant, Sha256Hex, ValidationCommandProfileV1,
};
use crate::orchestration::SchemaV1;

/// The parser contract is versioned independently of the project command.
pub const CARGO_PARSER_VERSION: u32 = 1;

pub const CARGO_CHECK_ARGUMENTS: &[&str] = &[
    "check",
    "--workspace",
    "--all-targets",
    "--locked",
    "--offline",
    "--message-format=json",
    "--color=never",
];

pub const CARGO_TEST_ARGUMENTS: &[&str] = &[
    "test",
    "--workspace",
    "--locked",
    "--offline",
    "--no-fail-fast",
    "--message-format=json",
    "--color=never",
    "--",
    "--test-threads=2",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CargoValidationPolicyV1 {
    pub schema_version: SchemaV1,
    /// Additional local inputs approved by the developer, including binary
    /// assets. Candidate capture still enforces its sensitive-path denylist.
    pub additional_inputs: Vec<String>,
    /// An installed Rust toolchain. With no override, the project pin is used
    /// before a configured host toolchain/default. Nothing is installed.
    pub toolchain: Option<String>,
    pub parser_version: u32,
}

impl Default for CargoValidationPolicyV1 {
    fn default() -> Self {
        Self {
            schema_version: SchemaV1,
            additional_inputs: Vec::new(),
            toolchain: None,
            parser_version: CARGO_PARSER_VERSION,
        }
    }
}

fn error(message: impl Into<String>) -> CompositionError {
    CompositionError::new(Invariant::Sc011, ErrorCode::HostContractError, message)
}

impl CargoValidationPolicyV1 {
    pub fn validate(&self) -> Result<(), CompositionError> {
        if self.parser_version != CARGO_PARSER_VERSION {
            return Err(error("The Cargo result-parser version is unsupported."));
        }
        if self.additional_inputs.len() > 64 {
            return Err(error("Cargo additional inputs are limited to 64 patterns."));
        }
        let mut seen = BTreeSet::new();
        for input in &self.additional_inputs {
            if input.is_empty()
                || input.len() > 1024
                || input.contains(['\\', '\0', ':'])
                || Path::new(input).is_absolute()
                || Path::new(input).components().any(|component| {
                    matches!(
                        component,
                        Component::ParentDir | Component::RootDir | Component::Prefix(_)
                    )
                })
                || input.split('/').any(|part| part == "." || part.is_empty())
                || globset::Glob::new(input).is_err()
            {
                return Err(error(
                    "Cargo additional inputs must be relative path patterns without traversal.",
                ));
            }
            if !seen.insert(input.to_ascii_lowercase()) {
                return Err(error("Cargo additional input patterns must be unique."));
            }
        }
        if let Some(toolchain) = &self.toolchain {
            validate_toolchain_name(toolchain)?;
        }
        Ok(())
    }
}

fn validate_toolchain_name(name: &str) -> Result<(), CompositionError> {
    if name.is_empty()
        || name.len() > 128
        || name.starts_with('.')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(error(
            "The Cargo toolchain must name an installed toolchain, not a path or command.",
        ));
    }
    Ok(())
}

/// Additional host identity. Versions are filled only after a bounded probe;
/// an absent version remains unknown rather than being inferred from a label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CargoToolIdentity {
    pub path: String,
    pub sha256: Sha256Hex,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CargoToolchainSnapshot {
    pub selected_toolchain: String,
    pub cargo: CargoToolIdentity,
    pub rustc: CargoToolIdentity,
    pub rustdoc: CargoToolIdentity,
}

/// Recognize the implemented command contract. `profileVersion` is an edit
/// counter, so a larger number alone cannot qualify a legacy command. The
/// caller must additionally require the separately approved Cargo policy row.
pub fn is_qualified_cargo_profile(profile: &ValidationCommandProfileV1) -> bool {
    profile.profile_version >= 2
        && profile.toolchain_identity == "cargo"
        && profile.cwd_relative_path == "."
        && profile.input_globs == ["**/*"]
        && match (profile.check_ref.as_str(), profile.kind) {
            ("cargo_check", CheckKind::Compile) => profile.argv == CARGO_CHECK_ARGUMENTS,
            ("cargo_test", CheckKind::Test) => profile.argv == CARGO_TEST_ARGUMENTS,
            _ => false,
        }
}

/// Locate already installed tools by inspecting Rustup's local installation.
/// Never invoke a Rustup proxy: resolving a missing pin must not download it.
pub(super) fn installed_toolchain(
    root: &Path,
    policy: &CargoValidationPolicyV1,
) -> Result<(CargoToolchainSnapshot, Vec<(String, String)>), String> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    let rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            home.as_ref()
                .map(|path| PathBuf::from(path).join(".rustup"))
        })
        .ok_or_else(|| "Rustup's local installation directory is unavailable.".to_owned())?;
    let requested = match &policy.toolchain {
        Some(toolchain) => toolchain.clone(),
        None => match project_toolchain(root)? {
            Some(toolchain) => toolchain,
            None => std::env::var("RUSTUP_TOOLCHAIN")
                .ok()
                .filter(|value| !value.is_empty())
                .or_else(|| installed_default(&rustup_home))
                .ok_or_else(|| {
                    "Select an installed Rust toolchain or add a project toolchain pin.".to_owned()
                })?,
        },
    };
    validate_toolchain_name(&requested).map_err(|error| error.to_string())?;
    let directory = installed_directory(&rustup_home, &requested)?;
    let selected_toolchain = directory
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "The installed toolchain directory has an unsupported name.".to_owned())?
        .to_owned();
    let bin = directory.join("bin");
    let toolchain = CargoToolchainSnapshot {
        selected_toolchain: selected_toolchain.clone(),
        cargo: tool_identity(&bin, "cargo")?,
        rustc: tool_identity(&bin, "rustc")?,
        rustdoc: tool_identity(&bin, "rustdoc")?,
    };
    let mut environment = Vec::new();
    for name in ["HOME", "USERPROFILE"] {
        if let Ok(value) = std::env::var(name) {
            environment.push((name.to_owned(), value));
        }
    }
    if let Some(cargo_home) = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|path| PathBuf::from(path).join(".cargo")))
    {
        environment.push((
            "CARGO_HOME".into(),
            cargo_home.to_string_lossy().into_owned(),
        ));
    }
    environment.extend([
        (
            "RUSTUP_HOME".into(),
            rustup_home.to_string_lossy().into_owned(),
        ),
        ("RUSTUP_TOOLCHAIN".into(), selected_toolchain),
        ("RUSTC".into(), toolchain.rustc.path.clone()),
        ("RUSTDOC".into(), toolchain.rustdoc.path.clone()),
    ]);
    environment.sort();
    Ok((toolchain, environment))
}

fn installed_directory(rustup_home: &Path, requested: &str) -> Result<PathBuf, String> {
    let installations = rustup_home.join("toolchains");
    for name in [
        requested.to_owned(),
        format!("{requested}-{}", host_triple_suffix()),
    ] {
        let directory = installations.join(name);
        if directory.is_dir() {
            return Ok(directory);
        }
    }
    Err(format!(
        "Rust toolchain `{requested}` is not installed for this host; install it explicitly before validation."
    ))
}

fn project_toolchain(root: &Path) -> Result<Option<String>, String> {
    for file in ["rust-toolchain.toml", "rust-toolchain"] {
        let path = root.join(file);
        // exists() follows links, so a dangling pin would otherwise silently
        // fall back to a different toolchain. Inspect the entry itself.
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("The project toolchain pin could not be inspected.".into()),
        };
        if !metadata.is_file() || metadata.len() > 64 * 1024 {
            return Err(
                "The project toolchain pin must be a regular file of at most 64 KiB.".into(),
            );
        }
        let contents = std::fs::read_to_string(&path)
            .map_err(|_| "The project toolchain pin could not be read.".to_owned())?;
        let channel = if file.ends_with(".toml") || contents.trim_start().starts_with('[') {
            let value: toml::Value = toml::from_str(&contents)
                .map_err(|_| "The project toolchain pin is not valid TOML.".to_owned())?;
            value
                .get("toolchain")
                .and_then(|value| value.get("channel"))
                .and_then(toml::Value::as_str)
                .ok_or_else(|| "The project toolchain pin needs a channel.".to_owned())?
                .to_owned()
        } else {
            contents.trim().to_owned()
        };
        validate_toolchain_name(&channel).map_err(|error| error.to_string())?;
        return Ok(Some(channel));
    }
    Ok(None)
}

fn installed_default(rustup_home: &Path) -> Option<String> {
    let path = rustup_home.join("settings.toml");
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return None;
    }
    let contents = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&contents).ok()?;
    value.get("default_toolchain")?.as_str().map(str::to_owned)
}

fn host_triple_suffix() -> String {
    let architecture = std::env::consts::ARCH;
    match std::env::consts::OS {
        "windows" => format!("{architecture}-pc-windows-msvc"),
        "macos" => format!("{architecture}-apple-darwin"),
        _ => format!("{architecture}-unknown-linux-gnu"),
    }
}

fn tool_identity(bin: &Path, name: &str) -> Result<CargoToolIdentity, String> {
    let name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    let path = bin.join(name);
    let path = path
        .canonicalize()
        .map_err(|_| "The selected toolchain is missing Cargo, rustc or rustdoc.".to_owned())?;
    if !path.is_file() {
        return Err("The selected toolchain is missing Cargo, rustc or rustdoc.".into());
    }
    let sha = crate::terminals::hash_launch_files(&[&path])
        .map_err(|_| "An installed Cargo tool could not be hashed.".to_owned())?;
    Ok(CargoToolIdentity {
        path: path.to_string_lossy().into_owned(),
        sha256: Sha256Hex::new(sha).map_err(|error| error.to_string())?,
        version: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_default_does_not_approve_extra_files_and_rejects_unsupported_changes() {
        let mut policy = CargoValidationPolicyV1::default();
        policy.validate().unwrap();
        assert!(policy.additional_inputs.is_empty());
        policy.additional_inputs = vec!["assets/icon.ico".into(), "fixtures/**/*.jsonl".into()];
        policy.validate().unwrap();
        for invalid in [
            "../outside",
            "/absolute",
            "C:/outside",
            "a/../b",
            "a\\b",
            "a//b",
        ] {
            policy.additional_inputs = vec![invalid.into()];
            assert!(policy.validate().is_err(), "{invalid}");
        }
        policy.additional_inputs = vec!["a.bin".into(), "A.bin".into()];
        assert!(policy.validate().is_err());
        policy.additional_inputs.clear();
        policy.toolchain = Some("1.98.0".into());
        policy.validate().unwrap();
        policy.toolchain = Some("../toolchain".into());
        assert!(policy.validate().is_err());
        policy.toolchain = None;
        policy.parser_version += 1;
        assert!(policy.validate().is_err());
    }

    #[test]
    fn project_pin_is_read_without_launching_rustup_or_installing_anything() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(project_toolchain(root.path()).unwrap(), None);
        std::fs::write(root.path().join("rust-toolchain"), "1.98.0\n").unwrap();
        assert_eq!(
            project_toolchain(root.path()).unwrap(),
            Some("1.98.0".into())
        );
        std::fs::write(
            root.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"stable\"\n",
        )
        .unwrap();
        assert_eq!(
            project_toolchain(root.path()).unwrap(),
            Some("stable".into())
        );
        std::fs::write(
            root.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"../unsafe\"\n",
        )
        .unwrap();
        assert!(project_toolchain(root.path()).is_err());
    }

    #[test]
    fn an_installed_pin_binds_real_tool_bytes_without_executing_a_proxy() {
        let root = tempfile::tempdir().unwrap();
        let installed = root
            .path()
            .join("toolchains")
            .join(format!("1.98.0-{}", host_triple_suffix()));
        let bin = installed.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for name in ["cargo", "rustc", "rustdoc"] {
            let file = if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_owned()
            };
            // Deliberately not executable. Resolution hashes files and cannot
            // accidentally invoke Rustup or install the selected toolchain.
            std::fs::write(bin.join(file), format!("fixture {name}")).unwrap();
        }
        assert_eq!(
            installed_directory(root.path(), "1.98.0").unwrap(),
            installed
        );
        assert!(installed_directory(root.path(), "not-installed").is_err());
        assert!(!root.path().join("toolchains/not-installed").exists());
        let before = tool_identity(&bin, "rustc").unwrap();
        let file = if cfg!(windows) { "rustc.exe" } else { "rustc" };
        std::fs::write(bin.join(file), "updated compiler bytes").unwrap();
        let after = tool_identity(&bin, "rustc").unwrap();
        assert_ne!(before.sha256, after.sha256);
        assert!(after.version.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_dangling_project_pin_does_not_silently_fall_back_to_the_host_toolchain() {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("missing-toolchain", root.path().join("rust-toolchain.toml"))
            .unwrap();
        assert!(project_toolchain(root.path()).is_err());
    }
}
