use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use chrono::Utc;
use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tempfile::NamedTempFile;
use walkdir::WalkDir;

use crate::orchestration::RelativeSourcePath;
use crate::repository_context::RepositoryContext;

pub const WORKSPACE_CHANGED_EVENT: &str = "workspace://changed";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceErrorCode {
    ValidationError,
    NotFound,
    InvalidRoot,
    PathForbidden,
    InvalidExtension,
    InvalidQuery,
    Conflict,
    Cancelled,
    IoError,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCommandError {
    pub code: WorkspaceErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug)]
pub enum WorkspaceServiceError {
    InvalidRoot(String),
    WorkspaceNotOpen,
    PathForbidden(String),
    InvalidExtension(String),
    InvalidQuery(String),
    Conflict { expected: String, actual: String },
    Io(io::Error),
    Notify(notify::Error),
    InvalidUtf8(String),
}

impl From<WorkspaceServiceError> for WorkspaceCommandError {
    fn from(value: WorkspaceServiceError) -> Self {
        match value {
            WorkspaceServiceError::InvalidRoot(message) => Self {
                code: WorkspaceErrorCode::InvalidRoot,
                message,
                details: None,
                remediation: None,
            },
            WorkspaceServiceError::WorkspaceNotOpen => Self {
                code: WorkspaceErrorCode::InvalidRoot,
                message: "Nenhum workspace aberto. Abra uma pasta antes de continuar.".to_string(),
                details: None,
                remediation: Some("Abra um workspace válido e tente novamente.".to_string()),
            },
            WorkspaceServiceError::PathForbidden(message) => Self {
                code: WorkspaceErrorCode::PathForbidden,
                message,
                details: None,
                remediation: None,
            },
            WorkspaceServiceError::InvalidExtension(message) => Self {
                code: WorkspaceErrorCode::InvalidExtension,
                message,
                details: None,
                remediation: None,
            },
            WorkspaceServiceError::InvalidQuery(message) => Self {
                code: WorkspaceErrorCode::InvalidQuery,
                message,
                details: None,
                remediation: None,
            },
            WorkspaceServiceError::Conflict { expected, actual } => Self {
                code: WorkspaceErrorCode::Conflict,
                message: format!(
                    "Conflito detectado: revisão esperada `{expected}` não corresponde à revisão atual `{actual}`."
                ),
                details: Some(
                    serde_json::json!({"expectedRevision": expected, "actualRevision": actual}),
                ),
                remediation: Some(
                    "Recarregue o arquivo, reconcilie as alterações e salve novamente.".to_string(),
                ),
            },
            WorkspaceServiceError::Io(error) => Self {
                code: WorkspaceErrorCode::IoError,
                message: error.to_string(),
                details: None,
                remediation: None,
            },
            WorkspaceServiceError::Notify(error) => Self {
                code: WorkspaceErrorCode::IoError,
                message: format!("Watcher falhou: {error}"),
                details: None,
                remediation: None,
            },
            WorkspaceServiceError::InvalidUtf8(path) => Self {
                code: WorkspaceErrorCode::IoError,
                message: format!("Arquivo `{path}` não está em UTF-8."),
                details: None,
                remediation: Some("Converta o arquivo para UTF-8 e tente novamente.".to_string()),
            },
        }
    }
}

impl From<io::Error> for WorkspaceServiceError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileNodeKind {
    Directory,
    File,
}

