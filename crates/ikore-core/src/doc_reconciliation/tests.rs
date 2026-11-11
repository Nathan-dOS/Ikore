use super::*;
use crate::persistence_service::EnsureProjectForWorkspaceRequest;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const NOTE: &str = "knowledge/obsidian/features/api.md";
const CODE: &str = "src/api.rs";

struct Fixture {
    _data: tempfile::TempDir,
    workspace: tempfile::TempDir,
    service: PersistenceService,
    project: String,
}

impl Fixture {
    fn new() -> Self {
        let data = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let service = PersistenceService::initialize(data.path()).unwrap();
        let project = service
            .ensure_project_for_workspace(EnsureProjectForWorkspaceRequest {
                workspace_path: workspace.path().to_string_lossy().into(),
                preferred_name: None,
            })
            .unwrap()
            .project
            .id;
        let f = Self {
            _data: data,
            workspace,
            service,
            project,
        };
        f.write(CODE, "pub fn api() -> &'static str { \"json\" }\n");
        f.write(
            NOTE,
            "---\ntype: feature\nsource-files:\n  - src/api.rs\n---\n# API\nThe API returns JSON.\n## Errors\nErrors use code 400.\n",
        );
        f
    }
    fn write(&self, relative: &str, content: &str) {
        let path = self.workspace.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.workspace.path().join(relative)).unwrap()
    }
    fn root(&self) -> std::path::PathBuf {
        self.workspace.path().canonicalize().unwrap()
    }
    fn changed(&self, relative: &str) {
        self.service
            .record_doc_drift_change(
                &self.service.connection().unwrap(),
                &self.project,
                &self.root(),
                relative,
            )
            .unwrap();
    }
    fn overview(&self) -> DocDriftOverview {
        self.service.doc_drift_overview(&self.project).unwrap()
    }
    fn act(&self, action: ReconciliationAction) -> Result<Option<ApprovalOutcome>, Error> {
        self.service.reconcile_doc_drift(&self.project, action)
    }
    /// The note with its linked file changed once.
    fn drifted(&self) {
        assert!(
            self.overview().notes.is_empty(),
            "a first observation never signals"
        );
        self.write(CODE, "pub fn api() -> &'static str { \"msgpack\" }\n");
        self.changed(CODE);
    }
}

/// Answers with the next scripted reply (the last one repeats) and keeps
/// every prompt.
struct Gateway {
    replies: std::sync::Mutex<std::collections::VecDeque<String>>,
    prompts: std::sync::Mutex<Vec<String>>,
}

impl Gateway {
    fn new(replies: Vec<String>) -> Arc<Self> {
        Arc::new(Self {
            replies: std::sync::Mutex::new(replies.into()),
            prompts: std::sync::Mutex::new(Vec::new()),
        })
    }
    fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
}

impl crate::llm::LlmGateway for Gateway {
    fn execute<'a>(
        &'a self,
        request: &'a crate::llm::LlmRequest,
        _cancellation: &'a CancellationToken,
    ) -> crate::llm::GatewayFuture<'a> {
        Box::pin(async move {
            use crate::llm::*;
            assert_eq!(
                request.target,
                AgentExecutionTarget::canonical(AgentRole::DocumentationReconciler)
            );
            self.prompts
                .lock()
                .unwrap()
                .push(request.user_prompt.clone());
            let content = {
                let mut queue = self.replies.lock().unwrap();
                if queue.len() > 1 {
                    queue.pop_front().unwrap()
                } else {
                    queue.front().cloned().unwrap_or_default()
                }
            };
            Ok(LlmResponse {
                execution_id: request.execution_id.clone(),
                generation_id: request.generation_id.clone(),
                route_id: RouteId::new("fixture"),
                connection_id: ConnectionId::new("fixture"),
                provider_id: ProviderId::new("fixture"),
                model: "fixture".into(),
                request_payload: json!({}),
                response_payload: json!({}),
                content,
                usage: LlmUsage::default(),
                finish_reason: None,
                estimated_cost_usd: None,
            })
        })
    }
}

