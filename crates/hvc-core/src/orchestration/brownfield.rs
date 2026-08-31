use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::orchestration::{CollectionBudget, ContextCoverage};
use crate::repository_context::{InventoryFile, RepositoryContext, RepositoryError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrownfieldConsentState {
    NotDecided,
    Declined,
    Granted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrownfieldOnboardingStatus {
    pub project_id: String,
    pub consent: BrownfieldConsentState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<BrownfieldCoverageMetrics>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrownfieldCoverageMetrics {
    /// True only after the bounded structural bootstrap committed successfully.
    pub frontier_initialized: bool,
    /// Every source currently known by the persisted frontier, including ignored sources.
    pub inventory_known_sources: u32,
    /// Explicit denominator for every ratio: known sources minus ignored sources.
    pub eligible_source_denominator: u32,
    pub ignored_sources: u32,
    pub extracted_valid_sources: u32,
    pub extracted_partial_sources: u32,
    pub extracted_stale_sources: u32,
    pub extraction_failed_sources: u32,
    /// Integer basis points (0..=10_000), avoiding floating-point serialization drift.
    pub extraction_progress_basis_points: u32,
    /// Documentation counts distinct eligible sources and assigns one state per source.
    pub documentation_proposed_sources: u32,
    pub documentation_validated_sources: u32,
    pub documentation_stale_sources: u32,
    #[serde(default)]
    pub resolution_complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolver_version: Option<String>,
    #[serde(default)]
    pub resolved_references: u32,
    #[serde(default)]
    pub ambiguous_references: u32,
    #[serde(default)]
    pub external_references: u32,
    #[serde(default)]
    pub unresolved_references: u32,
    #[serde(default)]
    pub resolution_elapsed_us: u64,
}

impl BrownfieldCoverageMetrics {
    pub const SCALE_BASIS_POINTS: u32 = 10_000;

    pub fn extraction_percent(&self) -> u32 {
        self.extraction_progress_basis_points / 100
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetBrownfieldConsentRequest {
    pub project_id: String,
    pub consent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetBrownfieldIgnoredRequest {
    pub project_id: String,
    pub relative_path: String,
    pub ignored: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrownfieldFrontierStatus {
    Pending,
    InProgress,
    Partial,
    Mapped,
    Stale,
    Ignored,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrownfieldFrontierEntry {
    pub project_id: String,
    pub relative_path: String,
    pub status: BrownfieldFrontierStatus,
    pub content_sha256: Option<String>,
    pub in_degree: u32,
    pub priority_reason: String,
    pub budget_consumed: u32,
    pub attempts: u32,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageSummary {
    pub language: String,
    pub file_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrownfieldInventory {
    pub manifests: Vec<String>,
    pub languages: Vec<LanguageSummary>,
    pub entry_points: Vec<String>,
    pub source_files: u32,
    pub total_size_bytes: u64,
    pub coverage: ContextCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrownfieldOnboardingResult {
    pub status: BrownfieldOnboardingStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inventory: Option<BrownfieldInventory>,
}

pub struct InventoryScanner;

impl InventoryScanner {
    pub fn scan(
        root: &Path,
        budget: CollectionBudget,
        cancellation: &CancellationToken,
    ) -> Result<BrownfieldInventory, RepositoryError> {
        let repository = RepositoryContext::new(root)?;
        let (files, mut coverage) = repository.inventory_files(budget, cancellation)?;
        let mut manifests = Vec::new();
        let mut entry_points = Vec::new();
        let mut languages = BTreeMap::<&'static str, u32>::new();
        let mut source_files = 0_u32;
        let mut total_size_bytes = 0_u64;
        let mut manifests_truncated = false;

        for file in files {
            total_size_bytes = total_size_bytes.saturating_add(file.size_bytes);
            if is_manifest(&file.relative_path) {
                if manifests.len() < budget.max_package_files as usize {
                    manifests.push(file.relative_path.clone());
                } else {
                    manifests_truncated = true;
                }
            }
            if let Some(language) = language_for(&file) {
                source_files = source_files.saturating_add(1);
                *languages.entry(language).or_default() += 1;
            }
            if is_entry_point(&file.relative_path) {
                entry_points.push(file.relative_path);
            }
        }

        if manifests_truncated {
            coverage.truncated = true;
            coverage.state = crate::orchestration::CoverageState::Partial;
            coverage.diagnostics.push(format!(
                "truncated: mais de {} manifests encontrados.",
                budget.max_package_files
            ));
        }
        manifests.sort();
        entry_points.sort();
        let languages = languages
            .into_iter()
            .map(|(language, file_count)| LanguageSummary {
                language: language.to_string(),
                file_count,
            })
            .collect();

        Ok(BrownfieldInventory {
            manifests,
            languages,
            entry_points,
            source_files,
            total_size_bytes,
            coverage,
        })
    }
}

fn is_manifest(relative_path: &str) -> bool {
    let name = relative_path
        .rsplit('/')
        .next()
        .unwrap_or(relative_path)
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "cargo.toml"
            | "package.json"
            | "pyproject.toml"
            | "requirements.txt"
            | "go.mod"
            | "go.work"
            | "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "settings.gradle"
            | "settings.gradle.kts"
            | "tsconfig.json"
    ) || name.ends_with(".csproj")
        || name.ends_with(".sln")
}

fn language_for(file: &InventoryFile) -> Option<&'static str> {
    let lower = file.relative_path.to_ascii_lowercase();
    let extension = Path::new(&lower).extension()?.to_str()?;
    match extension {
        "rs" => Some("Rust"),
        "ts" | "tsx" => Some("TypeScript"),
        "js" | "jsx" => Some("JavaScript"),
        "py" => Some("Python"),
        "go" => Some("Go"),
        "java" => Some("Java"),
        "kt" | "kts" => Some("Kotlin"),
        "cs" => Some("C#"),
        "c" | "h" => Some("C"),
        "cc" | "cpp" | "hpp" => Some("C++"),
        "rb" => Some("Ruby"),
        "php" => Some("PHP"),
        "swift" => Some("Swift"),
        "sql" => Some("SQL"),
        "sh" | "ps1" => Some("Shell"),
        _ => None,
    }
}

fn is_entry_point(relative_path: &str) -> bool {
    let normalized = relative_path.to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or(&normalized);
    matches!(
        name,
        "main.rs"
            | "lib.rs"
            | "main.ts"
            | "main.tsx"
            | "index.ts"
            | "index.tsx"
            | "main.js"
            | "index.js"
            | "main.py"
            | "app.py"
            | "manage.py"
            | "main.go"
            | "program.cs"
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::orchestration::{CoverageState, DocumentationAvailability};

    #[test]
    fn inventories_mixed_rust_and_typescript_without_ignored_or_sensitive_files() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("src")).unwrap();
        fs::create_dir_all(root.path().join("web/src")).unwrap();
        fs::write(root.path().join("Cargo.toml"), "[package]\nname='mixed'\n").unwrap();
        fs::write(root.path().join("package.json"), "{}").unwrap();
        fs::write(root.path().join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.path().join("web/src/index.ts"), "export {};\n").unwrap();
        fs::write(
            root.path().join(".gitignore"),
            "ignored.ts\nnested/*\n!nested/keep.ts\n",
        )
        .unwrap();
        fs::write(root.path().join("ignored.ts"), "const secret = true;\n").unwrap();
        fs::write(root.path().join(".env"), "TOKEN=secret\n").unwrap();
        fs::create_dir_all(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested/drop.ts"), "export {};\n").unwrap();
        fs::write(root.path().join("nested/keep.ts"), "export {};\n").unwrap();
        fs::write(root.path().join("image.png"), [0_u8, 1, 2, 3]).unwrap();

        let inventory = InventoryScanner::scan(
            root.path(),
            CollectionBudget::default(),
            &CancellationToken::new(),
        )
        .unwrap();

        assert_eq!(inventory.manifests, ["Cargo.toml", "package.json"]);
        assert_eq!(inventory.entry_points, ["src/main.rs", "web/src/index.ts"]);
        assert_eq!(inventory.source_files, 3);
        assert_eq!(
            inventory
                .languages
                .iter()
                .map(|item| (item.language.as_str(), item.file_count))
                .collect::<Vec<_>>(),
            [("Rust", 1), ("TypeScript", 2)]
        );
        assert_eq!(inventory.coverage.read_bytes, 0);
        assert_eq!(inventory.coverage.state, CoverageState::Complete);
        assert_eq!(
            inventory.coverage.documentation,
            DocumentationAvailability::Absent
        );
    }

    #[test]
    fn reports_empty_manifest_inventory_and_honors_cancellation() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("main.py"), "print('safe')\n").unwrap();
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(
            InventoryScanner::scan(root.path(), CollectionBudget::default(), &cancelled).is_err()
        );

        let inventory = InventoryScanner::scan(
            root.path(),
            CollectionBudget::default(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(inventory.manifests.is_empty());
        assert_eq!(inventory.source_files, 1);

        fs::write(root.path().join("other.py"), "print('bounded')\n").unwrap();
        let bounded = InventoryScanner::scan(
            root.path(),
            CollectionBudget {
                max_files: 1,
                max_package_files: 1,
                ..CollectionBudget::default()
            },
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(bounded.coverage.enumerated_files, 1);
        assert!(bounded.coverage.truncated);
    }

    #[cfg(windows)]
    #[test]
    fn inventory_never_follows_junctions() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("main.rs"), "fn main() {}\n").unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.path().join("linked"))
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(status.status.success(), "junction fixture creation");

        let inventory = InventoryScanner::scan(
            root.path(),
            CollectionBudget::default(),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(inventory.source_files, 0);
        assert!(inventory.entry_points.is_empty());
    }
}