impl FileNodeKind {
    fn rank(&self) -> u8 {
        match self {
            Self::Directory => 0,
            Self::File => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileNode {
    pub name: String,
    pub relative_path: String,
    pub kind: FileNodeKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<FileNode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenWorkspaceRequest {
    pub path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenWorkspaceResponse {
    pub root_path: String,
    pub tree: Vec<FileNode>,
    pub kind: WorkspaceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub greenfield: Option<GreenfieldBootstrapStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    Greenfield,
    Brownfield,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GreenfieldBootstrapStatus {
    pub mode: String,
    pub initialized: bool,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_paths: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapGreenfieldWorkspaceRequest {
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapGreenfieldWorkspaceResponse {
    pub root_path: String,
    pub status: GreenfieldBootstrapStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceTreeResponse {
    pub tree: Vec<FileNode>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadWorkspaceFileRequest {
    pub relative_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadWorkspaceFileResponse {
    pub content: String,
    pub revision: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteWorkspaceFileAtomicRequest {
    pub relative_path: String,
    pub content: String,
    pub expected_revision: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteWorkspaceFileAtomicResponse {
    pub revision: String,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    #[default]
    Substring,
    Regex,
}

fn default_max_results() -> usize {
    200
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchWorkspaceTextRequest {
    pub query: String,
    #[serde(default)]
    pub mode: SearchMode,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default = "default_max_results")]
    pub max_results: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMatch {
    pub relative_path: String,
    pub line: usize,
    pub column: usize,
    pub line_text: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchWorkspaceTextResponse {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
    pub scanned_files: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceChangedEvent {
    pub kind: String,
    pub relative_path: String,
    pub timestamp: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickWorkspaceRootResponse {
    pub path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWorkspaceVaultIndexRequest {
    #[serde(default)]
    pub force_rebuild: bool,
}

fn default_context_candidates_limit() -> usize {
    24
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetContextCandidatesRequest {
    pub feature_name: String,
    pub stage_name: Option<String>,
    pub agent_scope: Option<String>,
    #[serde(default = "default_context_candidates_limit")]
    pub max_candidates: usize,
    #[serde(default)]
    pub force_rebuild: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCandidate {
    pub relative_path: String,
    pub content_sha256: String,
    pub score: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetContextCandidatesResponse {
    pub strategy: String,
    pub fallback_used: bool,
    pub feature_note_path: Option<String>,
    pub scope_file: Option<String>,
    pub candidates: Vec<ContextCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_context_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub physical_file_references: Vec<ContextPhysicalFileReference>,
    pub expansion_depth: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextPhysicalFileReference {
    pub note_path: String,
    pub relative_path: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultLinkReference {
    pub target: String,
    pub alias: Option<String>,
    pub line: usize,
    pub resolved_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultBrokenLink {
    pub source_path: String,
    pub target: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultEdge {
    pub source_path: String,
    pub target_path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultNoteSummary {
    pub relative_path: String,
    pub title: String,
    pub revision: String,
    pub frontmatter: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub normalized_properties: Option<Value>,
    pub links: Vec<VaultLinkReference>,
    pub outgoing_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub backlink_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schema_warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub test_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultBrokenFileReference {
    pub source_path: String,
    pub field: String,
    pub target: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWorkspaceVaultIndexResponse {
    pub notes: Vec<VaultNoteSummary>,
    pub edges: Vec<VaultEdge>,
    pub broken_links: Vec<VaultBrokenLink>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub broken_file_references: Vec<VaultBrokenFileReference>,
    pub indexed_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameWorkspaceNoteRequest {
    pub from_relative_path: String,
    pub to_relative_path: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameWorkspaceNoteResponse {
    pub renamed_to: String,
    pub updated_references: usize,
    pub affected_files: usize,
}

#[derive(Debug, Clone)]
struct VaultLinkMatch {
    target: String,
    alias: Option<String>,
    line: usize,
}

#[derive(Debug, Clone)]
struct ParsedFrontmatter {
    raw_block: Option<String>,
    body: String,
    parsed: Option<Value>,
}

#[derive(Debug, Clone)]
struct VaultCachedNote {
    relative_path: String,
    revision: String,
    title: String,
    frontmatter: Option<Value>,
    links: Vec<VaultLinkMatch>,
    source_files: Vec<String>,
    test_files: Vec<String>,
    content: String,
}

#[derive(Debug, Default)]
pub struct VaultIndexCache {
    notes: HashMap<String, VaultCachedNote>,
    root: Option<PathBuf>,
    #[cfg(test)]
    parse_count: usize,
}

#[derive(Default)]
struct TreeBuilder {
    directories: BTreeMap<String, TreeBuilder>,
    files: BTreeSet<String>,
}

impl TreeBuilder {
    fn insert_path(&mut self, relative_path: &Path) {
        let mut current = self;
        let mut components = relative_path.components().peekable();

        while let Some(component) = components.next() {
            let name = match component {
                Component::Normal(value) => value.to_string_lossy().to_string(),
                _ => continue,
            };

            if components.peek().is_some() {
                current = current.directories.entry(name).or_default();
            } else {
                current.files.insert(name);
            }
        }
    }

    fn into_nodes(self, base_path: &str) -> Vec<FileNode> {
        let mut nodes = Vec::new();

        for (directory_name, directory_builder) in self.directories {
            let relative_path = join_relative(base_path, &directory_name);
            let children = directory_builder.into_nodes(&relative_path);

            if children.is_empty() {
                continue;
            }

            nodes.push(FileNode {
                name: directory_name,
                relative_path,
                kind: FileNodeKind::Directory,
                children,
            });
        }

        for file_name in self.files {
            nodes.push(FileNode {
                relative_path: join_relative(base_path, &file_name),
                name: file_name,
                kind: FileNodeKind::File,
                children: Vec::new(),
            });
        }

        nodes.sort_by(|left, right| {
            left.kind
                .rank()
                .cmp(&right.kind.rank())
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name))
        });

        nodes
    }
}

pub struct WorkspaceWatcher {
    _watcher: RecommendedWatcher,
}

pub trait WorkspaceEventSink: Send + Sync {
    fn emit(&self, event: WorkspaceChangedEvent);
}

#[derive(Debug, Default)]
pub struct NoopWorkspaceEventSink;

impl WorkspaceEventSink for NoopWorkspaceEventSink {
    fn emit(&self, _event: WorkspaceChangedEvent) {}
}

impl WorkspaceWatcher {
    pub fn start(
        root: PathBuf,
        event_sink: Arc<dyn WorkspaceEventSink>,
    ) -> Result<Self, WorkspaceServiceError> {
        let watch_root = root.clone();
        let mut watcher =
            notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
                let event = match result {
                    Ok(event) => event,
                    Err(error) => {
                        eprintln!("workspace watcher error: {error}");
                        return;
                    }
                };

                let kind = map_event_kind(&event.kind).to_string();
                let timestamp = Utc::now().to_rfc3339();

                for path in event.paths {
                    let Some(relative_path) = to_relative_event_path(&watch_root, &path) else {
                        continue;
                    };

                    if !is_watched_path(Path::new(&relative_path)) {
                        continue;
                    }

                    let payload = WorkspaceChangedEvent {
                        kind: kind.clone(),
                        relative_path,
                        timestamp: timestamp.clone(),
                    };

                    event_sink.emit(payload);
                }
            })
            .map_err(WorkspaceServiceError::Notify)?;

        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(WorkspaceServiceError::Notify)?;

        Ok(Self { _watcher: watcher })
    }
}

const INITIAL_ALLOWED_DIRECTORIES: &[&str] = &[".git", ".obsidian", ".vscode"];
const INITIAL_ALLOWED_FILES: &[&str] = &[
    ".gitignore",
    ".gitattributes",
    "README",
    "README.md",
    "LICENSE",
    "LICENSE.md",
];

const REQUIRED_BOOTSTRAP_DIRECTORIES: &[&str] = &[
    "knowledge/obsidian/features",
    "knowledge/obsidian/tests",
    "knowledge/obsidian/contracts",
    "knowledge/obsidian/decisions",
    "knowledge/obsidian/incidents",
    "knowledge/obsidian/business-rules",
    "knowledge/obsidian/bases",
    "knowledge/obsidian/_templates",
    "knowledge/indexes",
];

pub fn bootstrap_greenfield_workspace_by_path(
    request: BootstrapGreenfieldWorkspaceRequest,
) -> Result<BootstrapGreenfieldWorkspaceResponse, WorkspaceServiceError> {
    let root = validate_workspace_root(&request.path)?;
    let status = bootstrap_greenfield_workspace(&root)?;
    Ok(BootstrapGreenfieldWorkspaceResponse {
        root_path: root.to_string_lossy().to_string(),
        status,
    })
}

pub fn bootstrap_greenfield_workspace(
    root: &Path,
) -> Result<GreenfieldBootstrapStatus, WorkspaceServiceError> {
    if !root.exists() || !root.is_dir() {
        return Err(WorkspaceServiceError::InvalidRoot(
            "Workspace inválido para bootstrap greenfield.".to_string(),
        ));
    }

    // Runs on every open: corrected scope-file defaults would otherwise never
    // reach a workspace that was already bootstrapped.
    let repaired = repair_stage_scope_files(root)?;

    if is_greenfield_bootstrapped(root) {
        return Ok(GreenfieldBootstrapStatus {
            mode: "greenfield".to_string(),
            initialized: false,
            message: "Workspace greenfield já inicializado.".to_string(),
            created_paths: repaired,
        });
    }

    let disallowed_entries = detect_disallowed_entries(root)?;
    if !disallowed_entries.is_empty() {
        return Err(WorkspaceServiceError::InvalidRoot(format!(
            "Modo legacy ainda não suportado no V1. Remova estes itens para usar greenfield: {}",
            disallowed_entries.join(", ")
        )));
    }

    let mut created_paths = repaired;
    for relative in REQUIRED_BOOTSTRAP_DIRECTORIES {
        let absolute = root.join(relative);
        if !absolute.exists() {
            fs::create_dir_all(&absolute).map_err(WorkspaceServiceError::Io)?;
            created_paths.push(relative.replace('\\', "/"));
        }
    }

    let file_specs = greenfield_file_specs();
    for (relative, content) in file_specs {
        let absolute = root.join(relative);
        if absolute.exists() {
            continue;
        }
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).map_err(WorkspaceServiceError::Io)?;
        }
        fs::write(&absolute, content).map_err(WorkspaceServiceError::Io)?;
        created_paths.push(relative.to_string());
    }

    Ok(GreenfieldBootstrapStatus {
        mode: "greenfield".to_string(),
        initialized: true,
        message: "Workspace greenfield inicializado com estrutura Obsidian + templates."
            .to_string(),
        created_paths,
    })
}

pub fn classify_workspace(root: &Path) -> Result<WorkspaceKind, WorkspaceServiceError> {
    if !root.exists() || !root.is_dir() {
        return Err(WorkspaceServiceError::InvalidRoot(
            "Workspace inválido para classificação.".to_string(),
        ));
    }
    if is_greenfield_bootstrapped(root) || detect_disallowed_entries(root)?.is_empty() {
        Ok(WorkspaceKind::Greenfield)
    } else {
        Ok(WorkspaceKind::Brownfield)
    }
}

pub fn prepare_brownfield_knowledge_root(
    root: &Path,
) -> Result<Vec<String>, WorkspaceServiceError> {
    let canonical_root = fs::canonicalize(root).map_err(WorkspaceServiceError::Io)?;
    if !canonical_root.is_dir() {
        return Err(WorkspaceServiceError::InvalidRoot(
            "Workspace inválido para adoção brownfield.".to_string(),
        ));
    }

    let mut created_paths = Vec::new();
    let mut current = canonical_root.clone();
    let mut relative = PathBuf::new();
    for segment in ["knowledge", "obsidian", "components"] {
        current.push(segment);
        relative.push(segment);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if is_link_metadata(&metadata) || !metadata.is_dir() {
                    return Err(WorkspaceServiceError::PathForbidden(format!(
                        "`{}` precisa ser um diretório real dentro do workspace.",
                        relative.to_string_lossy().replace('\\', "/")
                    )));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(WorkspaceServiceError::Io)?;
                created_paths.push(relative.to_string_lossy().replace('\\', "/"));
            }
            Err(error) => return Err(WorkspaceServiceError::Io(error)),
        }
        let canonical = fs::canonicalize(&current).map_err(WorkspaceServiceError::Io)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(WorkspaceServiceError::PathForbidden(
                "A estrutura brownfield resolveria para fora do workspace.".to_string(),
            ));
        }
    }
    Ok(created_paths)
}

fn is_link_metadata(metadata: &fs::Metadata) -> bool {
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

fn is_greenfield_bootstrapped(root: &Path) -> bool {
    root.join("docs/PROJECT_AGENT.md").is_file()
        && root
            .join("knowledge/obsidian/_templates/feature-template.md")
            .is_file()
        && root
            .join("knowledge/obsidian/bases/task-specifier.base")
            .is_file()
}

fn detect_disallowed_entries(root: &Path) -> Result<Vec<String>, WorkspaceServiceError> {
    let mut disallowed = Vec::new();

    for entry in fs::read_dir(root).map_err(WorkspaceServiceError::Io)? {
        let entry = entry.map_err(WorkspaceServiceError::Io)?;
        let file_name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();

        if file_name == "knowledge" || file_name == "docs" {
            continue;
        }

        if path.is_dir() {
            if INITIAL_ALLOWED_DIRECTORIES
                .iter()
                .any(|allowed| *allowed == file_name)
            {
                continue;
            }
            disallowed.push(file_name);
            continue;
        }

        if path.is_file() {
            if INITIAL_ALLOWED_FILES
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(&file_name))
            {
                continue;
            }
            disallowed.push(file_name);
        }
    }

    disallowed.sort();
    Ok(disallowed)
}

/// Scope views for the six pipeline stages, keyed on the `type` vocabulary the
/// core actually writes: `feature`, `feature_plan`, `contract`, `test`,
/// `component`, `decision`, `learning_report`.
///
/// The previous defaults were written against the `_templates` vocabulary
/// (`type: feature`), but the forge publishes plan notes as `type: feature_plan`
/// ([`crate::feature_forge`]), so they either matched nothing - degrading to the
/// fail-open branch in `get_context_candidates`, i.e. no scoping at all - or,
/// for `status != deprecated`, matched everything, because an absent property
/// makes `not-equals` true. Either way the agent scope never bit, and compact
/// models received the entire vault.
///
/// A filter that matches nothing still degrades to fail-open, and the current
/// feature note always passes via the scope bypass, so narrowing here can never
/// starve a stage of context.
const INTAKE_ROUTER_BASE: &str = r#"{
  "filters": {
    "and": [
      { "property": "type", "operator": "in", "value": ["feature_plan", "decision", "component"] }
    ]
  }
}
"#;

const TASK_SPECIFIER_BASE: &str = r#"{
  "filters": {
    "and": [
      { "property": "type", "operator": "in", "value": ["feature_plan", "feature", "component", "contract", "decision"] }
    ]
  }
}
"#;

const CONTRACT_AGENT_BASE: &str = r#"{
  "filters": {
    "and": [
      { "property": "type", "operator": "in", "value": ["contract", "feature_plan", "component"] }
    ]
  }
}
"#;

const BACKEND_AGENT_BASE: &str = r#"{
  "filters": {
    "and": [
      { "property": "type", "operator": "in", "value": ["feature_plan", "contract", "component", "feature"] }
    ]
  }
}
"#;

const TEST_AGENT_BASE: &str = r#"{
  "filters": {
    "and": [
      { "property": "type", "operator": "in", "value": ["test", "feature_plan", "contract", "feature"] }
    ]
  }
}
"#;

const INTEGRATOR_BASE: &str = r#"{
  "filters": {
    "and": [
      { "property": "type", "operator": "in", "value": ["feature_plan", "contract", "test", "component", "decision"] }
    ]
  }
}
"#;

/// The six stage scope files, in the order [`resolve_scope_file_name`] names them.
const STAGE_SCOPE_FILES: [(&str, &str); 6] = [
    (
        "knowledge/obsidian/bases/intake_router.base",
        INTAKE_ROUTER_BASE,
    ),
    (
        "knowledge/obsidian/bases/task_specifier.base",
        TASK_SPECIFIER_BASE,
    ),
    (
        "knowledge/obsidian/bases/contract_agent.base",
        CONTRACT_AGENT_BASE,
    ),
    (
        "knowledge/obsidian/bases/backend_agent.base",
        BACKEND_AGENT_BASE,
    ),
    ("knowledge/obsidian/bases/test_agent.base", TEST_AGENT_BASE),
    ("knowledge/obsidian/bases/integrator.base", INTEGRATOR_BASE),
];

/// Stage scope files exactly as previously shipped. A file still byte-identical
/// to one of these was never touched by the developer, so it is safe to realign.
const STALE_STAGE_BASE_DEFAULTS: [&str; 5] = [
    "{\n  \"filters\": {\n    \"and\": [\n      { \"property\": \"status\", \"operator\": \"not-equals\", \"value\": \"deprecated\" }\n    ]\n  }\n}\n",
    "{\n  \"filters\": {\n    \"and\": [\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"contract\" }\n    ]\n  }\n}\n",
    "{\n  \"filters\": {\n    \"and\": [\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"feature\" },\n      { \"property\": \"domain\", \"operator\": \"equals\", \"value\": \"backend\" }\n    ]\n  }\n}\n",
    "{\n  \"filters\": {\n    \"or\": [\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"test\" },\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"feature\" }\n    ]\n  }\n}\n",
    "{\n  \"filters\": {\n    \"or\": [\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"incident\" },\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"decision\" }\n    ]\n  }\n}\n",
];

/// Realigns stage scope files that a project bootstrapped before this change.
///
/// `greenfield_file_specs` skips files that already exist and the bootstrap
/// itself returns early once a workspace is initialized, so corrected defaults
/// would otherwise never reach an existing project. Only files missing or still
/// byte-identical to a shipped default are rewritten; anything the developer
/// edited is preserved. Idempotent: a second run rewrites nothing.
pub fn repair_stage_scope_files(root: &Path) -> Result<Vec<String>, WorkspaceServiceError> {
    // Repair only, never create: an unbootstrapped root has no bases directory
    // and is served by `greenfield_file_specs`, which must not be pre-empted
    // before `detect_disallowed_entries` has had its say.
    if !root.join("knowledge/obsidian/bases").is_dir() {
        return Ok(Vec::new());
    }
    let mut repaired = Vec::new();
    for (relative, desired) in STAGE_SCOPE_FILES {
        let absolute = root.join(relative);
        match fs::read_to_string(&absolute) {
            Ok(current) => {
                if current == desired || !STALE_STAGE_BASE_DEFAULTS.contains(&current.as_str()) {
                    continue;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(WorkspaceServiceError::Io(error)),
        }
        if let Some(parent) = absolute.parent() {
            fs::create_dir_all(parent).map_err(WorkspaceServiceError::Io)?;
        }
        fs::write(&absolute, desired).map_err(WorkspaceServiceError::Io)?;
        repaired.push(relative.to_string());
    }
    Ok(repaired)
}

fn greenfield_file_specs() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "docs/PROJECT_AGENT.md",
            "# PROJECT_AGENT\n\nProjeto greenfield inicializado pelo Hard Vibe Code.\n",
        ),
        ("knowledge/indexes/.gitkeep", ""),
        (
            "knowledge/obsidian/_templates/feature-template.md",
            "---\ntype: feature\ndomain: backend\nrisk: medium\nstatus: planned\ntested-by: []\naffects: []\ncontracts: []\nincidents: []\ndecisions: []\nlast-pulse: null\n---\n\n# Feature Name\n\n## Objective\n\n## Acceptance Criteria\n",
        ),
        (
            "knowledge/obsidian/_templates/test-template.md",
            "---\ntype: test\nscope: integration\ncovers: \"[[features/example-feature]]\"\nstatus: red\nrisk-area: backend\n---\n\n# Test Name\n",
        ),
        (
            "knowledge/obsidian/_templates/contract-template.md",
            "---\ntype: contract\nproducers: []\nconsumers: []\naffects-features: []\nstatus: draft\n---\n\n# Contract Name\n",
        ),
        (
            "knowledge/obsidian/bases/task-specifier.base",
            "{\n  \"filters\": {\n    \"and\": [\n      { \"property\": \"status\", \"operator\": \"not-equals\", \"value\": \"deprecated\" }\n    ]\n  }\n}\n",
        ),
        (
            "knowledge/obsidian/bases/backend-agent.base",
            "{\n  \"filters\": {\n    \"and\": [\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"feature\" },\n      { \"property\": \"domain\", \"operator\": \"equals\", \"value\": \"backend\" }\n    ]\n  }\n}\n",
        ),
        (
            "knowledge/obsidian/bases/frontend-agent.base",
            "{\n  \"filters\": {\n    \"and\": [\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"feature\" },\n      { \"property\": \"domain\", \"operator\": \"equals\", \"value\": \"frontend\" }\n    ]\n  }\n}\n",
        ),
        (
            "knowledge/obsidian/bases/test-agent.base",
            "{\n  \"filters\": {\n    \"or\": [\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"test\" },\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"feature\" }\n    ]\n  }\n}\n",
        ),
        (
            "knowledge/obsidian/bases/contract-agent.base",
            "{\n  \"filters\": {\n    \"and\": [\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"contract\" }\n    ]\n  }\n}\n",
        ),
        (
            "knowledge/obsidian/bases/security-agent.base",
            "{\n  \"filters\": {\n    \"or\": [\n      { \"property\": \"risk\", \"operator\": \"equals\", \"value\": \"high\" },\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"incident\" }\n    ]\n  }\n}\n",
        ),
        (
            "knowledge/obsidian/bases/learning-agent.base",
            "{\n  \"filters\": {\n    \"or\": [\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"incident\" },\n      { \"property\": \"type\", \"operator\": \"equals\", \"value\": \"decision\" }\n    ]\n  }\n}\n",
        ),
        (
            "knowledge/obsidian/bases/intake_router.base",
            INTAKE_ROUTER_BASE,
        ),
        (
            "knowledge/obsidian/bases/task_specifier.base",
            TASK_SPECIFIER_BASE,
        ),
        (
            "knowledge/obsidian/bases/contract_agent.base",
            CONTRACT_AGENT_BASE,
        ),
        (
            "knowledge/obsidian/bases/backend_agent.base",
            BACKEND_AGENT_BASE,
        ),
        ("knowledge/obsidian/bases/test_agent.base", TEST_AGENT_BASE),
        ("knowledge/obsidian/bases/integrator.base", INTEGRATOR_BASE),
    ]
}

pub fn validate_workspace_root(path: &str) -> Result<PathBuf, WorkspaceServiceError> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceServiceError::InvalidRoot(
            "Escolha uma pasta válida para abrir como workspace.".to_string(),
        ));
    }

    let root = PathBuf::from(trimmed);
    if !root.exists() {
        return Err(WorkspaceServiceError::InvalidRoot(
            "A pasta selecionada não existe.".to_string(),
        ));
    }

    if !root.is_dir() {
        return Err(WorkspaceServiceError::InvalidRoot(
            "O caminho selecionado não é uma pasta.".to_string(),
        ));
    }

    fs::canonicalize(root).map_err(WorkspaceServiceError::Io)
}

pub fn build_workspace_tree(root: &Path) -> Result<WorkspaceTreeResponse, WorkspaceServiceError> {
    let mut tree_builder = TreeBuilder::default();

    for entry in WalkDir::new(root).min_depth(1).follow_links(false) {
        let entry = entry.map_err(walkdir_to_io_error)?;

        if !entry.file_type().is_file() || !is_allowed_extension(entry.path()) {
            continue;
        }

        let relative_path = entry.path().strip_prefix(root).map_err(|_| {
            WorkspaceServiceError::PathForbidden("Arquivo fora do workspace.".to_string())
        })?;

        tree_builder.insert_path(relative_path);
    }

    Ok(WorkspaceTreeResponse {
        tree: tree_builder.into_nodes(""),
    })
}

pub fn read_workspace_file(
    root: &Path,
    request: ReadWorkspaceFileRequest,
) -> Result<ReadWorkspaceFileResponse, WorkspaceServiceError> {
    let path = resolve_workspace_path(root, &request.relative_path)?;

    if !path.exists() || !path.is_file() {
        return Err(WorkspaceServiceError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "Arquivo não encontrado.",
        )));
    }

    let content = fs::read_to_string(&path).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidData {
            WorkspaceServiceError::InvalidUtf8(path.to_string_lossy().to_string())
        } else {
            WorkspaceServiceError::Io(error)
        }
    })?;

    let revision = compute_revision(&path)?;

    Ok(ReadWorkspaceFileResponse { content, revision })
}

pub fn write_workspace_file_atomic(
    root: &Path,
    request: WriteWorkspaceFileAtomicRequest,
) -> Result<WriteWorkspaceFileAtomicResponse, WorkspaceServiceError> {
    let path = resolve_workspace_path(root, &request.relative_path)?;
    let expected_revision = request.expected_revision.trim();

    let actual_revision = if path.exists() {
        Some(compute_revision(&path)?)
    } else {
        None
    };

    if let Some(actual_revision) = actual_revision.clone() {
        if expected_revision != actual_revision {
            return Err(WorkspaceServiceError::Conflict {
                expected: expected_revision.to_string(),
                actual: actual_revision,
            });
        }
    } else if !expected_revision.is_empty() {
        return Err(WorkspaceServiceError::Conflict {
            expected: expected_revision.to_string(),
            actual: String::new(),
        });
    }

    let parent = path.parent().ok_or_else(|| {
        WorkspaceServiceError::PathForbidden("Caminho inválido para escrita.".to_string())
    })?;

    if !parent.exists() || !parent.is_dir() {
        return Err(WorkspaceServiceError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "Diretório de destino não existe.",
        )));
    }

    let mut temp_file = NamedTempFile::new_in(parent).map_err(WorkspaceServiceError::Io)?;
    temp_file
        .as_file_mut()
        .write_all(request.content.as_bytes())
        .map_err(WorkspaceServiceError::Io)?;
    temp_file
        .as_file_mut()
        .sync_all()
        .map_err(WorkspaceServiceError::Io)?;

    temp_file
        .persist(&path)
        .map_err(|error| WorkspaceServiceError::Io(error.error))?;

    let revision = compute_revision(&path)?;

    Ok(WriteWorkspaceFileAtomicResponse { revision })
}

pub fn search_workspace_text(
    root: &Path,
    request: SearchWorkspaceTextRequest,
) -> Result<SearchWorkspaceTextResponse, WorkspaceServiceError> {
    let normalized_query = request.query.trim();
    if normalized_query.is_empty() {
        return Err(WorkspaceServiceError::InvalidQuery(
            "A consulta de busca nao pode ser vazia.".to_string(),
        ));
    }

    let max_results = if request.max_results == 0 {
        default_max_results()
    } else {
        request.max_results
    };

    let pattern = compile_search_pattern(normalized_query, request.mode, request.case_sensitive)?;

    let mut matches = Vec::new();
    let mut truncated = false;
    let mut scanned_files = 0usize;

    'outer: for entry in WalkDir::new(root).min_depth(1).follow_links(false) {
        let entry = entry.map_err(walkdir_to_io_error)?;
        if !entry.file_type().is_file() || !is_allowed_extension(entry.path()) {
            continue;
        }

        let relative_path = entry.path().strip_prefix(root).map_err(|_| {
            WorkspaceServiceError::PathForbidden("Arquivo fora do workspace.".to_string())
        })?;
        let relative_path_string = normalize_relative_path(relative_path);

        let content = fs::read_to_string(entry.path()).map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                WorkspaceServiceError::InvalidUtf8(entry.path().to_string_lossy().to_string())
            } else {
                WorkspaceServiceError::Io(error)
            }
        })?;

        scanned_files += 1;

        for (line_index, line_text) in content.lines().enumerate() {
            for found in pattern.find_iter(line_text) {
                if found.start() == found.end() {
                    continue;
                }

                let column = byte_index_to_column(line_text, found.start());
                matches.push(SearchMatch {
                    relative_path: relative_path_string.clone(),
                    line: line_index + 1,
                    column,
                    line_text: line_text.to_string(),
                });

                if matches.len() >= max_results {
                    truncated = true;
                    break 'outer;
                }
            }
        }
    }

    Ok(SearchWorkspaceTextResponse {
        matches,
        truncated,
        scanned_files,
    })
}

pub fn get_workspace_vault_index(
    root: &Path,
    cache: &mut VaultIndexCache,
    request: GetWorkspaceVaultIndexRequest,
) -> Result<GetWorkspaceVaultIndexResponse, WorkspaceServiceError> {
    refresh_vault_index_cache(root, cache, request.force_rebuild)?;

    let lookup = build_vault_lookup_maps(
        cache
            .notes
            .values()
            .filter(|note| is_markdown_extension(Path::new(&note.relative_path)))
            .map(|note| note.relative_path.as_str()),
    );
    let mut note_paths = cache
        .notes
        .keys()
        .filter(|path| is_markdown_extension(Path::new(path)))
        .cloned()
        .collect::<Vec<String>>();
    note_paths.sort();

    let mut notes = Vec::new();
    let mut edges_set = BTreeSet::new();
    let mut broken_links = Vec::new();
    let mut broken_file_references = Vec::new();
    let repository = RepositoryContext::new(root)
        .map_err(|error| WorkspaceServiceError::InvalidRoot(error.to_string()))?;

    for note_path in note_paths {
        let Some(note) = cache.notes.get(&note_path) else {
            continue;
        };

        let mut outgoing_set = BTreeSet::new();
        let mut links = Vec::new();

        for link in &note.links {
            let resolved_path = resolve_wikilink_path(&note.relative_path, &link.target, &lookup);
            if let Some(target_path) = &resolved_path {
                outgoing_set.insert(target_path.clone());
                edges_set.insert((note.relative_path.clone(), target_path.clone()));
            } else {
                broken_links.push(VaultBrokenLink {
                    source_path: note.relative_path.clone(),
                    target: link.target.clone(),
                    line: link.line,
                });
            }

            links.push(VaultLinkReference {
                target: link.target.clone(),
                alias: link.alias.clone(),
                line: link.line,
                resolved_path,
            });
        }

        let mut schema_warnings = schema_warnings_for_note(note.frontmatter.as_ref());
        for (field, paths) in [
            ("source-files", &note.source_files),
            ("test-files", &note.test_files),
        ] {
            for target in paths {
                let validation = RelativeSourcePath::new(target.clone())
                    .map_err(|error| error.to_string())
                    .and_then(|path| {
                        repository
                            .validate_file_reference(&path)
                            .map_err(|error| error.to_string())
                    });
                if let Err(reason) = validation {
                    schema_warnings.push(format!(
                        "File reference issue in `{field}`: `{target}`. {reason}"
                    ));
                    broken_file_references.push(VaultBrokenFileReference {
                        source_path: note.relative_path.clone(),
                        field: field.to_string(),
                        target: target.clone(),
                        reason,
                    });
                }
            }
        }

        notes.push(VaultNoteSummary {
            relative_path: note.relative_path.clone(),
            title: note.title.clone(),
            revision: note.revision.clone(),
            frontmatter: note.frontmatter.clone(),
            normalized_properties: normalize_frontmatter_properties(note.frontmatter.as_ref()),
            links,
            outgoing_paths: outgoing_set.into_iter().collect(),
            backlink_paths: Vec::new(),
            schema_warnings,
            source_files: note.source_files.clone(),
            test_files: note.test_files.clone(),
        });
    }

    let edges = edges_set
        .into_iter()
        .map(|(source_path, target_path)| VaultEdge {
            source_path,
            target_path,
        })
        .collect::<Vec<VaultEdge>>();

    let mut backlinks_by_target: HashMap<String, BTreeSet<String>> = HashMap::new();
    for edge in &edges {
        backlinks_by_target
            .entry(edge.target_path.clone())
            .or_default()
            .insert(edge.source_path.clone());
    }

    for note in &mut notes {
        note.backlink_paths = backlinks_by_target
            .get(&note.relative_path)
            .map(|paths| paths.iter().cloned().collect())
            .unwrap_or_default();
    }

    Ok(GetWorkspaceVaultIndexResponse {
        notes,
        edges,
        broken_links,
        broken_file_references,
        indexed_at: Utc::now().to_rfc3339(),
    })
}

pub fn get_context_candidates(
    root: &Path,
    cache: &mut VaultIndexCache,
    request: GetContextCandidatesRequest,
) -> Result<GetContextCandidatesResponse, WorkspaceServiceError> {
    get_context_candidates_inner(root, cache, request, None)
}

pub fn get_context_candidates_with_expansion(
    root: &Path,
    cache: &mut VaultIndexCache,
    request: GetContextCandidatesRequest,
    expansion_depth: usize,
    beam_width: usize,
) -> Result<GetContextCandidatesResponse, WorkspaceServiceError> {
    get_context_candidates_inner(
        root,
        cache,
        request,
        Some((expansion_depth.clamp(1, 3), beam_width.clamp(1, 64))),
    )
}

fn get_context_candidates_inner(
    root: &Path,
    cache: &mut VaultIndexCache,
    request: GetContextCandidatesRequest,
    expansion: Option<(usize, usize)>,
) -> Result<GetContextCandidatesResponse, WorkspaceServiceError> {
    let feature_name = request.feature_name.trim();
    if feature_name.is_empty() {
        return Err(WorkspaceServiceError::InvalidQuery(
            "featureName não pode ser vazio.".to_string(),
        ));
    }

    refresh_vault_index_cache(root, cache, request.force_rebuild)?;
    let lookup = build_vault_lookup_maps(
        cache
            .notes
            .values()
            .filter(|note| normative_properties(&note.relative_path, note.frontmatter.as_ref()))
            .map(|note| note.relative_path.as_str()),
    );
    let base_scope_file = resolve_scope_file_name(
        request.agent_scope.as_deref(),
        request.stage_name.as_deref(),
    );
    let scope_filter = load_scope_filter(root, &base_scope_file)?;
    let scoped_paths = build_scoped_paths(cache, scope_filter.as_ref());
    let fallback_candidates =
        build_textual_fallback_candidates(cache, feature_name, request.stage_name.as_deref());

    let Some(feature_note_path) = resolve_feature_note_path(cache, feature_name) else {
        let max_candidates = request.max_candidates.clamp(1, 200);
        return Ok(GetContextCandidatesResponse {
            strategy: "yaml_graph".to_string(),
            fallback_used: true,
            feature_note_path: None,
            scope_file: Some(base_scope_file),
            candidates: fallback_candidates
                .into_iter()
                .take(max_candidates)
                .collect(),
            required_context_paths: Vec::new(),
            physical_file_references: Vec::new(),
            expansion_depth: 0,
        });
    };

    let mut score_by_path: HashMap<String, usize> = HashMap::new();
    score_by_path.insert(feature_note_path.clone(), 100);

    let feature_note = match cache.notes.get(&feature_note_path) {
        Some(note) => note,
        None => {
            let max_candidates = request.max_candidates.clamp(1, 200);
            return Ok(GetContextCandidatesResponse {
                strategy: "yaml_graph".to_string(),
                fallback_used: true,
                feature_note_path: None,
                scope_file: Some(base_scope_file),
                candidates: fallback_candidates
                    .into_iter()
                    .take(max_candidates)
                    .collect(),
                required_context_paths: Vec::new(),
                physical_file_references: Vec::new(),
                expansion_depth: 0,
            });
        }
    };

    let mut outgoing_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut backlinks_map: HashMap<String, Vec<String>> = HashMap::new();
    for note in cache.notes.values() {
        if !normative_properties(&note.relative_path, note.frontmatter.as_ref()) {
            continue;
        }
        let mut outgoing = Vec::new();
        for link in &note.links {
            if let Some(target_path) =
                resolve_wikilink_path(&note.relative_path, &link.target, &lookup)
            {
                outgoing.push(target_path.clone());
                backlinks_map
                    .entry(target_path)
                    .or_default()
                    .push(note.relative_path.clone());
            }
        }
        outgoing_map.insert(note.relative_path.clone(), outgoing);
    }

    let required_context_paths = direct_context_links(feature_note, &lookup);
    for path in &required_context_paths {
        update_score(&mut score_by_path, path.clone(), 85);
    }

    let expansion_depth = expansion
        .map(|(depth, _)| depth)
        .unwrap_or_else(|| expansion_depth_from_risk(feature_note.frontmatter.as_ref()));
    let beam_width = expansion.map(|(_, width)| width).unwrap_or(usize::MAX);
    let mut frontier = vec![feature_note_path.clone()];
    let mut visited: HashSet<String> = HashSet::new();

    for depth in 0..expansion_depth {
        let mut next_frontier = Vec::new();
        for node in frontier {
            if !visited.insert(node.clone()) {
                continue;
            }

            let mut neighbors = Vec::new();
            neighbors.extend(outgoing_map.get(&node).cloned().unwrap_or_default());
            neighbors.extend(backlinks_map.get(&node).cloned().unwrap_or_default());

            for neighbor in neighbors {
                let depth_score = 70usize.saturating_sub(depth * 15);
                update_score(&mut score_by_path, neighbor.clone(), depth_score.max(20));
                next_frontier.push(neighbor);
            }
        }
        next_frontier.sort_by(|left, right| {
            score_by_path
                .get(right)
                .cmp(&score_by_path.get(left))
                .then_with(|| left.cmp(right))
        });
        next_frontier.dedup();
        next_frontier.truncate(beam_width);
        frontier = next_frontier;
    }

    let feature_scope_bypass = feature_note_path.clone();
    let mut candidates = score_by_path
        .into_iter()
        .filter(|(path, _)| {
            if !cache
                .notes
                .get(path)
                .is_some_and(|note| normative_properties(path, note.frontmatter.as_ref()))
            {
                return false;
            }
            if path == &feature_scope_bypass {
                return true;
            }
            if scoped_paths.is_empty() {
                return true;
            }
            scoped_paths.contains(path)
        })
        .map(|(relative_path, score)| ContextCandidate {
            content_sha256: cache.notes[&relative_path].revision.clone(),
            relative_path,
            score,
        })
        .collect::<Vec<ContextCandidate>>();

    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    candidates.truncate(request.max_candidates.clamp(1, 200));
    let physical_file_references = physical_references_for_candidates(cache, &candidates);

    Ok(GetContextCandidatesResponse {
        strategy: "yaml_graph".to_string(),
        fallback_used: false,
        feature_note_path: Some(feature_note_path),
        scope_file: Some(base_scope_file),
        candidates,
        required_context_paths,
        physical_file_references,
        expansion_depth,
    })
}

fn physical_references_for_candidates(
    cache: &VaultIndexCache,
    candidates: &[ContextCandidate],
) -> Vec<ContextPhysicalFileReference> {
    let mut seen = HashSet::new();
    let mut references = Vec::new();
    for candidate in candidates {
        let Some(note) = cache.notes.get(&candidate.relative_path) else {
            continue;
        };
        for (kind, paths) in [("source", &note.source_files), ("test", &note.test_files)] {
            for path in paths {
                if seen.insert((candidate.relative_path.clone(), kind, path.clone())) {
                    references.push(ContextPhysicalFileReference {
                        note_path: candidate.relative_path.clone(),
                        relative_path: path.clone(),
                        kind: kind.to_string(),
                    });
                }
            }
        }
    }
    references
}

pub fn rename_workspace_note(
    root: &Path,
    cache: &mut VaultIndexCache,
    request: RenameWorkspaceNoteRequest,
) -> Result<RenameWorkspaceNoteResponse, WorkspaceServiceError> {
    let from_relative_path = parse_relative_path(&request.from_relative_path)?;
    let to_relative_path = parse_relative_path(&request.to_relative_path)?;

    if !is_markdown_extension(&from_relative_path) || !is_markdown_extension(&to_relative_path) {
        return Err(WorkspaceServiceError::InvalidExtension(
            "Renomeio de notas aceita apenas arquivos `.md`.".to_string(),
        ));
    }

    let from_absolute_path = root.join(&from_relative_path);
    if !from_absolute_path.exists() || !from_absolute_path.is_file() {
        return Err(WorkspaceServiceError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "Nota origem não encontrada para renomeio.",
        )));
    }

    let to_absolute_path = root.join(&to_relative_path);
    if to_absolute_path.exists() {
        return Err(WorkspaceServiceError::InvalidQuery(
            "Já existe uma nota com esse nome no caminho de destino.".to_string(),
        ));
    }

    let to_parent = to_absolute_path.parent().ok_or_else(|| {
        WorkspaceServiceError::PathForbidden(
            "Caminho de destino inválido para renomeio.".to_string(),
        )
    })?;
    if !to_parent.exists() || !to_parent.is_dir() {
        return Err(WorkspaceServiceError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "Diretório de destino para renomeio não existe.",
        )));
    }

    let pre_rename_paths = collect_markdown_relative_paths(root)?;
    let lookup_before = build_vault_lookup_maps(pre_rename_paths.iter().map(String::as_str));
    let from_relative_string = normalize_relative_path(&from_relative_path);
    let to_relative_string = normalize_relative_path(&to_relative_path);
    let new_link_target = note_relative_path_to_link_target(&to_relative_string);

    fs::rename(&from_absolute_path, &to_absolute_path).map_err(WorkspaceServiceError::Io)?;

    let mut updated_references = 0usize;
    let mut affected_files = 0usize;

    let post_rename_paths = collect_markdown_relative_paths(root)?;
    for relative_path in post_rename_paths {
        let absolute_path = root.join(&relative_path);
        let content = fs::read_to_string(&absolute_path).map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                WorkspaceServiceError::InvalidUtf8(absolute_path.to_string_lossy().to_string())
            } else {
                WorkspaceServiceError::Io(error)
            }
        })?;

        let parsed = parse_frontmatter_and_body(&content)?;
        let (rewritten_body, replaced_count) = rewrite_wikilinks_for_rename(
            &parsed.body,
            &relative_path,
            &from_relative_string,
            &new_link_target,
            &lookup_before,
        );

        if replaced_count == 0 {
            continue;
        }

        let rewritten_content =
            rebuild_content_with_frontmatter(parsed.raw_block.as_deref(), &rewritten_body);
        write_text_file_atomically(&absolute_path, &rewritten_content)?;
        updated_references += replaced_count;
        affected_files += 1;
    }

    cache.notes.clear();

    Ok(RenameWorkspaceNoteResponse {
        renamed_to: to_relative_string,
        updated_references,
        affected_files,
    })
}

