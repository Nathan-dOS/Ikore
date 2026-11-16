//! Byte-preserving inputs for qualified host validation. This reader is deliberately
//! separate from `RepositoryContext`: opaque assets never become model context.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use super::super::super::{CompositionError, RelPath, Sha256Hex};
use super::{Candidate, MAX_CANDIDATE_BYTES, MAX_CANDIDATE_FILES, error, manifest_digest};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ENUMERATED_ENTRIES: usize = 100_000;
const MAX_DEPTH: usize = 72;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CopyKind {
    Baseline,
    Materialized,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CaptureMetadata {
    version: u32,
    directory_name: String,
    kind: CopyKind,
    additional_inputs: Vec<String>,
    /// Exact host-generated document paths, never a directory or glob exclusion.
    #[serde(default)]
    excluded_paths: Vec<String>,
    manifest: BTreeMap<String, Sha256Hex>,
    byte_lengths: BTreeMap<String, u64>,
    candidate_sha256: Sha256Hex,
    /// Manifest entries originate in the frozen baseline unless named here;
    /// these paths were overlaid from approved staged textual outputs.
    staged_paths: Vec<String>,
}

#[derive(Debug, Clone)]
struct Input {
    bytes: Vec<u8>,
    permissions: fs::Permissions,
}

struct Selection {
    globs: GlobSet,
    matched: Vec<bool>,
}

impl Selection {
    fn new(patterns: &[String]) -> Result<Self, CompositionError> {
        if patterns.len() > 64 {
            return Err(error("At most 64 additional input patterns are supported."));
        }
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            if pattern.is_empty()
                || pattern.len() > 1024
                || pattern.starts_with('/')
                || pattern.contains(['\\', ':', '\0'])
                || pattern.split('/').any(|part| part == ".." || part == ".")
            {
                return Err(error(
                    "Additional input patterns must be relative paths without traversal.",
                ));
            }
            builder.add(
                Glob::new(pattern).map_err(|_| error("An additional input glob is invalid."))?,
            );
        }
        Ok(Self {
            globs: builder
                .build()
                .map_err(|_| error("Additional input globs are invalid."))?,
            matched: vec![false; patterns.len()],
        })
    }

    fn includes(&mut self, path: &str) -> bool {
        let matches = self.globs.matches(path);
        for index in &matches {
            self.matched[*index] = true;
        }
        !matches.is_empty()
    }

    fn require_matches(&self) -> Result<(), CompositionError> {
        if self.matched.iter().any(|matched| !matched) {
            return Err(error(
                "A declared additional input matched no permitted file.",
            ));
        }
        Ok(())
    }
}

fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

fn reject_links(path: &Path) -> Result<(), CompositionError> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if linked(&metadata) => {
                return Err(error(
                    "Validation inputs and destinations cannot contain links or reparse points.",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(error("A validation path could not be inspected.")),
        }
    }
    Ok(())
}

fn canonical_root(root: &Path) -> Result<PathBuf, CompositionError> {
    reject_links(root)?;
    let root = root
        .canonicalize()
        .map_err(|_| error("The validation input root is unavailable."))?;
    if !root.is_dir() {
        return Err(error("The validation input root is not a directory."));
    }
    Ok(root)
}

fn target_path(target: &Path) -> Result<PathBuf, CompositionError> {
    if target
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(error("A validation destination cannot contain traversal."));
    }
    let absolute = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| error("The current directory is unavailable."))?
            .join(target)
    };
    reject_links(&absolute)?;
    if absolute.exists() {
        return absolute
            .canonicalize()
            .map_err(|_| error("The validation destination is unavailable."));
    }
    let existing = absolute
        .ancestors()
        .find(|path| path.exists())
        .ok_or_else(|| error("The validation destination has no available ancestor."))?;
    let suffix = absolute
        .strip_prefix(existing)
        .map_err(|_| error("The validation destination is invalid."))?;
    let parent = existing
        .canonicalize()
        .map_err(|_| error("The validation destination parent is unavailable."))?;
    let target = parent.join(suffix);
    if target.file_name().is_none() {
        return Err(error("The validation destination has no name."));
    }
    Ok(target)
}

fn separate(source: &Path, target: &Path) -> Result<(), CompositionError> {
    if source.starts_with(target) || target.starts_with(source) {
        return Err(error(
            "Validation source and destination directories must not overlap.",
        ));
    }
    Ok(())
}

fn metadata_dir(target: &Path) -> Result<PathBuf, CompositionError> {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| error("A validation destination must have a UTF-8 name."))?;
    let canonical = target.with_file_name(format!(".{name}.ikore-candidate"));
    let legacy = target.with_file_name(format!(".{name}.hvc-candidate"));
    if !canonical.exists() && legacy.exists() {
        return Ok(legacy);
    }
    Ok(canonical)
}

