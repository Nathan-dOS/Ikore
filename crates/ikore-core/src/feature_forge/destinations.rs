//! Bounded, host-owned file planning. Destinations supplement the immutable
//! approved task; they never revise its scope or grant publication authority.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::service::invalid;
use super::{Feature, WorkTask};
use crate::orchestration::composition::{RelPath, recipes, review_policy};
use crate::persistence_service::PersistenceServiceError;
use crate::repository_context::{
    ListRepositoryTreeArgs, RepositoryContext, is_rust_binary_source_directory,
};

const MAX_TREE_ENTRIES: usize = 1_000;
const MAX_PATHS_PER_TASK: usize = 8;
const MAX_PATHS_PER_BATCH: usize = 256;
const MAX_MANIFEST_BYTES: usize = 32 * 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DestinationOrigin {
    Manual,
    TaskText,
    RepositoryConvention,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskDestinations {
    pub task_id: String,
    pub paths: Vec<RelPath>,
    pub origin: DestinationOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationResolution {
    pub task_destinations: Vec<TaskDestinations>,
    pub repository_truncated: bool,
    pub greenfield: bool,
    pub notices: Vec<String>,
}

/// Validate both existing and future files. Existing ancestors are checked
/// independently so a symlink cannot hide behind a not-yet-created leaf.
pub fn validate_destination_path(
    root: &Path,
    value: &str,
) -> Result<RelPath, PersistenceServiceError> {
    let path = RelPath::new(value).map_err(invalid)?;
    if review_policy::is_review_policy_path(value) || value.split('/').any(forbidden_component) {
        return Err(invalid(
            "Task destination is a private, generated or policy path",
        ));
    }
    let file_name = value.rsplit('/').next().unwrap_or(value);
    if !file_name.contains('.')
        && !matches!(
            file_name.to_ascii_lowercase().as_str(),
            "dockerfile" | "makefile" | "license"
        )
    {
        return Err(invalid(
            "Task destination must name a file, not a repository directory",
        ));
    }
    let canonical_root = root.canonicalize().map_err(invalid)?;
    if !canonical_root.is_dir() {
        return Err(invalid("Task destination workspace is not a directory"));
    }
    let mut candidate = canonical_root.clone();
    for component in value.split('/') {
        candidate.push(component);
        if component.eq_ignore_ascii_case("bin") && !is_rust_binary_source_directory(&candidate) {
            return Err(invalid(
                "Generated binary directories are not task destinations",
            ));
        }
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) => {
                if is_link(&metadata) {
                    return Err(invalid(
                        "Task destinations cannot cross symbolic links or reparse points",
                    ));
                }
                let canonical = candidate.canonicalize().map_err(invalid)?;
                if !canonical.starts_with(&canonical_root) {
                    return Err(invalid("Task destination is outside the repository"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(invalid(error)),
        }
    }
    if candidate.is_dir() {
        return Err(invalid(
            "Task destination names a directory instead of a file",
        ));
    }
    if candidate.exists() {
        RepositoryContext::new(&canonical_root)
            .map_err(invalid)?
            .read_document(value)
            .map_err(invalid)?;
    }
    Ok(path)
}

fn is_link(metadata: &std::fs::Metadata) -> bool {
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

fn forbidden_component(component: &str) -> bool {
    let name = component.to_ascii_lowercase();
    name.starts_with(".env")
        || name.contains("service-account")
        || name.contains("private-key")
        || matches!(
            name.as_str(),
            ".git"
                | ".hvc"
                | ".ikore"
                | ".hard-vibe-code"
                | ".agents"
                | ".claude"
                | ".codex"
                | ".gemini"
                | ".opencode"
                | ".aws"
                | ".ssh"
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
                | "knowledge"
                | "target"
                | "node_modules"
                | "dist"
                | "build"
                | "out"
                | "obj"
                | "vendor"
                | "id_rsa"
                | "id_dsa"
                | "id_ecdsa"
                | "id_ed25519"
                | "credentials"
                | "secrets"
                | "credentials.json"
                | "credentials.yaml"
                | "credentials.yml"
                | "credentials.toml"
                | "secrets.json"
                | "secrets.yaml"
                | "secrets.yml"
                | "secrets.toml"
                | ".npmrc"
                | ".pypirc"
                | ".netrc"
        )
        || [".pem", ".key", ".p12", ".pfx", ".crt", ".cer"]
            .iter()
            .any(|extension| name.ends_with(extension))
}

/// Resolve every selected task using explicit paths first and repository
/// conventions second. This performs local reads only and creates no files.
pub fn resolve_task_destinations(
    root: &Path,
    tasks: &[WorkTask],
    features: &[Feature],
    manual_hints: &HashMap<String, String>,
) -> Result<DestinationResolution, PersistenceServiceError> {
    if tasks.is_empty() || tasks.len() > 100 {
        return Err(invalid(
            "Destination planning requires between 1 and 100 tasks",
        ));
    }
    if manual_hints
        .keys()
        .any(|id| !tasks.iter().any(|task| &task.id == id))
    {
        return Err(invalid("A path hint belongs to an unselected task"));
    }
    let repository = RepositoryContext::new(root).map_err(invalid)?;
    let tree = repository
        .list_tree(ListRepositoryTreeArgs {
            path: None,
            depth: Some(8),
            max_entries: Some(MAX_TREE_ENTRIES),
        })
        .map_err(invalid)?;
    let files: BTreeSet<String> = tree
        .entries
        .iter()
        .filter(|entry| entry.kind == "file")
        .filter(|entry| !entry.path.split('/').any(forbidden_component))
        .map(|entry| entry.path.clone())
        .collect();
    let directories: BTreeSet<String> = tree
        .entries
        .iter()
        .filter(|entry| entry.kind == "directory")
        .filter(|entry| !entry.path.split('/').any(forbidden_component))
        .map(|entry| entry.path.clone())
        .collect();
    let inventory_truncated = tree.truncated
        || tree
            .entries
            .iter()
            .any(|entry| entry.kind == "directory" && entry.path.split('/').count() >= 8);
    let greenfield = !inventory_truncated && !files.iter().any(|path| is_application_file(path));
    let package = repository
        .read_document("package.json")
        .ok()
        .filter(|content| content.len() <= MAX_MANIFEST_BYTES)
        .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok());
    let cargo = repository
        .read_document("Cargo.toml")
        .ok()
        .filter(|content| content.len() <= MAX_MANIFEST_BYTES)
        .and_then(|content| toml::from_str::<toml::Value>(&content).ok());
    let mut result = DestinationResolution {
        task_destinations: Vec::new(),
        repository_truncated: inventory_truncated,
        greenfield,
        notices: Vec::new(),
    };
    if inventory_truncated {
        result.notices.push("Repository destination inventory was bounded; omitted files are not evidence that they do not exist.".into());
    }
    let mut path_count = 0;
    for task in tasks {
        let text = format!("{} {}", task.draft.title, task.draft.description);
        let explicit = recipes::explicit_paths(&text);
        let (raw_paths, origin) = if let Some(hint) = manual_hints
            .get(&task.id)
            .filter(|hint| !hint.trim().is_empty())
        {
            (
                hint.split([',', ';', '\n'])
                    .map(|value| value.trim().trim_matches('`').to_owned())
                    .filter(|value| !value.is_empty())
                    .collect(),
                DestinationOrigin::Manual,
            )
        } else if !explicit.is_empty() {
            (
                explicit
                    .iter()
                    .map(|path| path.as_str().to_owned())
                    .collect(),
                DestinationOrigin::TaskText,
            )
        } else {
            let feature_scope = task
                .draft
                .feature_id
                .as_ref()
                .and_then(|id| features.iter().find(|feature| &feature.id == id))
                .map(|feature| {
                    format!(
                        "{} {} {} {}",
                        feature.brief.objective,
                        feature.brief.behavior,
                        feature.brief.scope,
                        feature.brief.decisions.join(" ")
                    )
                })
                .unwrap_or_default();
            let scope = format!("{feature_scope} {text}");
            let stack = Stack::detect(&files, package.as_ref(), &scope);
            let prefer_typescript = package.as_ref().is_some_and(|package| {
                ["dependencies", "devDependencies"].iter().any(|field| {
                    package
                        .get(field)
                        .and_then(|dependencies| dependencies.get("typescript"))
                        .is_some()
                })
            }) || (!files.iter().any(|path| {
                [".js", ".jsx", ".mjs", ".cjs"]
                    .iter()
                    .any(|extension| path.ends_with(extension))
            }) && words(&scope.to_ascii_lowercase())
                .contains("typescript"));
            let (paths, unknown) = convention_paths(
                task,
                &files,
                &directories,
                stack,
                prefer_typescript,
                bootstrap_entrypoint(stack, cargo.as_ref(), package.as_ref(), &scope).as_deref(),
            );
            if unknown && result.notices.len() < 16 {
                result.notices.push(format!("Task '{}' has no established implementation stack; docs/architecture.md records the architecture decision before implementation.", task.draft.title));
            }
            (paths, DestinationOrigin::RepositoryConvention)
        };
        let mut paths = Vec::new();
        for value in raw_paths {
            let path = validate_destination_path(root, &value)?;
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        if paths.is_empty() || paths.len() > MAX_PATHS_PER_TASK {
            return Err(invalid("A task requires between 1 and 8 destination files"));
        }
        path_count += paths.len();
        if path_count > MAX_PATHS_PER_BATCH {
            return Err(invalid(
                "The batch exceeds the bounded destination plan (256 files)",
            ));
        }
        result.task_destinations.push(TaskDestinations {
            task_id: task.id.clone(),
            paths,
            origin,
        });
    }
    Ok(result)
}

fn is_application_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    if lower.starts_with("docs/")
        || lower.starts_with("documents/")
        || lower.starts_with("documentos/")
    {
        return false;
    }
    RepositoryContext::is_code_relative_path(path)
        || lower.ends_with(".html")
        || lower.ends_with(".htm")
        || lower.ends_with(".css")
        || lower.ends_with(".scss")
        || lower.ends_with(".vue")
        || lower.ends_with(".svelte")
}

#[derive(Clone, Copy)]
enum Stack {
    Rust,
    Next,
    React,
    Node,
    Static,
    Unknown,
}

impl Stack {
    fn detect(files: &BTreeSet<String>, package: Option<&serde_json::Value>, scope: &str) -> Self {
        let has_dependency = |name: &str| {
            package.is_some_and(|package| {
                ["dependencies", "devDependencies"].iter().any(|field| {
                    package
                        .get(field)
                        .and_then(|dependencies| dependencies.get(name))
                        .is_some()
                })
            })
        };
        let scope = scope.to_ascii_lowercase();
        let tokens = words(&scope);
        if has_dependency("next") || files.iter().any(|path| path.starts_with("next.config.")) {
            Self::Next
        } else if has_dependency("react") {
            Self::React
        } else if has_dependency("vite") || package.is_some() {
            Self::Node
        } else if files.contains("Cargo.toml") || files.iter().any(|path| path.ends_with(".rs")) {
            Self::Rust
        } else if files.iter().any(|path| path.ends_with(".html")) {
            Self::Static
        } else if files.iter().any(|path| {
            [".js", ".ts", ".tsx", ".jsx"]
                .iter()
                .any(|extension| path.ends_with(extension))
        }) {
            Self::Node
        } else if tokens.contains("rust") {
            Self::Rust
        } else if scope.contains("next.js") || scope.contains("nextjs") {
            Self::Next
        } else if tokens.contains("react") {
            Self::React
        } else if (tokens.contains("static") && (tokens.contains("web") || tokens.contains("html")))
            || (tokens.contains("html") && tokens.contains("javascript"))
        {
            Self::Static
        } else if tokens.contains("node")
            || tokens.contains("nodejs")
            || tokens.contains("javascript")
        {
            Self::Node
        } else {
            Self::Unknown
        }
    }
}

fn convention_paths(
    task: &WorkTask,
    files: &BTreeSet<String>,
    directories: &BTreeSet<String>,
    stack: Stack,
    prefer_typescript: bool,
    entrypoint: Option<&str>,
) -> (Vec<String>, bool) {
    let title = task.draft.title.to_ascii_lowercase();
    if let Some(document) = planning_document(&title) {
        return (vec![document.into()], false);
    }
    let task_words = useful_words(&title);
    let description = task.draft.description.to_ascii_lowercase();
    let description_words = useful_words(&description);
    let mut relevant: Vec<(usize, String)> = files
        .iter()
        .filter(|path| is_application_file(path))
        .map(|path| {
            let lower_path = path.to_ascii_lowercase();
            let path_words = words(&lower_path);
            (
                4 * task_words
                    .iter()
                    .filter(|word| path_words.contains(word.as_str()))
                    .count()
                    + description_words
                        .iter()
                        .filter(|word| path_words.contains(word.as_str()))
                        .count(),
                path.clone(),
            )
        })
        .filter(|(score, _)| *score > 0)
        .collect();
    relevant.sort_by(|(a_score, a_path), (b_score, b_path)| {
        b_score.cmp(a_score).then_with(|| a_path.cmp(b_path))
    });
    if !relevant.is_empty() {
        let mut paths = Vec::new();
        for (_, path) in relevant {
            if paths.contains(&path) {
                continue;
            }
            if paths.len() == MAX_PATHS_PER_TASK {
                break;
            }
            paths.push(path.clone());
            if let Some(test) = corresponding_test_path(&path, files, directories)
                && paths.len() < MAX_PATHS_PER_TASK
                && !paths.contains(&test)
            {
                paths.push(test);
            }
        }
        return (paths, false);
    }
    let slug = if task_words.is_empty() {
        "feature".into()
    } else {
        task_words
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("-")
    };
    let typed = prefer_typescript
        || files.iter().any(|path| {
            path.ends_with(".ts") || path.ends_with(".tsx") || path.ends_with("tsconfig.json")
        });
    let jsx = if typed { "tsx" } else { "jsx" };
    let javascript = if typed { "ts" } else { "js" };
    let test_dir = if directories.contains("src/__tests__") {
        "src/__tests__"
    } else if directories.contains("__tests__") {
        "__tests__"
    } else {
        "tests"
    };
    let bootstrap = !files.iter().any(|path| is_application_file(path));
    match stack {
        Stack::Rust => {
            let module = slug.replace('-', "_");
            if bootstrap {
                let entry = entrypoint.unwrap_or("src/main.rs");
                let source = entry
                    .rsplit_once('/')
                    .map(|(parent, _)| parent)
                    .unwrap_or("src");
                return (
                    vec![
                        "Cargo.toml".into(),
                        entry.into(),
                        format!("{source}/{module}.rs"),
                        format!("tests/{module}.rs"),
                    ],
                    false,
                );
            }
            let source = rust_source_root(files, &task_words);
            let crate_root = source.strip_suffix("src").unwrap_or("");
            let mut paths = vec![
                format!("{source}/{module}.rs"),
                format!("{crate_root}tests/{module}.rs"),
            ];
            let library = format!("{source}/lib.rs");
            let binary = format!("{source}/main.rs");
            if files.contains(&library) {
                paths.push(library);
            } else if files.contains(&binary) {
                paths.push(binary);
            }
            (paths, false)
        }
        Stack::Next => {
            let app = if directories.contains("src/app") {
                "src/app"
            } else if directories.contains("app") {
                "app"
            } else if directories.contains("src/pages") {
                "src/pages"
            } else if directories.contains("pages") {
                "pages"
            } else {
                "app"
            };
            let ext = if files
                .iter()
                .any(|path| path.starts_with(app) && path.ends_with(".js"))
                && !typed
            {
                "js"
            } else {
                jsx
            };
            if bootstrap {
                let mut paths = if app.ends_with("pages") {
                    vec![
                        "package.json".into(),
                        format!("{app}/_app.{ext}"),
                        format!("{app}/index.{ext}"),
                        "styles/globals.css".into(),
                        format!("{test_dir}/{slug}.test.{ext}"),
                    ]
                } else {
                    vec![
                        "package.json".into(),
                        format!("{app}/layout.{ext}"),
                        format!("{app}/page.{ext}"),
                        format!("{app}/globals.css"),
                        format!("{test_dir}/{slug}.test.{ext}"),
                    ]
                };
                if typed {
                    paths.push("tsconfig.json".into());
                }
                return (paths, false);
            }
            let page = if app.ends_with("pages") {
                format!("{app}/{slug}.{ext}")
            } else {
                format!("{app}/{slug}/page.{ext}")
            };
            (vec![page, format!("{test_dir}/{slug}.test.{ext}")], false)
        }
        Stack::React => {
            if bootstrap {
                let mut paths = vec![
                    "package.json".into(),
                    "index.html".into(),
                    format!("src/main.{jsx}"),
                    format!("src/App.{jsx}"),
                    "src/styles.css".into(),
                    format!("{test_dir}/App.test.{jsx}"),
                ];
                if typed {
                    paths.push("tsconfig.json".into());
                }
                return (paths, false);
            }
            let component_dir = if directories.contains("src/components") {
                "src/components"
            } else if directories.contains("components") {
                "components"
            } else if directories.contains("src")
                || !files
                    .iter()
                    .any(|path| path.ends_with(".jsx") || path.ends_with(".tsx"))
            {
                "src/components"
            } else {
                "components"
            };
            (
                vec![
                    format!("{component_dir}/{slug}.{jsx}"),
                    format!("{test_dir}/{slug}.test.{jsx}"),
                ],
                false,
            )
        }
        Stack::Node => {
            let source_dir = if directories.contains("src/js") {
                "src/js"
            } else if directories.contains("lib") && !directories.contains("src") {
                "lib"
            } else {
                "src"
            };
            if bootstrap {
                let mut paths = vec![
                    "package.json".into(),
                    entrypoint
                        .map(str::to_owned)
                        .unwrap_or_else(|| format!("{source_dir}/index.{javascript}")),
                    format!("{source_dir}/{slug}.{javascript}"),
                    format!("{test_dir}/{slug}.test.{javascript}"),
                ];
                if typed {
                    paths.push("tsconfig.json".into());
                }
                return (paths, false);
            }
            (
                vec![
                    format!("{source_dir}/{slug}.{javascript}"),
                    format!("{test_dir}/{slug}.test.{javascript}"),
                ],
                false,
            )
        }
        Stack::Static => {
            let js_dir = if directories.contains("src/js") {
                "src/js"
            } else if directories.contains("js") {
                "js"
            } else {
                "src/js"
            };
            let styles = [
                "styles.css",
                "src/styles.css",
                "css/styles.css",
                "assets/styles.css",
            ]
            .into_iter()
            .find(|path| files.contains(*path))
            .unwrap_or("styles.css");
            (
                vec![
                    "index.html".into(),
                    styles.into(),
                    format!("{js_dir}/{slug}.js"),
                    format!("{test_dir}/{slug}.test.js"),
                ],
                false,
            )
        }
        Stack::Unknown => (vec!["docs/architecture.md".into()], true),
    }
}

fn bootstrap_entrypoint(
    stack: Stack,
    cargo: Option<&toml::Value>,
    package: Option<&serde_json::Value>,
    scope: &str,
) -> Option<String> {
    match stack {
        Stack::Rust => {
            if let Some(path) = cargo
                .and_then(|manifest| manifest.get("bin"))
                .and_then(toml::Value::as_array)
                .and_then(|bins| bins.first())
                .and_then(|bin| bin.get("path"))
                .and_then(toml::Value::as_str)
            {
                return Some(path.into());
            }
            if let Some(library) = cargo.and_then(|manifest| manifest.get("lib")) {
                return Some(
                    library
                        .get("path")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("src/lib.rs")
                        .into(),
                );
            }
            Some(
                if words(&scope.to_ascii_lowercase()).contains("library") {
                    "src/lib.rs"
                } else {
                    "src/main.rs"
                }
                .into(),
            )
        }
        Stack::Node => package
            .and_then(|manifest| manifest.get("main"))
            .and_then(serde_json::Value::as_str)
            .map(|path| path.strip_prefix("./").unwrap_or(path))
            .filter(|path| !path.split('/').any(forbidden_component))
            .map(str::to_owned),
        _ => None,
    }
}

fn is_test_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower
        .split('/')
        .any(|part| matches!(part, "test" | "tests" | "__tests__"))
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.ends_with("_test.go")
        || lower.ends_with("_test.py")
        || lower
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with("test_"))
}

/// A related source needs a test ref even when that test does not exist yet.
/// Prefer the concrete suite, then neighbouring naming conventions, then the
/// language's conventional new-test destination.
fn corresponding_test_path(
    source: &str,
    files: &BTreeSet<String>,
    directories: &BTreeSet<String>,
) -> Option<String> {
    if is_test_path(source) {
        return None;
    }
    let (parent, name) = source.rsplit_once('/').unwrap_or(("", source));
    let (stem, extension) = name.rsplit_once('.')?;
    let prefix = if parent.is_empty() {
        String::new()
    } else {
        format!("{parent}/")
    };
    let local_tests = format!("{prefix}__tests__");
    let extension = extension.to_ascii_lowercase();
    if extension == "rs" {
        let crate_prefix = source
            .find("/src/")
            .map(|position| format!("{}/", &source[..position]))
            .unwrap_or_default();
        return Some(format!("{crate_prefix}tests/{stem}.rs"));
    }
    if extension == "go" {
        return Some(format!("{prefix}{stem}_test.go"));
    }
    if extension == "py" {
        let existing = format!("{prefix}test_{stem}.py");
        return Some(if files.contains(&existing) {
            existing
        } else {
            format!("tests/test_{stem}.py")
        });
    }
    let test_extension = match extension.as_str() {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => extension.as_str(),
        "html" | "htm" | "css" | "scss" | "vue" | "svelte" => "js",
        _ => return None,
    };
    let test_directories = [
        parent.to_owned(),
        local_tests.clone(),
        "__tests__".into(),
        "src/__tests__".into(),
        "tests".into(),
    ];
    for directory in &test_directories {
        for marker in ["test", "spec"] {
            let file = format!("{stem}.{marker}.{test_extension}");
            let candidate = if directory.is_empty() {
                file
            } else {
                format!("{directory}/{file}")
            };
            if files.contains(&candidate) {
                return Some(candidate);
            }
        }
    }
    for directory in [parent, local_tests.as_str()] {
        for observed in files {
            let (observed_parent, observed_name) =
                observed.rsplit_once('/').unwrap_or(("", observed));
            if observed_parent != directory {
                continue;
            }
            for marker in ["test", "spec"] {
                let delimiter = format!(".{marker}.");
                if let Some((_, observed_extension)) = observed_name.rsplit_once(delimiter.as_str())
                    && matches!(
                        observed_extension,
                        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs"
                    )
                {
                    let name = format!("{stem}.{marker}.{observed_extension}");
                    return Some(if directory.is_empty() {
                        name
                    } else {
                        format!("{directory}/{name}")
                    });
                }
            }
        }
    }
    let directory = if directories.contains(&local_tests) {
        local_tests.as_str()
    } else if directories.contains("__tests__") {
        "__tests__"
    } else if directories.contains("src/__tests__") {
        "src/__tests__"
    } else {
        "tests"
    };
    Some(format!("{directory}/{stem}.test.{test_extension}"))
}

fn rust_source_root(files: &BTreeSet<String>, task_words: &[String]) -> String {
    let mut roots: HashMap<String, (usize, usize)> = HashMap::new();
    for path in files.iter().filter(|path| path.ends_with(".rs")) {
        let root = if path.starts_with("src/") {
            Some("src")
        } else {
            path.find("/src/").map(|position| &path[..position + 4])
        };
        if let Some(root) = root {
            let entry = roots.entry(root.to_owned()).or_default();
            entry.0 += 1;
            entry.1 = task_words
                .iter()
                .filter(|word| words(root).contains(word.as_str()))
                .count();
        }
    }
    let mut roots: Vec<_> = roots.into_iter().collect();
    roots.sort_by(|(a, (a_count, a_match)), (b, (b_count, b_match))| {
        b_match
            .cmp(a_match)
            .then_with(|| b_count.cmp(a_count))
            .then_with(|| a.cmp(b))
    });
    roots
        .into_iter()
        .next()
        .map(|(root, _)| root)
        .unwrap_or_else(|| "src".into())
}

fn planning_document(title: &str) -> Option<&'static str> {
    if title.contains("vision") || title.contains("mvp bound") || title.contains("product scope") {
        Some("docs/product-vision.md")
    } else if title.contains("document architecture")
        || title.contains("define architecture")
        || title.contains("architecture decision")
        || title.contains("initial architecture")
    {
        Some("docs/architecture.md")
    } else if title.contains("document requirements")
        || title.contains("define requirements")
        || title.contains("acceptance criteria")
    {
        Some("docs/requirements.md")
    } else if title.contains("test strategy") || title.contains("testing strategy") {
        Some("docs/testing.md")
    } else {
        None
    }
}

fn words(text: &str) -> BTreeSet<&str> {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect()
}

fn useful_words(title: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "a",
        "an",
        "the",
        "and",
        "or",
        "for",
        "to",
        "in",
        "of",
        "with",
        "add",
        "create",
        "implement",
        "build",
        "fix",
        "improve",
        "update",
        "support",
        "define",
        "ensure",
        "enable",
        "initial",
        "application",
        "app",
        "page",
        "component",
        "feature",
        "test",
        "tests",
    ];
    let mut seen = BTreeSet::new();
    title
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| word.len() > 1 && !STOP.contains(word) && seen.insert((*word).to_owned()))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
#[path = "destinations_tests.rs"]
mod tests;