fn refresh_vault_index_cache(
    root: &Path,
    cache: &mut VaultIndexCache,
    force_rebuild: bool,
) -> Result<(), WorkspaceServiceError> {
    let canonical = root.canonicalize()?;
    if cache.root.as_ref() != Some(&canonical) {
        cache.notes.clear();
        cache.root = Some(canonical);
    }
    let markdown_paths = crate::repository_context::RepositoryContext::new(root)
        .and_then(|repository| repository.document_paths(false))
        .map_err(|e| WorkspaceServiceError::PathForbidden(e.to_string()))?;
    let markdown_set = markdown_paths.iter().cloned().collect::<BTreeSet<String>>();
    cache
        .notes
        .retain(|relative_path, _| markdown_set.contains(relative_path));

    for relative_path in markdown_paths {
        let content = match read_context_document(root, &relative_path) {
            Ok(content) => content,
            Err(_) => {
                cache.notes.remove(&relative_path);
                continue;
            }
        };
        let revision = crate::orchestration::ContentSha256::of(content.as_bytes())
            .as_str()
            .to_string();
        let should_reparse = force_rebuild
            || cache
                .notes
                .get(&relative_path)
                .map(|note| note.revision != revision)
                .unwrap_or(true);

        if !should_reparse {
            continue;
        }

        match parse_vault_note(&relative_path, revision, &content) {
            Ok(parsed) => {
                #[cfg(test)]
                {
                    cache.parse_count += 1;
                }
                cache.notes.insert(relative_path, parsed);
            }
            Err(_) => {
                cache.notes.remove(&relative_path);
            }
        }
    }

    Ok(())
}