fn sensitive(name: &str) -> bool {
    name.starts_with(".env")
        || name.starts_with("credentials")
        || name.starts_with("secrets")
        || name.starts_with("id_rsa")
        || name.starts_with("id_dsa")
        || name.starts_with("id_ecdsa")
        || name.starts_with("id_ed25519")
        || name.contains("service-account")
        || name.contains("private-key")
        || matches!(name, ".npmrc" | ".pypirc" | ".netrc")
        || [".pem", ".key", ".p12", ".pfx", ".crt", ".cer"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn protected(relative: &str) -> bool {
    relative.split('/').any(|part| {
        let name = part.to_ascii_lowercase();
        sensitive(&name)
            || matches!(
                name.as_str(),
                ".git"
                    | ".hg"
                    | ".svn"
                    | ".ikore"
                    | ".hvc"
                    | ".hard-vibe-code"
                    | ".agents"
                    | ".claude"
                    | ".codex"
                    | ".gemini"
                    | ".opencode"
                    | ".aws"
                    | ".ssh"
                    | "node_modules"
                    | "target"
                    | "dist"
                    | "build"
                    | "out"
                    | "obj"
                    | ".next"
                    | ".nuxt"
                    | ".svelte-kit"
                    | ".cache"
                    | ".turbo"
                    | ".vite"
                    | ".parcel-cache"
                    | "coverage"
                    | ".coverage"
                    | "__pycache__"
                    | ".pytest_cache"
                    | ".mypy_cache"
                    | ".gradle"
                    | ".idea"
            )
    }) || relative
        .split('/')
        .collect::<Vec<_>>()
        .windows(2)
        .any(|parts| parts[1].eq_ignore_ascii_case("bin") && !parts[0].eq_ignore_ascii_case("src"))
        || relative.eq_ignore_ascii_case("bin")
        || relative.to_ascii_lowercase().starts_with("bin/")
}

fn default_text(relative: &str) -> bool {
    let path = Path::new(relative);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "cargo.lock"
            | "rust-toolchain"
            | "dockerfile"
            | "makefile"
            | "license"
            | "go.mod"
            | "go.work"
            | ".gitignore"
            | ".gitattributes"
            | ".editorconfig"
    ) || name.starts_with("readme")
        || relative.replace('\\', "/").ends_with(".cargo/config")
        || matches!(
            extension.as_str(),
            "c" | "cc"
                | "cpp"
                | "cs"
                | "css"
                | "csv"
                | "go"
                | "graphql"
                | "h"
                | "hpp"
                | "html"
                | "java"
                | "js"
                | "json"
                | "jsx"
                | "kt"
                | "kts"
                | "md"
                | "php"
                | "proto"
                | "ps1"
                | "py"
                | "rb"
                | "rs"
                | "scss"
                | "sh"
                | "csproj"
                | "gradle"
                | "mod"
                | "sln"
                | "sql"
                | "svg"
                | "swift"
                | "toml"
                | "ts"
                | "tsx"
                | "txt"
                | "xml"
                | "yaml"
                | "yml"
                | "rc"
                | "hlsl"
                | "wgsl"
                | "metal"
                | "jsonl"
        )
}

fn cargo_input(relative: &str) -> bool {
    if relative
        .split('/')
        .any(|part| part.eq_ignore_ascii_case("vendor"))
    {
        return false;
    }
    let path = Path::new(relative);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    name.eq_ignore_ascii_case("Cargo.lock")
        || name.eq_ignore_ascii_case("rust-toolchain")
        || name.eq_ignore_ascii_case("rust-toolchain.toml")
        || relative.ends_with(".cargo/config")
        || relative.ends_with(".cargo/config.toml")
}

fn relative(root: &Path, path: &Path) -> Result<String, CompositionError> {
    let path = path
        .strip_prefix(root)
        .map_err(|_| error("An input escapes the validation root."))?
        .to_str()
        .ok_or_else(|| error("A validation input has a non-UTF-8 path."))?
        .replace('\\', "/");
    RelPath::new(path.clone())?;
    Ok(path)
}

fn read_input(path: &Path) -> Result<Input, CompositionError> {
    reject_links(path)?;
    let before = fs::symlink_metadata(path)
        .map_err(|_| error("A selected input could not be inspected."))?;
    if !before.is_file() || before.len() > MAX_FILE_BYTES {
        return Err(error(
            "A selected validation input is not a regular file of at most 1 MiB.",
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|_| error("A selected input could not be opened."))?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| error("A selected input could not be read completely."))?;
    reject_links(path)?;
    let after = fs::symlink_metadata(path)
        .map_err(|_| error("A selected input changed while being read."))?;
    if !after.is_file()
        || before.len() != bytes.len() as u64
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err(error(
            "A selected validation input changed while being read.",
        ));
    }
    Ok(Input {
        bytes,
        permissions: before.permissions(),
    })
}

fn opaque_input_error(path: &str) -> CompositionError {
    let relative = crate::secret_redactor::redact_secrets(path)
        .map(|result| result.text)
        .unwrap_or_else(|_| "selected input".into());
    let relative: String = relative.chars().take(240).collect();
    error(format!(
        "Opaque validation input `{relative}` requires an explicit additional input pattern."
    ))
}

fn insert_input(
    inputs: &mut BTreeMap<String, Input>,
    identities: &mut BTreeMap<String, String>,
    total: &mut u64,
    path: String,
    input: Input,
) -> Result<(), CompositionError> {
    let key = RelPath::new(path.clone())?.canonical_key();
    if let Some(previous) = identities.get(&key) {
        if previous != &path {
            return Err(error(
                "Selected inputs collide under Windows path identity.",
            ));
        }
        if inputs
            .get(&path)
            .is_some_and(|old| old.bytes != input.bytes)
        {
            return Err(error("A selected validation input changed during capture."));
        }
        return Ok(());
    }
    let bytes = input.bytes.len() as u64;
    if inputs.len() >= MAX_CANDIDATE_FILES || total.saturating_add(bytes) > MAX_CANDIDATE_BYTES {
        return Err(error(
            "Validation inputs exceed the 10,000 file or 64 MiB limit.",
        ));
    }
    *total += bytes;
    identities.insert(key, path.clone());
    inputs.insert(path, input);
    Ok(())
}

fn exclusion_keys(paths: &[String]) -> Result<BTreeSet<String>, CompositionError> {
    paths
        .iter()
        .map(|path| RelPath::new(path.clone()).map(|path| path.canonical_key()))
        .collect()
}

fn is_excluded(path: &str, keys: &BTreeSet<String>) -> bool {
    RelPath::new(path).is_ok_and(|path| keys.contains(&path.canonical_key()))
}

fn inventory(
    root: &Path,
    additional_inputs: &[String],
    staged_paths: &[String],
    excluded_paths: &[String],
    respect_ignores: bool,
) -> Result<BTreeMap<String, Input>, CompositionError> {
    let mut selection = Selection::new(additional_inputs)?;
    let excluded = exclusion_keys(excluded_paths)?;
    let staged: BTreeSet<_> = staged_paths.iter().map(String::as_str).collect();
    let mut inputs = BTreeMap::new();
    let mut identities = BTreeMap::new();
    let mut total = 0;
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(false)
        .parents(respect_ignores)
        .git_ignore(respect_ignores)
        .git_exclude(respect_ignores)
        .git_global(false)
        .require_git(false)
        .ignore(respect_ignores)
        .follow_links(false)
        .max_depth(Some(MAX_DEPTH))
        .sort_by_file_name(|a, b| a.cmp(b));
    let filter_root = root.to_path_buf();
    let filter_excluded = excluded.clone();
    builder.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        let Ok(path) = relative(&filter_root, entry.path()) else {
            return false;
        };
        !protected(&path)
            && (entry.file_type().is_some_and(|kind| kind.is_dir())
                || !is_excluded(&path, &filter_excluded))
            && !path
                .split('/')
                .any(|part| part.eq_ignore_ascii_case("vendor"))
    });
    for (index, item) in builder.build().enumerate() {
        if index >= MAX_ENUMERATED_ENTRIES {
            return Err(error("The validation input enumeration limit was reached."));
        }
        let entry =
            item.map_err(|_| error("Validation inputs could not be enumerated completely."))?;
        if entry.depth() == 0 {
            continue;
        }
        let path = relative(root, entry.path())?;
        if entry.depth() >= MAX_DEPTH && entry.file_type().is_some_and(|kind| kind.is_dir()) {
            return Err(error("The validation input depth limit was reached."));
        }
        if !default_text(&path) && !staged.contains(path.as_str()) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|_| error("A selected input is unavailable."))?;
        if linked(&metadata) {
            return Err(error("A selected validation input is a link."));
        }
        if !metadata.is_file() {
            continue;
        }
        let input = read_input(entry.path())?;
        if (input.bytes.contains(&0) || std::str::from_utf8(&input.bytes).is_err())
            && !selection.globs.is_match(&path)
        {
            return Err(opaque_input_error(&path));
        }
        insert_input(&mut inputs, &mut identities, &mut total, path, input)?;
    }
    // Explicit assets may be ignored by Git, or live in a vendored dependency.
    // All immutable deny rules still apply before the allowlist is considered.
    {
        let walker = walkdir::WalkDir::new(root)
            .follow_links(false)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|entry| {
                entry.depth() == 0
                    || relative(root, entry.path()).is_ok_and(|path| {
                        !protected(&path)
                            && (entry.file_type().is_dir() || !is_excluded(&path, &excluded))
                    })
            });
        for (index, item) in walker.enumerate() {
            if index >= MAX_ENUMERATED_ENTRIES {
                return Err(error("The validation input enumeration limit was reached."));
            }
            let entry = item.map_err(|_| {
                error("Additional validation inputs could not be enumerated completely.")
            })?;
            if entry.depth() == 0 {
                continue;
            }
            if entry.depth() >= MAX_DEPTH {
                return Err(error("The validation input depth limit was reached."));
            }
            let path = relative(root, entry.path())?;
            let selected = selection.globs.is_match(&path)
                || staged.contains(path.as_str())
                || cargo_input(&path);
            if !selected {
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| error("A declared input is unavailable."))?;
            if linked(&metadata) {
                return Err(error("A declared validation input is a link."));
            }
            if !metadata.is_file() {
                continue;
            }
            selection.includes(&path);
            let input = read_input(entry.path())?;
            if (input.bytes.contains(&0) || std::str::from_utf8(&input.bytes).is_err())
                && !selection.globs.is_match(&path)
            {
                return Err(opaque_input_error(&path));
            }
            insert_input(&mut inputs, &mut identities, &mut total, path, input)?;
        }
    }
    selection.require_matches()?;
    Ok(inputs)
}

