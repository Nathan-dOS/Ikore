//! Records the agent console's corpus of real CLI frames (ADR-040, Etapa 0).
//!
//! The capture runs the developer's real, signed-in CLIs on the pipeline
//! launch path: the adapter's arguments, `Hardening::Pipeline`, the trusted
//! executable resolution and the cleared environment. Both tests are ignored:
//! the capture spends plan quota, and the scrub writes into `fixtures/cli`.
//!
//! ```text
//! $env:IKORE_CORPUS_OUT = "<a directory outside the repository>"
//! $env:IKORE_CORPUS_WORKSPACE = "C:\Users\Public\ikore-corpus"   # a path that names no one
//! $env:IKORE_CORPUS_BACKEND = "claude"      # codex, claude, gemini, opencode, antigravity
//! $env:IKORE_CORPUS_MODEL = "haiku"         # optional; the CLI's default otherwise
//! $env:IKORE_CORPUS_SCENARIOS = "tool-ok"   # optional; every scenario otherwise
//! cargo test -p ikore-terminal --lib capture_cli_corpus -- --ignored --nocapture
//! $env:IKORE_CORPUS_SCRUB_EXTRA = "<private words, comma-separated>"   # optional
//! cargo test -p ikore-terminal --lib scrub_captured_corpus -- --ignored --nocapture
//! ```
//!
//! The capture writes `<out>/<backend>/<version>/<scenario>.raw.jsonl`, one
//! `{"ms", "stream", "line"}` object per line the CLI printed, and
//! `<scenario>.meta.json`. Both are unsanitized and stay outside the
//! repository. The scrub writes `fixtures/cli/<backend>/<version>/<scenario>.jsonl`
//! (the stdout lines) and `<scenario>.stderr.txt`. It replaces the workspace
//! and home paths at any escaping depth, the user and machine names, Windows
//! SIDs, e-mail addresses, session identifiers and the extra private words.
//! A model can still repeat a path in pieces across text deltas, which no
//! pattern catches: keep the workspace on a neutral path, and read every
//! scrubbed file before committing it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::runtime::Handle;
use tokio::sync::mpsc;

use super::tests::test_service;
use super::{
    CliBackend, CliInvocationRequest, Hardening, ProcessTreeJob, TerminalEventEnvelope,
    TerminalEventKind, TerminalEventSink, ThinkingLevel, adapters, configure_hidden,
    copy_safe_environment, harden_spec, secure_launch_spec,
};
use crate::test_support::TestDatabase;

/// One cheap, fixed prompt per behavior the console reads (specification, section 14).
struct Scenario {
    name: &'static str,
    prompt: &'static str,
    thinking: ThinkingLevel,
    /// Stops the run at its first tool call, as the developer's Stop would.
    cancel_at_tool: bool,
}

const SCENARIOS: [Scenario; 6] = [
    Scenario {
        name: "tool-ok",
        prompt: "Read the file notes.txt in the current directory and reply with its second line only.",
        thinking: ThinkingLevel::Default,
        cancel_at_tool: false,
    },
    Scenario {
        name: "tool-error",
        prompt: "Read the file missing-notes.txt in the current directory. If it cannot be read, reply with the single word MISSING.",
        thinking: ThinkingLevel::Default,
        cancel_at_tool: false,
    },
    Scenario {
        name: "denied",
        prompt: "Create a file named created.txt in the current directory containing the word hello, then reply DONE. If you cannot, reply DENIED.",
        thinking: ThinkingLevel::Default,
        cancel_at_tool: false,
    },
    Scenario {
        name: "reasoning",
        prompt: "Think it through before answering: a bat and a ball cost 1.10 in total, and the bat costs 1.00 more than the ball. How much does the ball cost? Reply with the number only.",
        thinking: ThinkingLevel::High,
        cancel_at_tool: false,
    },
    Scenario {
        name: "secret-read",
        prompt: "Read config.env in the current directory and reply with the number of lines it has.",
        thinking: ThinkingLevel::Default,
        cancel_at_tool: false,
    },
    Scenario {
        name: "cancel",
        prompt: "Read every file in the current directory, one at a time, and summarize each in one sentence.",
        thinking: ThinkingLevel::Default,
        cancel_at_tool: true,
    },
];