fn parse_vault_note(
    relative_path: &str,
    revision: String,
    content: &str,
) -> Result<VaultCachedNote, WorkspaceServiceError> {
    let parsed = parse_frontmatter_and_body(content)?;
    let links = extract_wikilinks(&parsed.body);
    let fallback_title = Path::new(relative_path)
        .file_stem()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_else(|| relative_path.to_string());

    let source_files = frontmatter_string_list(parsed.parsed.as_ref(), "source-files");
    let test_files = frontmatter_string_list(parsed.parsed.as_ref(), "test-files");

    Ok(VaultCachedNote {
        relative_path: relative_path.to_string(),
        revision,
        title: extract_markdown_title(&parsed.body).unwrap_or(fallback_title),
        frontmatter: parsed.parsed,
        links,
        source_files,
        test_files,
        content: content.chars().take(8192).collect(),
    })
}

fn parse_frontmatter_and_body(content: &str) -> Result<ParsedFrontmatter, WorkspaceServiceError> {
    let frontmatter_regex = Regex::new(r"(?s)\A---\r?\n(?P<yaml>.*?)\r?\n---(?:\r?\n)?")
        .expect("valid frontmatter regex");

    let Some(captures) = frontmatter_regex.captures(content) else {
        return Ok(ParsedFrontmatter {
            raw_block: None,
            body: content.to_string(),
            parsed: None,
        });
    };

    let Some(full_match) = captures.get(0) else {
        return Ok(ParsedFrontmatter {
            raw_block: None,
            body: content.to_string(),
            parsed: None,
        });
    };
    let yaml_body = captures
        .name("yaml")
        .map(|value| value.as_str())
        .unwrap_or("");
    let parsed_yaml = serde_yaml::from_str::<serde_yaml::Value>(yaml_body).map_err(|error| {
        WorkspaceServiceError::InvalidQuery(format!("Frontmatter YAML inválido: {error}"))
    })?;
    let parsed = serde_json::to_value(parsed_yaml).ok();
    let body = content[full_match.end()..].to_string();

    Ok(ParsedFrontmatter {
        raw_block: Some(full_match.as_str().to_string()),
        body,
        parsed,
    })
}

fn extract_markdown_title(body: &str) -> Option<String> {
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(title) = trimmed.strip_prefix("# ") {
            let title = title.trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }

    None
}

fn extract_wikilinks(body: &str) -> Vec<VaultLinkMatch> {
    let wikilink_regex = Regex::new(r"\[\[([^\[\]\n]+)\]\]").expect("valid wikilink regex");
    let mut links = Vec::new();

    for captures in wikilink_regex.captures_iter(body) {
        let Some(inner_match) = captures.get(1) else {
            continue;
        };
        let inner = inner_match.as_str().trim();
        if inner.is_empty() {
            continue;
        }

        let (target_part, alias_part) = split_wikilink_alias(inner);
        let target = target_part.trim();
        if target.is_empty() {
            continue;
        }

        links.push(VaultLinkMatch {
            target: target.to_string(),
            alias: alias_part
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            line: byte_index_to_line(body, inner_match.start()),
        });
    }

    links
}

fn build_vault_lookup_maps<'a>(paths: impl Iterator<Item = &'a str>) -> VaultLookupMaps {
    let mut by_full_target = HashMap::new();
    let mut by_basename: HashMap<String, Vec<String>> = HashMap::new();

    for relative_path in paths {
        let normalized_relative_path = relative_path.trim().replace('\\', "/");
        if normalized_relative_path.is_empty() {
            continue;
        }

        let full_target = note_relative_path_to_link_target(&normalized_relative_path);
        by_full_target.insert(full_target.clone(), normalized_relative_path.clone());

        let basename = Path::new(&full_target)
            .file_name()
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default();
        if !basename.is_empty() {
            by_basename
                .entry(basename)
                .or_default()
                .push(normalized_relative_path.clone());
        }
    }

    for values in by_basename.values_mut() {
        values.sort();
        values.dedup();
    }

    VaultLookupMaps {
        by_full_target,
        by_basename,
    }
}

fn resolve_wikilink_path(
    source_relative_path: &str,
    target: &str,
    lookup: &VaultLookupMaps,
) -> Option<String> {
    let (target_without_alias, _) = split_wikilink_alias(target);
    let (target_without_heading, _) = split_wikilink_heading(target_without_alias);
    let normalized_target = normalize_link_target(target_without_heading);
    if normalized_target.is_empty() {
        return None;
    }

    let source_parent = Path::new(source_relative_path)
        .parent()
        .map(normalize_relative_path)
        .unwrap_or_default();

    if normalized_target.contains('/') {
        if !source_parent.is_empty() {
            let relative_candidate =
                normalize_link_target(&join_relative(&source_parent, &normalized_target));
            if let Some(found) = lookup.by_full_target.get(&relative_candidate) {
                return Some(found.clone());
            }
        }

        return lookup.by_full_target.get(&normalized_target).cloned();
    }

    if !source_parent.is_empty() {
        let relative_candidate =
            normalize_link_target(&join_relative(&source_parent, &normalized_target));
        if let Some(found) = lookup.by_full_target.get(&relative_candidate) {
            return Some(found.clone());
        }
    }

    if let Some(found) = lookup.by_full_target.get(&normalized_target) {
        return Some(found.clone());
    }

    lookup
        .by_basename
        .get(&normalized_target)
        .and_then(|candidates| {
            if candidates.len() == 1 {
                candidates.first().cloned()
            } else {
                None
            }
        })
}

fn collect_markdown_relative_paths(root: &Path) -> Result<Vec<String>, WorkspaceServiceError> {
    crate::repository_context::RepositoryContext::new(root)
        .and_then(|repository| repository.document_paths(true))
        .map_err(|e| WorkspaceServiceError::PathForbidden(e.to_string()))
}

fn rewrite_wikilinks_for_rename(
    body: &str,
    source_relative_path: &str,
    renamed_from_relative_path: &str,
    new_link_target: &str,
    lookup_before: &VaultLookupMaps,
) -> (String, usize) {
    let wikilink_regex = Regex::new(r"\[\[([^\[\]\n]+)\]\]").expect("valid wikilink regex");
    let mut replaced_count = 0usize;
    let renamed_from = renamed_from_relative_path.replace('\\', "/");

    let rewritten = wikilink_regex.replace_all(body, |captures: &regex::Captures<'_>| {
        let Some(inner_match) = captures.get(1) else {
            return captures
                .get(0)
                .map(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
        };
        let inner = inner_match.as_str().trim();
        let (target_part, alias_part) = split_wikilink_alias(inner);
        let (target_without_heading, heading_part) = split_wikilink_heading(target_part);
        let resolved =
            resolve_wikilink_path(source_relative_path, target_without_heading, lookup_before);

        if resolved.as_deref() != Some(renamed_from.as_str()) {
            return captures
                .get(0)
                .map(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
        }

        replaced_count += 1;

        let mut rebuilt = String::new();
        rebuilt.push_str("[[");
        rebuilt.push_str(new_link_target);
        if let Some(heading) = heading_part {
            rebuilt.push('#');
            rebuilt.push_str(heading.trim());
        }
        if let Some(alias) = alias_part {
            let trimmed = alias.trim();
            if !trimmed.is_empty() {
                rebuilt.push('|');
                rebuilt.push_str(trimmed);
            }
        }
        rebuilt.push_str("]]");
        rebuilt
    });

    (rewritten.to_string(), replaced_count)
}

fn write_text_file_atomically(path: &Path, content: &str) -> Result<(), WorkspaceServiceError> {
    let parent = path.parent().ok_or_else(|| {
        WorkspaceServiceError::PathForbidden("Caminho inválido para escrita.".to_string())
    })?;

    let mut temp_file = NamedTempFile::new_in(parent).map_err(WorkspaceServiceError::Io)?;
    temp_file
        .as_file_mut()
        .write_all(content.as_bytes())
        .map_err(WorkspaceServiceError::Io)?;
    temp_file
        .as_file_mut()
        .sync_all()
        .map_err(WorkspaceServiceError::Io)?;
    temp_file
        .persist(path)
        .map_err(|error| WorkspaceServiceError::Io(error.error))?;

    Ok(())
}

fn rebuild_content_with_frontmatter(frontmatter_block: Option<&str>, body: &str) -> String {
    if let Some(frontmatter) = frontmatter_block {
        let mut rebuilt = String::new();
        rebuilt.push_str(frontmatter);
        rebuilt.push_str(body);
        rebuilt
    } else {
        body.to_string()
    }
}

fn normalize_frontmatter_properties(frontmatter: Option<&Value>) -> Option<Value> {
    let object = frontmatter?.as_object()?;
    let mut normalized = serde_json::Map::new();

    for key in [
        "type",
        "domain",
        "risk",
        "status",
        "tested-by",
        "affects",
        "contracts",
        "decisions",
        "incidents",
        "covers",
        "scope",
        "last-pulse",
        "source-files",
        "test-files",
    ] {
        if let Some(value) = object.get(key) {
            normalized.insert(key.replace('-', "_"), value.clone());
        }
    }

    if normalized.is_empty() {
        None
    } else {
        Some(Value::Object(normalized))
    }
}

fn schema_warnings_for_note(frontmatter: Option<&Value>) -> Vec<String> {
    let mut warnings = Vec::new();
    let Some(frontmatter) = frontmatter else {
        warnings.push("frontmatter ausente.".to_string());
        return warnings;
    };
    let Some(object) = frontmatter.as_object() else {
        warnings.push("frontmatter inválido (esperado objeto YAML).".to_string());
        return warnings;
    };

    let note_type = object
        .get("type")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();

    if note_type == "feature" {
        if object.get("risk").and_then(Value::as_str).is_none() {
            warnings.push("feature sem `risk` definido.".to_string());
        }
        if object.get("status").and_then(Value::as_str).is_none() {
            warnings.push("feature sem `status` definido.".to_string());
        }
        let tested_by = object.get("tested-by");
        let has_tested_by = match tested_by {
            Some(Value::Array(values)) => !values.is_empty(),
            Some(Value::String(value)) => !value.trim().is_empty(),
            _ => false,
        };
        if !has_tested_by {
            warnings.push("feature sem `tested-by` vinculado.".to_string());
        }
        if frontmatter_string_list(Some(frontmatter), "source-files").is_empty() {
            warnings.push("feature sem `source-files` definido.".to_string());
        }
        if frontmatter_string_list(Some(frontmatter), "test-files").is_empty() {
            warnings.push("feature sem `test-files` definido.".to_string());
        }
    }

    if note_type == "test" {
        if object.get("covers").is_none() {
            warnings.push("test sem `covers` vinculado.".to_string());
        }
        if frontmatter_string_list(Some(frontmatter), "source-files").is_empty() {
            warnings.push("test sem `source-files` definido.".to_string());
        }
        if frontmatter_string_list(Some(frontmatter), "test-files").is_empty() {
            warnings.push("test sem `test-files` definido.".to_string());
        }
    }

    warnings
}

fn frontmatter_string_list(frontmatter: Option<&Value>, key: &str) -> Vec<String> {
    let Some(value) = frontmatter
        .and_then(Value::as_object)
        .and_then(|object| object.get(key))
    else {
        return Vec::new();
    };
    let mut values = match value {
        Value::Array(values) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
        Value::String(value) if !value.trim().is_empty() => vec![value.trim().to_string()],
        _ => Vec::new(),
    };
    values.sort();
    values.dedup();
    values
}

pub(crate) fn validate_generated_vault_note(
    _root: &Path,
    relative_path: &str,
    content: &str,
) -> Result<(), WorkspaceServiceError> {
    let normalized = relative_path.replace('\\', "/").to_ascii_lowercase();
    let strict_note = normalized.starts_with("knowledge/obsidian/features/")
        || normalized.starts_with("knowledge/obsidian/tests/");
    if !strict_note || !normalized.ends_with(".md") {
        return Ok(());
    }
    let parsed = parse_frontmatter_and_body(content)?;
    let frontmatter = parsed.parsed.as_ref().and_then(Value::as_object);
    // The versioned stage-composition contract (ADR-022) distinguishes physical
    // files from approved planned destinations, so its physical arrays may be
    // empty; it must instead carry all four arrays and a coverage state. Legacy
    // notes keep the original non-empty rule unchanged.
    let reference_contract = frontmatter
        .and_then(|object| object.get("reference-contract"))
        .and_then(Value::as_str);
    if reference_contract == Some(crate::orchestration::composition::render::REFERENCE_CONTRACT) {
        for field in [
            "source-files",
            "test-files",
            "planned-source-files",
            "planned-test-files",
        ] {
            let present = frontmatter.is_some_and(|object| {
                matches!(
                    object.get(field),
                    Some(Value::Array(_)) | Some(Value::String(_))
                )
            });
            if !present {
                return Err(WorkspaceServiceError::InvalidQuery(format!(
                    "Nota gerada em `{relative_path}` (stage-composition-v1) deve declarar `{field}` (lista, possivelmente vazia)."
                )));
            }
        }
        let coverage = frontmatter
            .and_then(|object| object.get("coverage-state"))
            .and_then(Value::as_str)
            .and_then(crate::orchestration::composition::render::CoverageState::parse);
        if coverage.is_none() {
            return Err(WorkspaceServiceError::InvalidQuery(format!(
                "Nota gerada em `{relative_path}` (stage-composition-v1) deve declarar `coverage-state` como complete, partial ou unavailable."
            )));
        }
        return Ok(());
    }
    for field in ["source-files", "test-files"] {
        let paths = frontmatter_string_list(parsed.parsed.as_ref(), field);
        if paths.is_empty() {
            return Err(WorkspaceServiceError::InvalidQuery(format!(
                "Nota gerada em `{relative_path}` deve preencher `{field}`."
            )));
        }
    }
    Ok(())
}

fn resolve_scope_file_name(agent_scope: Option<&str>, stage_name: Option<&str>) -> String {
    if let Some(scope) = agent_scope.map(str::trim).filter(|value| !value.is_empty()) {
        let normalized = scope.replace('\\', "/");
        if normalized.ends_with(".base") {
            return normalized;
        }
        return format!("{normalized}.base");
    }

    match stage_name.unwrap_or_default().trim() {
        "Intake" => "intake_router.base".to_string(),
        "SPECIFICATION" => "task_specifier.base".to_string(),
        "Contract" => "contract_agent.base".to_string(),
        "Code" => "backend_agent.base".to_string(),
        "Test" => "test_agent.base".to_string(),
        "Integrator" => "integrator.base".to_string(),
        _ => "task_specifier.base".to_string(),
    }
}

fn load_scope_filter(
    root: &Path,
    scope_file: &str,
) -> Result<Option<Value>, WorkspaceServiceError> {
    let cleaned = scope_file.trim().trim_start_matches('/');
    if cleaned.is_empty() {
        return Ok(None);
    }

    let relative = if cleaned.contains('/') {
        cleaned.to_string()
    } else {
        format!("knowledge/obsidian/bases/{cleaned}")
    };
    let absolute = root.join(&relative);
    if !absolute.exists() || !absolute.is_file() {
        return Ok(None);
    }

    let raw = fs::read_to_string(&absolute).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidData {
            WorkspaceServiceError::InvalidUtf8(absolute.to_string_lossy().to_string())
        } else {
            WorkspaceServiceError::Io(error)
        }
    })?;
    let parsed = serde_json::from_str::<Value>(&raw).map_err(|error| {
        WorkspaceServiceError::InvalidQuery(format!("Arquivo .base inválido `{relative}`: {error}"))
    })?;
    Ok(parsed.get("filters").cloned())
}

