//! RS2: bounded, revalidated one-hop expansion over the complete RS1 graph.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use rusqlite::types::Value as SqlValue;
use rusqlite::{TransactionBehavior, params_from_iter};

use super::*;
use crate::orchestration::{
    CodeContextFacts, CodeContextRequest, ContentSha256, CoverageState, ReferenceResolution,
    ResolutionEvidence, SourceSnapshot, TreeSitterExtractor,
};
use crate::repository_context::RepositoryContext;

const MAX_GRAPH_EDGES: usize = 256;
const MAX_GRAPH_ADDITIONS: usize = 4;

#[derive(Debug, Clone)]
struct ContextEdge {
    source_path: String,
    destination_path: String,
    source_revision: String,
    destination_revision: String,
    source_symbol_key: Option<String>,
    destination_symbol_key: Option<String>,
    reference_key: String,
    relation: String,
    method: String,
}

impl ContextEdge {
    fn neighbor<'a>(&'a self, seeds: &HashSet<String>) -> Option<(&'a str, Option<&'a str>)> {
        if seeds.contains(self.source_path.as_str())
            && !seeds.contains(self.destination_path.as_str())
        {
            Some((
                self.destination_path.as_str(),
                self.destination_symbol_key.as_deref(),
            ))
        } else if seeds.contains(self.destination_path.as_str())
            && !seeds.contains(self.source_path.as_str())
        {
            Some((self.source_path.as_str(), self.source_symbol_key.as_deref()))
        } else {
            None
        }
    }

    fn priority_rank(&self, seeds: &HashSet<String>) -> u8 {
        let exact_symbol = self
            .neighbor(seeds)
            .and_then(|(_, symbol)| symbol)
            .is_some();
        if exact_symbol {
            0
        } else {
            match self.relation.as_str() {
                "call" => 1,
                "type_use" | "inherits" | "implements" => 2,
                _ => 3,
            }
        }
    }
}