/// The longest a scenario runs before the capture stops it.
const SCENARIO_TIMEOUT: Duration = Duration::from_secs(240);

struct NoEvents;

impl TerminalEventSink for NoEvents {
    fn emit(&self, _: &TerminalEventEnvelope) {}
}

/// The workspace every scenario runs in. The token is a synthetic canary the
/// fixture redactor knows (`synthetic-…`), never a credential.
fn write_fixture_workspace(root: &Path) {
    if root.exists() {
        std::fs::remove_dir_all(root).unwrap();
    }
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("notes.txt"),
        "Ikore corpus fixture\nThe second line is the answer.\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src").join("lib.rs"),
        "pub fn add(left: u32, right: u32) -> u32 {\n    left + right\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("config.env"),
        "SERVICE_URL=https://example.invalid\nSERVICE_TOKEN=synthetic-corpus-canary-7c1e\n",
    )
    .unwrap();
}

/// Where the scenarios run: `IKORE_CORPUS_WORKSPACE`, or `<out>/workspace`.
fn corpus_workspace(out: &Path) -> PathBuf {
    std::env::var("IKORE_CORPUS_WORKSPACE")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .map_or_else(|| out.join("workspace"), PathBuf::from)
}

/// `1.2.3` out of a version banner such as `codex-cli 0.155.1`.
fn semantic_version(banner: Option<&str>) -> String {
    Regex::new(r"\d+\.\d+\.\d+")
        .unwrap()
        .find(banner.unwrap_or_default())
        .map_or_else(|| "unknown".to_owned(), |found| found.as_str().to_owned())
}

fn pump<R: AsyncRead + Unpin + Send + 'static>(
    stream: &'static str,
    reader: R,
    started: Instant,
    sender: mpsc::UnboundedSender<(u128, &'static str, String)>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let text = String::from_utf8_lossy(&line)
                        .trim_end_matches(['\r', '\n'])
                        .to_owned();
                    if sender
                        .send((started.elapsed().as_millis(), stream, text))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    })
}

struct Captured {
    records: Vec<Value>,
    meta: Value,
}

async fn capture(
    backend: CliBackend,
    executable: &Path,
    model: Option<&str>,
    workspace: &Path,
    scenario: &Scenario,
) -> Captured {
    let request = |thinking_level| CliInvocationRequest {
        executable: executable.to_path_buf(),
        backend,
        model: model.map(str::to_owned),
        model_profile: None,
        thinking_level,
        prompt: scenario.prompt.into(),
        schema_file: None,
        schema: None,
        custom_args: Vec::new(),
        resume_session_id: None,
    };
    // An adapter without effort levels refuses one; the scenario then runs on the default.
    let (mut spec, thinking) = match adapters::build_process_spec(request(scenario.thinking)) {
        Ok(spec) => (spec, scenario.thinking),
        Err(_) => (
            adapters::build_process_spec(request(ThinkingLevel::Default)).unwrap(),
            ThinkingLevel::Default,
        ),
    };
    let _hardening = harden_spec(backend, Hardening::Pipeline, &mut spec).unwrap();
    let spec = secure_launch_spec(spec, backend).unwrap().spec;
    let mut command = Command::new(&spec.executable);
    command
        .args(&spec.args)
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();
    copy_safe_environment(&mut command);
    command.envs(spec.environment.iter().map(|(key, value)| (key, value)));
    configure_hidden(&mut command);
    let started = Instant::now();
    let mut child = command.spawn().unwrap();
    let tree = ProcessTreeJob::assign(&child).unwrap();
    // A CLI that dies at start-up closes stdin first; what it printed still counts.
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(&spec.stdin_payload).await;
    let _ = stdin.write_all(b"\n").await;
    let _ = stdin.shutdown().await;
    drop(stdin);
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let readers = [
        pump(
            "stdout",
            child.stdout.take().unwrap(),
            started,
            sender.clone(),
        ),
        pump("stderr", child.stderr.take().unwrap(), started, sender),
    ];
    let mut records = Vec::new();
    let (mut cancelled, mut timed_out) = (false, false);
    let deadline = tokio::time::sleep(SCENARIO_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            received = receiver.recv() => {
                let Some((ms, stream, line)) = received else { break };
                let tool = stream == "stdout"
                    && adapters::normalize_frame(backend, stream, line.as_bytes()).kind
                        == TerminalEventKind::ToolUse;
                records.push(json!({ "ms": ms, "stream": stream, "line": line }));
                if scenario.cancel_at_tool && tool && !cancelled {
                    cancelled = true;
                    tree.terminate();
                    let _ = child.start_kill();
                }
            }
            () = &mut deadline, if !timed_out => {
                timed_out = true;
                tree.terminate();
                let _ = child.start_kill();
            }
        }
    }
    for reader in readers {
        let _ = reader.await;
    }
    let status = child.wait().await.ok();
    let meta = json!({
        "backend": backend.as_str(),
        "model": model,
        "scenario": scenario.name,
        "prompt": scenario.prompt,
        "thinking": thinking.as_str(),
        "thinkingRequested": scenario.thinking.as_str(),
        "args": spec.args,
        "exitCode": status.and_then(|status| status.code()),
        "cancelled": cancelled,
        "timedOut": timed_out,
        "durationMs": started.elapsed().as_millis(),
        "lines": records.len(),
    });
    Captured { records, meta }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the developer's real, signed-in CLIs and spends plan quota (ADR-040 corpus)"]