fn runtime(f: &Fixture, gateway: Arc<Gateway>) -> Arc<DocReconcilerRuntime> {
    let mut runtime = DocReconcilerRuntime::new(
        f.service.clone(),
        Arc::new(crate::AppEventBus::default()),
        tokio::runtime::Handle::current(),
    );
    runtime.gateway_override = Some(gateway);
    Arc::new(runtime)
}

/// Replaces the `# API` section, which the heading partition issues as the
/// second anchor (after the front matter).
fn update_reply() -> String {
    json!({
        "outputLanguage": "en",
        "changeSummary": ["The API now returns MessagePack instead of JSON."],
        "edits": [{"anchorRef": "anchor_2", "content": "# API\nThe API returns MessagePack.\n"}],
        "unresolved": []
    })
    .to_string()
}

async fn proposed(f: &Fixture, gateway: Arc<Gateway>) -> ReconciliationProposal {
    let runtime = runtime(f, gateway);
    runtime.propose(&f.project, NOTE).unwrap();
    runtime.wait_for_idle().await;
    let overview = runtime.overview(&f.project).unwrap();
    assert!(overview.failures.is_empty(), "{:?}", overview.failures);
    overview.notes[0].proposal.clone().expect("a proposal")
}

#[test]
fn a_first_observation_never_signals_and_a_change_opens_one_signal_per_pair() {
    let f = Fixture::new();
    let overview = f.overview();
    assert_eq!(overview.tracked_links, 1);
    assert!(overview.notes.is_empty());
    assert_eq!(overview.drifted(), 0);

    f.drifted();
    let overview = f.overview();
    assert_eq!(overview.drifted(), 1);
    let signal = &overview.notes[0].signals[0];
    assert_eq!(
        (signal.note_path.as_str(), signal.code_path.as_str()),
        (NOTE, CODE)
    );
    assert_eq!(signal.origin, DriftOrigin::Watcher);
    assert_ne!(signal.base_sha256, signal.current_sha256);

    // A second change updates the same signal instead of adding one.
    f.write(CODE, "pub fn api() -> &'static str { \"cbor\" }\n");
    f.changed(CODE);
    let overview = f.overview();
    assert_eq!(overview.notes[0].signals.len(), 1);
    assert_ne!(
        overview.notes[0].signals[0].current_sha256,
        signal.current_sha256
    );

    // Back to the reconciled bytes, nothing is left to update.
    f.write(CODE, "pub fn api() -> &'static str { \"json\" }\n");
    f.changed(CODE);
    assert!(f.overview().notes.is_empty());
}

#[test]
fn the_scan_at_project_open_finds_changes_made_while_the_app_was_closed() {
    let f = Fixture::new();
    f.overview();
    // An offline change (a `git pull`): no watcher event.
    f.write(CODE, "pub fn api() -> &'static str { \"pulled\" }\n");
    assert!(f.overview().notes.is_empty());
    f.service.scan_doc_drift(&f.project).unwrap();
    let overview = f.overview();
    assert_eq!(
        overview.notes[0].signals[0].origin,
        DriftOrigin::StartupScan
    );
    let scan = overview.scan.unwrap();
    assert!(!scan.partial);
    assert_eq!((scan.scanned, scan.total), (1, 1));
}

#[test]
fn a_removed_file_is_reported_and_a_removed_link_or_note_drops_its_rows() {
    let f = Fixture::new();
    f.overview();
    std::fs::remove_file(f.workspace.path().join(CODE)).unwrap();
    f.changed(CODE);
    let overview = f.overview();
    assert_eq!(overview.notes[0].signals[0].current_sha256, None, "removed");

    // Dropping the link from the front matter drops the pair.
    f.write(
        NOTE,
        "---\ntype: feature\n---\n# API\nThe API returns JSON.\n",
    );
    f.changed(NOTE);
    let overview = f.overview();
    assert!(overview.notes.is_empty());
    assert_eq!(overview.tracked_links, 0);
    assert_eq!(overview.unlinked_notes, 1);

    // A note deleted with its links leaves no row behind.
    let g = Fixture::new();
    g.drifted();
    std::fs::remove_file(g.workspace.path().join(NOTE)).unwrap();
    g.changed(NOTE);
    let overview = g.overview();
    assert!(overview.notes.is_empty());
    assert_eq!(overview.tracked_links, 0);
}

