use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use ignore::{DirEntry, WalkBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::orchestration::{
    CollectionBudget, ContextCoverage, CoverageState, DocumentationAvailability,
};

const MAX_TREE_DEPTH: usize = 8;
const MAX_TREE_ENTRIES: usize = 1_000;
const MAX_QUERY_CHARS: usize = 256;
const MAX_SEARCH_RESULTS: usize = 50;
const MAX_READ_LINES: usize = 400;
const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryError(pub String);

impl Display for RepositoryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RepositoryError {}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListRepositoryTreeArgs {
    pub path: Option<String>,
    pub depth: Option<usize>,
    pub max_entries: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRepositoryArgs {
    pub query: String,
    pub path: Option<String>,
    pub mode: Option<String>,
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadRepositoryFileArgs {
    pub path: String,
    pub start_line: Option<usize>,
    pub end_line: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryTreeResult {
    pub entries: Vec<RepositoryTreeEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryTreeEntry {
    pub path: String,
    pub kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositorySearchResult {
    pub matches: Vec<RepositorySearchMatch>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositorySearchMatch {
    pub path: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryFileResult {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub content: String,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct RepositoryContext {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InventoryFile {
    pub(crate) relative_path: String,
    pub(crate) size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SafeSnapshotFile {
    pub(crate) relative_path: String,
    pub(crate) content: String,
}

impl RepositoryContext {
    /// Host-only original read for explicit local exception approval. Model tools use projections.
    pub(crate) fn read_original_for_sanitization(
        &self,
        relative: &str,
    ) -> Result<String, RepositoryError> {
        let path = self.resolve_relative(relative, false)?;
        self.read_safe_text(&path)
    }
    pub(crate) fn is_code_relative_path(relative: &str) -> bool {
        let path = Path::new(relative);
        !path.is_absolute()
            && !relative.replace('\\', "/").starts_with("knowledge/")
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "rs" | "ts"
                            | "tsx"
                            | "js"
                            | "jsx"
                            | "py"
                            | "go"
                            | "java"
                            | "kt"
                            | "kts"
                            | "cs"
                            | "c"
                            | "h"
                            | "cc"
                            | "cpp"
                            | "hpp"
                            | "rb"
                            | "php"
                            | "swift"
                            | "sql"
                            | "sh"
                            | "ps1"
                    )
                })
            && is_code_path(path)
    }

    pub fn new(root: &Path) -> Result<Self, RepositoryError> {
        let canonical = root
            .canonicalize()
            .map_err(|error| RepositoryError(format!("Invalid workspace: {error}")))?;
        if !canonical.is_dir() {
            return Err(RepositoryError("The workspace must be a directory.".into()));
        }
        Ok(Self { root: canonical })
    }

    pub fn list_tree(
        &self,
        args: ListRepositoryTreeArgs,
    ) -> Result<RepositoryTreeResult, RepositoryError> {
        let scope = self.resolve_relative(args.path.as_deref().unwrap_or(""), true)?;
        let depth = args.depth.unwrap_or(4).min(MAX_TREE_DEPTH);
        let max_entries = args.max_entries.unwrap_or(300).clamp(1, MAX_TREE_ENTRIES);
        let mut entries = Vec::new();
        let mut truncated = false;

        for item in self.walk(&scope, depth).skip(1) {
            let entry = item.map_err(|error| RepositoryError(error.to_string()))?;
            if entry.file_type().is_some_and(|kind| kind.is_file())
                && self.read_safe_text(entry.path()).is_err()
            {
                continue;
            }
            if entries.len() == max_entries {
                truncated = true;
                break;
            }
            let relative = self.relative_display(entry.path())?;
            entries.push(RepositoryTreeEntry {
                path: relative,
                kind: if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                    "directory"
                } else {
                    "file"
                },
            });
        }

        Ok(RepositoryTreeResult { entries, truncated })
    }

    pub fn search(
        &self,
        args: SearchRepositoryArgs,
    ) -> Result<RepositorySearchResult, RepositoryError> {
        self.search_with_sanitizer(args, |_, content| {
            crate::secret_redactor::redact_context(content)
        })
    }

    pub(crate) fn search_with_sanitizer(
        &self,
        args: SearchRepositoryArgs,
        mut sanitize: impl FnMut(&str, &str) -> Result<crate::secret_redactor::ContextRedaction, String>,
    ) -> Result<RepositorySearchResult, RepositoryError> {
        if args.query.is_empty() || args.query.chars().count() > MAX_QUERY_CHARS {
            return Err(RepositoryError(
                "The search must have between 1 and 256 characters.".into(),
            ));
        }
        let scope = self.resolve_relative(args.path.as_deref().unwrap_or(""), true)?;
        let max_results = args.max_results.unwrap_or(20).clamp(1, MAX_SEARCH_RESULTS);
        let mode = args.mode.as_deref().unwrap_or("literal");
        let expression = match mode {
            "literal" => None,
            "regex" => Some(
                Regex::new(&args.query)
                    .map_err(|error| RepositoryError(format!("Invalid regex: {error}")))?,
            ),
            _ => {
                return Err(RepositoryError(
                    "Search mode must be 'literal' or 'regex'.".into(),
                ));
            }
        };

        let mut matches = Vec::new();
        let mut truncated = false;
        for item in self.walk(&scope, MAX_TREE_DEPTH + 64) {
            let entry = item.map_err(|error| RepositoryError(error.to_string()))?;
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let content = match self.read_safe_text(entry.path()) {
                Ok(content) => content,
                Err(_) => continue,
            };
            // A line excerpt must never expose a fragment of a multiline
            // secret whose terminator lies beyond the excerpt's budget.
            let relative = self.relative_display(entry.path())?;
            let projected = sanitize(&relative, &content).map_err(RepositoryError)?;
            let projected_lines: Vec<_> = projected.text.lines().collect();
            for (index, line) in content.lines().enumerate() {
                let found = expression
                    .as_ref()
                    .map_or_else(|| line.contains(&args.query), |regex| regex.is_match(line));
                if !found {
                    continue;
                }
                if matches.len() == max_results {
                    truncated = true;
                    break;
                }
                matches.push(RepositorySearchMatch {
                    path: relative.clone(),
                    line: index + 1,
                    text: truncate_chars(
                        projected_lines.get(index).copied().unwrap_or("").trim(),
                        500,
                    ),
                });
            }
            if truncated {
                break;
            }
        }

        Ok(RepositorySearchResult { matches, truncated })
    }

    pub fn read_file(
        &self,
        args: ReadRepositoryFileArgs,
    ) -> Result<RepositoryFileResult, RepositoryError> {
        self.read_file_with_sanitizer(args, |_, content| {
            crate::secret_redactor::redact_context(content)
        })
    }

    pub(crate) fn read_file_with_sanitizer(
        &self,
        args: ReadRepositoryFileArgs,
        sanitize: impl FnOnce(&str, &str) -> Result<crate::secret_redactor::ContextRedaction, String>,
    ) -> Result<RepositoryFileResult, RepositoryError> {
        let path = self.resolve_relative(&args.path, false)?;
        let content = self.read_safe_text(&path)?;
        let relative = self.relative_display(&path)?;
        let content = sanitize(&relative, &content).map_err(RepositoryError)?.text;
        let start = args.start_line.unwrap_or(1).max(1);
        let requested_end = args.end_line.unwrap_or(start + MAX_READ_LINES - 1);
        if requested_end < start {
            return Err(RepositoryError(
                "endLine must be greater than or equal to startLine.".into(),
            ));
        }
        let end = requested_end.min(start + MAX_READ_LINES - 1);
        let mut rendered = String::new();
        let mut actual_end = start.saturating_sub(1);
        let mut truncated = requested_end > end;
        for (index, line) in content.lines().enumerate() {
            let line_number = index + 1;
            if line_number < start {
                continue;
            }
            if line_number > end {
                truncated = true;
                break;
            }
            let fragment = format!("{line_number:>6} | {line}\n");
            if rendered.len() + fragment.len() > MAX_READ_BYTES {
                truncated = true;
                break;
            }
            rendered.push_str(&fragment);
            actual_end = line_number;
        }

        Ok(RepositoryFileResult {
            path: relative,
            start_line: start,
            end_line: actual_end,
            content: rendered,
            truncated,
        })
    }

    fn resolve_relative(&self, input: &str, directory: bool) -> Result<PathBuf, RepositoryError> {
        let requested = Path::new(input);
        if requested.is_absolute()
            || requested.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(RepositoryError(
                "The tool accepts only relative paths without '..'.".into(),
            ));
        }
        let candidate = self.root.join(requested);
        self.reject_symlink_components(&candidate)?;
        let canonical = candidate
            .canonicalize()
            .map_err(|_| RepositoryError("Path not found in the repository.".into()))?;
        if !canonical.starts_with(&self.root) {
            return Err(RepositoryError(
                "Access outside the repository root was blocked.".into(),
            ));
        }
        if canonical != self.root && !self.path_is_visible(&canonical) {
            return Err(RepositoryError(
                "Path ignored or excluded by the safety rules.".into(),
            ));
        }
        if directory && !canonical.is_dir() {
            return Err(RepositoryError("The path is not a directory.".into()));
        }
        if !directory && !canonical.is_file() {
            return Err(RepositoryError("The path is not a file.".into()));
        }
        Ok(canonical)
    }

    fn path_is_visible(&self, path: &Path) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        self.walk(parent, 1)
            .filter_map(Result::ok)
            .any(|entry| entry.path() == path)
    }

    fn reject_symlink_components(&self, candidate: &Path) -> Result<(), RepositoryError> {
        let relative = candidate
            .strip_prefix(&self.root)
            .map_err(|_| RepositoryError("Path outside the repository.".into()))?;
        let mut current = self.root.clone();
        for component in relative.components() {
            current.push(component.as_os_str());
            if fs::symlink_metadata(&current)
                .map(|metadata| is_link(&metadata))
                .unwrap_or(false)
            {
                return Err(RepositoryError("Symbolic links are not followed.".into()));
            }
        }
        Ok(())
    }

    pub(crate) fn read_document(&self, relative: &str) -> Result<String, RepositoryError> {
        let path = self.resolve_relative(relative, false)?;
        self.read_safe_text(&path)
    }

    pub(crate) fn capture_code_source(
        &self,
        project_id: &str,
        relative: &crate::orchestration::RelativeSourcePath,
    ) -> Result<crate::orchestration::SourceSnapshot, RepositoryError> {
        let path = self.resolve_relative(relative.as_str(), false)?;
        if !is_code_path(&path) {
            return Err(RepositoryError(
                "The path is not an eligible source file.".into(),
            ));
        }
        crate::orchestration::SourceSnapshot::from_text(
            project_id,
            relative.clone(),
            self.read_safe_text(&path)?,
        )
        .map_err(|error| RepositoryError(error.to_string()))
    }

    /// Validate a physical reference independently of its eligibility as code.
    /// Documents use the same containment, visibility and symlink checks.
    pub(crate) fn validate_file_reference(
        &self,
        relative: &crate::orchestration::RelativeSourcePath,
    ) -> Result<(), RepositoryError> {
        self.resolve_relative(relative.as_str(), false)?;
        Ok(())
    }

    pub(crate) fn capture_exact_code_sources(
        &self,
        project: &str,
        paths: &[crate::orchestration::RelativeSourcePath],
        budget: crate::orchestration::CollectionBudget,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<
        (
            Vec<crate::orchestration::SourceSnapshot>,
            crate::orchestration::ContextCoverage,
        ),
        crate::orchestration::ContractError,
    > {
        use crate::orchestration::*;
        budget.validate()?;
        let started = std::time::Instant::now();
        let mut coverage = ContextCoverage {
            state: CoverageState::Complete,
            documentation: DocumentationAvailability::Available,
            enumerated_files: 0,
            read_bytes: 0,
            truncated: false,
            diagnostics: vec!["exact_paths: sources resolved by the Vault index.".into()],
        };
        let mut sources = Vec::new();
        let mut seen = HashSet::new();
        for relative in paths {
            if cancellation.is_cancelled() {
                return Err(ContractError::Cancelled);
            }
            if started.elapsed().as_millis() >= u128::from(budget.max_duration_ms) {
                coverage.truncated = true;
                break;
            }
            if !seen.insert(relative.clone()) {
                continue;
            }
            coverage.enumerated_files = coverage.enumerated_files.saturating_add(1);
            let path = match self.resolve_relative(relative.as_str(), false) {
                Ok(path) if is_code_path(&path) => path,
                // Existing non-code references are valid, but do not belong in
                // the code projection or imply incomplete code coverage.
                Ok(_) => continue,
                Err(error) => {
                    coverage.state = CoverageState::Partial;
                    coverage.diagnostics.push(format!(
                        "broken_file_reference: {}: {error}",
                        relative.as_str()
                    ));
                    continue;
                }
            };
            if sources.len() >= budget.max_package_files as usize
                || coverage.read_bytes >= budget.max_read_bytes
            {
                coverage.truncated = true;
                break;
            }
            let limit = budget
                .max_file_bytes
                .min(budget.max_read_bytes.saturating_sub(coverage.read_bytes));
            let read = self.read_bounded_text(&path, limit);
            coverage.read_bytes = coverage
                .read_bytes
                .saturating_add(read.as_ref().map_or(limit, |(_, size)| *size));
            let Ok((content, _)) = read else {
                coverage.state = CoverageState::Partial;
                continue;
            };
            sources.push(SourceSnapshot::from_text(
                project,
                relative.clone(),
                content,
            )?);
        }
        if coverage.truncated {
            coverage.state = CoverageState::Partial;
            coverage
                .diagnostics
                .push("truncated: the exact-resolution budget was reached.".into());
        }
        Ok((sources, coverage))
    }

    pub(crate) fn document_paths(
        &self,
        markdown_only: bool,
    ) -> Result<Vec<String>, RepositoryError> {
        let mut paths = Vec::new();
        for entry in self.walk(&self.root, 72) {
            let entry = entry.map_err(|e| RepositoryError(e.to_string()))?;
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let extension = entry
                .path()
                .extension()
                .and_then(|v| v.to_str())
                .unwrap_or("");
            if extension.eq_ignore_ascii_case("md")
                || (!markdown_only && extension.eq_ignore_ascii_case("txt"))
            {
                if paths.len() >= 10_000 {
                    return Err(RepositoryError(
                        "The document catalog exceeds 10,000 files; the scope is incomplete."
                            .into(),
                    ));
                }
                paths.push(self.relative_display(entry.path())?);
            }
        }
        paths.sort();
        Ok(paths)
    }

    fn walk(&self, scope: &Path, depth: usize) -> ignore::Walk {
        let mut builder = WalkBuilder::new(scope);
        builder
            .hidden(false)
            .parents(true)
            .git_ignore(true)
            .git_exclude(true)
            .git_global(false)
            .require_git(false)
            .follow_links(false)
            .sort_by_file_name(|a, b| a.cmp(b))
            .max_depth(Some(depth));
        builder.filter_entry(allowed_entry);
        builder.build()
    }

    pub(crate) fn inventory_files(
        &self,
        budget: CollectionBudget,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<InventoryFile>, ContextCoverage), RepositoryError> {
        budget
            .validate()
            .map_err(|error| RepositoryError(error.to_string()))?;
        let started = std::time::Instant::now();
        let mut files = Vec::new();
        let mut coverage = ContextCoverage {
            state: CoverageState::Complete,
            documentation: DocumentationAvailability::Absent,
            enumerated_files: 0,
            read_bytes: 0,
            truncated: false,
            diagnostics: vec![
                "Limited heuristic inventory; it does not prove global repository coverage.".into(),
            ],
        };

        for item in self.walk(&self.root, MAX_TREE_DEPTH + 64) {
            if cancellation.is_cancelled() {
                return Err(RepositoryError("Coleta cancelada.".into()));
            }
            if started.elapsed().as_millis() >= u128::from(budget.max_duration_ms) {
                coverage.truncated = true;
                break;
            }
            let entry = match item {
                Ok(entry) => entry,
                Err(error) => {
                    coverage.state = CoverageState::Partial;
                    coverage
                        .diagnostics
                        .push(format!("Entry unavailable during enumeration: {error}"));
                    continue;
                }
            };
            if entry.depth() >= MAX_TREE_DEPTH + 64
                && entry.file_type().is_some_and(|kind| kind.is_dir())
            {
                coverage.truncated = true;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            if coverage.enumerated_files >= budget.max_files {
                coverage.truncated = true;
                break;
            }
            coverage.enumerated_files += 1;
            let relative_path = match self.relative_display(entry.path()) {
                Ok(path) => path,
                Err(error) => {
                    coverage.state = CoverageState::Partial;
                    coverage.diagnostics.push(error.to_string());
                    continue;
                }
            };
            if is_documentation_path(&relative_path) {
                coverage.documentation = DocumentationAvailability::Available;
            }
            let size_bytes = match entry.metadata() {
                Ok(metadata) => metadata.len(),
                Err(error) => {
                    coverage.state = CoverageState::Partial;
                    coverage.diagnostics.push(format!(
                        "Metadata unavailable for `{relative_path}`: {error}"
                    ));
                    continue;
                }
            };
            files.push(InventoryFile {
                relative_path,
                size_bytes,
            });
        }

        if coverage.truncated {
            coverage.state = CoverageState::Partial;
            coverage
                .diagnostics
                .push("truncated: the file, depth or time limit was reached.".into());
        }
        Ok((files, coverage))
    }

    pub(crate) fn safe_snapshot_files(
        &self,
        max_files: usize,
        max_total_bytes: u64,
    ) -> Result<Vec<SafeSnapshotFile>, RepositoryError> {
        let mut files = Vec::new();
        let mut total_bytes = 0u64;
        for item in self.walk(&self.root, MAX_TREE_DEPTH + 64) {
            let entry = item.map_err(|error| RepositoryError(error.to_string()))?;
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            let relative_path = self.relative_display(entry.path())?;
            let Ok(content) = self.read_safe_text(entry.path()) else {
                continue;
            };
            let bytes = content.len() as u64;
            if files.len() >= max_files || total_bytes.saturating_add(bytes) > max_total_bytes {
                return Err(RepositoryError(
                    "The safe workspace exceeds the file or byte limit.".into(),
                ));
            }
            total_bytes = total_bytes.saturating_add(bytes);
            files.push(SafeSnapshotFile {
                relative_path,
                content,
            });
        }
        Ok(files)
    }

    fn read_safe_text(&self, path: &Path) -> Result<String, RepositoryError> {
        self.read_bounded_text(path, MAX_FILE_BYTES)
            .map(|(text, _)| text)
    }

    fn read_bounded_text(&self, path: &Path, limit: u64) -> Result<(String, u64), RepositoryError> {
        self.reject_symlink_components(path)?;
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if is_sensitive_name(&name) || !allowed_text_name(&name) {
            return Err(RepositoryError(
                "A sensitive, binary or unsupported file was blocked.".into(),
            ));
        }
        let file = fs::File::open(path)
            .map_err(|error| RepositoryError(format!("Failed to read the file: {error}")))?;
        let metadata = file
            .metadata()
            .map_err(|e| RepositoryError(e.to_string()))?;
        if !metadata.is_file() || metadata.len() > limit.min(MAX_FILE_BYTES) {
            return Err(RepositoryError("Files over 1 MiB cannot be read.".into()));
        }
        let mut bytes = Vec::new();
        file.take(limit.min(MAX_FILE_BYTES))
            .read_to_end(&mut bytes)
            .map_err(|error| RepositoryError(format!("Failed to read the file: {error}")))?;
        self.reject_symlink_components(path)?;
        if bytes.contains(&0) || bytes.len() as u64 != metadata.len() {
            return Err(RepositoryError(
                "The file is binary or changed while being read.".into(),
            ));
        }
        let size = bytes.len() as u64;
        String::from_utf8(bytes)
            .map(|text| (text, size))
            .map_err(|_| RepositoryError("A binary or non-UTF-8 file was blocked.".into()))
    }

    /// One bounded enumeration, no per-keyword scans and no file reads in the walker.
    /// All evidence is derived downstream from these captured bytes.
    pub(crate) fn capture_code_sources(
        &self,
        project: &str,
        budget: crate::orchestration::CollectionBudget,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<
        (
            Vec<crate::orchestration::SourceSnapshot>,
            crate::orchestration::ContextCoverage,
        ),
        crate::orchestration::ContractError,
    > {
        use crate::orchestration::*;
        let started = std::time::Instant::now();
        let mut coverage = ContextCoverage {
            state: CoverageState::Complete,
            documentation: DocumentationAvailability::Unknown,
            enumerated_files: 0,
            read_bytes: 0,
            truncated: false,
            diagnostics: vec![
                "Limited lexical collection; it proves neither missing dependencies nor global coverage."
                    .into(),
            ],
        };
        let mut sources = Vec::new();
        for item in self.walk(&self.root, MAX_TREE_DEPTH + 64) {
            if cancellation.is_cancelled() {
                return Err(ContractError::Cancelled);
            }
            if started.elapsed().as_millis() >= u128::from(budget.max_duration_ms) {
                coverage.truncated = true;
                break;
            }
            let Ok(entry) = item else {
                coverage.state = CoverageState::Partial;
                continue;
            };
            if entry.depth() >= MAX_TREE_DEPTH + 64
                && entry.file_type().is_some_and(|kind| kind.is_dir())
            {
                coverage.truncated = true;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file()) || !is_code_path(entry.path())
            {
                continue;
            }
            if coverage.enumerated_files >= budget.max_files
                || coverage.read_bytes >= budget.max_read_bytes
            {
                coverage.truncated = true;
                break;
            }
            coverage.enumerated_files += 1;
            let limit = budget
                .max_file_bytes
                .min(budget.max_read_bytes - coverage.read_bytes);
            let read = self.read_bounded_text(entry.path(), limit);
            // Includes failed UTF-8 reads and growth between enumeration and open.
            coverage.read_bytes += read.as_ref().map_or(limit, |(_, size)| *size);
            let Ok((content, _)) = read else {
                coverage.state = CoverageState::Partial;
                continue;
            };
            let Ok(path) = self.relative_display(entry.path()).and_then(|p| {
                RelativeSourcePath::new(p).map_err(|e| RepositoryError(e.to_string()))
            }) else {
                coverage.state = CoverageState::Partial;
                continue;
            };
            sources.push(SourceSnapshot::from_text(project, path, content)?);
        }
        if coverage.truncated {
            coverage.state = CoverageState::Partial;
            coverage
                .diagnostics
                .push("truncated: the enumeration, byte or time limit was reached.".into());
        }
        if coverage.state == CoverageState::Partial {
            coverage.diagnostics.push(
                "Some sources were not read; unavailable files are not negative evidence.".into(),
            );
        }
        Ok((sources, coverage))
    }

    fn relative_display(&self, path: &Path) -> Result<String, RepositoryError> {
        path.strip_prefix(&self.root)
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .map_err(|_| RepositoryError("Path outside the repository.".into()))
    }
}

fn allowed_entry(entry: &DirEntry) -> bool {
    if entry.metadata().is_ok_and(|m| is_link(&m)) {
        return false;
    }
    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
    if entry.file_type().is_some_and(|kind| kind.is_dir()) {
        return !excluded_directories().contains(name.as_str())
            && !entry.file_type().is_some_and(|kind| kind.is_symlink());
    }
    !is_sensitive_name(&name)
        && allowed_text_name(&name)
        && !entry.file_type().is_some_and(|kind| kind.is_symlink())
}

fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn is_code_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    !name.starts_with("readme")
        && !matches!(
            name.as_str(),
            "license" | ".gitignore" | ".gitattributes" | ".editorconfig"
        )
        && ![".md", ".txt", ".csv", ".svg", ".min.js", ".min.css"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        && !path
            .components()
            .any(|p| matches!(p.as_os_str().to_str(), Some("knowledge" | "docs" | "doc")))
}

fn excluded_directories() -> HashSet<&'static str> {
    HashSet::from([
        ".git",
        ".hard-vibe-code",
        ".agents",
        ".claude",
        ".codex",
        ".gemini",
        ".opencode",
        ".aws",
        ".ssh",
        "node_modules",
        "vendor",
        "target",
        "dist",
        "build",
        "out",
        "bin",
        "obj",
        ".next",
        ".nuxt",
        ".svelte-kit",
        ".cache",
        ".turbo",
        ".vite",
        ".parcel-cache",
        "coverage",
        ".coverage",
        "__pycache__",
        ".pytest_cache",
        ".mypy_cache",
        ".gradle",
        ".idea",
    ])
}

fn is_sensitive_name(name: &str) -> bool {
    if name.starts_with("credentials.") || name.starts_with("secrets.") {
        return true;
    }
    if name.starts_with(".env") {
        return true;
    }
    matches!(
        name,
        ".npmrc"
            | ".pypirc"
            | ".netrc"
            | "credentials"
            | "credentials.json"
            | "credentials.yaml"
            | "credentials.yml"
            | "credentials.toml"
            | "secrets.json"
            | "secrets.yaml"
            | "secrets.yml"
            | "secrets.toml"
            | "id_rsa"
            | "id_dsa"
            | "id_ecdsa"
            | "id_ed25519"
    ) || name.contains("service-account")
        || name.contains("private-key")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.ends_with(".p12")
        || name.ends_with(".pfx")
        || name.ends_with(".crt")
        || name.ends_with(".cer")
}

fn allowed_text_name(name: &str) -> bool {
    const EXTENSIONS: &[&str] = &[
        "c", "cc", "cpp", "cs", "css", "csv", "go", "graphql", "h", "hpp", "html", "java", "js",
        "json", "jsx", "kt", "kts", "md", "php", "proto", "ps1", "py", "rb", "rs", "scss", "sh",
        "csproj", "gradle", "mod", "sln", "sql", "svg", "swift", "toml", "ts", "tsx", "txt", "xml",
        "yaml", "yml",
    ];
    let special = matches!(
        name,
        "dockerfile"
            | "makefile"
            | "license"
            | "go.mod"
            | "go.work"
            | ".gitignore"
            | ".gitattributes"
            | ".editorconfig"
    ) || name.starts_with("readme")
        || (name.starts_with(".env") && !is_sensitive_name(name));
    special
        || Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| EXTENSIONS.contains(&extension))
}

fn is_documentation_path(relative_path: &str) -> bool {
    let normalized = relative_path.to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or(&normalized);
    name.starts_with("readme")
        || name.ends_with(".md")
        || normalized.starts_with("docs/")
        || normalized.contains("/docs/")
}

fn truncate_chars(value: &str, max: usize) -> String {
    let mut truncated: String = value.chars().take(max).collect();
    if value.chars().count() > max {
        truncated.push('…');
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_code_context_skips_valid_documents_but_reports_broken_references() {
        use crate::orchestration::{CollectionBudget, CoverageState, RelativeSourcePath};
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join("docs")).unwrap();
        fs::write(temp.path().join("docs/spec.md"), "# Specification\n").unwrap();
        fs::write(temp.path().join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(temp.path().join(".env"), "TOKEN=fixture\n").unwrap();
        let repository = RepositoryContext::new(temp.path()).unwrap();
        let path = |value: &str| RelativeSourcePath::new(value).unwrap();
        let budget = CollectionBudget {
            max_package_files: 1,
            ..Default::default()
        };
        let cancel = CancellationToken::new();
        for paths in [
            vec![path("docs/spec.md"), path("main.rs")],
            vec![path("main.rs"), path("docs/spec.md")],
            vec![path("docs/spec.md")],
        ] {
            let (sources, coverage) = repository
                .capture_exact_code_sources("fixture", &paths, budget, &cancel)
                .unwrap();
            assert_eq!(sources.len(), usize::from(paths.len() == 2));
            assert_eq!(coverage.state, CoverageState::Complete);
            assert!(!coverage.truncated);
            assert!(
                !coverage
                    .diagnostics
                    .iter()
                    .any(|message| message.contains("broken_file_reference"))
            );
        }
        for invalid in ["missing.md", ".env", "docs"] {
            let (_, coverage) = repository
                .capture_exact_code_sources(
                    "fixture",
                    &[path("docs/spec.md"), path(invalid)],
                    budget,
                    &cancel,
                )
                .unwrap();
            assert_eq!(coverage.state, CoverageState::Partial);
            assert!(
                coverage
                    .diagnostics
                    .iter()
                    .any(|message| message.contains("broken_file_reference")
                        && message.contains(invalid))
            );
        }
        // A valid document cannot become a source snapshot through the strict API.
        assert!(
            repository
                .capture_code_source("fixture", &path("docs/spec.md"))
                .is_err()
        );
    }

    #[test]
    fn blocks_traversal_secrets_ignored_files_and_binary_content() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join(".gitignore"), "ignored.ts\n").unwrap();
        fs::write(temp.path().join("visible.ts"), "const visible = true;\n").unwrap();
        fs::write(temp.path().join("ignored.ts"), "secret ignored\n").unwrap();
        fs::write(temp.path().join(".env"), "TOKEN=secret\n").unwrap();
        fs::write(temp.path().join(".env.example"), "TOKEN=\n").unwrap();
        fs::write(temp.path().join("binary.txt"), [0xff, 0xfe, 0xfd]).unwrap();
        let repository = RepositoryContext::new(temp.path()).unwrap();

        assert!(
            repository
                .read_file(ReadRepositoryFileArgs {
                    path: "../outside.ts".into(),
                    start_line: None,
                    end_line: None,
                })
                .is_err()
        );
        assert!(
            repository
                .read_file(ReadRepositoryFileArgs {
                    path: "ignored.ts".into(),
                    start_line: None,
                    end_line: None,
                })
                .is_err()
        );
        assert!(
            repository
                .read_file(ReadRepositoryFileArgs {
                    path: ".env".into(),
                    start_line: None,
                    end_line: None,
                })
                .is_err()
        );
        assert!(
            repository
                .read_file(ReadRepositoryFileArgs {
                    path: "binary.txt".into(),
                    start_line: None,
                    end_line: None,
                })
                .is_err()
        );

        let tree = repository
            .list_tree(ListRepositoryTreeArgs {
                path: None,
                depth: None,
                max_entries: None,
            })
            .unwrap();
        let paths: Vec<_> = tree
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert!(paths.contains(&"visible.ts"));
        assert!(!paths.contains(&".env.example"));
        assert!(!paths.contains(&"ignored.ts"));
        assert!(!paths.contains(&".env"));
        assert!(!paths.contains(&"binary.txt"));
    }

    #[test]
    fn numbers_lines_and_enforces_search_limits() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("main.rs"), "one\ntarget\nthree\n").unwrap();
        let repository = RepositoryContext::new(temp.path()).unwrap();
        let read = repository
            .read_file(ReadRepositoryFileArgs {
                path: "main.rs".into(),
                start_line: Some(2),
                end_line: Some(3),
            })
            .unwrap();
        assert!(read.content.contains("     2 | target"));
        let search = repository
            .search(SearchRepositoryArgs {
                query: "target".into(),
                path: None,
                mode: Some("literal".into()),
                max_results: Some(100),
            })
            .unwrap();
        assert_eq!(search.matches.len(), 1);
        assert_eq!(search.matches[0].line, 2);
    }

    #[test]
    fn model_excerpts_and_search_redact_complete_file_before_clipping() {
        let temp = tempfile::tempdir().unwrap();
        let raw = "ação\n-----BEGIN PRIVATE KEY-----\nsynthetic-private-body\n-----END PRIVATE KEY-----\nafter\n";
        fs::write(temp.path().join("fixture.rs"), raw).unwrap();
        let repository = RepositoryContext::new(temp.path()).unwrap();
        let read = repository
            .read_file(ReadRepositoryFileArgs {
                path: "fixture.rs".into(),
                start_line: Some(3),
                end_line: Some(3),
            })
            .unwrap();
        assert!(!read.content.contains("synthetic-private-body"));
        let found = repository
            .search(SearchRepositoryArgs {
                query: "synthetic".into(),
                path: None,
                mode: None,
                max_results: None,
            })
            .unwrap();
        assert_eq!(found.matches.len(), 1);
        assert_eq!(found.matches[0].line, 3);
        assert!(!found.matches[0].text.contains("synthetic-private-body"));
        assert_eq!(
            repository
                .read_original_for_sanitization("fixture.rs")
                .unwrap(),
            raw
        );
    }
}