async fn capture_cli_corpus() {
    let out = PathBuf::from(
        std::env::var("IKORE_CORPUS_OUT")
            .expect("IKORE_CORPUS_OUT names the raw capture directory"),
    );
    let backend = CliBackend::parse(
        &std::env::var("IKORE_CORPUS_BACKEND").expect("IKORE_CORPUS_BACKEND names the CLI"),
    )
    .filter(|backend| *backend != CliBackend::Custom)
    .expect("one of codex, claude, gemini, opencode, antigravity");
    let model = std::env::var("IKORE_CORPUS_MODEL")
        .ok()
        .filter(|model| !model.trim().is_empty());
    let only = std::env::var("IKORE_CORPUS_SCENARIOS").ok();
    let data = tempfile::tempdir().unwrap();
    let database = TestDatabase::initialize(data.path()).unwrap();
    let service = test_service(
        database.database_path(),
        Handle::current(),
        Arc::new(NoEvents),
    )
    .unwrap();
    let (executable, banner, _) = service.resolve_launch(backend, None).await.unwrap();
    let version = semantic_version(banner.as_deref());
    let directory = out.join(backend.as_str()).join(&version);
    std::fs::create_dir_all(&directory).unwrap();
    let workspace = corpus_workspace(&out);
    for scenario in SCENARIOS.iter().filter(|scenario| {
        only.as_deref()
            .is_none_or(|list| list.split(',').any(|name| name.trim() == scenario.name))
    }) {
        write_fixture_workspace(&workspace);
        let captured = capture(backend, &executable, model.as_deref(), &workspace, scenario).await;
        let raw = captured
            .records
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(
            directory.join(format!("{}.raw.jsonl", scenario.name)),
            raw + "\n",
        )
        .unwrap();
        let mut meta = captured.meta;
        meta["version"] = json!(version);
        std::fs::write(
            directory.join(format!("{}.meta.json", scenario.name)),
            serde_json::to_string_pretty(&meta).unwrap(),
        )
        .unwrap();
        println!(
            "{} {version} {}: {} lines, exit {}, cancelled {}, timed out {}",
            backend.as_str(),
            scenario.name,
            meta["lines"],
            meta["exitCode"],
            meta["cancelled"],
            meta["timedOut"],
        );
    }
}

/// `path` at any escaping depth: each separator matches any run of slashes
/// and backslashes, and the drive letter may be missing.
fn path_pattern(path: &str) -> Option<Regex> {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let mut parts = path
        .split(['\\', '/'])
        .filter(|part| !part.is_empty())
        .peekable();
    let mut pattern = String::from("(?i)");
    if parts.peek().is_some_and(|part| part.ends_with(':')) {
        let drive = parts.next()?;
        pattern.push_str(&format!(r"(?:{}[\\/]+)?", regex::escape(drive)));
    }
    let segments = parts.map(regex::escape).collect::<Vec<_>>();
    if segments.is_empty() {
        return None;
    }
    pattern.push_str(&segments.join(r"[\\/]+"));
    Regex::new(&pattern).ok()
}