fn input_manifest(inputs: &BTreeMap<String, Input>) -> BTreeMap<String, Sha256Hex> {
    inputs
        .iter()
        .map(|(path, input)| (path.clone(), Sha256Hex::of(&input.bytes)))
        .collect()
}

fn byte_lengths(inputs: &BTreeMap<String, Input>) -> BTreeMap<String, u64> {
    inputs
        .iter()
        .map(|(path, input)| (path.clone(), input.bytes.len() as u64))
        .collect()
}

fn write_copy(
    target: &Path,
    inputs: &BTreeMap<String, Input>,
    additional_inputs: &[String],
    excluded_paths: &[String],
    staged_paths: Vec<String>,
    kind: CopyKind,
    replace: bool,
) -> Result<Candidate, CompositionError> {
    let sidecar = metadata_dir(target)?;
    if target.exists() || sidecar.exists() {
        if !replace {
            return Err(error("An immutable baseline destination already exists."));
        }
        let old = read_metadata(target)?;
        if old.kind != CopyKind::Materialized {
            return Err(error(
                "An immutable baseline cannot be replaced by a build copy.",
            ));
        }
        reject_links(target)?;
        reject_links(&sidecar)?;
    }
    let parent = target
        .parent()
        .ok_or_else(|| error("The validation destination has no parent."))?;
    fs::create_dir_all(parent)
        .map_err(|_| error("The validation destination parent is unavailable."))?;
    reject_links(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".ikore-capture-")
        .tempdir_in(parent)
        .map_err(|_| error("Temporary validation storage is unavailable."))?;
    let data = staging.path().join("data");
    let metadata = staging.path().join("metadata");
    fs::create_dir(&data).map_err(|_| error("Temporary validation storage is unavailable."))?;
    fs::create_dir(&metadata)
        .map_err(|_| error("Temporary validation metadata is unavailable."))?;
    for (path, input) in inputs {
        let destination = data.join(path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|_| error("A validation input directory could not be created."))?;
        }
        fs::write(&destination, &input.bytes)
            .map_err(|_| error("A validation input could not be copied."))?;
        fs::set_permissions(&destination, input.permissions.clone())
            .map_err(|_| error("Validation input permissions could not be preserved."))?;
    }
    let manifest = input_manifest(inputs);
    let candidate_sha256 = manifest_digest(manifest.iter());
    let record = CaptureMetadata {
        version: 1,
        directory_name: target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .into(),
        kind,
        additional_inputs: additional_inputs.to_vec(),
        excluded_paths: excluded_paths.to_vec(),
        manifest: manifest.clone(),
        byte_lengths: byte_lengths(inputs),
        candidate_sha256: candidate_sha256.clone(),
        staged_paths: staged_paths.clone(),
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|_| error("Validation metadata could not be serialized."))?;
    if bytes.len() as u64 > MAX_METADATA_BYTES {
        return Err(error("Validation capture metadata exceeds its limit."));
    }
    fs::write(metadata.join("manifest.json"), &bytes)
        .map_err(|_| error("Validation metadata could not be saved."))?;
    fs::write(metadata.join("complete"), Sha256Hex::of(&bytes).as_str())
        .map_err(|_| error("Validation capture completion could not be saved."))?;
    if replace {
        if target.exists() {
            fs::remove_dir_all(target)
                .map_err(|_| error("The prior validation build could not be replaced."))?;
        }
        if sidecar.exists() {
            fs::remove_dir_all(&sidecar)
                .map_err(|_| error("The prior validation metadata could not be replaced."))?;
        }
    }
    fs::rename(&data, target)
        .map_err(|_| error("The complete validation copy could not be promoted."))?;
    fs::rename(&metadata, sidecar)
        .map_err(|_| error("The complete validation metadata could not be promoted."))?;
    Ok(Candidate {
        dir: target.to_path_buf(),
        manifest,
        candidate_sha256,
        staged_paths,
    })
}

