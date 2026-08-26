use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::fs;
use std::path::{Component, Path, PathBuf};

use ignore::{DirEntry, WalkBuilder};
use regex::Regex;
use serde::{Deserialize, Serialize};

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

impl RepositoryContext {
    pub fn new(root: &Path) -> Result<Self, RepositoryError> {
        let canonical = root
            .canonicalize()
            .map_err(|error| RepositoryError(format!("Workspace inválido: {error}")))?;
        if !canonical.is_dir() {
            return Err(RepositoryError(
                "O workspace precisa ser um diretório.".into(),
            ));
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
        if args.query.is_empty() || args.query.chars().count() > MAX_QUERY_CHARS {
            return Err(RepositoryError(
                "A busca deve conter entre 1 e 256 caracteres.".into(),
            ));
        }
        let scope = self.resolve_relative(args.path.as_deref().unwrap_or(""), true)?;
        let max_results = args.max_results.unwrap_or(20).clamp(1, MAX_SEARCH_RESULTS);
        let mode = args.mode.as_deref().unwrap_or("literal");
        let expression = match mode {
            "literal" => None,
            "regex" => Some(
                Regex::new(&args.query)
                    .map_err(|error| RepositoryError(format!("Regex inválida: {error}")))?,
            ),
            _ => {
                return Err(RepositoryError(
                    "Modo de busca deve ser 'literal' ou 'regex'.".into(),
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
                    path: self.relative_display(entry.path())?,
                    line: index + 1,
                    text: truncate_chars(line.trim(), 500),
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
        let path = self.resolve_relative(&args.path, false)?;
        let content = self.read_safe_text(&path)?;
        let start = args.start_line.unwrap_or(1).max(1);
        let requested_end = args.end_line.unwrap_or(start + MAX_READ_LINES - 1);
        if requested_end < start {
            return Err(RepositoryError(
                "endLine deve ser maior ou igual a startLine.".into(),
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
            path: self.relative_display(&path)?,
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
                "A ferramenta aceita somente paths relativos sem '..'.".into(),
            ));
        }
        let candidate = self.root.join(requested);
        self.reject_symlink_components(&candidate)?;
        let canonical = candidate
            .canonicalize()
            .map_err(|_| RepositoryError("Path não encontrado no repositório.".into()))?;
        if !canonical.starts_with(&self.root) {
            return Err(RepositoryError(
                "Acesso fora da raiz do repositório foi bloqueado.".into(),
            ));
        }
        if canonical != self.root && !self.path_is_visible(&canonical) {
            return Err(RepositoryError(
                "Path ignorado ou excluído pelas regras de segurança.".into(),
            ));
        }
        if directory && !canonical.is_dir() {
            return Err(RepositoryError(
                "O path informado não é um diretório.".into(),
            ));
        }
        if !directory && !canonical.is_file() {
            return Err(RepositoryError("O path informado não é um arquivo.".into()));
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
            .map_err(|_| RepositoryError("Path fora do repositório.".into()))?;
        let mut current = self.root.clone();
        for component in relative.components() {
            current.push(component.as_os_str());
            if fs::symlink_metadata(&current)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
            {
                return Err(RepositoryError("Links simbólicos não são seguidos.".into()));
            }
        }
        Ok(())
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
            .max_depth(Some(depth));
        builder.filter_entry(allowed_entry);
        builder.build()
    }

    fn read_safe_text(&self, path: &Path) -> Result<String, RepositoryError> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if is_sensitive_name(&name) || !allowed_text_name(&name) {
            return Err(RepositoryError(
                "Arquivo sensível, binário ou não suportado foi bloqueado.".into(),
            ));
        }
        let metadata = fs::metadata(path)
            .map_err(|error| RepositoryError(format!("Falha ao ler arquivo: {error}")))?;
        if metadata.len() > MAX_FILE_BYTES {
            return Err(RepositoryError(
                "Arquivos acima de 1 MiB não podem ser consultados.".into(),
            ));
        }
        let bytes = fs::read(path)
            .map_err(|error| RepositoryError(format!("Falha ao ler arquivo: {error}")))?;
        String::from_utf8(bytes)
            .map_err(|_| RepositoryError("Arquivo binário ou não UTF-8 foi bloqueado.".into()))
    }

    fn relative_display(&self, path: &Path) -> Result<String, RepositoryError> {
        path.strip_prefix(&self.root)
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .map_err(|_| RepositoryError("Path fora do repositório.".into()))
    }
}

fn allowed_entry(entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
    if entry.file_type().is_some_and(|kind| kind.is_dir()) {
        return !excluded_directories().contains(name.as_str())
            && !entry.file_type().is_some_and(|kind| kind.is_symlink());
    }
    !is_sensitive_name(&name)
        && allowed_text_name(&name)
        && !entry.file_type().is_some_and(|kind| kind.is_symlink())
        && fs::metadata(entry.path()).is_ok_and(|metadata| metadata.len() <= MAX_FILE_BYTES)
        && fs::read(entry.path()).is_ok_and(|bytes| String::from_utf8(bytes).is_ok())
}

fn excluded_directories() -> HashSet<&'static str> {
    HashSet::from([
        ".git",
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
    if name.starts_with(".env") {
        return !["example", "template", "sample"]
            .iter()
            .any(|tag| name.contains(tag));
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
        "sql", "svg", "swift", "toml", "ts", "tsx", "txt", "xml", "yaml", "yml",
    ];
    let special = matches!(
        name,
        "dockerfile" | "makefile" | "license" | ".gitignore" | ".gitattributes" | ".editorconfig"
    ) || name.starts_with("readme")
        || (name.starts_with(".env") && !is_sensitive_name(name));
    special
        || Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| EXTENSIONS.contains(&extension))
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
        assert!(paths.contains(&".env.example"));
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
}