/// What identifies the developer or the machine, read from the environment.
struct Private<'a> {
    workspace: &'a Path,
    home: Option<&'a str>,
    user: Option<&'a str>,
    machine: Option<&'a str>,
    /// Words of the developer's own paths and projects, never committed.
    extra: &'a [String],
}

/// Replaces what identifies the machine or the account, each identifier with a
/// stable placeholder, so a call and its result still pair after the scrub.
struct Scrubber {
    rules: Vec<(Regex, &'static str)>,
    identifiers: Vec<Regex>,
    email: Regex,
    seen: HashMap<String, String>,
}

impl Scrubber {
    fn new(private: &Private<'_>) -> Self {
        let word = |text: &str| Regex::new(&format!(r"(?i)\b{}\b", regex::escape(text))).ok();
        let mut rules = Vec::new();
        rules.extend(
            path_pattern(&private.workspace.to_string_lossy())
                .map(|pattern| (pattern, "<workspace>")),
        );
        rules.extend(
            private
                .home
                .and_then(path_pattern)
                .map(|pattern| (pattern, "<home>")),
        );
        // The machine before the user: a host name often contains it.
        for (value, placeholder) in [(private.machine, "<machine>"), (private.user, "<user>")] {
            rules.extend(
                value
                    .filter(|value| value.len() >= 3)
                    .and_then(word)
                    .map(|pattern| (pattern, placeholder)),
            );
        }
        for extra in private
            .extra
            .iter()
            .filter(|extra| !extra.trim().is_empty())
        {
            rules.extend(word(extra.trim()).map(|pattern| (pattern, "<private>")));
        }
        rules.push((
            Regex::new(r"S-1-5-21(?:-\d+)+").unwrap(),
            "S-1-5-21-0-0-0-1000",
        ));
        Self {
            rules,
            identifiers: vec![
                Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
                    .unwrap(),
                Regex::new(
                    r"\b(?:toolu|msg|req|ses|prt|call|fc|rs|resp|chatcmpl)_[A-Za-z0-9][A-Za-z0-9-]{7,}\b",
                )
                .unwrap(),
            ],
            email: Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")
                .unwrap(),
            seen: HashMap::new(),
        }
    }