fn read_metadata(target: &Path) -> Result<CaptureMetadata, CompositionError> {
    reject_links(target)?;
    let sidecar = metadata_dir(target)?;
    reject_links(&sidecar)?;
    for name in ["manifest.json", "complete"] {
        reject_links(&sidecar.join(name))?;
    }
    let path = sidecar.join("manifest.json");
    fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, CompositionError> {
        let file = fs::File::open(path)
            .map_err(|_| error("Validation capture metadata is missing or unreadable."))?;
        let mut bytes = Vec::new();
        file.take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| error("Validation capture metadata is unreadable."))?;
        if bytes.len() as u64 > limit {
            return Err(error("Validation capture metadata exceeds its limit."));
        }
        Ok(bytes)
    }
    let bytes = bounded(&path, MAX_METADATA_BYTES)?;
    let completion = bounded(&sidecar.join("complete"), 64)?;
    if completion != Sha256Hex::of(&bytes).as_str().as_bytes() {
        return Err(error(
            "Validation capture metadata is incomplete or corrupt.",
        ));
    }
    let record: CaptureMetadata = serde_json::from_slice(&bytes)
        .map_err(|_| error("Validation capture metadata is invalid."))?;
    if record.version != 1
        || record.directory_name
            != target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
        || record.manifest.len() > MAX_CANDIDATE_FILES
        || record.manifest.keys().ne(record.byte_lengths.keys())
        || record
            .byte_lengths
            .values()
            .any(|length| *length > MAX_FILE_BYTES)
        || record.byte_lengths.values().copied().sum::<u64>() > MAX_CANDIDATE_BYTES
        || record
            .staged_paths
            .iter()
            .any(|path| !record.manifest.contains_key(path))
        || (record.kind == CopyKind::Baseline && !record.staged_paths.is_empty())
        || manifest_digest(record.manifest.iter()) != record.candidate_sha256
    {
        return Err(error(
            "Validation capture metadata does not bind this candidate.",
        ));
    }
    Selection::new(&record.additional_inputs)?;
    let excluded = exclusion_keys(&record.excluded_paths)?;
    for path in record.manifest.keys().chain(record.staged_paths.iter()) {
        RelPath::new(path.clone())?;
        if protected(path) || is_excluded(path, &excluded) {
            return Err(error("Validation metadata names a protected path."));
        }
    }
    Ok(record)
}