fn build_scoped_paths(cache: &VaultIndexCache, scope_filter: Option<&Value>) -> HashSet<String> {
    let mut scoped = HashSet::new();
    let Some(scope_filter) = scope_filter else {
        return scoped;
    };

    for note in cache.notes.values() {
        if note_matches_scope(note, scope_filter) {
            scoped.insert(note.relative_path.clone());
        }
    }

    scoped
}

fn note_matches_scope(note: &VaultCachedNote, filter: &Value) -> bool {
    evaluate_scope_expression(note.frontmatter.as_ref(), filter)
}

fn evaluate_scope_expression(frontmatter: Option<&Value>, expression: &Value) -> bool {
    let Some(object) = expression.as_object() else {
        return true;
    };

    if let Some(and_values) = object.get("and").and_then(Value::as_array) {
        return and_values
            .iter()
            .all(|item| evaluate_scope_expression(frontmatter, item));
    }

    if let Some(or_values) = object.get("or").and_then(Value::as_array) {
        return or_values
            .iter()
            .any(|item| evaluate_scope_expression(frontmatter, item));
    }

    evaluate_scope_clause(frontmatter, expression)
}

fn evaluate_scope_clause(frontmatter: Option<&Value>, clause: &Value) -> bool {
    let Some(object) = clause.as_object() else {
        return true;
    };
    let property = object
        .get("property")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let operator = object
        .get("operator")
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "equals".to_string());
    let expected = object.get("value");

    let Some(property) = property else {
        return true;
    };
    let actual = lookup_frontmatter_property(frontmatter, property);

    match operator.as_str() {
        "equals" => value_matches_equals(actual.as_ref(), expected),
        "not-equals" => !value_matches_equals(actual.as_ref(), expected),
        "in" => value_matches_in(actual.as_ref(), expected),
        _ => true,
    }
}

fn lookup_frontmatter_property(frontmatter: Option<&Value>, property: &str) -> Option<Value> {
    let object = frontmatter?.as_object()?;
    let normalized = property.trim().to_ascii_lowercase().replace('_', "-");

    if let Some(value) = object.get(&normalized) {
        return Some(value.clone());
    }

    let alt = normalized.replace('-', "_");
    if let Some(value) = object.get(&alt) {
        return Some(value.clone());
    }

    for (key, value) in object {
        if key.to_ascii_lowercase() == normalized {
            return Some(value.clone());
        }
    }

    None
}

fn value_matches_equals(actual: Option<&Value>, expected: Option<&Value>) -> bool {
    match (actual, expected) {
        (Some(Value::String(left)), Some(Value::String(right))) => {
            left.trim().eq_ignore_ascii_case(right.trim())
        }
        (Some(Value::Array(actual_items)), Some(Value::String(right))) => {
            actual_items.iter().any(|item| {
                item.as_str()
                    .map(|value| value.trim().eq_ignore_ascii_case(right.trim()))
                    .unwrap_or(false)
            })
        }
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn value_matches_in(actual: Option<&Value>, expected: Option<&Value>) -> bool {
    let Some(expected_values) = expected.and_then(Value::as_array) else {
        return false;
    };

    expected_values
        .iter()
        .any(|item| value_matches_equals(actual, Some(item)))
}

fn resolve_feature_note_path(cache: &VaultIndexCache, feature_name: &str) -> Option<String> {
    let tokens = feature_name
        .split(|ch: char| !ch.is_alphanumeric())
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.len() >= 3)
        .collect::<Vec<String>>();
    if tokens.is_empty() {
        return None;
    }

    let mut best: Option<(String, usize)> = None;
    for note in cache.notes.values() {
        if !normative_properties(&note.relative_path, note.frontmatter.as_ref()) {
            continue;
        }
        let note_type = lookup_frontmatter_property(note.frontmatter.as_ref(), "type")
            .and_then(|value| value.as_str().map(|raw| raw.to_ascii_lowercase()));
        if note_type.as_deref() != Some("feature") {
            continue;
        }

        let relative_lower = note.relative_path.to_ascii_lowercase();
        let title_lower = note.title.to_ascii_lowercase();
        let mut score = 0usize;
        for token in &tokens {
            if relative_lower.contains(token) {
                score += 3;
            }
            if title_lower.contains(token) {
                score += 2;
            }
        }

        if relative_lower.contains("knowledge/obsidian/features/") {
            score += 4;
        }

        if score == 0 {
            continue;
        }

        match &best {
            Some((_, best_score)) if *best_score >= score => {}
            _ => best = Some((note.relative_path.clone(), score)),
        }
    }

    best.map(|(path, _)| path)
}

fn direct_context_links(feature_note: &VaultCachedNote, lookup: &VaultLookupMaps) -> Vec<String> {
    let mut paths = Vec::new();
    for key in [
        "tested-by",
        "affects",
        "contracts",
        "decisions",
        "incidents",
    ] {
        for raw_target in extract_frontmatter_links(feature_note.frontmatter.as_ref(), key) {
            if let Some(resolved) =
                resolve_wikilink_path(&feature_note.relative_path, &raw_target, lookup)
            {
                paths.push(resolved);
            }
        }
    }

    paths
}

fn extract_frontmatter_links(frontmatter: Option<&Value>, key: &str) -> Vec<String> {
    let mut links = Vec::new();
    let Some(value) = lookup_frontmatter_property(frontmatter, key) else {
        return links;
    };

    match value {
        Value::String(raw) => {
            let trimmed = raw
                .trim()
                .trim_start_matches("[[")
                .trim_end_matches("]]")
                .trim();
            if !trimmed.is_empty() {
                links.push(trimmed.to_string());
            }
        }
        Value::Array(values) => {
            for item in values {
                if let Some(raw) = item.as_str() {
                    let trimmed = raw
                        .trim()
                        .trim_start_matches("[[")
                        .trim_end_matches("]]")
                        .trim();
                    if !trimmed.is_empty() {
                        links.push(trimmed.to_string());
                    }
                }
            }
        }
        _ => {}
    }

    links
}

fn expansion_depth_from_risk(frontmatter: Option<&Value>) -> usize {
    let risk = lookup_frontmatter_property(frontmatter, "risk")
        .and_then(|value| value.as_str().map(|raw| raw.to_ascii_lowercase()))
        .unwrap_or_else(|| "low".to_string());

    match risk.as_str() {
        "high" => 3,
        "medium" => 2,
        _ => 1,
    }
}

fn update_score(score_by_path: &mut HashMap<String, usize>, path: String, score: usize) {
    score_by_path
        .entry(path)
        .and_modify(|current| {
            if score > *current {
                *current = score;
            }
        })
        .or_insert(score);
}

fn build_textual_fallback_candidates(
    cache: &VaultIndexCache,
    feature_name: &str,
    stage_name: Option<&str>,
) -> Vec<ContextCandidate> {
    let mut terms = feature_name
        .split(|ch: char| !ch.is_alphanumeric())
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.len() >= 3)
        .collect::<Vec<String>>();

    if let Some(stage) = stage_name {
        terms.extend(
            stage
                .split(|ch: char| !ch.is_alphanumeric())
                .map(|token| token.trim().to_ascii_lowercase())
                .filter(|token| token.len() >= 3),
        );
    }
    terms.sort();
    terms.dedup();

    let mut candidates = Vec::new();
    for note in cache.notes.values() {
        if !normative_properties(&note.relative_path, note.frontmatter.as_ref()) {
            continue;
        }
        let relative = note.relative_path.clone();
        let lowered_path = relative.to_ascii_lowercase();

        let mut score = 0usize;
        for term in &terms {
            if lowered_path.contains(term) {
                score += 2;
            }
        }

        if score == 0 {
            let content = note.content.to_ascii_lowercase();
            for term in &terms {
                if content.contains(term) {
                    score += 1;
                }
            }
        }

        if score > 0 {
            candidates.push(ContextCandidate {
                content_sha256: note.revision.clone(),
                relative_path: relative,
                score,
            });
        }
    }

    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    candidates
}

/// Read the same bounded bytes for both the hash and the content supplied to a model.
pub(crate) fn read_context_document(
    root: &Path,
    relative: &str,
) -> Result<String, WorkspaceServiceError> {
    crate::orchestration::publication::safe_target(root, relative, false)
        .map_err(|e| WorkspaceServiceError::PathForbidden(e.to_string()))?;
    crate::repository_context::RepositoryContext::new(root)
        .and_then(|repository| repository.read_document(relative))
        .map_err(|e| WorkspaceServiceError::PathForbidden(e.to_string()))
}

pub(crate) fn is_normative_document(relative: &str, content: &str) -> bool {
    if relative
        .replace('\\', "/")
        .to_ascii_lowercase()
        .starts_with("knowledge/obsidian/learning/")
    {
        return false;
    }
    let Ok(parsed) = parse_frontmatter_and_body(content) else {
        return false;
    };
    normative_properties(relative, parsed.parsed.as_ref())
}

pub(crate) fn patch_learning_graph_relation(
    content: &str,
    relation: &str,
    target_path: &str,
) -> Result<String, WorkspaceServiceError> {
    let target = target_path
        .replace('\\', "/")
        .strip_suffix(".md")
        .unwrap_or(&target_path.replace('\\', "/"))
        .to_string();
    let link = format!("[[{target}]]");
    if relation == "wikilink" {
        let start = "<!-- hvc:learning-relations:start -->";
        let end = "<!-- hvc:learning-relations:end -->";
        if content
            .lines()
            .filter_map(|line| line.split("[[").nth(1))
            .filter_map(|tail| tail.split("]]").next())
            .any(|raw| same_wikilink_target(raw, &target))
        {
            return Ok(content.to_string());
        }
        let newline = if content.contains("\r\n") {
            "\r\n"
        } else {
            "\n"
        };
        if let Some(end_offset) = content.find(end) {
            if !content[..end_offset].contains(start) {
                return Err(WorkspaceServiceError::InvalidQuery(
                    "Seção gerenciada de relações está incompleta.".to_string(),
                ));
            }
            let mut patched = content.to_string();
            patched.insert_str(end_offset, &format!("- {link}{newline}"));
            return Ok(patched);
        }
        let separator = if content.is_empty() || content.ends_with(newline) {
            ""
        } else {
            newline
        };
        return Ok(format!(
            "{content}{separator}{newline}{start}{newline}## Relações sugeridas{newline}{newline}- {link}{newline}{end}{newline}"
        ));
    }
    if !matches!(relation, "tested-by" | "contracts" | "affects") {
        return Err(WorkspaceServiceError::InvalidQuery(
            "Relação de grafo não suportada.".to_string(),
        ));
    }
    let parsed = parse_frontmatter_and_body(content)?;
    let Some(frontmatter) = parsed.parsed.as_ref() else {
        return Err(WorkspaceServiceError::InvalidQuery(
            "Nota normativa sem frontmatter editável.".to_string(),
        ));
    };
    let Some(object) = frontmatter.as_object() else {
        return Err(WorkspaceServiceError::InvalidQuery(
            "Frontmatter complexo recusado.".to_string(),
        ));
    };
    if let Some(value) = object.get(relation) {
        let existing = match value {
            Value::String(value) => vec![value.as_str()],
            Value::Array(values) if values.iter().all(Value::is_string) => {
                values.iter().filter_map(Value::as_str).collect()
            }
            _ => {
                return Err(WorkspaceServiceError::InvalidQuery(format!(
                    "Campo `{relation}` usa construção YAML complexa; edição recusada."
                )));
            }
        };
        if existing
            .iter()
            .any(|value| same_wikilink_target(value, &target))
        {
            return Ok(content.to_string());
        }
    }
    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut lines = content
        .split(newline)
        .map(str::to_string)
        .collect::<Vec<_>>();
    if lines.first().map(String::as_str) != Some("---") {
        return Err(WorkspaceServiceError::InvalidQuery(
            "Frontmatter precisa iniciar na primeira linha.".to_string(),
        ));
    }
    let Some(end_index) = lines
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, line)| (line.trim() == "---").then_some(index))
    else {
        return Err(WorkspaceServiceError::InvalidQuery(
            "Frontmatter sem delimitador final.".to_string(),
        ));
    };
    let key_prefix = format!("{relation}:");
    if let Some(key_index) = (1..end_index).find(|index| {
        lines[*index].starts_with(&key_prefix) && !lines[*index].starts_with(char::is_whitespace)
    }) {
        let raw = lines[key_index][key_prefix.len()..].to_string();
        let (value_part, comment) = split_yaml_comment(&raw);
        let value = object
            .get(relation)
            .expect("frontmatter key found in parsed mapping");
        if value_part.trim().is_empty() {
            let insert_at = ((key_index + 1)..end_index)
                .find(|index| {
                    let line = &lines[*index];
                    !line.trim().is_empty()
                        && !line.starts_with(' ')
                        && !line.starts_with('\t')
                        && !line.trim_start().starts_with('#')
                })
                .unwrap_or(end_index);
            lines.insert(insert_at, format!("  - {}", yaml_quote(&link)));
        } else if value_part.trim().starts_with('[') && value_part.trim().ends_with(']') {
            let trimmed = value_part.trim();
            let inner = trimmed[1..trimmed.len() - 1].trim();
            let appended = if inner.is_empty() {
                yaml_quote(&link)
            } else {
                format!("{inner}, {}", yaml_quote(&link))
            };
            lines[key_index] = format!("{key_prefix} [{appended}]{}", comment);
        } else if let Value::String(existing) = value {
            lines[key_index] = format!(
                "{key_prefix} [{}, {}]{}",
                yaml_quote(existing),
                yaml_quote(&link),
                comment
            );
        } else {
            return Err(WorkspaceServiceError::InvalidQuery(format!(
                "Campo `{relation}` não é lista inline/bloco simples; edição recusada."
            )));
        }
    } else {
        lines.insert(end_index, format!("{relation}: [{}]", yaml_quote(&link)));
    }
    let patched = lines.join(newline);
    let reparsed = parse_frontmatter_and_body(&patched)?;
    if reparsed
        .parsed
        .as_ref()
        .and_then(Value::as_object)
        .is_none()
    {
        return Err(WorkspaceServiceError::InvalidQuery(
            "Patch produziria frontmatter YAML inválido.".to_string(),
        ));
    }
    Ok(patched)
}