    fn scrub(&mut self, text: &str) -> String {
        // Addresses first: the user name inside one would otherwise break it.
        let mut text = self
            .email
            .replace_all(text, "developer@example.invalid")
            .into_owned();
        for (pattern, placeholder) in &self.rules {
            text = pattern.replace_all(&text, *placeholder).into_owned();
        }
        for pattern in &self.identifiers {
            let mut replaced = String::with_capacity(text.len());
            let mut cursor = 0;
            for found in pattern.find_iter(&text) {
                replaced.push_str(&text[cursor..found.start()]);
                let next = self.seen.len() + 1;
                let original = found.as_str();
                let stand_in =
                    self.seen.entry(original.to_owned()).or_insert_with(|| {
                        match original.split_once('_') {
                            Some((prefix, _)) => format!("{prefix}_corpus{next:04}"),
                            None => format!("00000000-0000-4000-8000-{next:012}"),
                        }
                    });
                replaced.push_str(stand_in);
                cursor = found.end();
            }
            replaced.push_str(&text[cursor..]);
            text = replaced;
        }
        text
    }
}

#[test]
#[ignore = "writes the scrubbed corpus into fixtures/cli from a raw capture (ADR-040)"]
fn scrub_captured_corpus() {
    let out = PathBuf::from(
        std::env::var("IKORE_CORPUS_OUT")
            .expect("IKORE_CORPUS_OUT names the raw capture directory"),
    );
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("cli");
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok();
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok();
    let machine = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok();
    let extra = std::env::var("IKORE_CORPUS_SCRUB_EXTRA")
        .unwrap_or_default()
        .split(',')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let workspace = corpus_workspace(&out);
    let private = Private {
        workspace: &workspace,
        home: home.as_deref(),
        user: user.as_deref(),
        machine: machine.as_deref(),
        extra: &extra,
    };
    for entry in walkdir::WalkDir::new(&out).min_depth(3).max_depth(3) {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(scenario) = name.strip_suffix(".raw.jsonl") else {
            continue;
        };
        let version = entry.path().parent().unwrap();
        let backend = version.parent().unwrap();
        let target = fixtures
            .join(backend.file_name().unwrap())
            .join(version.file_name().unwrap());
        std::fs::create_dir_all(&target).unwrap();
        let mut scrubber = Scrubber::new(&private);
        let (mut stdout, mut stderr) = (String::new(), String::new());
        for line in std::fs::read_to_string(entry.path()).unwrap().lines() {
            if line.trim().is_empty() {
                continue;
            }
            let record: Value = serde_json::from_str(line).unwrap();
            let text = scrubber.scrub(record["line"].as_str().unwrap_or_default());
            let into = if record["stream"] == "stderr" {
                &mut stderr
            } else {
                &mut stdout
            };
            into.push_str(&text);
            into.push('\n');
        }
        std::fs::write(target.join(format!("{scenario}.jsonl")), stdout).unwrap();
        if !stderr.trim().is_empty() {
            std::fs::write(target.join(format!("{scenario}.stderr.txt")), stderr).unwrap();
        }
        println!("scrubbed {}", target.join(scenario).display());
    }
}

/// The committed corpus: `(backend, version, scenario, stdout lines)` for every
/// `fixtures/cli/<backend>/<x.y.z>/<scenario>.jsonl`.
fn committed_corpus() -> Vec<(CliBackend, String, String, Vec<String>)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("cli");
    let version = Regex::new(r"^\d+\.\d+\.\d+$").unwrap();
    let mut corpus = Vec::new();
    for entry in walkdir::WalkDir::new(&root).min_depth(3).max_depth(3) {
        let entry = entry.unwrap();
        let path = entry.path();
        let directory = path.parent().unwrap();
        let release = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let Some(backend) = directory
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| CliBackend::parse(&name.to_string_lossy()))
        else {
            continue;
        };
        if !version.is_match(&release) || path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let scenario = path.file_stem().unwrap().to_string_lossy().into_owned();
        let lines = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect();
        corpus.push((backend, release, scenario, lines));
    }
    corpus
}

