use std::collections::HashSet;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use super::{ContractError, SchemaV1};

/// Lexically normalized only. The reader/publication service must still check
/// canonical containment, ignores, secrets and symlinks against the actual root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelativeSourcePath(String);

impl RelativeSourcePath {
    pub fn new(value: impl Into<String>) -> Result<Self, ContractError> {
        let value = value.into().replace('\\', "/");
        if value.is_empty()
            || value
                .chars()
                .any(|ch| ch.is_control() || ":<>\"|?*".contains(ch))
            || value.split('/').any(|part| {
                part.is_empty() || matches!(part, "." | "..") || part.ends_with(['.', ' '])
            })
        {
            return Err(ContractError::Invalid("path relativo"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RelativeSourcePath {
    type Error = ContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RelativeSourcePath> for String {
    fn from(value: RelativeSourcePath) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContentSha256(String);

impl ContentSha256 {
    pub fn of(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ContentSha256 {
    type Error = ContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ContractError::Invalid("SHA-256"));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

impl From<ContentSha256> for String {
    fn from(value: ContentSha256) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRevision {
    pub project_id: String,
    pub relative_path: RelativeSourcePath,
    pub content_sha256: ContentSha256,
}

/// Captured bytes, not a path that will be read again later. Construction is not
/// permission to read a file: production callers must use RepositoryContext.
#[derive(Clone)]
pub struct SourceSnapshot {
    revision: SourceRevision,
    content: Arc<str>,
}

impl SourceSnapshot {
    pub fn from_text(
        project_id: impl Into<String>,
        relative_path: RelativeSourcePath,
        content: String,
    ) -> Result<Self, ContractError> {
        let project_id = project_id.into();
        if project_id.trim().is_empty() || content.contains('\0') {
            return Err(ContractError::Invalid("fonte textual/projeto"));
        }
        if content.len() > 1024 * 1024 {
            return Err(ContractError::BudgetExceeded);
        }
        Ok(Self {
            revision: SourceRevision {
                project_id,
                relative_path,
                content_sha256: ContentSha256::of(content.as_bytes()),
            },
            content: Arc::from(content),
        })
    }

    pub fn revision(&self) -> &SourceRevision {
        &self.revision
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn snippet(&self, start_line: u32, end_line: u32) -> Result<CodeSnippet, ContractError> {
        if start_line == 0 || end_line < start_line {
            return Err(ContractError::Invalid("intervalo de linhas"));
        }
        let lines: Vec<_> = self.content.split_inclusive('\n').collect();
        if end_line as usize > lines.len() {
            return Err(ContractError::Invalid("linha fora da fonte capturada"));
        }
        Ok(CodeSnippet {
            source: self.revision.clone(),
            start_line,
            end_line,
            content: lines[start_line as usize - 1..end_line as usize].concat(),
        })
    }
}

impl fmt::Debug for SourceSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceSnapshot")
            .field("revision", &self.revision)
            .field("bytes", &self.content.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractorRevision {
    pub name: String,
    pub version: String,
    pub policy_version: String,
    /// Future resolver/manifest fingerprint; absent for the lexical collector.
    pub resolution_context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFactKey {
    pub source: SourceRevision,
    pub extractor: ExtractorRevision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeSnippet {
    pub source: SourceRevision,
    pub start_line: u32,
    pub end_line: u32,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StructuredSymbolKind {
    Function,
    Method,
    Struct,
    Class,
    Trait,
    Interface,
    Enum,
    TypeAlias,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolVisibility {
    #[default]
    Unknown,
    Private,
    Project,
    Public,
    Exported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StructuredSymbol {
    pub source: SourceRevision,
    pub kind: StructuredSymbolKind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualified_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_symbol_key: Option<String>,
    #[serde(default)]
    pub visibility: SymbolVisibility,
    pub start_line: u32,
    pub end_line: u32,
    /// Bounded declaration header, never the entire symbol body.
    pub signature: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceRelation {
    Import,
    Reexport,
    Call,
    TypeUse,
    Inherits,
    Implements,
    #[default]
    Lexical,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceBindingKind {
    Default,
    Namespace,
    Type,
    #[default]
    Named,
    Glob,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceBinding {
    pub imported: String,
    pub local: String,
    #[serde(default)]
    pub kind: ReferenceBindingKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ReferenceDescriptor {
    Module {
        specifier: String,
        #[serde(default)]
        bindings: Vec<ReferenceBinding>,
    },
    Symbol {
        path: Vec<String>,
    },
    Type {
        path: Vec<String>,
    },
    Implementation {
        trait_path: Vec<String>,
        for_type_path: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionEvidence {
    pub method: String,
    pub resolver_version: String,
    pub context_sha256: ContentSha256,
    pub candidate_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ReferenceResolution {
    Unresolved { reason: String },
    Ambiguous { candidates: u32, reason: String },
    External { module: String },
    Resolved { target: RelativeSourcePath },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservedReference {
    pub source: SourceRevision,
    pub line: u32,
    pub observed_text: String,
    #[serde(default)]
    pub relation: ReferenceRelation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub descriptor: Option<ReferenceDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_symbol_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_symbol_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_revision: Option<ContentSha256>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_evidence: Option<ResolutionEvidence>,
    pub resolution: ReferenceResolution,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageState {
    Complete,
    Partial,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocumentationAvailability {
    Unknown,
    Absent,
    Partial,
    Available,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCoverage {
    /// Complete refers to the requested bounded scope, never the entire repository.
    pub state: CoverageState,
    pub documentation: DocumentationAvailability,
    pub enumerated_files: u32,
    pub read_bytes: u64,
    pub truncated: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionBudget {
    pub max_enumerations: u32,
    pub max_files: u32,
    pub max_read_bytes: u64,
    pub max_file_bytes: u64,
    pub max_package_files: u32,
    pub max_snippet_bytes: u64,
    pub max_duration_ms: u64,
}

impl Default for CollectionBudget {
    fn default() -> Self {
        Self {
            max_enumerations: 1,
            max_files: 2_000,
            max_read_bytes: 16 * 1024 * 1024,
            max_file_bytes: 1024 * 1024,
            max_package_files: 12,
            max_snippet_bytes: 32 * 1024,
            max_duration_ms: 2_000,
        }
    }
}

impl CollectionBudget {
    pub fn validate(self) -> Result<(), ContractError> {
        let maximum = Self::default();
        if self.max_enumerations != 1
            || self.max_files > maximum.max_files
            || self.max_read_bytes > maximum.max_read_bytes
            || self.max_file_bytes > maximum.max_file_bytes
            || self.max_package_files > maximum.max_package_files
            || self.max_snippet_bytes > maximum.max_snippet_bytes
            || self.max_duration_ms > maximum.max_duration_ms
            || self.max_files == 0
            || self.max_file_bytes == 0
            || self.max_file_bytes > self.max_read_bytes
            || self.max_package_files == 0
            || self.max_package_files > self.max_files
            || self.max_snippet_bytes == 0
            || self.max_duration_ms == 0
        {
            return Err(ContractError::Invalid("orçamento de coleta"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeContextFacts {
    pub schema_version: SchemaV1,
    pub project_id: String,
    pub extractor: ExtractorRevision,
    pub scope: String,
    /// Fingerprint of the bounded discovered set, including non-selected source content.
    #[serde(default)]
    pub candidate_set_sha256: Option<ContentSha256>,
    pub sources: Vec<SourceRevision>,
    pub snippets: Vec<CodeSnippet>,
    #[serde(default)]
    pub symbols: Vec<StructuredSymbol>,
    pub references: Vec<ObservedReference>,
    pub coverage: ContextCoverage,
}

impl CodeContextFacts {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut sources = HashSet::new();
        if self.project_id.trim().is_empty()
            || self.scope.trim().is_empty()
            || self.extractor.name.trim().is_empty()
            || self.extractor.version.trim().is_empty()
            || self.extractor.policy_version.trim().is_empty()
            || self
                .sources
                .iter()
                .any(|source| !sources.insert(&source.relative_path))
            || self
                .sources
                .iter()
                .any(|source| source.project_id != self.project_id)
            || self.snippets.iter().any(|snippet| {
                !self.sources.contains(&snippet.source)
                    || snippet.start_line == 0
                    || snippet.end_line < snippet.start_line
            })
            || self.symbols.iter().any(|symbol| {
                !self.sources.contains(&symbol.source)
                    || symbol.name.trim().is_empty()
                    || symbol.symbol_key.as_deref().is_some_and(|key| {
                        key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                    || symbol
                        .qualified_name
                        .as_deref()
                        .is_some_and(|name| name.trim().is_empty())
                    || symbol.owner_symbol_key.as_deref().is_some_and(|key| {
                        key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                    || symbol.start_line == 0
                    || symbol.end_line < symbol.start_line
                    || symbol.signature.len() > 512
            })
            || self.references.iter().any(|reference| {
                !self.sources.contains(&reference.source)
                    || reference.line == 0
                    || reference.observed_text.trim().is_empty()
                    || reference.reference_key.as_deref().is_some_and(|key| {
                        key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                    || reference
                        .descriptor
                        .as_ref()
                        .is_some_and(|descriptor| !reference_descriptor_is_valid(descriptor))
                    || !reference_resolution_is_valid(reference)
            })
            || (self.coverage.truncated && self.coverage.state == CoverageState::Complete)
            || (self.coverage.state == CoverageState::Unavailable && !self.sources.is_empty())
        {
            return Err(ContractError::Invalid("proveniência/cobertura do contexto"));
        }
        Ok(())
    }

    /// Ensures snippets were derived from these exact captured bytes. This is
    /// separate from freshness checks against a later filesystem revision.
    pub fn validate_against(&self, snapshots: &[SourceSnapshot]) -> Result<(), ContractError> {
        self.validate()?;
        for source in &self.sources {
            if !snapshots
                .iter()
                .any(|snapshot| snapshot.revision() == source)
            {
                return Err(ContractError::StaleExecution);
            }
        }
        for snippet in &self.snippets {
            let snapshot = snapshots
                .iter()
                .find(|snapshot| snapshot.revision() == &snippet.source)
                .ok_or(ContractError::StaleExecution)?;
            if snapshot.snippet(snippet.start_line, snippet.end_line)? != *snippet {
                return Err(ContractError::Invalid(
                    "snippet divergente da fonte capturada",
                ));
            }
        }
        for reference in &self.references {
            let snapshot = snapshots
                .iter()
                .find(|snapshot| snapshot.revision() == &reference.source)
                .ok_or(ContractError::StaleExecution)?;
            let line = snapshot.snippet(reference.line, reference.line)?;
            if !line.content.contains(&reference.observed_text) {
                return Err(ContractError::Invalid(
                    "referência divergente da linha observada",
                ));
            }
            if let ReferenceResolution::Resolved { target } = &reference.resolution
                && let Some(target_revision) = &reference.target_revision
            {
                let target_snapshot = snapshots
                    .iter()
                    .find(|candidate| {
                        candidate.revision().project_id == reference.source.project_id
                            && candidate.revision().relative_path == *target
                    })
                    .ok_or(ContractError::StaleExecution)?;
                if &target_snapshot.revision().content_sha256 != target_revision {
                    return Err(ContractError::StaleExecution);
                }
                if let Some(target_symbol_key) = &reference.target_symbol_key
                    && !self.symbols.iter().any(|symbol| {
                        symbol.source == *target_snapshot.revision()
                            && symbol.symbol_key.as_ref() == Some(target_symbol_key)
                    })
                {
                    return Err(ContractError::StaleExecution);
                }
            }
        }
        for symbol in &self.symbols {
            let snapshot = snapshots
                .iter()
                .find(|snapshot| snapshot.revision() == &symbol.source)
                .ok_or(ContractError::StaleExecution)?;
            let declaration = snapshot.snippet(symbol.start_line, symbol.end_line)?;
            if !declaration.content.contains(&symbol.name) {
                return Err(ContractError::Invalid(
                    "símbolo divergente da fonte capturada",
                ));
            }
        }
        Ok(())
    }
}

fn reference_descriptor_is_valid(descriptor: &ReferenceDescriptor) -> bool {
    match descriptor {
        ReferenceDescriptor::Module {
            specifier,
            bindings,
        } => {
            !specifier.trim().is_empty()
                && bindings.iter().all(|binding| {
                    !binding.imported.trim().is_empty() && !binding.local.trim().is_empty()
                })
        }
        ReferenceDescriptor::Symbol { path } | ReferenceDescriptor::Type { path } => {
            !path.is_empty() && path.iter().all(|part| !part.trim().is_empty())
        }
        ReferenceDescriptor::Implementation {
            trait_path,
            for_type_path,
        } => {
            !trait_path.is_empty()
                && !for_type_path.is_empty()
                && trait_path.iter().all(|part| !part.trim().is_empty())
                && for_type_path.iter().all(|part| !part.trim().is_empty())
        }
    }
}

fn reference_resolution_is_valid(reference: &ObservedReference) -> bool {
    let target_metadata =
        reference.target_symbol_key.is_some() || reference.target_revision.is_some();
    let target_key_valid = reference
        .target_symbol_key
        .as_deref()
        .is_none_or(|key| key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()));
    target_key_valid
        && match &reference.resolution {
            ReferenceResolution::Resolved { .. } => true,
            ReferenceResolution::Ambiguous { candidates, reason } => {
                *candidates >= 2 && !reason.trim().is_empty() && !target_metadata
            }
            ReferenceResolution::External { module } => {
                !module.trim().is_empty() && !target_metadata
            }
            ReferenceResolution::Unresolved { reason } => {
                !reason.trim().is_empty() && !target_metadata
            }
        }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeContextDiscoveryMode {
    RepositoryScan,
    ExactPaths,
}

#[derive(Debug, Clone)]
pub struct CodeContextRequest {
    pub project_id: String,
    pub workspace_root: PathBuf,
    pub query: String,
    pub hinted_paths: Vec<RelativeSourcePath>,
    pub discovery_mode: CodeContextDiscoveryMode,
    pub budget: CollectionBudget,
}

pub type CodeContextFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CodeContextFacts, ContractError>> + Send + 'a>>;

/// Collection is independent of pulsations and can serve a first request without
/// PreSpec or a vault. RoutedCodeContextService adds structural extraction behind this contract.
pub trait CodeContextService: Send + Sync {
    fn collect<'a>(
        &'a self,
        request: &'a CodeContextRequest,
        cancellation: &'a CancellationToken,
    ) -> CodeContextFuture<'a>;
}

/// CPU-only extraction over captured bytes; no filesystem or UI dependencies.
pub trait CodeContextExtractor: Send + Sync {
    fn extract(
        &self,
        sources: &[SourceSnapshot],
        budget: CollectionBudget,
        cancellation: &CancellationToken,
    ) -> Result<CodeContextFacts, ContractError>;
}