impl PersistenceService {
    pub(super) fn expand_context_through_brownfield_graph(
        &self,
        request: &CodeContextRequest,
        mut facts: CodeContextFacts,
        cancellation: &CancellationToken,
        remaining_duration: Duration,
    ) -> Result<(CodeContextFacts, bool), PersistenceServiceError> {
        if cancellation.is_cancelled() || facts.sources.is_empty() {
            return Ok((facts, false));
        }
        let started = Instant::now();
        let db = self.connection()?;
        let consent =
            Self::read_runtime_setting(&db, &brownfield_consent_key(&request.project_id))?;
        if consent.as_deref() != Some("true") {
            if consent.is_some() {
                mark_graph_partial(&mut facts, "graph_expansion_unavailable: consent_disabled");
            }
            return Ok((facts, false));
        }
        let state = db
            .query_row(
                "SELECT resolver_version,resolution_context_sha256,state FROM brownfield_resolution_state WHERE project_id=?1",
                [&request.project_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((resolver_version, expected_context, state)) = state else {
            mark_graph_partial(&mut facts, "graph_expansion_unavailable: graph_missing");
            return Ok((facts, false));
        };
        if state != "complete" {
            mark_graph_partial(
                &mut facts,
                &format!("graph_expansion_unavailable: graph_{state}"),
            );
            return Ok((facts, false));
        }
        let current_context =
            Self::resolution_context(&db, Path::new(&request.workspace_root), &request.project_id)?;
        if current_context.sha256 != expected_context {
            drop(db);
            self.ensure_brownfield_resolution_job(&request.project_id)?;
            mark_graph_partial(
                &mut facts,
                "graph_expansion_unavailable: manifest_context_stale",
            );
            return Ok((facts, false));
        }
        let seed_paths = facts
            .sources
            .iter()
            .map(|source| source.relative_path.as_str().to_string())
            .collect::<HashSet<_>>();
        let mut edges =
            Self::context_edges(&db, &request.project_id, &expected_context, &seed_paths)?;
        drop(db);
        edges.sort_by(|left, right| {
            left.priority_rank(&seed_paths)
                .cmp(&right.priority_rank(&seed_paths))
                .then_with(|| left.source_path.cmp(&right.source_path))
                .then_with(|| left.reference_key.cmp(&right.reference_key))
        });
        let edge_limit_reached = edges.len() == MAX_GRAPH_EDGES;

        let repository = RepositoryContext::new(&request.workspace_root)
            .map_err(|error| coordination::invalid(error.to_string()))?;
        let mut snapshots = HashMap::<String, SourceSnapshot>::new();
        let mut stale = Vec::new();
        let original = facts.clone();
        for source in &facts.sources {
            match repository.capture_code_source(&request.project_id, &source.relative_path) {
                Ok(snapshot) if snapshot.revision() == source => {
                    facts.coverage.read_bytes = facts
                        .coverage
                        .read_bytes
                        .saturating_add(snapshot.content().len() as u64);
                    snapshots.insert(source.relative_path.as_str().to_string(), snapshot);
                }
                Ok(snapshot) => stale.push((
                    source.relative_path.as_str().to_string(),
                    Some(snapshot.revision().content_sha256.as_str().to_string()),
                )),
                Err(_) => stale.push((source.relative_path.as_str().to_string(), None)),
            }
        }
        if !stale.is_empty() {
            self.schedule_graph_refreshes(&request.project_id, &stale)?;
            let mut fallback = original;
            let stale_paths = stale
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<HashSet<_>>();
            fallback
                .sources
                .retain(|source| !stale_paths.contains(source.relative_path.as_str()));
            fallback
                .snippets
                .retain(|snippet| !stale_paths.contains(snippet.source.relative_path.as_str()));
            fallback
                .symbols
                .retain(|symbol| !stale_paths.contains(symbol.source.relative_path.as_str()));
            fallback
                .references
                .retain(|reference| !stale_paths.contains(reference.source.relative_path.as_str()));
            mark_graph_partial(
                &mut fallback,
                "graph_expansion_unavailable: seed_revision_stale",
            );
            return Ok((fallback, false));
        }

        edges.retain(|edge| {
            let source_valid = !seed_paths.contains(&edge.source_path)
                || snapshots.get(&edge.source_path).is_some_and(|snapshot| {
                    snapshot.revision().content_sha256.as_str() == edge.source_revision
                });
            let destination_valid = !seed_paths.contains(&edge.destination_path)
                || snapshots
                    .get(&edge.destination_path)
                    .is_some_and(|snapshot| {
                        snapshot.revision().content_sha256.as_str() == edge.destination_revision
                    });
            if !source_valid {
                stale.push((
                    edge.source_path.clone(),
                    snapshots
                        .get(&edge.source_path)
                        .map(|snapshot| snapshot.revision().content_sha256.as_str().to_string()),
                ));
            }
            if !destination_valid {
                stale.push((
                    edge.destination_path.clone(),
                    snapshots
                        .get(&edge.destination_path)
                        .map(|snapshot| snapshot.revision().content_sha256.as_str().to_string()),
                ));
            }
            source_valid && destination_valid
        });
        stale.sort();
        stale.dedup();

        let source_limit = request.budget.max_package_files as usize;
        let mut remaining_snippet_bytes = (request.budget.max_snippet_bytes as usize)
            .saturating_sub(
                facts
                    .snippets
                    .iter()
                    .map(|snippet| snippet.content.len())
                    .sum(),
            );
        let mut candidate_paths = Vec::new();
        let semantic_candidate_available = edges.iter().any(|edge| {
            edge.neighbor(&seed_paths).is_some() && edge.priority_rank(&seed_paths) <= 2
        });
        let mut pruned_module_edges = 0usize;
        for edge in &edges {
            if facts.sources.len() >= source_limit
                || candidate_paths.len() >= MAX_GRAPH_ADDITIONS
                || started.elapsed() >= remaining_duration
                || cancellation.is_cancelled()
            {
                break;
            }
            let Some((path, target_symbol)) = edge.neighbor(&seed_paths) else {
                continue;
            };
            if semantic_candidate_available && edge.priority_rank(&seed_paths) > 2 {
                pruned_module_edges = pruned_module_edges.saturating_add(1);
                continue;
            }
            if snapshots.contains_key(path) || candidate_paths.iter().any(|value| value == path) {
                continue;
            }
            let Ok(relative) = crate::orchestration::RelativeSourcePath::new(path) else {
                stale.push((path.to_string(), None));
                continue;
            };
            let snapshot = match repository.capture_code_source(&request.project_id, &relative) {
                Ok(snapshot) => snapshot,
                Err(_) => {
                    stale.push((path.to_string(), None));
                    continue;
                }
            };
            let expected_revision = if path == edge.source_path {
                &edge.source_revision
            } else {
                &edge.destination_revision
            };
            if snapshot.revision().content_sha256.as_str() != expected_revision {
                stale.push((
                    path.to_string(),
                    Some(snapshot.revision().content_sha256.as_str().to_string()),
                ));
                continue;
            }
            let next_read_bytes = facts
                .coverage
                .read_bytes
                .saturating_add(snapshot.content().len() as u64);
            if next_read_bytes > request.budget.max_read_bytes {
                break;
            }
            let extracted = TreeSitterExtractor
                .extract_source(&snapshot, cancellation)
                .map_err(|error| coordination::invalid(error.to_string()))?;
            let target = target_symbol.and_then(|key| {
                extracted
                    .symbols
                    .iter()
                    .find(|symbol| symbol.symbol_key.as_deref() == Some(key))
            });
            let Some(snippet) = graph_snippet(&snapshot, target, remaining_snippet_bytes)? else {
                break;
            };
            remaining_snippet_bytes = remaining_snippet_bytes.saturating_sub(snippet.content.len());
            facts.coverage.read_bytes = next_read_bytes;
            facts.sources.push(snapshot.revision().clone());
            facts.snippets.push(snippet);
            facts.symbols.extend(extracted.symbols);
            facts.references.extend(extracted.references);
            candidate_paths.push(path.to_string());
            snapshots.insert(path.to_string(), snapshot);
        }
        let stale_edges_detected = !stale.is_empty();
        if stale_edges_detected {
            self.schedule_graph_refreshes(&request.project_id, &stale)?;
        }

        let final_context = Self::resolution_context(
            &self.connection()?,
            Path::new(&request.workspace_root),
            &request.project_id,
        )?;
        if final_context.sha256 != expected_context {
            self.ensure_brownfield_resolution_job(&request.project_id)?;
            let mut fallback = original;
            mark_graph_partial(
                &mut fallback,
                "graph_expansion_unavailable: context_advanced",
            );
            return Ok((fallback, false));
        }
        let selected = facts
            .sources
            .iter()
            .map(|source| source.relative_path.as_str())
            .collect::<HashSet<_>>();
        let valid_edges = edges
            .iter()
            .filter(|edge| {
                selected.contains(edge.source_path.as_str())
                    && selected.contains(edge.destination_path.as_str())
                    && snapshots.get(&edge.source_path).is_some_and(|snapshot| {
                        snapshot.revision().content_sha256.as_str() == edge.source_revision
                    })
                    && snapshots
                        .get(&edge.destination_path)
                        .is_some_and(|snapshot| {
                            snapshot.revision().content_sha256.as_str() == edge.destination_revision
                        })
            })
            .map(|edge| (edge.reference_key.as_str(), edge))
            .collect::<HashMap<_, _>>();
        for reference in &mut facts.references {
            let Some(key) = reference.reference_key.as_deref() else {
                continue;
            };
            let Some(edge) = valid_edges.get(key) else {
                continue;
            };
            let Ok(target) = crate::orchestration::RelativeSourcePath::new(&edge.destination_path)
            else {
                continue;
            };
            let Ok(target_revision) = ContentSha256::try_from(edge.destination_revision.clone())
            else {
                continue;
            };
            reference.resolution = ReferenceResolution::Resolved { target };
            reference.target_symbol_key = edge.destination_symbol_key.clone();
            reference.target_revision = Some(target_revision);
            reference.resolution_evidence = Some(ResolutionEvidence {
                method: edge.method.clone(),
                resolver_version: resolver_version.clone(),
                context_sha256: ContentSha256::try_from(expected_context.clone())
                    .expect("persisted resolution context"),
                candidate_count: 1,
            });
        }
        facts.extractor.resolution_context = Some(expected_context);
        let exhausted = edge_limit_reached
            || stale_edges_detected
            || candidate_paths.len() >= MAX_GRAPH_ADDITIONS
            || facts.sources.len() >= source_limit
                && edges
                    .iter()
                    .any(|edge| edge.neighbor(&seed_paths).is_some())
            || remaining_snippet_bytes == 0
            || started.elapsed() >= remaining_duration;
        if exhausted {
            mark_graph_partial(&mut facts, "graph_expansion_partial: budget_or_edge_limit");
        } else {
            facts.coverage.diagnostics.push(format!(
                "graph_expansion_complete: one_hop_sources={}; candidate_edges={}; pruned_module_edges={}",
                candidate_paths.len(),
                edges.len(),
                pruned_module_edges
            ));
        }
        let validation = snapshots.into_values().collect::<Vec<_>>();
        facts
            .validate_against(&validation)
            .map_err(|error| coordination::invalid(error.to_string()))?;
        Ok((facts, true))
    }

    fn context_edges(
        db: &Connection,
        project_id: &str,
        context: &str,
        seeds: &HashSet<String>,
    ) -> Result<Vec<ContextEdge>, PersistenceServiceError> {
        let placeholders = (0..seeds.len())
            .map(|index| format!("?{}", index + 3))
            .collect::<Vec<_>>()
            .join(",");
        let second_offset = seeds.len() + 3;
        let second_placeholders = (0..seeds.len())
            .map(|index| format!("?{}", index + second_offset))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT edge.source_path,edge.destination_path,edge.source_revision_sha256,edge.destination_revision_sha256,edge.source_symbol_key,edge.destination_symbol_key,edge.reference_key,edge.relation,COALESCE(edge.resolution_method,'persisted_graph') FROM brownfield_edges edge LEFT JOIN brownfield_frontier source ON source.project_id=edge.project_id AND source.relative_path=edge.source_path LEFT JOIN brownfield_frontier destination ON destination.project_id=edge.project_id AND destination.relative_path=edge.destination_path WHERE edge.project_id=?1 AND edge.resolution_context_sha256=?2 AND edge.resolution='resolved' AND edge.destination_path IS NOT NULL AND edge.destination_revision_sha256 IS NOT NULL AND COALESCE(source.status,'mapped')<>'ignored' AND COALESCE(destination.status,'mapped')<>'ignored' AND (edge.source_path IN ({placeholders}) OR edge.destination_path IN ({second_placeholders})) ORDER BY CASE WHEN edge.destination_symbol_key IS NOT NULL THEN 0 WHEN edge.relation='call' THEN 1 WHEN edge.relation IN ('type_use','inherits','implements') THEN 2 ELSE 3 END,edge.source_path,edge.reference_key LIMIT {MAX_GRAPH_EDGES}"
        );
        let mut parameters = vec![
            SqlValue::Text(project_id.to_string()),
            SqlValue::Text(context.to_string()),
        ];
        parameters.extend(seeds.iter().cloned().map(SqlValue::Text));
        parameters.extend(seeds.iter().cloned().map(SqlValue::Text));
        db.prepare(&sql)?
            .query_map(params_from_iter(parameters), |row| {
                Ok(ContextEdge {
                    source_path: row.get(0)?,
                    destination_path: row.get(1)?,
                    source_revision: row.get(2)?,
                    destination_revision: row.get(3)?,
                    source_symbol_key: row.get(4)?,
                    destination_symbol_key: row.get(5)?,
                    reference_key: row.get(6)?,
                    relation: row.get(7)?,
                    method: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn schedule_graph_refreshes(
        &self,
        project_id: &str,
        stale: &[(String, Option<String>)],
    ) -> Result<(), PersistenceServiceError> {
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for (path, revision) in stale {
            Self::enqueue_brownfield_refresh_tx(
                &tx,
                project_id,
                path,
                revision.as_deref(),
                "graph_revalidation_stale",
            )?;
        }
        Self::ensure_brownfield_mapping_job_tx(&tx, project_id)?;
        tx.commit()?;
        self.wake_scheduler();
        Ok(())
    }
}

fn graph_snippet(
    snapshot: &SourceSnapshot,
    symbol: Option<&crate::orchestration::StructuredSymbol>,
    remaining: usize,
) -> Result<Option<crate::orchestration::CodeSnippet>, PersistenceServiceError> {
    if remaining == 0 {
        return Ok(None);
    }
    let lines = snapshot.content().split_inclusive('\n').collect::<Vec<_>>();
    if lines.is_empty() {
        return Ok(None);
    }
    let start = symbol.map_or(1, |symbol| symbol.start_line).max(1) as usize;
    let desired_end = symbol
        .map_or(start.saturating_add(23), |symbol| symbol.end_line as usize)
        .min(lines.len());
    let mut end = start.saturating_sub(1);
    let mut bytes = 0;
    while end < desired_end && bytes + lines[end].len() <= remaining.min(4096) {
        bytes += lines[end].len();
        end += 1;
    }
    if end < start {
        return Ok(None);
    }
    snapshot
        .snippet(start as u32, end as u32)
        .map(Some)
        .map_err(|error| coordination::invalid(error.to_string()))
}

fn mark_graph_partial(facts: &mut CodeContextFacts, diagnostic: &str) {
    facts.coverage.state = CoverageState::Partial;
    facts.coverage.truncated = true;
    facts.coverage.diagnostics.push(diagnostic.into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::{
        CodeContextDiscoveryMode, CollectionBudget, RelativeSourcePath, RoutedCodeContextService,
    };
    use crate::persistence_service::CreateProjectRequest;
    use std::fs;

    fn fixture(
        files: &[(&str, &str)],
    ) -> (
        tempfile::TempDir,
        tempfile::TempDir,
        PersistenceService,
        String,
    ) {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        for (path, content) in files {
            let target = workspace.path().join(path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(target, content).unwrap();
        }
        let service = PersistenceService::initialize(data.path()).unwrap();
        let project = service
            .create_project(CreateProjectRequest {
                workspace_path: workspace.path().to_string_lossy().into_owned(),
                name: Some("RS2 fixture".into()),
            })
            .unwrap()
            .project;
        service
            .set_brownfield_onboarding_consent(&project.id, true)
            .unwrap();
        (data, workspace, service, project.id)
    }

    async fn map_and_resolve(service: &PersistenceService, project: &str) {
        service.enqueue_brownfield_mapping(project).unwrap();
        assert!(
            service
                .process_brownfield_mapping(&CancellationToken::new())
                .await
                .unwrap()
        );
        assert!(
            service
                .process_brownfield_resolution(&CancellationToken::new())
                .await
                .unwrap()
        );
    }

    fn request(root: &Path, project: &str, query: &str) -> CodeContextRequest {
        CodeContextRequest {
            project_id: project.into(),
            workspace_root: root.into(),
            query: query.into(),
            hinted_paths: Vec::new(),
            discovery_mode: CodeContextDiscoveryMode::RepositoryScan,
            budget: CollectionBudget::default(),
        }
    }

    fn collect_seed(request: &CodeContextRequest) -> CodeContextFacts {
        RoutedCodeContextService::collect_blocking(request, &CancellationToken::new()).unwrap()
    }

    #[tokio::test]
    async fn expands_one_hop_in_both_directions_and_revalidates_exact_symbol() {
        let (_data, workspace, service, project) = fixture(&[
            (
                "src/lib.rs",
                "mod callee;\nuse crate::callee::hidden_target;\npub fn caller() { hidden_target(); }\n",
            ),
            ("src/callee.rs", "pub fn hidden_target() -> bool { true }\n"),
        ]);
        map_and_resolve(&service, &project).await;

        let caller_request = request(workspace.path(), &project, "alterar caller");
        let caller_seed = collect_seed(&caller_request);
        assert_eq!(caller_seed.sources.len(), 1);
        assert_eq!(caller_seed.sources[0].relative_path.as_str(), "src/lib.rs");
        let (expanded, applied) = service
            .expand_context_through_brownfield_graph(
                &caller_request,
                caller_seed,
                &CancellationToken::new(),
                Duration::from_secs(2),
            )
            .unwrap();
        assert!(applied);
        assert_eq!(expanded.sources.len(), 2);
        assert!(
            expanded
                .sources
                .iter()
                .any(|source| source.relative_path.as_str() == "src/callee.rs")
        );
        assert!(
            expanded
                .snippets
                .iter()
                .any(
                    |snippet| snippet.source.relative_path.as_str() == "src/callee.rs"
                        && snippet.content.contains("hidden_target")
                )
        );
        let call = expanded
            .references
            .iter()
            .find(|reference| reference.observed_text == "hidden_target")
            .unwrap();
        assert!(matches!(
            call.resolution,
            ReferenceResolution::Resolved { .. }
        ));
        assert!(call.target_symbol_key.is_some());
        assert!(call.target_revision.is_some());
        assert!(expanded.extractor.resolution_context.is_some());
        expanded.validate().unwrap();

        let mut callee_request = request(workspace.path(), &project, "implementação isolada");
        callee_request.hinted_paths = vec![RelativeSourcePath::new("src/callee.rs").unwrap()];
        let callee_seed = collect_seed(&callee_request);
        assert_eq!(callee_seed.sources.len(), 1);
        let (incoming, _) = service
            .expand_context_through_brownfield_graph(
                &callee_request,
                callee_seed,
                &CancellationToken::new(),
                Duration::from_secs(2),
            )
            .unwrap();
        assert!(
            incoming
                .sources
                .iter()
                .any(|source| source.relative_path.as_str() == "src/lib.rs")
        );
    }

    #[tokio::test]
    async fn stale_edge_is_ignored_and_schedules_exact_refresh_without_cached_snippet() {
        let (_data, workspace, service, project) = fixture(&[
            (
                "src/lib.rs",
                "mod callee;\nuse crate::callee::hidden_target;\npub fn caller() { hidden_target(); }\n",
            ),
            ("src/callee.rs", "pub fn hidden_target() -> bool { true }\n"),
        ]);
        map_and_resolve(&service, &project).await;
        fs::write(
            workspace.path().join("src/callee.rs"),
            "pub fn hidden_target() -> bool { false }\n",
        )
        .unwrap();
        let graph_request = request(workspace.path(), &project, "alterar caller");
        let seed = collect_seed(&graph_request);
        let (facts, applied) = service
            .expand_context_through_brownfield_graph(
                &graph_request,
                seed,
                &CancellationToken::new(),
                Duration::from_secs(2),
            )
            .unwrap();
        assert!(applied);
        assert!(
            !facts
                .sources
                .iter()
                .any(|source| source.relative_path.as_str() == "src/callee.rs")
        );
        assert!(
            !facts
                .snippets
                .iter()
                .any(|snippet| snippet.content.contains("false"))
        );
        assert!(facts.coverage.truncated);
        let pending: i64 = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM brownfield_refresh_queue WHERE project_id=?1 AND relative_path='src/callee.rs' AND state='pending'",
                [&project],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending, 1);
    }

    #[tokio::test]
    async fn ambiguous_edges_and_exhausted_source_budget_never_expand() {
        let (_data, workspace, service, project) = fixture(&[
            (
                "src/client.ts",
                "import { first as choose } from './first';\nimport { second as choose } from './second';\nexport function caller() { return choose(); }\n",
            ),
            ("src/first.ts", "export function first() { return 1; }\n"),
            ("src/second.ts", "export function second() { return 2; }\n"),
        ]);
        map_and_resolve(&service, &project).await;
        service
            .connection()
            .unwrap()
            .execute(
                "UPDATE brownfield_edges SET resolution='ambiguous',destination_path=NULL,destination_revision_sha256=NULL,destination_symbol_key=NULL,candidate_count=2 WHERE project_id=?1 AND source_path='src/client.ts'",
                [&project],
            )
            .unwrap();
        let mut graph_request = request(workspace.path(), &project, "alterar caller");
        graph_request.budget.max_package_files = 1;
        let seed = collect_seed(&graph_request);
        assert_eq!(seed.sources.len(), 1);
        let (facts, applied) = service
            .expand_context_through_brownfield_graph(
                &graph_request,
                seed,
                &CancellationToken::new(),
                Duration::from_secs(2),
            )
            .unwrap();
        assert!(applied);
        assert_eq!(facts.sources.len(), 1);
        assert!(facts.extractor.resolution_context.is_some());
        assert!(
            !facts.references.iter().any(|reference| matches!(
                reference.resolution,
                ReferenceResolution::Resolved { .. }
            ))
        );
    }

    #[tokio::test]
    async fn manifest_advance_falls_back_and_schedules_resolution_only() {
        let (_data, workspace, service, project) = fixture(&[
            ("Cargo.toml", "[package]\nname='rs2'\nversion='0.1.0'\n"),
            (
                "src/lib.rs",
                "mod callee;\nuse crate::callee::hidden_target;\npub fn caller() { hidden_target(); }\n",
            ),
            ("src/callee.rs", "pub fn hidden_target() {}\n"),
        ]);
        map_and_resolve(&service, &project).await;
        fs::write(
            workspace.path().join("Cargo.toml"),
            "[package]\nname='rs2'\nversion='0.2.0'\n",
        )
        .unwrap();
        let graph_request = request(workspace.path(), &project, "alterar caller");
        let seed = collect_seed(&graph_request);
        let (facts, applied) = service
            .expand_context_through_brownfield_graph(
                &graph_request,
                seed,
                &CancellationToken::new(),
                Duration::from_secs(2),
            )
            .unwrap();
        assert!(!applied);
        assert_eq!(facts.sources.len(), 1);
        assert!(facts.extractor.resolution_context.is_none());
        let jobs: (i64, i64) = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT SUM(kind='brownfield_resolve' AND status='pending'),SUM(kind='brownfield_map' AND status='pending') FROM jobs WHERE project_id=?1",
                [&project],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(jobs, (1, 0));
    }
}