#[test]
fn the_committed_corpus_names_no_one() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("cli");
    let release = Regex::new(r"[\\/]\d+\.\d+\.\d+[\\/]").unwrap();
    let user_folder = Regex::new(r"(?i)Users[\\/]+([A-Za-z0-9._ -]+)").unwrap();
    let email =
        Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}").unwrap();
    // On the machine that captured the corpus, its own names must not appear.
    let local = ["USERNAME", "USER", "COMPUTERNAME", "HOSTNAME"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .filter(|value| value.len() >= 4)
        .map(|value| Regex::new(&format!(r"(?i)\b{}\b", regex::escape(&value))).unwrap())
        .collect::<Vec<_>>();
    let mut checked = 0;
    for entry in walkdir::WalkDir::new(&root) {
        let entry = entry.unwrap();
        let path = entry.path();
        if !entry.file_type().is_file() || !release.is_match(&path.to_string_lossy()) {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        for forbidden in ["sk-proj-", "-----BEGIN", r"C:\Users\"] {
            assert!(!text.contains(forbidden), "{forbidden} in {path:?}");
        }
        for folder in user_folder.captures_iter(&text) {
            assert_eq!(&folder[1], "Public", "a user folder in {path:?}");
        }
        for address in email.find_iter(&text) {
            assert_eq!(address.as_str(), "developer@example.invalid", "{path:?}");
        }
        for name in &local {
            assert!(!name.is_match(&text), "the local {name} in {path:?}");
        }
        checked += 1;
    }
    assert!(checked > 0, "no versioned corpus under {root:?}");
}

/// Confirms, on real frames, the pairing keys of the specification's section 2
/// that the console's action summaries will rely on (ADR-040, M1).
#[test]
fn real_frames_normalize_and_carry_the_pairing_keys_the_console_needs() {
    let corpus = committed_corpus();
    assert!(!corpus.is_empty());
    let mut tool_frames: HashMap<CliBackend, usize> = HashMap::new();
    for (backend, version, scenario, lines) in &corpus {
        for line in lines {
            let frame = adapters::normalize_frame(*backend, "stdout", line.as_bytes());
            if frame.kind != TerminalEventKind::ToolUse {
                continue;
            }
            *tool_frames.entry(*backend).or_default() += 1;
            let value: Value = serde_json::from_str(line).unwrap();
            match backend {
                // One frame holds the call and its outcome, paired by `callID`.
                CliBackend::Opencode => {
                    assert!(value["part"]["callID"].is_string(), "{version} {scenario}");
                    assert!(value["part"]["state"]["status"].is_string());
                }
                // A tool step goes ACTIVE, then DONE or ERROR, under one `step_index`.
                CliBackend::Antigravity => {
                    let step = &value["step_update"];
                    assert!(step["step_index"].is_u64(), "{version} {scenario}");
                    assert!(matches!(
                        step["state"].as_str(),
                        Some("ACTIVE" | "DONE" | "ERROR")
                    ));
                    assert!(step["tool_name"].is_string());
                }
                _ => {}
            }
        }
    }
    assert!(
        tool_frames
            .get(&CliBackend::Opencode)
            .is_some_and(|count| *count > 0)
    );
    assert!(
        tool_frames
            .get(&CliBackend::Antigravity)
            .is_some_and(|count| *count > 0)
    );
    // Codex 0.155.1 on Windows, under the pipeline sandbox, never ran a command:
    // each one was rejected before an item existed, and only stderr says so.
    assert_eq!(tool_frames.get(&CliBackend::Codex), None);
}

/// What the coordinator spends per frame on the corpus, end to end: adapter,
/// identity checks, held audit events and their SQLite writes. ADR-040 Etapa 0
/// baseline; the M5 pulse tracker may add at most 2 % to it. Debug build: only
/// comparable with this same harness on the same machine.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "ADR-040 baseline; run explicitly with --ignored --nocapture"]
async fn corpus_coordinator_cost_per_frame() {
    use std::sync::Mutex;

    use ikore_contracts::AgentRole;
    use tokio_util::sync::CancellationToken;

    use super::{
        AgentExecutionMode, CreateTerminalRunRequest, ExecutionOrigin, ProcessStreamItem,
        TerminalRepository, TerminalRunStatus, coordinate_process_events_blocking,
    };
    use crate::test_support::host;

    /// The corpus over and over, so a run is long enough to time.
    const ROUNDS: usize = 200;
    let mut by_backend: Vec<(CliBackend, Vec<String>)> = Vec::new();
    for (backend, _, _, lines) in committed_corpus() {
        match by_backend.iter_mut().find(|(known, _)| *known == backend) {
            Some((_, all)) => all.extend(lines),
            None => by_backend.push((backend, lines)),
        }
    }
    for (backend, lines) in by_backend
        .into_iter()
        .filter(|(_, lines)| !lines.is_empty())
    {
        let data = tempfile::tempdir().unwrap();
        let database = TestDatabase::initialize(data.path()).unwrap();
        let project = database.insert_project(data.path(), None).unwrap().id;
        let repository = TerminalRepository::new(database.database_path(), host());
        repository.ensure_project_configs(&project, false).unwrap();
        // The model the corpus was recorded with, so the identity guard does not
        // stop the run at its first generation.
        let model = match backend {
            CliBackend::Antigravity => Some("gemini-3.8-flash".to_owned()),
            CliBackend::Opencode => Some("groq/openai/gpt-oss-20b".to_owned()),
            _ => None,
        };
        let run = repository
            .create_run(CreateTerminalRunRequest {
                project_id: project,
                agent_role: AgentRole::BackendAgent,
                origin: ExecutionOrigin::Manual,
                execution_mode: AgentExecutionMode::CliProcess,
                backend: Some(backend),
                model,
                pulsation_id: None,
                job_id: None,
                agent_run_id: None,
                llm_execution_id: None,
            })
            .unwrap();
        for status in [TerminalRunStatus::Starting, TerminalRunStatus::Running] {
            repository
                .set_run_status(&run.id, status, None, None, None)
                .unwrap();
        }
        let run = repository.get_run(&run.id).unwrap();
        let (sender, receiver) = mpsc::channel(256);
        let started = Instant::now();
        let coordinator = tokio::task::spawn_blocking({
            let repository = repository.clone();
            move || {
                coordinate_process_events_blocking(
                    repository,
                    Arc::new(NoEvents),
                    run,
                    backend,
                    receiver,
                    CancellationToken::new(),
                    Arc::new(Mutex::new(None)),
                    None,
                    None,
                )
            }
        });
        let mut sent = 0usize;
        'feed: for _ in 0..ROUNDS {
            for line in &lines {
                let frame = ProcessStreamItem::Frame {
                    stream: "stdout",
                    bytes: line.clone().into_bytes(),
                    received_at: Instant::now(),
                };
                if sender.send(frame).await.is_err() {
                    break 'feed;
                }
                sent += 1;
            }
        }
        drop(sender);
        let outcome = coordinator.await.unwrap();
        let elapsed = started.elapsed();
        println!(
            "AGENT_CONSOLE_BASELINE {}",
            json!({
                "measurement": "coordinator_per_frame",
                "backend": backend.as_str(),
                "corpusFrames": lines.len(),
                "framesSent": sent,
                "totalMs": elapsed.as_millis(),
                "perFrameUs": elapsed.as_micros() / sent.max(1) as u128,
                "outcome": match &outcome {
                    Ok(_) => "completed".to_owned(),
                    Err(error) => error.code().to_owned(),
                },
            })
        );
    }
}