#[test]
fn renaming_a_note_keeps_its_baselines_and_signals() {
    let f = Fixture::new();
    f.drifted();
    let renamed = "knowledge/obsidian/features/http-api.md";
    std::fs::rename(
        f.workspace.path().join(NOTE),
        f.workspace.path().join(renamed),
    )
    .unwrap();
    f.service
        .rename_doc_drift_note(&f.root(), NOTE, renamed)
        .unwrap();
    let overview = f.overview();
    assert_eq!(overview.notes[0].note_path, renamed);
    assert_eq!(overview.notes[0].signals[0].origin, DriftOrigin::Watcher);
}

#[test]
fn links_outside_the_workspace_symlinks_and_vault_notes_are_never_tracked() {
    let f = Fixture::new();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("x.rs"), "fn x() {}\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        outside.path().join("x.rs"),
        f.workspace.path().join("src/link.rs"),
    )
    .unwrap();
    f.write(
        NOTE,
        "---\ntype: feature\nsource-files: [src/api.rs, ../x.rs, src/link.rs, REVIEW.md, knowledge/obsidian/features/other.md]\n---\n# API\n",
    );
    f.write("REVIEW.md", "# Review\n");
    f.changed(NOTE);
    assert_eq!(f.overview().tracked_links, 1, "only src/api.rs");
}

#[test]
fn ignore_advances_the_baseline_and_silence_stops_the_pair() {
    let f = Fixture::new();
    f.drifted();
    assert!(
        f.act(ReconciliationAction::Ignore {
            note_path: NOTE.into(),
            reason: Some("x".repeat(MAX_REASON_CHARS + 1)),
        })
        .is_err()
    );
    f.act(ReconciliationAction::Ignore {
        note_path: NOTE.into(),
        reason: Some("Internal refactor; the behavior is the same.".into()),
    })
    .unwrap();
    assert!(f.overview().notes.is_empty());
    // The ignored bytes are the new base: only a later change signals.
    f.changed(CODE);
    assert!(f.overview().notes.is_empty());
    f.write(CODE, "pub fn api() -> u8 { 1 }\n");
    f.changed(CODE);
    assert_eq!(f.overview().drifted(), 1);

    f.act(ReconciliationAction::Silence {
        note_path: NOTE.into(),
        code_path: CODE.into(),
    })
    .unwrap();
    let overview = f.overview();
    assert!(overview.notes.is_empty());
    assert_eq!(overview.silenced.len(), 1);
    f.write(CODE, "pub fn api() -> u8 { 2 }\n");
    f.changed(CODE);
    assert!(
        f.overview().notes.is_empty(),
        "a silenced pair never signals"
    );

    f.act(ReconciliationAction::Unsilence {
        note_path: NOTE.into(),
        code_path: CODE.into(),
    })
    .unwrap();
    // Tracking resumes from the file as it is now.
    f.changed(CODE);
    assert!(f.overview().notes.is_empty());
    assert!(f.overview().silenced.is_empty());
}