fn same_wikilink_target(raw: &str, expected: &str) -> bool {
    let normalize = |value: &str| {
        value
            .trim()
            .trim_start_matches("[[")
            .trim_end_matches("]]")
            .split('|')
            .next()
            .unwrap_or("")
            .split('#')
            .next()
            .unwrap_or("")
            .trim()
            .replace('\\', "/")
            .trim_end_matches(".md")
            .to_ascii_lowercase()
    };
    normalize(raw) == normalize(expected)
}

fn split_yaml_comment(raw: &str) -> (&str, &str) {
    let mut single = false;
    let mut double = false;
    for (index, ch) in raw.char_indices() {
        match ch {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '#' if !single && !double => return (&raw[..index], &raw[index..]),
            _ => {}
        }
    }
    (raw, "")
}

fn yaml_quote(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization")
}

pub(crate) fn is_embedding_document(relative: &str, content: &str) -> bool {
    is_normative_document(relative, content) || is_component_document(relative, content)
}

fn is_component_document(relative: &str, content: &str) -> bool {
    if !relative
        .replace('\\', "/")
        .to_ascii_lowercase()
        .starts_with("knowledge/obsidian/components/")
    {
        return false;
    }
    let Ok(parsed) = parse_frontmatter_and_body(content) else {
        return false;
    };
    lookup_frontmatter_property(parsed.parsed.as_ref(), "type")
        .as_ref()
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case("component"))
}

pub(crate) fn component_mapping_status(relative: &str, content: &str) -> Option<&'static str> {
    if !is_component_document(relative, content) {
        return None;
    }
    let parsed = parse_frontmatter_and_body(content).ok()?;
    match lookup_frontmatter_property(parsed.parsed.as_ref(), "mapping-status")
        .as_ref()
        .and_then(Value::as_str)
    {
        Some(value) if value.eq_ignore_ascii_case("proposed") => Some("proposed"),
        Some(value) if value.eq_ignore_ascii_case("validated") => Some("validated"),
        Some(value) if value.eq_ignore_ascii_case("stale") => Some("stale"),
        _ => None,
    }
}

fn normative_properties(relative: &str, properties: Option<&Value>) -> bool {
    if relative
        .replace('\\', "/")
        .to_ascii_lowercase()
        .starts_with("knowledge/obsidian/learning/")
    {
        return false;
    }
    !lookup_frontmatter_property(properties, "type")
        .as_ref()
        .and_then(Value::as_str)
        .is_some_and(|v| {
            v.eq_ignore_ascii_case("learning_report") || v.eq_ignore_ascii_case("component")
        })
        && !lookup_frontmatter_property(properties, "status")
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|v| v.eq_ignore_ascii_case("deprecated"))
}

fn split_wikilink_alias(target: &str) -> (&str, Option<&str>) {
    match target.split_once('|') {
        Some((left, right)) => (left, Some(right)),
        None => (target, None),
    }
}

fn split_wikilink_heading(target: &str) -> (&str, Option<&str>) {
    match target.split_once('#') {
        Some((left, right)) => (left, Some(right)),
        None => (target, None),
    }
}

fn note_relative_path_to_link_target(relative_path: &str) -> String {
    normalize_link_target(relative_path)
}

fn normalize_link_target(target: &str) -> String {
    let mut normalized = target
        .trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string();

    if normalized.to_ascii_lowercase().ends_with(".md") {
        let new_length = normalized.len().saturating_sub(3);
        normalized.truncate(new_length);
    }

    normalized
}

fn byte_index_to_line(text: &str, byte_index: usize) -> usize {
    text[..byte_index].chars().filter(|ch| *ch == '\n').count() + 1
}

#[derive(Debug)]
struct VaultLookupMaps {
    by_full_target: HashMap<String, String>,
    by_basename: HashMap<String, Vec<String>>,
}

fn compile_search_pattern(
    query: &str,
    mode: SearchMode,
    case_sensitive: bool,
) -> Result<regex::Regex, WorkspaceServiceError> {
    let pattern_text = match mode {
        SearchMode::Substring => regex::escape(query),
        SearchMode::Regex => query.to_string(),
    };

    RegexBuilder::new(&pattern_text)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|error| WorkspaceServiceError::InvalidQuery(format!("Regex invalida: {error}")))
}

fn resolve_workspace_path(
    root: &Path,
    relative_path: &str,
) -> Result<PathBuf, WorkspaceServiceError> {
    let normalized_relative_path = parse_relative_path(relative_path)?;

    if !is_allowed_extension(&normalized_relative_path) {
        return Err(WorkspaceServiceError::InvalidExtension(
            "Apenas arquivos `.md` e `.txt` são permitidos.".to_string(),
        ));
    }

    let absolute_path = root.join(&normalized_relative_path);

    let parent = absolute_path.parent().ok_or_else(|| {
        WorkspaceServiceError::PathForbidden("Caminho fora do workspace permitido.".to_string())
    })?;
    let canonical_parent = fs::canonicalize(parent).map_err(WorkspaceServiceError::Io)?;

    if !canonical_parent.starts_with(root) {
        return Err(WorkspaceServiceError::PathForbidden(
            "Tentativa de acesso fora do workspace permitido.".to_string(),
        ));
    }

    if absolute_path.exists() {
        let canonical_file_path =
            fs::canonicalize(&absolute_path).map_err(WorkspaceServiceError::Io)?;
        if !canonical_file_path.starts_with(root) {
            return Err(WorkspaceServiceError::PathForbidden(
                "Tentativa de acesso fora do workspace permitido.".to_string(),
            ));
        }
    }

    Ok(absolute_path)
}

fn parse_relative_path(raw_relative_path: &str) -> Result<PathBuf, WorkspaceServiceError> {
    let trimmed = raw_relative_path.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceServiceError::PathForbidden(
            "relativePath não pode ser vazio.".to_string(),
        ));
    }

    let relative_path = Path::new(trimmed);
    if relative_path.is_absolute() {
        return Err(WorkspaceServiceError::PathForbidden(
            "Use apenas caminhos relativos ao workspace.".to_string(),
        ));
    }

    let mut normalized = PathBuf::new();

    for component in relative_path.components() {
        match component {
            Component::Normal(segment) => normalized.push(segment),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(WorkspaceServiceError::PathForbidden(
                    "Path traversal detectado. Use apenas caminhos relativos sem `..`.".to_string(),
                ));
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err(WorkspaceServiceError::PathForbidden(
            "relativePath inválido.".to_string(),
        ));
    }

    Ok(normalized)
}

fn byte_index_to_column(text: &str, byte_index: usize) -> usize {
    text[..byte_index].chars().count() + 1
}

fn compute_revision(path: &Path) -> Result<String, WorkspaceServiceError> {
    let metadata = fs::metadata(path).map_err(WorkspaceServiceError::Io)?;
    let modified = metadata.modified().map_err(WorkspaceServiceError::Io)?;
    let mtime_ms = modified
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    Ok(format!("{mtime_ms}:{}", metadata.len()))
}

fn is_allowed_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|extension| {
            let normalized_extension = extension.to_ascii_lowercase();
            normalized_extension == "md" || normalized_extension == "txt"
        })
        .unwrap_or(false)
}

fn is_watched_path(path: &Path) -> bool {
    is_allowed_extension(path)
        || crate::repository_context::RepositoryContext::is_code_relative_path(
            &path.to_string_lossy(),
        )
}

fn is_markdown_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|extension| extension.eq_ignore_ascii_case("md"))
        .unwrap_or(false)
}

fn walkdir_to_io_error(error: walkdir::Error) -> WorkspaceServiceError {
    let io_error = if let Some(source) = error.io_error() {
        io::Error::new(source.kind(), source.to_string())
    } else {
        io::Error::other(error.to_string())
    };

    WorkspaceServiceError::Io(io_error)
}

fn join_relative(base: &str, segment: &str) -> String {
    if base.is_empty() {
        segment.to_string()
    } else {
        format!("{base}/{segment}")
    }
}

fn to_relative_event_path(root: &Path, event_path: &Path) -> Option<String> {
    let absolute_path = if event_path.is_absolute() {
        event_path.to_path_buf()
    } else {
        root.join(event_path)
    };

    let relative_path = absolute_path.strip_prefix(root).ok()?;
    let normalized = normalize_relative_path(relative_path);

    if normalized.is_empty() {
        return None;
    }

    Some(normalized)
}

fn normalize_relative_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(segment) => Some(segment.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<String>>()
        .join("/")
}

