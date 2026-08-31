//! Deterministic project-local resolution for V17 Brownfield structural facts.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use rusqlite::TransactionBehavior;

use super::*;
use crate::orchestration::{ContentSha256, ReferenceBindingKind, ReferenceDescriptor};

pub const BROWNFIELD_RESOLVER_VERSION: &str = "brownfield-project-resolver-v1";
const MAX_RESOLUTION_EDGES: usize = 20_000;
const MAX_REFERENCE_CANDIDATES: usize = 16;
const MAX_MANIFESTS: usize = 12;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_RESOLUTION_DURATION: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrownfieldResolutionMetrics {
    pub examined_edges: u32,
    pub resolved_edges: u32,
    pub ambiguous_edges: u32,
    pub external_edges: u32,
    pub unresolved_edges: u32,
    pub changed_edges: u32,
    pub coverage_partial: bool,
    pub resolver_version: String,
    pub resolution_context_sha256: String,
    pub diagnostics: Vec<String>,
    pub elapsed_us: u64,
}

#[derive(Debug, Clone)]
struct SourceRow {
    path: String,
    revision: String,
    language: String,
    query_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SymbolTarget {
    path: String,
    revision: String,
    symbol_key: Option<String>,
}

#[derive(Debug, Clone)]
struct SymbolRow {
    target: SymbolTarget,
    name: String,
    qualified_name: String,
    kind: String,
    visibility: String,
    is_default: bool,
}

#[derive(Debug, Clone)]
struct EdgeRow {
    source_path: String,
    reference_key: String,
    descriptor: Option<ReferenceDescriptor>,
    source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum BindingTarget {
    Module(SymbolTarget),
    Symbol(SymbolTarget),
}

#[derive(Debug, Clone)]
enum ResolutionOutcome {
    Resolved {
        target: SymbolTarget,
        method: &'static str,
    },
    Ambiguous {
        candidates: usize,
        method: &'static str,
        reason: String,
    },
    External {
        module: String,
        method: &'static str,
    },
    Unresolved {
        method: &'static str,
        reason: String,
    },
}

#[derive(Debug, Clone, Default)]
struct ManifestModel {
    hashes: BTreeMap<String, String>,
    rust_dependencies: HashSet<String>,
    ts_aliases: Vec<TsAlias>,
    ts_base_urls: Vec<String>,
    rust_packages: HashMap<String, String>,
    ts_packages: HashMap<String, String>,
}

#[derive(Debug, Clone)]
struct TsAlias {
    pattern: String,
    targets: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ResolutionContext {
    pub(super) sha256: String,
    manifests: ManifestModel,
}

#[derive(Debug)]
struct ProjectIndex {
    sources: HashMap<String, SourceRow>,
    symbols: Vec<SymbolRow>,
    symbols_by_source_name: HashMap<(String, String), Vec<SymbolTarget>>,
    bindings: HashMap<(String, String), Vec<BindingTarget>>,
    exported_bindings: HashMap<(String, String), Vec<BindingTarget>>,
    manifests: ManifestModel,
}

impl ProjectIndex {
    fn load(
        db: &Connection,
        project_id: &str,
        manifests: ManifestModel,
    ) -> Result<Self, PersistenceServiceError> {
        let source_rows = db
            .prepare(
                "SELECT cache.relative_path,cache.content_sha256,cache.language,cache.query_version FROM brownfield_source_cache cache LEFT JOIN brownfield_frontier frontier ON frontier.project_id=cache.project_id AND frontier.relative_path=cache.relative_path WHERE cache.project_id=?1 AND COALESCE(frontier.status,'mapped')<>'ignored' ORDER BY cache.relative_path",
            )?
            .query_map([project_id], |row| {
                Ok(SourceRow {
                    path: row.get(0)?,
                    revision: row.get(1)?,
                    language: row.get(2)?,
                    query_version: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let sources = source_rows
            .into_iter()
            .map(|source| (source.path.clone(), source))
            .collect::<HashMap<_, _>>();
        let symbols = db
            .prepare(
                "SELECT symbol.source_path,symbol.symbol_key,symbol.name,symbol.qualified_name,symbol.kind,symbol.visibility,symbol.source_revision_sha256,symbol.signature FROM brownfield_symbols symbol LEFT JOIN brownfield_frontier frontier ON frontier.project_id=symbol.project_id AND frontier.relative_path=symbol.source_path WHERE symbol.project_id=?1 AND COALESCE(frontier.status,'mapped')<>'ignored' ORDER BY symbol.source_path,symbol.qualified_name,symbol.symbol_key",
            )?
            .query_map([project_id], |row| {
                Ok(SymbolRow {
                    target: SymbolTarget {
                        path: row.get(0)?,
                        symbol_key: Some(row.get(1)?),
                        revision: row.get(6)?,
                    },
                    name: row.get(2)?,
                    qualified_name: row.get(3)?,
                    kind: row.get(4)?,
                    visibility: row.get(5)?,
                    is_default: row.get::<_, String>(7)?.starts_with("export default "),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut symbols_by_source_name: HashMap<_, Vec<_>> = HashMap::new();
        for symbol in &symbols {
            symbols_by_source_name
                .entry((symbol.target.path.clone(), symbol.name.clone()))
                .or_default()
                .push(symbol.target.clone());
        }
        let mut index = Self {
            sources,
            symbols,
            symbols_by_source_name,
            bindings: HashMap::new(),
            exported_bindings: HashMap::new(),
            manifests,
        };
        index.build_bindings(db, project_id)?;
        Ok(index)
    }

    fn build_bindings(
        &mut self,
        db: &Connection,
        project_id: &str,
    ) -> Result<(), PersistenceServiceError> {
        let imports = db
            .prepare(
                "SELECT source_path,relation,normalized_reference_json FROM brownfield_edges WHERE project_id=?1 AND relation IN ('import','reexport') AND normalized_reference_json IS NOT NULL ORDER BY source_path,reference_key",
            )?
            .query_map([project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut resolved_imports = Vec::new();
        for (source, relation, descriptor_json) in imports {
            let Ok(ReferenceDescriptor::Module {
                specifier,
                bindings,
            }) = serde_json::from_str::<ReferenceDescriptor>(&descriptor_json)
            else {
                continue;
            };
            let ResolutionOutcome::Resolved { target, .. } =
                self.resolve_module(&source, &specifier)
            else {
                continue;
            };
            resolved_imports.push((source, relation == "reexport", target, bindings));
        }
        for _ in 0..MAX_REFERENCE_CANDIDATES {
            let mut changed = false;
            for (source, _reexport, target, bindings) in resolved_imports
                .iter()
                .filter(|(_, reexport, _, _)| *reexport)
            {
                for binding in bindings {
                    for (local, candidate) in self.binding_candidates(target, binding) {
                        changed |= insert_binding(
                            &mut self.exported_bindings,
                            (source.clone(), local.clone()),
                            candidate.clone(),
                        );
                        insert_binding(&mut self.bindings, (source.clone(), local), candidate);
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for (source, reexport, target, bindings) in resolved_imports {
            if reexport {
                continue;
            }
            for binding in bindings {
                for (local, candidate) in self.binding_candidates(&target, &binding) {
                    insert_binding(&mut self.bindings, (source.clone(), local), candidate);
                }
            }
        }
        Ok(())
    }

    fn binding_candidates(
        &self,
        target: &SymbolTarget,
        binding: &crate::orchestration::ReferenceBinding,
    ) -> Vec<(String, BindingTarget)> {
        if binding.kind == ReferenceBindingKind::Namespace {
            return vec![(binding.local.clone(), BindingTarget::Module(target.clone()))];
        }
        if binding.kind == ReferenceBindingKind::Glob {
            let mut candidates = self
                .symbols
                .iter()
                .filter(|symbol| {
                    symbol.target.path == target.path
                        && matches!(symbol.visibility.as_str(), "public" | "exported")
                })
                .map(|symbol| {
                    (
                        symbol.name.clone(),
                        BindingTarget::Symbol(symbol.target.clone()),
                    )
                })
                .collect::<Vec<_>>();
            candidates.extend(
                self.exported_bindings
                    .iter()
                    .filter(|((path, _), _)| path == &target.path)
                    .flat_map(|((_, name), targets)| {
                        targets.iter().cloned().map(|target| (name.clone(), target))
                    }),
            );
            candidates.truncate(MAX_REFERENCE_CANDIDATES + 1);
            return candidates;
        }
        let mut named = if binding.kind == ReferenceBindingKind::Default {
            self.default_symbols(&target.path)
        } else {
            self.symbols_in(&target.path, &binding.imported)
        };
        let mut candidates = named
            .drain(..)
            .map(BindingTarget::Symbol)
            .collect::<Vec<_>>();
        if binding.kind != ReferenceBindingKind::Default
            && let Some(reexported) = self
                .exported_bindings
                .get(&(target.path.clone(), binding.imported.clone()))
        {
            candidates.extend(reexported.iter().cloned());
        }
        if candidates.is_empty()
            && module_stem(&target.path).as_deref() == Some(binding.imported.as_str())
        {
            candidates.push(BindingTarget::Module(target.clone()));
        }
        candidates
            .into_iter()
            .take(MAX_REFERENCE_CANDIDATES + 1)
            .map(|candidate| (binding.local.clone(), candidate))
            .collect()
    }

    fn default_symbols(&self, path: &str) -> Vec<SymbolTarget> {
        self.symbols
            .iter()
            .filter(|symbol| symbol.target.path == path && symbol.is_default)
            .map(|symbol| symbol.target.clone())
            .take(MAX_REFERENCE_CANDIDATES + 1)
            .collect()
    }

    fn symbols_in(&self, path: &str, name: &str) -> Vec<SymbolTarget> {
        self.symbols_by_source_name
            .get(&(path.to_string(), name.to_string()))
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .take(MAX_REFERENCE_CANDIDATES + 1)
            .collect()
    }

    fn resolve(&self, edge: &EdgeRow) -> ResolutionOutcome {
        let Some(descriptor) = &edge.descriptor else {
            return ResolutionOutcome::Unresolved {
                method: "no_structural_descriptor",
                reason: "Referência lexical ou legado V14 sem descritor estrutural.".into(),
            };
        };
        match descriptor {
            ReferenceDescriptor::Module { specifier, .. } => {
                self.resolve_module(&edge.source_path, specifier)
            }
            ReferenceDescriptor::Symbol { path } => {
                self.resolve_symbol(&edge.source_path, path, false)
            }
            ReferenceDescriptor::Type { path } => {
                self.resolve_symbol(&edge.source_path, path, true)
            }
            ReferenceDescriptor::Implementation { trait_path, .. } => {
                self.resolve_symbol(&edge.source_path, trait_path, true)
            }
        }
    }

    fn resolve_module(&self, source: &str, specifier: &str) -> ResolutionOutcome {
        let language = self
            .sources
            .get(source)
            .map(|source| source.language.as_str())
            .unwrap_or("unsupported");
        if language == "rust" {
            self.resolve_rust_module(source, specifier)
        } else if matches!(language, "typescript" | "tsx") {
            self.resolve_typescript_module(source, specifier)
        } else {
            ResolutionOutcome::Unresolved {
                method: "unsupported_language",
                reason: "Linguagem fora do lote RS1.".into(),
            }
        }
    }

    fn resolve_rust_module(&self, source: &str, specifier: &str) -> ResolutionOutcome {
        let clean = specifier
            .split('{')
            .next()
            .unwrap_or(specifier)
            .trim_end_matches("::")
            .trim();
        let mut segments = clean
            .split("::")
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>();
        if segments.is_empty() {
            return unresolved("rust_module", "Path Rust vazio.");
        }
        let first = segments[0].replace('-', "_");
        if matches!(first.as_str(), "std" | "core" | "alloc")
            || (self.manifests.rust_dependencies.contains(&first)
                && !self.manifests.rust_packages.contains_key(&first))
        {
            return ResolutionOutcome::External {
                module: first,
                method: "rust_external_crate",
            };
        }
        let source_parent = parent_path(source);
        let mut base = if matches!(segments[0], "crate") {
            segments.remove(0);
            rust_crate_source_root(source)
        } else if matches!(segments[0], "self") {
            segments.remove(0);
            rust_module_directory(source)
        } else if matches!(segments[0], "super") {
            let mut directory = rust_module_directory(source);
            while segments.first() == Some(&"super") {
                segments.remove(0);
                directory = parent_path(&directory);
            }
            directory
        } else if let Some(package_root) = self.manifests.rust_packages.get(&first) {
            segments.remove(0);
            package_root.clone()
        } else if is_rust_root(source) {
            source_parent
        } else {
            rust_module_directory(source)
        };
        if base.is_empty() {
            base = "src".into();
        }
        for length in (1..=segments.len()).rev() {
            let logical = join_relative(&base, &segments[..length].join("/"));
            let candidates = self.existing_module_variants(&logical, &["rs"]);
            if !candidates.is_empty() {
                return outcome_from_candidates(candidates, "rust_module_tree");
            }
        }
        let crate_roots = [format!("{base}/lib.rs"), format!("{base}/main.rs")]
            .into_iter()
            .filter_map(|path| {
                self.sources.get(&path).map(|source| SymbolTarget {
                    path,
                    revision: source.revision.clone(),
                    symbol_key: None,
                })
            })
            .collect::<Vec<_>>();
        if !crate_roots.is_empty() {
            return outcome_from_candidates(crate_roots, "rust_workspace_crate");
        }
        unresolved(
            "rust_module_tree",
            "Módulo Rust não encontrado no índice do projeto.",
        )
    }

    fn resolve_typescript_module(&self, source: &str, specifier: &str) -> ResolutionOutcome {
        let candidates = if specifier.starts_with('.') {
            normalize_relative(parent_path(source), specifier)
                .map(|base| self.existing_module_variants(&base, &["ts", "tsx"]))
                .unwrap_or_default()
        } else {
            let mut aliases = Vec::new();
            for alias in &self.manifests.ts_aliases {
                if let Some(star) = alias_match(&alias.pattern, specifier) {
                    for target in &alias.targets {
                        let replaced = target.replace('*', &star);
                        aliases.extend(self.existing_module_variants(&replaced, &["ts", "tsx"]));
                    }
                }
            }
            if aliases.is_empty() {
                for base_url in &self.manifests.ts_base_urls {
                    aliases.extend(self.existing_module_variants(
                        &join_relative(base_url, specifier),
                        &["ts", "tsx"],
                    ));
                }
            }
            if aliases.is_empty()
                && let Some(root) = self.manifests.ts_packages.get(specifier)
            {
                aliases.extend(self.existing_module_variants(root, &["ts", "tsx"]));
            }
            aliases
        };
        if !candidates.is_empty() {
            return outcome_from_candidates(candidates, "typescript_module_path");
        }
        if !specifier.starts_with('.') {
            return ResolutionOutcome::External {
                module: specifier
                    .split('/')
                    .take(if specifier.starts_with('@') { 2 } else { 1 })
                    .collect::<Vec<_>>()
                    .join("/"),
                method: "typescript_external_package",
            };
        }
        unresolved(
            "typescript_module_path",
            "Módulo TypeScript relativo não encontrado no índice do projeto.",
        )
    }

    fn existing_module_variants(&self, logical: &str, extensions: &[&str]) -> Vec<SymbolTarget> {
        let logical = logical.trim_matches('/');
        let mut variants = BTreeSet::new();
        if self.sources.contains_key(logical) {
            variants.insert(logical.to_string());
        }
        let without_known_extension = logical
            .strip_suffix(".js")
            .or_else(|| logical.strip_suffix(".jsx"))
            .unwrap_or(logical);
        for extension in extensions {
            variants.insert(format!("{without_known_extension}.{extension}"));
            variants.insert(format!("{without_known_extension}/index.{extension}"));
            if *extension == "rs" {
                variants.insert(format!("{without_known_extension}/mod.rs"));
            }
        }
        variants
            .into_iter()
            .filter_map(|path| {
                self.sources.get(&path).map(|source| SymbolTarget {
                    path,
                    revision: source.revision.clone(),
                    symbol_key: None,
                })
            })
            .take(MAX_REFERENCE_CANDIDATES + 1)
            .collect()
    }

    fn resolve_symbol(&self, source: &str, path: &[String], type_only: bool) -> ResolutionOutcome {
        let Some(name) = path.last() else {
            return unresolved("symbol_path", "Path de símbolo vazio.");
        };
        if is_builtin(name, type_only) {
            return ResolutionOutcome::External {
                module: name.clone(),
                method: "language_builtin",
            };
        }
        let mut candidates = Vec::new();
        if path.len() == 1 {
            candidates.extend(self.symbols_in(source, name));
            if let Some(bound) = self.bindings.get(&(source.to_string(), name.clone())) {
                candidates.extend(bound.iter().filter_map(|binding| match binding {
                    BindingTarget::Symbol(symbol) => Some(symbol.clone()),
                    BindingTarget::Module(_) => None,
                }));
            }
        } else {
            let qualifier = &path[0];
            if let Some(bound) = self.bindings.get(&(source.to_string(), qualifier.clone())) {
                for binding in bound {
                    match binding {
                        BindingTarget::Module(module) => {
                            candidates.extend(self.symbols_in(&module.path, name));
                        }
                        BindingTarget::Symbol(symbol) => {
                            let owner = self.symbols.iter().find(|candidate| {
                                candidate.target == *symbol && is_type_kind(&candidate.kind)
                            });
                            if let Some(owner) = owner {
                                candidates.extend(
                                    self.symbols
                                        .iter()
                                        .filter(|candidate| {
                                            candidate.target.path == owner.target.path
                                                && candidate
                                                    .qualified_name
                                                    .ends_with(&format!("{}::{name}", owner.name))
                                        })
                                        .map(|candidate| candidate.target.clone()),
                                );
                            }
                        }
                    }
                }
            }
            candidates.extend(
                self.symbols
                    .iter()
                    .filter(|symbol| {
                        symbol.target.path == source
                            && symbol
                                .qualified_name
                                .ends_with(&format!("{qualifier}::{name}"))
                            && (!type_only || is_type_kind(&symbol.kind))
                    })
                    .map(|symbol| symbol.target.clone()),
            );
            if candidates.is_empty() {
                let module_specifier = path[..path.len() - 1].join("::");
                if let ResolutionOutcome::Resolved { target, .. } =
                    self.resolve_module(source, &module_specifier)
                {
                    candidates.extend(self.symbols_in(&target.path, name));
                }
            }
        }
        if type_only {
            let allowed = self
                .symbols
                .iter()
                .filter(|symbol| is_type_kind(&symbol.kind))
                .map(|symbol| symbol.target.clone())
                .collect::<HashSet<_>>();
            candidates.retain(|candidate| allowed.contains(candidate));
        }
        candidates.sort_by(|left, right| {
            (&left.path, &left.symbol_key).cmp(&(&right.path, &right.symbol_key))
        });
        candidates.dedup();
        outcome_from_candidates(
            candidates,
            if type_only {
                "type_binding"
            } else {
                "call_binding"
            },
        )
    }
}

fn insert_binding(
    bindings: &mut HashMap<(String, String), Vec<BindingTarget>>,
    key: (String, String),
    candidate: BindingTarget,
) -> bool {
    let values = bindings.entry(key).or_default();
    if values.contains(&candidate) || values.len() > MAX_REFERENCE_CANDIDATES {
        return false;
    }
    values.push(candidate);
    true
}

impl PersistenceService {
    pub(super) fn ensure_brownfield_resolution_job(
        &self,
        project_id: &str,
    ) -> Result<Option<String>, PersistenceServiceError> {
        let project = self.active_project_by_id(project_id)?;
        let context = Self::resolution_context(
            &self.connection()?,
            Path::new(&project.workspace_path),
            project_id,
        )?;
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = Self::ensure_brownfield_resolution_job_tx(&tx, project_id, &context.sha256)?;
        tx.commit()?;
        if id.is_some() {
            self.wake_scheduler();
        }
        Ok(id)
    }

    pub(super) fn ensure_brownfield_resolution_job_tx(
        tx: &Connection,
        project_id: &str,
        context_sha256: &str,
    ) -> Result<Option<String>, PersistenceServiceError> {
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM brownfield_resolution_state WHERE project_id=?1 AND resolver_version=?2 AND resolution_context_sha256=?3 AND state='complete')",
            params![project_id, BROWNFIELD_RESOLVER_VERSION, context_sha256],
            |row| row.get::<_, bool>(0),
        )? {
            return Ok(None);
        }
        let previous_context = tx
            .query_row(
                "SELECT resolution_context_sha256 FROM brownfield_resolution_state WHERE project_id=?1",
                [project_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if previous_context
            .as_deref()
            .is_some_and(|value| value != context_sha256)
        {
            let now = Self::now_iso();
            tx.execute(
                "UPDATE jobs SET status='failed',last_error='Contexto de resolução superado.',cancellation_reason='superseded',finished_at=?2,updated_at=?2 WHERE project_id=?1 AND kind='brownfield_resolve' AND status IN ('pending','running','interrupted') AND cancellation_reason IS NULL",
                params![project_id, now],
            )?;
        }
        if let Some(existing) = tx
            .query_row(
                "SELECT id FROM jobs WHERE project_id=?1 AND kind='brownfield_resolve' AND status IN ('pending','running','interrupted') AND cancellation_reason IS NULL ORDER BY created_at,id LIMIT 1",
                [project_id],
                |row| row.get(0),
            )
            .optional()?
        {
            return Ok(Some(existing));
        }
        let now = Self::now_iso();
        tx.execute(
            "INSERT INTO brownfield_resolution_state(project_id,resolver_version,resolution_context_sha256,state,created_at,updated_at) VALUES(?1,?2,?3,'pending',?4,?4) ON CONFLICT(project_id) DO UPDATE SET resolver_version=excluded.resolver_version,resolution_context_sha256=excluded.resolution_context_sha256,state='pending',cursor_reference_key=NULL,examined_edges=0,resolved_edges=0,ambiguous_edges=0,external_edges=0,unresolved_edges=0,elapsed_us=0,diagnostic=NULL,updated_at=excluded.updated_at",
            params![project_id, BROWNFIELD_RESOLVER_VERSION, context_sha256, now],
        )?;
        let id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO jobs(id,kind,status,attempts,created_at,updated_at,project_id,job_class,idempotency_key) VALUES(?1,'brownfield_resolve','pending',0,?2,?2,?3,'background',?4)",
            params![id, now, project_id, format!("brownfield-resolve:{project_id}:{context_sha256}:{id}")],
        )?;
        Ok(Some(id))
    }

    pub(super) async fn process_brownfield_resolution(
        &self,
        shutdown: &CancellationToken,
    ) -> Result<bool, PersistenceServiceError> {
        if shutdown.is_cancelled() {
            return Ok(false);
        }
        let service = self.clone();
        let claim = tokio::task::spawn_blocking(move || {
            let id: Option<String> = service
                .connection()?
                .query_row(
                    "SELECT j.id FROM jobs j JOIN runtime_settings consent ON consent.key='brownfield.onboarding_consent.'||j.project_id AND consent.value='true' WHERE j.kind='brownfield_resolve' AND j.job_class='background' AND j.status='pending' AND j.cancellation_reason IS NULL AND NOT EXISTS(SELECT 1 FROM runtime_slots WHERE job_class='main') AND NOT EXISTS(SELECT 1 FROM jobs foreground WHERE foreground.job_class='main' AND foreground.status='pending' AND foreground.cancellation_reason IS NULL) AND NOT EXISTS(SELECT 1 FROM jobs mapping WHERE mapping.project_id=j.project_id AND mapping.kind='brownfield_map' AND mapping.status IN ('pending','running','interrupted') AND mapping.cancellation_reason IS NULL) ORDER BY j.created_at,j.id LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            id.map(|id| service.claim_background_id(&id))
                .transpose()
                .map(Option::flatten)
        })
        .await
        .map_err(|error| coordination::invalid(error.to_string()))??;
        let Some(claim) = claim else {
            return Ok(false);
        };
        let _slot = self.slot_guard(Self::job_slot_token(&claim.job));
        let service = self.clone();
        let work = claim.job.clone();
        let generation = claim.generation;
        let cancellation = claim.cancellation.clone();
        let worker_cancellation = cancellation.clone();
        let task = tokio::task::spawn_blocking(move || {
            service.execute_brownfield_resolution(&work, generation, &worker_cancellation)
        });
        tokio::pin!(task);
        let result = tokio::select! {
            result = &mut task => result.map_err(|error| coordination::invalid(error.to_string()))?,
            _ = shutdown.cancelled() => {
                cancellation.cancel();
                task.await.map_err(|error| coordination::invalid(error.to_string()))?
            }
        };
        self.unregister_job_cancellation(&claim.job.id);
        if let Err(error) = &result {
            let interrupted = shutdown.is_cancelled();
            let now = Self::now_iso();
            let db = self.connection()?;
            db.execute(
                "UPDATE jobs SET status=?2,last_error=?3,started_at=CASE WHEN ?4 THEN NULL ELSE started_at END,finished_at=CASE WHEN ?4 THEN NULL ELSE ?5 END,cancellation_reason=CASE WHEN ?4 THEN NULL ELSE cancellation_reason END,updated_at=?5 WHERE id=?1 AND generation=?6 AND status='running'",
                params![claim.job.id, if interrupted { "pending" } else { "failed" }, error.to_string(), interrupted, now, generation],
            )?;
            db.execute(
                "UPDATE brownfield_resolution_state SET state=?2,diagnostic=?3,updated_at=?4 WHERE project_id=?1 AND state='running'",
                params![
                    db.query_row(
                        "SELECT project_id FROM jobs WHERE id=?1",
                        [&claim.job.id],
                        |row| row.get::<_, String>(0),
                    )?,
                    if interrupted { "pending" } else { "failed" },
                    error.to_string(),
                    now
                ],
            )?;
        }
        result.map(|_| true)
    }

    fn execute_brownfield_resolution(
        &self,
        job: &Job,
        generation: u64,
        cancellation: &CancellationToken,
    ) -> Result<BrownfieldResolutionMetrics, PersistenceServiceError> {
        let started = Instant::now();
        let db = self.connection()?;
        let (project_id, root): (String, String) = db.query_row(
            "SELECT job.project_id,project.workspace_path FROM jobs job JOIN projects project ON project.id=job.project_id JOIN runtime_settings consent ON consent.key='brownfield.onboarding_consent.'||job.project_id AND consent.value='true' WHERE job.id=?1 AND job.kind='brownfield_resolve' AND job.status='running' AND job.generation=?2 AND job.cancellation_reason IS NULL AND project.is_archived=0",
            params![job.id, generation],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let context = Self::resolution_context(&db, Path::new(&root), &project_id)?;
        let expected: String = db.query_row(
            "SELECT resolution_context_sha256 FROM brownfield_resolution_state WHERE project_id=?1",
            [&project_id],
            |row| row.get(0),
        )?;
        if expected != context.sha256 {
            let mut db = self.connection()?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute(
                "UPDATE jobs SET status='done',finished_at=?2,updated_at=?2,last_error='Contexto de resolução avançou; novo lote agendado.' WHERE id=?1 AND generation=?3 AND status='running'",
                params![job.id, Self::now_iso(), generation],
            )?;
            Self::ensure_brownfield_resolution_job_tx(&tx, &project_id, &context.sha256)?;
            tx.commit()?;
            return Ok(BrownfieldResolutionMetrics {
                resolver_version: BROWNFIELD_RESOLVER_VERSION.into(),
                resolution_context_sha256: context.sha256,
                diagnostics: vec!["resolution_context_advanced".into()],
                ..Default::default()
            });
        }
        db.execute(
            "UPDATE brownfield_resolution_state SET state='running',diagnostic=NULL,updated_at=?2 WHERE project_id=?1 AND resolver_version=?3 AND resolution_context_sha256=?4",
            params![project_id, Self::now_iso(), BROWNFIELD_RESOLVER_VERSION, context.sha256],
        )?;
        let index = ProjectIndex::load(&db, &project_id, context.manifests.clone())?;
        let cursor: Option<String> = db.query_row(
            "SELECT cursor_reference_key FROM brownfield_resolution_state WHERE project_id=?1",
            [&project_id],
            |row| row.get(0),
        )?;
        let (cursor_source, cursor_key) = cursor
            .as_deref()
            .and_then(|cursor| cursor.split_once('\n'))
            .map_or(("", ""), |(source, key)| (source, key));
        let mut edges = db
            .prepare(
                "SELECT edge.source_path,edge.reference_key,edge.normalized_reference_json,edge.source_revision_sha256 FROM brownfield_edges edge LEFT JOIN brownfield_frontier frontier ON frontier.project_id=edge.project_id AND frontier.relative_path=edge.source_path WHERE edge.project_id=?1 AND COALESCE(frontier.status,'mapped')<>'ignored' AND (edge.source_path>?2 OR (edge.source_path=?2 AND edge.reference_key>?3)) ORDER BY edge.source_path,edge.reference_key LIMIT ?4",
            )?
            .query_map(
                params![project_id, cursor_source, cursor_key, (MAX_RESOLUTION_EDGES + 1) as i64],
                |row| {
                    let descriptor = row
                        .get::<_, Option<String>>(2)?
                        .and_then(|json| serde_json::from_str(&json).ok());
                    Ok(EdgeRow {
                        source_path: row.get(0)?,
                        reference_key: row.get(1)?,
                        descriptor,
                        source_revision: row.get(3)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let edge_limit_reached = edges.len() > MAX_RESOLUTION_EDGES;
        edges.truncate(MAX_RESOLUTION_EDGES);
        let mut resolved = Vec::with_capacity(edges.len());
        for edge in &edges {
            if cancellation.is_cancelled() {
                return Err(PersistenceServiceError::Cancelled(
                    "Resolução brownfield cancelada.".into(),
                ));
            }
            if !resolved.is_empty() && started.elapsed() >= MAX_RESOLUTION_DURATION {
                break;
            }
            resolved.push((edge.clone(), index.resolve(edge)));
        }
        let has_more = edge_limit_reached || resolved.len() < edges.len();
        let mut metrics = BrownfieldResolutionMetrics {
            examined_edges: resolved.len().min(u32::MAX as usize) as u32,
            coverage_partial: has_more,
            resolver_version: BROWNFIELD_RESOLVER_VERSION.into(),
            resolution_context_sha256: context.sha256.clone(),
            ..Default::default()
        };
        let mut db = self.connection()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::validate_resolution_fence(&tx, &job.id, generation, &project_id, &context.sha256)?;
        for (edge, outcome) in &resolved {
            metrics.changed_edges +=
                Self::persist_resolution_outcome(&tx, &project_id, edge, outcome, &context.sha256)?
                    as u32;
        }
        Self::recalculate_brownfield_frontier_tx(&tx, &project_id)?;
        let (all, resolved_count, ambiguous_count, external_count, unresolved_count): (
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = tx.query_row(
            "SELECT COUNT(*),COALESCE(SUM(edge.resolution='resolved'),0),COALESCE(SUM(edge.resolution='ambiguous'),0),COALESCE(SUM(edge.resolution='external'),0),COALESCE(SUM(edge.resolution='unresolved'),0) FROM brownfield_edges edge LEFT JOIN brownfield_frontier frontier ON frontier.project_id=edge.project_id AND frontier.relative_path=edge.source_path WHERE edge.project_id=?1 AND COALESCE(frontier.status,'mapped')<>'ignored'",
            [&project_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )?;
        metrics.resolved_edges = resolved_count.clamp(0, i64::from(u32::MAX)) as u32;
        metrics.ambiguous_edges = ambiguous_count.clamp(0, i64::from(u32::MAX)) as u32;
        metrics.external_edges = external_count.clamp(0, i64::from(u32::MAX)) as u32;
        metrics.unresolved_edges = unresolved_count.clamp(0, i64::from(u32::MAX)) as u32;
        metrics.elapsed_us = started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        let cursor = if has_more {
            resolved
                .last()
                .map(|(edge, _)| format!("{}\n{}", edge.source_path, edge.reference_key))
        } else {
            None
        };
        tx.execute(
            "UPDATE brownfield_resolution_state SET state=?2,cursor_reference_key=?3,examined_edges=?4,resolved_edges=?5,ambiguous_edges=?6,external_edges=?7,unresolved_edges=?8,elapsed_us=?9,diagnostic=?10,updated_at=?11 WHERE project_id=?1 AND resolver_version=?12 AND resolution_context_sha256=?13",
            params![project_id, if has_more { "partial" } else { "complete" }, cursor, all, resolved_count, ambiguous_count, external_count, unresolved_count, metrics.elapsed_us, if has_more { Some("Limite de 20.000 arestas; continuação agendada.") } else { None }, Self::now_iso(), BROWNFIELD_RESOLVER_VERSION, context.sha256],
        )?;
        tx.execute(
            "UPDATE jobs SET status='done',maintenance_metrics_json=?2,finished_at=?3,updated_at=?3 WHERE id=?1 AND generation=?4 AND status='running'",
            params![job.id, serde_json::to_string(&metrics).map_err(|error| coordination::invalid(error.to_string()))?, Self::now_iso(), generation],
        )?;
        if has_more {
            let id = Uuid::new_v4().to_string();
            let now = Self::now_iso();
            tx.execute(
                "INSERT INTO jobs(id,kind,status,attempts,created_at,updated_at,project_id,job_class,idempotency_key) VALUES(?1,'brownfield_resolve','pending',0,?2,?2,?3,'background',?4)",
                params![id, now, project_id, format!("brownfield-resolve:{project_id}:{}:{id}", context.sha256)],
            )?;
        }
        tx.commit()?;
        Ok(metrics)
    }

    fn persist_resolution_outcome(
        tx: &Connection,
        project_id: &str,
        edge: &EdgeRow,
        outcome: &ResolutionOutcome,
        context_sha256: &str,
    ) -> Result<usize, PersistenceServiceError> {
        let (resolution, path, revision, symbol, external, method, reason, candidates) =
            match outcome {
                ResolutionOutcome::Resolved { target, method } => (
                    "resolved",
                    Some(target.path.as_str()),
                    Some(target.revision.as_str()),
                    target.symbol_key.as_deref(),
                    None,
                    *method,
                    None,
                    1,
                ),
                ResolutionOutcome::Ambiguous {
                    candidates,
                    method,
                    reason,
                } => (
                    "ambiguous",
                    None,
                    None,
                    None,
                    None,
                    *method,
                    Some(reason.as_str()),
                    (*candidates).min(u32::MAX as usize) as u32,
                ),
                ResolutionOutcome::External { module, method } => (
                    "external",
                    None,
                    None,
                    None,
                    Some(module.as_str()),
                    *method,
                    None,
                    0,
                ),
                ResolutionOutcome::Unresolved { method, reason } => (
                    "unresolved",
                    None,
                    None,
                    None,
                    None,
                    *method,
                    Some(reason.as_str()),
                    0,
                ),
            };
        tx.execute(
            "UPDATE brownfield_edges SET destination_path=?4,destination_revision_sha256=?5,destination_symbol_key=?6,resolution=?7,external_module=?8,resolution_method=?9,resolution_reason=?10,candidate_count=?11,resolver_version=?12,resolution_context_sha256=?13,updated_at=?14 WHERE project_id=?1 AND source_path=?2 AND reference_key=?3 AND source_revision_sha256=?15 AND (destination_path IS NOT ?4 OR destination_revision_sha256 IS NOT ?5 OR destination_symbol_key IS NOT ?6 OR resolution IS NOT ?7 OR external_module IS NOT ?8 OR resolution_method IS NOT ?9 OR resolution_reason IS NOT ?10 OR candidate_count IS NOT ?11 OR resolver_version IS NOT ?12 OR resolution_context_sha256 IS NOT ?13)",
            params![project_id, edge.source_path, edge.reference_key, path, revision, symbol, resolution, external, method, reason, candidates, BROWNFIELD_RESOLVER_VERSION, context_sha256, Self::now_iso(), edge.source_revision],
        )
        .map_err(Into::into)
    }

    fn validate_resolution_fence(
        db: &Connection,
        job_id: &str,
        generation: u64,
        project_id: &str,
        context_sha256: &str,
    ) -> Result<(), PersistenceServiceError> {
        let valid = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs job JOIN runtime_settings consent ON consent.key='brownfield.onboarding_consent.'||job.project_id AND consent.value='true' JOIN brownfield_resolution_state state ON state.project_id=job.project_id WHERE job.id=?1 AND job.project_id=?2 AND job.kind='brownfield_resolve' AND job.status='running' AND job.generation=?3 AND job.cancellation_reason IS NULL AND state.resolver_version=?4 AND state.resolution_context_sha256=?5 AND state.state='running')",
            params![job_id, project_id, generation, BROWNFIELD_RESOLVER_VERSION, context_sha256],
            |row| row.get::<_, bool>(0),
        )?;
        if !valid {
            return Err(PersistenceServiceError::Cancelled(
                "Resolução brownfield superada ou sem consentimento.".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn resolution_context(
        db: &Connection,
        root: &Path,
        project_id: &str,
    ) -> Result<ResolutionContext, PersistenceServiceError> {
        let sources = db
            .prepare(
                "SELECT cache.relative_path,cache.content_sha256,cache.language,cache.query_version FROM brownfield_source_cache cache LEFT JOIN brownfield_frontier frontier ON frontier.project_id=cache.project_id AND frontier.relative_path=cache.relative_path WHERE cache.project_id=?1 AND COALESCE(frontier.status,'mapped')<>'ignored' ORDER BY cache.relative_path",
            )?
            .query_map([project_id], |row| {
                Ok(SourceRow {
                    path: row.get(0)?,
                    revision: row.get(1)?,
                    language: row.get(2)?,
                    query_version: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let manifests = read_manifests(root)?;
        let fingerprint = serde_json::to_vec(&(
            BROWNFIELD_RESOLVER_VERSION,
            sources
                .iter()
                .map(|source| {
                    (
                        source.path.as_str(),
                        source.revision.as_str(),
                        source.language.as_str(),
                        source.query_version.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
            &manifests.hashes,
        ))
        .map_err(|error| coordination::invalid(error.to_string()))?;
        Ok(ResolutionContext {
            sha256: ContentSha256::of(&fingerprint).as_str().to_string(),
            manifests,
        })
    }
}

fn read_manifests(root: &Path) -> Result<ManifestModel, PersistenceServiceError> {
    let root = root.canonicalize()?;
    let mut model = ManifestModel::default();
    let mut pending = [
        "Cargo.toml",
        "package.json",
        "tsconfig.json",
        "jsconfig.json",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<BTreeSet<_>>();
    let mut processed = BTreeSet::new();
    while model.hashes.len() < MAX_MANIFESTS {
        let Some(name) = pending.iter().next().cloned() else {
            break;
        };
        pending.remove(&name);
        if !processed.insert(name.clone()) {
            continue;
        }
        let Some(bytes) = read_manifest_bytes(&root, &name)? else {
            continue;
        };
        model
            .hashes
            .insert(name.clone(), ContentSha256::of(&bytes).as_str().to_string());
        let text = String::from_utf8_lossy(&bytes);
        let base = parent_path(&name);
        let members = if name.ends_with("Cargo.toml") {
            parse_cargo_manifest(&text, &base, &mut model)
        } else if name.ends_with("package.json") {
            parse_package_manifest(&text, &base, &mut model)
        } else {
            parse_ts_config(&text, &base, &mut model);
            Vec::new()
        };
        for member in members {
            for directory in expand_workspace_member(&root, &base, &member)? {
                for manifest in [
                    "Cargo.toml",
                    "package.json",
                    "tsconfig.json",
                    "jsconfig.json",
                ] {
                    pending.insert(join_relative(&directory, manifest));
                }
            }
        }
    }
    Ok(model)
}

fn read_manifest_bytes(
    root: &Path,
    relative: &str,
) -> Result<Option<Vec<u8>>, PersistenceServiceError> {
    let path = root.join(relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_MANIFEST_BYTES
    {
        return Ok(None);
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(root) {
        return Ok(None);
    }
    let bytes = fs::read(canonical)?;
    Ok((!bytes.contains(&0)).then_some(bytes))
}

fn parse_cargo_manifest(text: &str, base: &str, model: &mut ManifestModel) -> Vec<String> {
    let Ok(value) = toml::from_str::<toml::Value>(text) else {
        return Vec::new();
    };
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = value.get(section).and_then(toml::Value::as_table) {
            for dependency in table.keys() {
                model.rust_dependencies.insert(dependency.replace('-', "_"));
            }
        }
    }
    if let Some(name) = value
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
    {
        model
            .rust_packages
            .insert(name.replace('-', "_"), join_relative(base, "src"));
    }
    value
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(str::to_string)
        .collect()
}

fn parse_package_manifest(text: &str, base: &str, model: &mut ManifestModel) -> Vec<String> {
    let Ok(value) = parse_jsonc(text) else {
        return Vec::new();
    };
    if let Some(name) = value.get("name").and_then(Value::as_str) {
        model
            .ts_packages
            .insert(name.into(), join_relative(base, "src/index"));
    }
    value
        .get("workspaces")
        .and_then(|workspaces| {
            workspaces
                .as_array()
                .or_else(|| workspaces.get("packages").and_then(Value::as_array))
        })
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn expand_workspace_member(
    root: &Path,
    base: &str,
    pattern: &str,
) -> Result<Vec<String>, PersistenceServiceError> {
    let normalized = pattern.replace('\\', "/").trim_matches('/').to_string();
    if normalized.is_empty()
        || Path::new(&normalized).is_absolute()
        || normalized
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
        || normalized.matches('*').count() > 1
    {
        return Ok(Vec::new());
    }
    let joined = join_relative(base, &normalized);
    let Some((prefix, suffix)) = joined.split_once('*') else {
        return Ok(vec![joined]);
    };
    let directory = prefix.trim_end_matches('/');
    let mut entries = match fs::read_dir(root.join(directory)) {
        Ok(entries) => entries.filter_map(Result::ok).collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut expanded = Vec::new();
    for entry in entries.into_iter().take(MAX_MANIFESTS) {
        let metadata = entry.metadata()?;
        if !metadata.is_dir() || entry.file_type()?.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let candidate = format!("{prefix}{name}{suffix}")
            .trim_matches('/')
            .to_string();
        let canonical = root.join(&candidate).canonicalize()?;
        if canonical.starts_with(root) {
            expanded.push(candidate);
        }
    }
    Ok(expanded)
}

fn parse_ts_config(text: &str, base: &str, model: &mut ManifestModel) {
    let Ok(value) = parse_jsonc(text) else {
        return;
    };
    let Some(options) = value.get("compilerOptions") else {
        return;
    };
    let base_url = options
        .get("baseUrl")
        .and_then(Value::as_str)
        .and_then(normalize_config_path)
        .map(|path| join_relative(base, &path));
    if let Some(base_url) = &base_url
        && !model.ts_base_urls.contains(base_url)
    {
        model.ts_base_urls.push(base_url.clone());
        model.ts_base_urls.sort();
    }
    if let Some(paths) = options.get("paths").and_then(Value::as_object) {
        for (pattern, values) in paths {
            let targets = values
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(normalize_config_path)
                .map(|target| join_relative(base_url.as_deref().unwrap_or(base), &target))
                .take(MAX_REFERENCE_CANDIDATES)
                .collect::<Vec<_>>();
            if !targets.is_empty() {
                model.ts_aliases.push(TsAlias {
                    pattern: pattern.clone(),
                    targets,
                });
            }
        }
        model
            .ts_aliases
            .sort_by(|left, right| left.pattern.cmp(&right.pattern));
    }
}

fn parse_jsonc(text: &str) -> Result<Value, serde_json::Error> {
    if let Ok(value) = serde_json::from_str(text) {
        return Ok(value);
    }
    let without_comments = strip_json_comments(text);
    serde_json::from_str(&strip_trailing_commas(&without_comments))
}

fn strip_json_comments(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(character) = chars.next() {
        if in_string {
            result.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        if character == '"' {
            in_string = true;
            result.push(character);
        } else if character == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for next in chars.by_ref() {
                if next == '\n' {
                    result.push('\n');
                    break;
                }
            }
        } else if character == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = '\0';
            for next in chars.by_ref() {
                if previous == '*' && next == '/' {
                    break;
                }
                if next == '\n' {
                    result.push('\n');
                }
                previous = next;
            }
        } else {
            result.push(character);
        }
    }
    result
}

fn strip_trailing_commas(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(character) = chars.next() {
        if in_string {
            result.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            continue;
        }
        if character == '"' {
            in_string = true;
            result.push(character);
        } else if character == ',' {
            let mut lookahead = chars.clone();
            let next = lookahead.find(|next| !next.is_whitespace());
            if !matches!(next, Some('}') | Some(']')) {
                result.push(character);
            }
        } else {
            result.push(character);
        }
    }
    result
}

fn normalize_config_path(path: &str) -> Option<String> {
    if Path::new(path).is_absolute() {
        return None;
    }
    let mut parts = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => parts.push(value.to_string_lossy().to_string()),
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(parts.join("/"))
}

fn normalize_relative(base: String, relative: &str) -> Option<String> {
    let mut parts = base
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    for part in relative.replace('\\', "/").split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            value => parts.push(value.to_string()),
        }
    }
    Some(parts.join("/"))
}

fn alias_match(pattern: &str, specifier: &str) -> Option<String> {
    if let Some((prefix, suffix)) = pattern.split_once('*') {
        specifier
            .strip_prefix(prefix)
            .and_then(|tail| tail.strip_suffix(suffix))
            .map(str::to_string)
    } else if pattern == specifier {
        Some(String::new())
    } else {
        None
    }
}

fn parent_path(path: &str) -> String {
    path.rsplit_once('/')
        .map_or_else(String::new, |(parent, _)| parent.to_string())
}

fn rust_module_directory(path: &str) -> String {
    if path.ends_with("/mod.rs") {
        path.trim_end_matches("/mod.rs").to_string()
    } else if is_rust_root(path) {
        parent_path(path)
    } else {
        path.strip_suffix(".rs").unwrap_or(path).to_string()
    }
}

fn rust_crate_source_root(path: &str) -> String {
    if let Some((prefix, _)) = path.split_once("/src/") {
        return format!("{prefix}/src");
    }
    if path.starts_with("src/") || matches!(path, "src/lib.rs" | "src/main.rs") {
        return "src".into();
    }
    parent_path(path)
}

fn is_rust_root(path: &str) -> bool {
    path.ends_with("/lib.rs") || path.ends_with("/main.rs") || matches!(path, "lib.rs" | "main.rs")
}

fn join_relative(base: &str, tail: &str) -> String {
    match (base.trim_matches('/'), tail.trim_matches('/')) {
        ("", tail) => tail.to_string(),
        (base, "") => base.to_string(),
        (base, tail) => format!("{base}/{tail}"),
    }
}

fn module_stem(path: &str) -> Option<String> {
    let path = PathBuf::from(path);
    let stem = path.file_stem()?.to_str()?;
    if stem == "mod" || stem == "index" {
        path.parent()?.file_name()?.to_str().map(str::to_string)
    } else {
        Some(stem.to_string())
    }
}

fn is_type_kind(kind: &str) -> bool {
    matches!(
        kind,
        "struct" | "class" | "trait" | "interface" | "enum" | "type_alias"
    )
}

fn is_builtin(name: &str, type_only: bool) -> bool {
    let common = matches!(
        name,
        "String" | "Number" | "Boolean" | "Object" | "Array" | "Promise" | "Option" | "Result"
    );
    common
        || (type_only
            && matches!(
                name,
                "string"
                    | "number"
                    | "boolean"
                    | "void"
                    | "unknown"
                    | "never"
                    | "usize"
                    | "isize"
                    | "u8"
                    | "u16"
                    | "u32"
                    | "u64"
                    | "i8"
                    | "i16"
                    | "i32"
                    | "i64"
                    | "bool"
                    | "str"
            ))
}

fn unresolved(method: &'static str, reason: &str) -> ResolutionOutcome {
    ResolutionOutcome::Unresolved {
        method,
        reason: reason.into(),
    }
}

fn outcome_from_candidates(
    mut candidates: Vec<SymbolTarget>,
    method: &'static str,
) -> ResolutionOutcome {
    candidates.sort_by(|left, right| {
        (&left.path, &left.symbol_key).cmp(&(&right.path, &right.symbol_key))
    });
    candidates.dedup();
    match candidates.len() {
        0 => unresolved(method, "Nenhum alvo estrutural exato encontrado."),
        1 => ResolutionOutcome::Resolved {
            target: candidates.remove(0),
            method,
        },
        count => ResolutionOutcome::Ambiguous {
            candidates: count.min(MAX_REFERENCE_CANDIDATES + 1),
            method,
            reason: format!("{count} alvos estruturais exatos permanecem possíveis."),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence_service::CreateProjectRequest;

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
                name: Some("RS1 fixture".into()),
            })
            .unwrap()
            .project;
        service
            .set_brownfield_onboarding_consent(&project.id, true)
            .unwrap();
        (data, workspace, service, project.id)
    }

    async fn map_and_resolve(service: &PersistenceService, project_id: &str) {
        service.enqueue_brownfield_mapping(project_id).unwrap();
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

    #[test]
    fn jsonc_and_aliases_are_parsed_without_executing_configuration() {
        let mut model = ManifestModel::default();
        parse_ts_config(
            "{ // comment\n \"compilerOptions\": { \"baseUrl\": \"src\", \"paths\": { \"@app/*\": [\"app/*\",], }, }, }",
            "",
            &mut model,
        );
        assert_eq!(model.ts_base_urls, ["src"]);
        assert_eq!(model.ts_aliases[0].pattern, "@app/*");
        assert_eq!(alias_match("@app/*", "@app/auth").as_deref(), Some("auth"));
    }

    #[test]
    fn relative_normalization_fails_closed_when_it_escapes() {
        assert_eq!(
            normalize_relative("src/web".into(), "../auth"),
            Some("src/auth".into())
        );
        assert_eq!(normalize_relative(String::new(), "../outside"), None);
        assert_eq!(normalize_config_path("../outside"), None);
    }

    #[tokio::test]
    async fn resolves_supported_rust_and_typescript_relations_without_filename_fallback() {
        let (_data, _workspace, service, project) = fixture(&[
            (
                "src/lib.rs",
                "mod auth;\nuse crate::auth::validate_token;\nfn run() { validate_token(); }\n",
            ),
            ("src/auth.rs", "pub fn validate_token() -> bool { true }\n"),
            (
                "web/client.ts",
                "import { validateToken as check } from './token';\nexport function run() { check(); }\n",
            ),
            (
                "web/token.ts",
                "export function validateToken(): boolean { return true; }\n",
            ),
            (
                "web/view.tsx",
                "import * as React from 'react';\nexport function View() { return React.createElement('div'); }\n",
            ),
        ]);
        map_and_resolve(&service, &project).await;

        let db = service.connection().unwrap();
        let rust_call: (String, Option<String>, Option<String>) = db
            .query_row(
                "SELECT resolution,destination_path,destination_symbol_key FROM brownfield_edges WHERE project_id=?1 AND source_path='src/lib.rs' AND relation='call' AND observed_text='validate_token'",
                [&project],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(rust_call.0, "resolved");
        assert_eq!(rust_call.1.as_deref(), Some("src/auth.rs"));
        assert!(rust_call.2.is_some());

        let ts_call: (String, Option<String>, Option<String>) = db
            .query_row(
                "SELECT resolution,destination_path,destination_symbol_key FROM brownfield_edges WHERE project_id=?1 AND source_path='web/client.ts' AND relation='call' AND observed_text='check'",
                [&project],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(ts_call.0, "resolved");
        assert_eq!(ts_call.1.as_deref(), Some("web/token.ts"));
        assert!(ts_call.2.is_some());

        let external: (String, Option<String>) = db
            .query_row(
                "SELECT resolution,external_module FROM brownfield_edges WHERE project_id=?1 AND source_path='web/view.tsx' AND relation='import'",
                [&project],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(external, ("external".into(), Some("react".into())));
        let state: String = db
            .query_row(
                "SELECT state FROM brownfield_resolution_state WHERE project_id=?1",
                [&project],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state, "complete");
        drop(db);
        assert_eq!(
            service.ensure_brownfield_resolution_job(&project).unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn multiple_exact_bindings_remain_ambiguous() {
        let (_data, _workspace, service, project) = fixture(&[
            (
                "src/client.ts",
                "import { first as choose } from './first';\nimport { second as choose } from './second';\nexport function run() { choose(); }\n",
            ),
            ("src/first.ts", "export function first() { return 1; }\n"),
            ("src/second.ts", "export function second() { return 2; }\n"),
        ]);
        map_and_resolve(&service, &project).await;
        let outcome: (String, i64, Option<String>) = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT resolution,candidate_count,destination_path FROM brownfield_edges WHERE project_id=?1 AND source_path='src/client.ts' AND relation='call' AND observed_text='choose'",
                [&project],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(outcome, ("ambiguous".into(), 2, None));
    }

    #[tokio::test]
    async fn resolves_groups_reexports_default_static_and_type_relations() {
        let (_data, _workspace, service, project) = fixture(&[
            (
                "src/lib.rs",
                "mod auth;\nmod prelude;\nuse crate::prelude::{check, Token};\nfn run(_: Token) { check(); }\n",
            ),
            (
                "src/auth.rs",
                "pub struct Token;\npub fn validate_token() -> bool { true }\n",
            ),
            (
                "src/prelude.rs",
                "pub use crate::auth::{validate_token as check, Token};\n",
            ),
            ("web/api.ts", "export function execute() { return true; }\n"),
            (
                "web/barrel.ts",
                "export { execute as runApi } from './api';\n",
            ),
            (
                "web/default.ts",
                "export default function handler() { return true; }\n",
            ),
            (
                "web/models.ts",
                "export interface Contract {}\nexport class Base {}\nexport class Parser { static parse() { return true; } }\n",
            ),
            (
                "web/client.ts",
                "import handler from './default';\nimport { runApi } from './barrel';\nimport { Base, Contract, Parser } from './models';\nexport class Service extends Base implements Contract { run() { handler(); runApi(); Parser.parse(); } }\n",
            ),
        ]);
        map_and_resolve(&service, &project).await;
        let db = service.connection().unwrap();
        for (path, observed, destination) in [
            ("src/lib.rs", "check", "src/auth.rs"),
            ("web/client.ts", "handler", "web/default.ts"),
            ("web/client.ts", "runApi", "web/api.ts"),
            ("web/client.ts", "Parser.parse", "web/models.ts"),
        ] {
            let outcome: (String, Option<String>, Option<String>) = db
                .query_row(
                    "SELECT resolution,destination_path,destination_symbol_key FROM brownfield_edges WHERE project_id=?1 AND source_path=?2 AND relation='call' AND observed_text=?3",
                    params![project, path, observed],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(outcome.0, "resolved", "{path}:{observed}");
            assert_eq!(outcome.1.as_deref(), Some(destination), "{path}:{observed}");
            assert!(outcome.2.is_some(), "{path}:{observed}");
        }
        for relation in ["inherits", "implements"] {
            let outcome: (String, Option<String>) = db
                .query_row(
                    "SELECT resolution,destination_path FROM brownfield_edges WHERE project_id=?1 AND source_path='web/client.ts' AND relation=?2",
                    params![project, relation],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(outcome, ("resolved".into(), Some("web/models.ts".into())));
        }
    }

    #[tokio::test]
    async fn manifest_change_invalidates_only_resolution_context() {
        let (_data, workspace, service, project) = fixture(&[
            (
                "src/client.ts",
                "import { value } from '@app/value';\nexport function run() { value(); }\n",
            ),
            ("src/value.ts", "export function value() { return 1; }\n"),
        ]);
        map_and_resolve(&service, &project).await;
        let before: String = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT resolution_context_sha256 FROM brownfield_resolution_state WHERE project_id=?1",
                [&project],
                |row| row.get(0),
            )
            .unwrap();
        fs::write(
            workspace.path().join("tsconfig.json"),
            r#"{"compilerOptions":{"baseUrl":".","paths":{"@app/*":["src/*"]}}}"#,
        )
        .unwrap();
        service
            .record_workspace_change(
                workspace.path(),
                &crate::workspace_service::WorkspaceChangedEvent {
                    kind: "modify".into(),
                    relative_path: "tsconfig.json".into(),
                    timestamp: PersistenceService::now_iso(),
                },
            )
            .unwrap();
        let db = service.connection().unwrap();
        let pending: (i64, i64) = db
            .query_row(
                "SELECT SUM(kind='brownfield_resolve' AND status='pending'),SUM(kind='brownfield_map' AND status='pending') FROM jobs WHERE project_id=?1",
                [&project],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(pending, (1, 0));
        drop(db);
        assert!(
            service
                .process_brownfield_resolution(&CancellationToken::new())
                .await
                .unwrap()
        );
        let after: (String, String, Option<String>) = service
            .connection()
            .unwrap()
            .query_row(
                "SELECT state,resolution_context_sha256,(SELECT resolution FROM brownfield_edges WHERE project_id=?1 AND source_path='src/client.ts' AND relation='import') FROM brownfield_resolution_state WHERE project_id=?1",
                [&project],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(after.0, "complete");
        assert_ne!(after.1, before);
        assert_eq!(after.2.as_deref(), Some("resolved"));
    }

    #[tokio::test]
    async fn resolves_local_cargo_and_typescript_workspace_packages() {
        let (_data, _workspace, service, project) = fixture(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/*\"]\n[dependencies]\nauth-lib = { path = \"crates/auth\" }\n",
            ),
            (
                "src/lib.rs",
                "use auth_lib::validate;\nfn run() { validate(); }\n",
            ),
            (
                "crates/auth/Cargo.toml",
                "[package]\nname = \"auth-lib\"\nversion = \"0.1.0\"\n",
            ),
            (
                "crates/auth/src/lib.rs",
                "pub fn validate() -> bool { true }\n",
            ),
            ("package.json", r#"{"workspaces":["packages/*"]}"#),
            ("packages/token/package.json", r#"{"name":"@app/token"}"#),
            (
                "packages/token/src/index.ts",
                "export function check() { return true; }\n",
            ),
            (
                "web/client.ts",
                "import { check } from '@app/token';\nexport function run() { check(); }\n",
            ),
        ]);
        map_and_resolve(&service, &project).await;
        let db = service.connection().unwrap();
        for (source, destination) in [
            ("src/lib.rs", "crates/auth/src/lib.rs"),
            ("web/client.ts", "packages/token/src/index.ts"),
        ] {
            let outcome: (String, Option<String>) = db
                .query_row(
                    "SELECT resolution,destination_path FROM brownfield_edges WHERE project_id=?1 AND source_path=?2 AND relation='call'",
                    params![project, source],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(outcome, ("resolved".into(), Some(destination.into())));
        }
    }
}