#[tokio::test]
async fn a_proposal_is_made_only_on_request_and_publishes_on_the_expected_bytes() {
    let f = Fixture::new();
    f.drifted();
    let gateway = Gateway::new(vec![update_reply()]);
    // Detection alone never calls the model.
    assert!(gateway.prompts().is_empty());
    let proposal = proposed(&f, gateway.clone()).await;
    assert_eq!(proposal.state, ProposalState::Pending);
    assert_eq!(proposal.change_summary.len(), 1);
    assert!(
        proposal
            .preview_diff
            .contains("+The API returns MessagePack.")
    );
    assert!(proposal.preview_diff.contains("-The API returns JSON."));
    assert_eq!(proposal.code_changes[0].path, CODE);
    assert!(
        proposal.code_changes[0]
            .diff
            .as_deref()
            .unwrap()
            .contains("+pub fn api() -> &'static str { \"msgpack\" }")
    );
    let prompt = gateway.prompts().pop().unwrap();
    assert!(prompt.contains("\"anchorRef\":\"anchor_2\""));
    assert!(prompt.contains("msgpack"));

    let outcome = f
        .act(ReconciliationAction::Approve {
            proposal_id: proposal.id.clone(),
        })
        .unwrap();
    assert_eq!(outcome, Some(ApprovalOutcome::Published));
    assert!(f.read(NOTE).contains("The API returns MessagePack."));
    assert!(
        f.read(NOTE).contains("## Errors\nErrors use code 400."),
        "other anchors are untouched"
    );
    let overview = f.overview();
    assert!(
        overview.notes.is_empty(),
        "signals resolved, baseline advanced"
    );
    // Publishing the note is not a code change.
    f.changed(NOTE);
    f.changed(CODE);
    assert!(f.overview().notes.is_empty());
}

#[tokio::test]
async fn a_concurrent_change_invalidates_the_proposal_without_writing() {
    for change_code in [false, true] {
        let f = Fixture::new();
        f.drifted();
        let proposal = proposed(&f, Gateway::new(vec![update_reply()])).await;
        let before = f.read(NOTE);
        if change_code {
            f.write(CODE, "pub fn api() -> u8 { 3 }\n");
        } else {
            f.write(
                NOTE,
                &before.replace("Errors use code 400.", "Errors use code 422."),
            );
        }
        let outcome = f
            .act(ReconciliationAction::Approve {
                proposal_id: proposal.id.clone(),
            })
            .unwrap();
        assert!(
            matches!(outcome, Some(ApprovalOutcome::Invalidated(reason)) if reason.contains("changed since proposal"))
        );
        assert!(!f.read(NOTE).contains("MessagePack"), "nothing was written");
        let overview = f.overview();
        assert!(overview.notes[0].proposal.is_none());
        assert_eq!(
            overview.notes[0].signals[0].state,
            SignalState::Open,
            "the change is still to reconcile"
        );
        assert!(
            f.act(ReconciliationAction::Approve {
                proposal_id: proposal.id
            })
            .is_err(),
            "an invalidated proposal stays closed"
        );
    }
}

#[tokio::test]
async fn edit_and_reject_keep_the_developer_in_charge() {
    let f = Fixture::new();
    f.drifted();
    let proposal = proposed(&f, Gateway::new(vec![update_reply()])).await;
    let mine = f
        .read(NOTE)
        .replace("The API returns JSON.", "The API returns MessagePack (v2).");
    assert!(
        f.act(ReconciliationAction::Edit {
            proposal_id: proposal.id.clone(),
            content: "<REDACTED_SECRET:API_KEY_1>".into(),
        })
        .is_err()
    );
    f.act(ReconciliationAction::Edit {
        proposal_id: proposal.id.clone(),
        content: mine.clone(),
    })
    .unwrap();
    let edited = f.overview().notes[0].proposal.clone().unwrap();
    assert_eq!(edited.state, ProposalState::Edited);
    assert!(edited.preview_diff.contains("MessagePack (v2)"));
    f.act(ReconciliationAction::Approve {
        proposal_id: proposal.id,
    })
    .unwrap();
    assert_eq!(f.read(NOTE), mine);

    let g = Fixture::new();
    g.drifted();
    let proposal = proposed(&g, Gateway::new(vec![update_reply()])).await;
    g.act(ReconciliationAction::Reject {
        proposal_id: proposal.id,
    })
    .unwrap();
    let overview = g.overview();
    assert!(overview.notes[0].proposal.is_none());
    assert_eq!(overview.notes[0].signals[0].state, SignalState::Open);
}