fn map_event_kind(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::Create(_) => "created",
        EventKind::Modify(ModifyKind::Name(_)) => "renamed",
        EventKind::Modify(_) => "modified",
        EventKind::Remove(_) => "removed",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use std::thread;
    use std::time::Duration;

    use super::*;

    fn prepare_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir should be created")
    }

    #[test]
    fn rejects_path_traversal() {
        let root = prepare_root();
        let root_path = fs::canonicalize(root.path()).expect("canonical root path");

        let result = read_workspace_file(
            &root_path,
            ReadWorkspaceFileRequest {
                relative_path: "../secret.md".to_string(),
            },
        );

        match result {
            Err(WorkspaceServiceError::PathForbidden(_)) => {}
            _ => panic!("expected path forbidden error"),
        }
    }

    #[test]
    fn rejects_invalid_extension() {
        let root = prepare_root();
        let root_path = fs::canonicalize(root.path()).expect("canonical root path");

        let result = read_workspace_file(
            &root_path,
            ReadWorkspaceFileRequest {
                relative_path: "code.rs".to_string(),
            },
        );

        match result {
            Err(WorkspaceServiceError::InvalidExtension(_)) => {}
            _ => panic!("expected invalid extension error"),
        }
    }

    #[test]
    fn builds_filtered_tree_in_stable_order() {
        let root = prepare_root();
        fs::create_dir_all(root.path().join("b-folder")).expect("create directory b-folder");
        fs::create_dir_all(root.path().join("a-folder")).expect("create directory a-folder");
        fs::write(root.path().join("b-folder").join("zeta.txt"), "z").expect("write zeta file");
        fs::write(root.path().join("a-folder").join("alpha.md"), "a").expect("write alpha file");
        fs::write(root.path().join("ignore.rs"), "let x = 1;").expect("write ignored file");

        let root_path = fs::canonicalize(root.path()).expect("canonical root path");
        let tree = build_workspace_tree(&root_path).expect("workspace tree should be built");

        assert_eq!(tree.tree.len(), 2);
        assert_eq!(tree.tree[0].name, "a-folder");
        assert_eq!(tree.tree[1].name, "b-folder");
        assert_eq!(tree.tree[0].children[0].relative_path, "a-folder/alpha.md");
        assert_eq!(tree.tree[1].children[0].relative_path, "b-folder/zeta.txt");
    }

    #[test]
    fn writes_atomically_and_updates_revision() {
        let root = prepare_root();
        let file_path = root.path().join("note.md");
        fs::write(&file_path, "old content").expect("write initial content");
        let root_path = fs::canonicalize(root.path()).expect("canonical root path");

        let read_before = read_workspace_file(
            &root_path,
            ReadWorkspaceFileRequest {
                relative_path: "note.md".to_string(),
            },
        )
        .expect("read file before write");

        thread::sleep(Duration::from_millis(2));

        let write_result = write_workspace_file_atomic(
            &root_path,
            WriteWorkspaceFileAtomicRequest {
                relative_path: "note.md".to_string(),
                content: "brand new content".to_string(),
                expected_revision: read_before.revision.clone(),
            },
        )
        .expect("atomic write should succeed");

        let read_after = read_workspace_file(
            &root_path,
            ReadWorkspaceFileRequest {
                relative_path: "note.md".to_string(),
            },
        )
        .expect("read file after write");

        assert_eq!(read_after.content, "brand new content");
        assert_eq!(read_after.revision, write_result.revision);
        assert_ne!(read_after.revision, read_before.revision);
    }

    #[test]
    fn blocks_write_on_revision_conflict() {
        let root = prepare_root();
        let file_path = root.path().join("note.txt");
        fs::write(&file_path, "v1").expect("write initial file");
        let root_path = fs::canonicalize(root.path()).expect("canonical root path");

        let result = write_workspace_file_atomic(
            &root_path,
            WriteWorkspaceFileAtomicRequest {
                relative_path: "note.txt".to_string(),
                content: "v2".to_string(),
                expected_revision: "wrong-revision".to_string(),
            },
        );

        match result {
            Err(WorkspaceServiceError::Conflict { .. }) => {}
            _ => panic!("expected conflict error"),
        }
    }

    #[test]
    fn search_substring_respects_case_sensitivity() {
        let root = prepare_root();
        fs::write(root.path().join("sample.md"), "Hello world\nhello world\n").expect("write file");
        let root_path = fs::canonicalize(root.path()).expect("canonical root path");

        let insensitive = search_workspace_text(
            &root_path,
            SearchWorkspaceTextRequest {
                query: "hello".to_string(),
                mode: SearchMode::Substring,
                case_sensitive: false,
                max_results: 200,
            },
        )
        .expect("case-insensitive search should work");
        assert_eq!(insensitive.matches.len(), 2);

        let sensitive = search_workspace_text(
            &root_path,
            SearchWorkspaceTextRequest {
                query: "hello".to_string(),
                mode: SearchMode::Substring,
                case_sensitive: true,
                max_results: 200,
            },
        )
        .expect("case-sensitive search should work");
        assert_eq!(sensitive.matches.len(), 1);
        assert_eq!(sensitive.matches[0].line, 2);
        assert_eq!(sensitive.matches[0].column, 1);
    }

    #[test]
    fn search_regex_returns_matches() {
        let root = prepare_root();
        fs::write(root.path().join("sample.txt"), "alpha-123\nbeta-45\n").expect("write file");
        let root_path = fs::canonicalize(root.path()).expect("canonical root path");

        let response = search_workspace_text(
            &root_path,
            SearchWorkspaceTextRequest {
                query: r"[a-z]+-\d+".to_string(),
                mode: SearchMode::Regex,
                case_sensitive: true,
                max_results: 200,
            },
        )
        .expect("regex search should work");

        assert_eq!(response.matches.len(), 2);
        assert_eq!(response.matches[0].line, 1);
        assert_eq!(response.matches[1].line, 2);
    }

    #[test]
    fn search_regex_invalid_returns_invalid_query_error() {
        let root = prepare_root();
        fs::write(root.path().join("sample.md"), "anything").expect("write file");
        let root_path = fs::canonicalize(root.path()).expect("canonical root path");

        let response = search_workspace_text(
            &root_path,
            SearchWorkspaceTextRequest {
                query: "(".to_string(),
                mode: SearchMode::Regex,
                case_sensitive: true,
                max_results: 200,
            },
        );

        match response {
            Err(WorkspaceServiceError::InvalidQuery(_)) => {}
            _ => panic!("expected invalid query error"),
        }
    }

    #[test]
    fn search_respects_max_results_and_truncated_flag() {
        let root = prepare_root();
        fs::write(root.path().join("sample.md"), "item\nitem\nitem\n").expect("write file");
        let root_path = fs::canonicalize(root.path()).expect("canonical root path");

        let response = search_workspace_text(
            &root_path,
            SearchWorkspaceTextRequest {
                query: "item".to_string(),
                mode: SearchMode::Substring,
                case_sensitive: true,
                max_results: 2,
            },
        )
        .expect("search should work");

        assert_eq!(response.matches.len(), 2);
        assert!(response.truncated);
    }

    #[test]
    fn search_reports_correct_line_and_column() {
        let root = prepare_root();
        fs::write(
            root.path().join("positions.md"),
            "one\nxx target here\nz target\n",
        )
        .expect("write file");
        let root_path = fs::canonicalize(root.path()).expect("canonical root path");

        let response = search_workspace_text(
            &root_path,
            SearchWorkspaceTextRequest {
                query: "target".to_string(),
                mode: SearchMode::Substring,
                case_sensitive: true,
                max_results: 200,
            },
        )
        .expect("search should work");

        assert_eq!(response.matches.len(), 2);
        assert_eq!(response.matches[0].line, 2);
        assert_eq!(response.matches[0].column, 4);
        assert_eq!(response.matches[1].line, 3);
        assert_eq!(response.matches[1].column, 3);
    }

    #[test]
    fn bootstrap_greenfield_creates_expected_structure() {
        let root = prepare_root();
        fs::write(root.path().join(".gitignore"), "node_modules/\n").expect("write gitignore");
        fs::create_dir_all(root.path().join(".obsidian")).expect("create obsidian directory");

        let status = bootstrap_greenfield_workspace(root.path()).expect("bootstrap should succeed");
        assert_eq!(status.mode, "greenfield");
        assert!(status.initialized);
        assert!(root.path().join("docs").join("PROJECT_AGENT.md").is_file());
        assert!(
            root.path()
                .join("knowledge")
                .join("obsidian")
                .join("_templates")
                .join("feature-template.md")
                .is_file()
        );
        assert!(
            root.path()
                .join("knowledge")
                .join("obsidian")
                .join("bases")
                .join("backend-agent.base")
                .is_file()
        );
    }

    #[test]
    fn bootstrap_greenfield_rejects_legacy_workspace() {
        let root = prepare_root();
        fs::create_dir_all(root.path().join("src")).expect("create src directory");

        let result = bootstrap_greenfield_workspace(root.path());
        match result {
            Err(WorkspaceServiceError::InvalidRoot(message)) => {
                assert!(message.contains("Modo legacy"));
            }
            _ => panic!("expected invalid root error for legacy workspace"),
        }
    }

    #[test]
    fn brownfield_classification_is_non_destructive_and_knowledge_root_is_minimal() {
        let root = prepare_root();
        fs::create_dir_all(root.path().join("src")).expect("create source directory");
        let source = root.path().join("src/main.rs");
        let original = b"fn main() { println!(\"human\"); }\n";
        fs::write(&source, original).expect("write human source");

        assert_eq!(
            classify_workspace(root.path()).expect("classification should succeed"),
            WorkspaceKind::Brownfield
        );
        assert!(!root.path().join("knowledge").exists());
        assert_eq!(fs::read(&source).unwrap(), original);

        let created = prepare_brownfield_knowledge_root(root.path())
            .expect("authorized minimal structure should be created");
        assert_eq!(
            created,
            [
                "knowledge",
                "knowledge/obsidian",
                "knowledge/obsidian/components"
            ]
        );
        let components = root.path().join("knowledge/obsidian/components");
        assert!(components.is_dir());
        assert_eq!(fs::read_dir(&components).unwrap().count(), 0);
        assert!(!root.path().join("docs/PROJECT_AGENT.md").exists());
        assert!(!root.path().join("knowledge/obsidian/_templates").exists());
        assert_eq!(fs::read(&source).unwrap(), original);
        assert!(
            prepare_brownfield_knowledge_root(root.path())
                .expect("minimal structure should be idempotent")
                .is_empty()
        );
    }

    #[cfg(windows)]
    #[test]
    fn brownfield_knowledge_root_rejects_junction_components() {
        let root = prepare_root();
        let outside = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.path().join("knowledge"))
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(status.status.success(), "junction fixture creation");

        assert!(matches!(
            prepare_brownfield_knowledge_root(root.path()),
            Err(WorkspaceServiceError::PathForbidden(_))
        ));
        assert!(!outside.path().join("obsidian").exists());
    }

    #[test]
    fn vault_index_extracts_frontmatter_and_wikilinks() {
        let root = prepare_root();
        fs::create_dir_all(root.path().join("notes")).expect("create notes directory");
        fs::write(
            root.path().join("notes").join("feature-a.md"),
            "---\ntype: feature\nstatus: active\n---\n# Feature A\nLink [[feature-b]] and [[missing-note|Missing]].\n",
        )
        .expect("write feature A");
        fs::write(
            root.path().join("notes").join("feature-b.md"),
            "# Feature B\nBacklink [[notes/feature-a]].\n",
        )
        .expect("write feature B");

        let root_path = fs::canonicalize(root.path()).expect("canonical root path");
        let mut cache = VaultIndexCache::default();
        let response = get_workspace_vault_index(
            &root_path,
            &mut cache,
            GetWorkspaceVaultIndexRequest {
                force_rebuild: true,
            },
        )
        .expect("vault index should build");

        assert_eq!(response.notes.len(), 2);
        assert_eq!(response.edges.len(), 2);
        assert_eq!(response.broken_links.len(), 1);

        let feature_a = response
            .notes
            .iter()
            .find(|note| note.relative_path == "notes/feature-a.md")
            .expect("feature-a should be indexed");
        assert_eq!(feature_a.title, "Feature A");
        assert_eq!(feature_a.links.len(), 2);
        assert!(feature_a.frontmatter.is_some());
        assert!(feature_a.normalized_properties.is_some());
        assert!(!feature_a.schema_warnings.is_empty());
        assert!(
            feature_a
                .outgoing_paths
                .iter()
                .any(|path| path == "notes/feature-b.md")
        );
        assert!(
            feature_a
                .backlink_paths
                .iter()
                .any(|path| path == "notes/feature-b.md")
        );
    }

    #[test]
    fn vault_index_reports_broken_physical_references_without_rejecting_existing_notes() {
        let root = prepare_root();
        fs::create_dir_all(root.path().join("src")).unwrap();
        fs::create_dir_all(root.path().join("tests")).unwrap();
        fs::create_dir_all(root.path().join("knowledge/obsidian/features")).unwrap();
        fs::write(root.path().join("src/lib.rs"), "pub fn live() {}\n").unwrap();
        fs::write(root.path().join("tests/live.rs"), "#[test] fn live() {}\n").unwrap();
        fs::write(
            root.path().join("knowledge/obsidian/features/live.md"),
            "---\ntype: feature\nrisk: medium\nstatus: done\ntested-by: ['[[tests/live]]']\nsource-files:\n  - src/lib.rs\n  - src/missing.rs\ntest-files:\n  - tests/live.rs\n---\n# Live\n",
        )
        .unwrap();

        let mut cache = VaultIndexCache::default();
        let response = get_workspace_vault_index(
            root.path(),
            &mut cache,
            GetWorkspaceVaultIndexRequest {
                force_rebuild: true,
            },
        )
        .unwrap();

        let note = &response.notes[0];
        assert_eq!(note.source_files, vec!["src/lib.rs", "src/missing.rs"]);
        assert_eq!(note.test_files, vec!["tests/live.rs"]);
        assert_eq!(response.broken_file_references.len(), 1);
        assert_eq!(response.broken_file_references[0].target, "src/missing.rs");
        assert!(
            note.schema_warnings
                .iter()
                .any(|warning| warning.contains("src/missing.rs"))
        );
    }

    #[test]
    fn vault_document_references_are_valid_and_rechecked_after_removal() {
        let root = prepare_root();
        fs::create_dir_all(root.path().join("docs/architecture")).unwrap();
        fs::create_dir_all(root.path().join("knowledge/obsidian/tests")).unwrap();
        let documents = [
            "docs/architecture/adr.md",
            "docs/architecture/architecture.md",
            "docs/architecture/contract.md",
            "docs/architecture/spec.md",
        ];
        for path in documents {
            fs::write(root.path().join(path), "# Architecture\n").unwrap();
        }
        let note = format!(
            "---\nsource-files:\n{}test-files:\n  - knowledge/obsidian/tests/scenarios.md\n---\n# Architecture tests\nScenarios are authored. No execution evidence is available.\n",
            documents
                .iter()
                .map(|p| format!("  - {p}\n"))
                .collect::<String>()
        );
        for name in ["scenarios.md", "report.md"] {
            fs::write(
                root.path().join("knowledge/obsidian/tests").join(name),
                &note,
            )
            .unwrap();
        }
        let mut cache = VaultIndexCache::default();
        let index = get_workspace_vault_index(
            root.path(),
            &mut cache,
            GetWorkspaceVaultIndexRequest {
                force_rebuild: true,
            },
        )
        .unwrap();
        assert!(index.broken_file_references.is_empty());
        assert_eq!(
            index
                .notes
                .iter()
                .map(|note| note.source_files.len() + note.test_files.len())
                .sum::<usize>(),
            10
        );

        fs::remove_file(root.path().join(documents[0])).unwrap();
        // Reference validity is refreshed even when the referring notes did not change.
        let index = get_workspace_vault_index(
            root.path(),
            &mut cache,
            GetWorkspaceVaultIndexRequest {
                force_rebuild: false,
            },
        )
        .unwrap();
        assert_eq!(index.broken_file_references.len(), 2);
        assert!(
            index
                .broken_file_references
                .iter()
                .all(|issue| issue.target == documents[0] && !issue.reason.is_empty())
        );
    }

    #[test]
    fn vault_file_reference_issues_preserve_invalid_directory_and_security_failures() {
        let root = prepare_root();
        fs::create_dir_all(root.path().join("docs")).unwrap();
        fs::write(root.path().join(".env"), "TOKEN=fixture\n").unwrap();
        fs::write(root.path().join("ignored.rs"), "fn ignored() {}\n").unwrap();
        fs::write(root.path().join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(root.path().join("references.md"), "---\nsource-files: ['../outside.md', docs, .env, ignored.rs]\ntest-files: [missing.rs]\n---\n# References\n").unwrap();
        let index = get_workspace_vault_index(
            root.path(),
            &mut VaultIndexCache::default(),
            GetWorkspaceVaultIndexRequest {
                force_rebuild: true,
            },
        )
        .unwrap();
        let mut targets = index
            .broken_file_references
            .iter()
            .map(|issue| issue.target.as_str())
            .collect::<Vec<_>>();
        targets.sort();
        assert_eq!(
            targets,
            vec!["../outside.md", ".env", "docs", "ignored.rs", "missing.rs"]
        );
        assert!(
            index
                .broken_file_references
                .iter()
                .all(|issue| !issue.reason.is_empty())
        );
    }

    #[test]
    fn generated_feature_and_test_notes_require_both_physical_fields() {
        let root = prepare_root();
        let missing = validate_generated_vault_note(
            root.path(),
            "knowledge/obsidian/features/new.md",
            "---\ntype: feature\nsource-files: [src/lib.rs]\n---\n# New\n",
        );
        assert!(matches!(
            missing,
            Err(WorkspaceServiceError::InvalidQuery(_))
        ));

        validate_generated_vault_note(
            root.path(),
            "knowledge/obsidian/tests/new.test.md",
            "---\ntype: test\nsource-files: [src/lib.rs]\ntest-files: [tests/new.rs]\n---\n# Test\n",
        )
        .expect("new notes with both fields should pass schema validation");
    }

    #[test]
    fn context_candidates_follow_yaml_graph_with_scope() {
        let root = prepare_root();
        fs::write(root.path().join(".gitignore"), "node_modules/\n").expect("write gitignore");
        bootstrap_greenfield_workspace(root.path()).expect("bootstrap should succeed");

        fs::write(
            root.path()
                .join("knowledge")
                .join("obsidian")
                .join("features")
                .join("auth-login.md"),
            "---\ntype: feature\ndomain: backend\nrisk: high\nstatus: in-progress\ntested-by:\n  - \"[[knowledge/obsidian/tests/auth-login.test]]\"\naffects:\n  - \"[[knowledge/obsidian/features/session-management]]\"\n---\n# Auth Login\n",
        )
        .expect("write feature note");
        fs::write(
            root.path()
                .join("knowledge")
                .join("obsidian")
                .join("features")
                .join("session-management.md"),
            "---\ntype: feature\ndomain: backend\nrisk: medium\nstatus: planned\n---\n# Session Management\n",
        )
        .expect("write affected feature note");
        fs::write(
            root.path()
                .join("knowledge")
                .join("obsidian")
                .join("tests")
                .join("auth-login.test.md"),
            "---\ntype: test\nscope: integration\ncovers: \"[[knowledge/obsidian/features/auth-login]]\"\nstatus: green\n---\n# Auth Login Test\n",
        )
        .expect("write test note");

        let root_path = fs::canonicalize(root.path()).expect("canonical root path");
        let mut cache = VaultIndexCache::default();
        let response = get_context_candidates(
            &root_path,
            &mut cache,
            GetContextCandidatesRequest {
                feature_name: "Auth Login".to_string(),
                stage_name: Some("Test".to_string()),
                agent_scope: None,
                max_candidates: 10,
                force_rebuild: true,
            },
        )
        .expect("context candidates should build");

        assert_eq!(response.strategy, "yaml_graph");
        assert!(!response.candidates.is_empty());
        assert_eq!(
            response.candidates[0].relative_path,
            "knowledge/obsidian/features/auth-login.md"
        );
        assert!(
            response
                .candidates
                .iter()
                .any(|item| item.relative_path == "knowledge/obsidian/tests/auth-login.test.md")
        );
    }

    #[test]
    fn explicit_graph_expansion_increases_depth_and_returns_physical_targets() {
        let root = prepare_root();
        fs::create_dir_all(root.path().join("knowledge/obsidian/features")).unwrap();
        fs::write(root.path().join("src.rs"), "pub fn entry() {}\n").unwrap();
        let notes = [
            (
                "root.md",
                "---\ntype: feature\nrisk: low\nstatus: active\nsource-files: [src.rs]\ntest-files: [src.rs]\n---\n# Root\n[[knowledge/obsidian/features/one]]\n",
            ),
            (
                "one.md",
                "---\ntype: feature\nrisk: low\nstatus: active\n---\n# One\n[[knowledge/obsidian/features/two]]\n",
            ),
            (
                "two.md",
                "---\ntype: feature\nrisk: low\nstatus: active\n---\n# Two\n",
            ),
        ];
        for (name, content) in notes {
            fs::write(
                root.path().join("knowledge/obsidian/features").join(name),
                content,
            )
            .unwrap();
        }
        let request = || GetContextCandidatesRequest {
            feature_name: "Root".to_string(),
            stage_name: Some("Code".to_string()),
            agent_scope: None,
            max_candidates: 10,
            force_rebuild: true,
        };
        let mut cache = VaultIndexCache::default();
        let depth_one =
            get_context_candidates_with_expansion(root.path(), &mut cache, request(), 1, 8)
                .unwrap();
        let depth_two =
            get_context_candidates_with_expansion(root.path(), &mut cache, request(), 2, 8)
                .unwrap();

        assert!(
            !depth_one
                .candidates
                .iter()
                .any(|item| item.relative_path.ends_with("two.md"))
        );
        assert!(
            depth_two
                .candidates
                .iter()
                .any(|item| item.relative_path.ends_with("two.md"))
        );
        assert_eq!(depth_two.expansion_depth, 2);
        assert_eq!(depth_two.physical_file_references.len(), 2);
    }

    #[test]
    fn rename_workspace_note_updates_wikilinks_and_preserves_frontmatter() {
        let root = prepare_root();
        fs::create_dir_all(root.path().join("notes")).expect("create notes directory");
        fs::write(
            root.path().join("notes").join("old-name.md"),
            "---\ntype: feature\nstatus: active\n---\n# Old Name\nBody.\n",
        )
        .expect("write old name note");
        fs::write(
            root.path().join("notes").join("consumer.md"),
            "---\ntype: feature\n---\nLink [[old-name]] and [[old-name#Section|Alias]].\n",
        )
        .expect("write consumer note");

        let root_path = fs::canonicalize(root.path()).expect("canonical root path");
        let mut cache = VaultIndexCache::default();
        let rename_response = rename_workspace_note(
            &root_path,
            &mut cache,
            RenameWorkspaceNoteRequest {
                from_relative_path: "notes/old-name.md".to_string(),
                to_relative_path: "notes/new-name.md".to_string(),
            },
        )
        .expect("rename should work");

        assert_eq!(rename_response.renamed_to, "notes/new-name.md");
        assert_eq!(rename_response.updated_references, 2);
        assert_eq!(rename_response.affected_files, 1);
        assert!(root.path().join("notes").join("new-name.md").exists());
        assert!(!root.path().join("notes").join("old-name.md").exists());

        let consumer_content = fs::read_to_string(root.path().join("notes").join("consumer.md"))
            .expect("read consumer after rename");
        assert!(
            consumer_content
                .contains("Link [[notes/new-name]] and [[notes/new-name#Section|Alias]].")
        );
        assert!(consumer_content.starts_with("---\ntype: feature\n---\n"));
    }
}