/// Freezes the qualified workspace inputs once. Assets and vendored files require
/// explicit relative glob patterns. Existing baselines are never overwritten.
pub fn capture_baseline(
    root: &Path,
    target: &Path,
    additional_inputs: &[String],
) -> Result<Candidate, CompositionError> {
    capture_baseline_excluding(root, target, additional_inputs, &[])
}

/// Freezes a baseline while omitting exact host-owned generated document files.
/// All other documentation remains a validation input. Exclusions are captured
/// before execution and are retained by build copies and restart verification.
pub fn capture_baseline_excluding(
    root: &Path,
    target: &Path,
    additional_inputs: &[String],
    excluded_paths: &[String],
) -> Result<Candidate, CompositionError> {
    let root = canonical_root(root)?;
    let target = target_path(target)?;
    separate(&root, &target)?;
    let inputs = inventory(&root, additional_inputs, &[], excluded_paths, true)?;
    let again = inventory(&root, additional_inputs, &[], excluded_paths, true)?;
    if input_manifest(&inputs) != input_manifest(&again) {
        return Err(error(
            "Validation inputs changed while the baseline was captured.",
        ));
    }
    write_copy(
        &target,
        &inputs,
        additional_inputs,
        excluded_paths,
        Vec::new(),
        CopyKind::Baseline,
        false,
    )
}

/// Restores an immutable baseline after restart only when its completion record
/// and every selected byte still match. It never rereads the live checkout.
pub fn load_baseline(target: &Path) -> Result<Candidate, CompositionError> {
    let target = canonical_root(target)?;
    let record = read_metadata(&target)?;
    if record.kind != CopyKind::Baseline {
        return Err(error("The validation copy is not an immutable baseline."));
    }
    let candidate = Candidate {
        dir: target,
        manifest: record.manifest,
        candidate_sha256: record.candidate_sha256,
        staged_paths: record.staged_paths,
    };
    verify_candidate(&candidate)?;
    Ok(candidate)
}

/// Builds a disposable byte copy plus approved textual outputs. No hardlinks
/// connect it to the baseline, and a repair reuses the same frozen inputs.
pub fn materialize_from_baseline(
    baseline: &Candidate,
    staged: &[(RelPath, String)],
    target: &Path,
) -> Result<Candidate, CompositionError> {
    verify_candidate(baseline)?;
    let metadata = read_metadata(&baseline.dir)?;
    if metadata.kind != CopyKind::Baseline {
        return Err(error("A build must originate from an immutable baseline."));
    }
    let source = canonical_root(&baseline.dir)?;
    let target = target_path(target)?;
    separate(&source, &target)?;
    let mut inputs = inventory(
        &source,
        &metadata.additional_inputs,
        &metadata.staged_paths,
        &metadata.excluded_paths,
        false,
    )?;
    // The baseline may change between the initial integrity check and this
    // second read. Bind the bytes actually copied, rather than trusting the
    // earlier check of the same paths.
    if input_manifest(&inputs) != baseline.manifest {
        return Err(error(
            "The immutable baseline changed before materialization.",
        ));
    }
    let excluded = exclusion_keys(&metadata.excluded_paths)?;
    let mut staged_paths = Vec::new();
    for (path, content) in staged {
        if protected(path.as_str()) || is_excluded(path.as_str(), &excluded) {
            return Err(error("A staged validation output names a protected path."));
        }
        if content.len() as u64 > MAX_FILE_BYTES {
            return Err(error("A staged validation output exceeds 1 MiB."));
        }
        let key = path.canonical_key();
        if content.contains('\0')
            || inputs.iter().any(|(old, input)| {
                RelPath::new(old.as_str()).is_ok_and(|old| old.canonical_key() == key)
                    && (input.bytes.contains(&0) || std::str::from_utf8(&input.bytes).is_err())
            })
        {
            return Err(error(
                "Opaque assets cannot be replaced by staged textual outputs.",
            ));
        }
        let permissions = match inputs
            .iter()
            .find(|(old, _)| RelPath::new(old.as_str()).is_ok_and(|old| old.canonical_key() == key))
        {
            Some((_, input)) => input.permissions.clone(),
            None => {
                let mut permissions = fs::metadata(&source)
                    .map_err(|_| error("The baseline root is unavailable."))?
                    .permissions();
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    permissions.set_mode(0o644);
                }
                #[cfg(windows)]
                {
                    // This Windows-only flag does not change Unix permission bits.
                    #[allow(clippy::permissions_set_readonly_false)]
                    permissions.set_readonly(false);
                }
                permissions
            }
        };
        inputs.retain(|old, _| {
            RelPath::new(old.as_str()).is_ok_and(|old| old.canonical_key() != key)
        });
        inputs.insert(
            path.as_str().to_owned(),
            Input {
                bytes: content.as_bytes().to_vec(),
                permissions,
            },
        );
        staged_paths.push(path.as_str().to_owned());
    }
    let total: u64 = inputs.values().map(|input| input.bytes.len() as u64).sum();
    if inputs.len() > MAX_CANDIDATE_FILES || total > MAX_CANDIDATE_BYTES {
        return Err(error(
            "Staged validation inputs exceed the candidate limits.",
        ));
    }
    write_copy(
        &target,
        &inputs,
        &metadata.additional_inputs,
        &metadata.excluded_paths,
        staged_paths,
        CopyKind::Materialized,
        true,
    )
}

