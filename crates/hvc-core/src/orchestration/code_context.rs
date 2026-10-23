//! Safe lexical context for on-demand Specification AND optional `PreSpec`.
use super::{
    CodeContextDiscoveryMode, CodeContextExtractor, CodeContextFacts, CodeContextFuture,
    CodeContextRequest, CodeContextService, ContentSha256, ContextCoverage, ContractError,
    CoverageState, DocumentationAvailability, ExtractorRevision, ObservedReference,
    ReferenceRelation, ReferenceResolution, SchemaV1, SourceSnapshot, TreeSitterExtractor,
    task_refine,
};
use crate::repository_context::RepositoryContext;

#[derive(Default)]
pub struct LexicalCodeContextService;

#[derive(Default)]
pub struct RoutedCodeContextService;

impl CodeContextService for LexicalCodeContextService {
    fn collect<'a>(
        &'a self,
        request: &'a CodeContextRequest,
        cancellation: &'a tokio_util::sync::CancellationToken,
    ) -> CodeContextFuture<'a> {
        let request = request.clone();
        let cancellation = cancellation.clone();
        Box::pin(async move {
            // Await the bounded read even on cancellation; never detach a filesystem worker.
            tokio::task::spawn_blocking(move || Self::collect_blocking(&request, &cancellation))
                .await
                .map_err(|_| ContractError::Invalid("worker de contexto interrompido"))?
        })
    }
}

impl CodeContextService for RoutedCodeContextService {
    fn collect<'a>(
        &'a self,
        request: &'a CodeContextRequest,
        cancellation: &'a tokio_util::sync::CancellationToken,
    ) -> CodeContextFuture<'a> {
        let request = request.clone();
        let cancellation = cancellation.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || Self::collect_blocking(&request, &cancellation))
                .await
                .map_err(|_| ContractError::Invalid("worker de contexto interrompido"))?
        })
    }
}