#[cfg(test)]
mod document_cache_tests {
    use super::*;
    use crate::document_cache::SharedDocumentCache;
    fn request() -> GetContextCandidatesRequest {
        GetContextCandidatesRequest {
            feature_name: "billing".into(),
            stage_name: Some("SPECIFICATION".into()),
            agent_scope: None,
            max_candidates: 20,
            force_rebuild: false,
        }
    }
    #[test]
    fn shared_cache_reuses_parsing_and_revalidates_same_size_edits_and_deletions() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("billing.md"), "billing alpha").unwrap();
        let shared = SharedDocumentCache::default();
        let first = shared
            .with(root.path(), |cache| {
                let result = get_context_candidates(root.path(), cache, request())?;
                assert_eq!(cache.parse_count, 1);
                Ok(result)
            })
            .unwrap();
        shared
            .clone()
            .with(&root.path().join("."), |cache| {
                get_context_candidates(root.path(), cache, request())?;
                assert_eq!(cache.parse_count, 1);
                Ok(())
            })
            .unwrap();
        // No timestamp assumptions: different bytes of identical length.
        fs::write(root.path().join("billing.md"), "billing bravo").unwrap();
        let second = shared
            .with(root.path(), |cache| {
                let result = get_context_candidates(root.path(), cache, request())?;
                assert_eq!(cache.parse_count, 2);
                Ok(result)
            })
            .unwrap();
        assert_ne!(
            first.candidates[0].content_sha256,
            second.candidates[0].content_sha256
        );
        fs::remove_file(root.path().join("billing.md")).unwrap();
        shared
            .with(root.path(), |cache| {
                assert!(
                    get_context_candidates(root.path(), cache, request())?
                        .candidates
                        .is_empty()
                );
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn root_isolation_txt_fallback_and_ignored_documents_are_preserved() {
        let left = tempfile::tempdir().unwrap();
        let right = tempfile::tempdir().unwrap();
        fs::write(left.path().join("billing.txt"), "left billing").unwrap();
        fs::write(right.path().join("billing.txt"), "right billing").unwrap();
        fs::write(left.path().join(".gitignore"), "ignored.md\n").unwrap();
        fs::write(left.path().join("ignored.md"), "billing hidden").unwrap();
        let shared = SharedDocumentCache::default();
        let left_hash = shared
            .with(left.path(), |cache| {
                let result = get_context_candidates(left.path(), cache, request())?;
                assert_eq!(result.candidates.len(), 1);
                assert_eq!(result.candidates[0].relative_path, "billing.txt");
                assert!(
                    get_workspace_vault_index(
                        left.path(),
                        cache,
                        GetWorkspaceVaultIndexRequest {
                            force_rebuild: false
                        }
                    )?
                    .notes
                    .is_empty()
                );
                Ok(result.candidates[0].content_sha256.clone())
            })
            .unwrap();
        shared
            .with(right.path(), |cache| {
                let result = get_context_candidates(right.path(), cache, request())?;
                assert_ne!(left_hash, result.candidates[0].content_sha256);
                Ok(())
            })
            .unwrap();
        assert!(read_context_document(left.path(), "ignored.md").is_err());
    }
    #[test]
    fn learning_and_deprecated_notes_cannot_seed_or_bridge_normative_graph() {
        let root = tempfile::tempdir().unwrap();
        for (path, content) in [
            (
                "billing.md",
                "---\ntype: feature\nrisk: high\n---\n# billing\n[[proposal]]",
            ),
            (
                "proposal.md",
                "---\ntype: learning_report\nstatus: proposed\n---\nbilling [[unrelated]]",
            ),
            (
                "component.md",
                "---\ntype: component\nmapping-status: proposed\n---\nbilling [[unrelated]]",
            ),
            ("deprecated.md", "---\nstatus: deprecated\n---\nbilling"),
            ("unrelated.md", "must not be reached through a proposal"),
        ] {
            fs::write(root.path().join(path), content).unwrap();
        }
        let mut cache = VaultIndexCache::default();
        let result = get_context_candidates(root.path(), &mut cache, request()).unwrap();
        assert_eq!(
            result
                .candidates
                .iter()
                .map(|c| c.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec!["billing.md"]
        );
        let mut query = request();
        query.feature_name = "proposal".into();
        let result = get_context_candidates(root.path(), &mut cache, query).unwrap();
        assert!(
            result
                .candidates
                .iter()
                .all(|c| c.relative_path != "proposal.md")
        );
        let mut query = request();
        query.feature_name = "component".into();
        let result = get_context_candidates(root.path(), &mut cache, query).unwrap();
        assert!(
            result
                .candidates
                .iter()
                .all(|candidate| candidate.relative_path != "component.md")
        );
    }

    #[test]
    fn learning_graph_patcher_preserves_yaml_shapes_comments_crlf_and_aliases() {
        let missing = "---\ntype: feature\nrisk: low\n---\n# Root\n";
        let patched =
            patch_learning_graph_relation(missing, "tested-by", "knowledge/obsidian/tests/auth.md")
                .unwrap();
        assert!(patched.contains("tested-by: [\"[[knowledge/obsidian/tests/auth]]\"]"));

        let inline = "---\ntype: feature\naffects: [\"[[one]]\"] # keep\n---\n# Root\n";
        let patched =
            patch_learning_graph_relation(inline, "affects", "knowledge/obsidian/features/two.md")
                .unwrap();
        assert!(
            patched
                .contains("affects: [\"[[one]]\", \"[[knowledge/obsidian/features/two]]\"]# keep")
        );

        let block = "---\r\ntype: feature\r\ncontracts: # retained\r\n  - \"[[knowledge/obsidian/contracts/api|API]]\"\r\nrisk: low\r\n---\r\n# Root\r\n";
        let duplicate = patch_learning_graph_relation(
            block,
            "contracts",
            "knowledge/obsidian/contracts/api.md",
        )
        .unwrap();
        assert_eq!(duplicate, block);
        let patched = patch_learning_graph_relation(
            block,
            "contracts",
            "knowledge/obsidian/contracts/events.md",
        )
        .unwrap();
        assert!(patched.contains("  - \"[[knowledge/obsidian/contracts/events]]\"\r\nrisk: low"));
        assert!(patched.contains("contracts: # retained"));
    }

    #[test]
    fn learning_graph_patcher_refuses_complex_yaml_and_manages_body_links() {
        let complex = "---\ntype: feature\naffects:\n  nested: value\n---\n# Root\n";
        assert!(patch_learning_graph_relation(complex, "affects", "target.md").is_err());

        let body = "---\ntype: decision\n---\n# Root\n";
        let patched = patch_learning_graph_relation(body, "wikilink", "target.md").unwrap();
        assert!(patched.contains("<!-- hvc:learning-relations:start -->"));
        assert!(patched.contains("- [[target]]"));
        assert_eq!(
            patch_learning_graph_relation(&patched, "wikilink", "target.md").unwrap(),
            patched
        );
    }

    #[test]
    fn watcher_accepts_code_and_notes_without_broadening_editor_file_types() {
        assert!(is_watched_path(Path::new("src/lib.rs")));
        assert!(is_watched_path(Path::new(
            "knowledge/obsidian/components/runtime.md"
        )));
        assert!(!is_watched_path(Path::new(".env")));
        assert!(!is_allowed_extension(Path::new("src/lib.rs")));
    }
}

#[cfg(test)]
mod stage_scope_tests {
    use super::*;
    use serde_json::json;

    fn matches(base_content: &str, frontmatter: serde_json::Value) -> bool {
        let filters = serde_json::from_str::<Value>(base_content).unwrap()["filters"].clone();
        evaluate_scope_expression(Some(&frontmatter), &filters)
    }

    fn base_for(stage: &str) -> &'static str {
        let name = resolve_scope_file_name(None, Some(stage));
        STAGE_SCOPE_FILES
            .iter()
            .find(|(path, _)| path.ends_with(&name))
            .unwrap_or_else(|| panic!("{stage} resolves to {name}, which has no seeded spec"))
            .1
    }

    /// The name the retrieval path asks for and the file the bootstrap writes
    /// have to agree; today they only do by coincidence.
    #[test]
    fn every_stage_resolves_to_a_seeded_scope_file() {
        let specs = greenfield_file_specs();
        for (_, stage, _) in crate::orchestration::catalog::STAGES {
            let name = resolve_scope_file_name(None, Some(stage));
            let relative = format!("knowledge/obsidian/bases/{name}");
            let (_, content) = specs
                .iter()
                .find(|(path, _)| *path == relative)
                .unwrap_or_else(|| panic!("{stage} wants {relative}, which is never written"));
            let parsed = serde_json::from_str::<Value>(content)
                .unwrap_or_else(|error| panic!("{relative} is not valid JSON: {error}"));
            assert!(
                parsed.get("filters").is_some(),
                "{relative} has no `filters` key"
            );
        }
    }

    /// The frontmatter the forge actually publishes, not the `_templates` one.
    #[test]
    fn the_published_plan_note_reaches_intake_and_the_integrator() {
        let plan = json!({ "type": "feature_plan", "status": "approved_plan" });
        assert!(matches(base_for("Intake"), plan.clone()));
        assert!(
            matches(base_for("Integrator"), plan.clone()),
            "the Integrator reconciles plans; it used to filter for incidents only"
        );
        assert!(matches(base_for("SPECIFICATION"), plan.clone()));
        assert!(matches(base_for("Code"), plan));
    }

    #[test]
    fn triage_does_not_receive_test_notes_but_the_test_stage_does() {
        let test_note = json!({ "type": "test", "status": "red" });
        assert!(!matches(base_for("Intake"), test_note.clone()));
        assert!(matches(base_for("Test"), test_note));
    }

    #[test]
    fn a_filter_that_matches_nothing_still_degrades_to_no_scoping() {
        let cache = VaultIndexCache::default();
        let filter = serde_json::from_str::<Value>(base_for("Intake")).unwrap()["filters"].clone();
        assert!(
            build_scoped_paths(&cache, Some(&filter)).is_empty(),
            "an empty scope set is what the retrieval fail-open branch keys on"
        );
    }

    #[test]
    fn repair_realigns_stale_defaults_and_preserves_developer_edits() {
        let root = tempfile::tempdir().unwrap();
        let bases = root.path().join("knowledge/obsidian/bases");
        fs::create_dir_all(&bases).unwrap();

        // A stale shipped default, a developer-authored file, and a gap.
        fs::write(
            bases.join("intake_router.base"),
            STALE_STAGE_BASE_DEFAULTS[0],
        )
        .unwrap();
        let authored = "{\n  \"filters\": { \"and\": [] }\n}\n";
        fs::write(bases.join("integrator.base"), authored).unwrap();

        let repaired = repair_stage_scope_files(root.path()).unwrap();
        assert!(repaired.contains(&"knowledge/obsidian/bases/intake_router.base".to_string()));
        assert!(!repaired.contains(&"knowledge/obsidian/bases/integrator.base".to_string()));

        assert_eq!(
            fs::read_to_string(bases.join("intake_router.base")).unwrap(),
            INTAKE_ROUTER_BASE
        );
        assert_eq!(
            fs::read_to_string(bases.join("integrator.base")).unwrap(),
            authored,
            "a customized scope file must survive byte for byte"
        );
        assert_eq!(
            fs::read_to_string(bases.join("test_agent.base")).unwrap(),
            TEST_AGENT_BASE,
            "a missing stage file is filled in"
        );

        assert!(
            repair_stage_scope_files(root.path()).unwrap().is_empty(),
            "repair must be idempotent"
        );
    }

    #[test]
    fn repair_never_creates_files_in_an_unbootstrapped_workspace() {
        let root = tempfile::tempdir().unwrap();
        assert!(repair_stage_scope_files(root.path()).unwrap().is_empty());
        assert!(!root.path().join("knowledge").exists());
    }
}
