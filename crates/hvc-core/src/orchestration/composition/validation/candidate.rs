//! Disposable candidate materialization (§2 of the validation contract).
//!
//! A candidate is the fenced project baseline (the same text-only snapshot
//! the CLI transport uses: git-ignored, generated and VCS directories are
//! excluded) overlaid with the validated unit outputs of the execution,
//! written under the application's data directory - never the live
//! checkout. Every run binds to the manifest of what was written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSetBuilder};

use super::super::{CompositionError, ErrorCode, Invariant, RelPath, Sha256Hex};
use crate::repository_context::RepositoryContext;

/// Upper bounds of the disposable copy (the CLI workspace uses the same).
pub const MAX_CANDIDATE_FILES: usize = 10_000;
pub const MAX_CANDIDATE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Root of the disposable copy.
    pub dir: PathBuf,
    /// Every file written, `path -> sha256`, sorted by path.
    pub manifest: BTreeMap<String, Sha256Hex>,
    /// Digest of the whole manifest.
    pub candidate_sha256: Sha256Hex,
    /// Paths overlaid from validated unit outputs.
    pub staged_paths: Vec<String>,
}

fn error(message: impl Into<String>) -> CompositionError {
    CompositionError::new(Invariant::Sc011, ErrorCode::HostContractError, message)
}

/// Materializes the baseline of `root` plus `staged` (`path -> content`) into
/// `target`, replacing whatever a previous run left there. Paths must be
/// workspace-relative (they are, by construction of `RelPath`).
pub fn materialize_candidate(
    root: &Path,
    staged: &[(RelPath, String)],
    target: &Path,
) -> Result<Candidate, CompositionError> {
    let context = RepositoryContext::new(root).map_err(|e| error(e.to_string()))?;
    let files = context
        .safe_snapshot_files(MAX_CANDIDATE_FILES, MAX_CANDIDATE_BYTES)
        .map_err(|e| error(e.to_string()))?;
    if target.exists() {
        std::fs::remove_dir_all(target).map_err(|e| error(e.to_string()))?;
    }
    std::fs::create_dir_all(target).map_err(|e| error(e.to_string()))?;
    let mut contents: BTreeMap<String, String> = files
        .into_iter()
        .map(|file| (file.relative_path, file.content))
        .collect();
    let mut staged_paths = Vec::with_capacity(staged.len());
    for (path, content) in staged {
        // A staged output replaces the baseline under Windows identity.
        let key = path.canonical_key();
        contents.retain(|existing, _| {
            RelPath::new(existing.as_str()).map(|p| p.canonical_key()) != Ok(key.clone())
        });
        contents.insert(path.as_str().to_owned(), content.clone());
        staged_paths.push(path.as_str().to_owned());
    }
    let mut manifest = BTreeMap::new();
    for (relative, content) in &contents {
        let relative_path = Path::new(relative);
        if relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(error(format!(
                "Candidate path `{relative}` escapes the root."
            )));
        }
        let destination = target.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|e| error(e.to_string()))?;
        }
        std::fs::write(&destination, content.as_bytes()).map_err(|e| error(e.to_string()))?;
        manifest.insert(relative.clone(), Sha256Hex::of(content.as_bytes()));
    }
    let candidate_sha256 = manifest_digest(manifest.iter());
    Ok(Candidate {
        dir: target.to_path_buf(),
        manifest,
        candidate_sha256,
        staged_paths,
    })
}

/// Digest of `(path, sha256)` pairs in path order.
pub fn manifest_digest<'a>(
    entries: impl Iterator<Item = (&'a String, &'a Sha256Hex)>,
) -> Sha256Hex {
    let mut bytes = Vec::new();
    for (path, sha) in entries {
        bytes.extend_from_slice(path.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(sha.as_str().as_bytes());
        bytes.push(0);
    }
    Sha256Hex::of(&bytes)
}

impl Candidate {
    /// Digest of the manifest entries matched by the profile's input globs:
    /// the exact input set a receipt proves the command saw.
    pub fn input_manifest_sha256(
        &self,
        input_globs: &[String],
    ) -> Result<Sha256Hex, CompositionError> {
        let mut builder = GlobSetBuilder::new();
        for glob in input_globs {
            builder.add(
                Glob::new(glob).map_err(|e| error(format!("Invalid input glob `{glob}`: {e}")))?,
            );
        }
        let set = builder.build().map_err(|e| error(e.to_string()))?;
        Ok(manifest_digest(
            self.manifest
                .iter()
                .filter(|(path, _)| set.is_match(path.as_str())),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_candidate_is_the_text_baseline_plus_staged_outputs_with_a_bound_manifest() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::create_dir_all(root.path().join("target")).unwrap();
        std::fs::write(root.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        std::fs::write(root.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        std::fs::write(root.path().join("target/junk.rs"), "ignored").unwrap();
        let data = tempfile::tempdir().unwrap();
        let build = data.path().join("build");
        let staged = vec![
            (
                RelPath::new("src/lib.rs").unwrap(),
                "pub fn a() -> u8 { 1 }\n".to_owned(),
            ),
            (
                RelPath::new("src/new.rs").unwrap(),
                "pub fn b() {}\n".to_owned(),
            ),
        ];
        let candidate = materialize_candidate(root.path(), &staged, &build).unwrap();
        assert_eq!(
            std::fs::read_to_string(build.join("src/lib.rs")).unwrap(),
            "pub fn a() -> u8 { 1 }\n"
        );
        assert!(build.join("src/new.rs").exists());
        assert!(
            !build.join("target").exists(),
            "generated directories are excluded"
        );
        assert_eq!(
            candidate.manifest.keys().cloned().collect::<Vec<_>>(),
            vec!["Cargo.toml", "src/lib.rs", "src/new.rs"]
        );
        // The live checkout is untouched.
        assert_eq!(
            std::fs::read_to_string(root.path().join("src/lib.rs")).unwrap(),
            "pub fn a() {}\n"
        );
        // Input globs select the bound subset; identical inputs give identical digests.
        let rust = candidate
            .input_manifest_sha256(&["**/*.rs".into()])
            .unwrap();
        let all = candidate.input_manifest_sha256(&["**/*".into()]).unwrap();
        assert_ne!(rust, all);
        let again = materialize_candidate(root.path(), &staged, &build).unwrap();
        assert_eq!(again.candidate_sha256, candidate.candidate_sha256);
        assert_eq!(
            again.input_manifest_sha256(&["**/*.rs".into()]).unwrap(),
            rust
        );
        // A different staged output changes the candidate identity.
        let other = vec![(
            RelPath::new("src/lib.rs").unwrap(),
            "pub fn a() -> u8 { 2 }\n".to_owned(),
        )];
        assert_ne!(
            materialize_candidate(root.path(), &other, &build)
                .unwrap()
                .candidate_sha256,
            candidate.candidate_sha256
        );
    }
}