impl RoutedCodeContextService {
    pub(crate) fn collect_blocking(
        request: &CodeContextRequest,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<CodeContextFacts, ContractError> {
        Self::collect_revalidated(request, cancellation, None).map(|(facts, _)| facts)
    }

    pub(crate) fn collect_revalidated(
        request: &CodeContextRequest,
        cancellation: &tokio_util::sync::CancellationToken,
        previous: Option<&CodeContextFacts>,
    ) -> Result<(CodeContextFacts, bool), ContractError> {
        LexicalCodeContextService::collect_revalidated_with(request, cancellation, previous, true)
    }
}

impl LexicalCodeContextService {
    pub(crate) fn collect_blocking(
        request: &CodeContextRequest,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<CodeContextFacts, ContractError> {
        Self::collect_revalidated(request, cancellation, None).map(|(facts, _)| facts)
    }

    pub(crate) fn collect_revalidated(
        request: &CodeContextRequest,
        cancellation: &tokio_util::sync::CancellationToken,
        previous: Option<&CodeContextFacts>,
    ) -> Result<(CodeContextFacts, bool), ContractError> {
        Self::collect_revalidated_with(request, cancellation, previous, false)
    }

    fn collect_revalidated_with(
        request: &CodeContextRequest,
        cancellation: &tokio_util::sync::CancellationToken,
        previous: Option<&CodeContextFacts>,
        structural: bool,
    ) -> Result<(CodeContextFacts, bool), ContractError> {
        request.budget.validate()?;
        let started = std::time::Instant::now();
        if cancellation.is_cancelled() {
            return Err(ContractError::Cancelled);
        }
        let mut facts = CodeContextFacts {
            schema_version: SchemaV1,
            project_id: request.project_id.clone(),
            extractor: if structural {
                TreeSitterExtractor::extractor_revision()
            } else {
                ExtractorRevision {
                    name: "repository-lexical".into(),
                    version: "1".into(),
                    policy_version: "rsp12-v2".into(),
                    resolution_context: None,
                }
            },
            scope: format!(
                "task:{}",
                ContentSha256::of(
                    &serde_json::to_vec(&(&request.query, &request.hinted_paths, request.budget))
                        .expect("scope fingerprint")
                )
                .as_str()
            ),
            candidate_set_sha256: None,
            sources: vec![],
            snippets: vec![],
            symbols: vec![],
            references: vec![],
            coverage: ContextCoverage {
                state: CoverageState::Unavailable,
                documentation: DocumentationAvailability::Unknown,
                enumerated_files: 0,
                read_bytes: 0,
                truncated: false,
                diagnostics: vec![],
            },
        };
        let repository = match RepositoryContext::new(&request.workspace_root) {
            Ok(repository) => repository,
            Err(_) => {
                facts
                    .coverage
                    .diagnostics
                    .push("Workspace unavailable for safe collection.".into());
                return Ok((facts, false));
            }
        };
        let (snapshots, coverage) = match request.discovery_mode {
            CodeContextDiscoveryMode::RepositoryScan => repository.capture_code_sources(
                &request.project_id,
                request.budget,
                cancellation,
            )?,
            CodeContextDiscoveryMode::ExactPaths => repository.capture_exact_code_sources(
                &request.project_id,
                &request.hinted_paths,
                request.budget,
                cancellation,
            )?,
        };
        facts.coverage = coverage;
        let fingerprint: Vec<_> = snapshots.iter().map(SourceSnapshot::revision).collect();
        facts.candidate_set_sha256 = Some(ContentSha256::of(
            &serde_json::to_vec(&fingerprint).expect("source revisions"),
        ));
        if let Some(previous) = previous
            && previous.project_id == facts.project_id
            && previous.extractor == facts.extractor
            && previous.scope == facts.scope
            && previous.candidate_set_sha256 == facts.candidate_set_sha256
            && facts.coverage.state == CoverageState::Complete
            && previous.coverage.state == CoverageState::Complete
            && previous.validate_against(&snapshots).is_ok()
        {
            // Reuse extraction/ranking, never the old discovery or filesystem observations.
            return Ok((previous.clone(), true));
        }
        let terms = task_refine::search_terms(&request.query);
        let mut matches = Vec::new();
        for source in &snapshots {
            if cancellation.is_cancelled() {
                return Err(ContractError::Cancelled);
            }
            if started.elapsed().as_millis() >= u128::from(request.budget.max_duration_ms) {
                facts.coverage.truncated = true;
                break;
            }
            let path = source.revision().relative_path.as_str();
            let explicit = request
                .hinted_paths
                .contains(&source.revision().relative_path)
                || request
                    .query
                    .split(|c: char| c.is_whitespace() || "`\"'(),;".contains(c))
                    .any(|p| p.replace('\\', "/") == path);
            let words: std::collections::HashSet<_> =
                task_refine::tokens(source.content()).into_iter().collect();
            let symbol = terms.iter().filter(|term| words.contains(*term)).count();
            // An explicit auth.rs hint must not match every Rust file through the "rs"
            // extension and turn an otherwise complete package into artificial truncation.
            // Keep directory/stem terms and exact-path priority; source symbols remain eligible.
            let path_terms = task_refine::tokens(
                &std::path::Path::new(path)
                    .with_extension("")
                    .to_string_lossy(),
            );
            let path_match = terms
                .iter()
                .filter(|term| path_terms.contains(term))
                .count();
            if explicit || symbol + path_match > 0 {
                matches.push((
                    !explicit,
                    std::cmp::Reverse(symbol + path_match),
                    path,
                    source,
                ));
            }
        }
        matches.sort_by(|a, b| (&a.0, &a.1, a.2).cmp(&(&b.0, &b.1, b.2)));
        let mut remaining = request.budget.max_snippet_bytes as usize;
        let match_count = matches.len();
        let mut selected_snapshots = Vec::new();
        for (_, _, _, source) in matches
            .into_iter()
            .take(request.budget.max_package_files as usize)
        {
            let lines: Vec<_> = source.content().split_inclusive('\n').collect();
            if lines.is_empty() {
                continue;
            }
            let center = lines
                .iter()
                .position(|line| task_refine::tokens(line).iter().any(|w| terms.contains(w)))
                .unwrap_or(0);
            let start = center.saturating_sub(6);
            let mut end = start;
            let mut size = 0;
            while end < lines.len().min(center + 18)
                && size + lines[end].len() <= remaining.min(4096)
            {
                size += lines[end].len();
                end += 1;
            }
            if end == start {
                facts.coverage.truncated = true;
                continue;
            }
            let snippet = source.snippet(start as u32 + 1, end as u32)?;
            if !structural {
                for (index, line) in lines.iter().enumerate().take(end).skip(start) {
                    let observed = line.trim();
                    if ["import ", "from ", "use ", "#include ", "mod "]
                        .iter()
                        .any(|prefix| observed.starts_with(prefix))
                        || observed.contains("require(")
                    {
                        facts.references.push(ObservedReference {
                            source: source.revision().clone(),
                            line: index as u32 + 1,
                            observed_text: observed.into(),
                            relation: ReferenceRelation::Lexical,
                            reference_key: None,
                            descriptor: None,
                            source_symbol_key: None,
                            target_symbol_key: None,
                            target_revision: None,
                            resolution_evidence: None,
                            resolution: ReferenceResolution::Unresolved {
                                reason: "Lexical reference observed; no language resolver.".into(),
                            },
                        });
                    }
                }
            }
            remaining -= size;
            facts.sources.push(source.revision().clone());
            facts.snippets.push(snippet);
            selected_snapshots.push((*source).clone());
        }
        if match_count > facts.sources.len() {
            facts.coverage.truncated = true;
        }
        if facts.sources.is_empty() {
            facts.coverage.diagnostics.push(
                "no_matches: no evidence selected in this scope; dependencies unknown.".into(),
            );
        }
        if facts.coverage.truncated {
            facts.coverage.state = CoverageState::Partial;
        }
        if structural && !selected_snapshots.is_empty() {
            let extracted =
                TreeSitterExtractor.extract(&selected_snapshots, request.budget, cancellation)?;
            facts.symbols = extracted.symbols;
            facts.references = extracted.references;
            if extracted.coverage.state == CoverageState::Partial {
                facts.coverage.state = CoverageState::Partial;
            }
            if extracted.coverage.truncated {
                facts.coverage.truncated = true;
            }
            facts
                .coverage
                .diagnostics
                .extend(extracted.coverage.diagnostics);
        }
        facts.validate_against(&snapshots)?;
        Ok((facts, false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::CollectionBudget;
    use crate::orchestration::RelativeSourcePath;
    use crate::orchestration::StructuredSymbolKind;
    use std::fs;
    use tokio_util::sync::CancellationToken;

    fn request(root: &std::path::Path) -> CodeContextRequest {
        CodeContextRequest {
            project_id: "project-a".into(),
            workspace_root: root.into(),
            query: "adicione validate_token em auth.rs".into(),
            hinted_paths: vec![],
            discovery_mode: CodeContextDiscoveryMode::RepositoryScan,
            budget: CollectionBudget::default(),
        }
    }

    #[test]
    fn explicit_file_extension_does_not_select_unrelated_files_or_force_partial_coverage() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("auth.rs"), "fn validate_token() {}\n").unwrap();
        for index in 0..20 {
            fs::write(
                root.path().join(format!("unrelated_{index}.rs")),
                "fn unrelated() {}\n",
            )
            .unwrap();
        }
        let req = request(root.path());
        let token = CancellationToken::new();
        let facts = LexicalCodeContextService::collect_blocking(&req, &token).unwrap();
        assert_eq!(facts.coverage.enumerated_files, 21);
        assert_eq!(facts.sources.len(), 1);
        assert_eq!(facts.coverage.state, CoverageState::Complete);
        assert!(
            LexicalCodeContextService::collect_revalidated(&req, &token, Some(&facts))
                .unwrap()
                .1
        );
    }

    #[test]
    fn exact_path_mode_reads_only_vault_targets_without_repository_discovery() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("target.rs"), "pub fn selected() {}\n").unwrap();
        fs::write(root.path().join("unrelated.rs"), "pub fn selected() {}\n").unwrap();
        let mut request = request(root.path());
        request.query = "selected".to_string();
        request.discovery_mode = CodeContextDiscoveryMode::ExactPaths;
        request.hinted_paths = vec![RelativeSourcePath::new("target.rs").unwrap()];

        let facts = RoutedCodeContextService::collect_blocking(&request, &CancellationToken::new())
            .unwrap();

        assert_eq!(facts.coverage.enumerated_files, 1);
        assert_eq!(facts.sources.len(), 1);
        assert_eq!(facts.sources[0].relative_path.as_str(), "target.rs");
        assert!(
            facts
                .coverage
                .diagnostics
                .iter()
                .any(|message| message.starts_with("exact_paths:"))
        );
    }