#[tokio::test]
async fn a_refused_answer_gets_one_corrective_call_and_then_reports_the_failure() {
    let f = Fixture::new();
    f.drifted();
    let bad = json!({
        "outputLanguage": "en",
        "changeSummary": ["Changed."],
        "edits": [{"anchorRef": "anchor_9", "content": "x"}],
        "unresolved": []
    })
    .to_string();
    let gateway = Gateway::new(vec![bad]);
    let runtime = runtime(&f, gateway.clone());
    runtime.propose(&f.project, NOTE).unwrap();
    runtime.wait_for_idle().await;
    assert_eq!(gateway.prompts().len(), 2);
    let overview = runtime.overview(&f.project).unwrap();
    assert!(overview.notes[0].proposal.is_none());
    assert!(overview.failures[0].1.contains("refused twice"));
    // The developer can ask again.
    assert!(runtime.propose(&f.project, NOTE).is_ok());
    runtime.wait_for_idle().await;
}

#[tokio::test]
async fn the_request_is_sanitized_and_its_gaps_stay_visible() {
    let f = Fixture::new();
    f.write(
        NOTE,
        "---\ntype: feature\nsource-files: [src/api.rs, assets/data.txt, assets/logo.bin, src/big.rs]\n---\n# API\nDeploy key AKIAIOSFODNN7EXAMPLE stays here.\n## Errors\nErrors use code 400.\n",
    );
    // Bytes that are not UTF-8 under a text name are tracked by hash only; a
    // binary name is never a link (the repository rules block it).
    f.write("assets/logo.bin", "");
    std::fs::write(
        f.workspace.path().join("assets/logo.bin"),
        [0u8, 159, 146, 150],
    )
    .unwrap();
    std::fs::write(
        f.workspace.path().join("assets/data.txt"),
        [0xffu8, 0xfe, 0],
    )
    .unwrap();
    f.write("src/big.rs", &"// line\n".repeat(40_000));
    assert_eq!(f.overview().tracked_links, 3, "api.rs, data.txt and big.rs");
    f.write(CODE, "pub const KEY: &str = \"AKIAIOSFODNN7EXAMPLE\";\n");
    std::fs::write(
        f.workspace.path().join("assets/data.txt"),
        [0xffu8, 0xfe, 1],
    )
    .unwrap();
    f.write("src/big.rs", &"// line\n".repeat(40_001));
    for path in [CODE, "assets/data.txt", "src/big.rs"] {
        f.changed(path);
    }
    let gateway = Gateway::new(vec![
        json!({
            "outputLanguage": "en",
            "changeSummary": ["The key constant changed."],
            "edits": [],
            "unresolved": ["Whether the binary asset is still used."]
        })
        .to_string(),
    ]);
    let proposal = proposed(&f, gateway.clone()).await;
    let prompt = gateway.prompts().pop().unwrap();
    assert!(
        !prompt.contains("AKIAIOSFODNN7EXAMPLE"),
        "note and code are sanitized"
    );
    let gaps = proposal
        .code_changes
        .iter()
        .map(|change| (change.path.as_str(), change.gap.as_deref()))
        .collect::<Vec<_>>();
    assert!(gaps.contains(&("assets/data.txt", Some("content_unavailable"))));
    assert!(gaps.contains(&("src/big.rs", Some("content_unavailable"))));
    // The anchor holding the key is protected: an edit to it is refused.
    assert!(prompt.contains("\"protected\":true"));
    assert_eq!(
        proposal.proposed_content,
        f.read(NOTE),
        "only unresolved items"
    );
}