#[test]
fn the_scrubber_keeps_pairs_and_drops_what_identifies_the_machine() {
    let extra = ["Secret Project".to_owned()];
    let mut scrubber = Scrubber::new(&Private {
        workspace: Path::new(r"C:\Users\Public\corpus"),
        home: Some(r"C:\Users\someone"),
        user: Some("someone"),
        machine: Some("SOMEONEBOX"),
        extra: &extra,
    });
    let line = r#"{"cwd":"C:\\Users\\Public\\corpus","home":"c:/users/someone/.codex","session_id":"3f2b9c1e-1a2b-4c3d-8e4f-0a1b2c3d4e5f","tool_use":{"id":"toolu_01AbCdEfGhIjKlMn"},"user":"someone","mail":"someone@example.com"}"#;
    let result = r#"{"tool_use_id":"toolu_01AbCdEfGhIjKlMn","session":"3f2b9c1e-1a2b-4c3d-8e4f-0a1b2c3d4e5f"}"#;
    let scrubbed = scrubber.scrub(line);
    assert_eq!(
        scrubbed,
        r#"{"cwd":"<workspace>","home":"<home>/.codex","session_id":"00000000-0000-4000-8000-000000000001","tool_use":{"id":"toolu_corpus0002"},"user":"<user>","mail":"developer@example.invalid"}"#
    );
    // Any escaping depth, a path without its drive, the machine, SIDs and private words.
    assert_eq!(
        scrubber.scrub(
            r#"Rejected(\"Get-ChildItem 'C:\\\\\\\\Users\\\\\\\\someone\\\\\\\\Documents\\\\\\\\Secret Project'\") Users\\someone\\AppData owner SOMEONEBOX\someone S-1-5-21-1724597276-448646268-765490980-1669720638"#
        ),
        r#"Rejected(\"Get-ChildItem '<home>\\\\\\\\Documents\\\\\\\\<private>'\") <home>\\AppData owner <machine>\<user> S-1-5-21-0-0-0-1000"#
    );
    assert_eq!(
        scrubber.scrub(result),
        r#"{"tool_use_id":"toolu_corpus0002","session":"00000000-0000-4000-8000-000000000001"}"#
    );
    // A provider id that carries a UUID is replaced whole.
    assert_eq!(
        scrubber.scrub(r#""callID":"fc_18b5de2e-0f54-4260-97dd-af4afabc5080""#),
        r#""callID":"fc_corpus0003""#
    );
    assert_eq!(semantic_version(Some("codex-cli 0.155.1")), "0.155.1");
    assert_eq!(semantic_version(Some("2.1.283 (Claude Code)")), "2.1.283");
    assert_eq!(semantic_version(None), "unknown");
}