    #[test]
    fn routed_service_enriches_supported_sources_without_changing_the_context_contract() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("auth.rs"),
            "use crate::token;\npub fn validate_token() { verify(); }\n",
        )
        .unwrap();
        let request = request(root.path());
        let snapshots = RepositoryContext::new(root.path())
            .unwrap()
            .capture_code_sources(
                &request.project_id,
                request.budget,
                &CancellationToken::new(),
            )
            .unwrap()
            .0;
        let facts = RoutedCodeContextService::collect_blocking(&request, &CancellationToken::new())
            .unwrap();
        assert_eq!(facts.extractor.name, "tree-sitter-router");
        assert!(facts.symbols.iter().any(|symbol| {
            symbol.kind == StructuredSymbolKind::Function && symbol.name == "validate_token"
        }));
        assert!(
            facts
                .references
                .iter()
                .any(|reference| reference.relation == ReferenceRelation::Call)
        );
        facts.validate_against(&snapshots).unwrap();
    }

    #[test]
    fn routed_service_keeps_unsupported_sources_on_the_lexical_fallback() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join("auth.py"),
            "from token import validate_token\nvalidate_token()\n",
        )
        .unwrap();
        let facts = RoutedCodeContextService::collect_blocking(
            &request(root.path()),
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(facts.symbols.is_empty());
        assert!(
            facts
                .references
                .iter()
                .any(|reference| reference.relation == ReferenceRelation::Lexical)
        );
        assert!(
            facts
                .coverage
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.starts_with("lexical_fallback:"))
        );
    }

    #[tokio::test]
    async fn first_request_without_vault_uses_exact_bytes_and_unresolved_references() {
        let root = tempfile::tempdir().unwrap();
        let text = "use crate::token;\r\npub fn validate_token() { /* ação */ }\r\n";
        fs::write(root.path().join("auth.rs"), text).unwrap();
        fs::write(
            root.path().join("README.md"),
            "validate_token NARRATIVE_NOT_CODE",
        )
        .unwrap();
        let facts = LexicalCodeContextService
            .collect(&request(root.path()), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(facts.sources.len(), 1);
        assert_eq!(facts.snippets[0].content, text);
        assert_eq!(
            facts.sources[0].content_sha256,
            ContentSha256::of(text.as_bytes())
        );
        assert_eq!(facts.references[0].line, 1);
        assert!(matches!(
            facts.references[0].resolution,
            ReferenceResolution::Unresolved { .. }
        ));
        assert_eq!(
            facts.coverage.documentation,
            DocumentationAvailability::Unknown
        );
        assert_eq!(facts.coverage.state, CoverageState::Complete);
    }

    #[test]
    fn ignores_secrets_examples_generated_binary_and_narrative_even_when_explicitly_hinted() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(".gitignore"), "ignored.rs\n").unwrap();
        for path in [
            ".env.example",
            "credentials.example.json",
            "private-key.rs",
            "ignored.rs",
            "README.md",
            "auth.min.js",
        ] {
            fs::write(root.path().join(path), "validate_token SECRET").unwrap();
        }
        fs::create_dir(root.path().join("target")).unwrap();
        fs::write(
            root.path().join("target/auth.rs"),
            "validate_token GENERATED",
        )
        .unwrap();
        fs::write(root.path().join("binary.rs"), b"validate_token\0binary").unwrap();
        fs::write(root.path().join("auth.rs"), "fn validate_token() {}\n").unwrap();
        let mut req = request(root.path());
        req.hinted_paths = vec![RelativeSourcePath::new(".env.example").unwrap()];
        let facts =
            LexicalCodeContextService::collect_blocking(&req, &CancellationToken::new()).unwrap();
        assert_eq!(facts.sources.len(), 1);
        assert_eq!(facts.sources[0].relative_path.as_str(), "auth.rs");
        assert!(!serde_json::to_string(&facts).unwrap().contains("SECRET"));
        assert_eq!(
            facts.coverage.state,
            CoverageState::Partial,
            "blocked binary is not proof of complete knowledge"
        );
    }

    #[test]
    fn bounded_budget_marks_partial_and_stable_ranking_prioritizes_explicit_paths() {
        let root = tempfile::tempdir().unwrap();
        for path in ["a.rs", "auth.rs", "z.rs"] {
            fs::write(root.path().join(path), "fn validate_token() {}\n").unwrap();
        }
        let mut req = request(root.path());
        req.budget.max_package_files = 1;
        let facts =
            LexicalCodeContextService::collect_blocking(&req, &CancellationToken::new()).unwrap();
        assert_eq!(facts.sources[0].relative_path.as_str(), "auth.rs");
        assert!(facts.coverage.truncated);
        assert!(facts.coverage.read_bytes <= req.budget.max_read_bytes);
        req.budget.max_files = 1;
        let facts =
            LexicalCodeContextService::collect_blocking(&req, &CancellationToken::new()).unwrap();
        assert!(facts.coverage.truncated);
        assert_eq!(facts.coverage.enumerated_files, 1);
        req.budget.max_read_bytes = 24;
        req.budget.max_file_bytes = 24;
        let facts =
            LexicalCodeContextService::collect_blocking(&req, &CancellationToken::new()).unwrap();
        assert!(facts.coverage.read_bytes <= 24);
    }

    #[test]
    fn missing_empty_and_cancelled_scopes_do_not_invent_dependencies() {
        let root = tempfile::tempdir().unwrap();
        let empty = LexicalCodeContextService::collect_blocking(
            &request(root.path()),
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(empty.sources.is_empty());
        assert!(
            empty
                .coverage
                .diagnostics
                .iter()
                .any(|d| d.contains("no_matches"))
        );
        let missing = LexicalCodeContextService::collect_blocking(
            &request(&root.path().join("missing")),
            &CancellationToken::new(),
        )
        .unwrap();
        assert_eq!(missing.coverage.state, CoverageState::Unavailable);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            LexicalCodeContextService::collect_blocking(&request(root.path()), &cancelled),
            Err(ContractError::Cancelled)
        );
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_never_followed_by_code_collection() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(
            outside.path().join("auth.rs"),
            "fn validate_token() { /* OUTSIDE */ }",
        )
        .unwrap();
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.path().join("linked"))
            .arg(outside.path())
            .output()
            .unwrap();
        assert!(status.status.success(), "junction fixture creation");
        let facts = LexicalCodeContextService::collect_blocking(
            &request(root.path()),
            &CancellationToken::new(),
        )
        .unwrap();
        assert!(facts.sources.is_empty());
    }
}