#[test]
fn a_pulsation_that_also_published_the_note_advances_the_baseline() {
    for publishes_note in [true, false] {
        let f = Fixture::new();
        f.overview();
        f.write(CODE, "pub fn api() -> u8 { 9 }\n");
        {
            let db = f.service.connection().unwrap();
            db.pragma_update(None, "foreign_keys", "OFF").unwrap();
            db.execute("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at) VALUES('pulse',?1,'Work','Integrator','done','now','now')", [&f.project]).unwrap();
            let mut paths = vec![CODE];
            if publishes_note {
                paths.push(NOTE);
            }
            for (index, path) in paths.into_iter().enumerate() {
                db.execute(
                    "INSERT INTO artifact_publications(artifact_id,project_id,pulsation_id,relative_path,expected_sha256,desired_sha256,state,updated_at,job_class) VALUES(?1,?2,'pulse',?3,NULL,'x','applied','now','main')",
                    params![format!("a{index}"), f.project, path],
                )
                .unwrap();
            }
        }
        let overview = f.overview();
        if publishes_note {
            assert!(
                overview.notes.is_empty(),
                "the pulsation updated the note itself"
            );
        } else {
            let signal = &overview.notes[0].signals[0];
            assert_eq!(signal.origin, DriftOrigin::Pulsation);
            assert_eq!(signal.pulsation_id.as_deref(), Some("pulse"));
        }
        // Each finished pulsation is handled once.
        let handled: i64 = f
            .service
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM doc_reconciliation_pulsations",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(handled, 1);
    }
}

#[test]
fn base_bytes_are_bounded_per_file_and_released_when_unreferenced() {
    let f = Fixture::new();
    f.write(
        NOTE,
        "---\ntype: feature\nsource-files: [src/api.rs, src/big.rs]\n---\n# API\n",
    );
    f.write("src/big.rs", &"x".repeat(MAX_BLOB_BYTES + 1));
    f.overview();
    let db = f.service.connection().unwrap();
    let states = db
        .prepare("SELECT code_path,base_state FROM doc_reconciliation_baselines ORDER BY code_path")
        .unwrap()
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        states,
        vec![
            ("src/api.rs".to_owned(), "stored".to_owned()),
            ("src/big.rs".to_owned(), "base_unstored".to_owned())
        ]
    );
    f.drifted_again();
    f.act(ReconciliationAction::Ignore {
        note_path: NOTE.into(),
        reason: None,
    })
    .unwrap();
    let blobs: i64 = db
        .query_row("SELECT COUNT(*) FROM doc_reconciliation_blobs", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(blobs, 1, "the old base was released, the new one kept");
}

impl Fixture {
    fn drifted_again(&self) {
        self.write(CODE, "pub fn api() -> u8 { 5 }\n");
        self.changed(CODE);
    }
}

#[test]
fn the_indicator_text_never_names_an_author_or_a_defect() {
    let text = indicator_text(3);
    assert_eq!(text, "3 document(s) may need an update after code changes");
    for origin in [
        DriftOrigin::Watcher,
        DriftOrigin::StartupScan,
        DriftOrigin::Pulsation,
    ] {
        let label = origin.label().to_ascii_lowercase();
        for word in ["bug", "defect", "broke", "by you", "author", "fault"] {
            assert!(!label.contains(word), "{label}");
        }
    }
}

#[test]
fn workspace_events_only_compare_tracked_files_and_never_rebuild_the_links() {
    let f = Fixture::new();
    let baselines = |f: &Fixture| -> i64 {
        f.service
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM doc_reconciliation_baselines",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    // A note event and an event on an unlinked file write nothing: the
    // brownfield maintenance that delivers them must not pay for the Vault
    // index (links are derived by the overview and the scan).
    f.write("src/other.rs", "fn other() {}\n");
    f.changed(NOTE);
    f.changed("src/other.rs");
    assert_eq!(baselines(&f), 0);
    assert_eq!(f.overview().tracked_links, 1);
    assert_eq!(baselines(&f), 1);
    f.changed("src/other.rs");
    assert!(f.overview().notes.is_empty());
}