/// Verifies the entire conservative input set, including additions, removals and
/// opaque assets. Generated directories remain outside the validation input set.
pub fn verify_candidate(candidate: &Candidate) -> Result<(), CompositionError> {
    let root = canonical_root(&candidate.dir)?;
    let metadata = read_metadata(&root)?;
    if metadata.manifest != candidate.manifest
        || metadata.candidate_sha256 != candidate.candidate_sha256
        || metadata.staged_paths != candidate.staged_paths
    {
        return Err(error(
            "The candidate no longer matches its capture metadata.",
        ));
    }
    let current = inventory(
        &root,
        &metadata.additional_inputs,
        &metadata.staged_paths,
        &metadata.excluded_paths,
        false,
    )?;
    if input_manifest(&current) != candidate.manifest
        || byte_lengths(&current) != metadata.byte_lengths
    {
        return Err(error(
            "Validation inputs were added, removed or modified after capture.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, path: &str, bytes: impl AsRef<[u8]>) {
        let target = root.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }

    fn workspace(root: &Path) {
        write(
            root,
            "Cargo.toml",
            "[workspace]\nmembers=[\"crates/a\",\"crates/b\"]\n",
        );
        write(root, "Cargo.lock", "version=4\n");
        write(
            root,
            "crates/a/Cargo.toml",
            "[package]\nname=\"a\"\nversion=\"0.1.0\"\n",
        );
        write(
            root,
            "crates/b/Cargo.toml",
            "[package]\nname=\"b\"\nversion=\"0.1.0\"\n",
        );
        write(
            root,
            "crates/a/src/lib.rs",
            "pub const ASSET: &[u8] = include_bytes!(\"../assets/icon.bin\");\n",
        );
        write(root, "crates/a/assets/icon.bin", [0, 1, 255, 2]);
        write(root, "crates/b/src/bin/tool.rs", "fn main() {}\n");
        write(
            root,
            "crates/b/fixtures/events.jsonl",
            "{\"event\":\"ok\"}\n",
        );
        write(root, ".cargo/config", "[net]\noffline=true\n");
        write(root, "rust-toolchain", "1.98.0\n");
        write(root, "resources/app.rc", "1 ICON \"app.ico\"\n");
        write(root, "resources/shader.hlsl", "// shader\n");
    }

    #[test]
    fn captures_workspace_bytes_and_restores_them_without_changing_model_context() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        workspace(root.path());
        let baseline = capture_baseline(
            root.path(),
            &data.path().join("baseline"),
            &["crates/a/assets/*.bin".into()],
        )
        .unwrap();
        for path in [
            "Cargo.lock",
            "rust-toolchain",
            ".cargo/config",
            "crates/b/src/bin/tool.rs",
            "crates/b/fixtures/events.jsonl",
            "resources/app.rc",
            "resources/shader.hlsl",
        ] {
            assert!(baseline.manifest.contains_key(path), "{path}");
        }
        assert_eq!(
            fs::read(baseline.dir.join("crates/a/assets/icon.bin")).unwrap(),
            [0, 1, 255, 2]
        );
        assert_eq!(load_baseline(&baseline.dir).unwrap(), baseline);
        assert!(
            !baseline
                .manifest
                .keys()
                .any(|path| path.contains("hvc-candidate"))
        );
        assert!(
            metadata_dir(&baseline.dir)
                .unwrap()
                .join("complete")
                .is_file()
        );
        let context = crate::repository_context::RepositoryContext::new(root.path()).unwrap();
        let model_files = context
            .safe_snapshot_files(MAX_CANDIDATE_FILES, MAX_CANDIDATE_BYTES)
            .unwrap();
        assert!(
            !model_files
                .iter()
                .any(|file| file.relative_path.ends_with("icon.bin"))
        );
    }

    #[test]
    fn a_build_is_independent_and_repairs_reuse_the_original_baseline() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        workspace(root.path());
        let baseline = capture_baseline(
            root.path(),
            &data.path().join("baseline"),
            &["crates/a/assets/*.bin".into()],
        )
        .unwrap();
        let staged = [(
            RelPath::new("crates/b/src/lib.rs").unwrap(),
            "pub fn value() -> u8 { 1 }\n".into(),
        )];
        let target = data.path().join("build");
        let build = materialize_from_baseline(&baseline, &staged, &target).unwrap();
        verify_candidate(&build).unwrap();
        assert!(
            !fs::metadata(build.dir.join("crates/b/src/lib.rs"))
                .unwrap()
                .permissions()
                .readonly(),
            "a new staged source must remain writable in the disposable build"
        );
        write(&build.dir, "crates/a/assets/icon.bin", [9, 0, 8]);
        assert!(verify_candidate(&build).is_err());
        assert_eq!(
            fs::read(baseline.dir.join("crates/a/assets/icon.bin")).unwrap(),
            [0, 1, 255, 2]
        );
        let repaired = materialize_from_baseline(&baseline, &staged, &target).unwrap();
        verify_candidate(&repaired).unwrap();
        assert_eq!(build.candidate_sha256, repaired.candidate_sha256);
        assert!(!root.path().join("crates/b/src/lib.rs").exists());
        assert!(materialize_from_baseline(&baseline, &[], &baseline.dir).is_err());
        let opaque = [(
            RelPath::new("crates/a/assets/icon.bin").unwrap(),
            "replacement".into(),
        )];
        assert!(materialize_from_baseline(&baseline, &opaque, &target).is_err());
    }

    #[test]
    fn detects_new_removed_and_modified_inputs_but_ignores_build_outputs() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(root.path(), "src/lib.rs", "pub fn a() {}\n");
        write(root.path(), ".gitignore", "ignored.rs\n");
        write(root.path(), "ignored.rs", "not captured\n");
        let baseline = capture_baseline(root.path(), &data.path().join("baseline"), &[]).unwrap();
        assert!(!baseline.manifest.contains_key("ignored.rs"));
        write(&baseline.dir, "target/generated.rs", "generated\n");
        verify_candidate(&baseline).unwrap();
        write(
            &baseline.dir,
            "ignored.rs",
            "new input despite original ignore rule\n",
        );
        assert!(verify_candidate(&baseline).is_err());
        fs::remove_file(baseline.dir.join("ignored.rs")).unwrap();
        fs::remove_file(baseline.dir.join("src/lib.rs")).unwrap();
        assert!(verify_candidate(&baseline).is_err());
        write(&baseline.dir, "src/lib.rs", "pub fn changed() {}\n");
        assert!(verify_candidate(&baseline).is_err());
        assert!(load_baseline(&baseline.dir).is_err());
    }

    #[test]
    fn exact_generated_reports_are_excluded_but_source_documentation_remains_bound() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(root.path(), "src/lib.rs", "pub fn a() {}\n");
        write(
            root.path(),
            "docs/architecture.md",
            "original architecture\n",
        );
        write(
            root.path(),
            "knowledge/pulsations/old-validation.md",
            "old host report\n",
        );
        let exclusions = vec![
            "knowledge/pulsations/old-validation.md".into(),
            "Knowledge/Pulsations/New-Validation.md".into(),
            // An exact directory name never excludes its contents.
            "docs".into(),
        ];
        let baseline = capture_baseline_excluding(
            root.path(),
            &data.path().join("baseline"),
            &["**/*.md".into()],
            &exclusions,
        )
        .unwrap();
        assert!(baseline.manifest.contains_key("docs/architecture.md"));
        assert!(
            !baseline
                .manifest
                .contains_key("knowledge/pulsations/old-validation.md")
        );
        assert_eq!(
            read_metadata(&baseline.dir).unwrap().excluded_paths,
            exclusions
        );
        write(
            &baseline.dir,
            "knowledge/pulsations/new-validation.md",
            "new host report\n",
        );
        write(
            &baseline.dir,
            "knowledge/pulsations/old-validation.md",
            "updated host report\n",
        );
        verify_candidate(&baseline).unwrap();
        let restored = load_baseline(&baseline.dir).unwrap();
        let build = materialize_from_baseline(&restored, &[], &data.path().join("build")).unwrap();
        assert!(
            !build
                .dir
                .join("knowledge/pulsations/new-validation.md")
                .exists()
        );
        write(
            &build.dir,
            "knowledge/pulsations/new-validation.md",
            "published after checks\n",
        );
        verify_candidate(&build).unwrap();
        let staged = [(
            RelPath::new("knowledge/pulsations/new-validation.md").unwrap(),
            "not a source output".into(),
        )];
        assert!(materialize_from_baseline(&baseline, &staged, &data.path().join("other")).is_err());
        write(
            &baseline.dir,
            "docs/architecture.md",
            "changed source documentation\n",
        );
        assert!(verify_candidate(&baseline).is_err());
    }

    #[test]
    fn excludes_secrets_and_generated_directories_even_with_a_broad_allowlist() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(root.path(), "src/bin/tool.rs", "fn main() {}\n");
        for path in [
            ".env",
            ".aws/config",
            ".git/config",
            "target/generated.rs",
            "build/a.rs",
            "bin/generated.rs",
            "secrets/token.txt",
        ] {
            write(root.path(), path, "must not copy\n");
        }
        write(
            root.path(),
            "vendor/local/Cargo.toml",
            "[package]\nname=\"local\"\n",
        );
        write(
            root.path(),
            "vendor/local/src/lib.rs",
            "pub fn local() {}\n",
        );
        let plain = capture_baseline(root.path(), &data.path().join("plain"), &[]).unwrap();
        assert!(
            !plain
                .manifest
                .keys()
                .any(|path| path.starts_with("vendor/"))
        );
        let baseline =
            capture_baseline(root.path(), &data.path().join("baseline"), &["**/*".into()]).unwrap();
        assert!(baseline.manifest.contains_key("src/bin/tool.rs"));
        assert!(baseline.manifest.contains_key("vendor/local/src/lib.rs"));
        assert_eq!(baseline.manifest.len(), 3);
        verify_candidate(&baseline).unwrap();
    }

    #[test]
    fn declared_inputs_and_snapshot_metadata_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(root.path(), "src/lib.rs", "pub fn a() {}\n");
        for pattern in [
            "missing/*.bin",
            "../outside.bin",
            "/absolute.bin",
            "C:/outside.bin",
            "a\\b.bin",
        ] {
            assert!(
                capture_baseline(
                    root.path(),
                    &data.path().join("rejected"),
                    &[pattern.into()]
                )
                .is_err()
            );
        }
        let baseline = capture_baseline(root.path(), &data.path().join("baseline"), &[]).unwrap();
        assert!(capture_baseline(root.path(), &baseline.dir, &[]).is_err());
        let sidecar = metadata_dir(&baseline.dir).unwrap();
        fs::write(sidecar.join("complete"), "incomplete").unwrap();
        assert!(load_baseline(&baseline.dir).is_err());
        assert!(verify_candidate(&baseline).is_err());
    }

    #[test]
    fn capture_metadata_binds_file_lengths_and_derives_staged_origin() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(root.path(), "src/lib.rs", "pub fn a() {}\n");
        let baseline = capture_baseline(root.path(), &data.path().join("baseline"), &[]).unwrap();
        let original = read_metadata(&baseline.dir).unwrap();
        assert_eq!(original.byte_lengths["src/lib.rs"], 14);
        assert!(original.staged_paths.is_empty());
        let staged = [(
            RelPath::new("src/lib.rs").unwrap(),
            "pub fn b() {}\n".into(),
        )];
        let build =
            materialize_from_baseline(&baseline, &staged, &data.path().join("build")).unwrap();
        let metadata = read_metadata(&build.dir).unwrap();
        assert_eq!(metadata.staged_paths, vec!["src/lib.rs"]);
        assert_eq!(metadata.byte_lengths["src/lib.rs"], 14);
        let mut corrupted = read_metadata(&baseline.dir).unwrap();
        corrupted.byte_lengths.insert("src/lib.rs".into(), 15);
        let bytes = serde_json::to_vec(&corrupted).unwrap();
        let sidecar = metadata_dir(&baseline.dir).unwrap();
        fs::write(sidecar.join("manifest.json"), &bytes).unwrap();
        fs::write(sidecar.join("complete"), Sha256Hex::of(&bytes).as_str()).unwrap();
        assert!(load_baseline(&baseline.dir).is_err());
    }

    #[test]
    fn never_replaces_unowned_paths_or_captures_into_the_workspace() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(root.path(), "src/lib.rs", "pub fn a() {}\n");
        let overlapping = root.path().join("new/storage/baseline");
        assert!(capture_baseline(root.path(), &overlapping, &[]).is_err());
        assert!(!root.path().join("new").exists());
        let baseline = capture_baseline(root.path(), &data.path().join("baseline"), &[]).unwrap();
        let unowned = data.path().join("unowned");
        write(&unowned, "keep.txt", "keep");
        assert!(materialize_from_baseline(&baseline, &[], &unowned).is_err());
        assert_eq!(
            fs::read_to_string(unowned.join("keep.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn preserves_ignored_cargo_contract_files_and_rejects_oversized_inputs() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(root.path(), ".gitignore", "Cargo.lock\n.cargo/\n");
        write(root.path(), "Cargo.lock", "version=4\n");
        write(root.path(), ".cargo/config", "[net]\noffline=true\n");
        let baseline = capture_baseline(root.path(), &data.path().join("baseline"), &[]).unwrap();
        assert!(baseline.manifest.contains_key("Cargo.lock"));
        assert!(baseline.manifest.contains_key(".cargo/config"));
        write(
            root.path(),
            "huge.txt",
            vec![b'x'; MAX_FILE_BYTES as usize + 1],
        );
        assert!(capture_baseline(root.path(), &data.path().join("huge"), &[]).is_err());
    }

    #[test]
    fn opaque_text_fixture_diagnostic_identifies_only_the_relative_input() {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(root.path(), "fixtures/hostile.txt", b"fixture\0value");
        let failure =
            capture_baseline(root.path(), &data.path().join("rejected"), &[]).unwrap_err();
        assert!(failure.message.contains("fixtures/hostile.txt"));
        assert!(!failure.message.contains(root.path().to_str().unwrap()));
        assert!(!failure.message.contains("fixture\0value"));
        let baseline = capture_baseline(
            root.path(),
            &data.path().join("baseline"),
            &["fixtures/hostile.txt".into()],
        )
        .unwrap();
        verify_candidate(&baseline).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_links_and_windows_identity_collisions() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        write(root.path(), "src/lib.rs", "pub fn a() {}\n");
        write(root.path(), "src/Lib.rs", "pub fn b() {}\n");
        assert!(capture_baseline(root.path(), &data.path().join("collision"), &[]).is_err());
        fs::remove_file(root.path().join("src/Lib.rs")).unwrap();
        symlink(data.path(), root.path().join("linked")).unwrap();
        assert!(
            capture_baseline(
                root.path(),
                &data.path().join("link"),
                &["linked/**".into()]
            )
            .is_err()
        );
        fs::remove_file(root.path().join("linked")).unwrap();
        symlink(
            root.path().join("src/lib.rs"),
            root.path().join("asset.bin"),
        )
        .unwrap();
        assert!(
            capture_baseline(
                root.path(),
                &data.path().join("asset"),
                &["asset.bin".into()]
            )
            .is_err()
        );
    }
}
