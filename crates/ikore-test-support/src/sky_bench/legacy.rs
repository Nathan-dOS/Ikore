//! Legacy benchmark drives the same stage worker and graph/index paths as the app.
use super::{
    Corpus, MODEL, Question, Result, data, experiments, fixtures, hash, materialize, metrics, scale,
};
use crate::{
    IsolatedTestEnvironment, MemoryCredentialStore, ScriptedHttpServer, pulsation_benchmark,
};
use ikore_core::{
    AppCore, AppCoreDependencies, AppPaths,
    agent_runtime::{self, AgentRuntimeConfig, ContextRetrievalTrace, StageExecutionInput},
    document_cache::SharedDocumentCache,
    embedding_service::OllamaEmbeddingGateway,
    llm::{
        ConnectionId, GatewayFuture, LlmError, LlmErrorKind, LlmGateway, LlmRequest, LlmResponse,
        LlmUsage, ProviderId, RouteId,
    },
    persistence_service::{EnsureProjectForWorkspaceRequest, ReindexWorkspaceEmbeddingsRequest},
    workspace_service::OpenWorkspaceRequest,
};
use metrics::{Delivery, Item, Mode, RetrievalSystem, Snippet};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;
struct Boundary {
    url: String,
    client: reqwest::Client,
}
impl LlmGateway for Boundary {
    fn execute<'a>(
        &'a self,
        request: &'a LlmRequest,
        _: &'a CancellationToken,
    ) -> GatewayFuture<'a> {
        Box::pin(async move {
            let payload = json!({"model":"parcel-fixture","messages":[{"role":"system","content":request.system_prompt},{"role":"user","content":request.user_prompt}]});
            let response: Value = self
                .client
                .post(&self.url)
                .json(&payload)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|e| LlmError::new(LlmErrorKind::InvalidRequest, e.to_string()))?
                .json()
                .await
                .map_err(|e| LlmError::new(LlmErrorKind::InvalidRequest, e.to_string()))?;
            Ok(LlmResponse {
                execution_id: request.execution_id.clone(),
                generation_id: request.generation_id.clone(),
                route_id: RouteId::new("parcel"),
                connection_id: ConnectionId::new("parcel"),
                provider_id: ProviderId::new("fixture"),
                model: "parcel-fixture".into(),
                request_payload: payload,
                response_payload: response.clone(),
                content: response["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                usage: LlmUsage {
                    input_tokens: response["usage"]["prompt_tokens"].as_i64(),
                    output_tokens: response["usage"]["completion_tokens"].as_i64(),
                },
                estimated_cost_usd: None,
                finish_reason: Some("stop".into()),
            })
        })
    }
}
struct LegacyRag {
    runtime: Runtime,
    core: Arc<AppCore>,
    _env: IsolatedTestEnvironment,
    workspace: PathBuf,
    config: AgentRuntimeConfig,
    project: String,
    embeddings: OllamaEmbeddingGateway,
    cache: SharedDocumentCache,
    _server: ScriptedHttpServer,
    boundary: Boundary,
    received: std::sync::mpsc::Receiver<String>,
    index_ms: f64,
    db_bytes: u64,
    indexed_notes: usize,
    index_problem: Option<String>,
}
fn progress(update: Value) {
    if let Some(path) = std::env::var_os("IKORE_SKY_PROGRESS_FILE") {
        let _ = fs::write(path, update.to_string());
    }
}
impl LegacyRag {
    fn open(corpus: &Corpus) -> Result<Self> {
        let env = IsolatedTestEnvironment::new();
        let root = env.workspace_dir().with_file_name("parcel");
        fs::create_dir(&root)?;
        if corpus.facts.first().is_some_and(|f| f.id.starts_with('S')) {
            // A synthetic scale checkout has exactly N Markdown documents.
            // The legacy catalog counts every Markdown file, including a README.
            let status = std::process::Command::new("git")
                .current_dir(&root)
                .args(["init", "-q", "-b", "main"])
                .status()?;
            if !status.success() {
                return Err("could not initialize scale checkout".into());
            }
            fs::write(
                root.join("ledger.rs"),
                "pub const DISTRICT_LEDGER: bool = true;
",
            )?;
        } else {
            materialize(&root)?;
        }
        corpus.render_formats(&root, true, false)?;
        let runtime = Runtime::new()?;
        let client = reqwest::Client::new();
        runtime.block_on(experiments::model_digest(&client))?;
        let core = AppCore::initialize_with_dependencies(
            AppPaths::from_data_dir(env.data_dir()),
            runtime.handle().clone(),
            AppCoreDependencies {
                credentials: Arc::new(MemoryCredentialStore::default()),
                ..Default::default()
            },
        )
        .map_err(|e| format!("{e:?}"))?;
        let persistence = core.persistence();
        let config=persistence.update_agent_runtime_config(serde_json::from_value(json!({"contextStrategy":"yaml_graph_rag","embeddingEnabled":true,"embeddingModel":MODEL}))?).map_err(|e|format!("{e:?}"))?.config;
        if config.embedding_model != MODEL {
            return Err("legacy configuration rejected the pinned tag".into());
        }
        let workspace = root.to_string_lossy().into_owned();
        let now = Instant::now();
        core.open_workspace(OpenWorkspaceRequest {
            path: workspace.clone(),
        })
        .map_err(|e| format!("{e:?}"))?;
        let project = persistence
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: workspace.clone(),
                preferred_name: Some("Parcel clearinghouse".into()),
            })
            .map_err(|e| format!("{e:?}"))?
            .project
            .id;
        runtime
            .block_on(persistence.reindex_workspace_embeddings_for_workspace(
                &workspace,
                ReindexWorkspaceEmbeddingsRequest {
                    model: Some(MODEL.into()),
                },
            ))
            .map_err(|e| format!("{e:?}"))?;
        let db = Connection::open(persistence.database_path())?;
        let expected = corpus
            .facts
            .iter()
            .map(|f| &f.constellation.path)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        let mut previous = 0;
        let mut last_change = Instant::now();
        let mut heartbeat = Instant::now();
        let (indexed_notes, index_problem) = loop {
            let count: usize = db.query_row(
                "SELECT count(*) FROM workspace_note_embeddings WHERE embedding_model=?1",
                [MODEL],
                |r| r.get(0),
            )?;
            if count >= expected {
                break (count, None);
            }
            if count != previous {
                previous = count;
                last_change = Instant::now();
            }
            if heartbeat.elapsed() > Duration::from_secs(15) {
                progress(
                    json!({"phase":"indexing","indexed":count,"expected":expected,"elapsed_seconds":now.elapsed().as_secs()}),
                );
                heartbeat = Instant::now();
            }
            if last_change.elapsed() > Duration::from_secs(180) {
                let details:Vec<String>=db.prepare("SELECT coalesce(last_error,'unknown') FROM jobs WHERE status='failed' ORDER BY created_at DESC LIMIT 3")?.query_map([],|r|r.get(0))?.collect::<rusqlite::Result<_>>()?;
                let sync: Vec<String> = db
                    .prepare(
                        "SELECT coalesce(last_error,'no scan error') FROM embedding_sync_state",
                    )?
                    .query_map([], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                let problem = format!(
                    "legacy index stopped at {count}/{expected}; jobs={details:?}; scanner={sync:?}"
                );
                if expected >= 10000 {
                    break (count, Some(problem));
                }
                return Err(problem.into());
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let index_ms = now.elapsed().as_secs_f64() * 1000.;
        let db_bytes = db.query_row(
            "SELECT page_count*page_size FROM pragma_page_count,pragma_page_size",
            [],
            |r| r.get(0),
        )?;
        let (tx, received) = std::sync::mpsc::channel();
        let server = ScriptedHttpServer::respond_with(move |request| {
            let body: Value = serde_json::from_str(&request.body).unwrap();
            tx.send(body["messages"][1]["content"].as_str().unwrap().to_owned())
                .unwrap();
            pulsation_benchmark::stage_response(request)
        });
        let boundary = Boundary {
            url: server.url("/v1/chat/completions"),
            client: client.clone(),
        };
        Ok(Self {
            runtime,
            core,
            _env: env,
            workspace: root,
            config,
            project,
            embeddings: OllamaEmbeddingGateway::new(client),
            cache: SharedDocumentCache::default(),
            _server: server,
            boundary,
            received,
            index_ms,
            db_bytes,
            indexed_notes,
            index_problem,
        })
    }
}
fn items(prompt: &str, trace: &ContextRetrievalTrace) -> Result<Vec<Item>> {
    let mut items = Vec::new();
    if let Some((_, block)) = prompt.split_once("Never list it in \u{60}contextUsage\u{60}.\n") {
        let text = block
            .split_once("\n\n## Retrieved context files")
            .ok_or("missing context boundary")?
            .0;
        let mut snippets = Vec::new();
        // Clipping may cut the tail of the projection. Keep only complete objects visible to the worker.
        if let Some((sources, tail)) = text.split_once("\"snippets\":[")
            && let Some((_, paths)) = sources.split_once("\"sourceFiles\":")
        {
            let paths: Vec<Value> = serde_json::from_str(paths.trim_end_matches(','))?;
            let mut remaining = tail;
            while remaining.starts_with('{') {
                let mut stream = serde_json::Deserializer::from_str(remaining).into_iter::<Value>();
                let Some(Ok(entry)) = stream.next() else {
                    break;
                };
                let source = entry["source"].as_u64().ok_or("missing snippet source")? as usize;
                snippets.push(Snippet {
                    path: paths[source]["path"]
                        .as_str()
                        .ok_or("missing source path")?
                        .into(),
                    start: entry["lines"][0].as_u64().ok_or("missing start line")? as usize,
                    end: entry["lines"][1].as_u64().ok_or("missing end line")? as usize,
                    text: entry["content"]
                        .as_str()
                        .ok_or("missing snippet content")?
                        .into(),
                });
                remaining = remaining[stream.byte_offset()..].trim_start_matches(',');
            }
        }
        items.push(Item {
            path: "vault://exact-code-context".into(),
            text: text.into(),
            snippets,
            similarity: None,
        });
    }
    let mut lines = prompt
        .split_once("## Retrieved context files\n")
        .ok_or("worker did not produce a retrieval section")?
        .1
        .lines()
        .peekable();
    while let Some(line) = lines.next() {
        if line.starts_with("Use \u{60}contextUsage") || line.starts_with("\u{60}contextUsage") {
            break;
        }
        if let Some(path) = line.strip_prefix("### ") {
            let _score = lines.next();
            let _provenance = lines.next();
            let fence = lines.next().ok_or("missing content fence")?;
            if !fence.starts_with("\u{60}\u{60}\u{60}") {
                return Err("invalid content fence".into());
            }
            let body = lines
                .by_ref()
                .take_while(|l| *l != fence)
                .collect::<Vec<_>>()
                .join("\n");
            let similarity = trace
                .items
                .iter()
                .find(|i| i.delivered && i.relative_path == path)
                .and_then(|i| i.semantic_similarity);
            items.push(Item {
                path: path.into(),
                text: body,
                snippets: vec![],
                similarity,
            });
        }
    }
    Ok(items)
}
impl RetrievalSystem for LegacyRag {
    fn retrieve(&mut self, q: &Question, mode: Mode) -> Result<Delivery> {
        let input = StageExecutionInput {
            anticipatory: false,
            job_id: q.id.clone(),
            pulsation_id: "parcel-benchmark".into(),
            project_id: self.project.clone(),
            feature_name: q.feature.clone(),
            stage_name: if q.stage == "Specification" {
                "SPECIFICATION".into()
            } else {
                q.stage.clone()
            },
            workspace_path: self.workspace.to_string_lossy().into(),
            runtime_db_path: self
                .core
                .persistence()
                .database_path()
                .to_string_lossy()
                .into(),
            handoff: None,
            previous_attempt: None,
            verification_findings: vec![],
            agent_run_id: None,
        };
        let token = CancellationToken::new();
        let future = agent_runtime::execute_stage(
            &self.embeddings,
            &self.boundary,
            &self.cache,
            &self.config,
            &input,
            &token,
        );
        let result = match mode {
            Mode::Native => self.runtime.block_on(future),
            Mode::Question => self
                .runtime
                .block_on(agent_runtime::bench_with_query(q.text.clone(), future)),
        }
        .map_err(|e| format!("stage worker failed: {}", e.error))?;
        let prompt = self.received.recv_timeout(Duration::from_secs(2))?;
        Ok(Delivery {
            items: items(&prompt, &result.context_trace)?,
            latency_ms: result.context_trace.duration_ms as f64,
            strategy: result.context_trace.strategy_used,
        })
    }
}
pub fn run(corpus: &Corpus, scale: Option<usize>) -> Result<Value> {
    let mut legacy = LegacyRag::open(corpus)?;
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for (index, q) in corpus.questions.iter().enumerate() {
        progress(
            json!({"phase":"retrieval","question":q.id,"ordinal":index+1,"questions":corpus.questions.len(),"indexed_notes":legacy.indexed_notes,"index_problem":legacy.index_problem}),
        );
        let overlap = metrics::overlap(q, &corpus.facts)?;
        for repetition in 0..=5 {
            let mut modes = [Mode::Native, Mode::Question];
            if (index + repetition) % 2 == 1 {
                modes.reverse();
            }
            for mode in modes {
                let start = Instant::now();
                let d = match legacy.retrieve(q, mode) {
                    Ok(delivery) => delivery,
                    Err(error) if scale.is_some() => {
                        let reason = error
                            .to_string()
                            .replace(legacy.workspace.to_string_lossy().as_ref(), "<repo>");
                        failures.push(json!({"question":q.id,"mode":mode,"repetition":repetition,
                            "elapsed_ms":start.elapsed().as_secs_f64()*1000.0,"reason":reason}));
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if repetition > 0 {
                    rows.push(json!({"question":q.id,"category":q.category,"mode":mode,"repetition":repetition,"max_target_jaccard":overlap,"strategy":d.strategy,
                        "metrics":metrics::score(q,&corpus.facts,&d),"delivered":d.items.iter().map(|i|json!({"path":i.path,"sha256":hash(i.text.as_bytes()),"characters":i.text.chars().count(),"similarity":i.similarity,"code_files":metrics::file_references(&i.text),"snippets":i.snippets})).collect::<Vec<_>>()}));
                }
            }
        }
    }
    let mut summary = json!({});
    for mode in ["native", "question"] {
        let samples: Vec<_> = rows.iter().filter(|r| r["mode"] == mode).cloned().collect();
        summary[mode] = metrics::summarize(&samples);
    }
    let result = json!({"notes":scale,"model":MODEL,"adaptive_mode":legacy.config.adaptive_context_mode.as_str(),"indexed_notes":legacy.indexed_notes,"index_problem":legacy.index_problem,"index_ms":legacy.index_ms,"db_bytes":legacy.db_bytes,"summary":summary,"samples":rows,"failed_attempts":failures});
    legacy.runtime.block_on(legacy.core.shutdown());
    Ok(result)
}
pub fn mutations(corpus: &Corpus) -> Result<Value> {
    let dir = tempfile::tempdir()?;
    materialize(dir.path())?;
    let cases: Vec<scale::Mutation> = data("mutations.yaml")?;
    let mut rows = Vec::new();
    for m in cases {
        let code = corpus
            .facts
            .iter()
            .find(|f| f.id == m.fact)
            .and_then(|f| f.code.as_ref())
            .ok_or("missing mutation anchor")?;
        m.apply(dir.path())?;
        let file = dir.path().join(&code.path);
        let body = fs::read_to_string(&file)?;
        let correct = if m.operation == "move" {
            body.contains(&code.fp)
        } else {
            file.is_file()
        };
        if correct != m.legacy_file_correct {
            return Err(format!("legacy path oracle mismatch for {}", m.id).into());
        }
        rows.push(json!({"mutation":m.id,"legacy_file_correct":correct,"legacy_line":"not applicable","sky":"pending: 4b"}));
        let fixture = fixtures().join("mock-repo").join(&code.path);
        fs::copy(fixture, file)?;
        if m.operation == "move" {
            let target = m
                .patch
                .lines()
                .find_map(|l| l.strip_prefix("+++ b/").filter(|p| *p != code.path))
                .ok_or("missing moved path")?;
            let created = dir.path().join(target);
            fs::remove_file(created)?;
        }
    }
    Ok(json!(rows))
}
mod pulse;
pub mod replay;
pub fn pulsations(corpus: &Corpus, live: bool) -> Result<Value> {
    pulse::run(corpus, live)
}
