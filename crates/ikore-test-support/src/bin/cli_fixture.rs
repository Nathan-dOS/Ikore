#![allow(
    clippy::unwrap_used,
    reason = "Test-only harnesses fail immediately when fixture setup is invalid"
)]
use std::env;
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[allow(clippy::zombie_processes)]
fn main() {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let executable_name = env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_stem()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
        })
        .unwrap_or_default();
    // Antigravity's CLI ships as `agy`; every other backend's file stem is its id.
    let official_backend = [
        ("codex", "codex"),
        ("claude", "claude"),
        ("gemini", "gemini"),
        ("opencode", "opencode"),
        ("agy", "antigravity"),
    ]
    .into_iter()
    .find(|(stem, _)| executable_name == *stem)
    .map(|(_, backend)| backend);
    // The state comes from the environment or, because the host launches CLIs with a scrubbed
    // environment, from a `<backend>.fixture-state` file next to the executable.
    let fixture_state = official_backend
        .and_then(|backend| {
            env::var(format!(
                "IKORE_CLI_FIXTURE_{}_STATE",
                backend.to_ascii_uppercase()
            ))
            .or_else(|_| {
                env::var(format!(
                    "HVC_CLI_FIXTURE_{}_STATE",
                    backend.to_ascii_uppercase()
                ))
            })
            .ok()
            .or_else(|| {
                let sibling = env::current_exe()
                    .ok()?
                    .with_file_name(format!("{backend}.fixture-state"));
                std::fs::read_to_string(sibling)
                    .ok()
                    .map(|state| state.trim().to_string())
            })
        })
        .unwrap_or_default();
    if args.iter().any(|arg| arg == "--version") {
        if fixture_state == "probe_failed" {
            eprintln!("fixture probe failed");
            std::process::exit(2);
        }
        if fixture_state == "unsupported" {
            println!("0.0.0-fixture");
        } else if fixture_state == "newer" || fixture_state == "newer_broken" {
            // Above every adapter's validated ceiling.
            println!("99.0.0");
        } else {
            println!(
                "{}",
                match official_backend.expect("backend fixture must be known") {
                    "codex" => "codex-cli 0.153.0-fixture",
                    "claude" => "2.1.81 (Claude Code)",
                    "gemini" => "0.59.0",
                    "opencode" => "1.18.17",
                    "antigravity" => "1.3.5",
                    _ => unreachable!(),
                }
            );
        }
        return;
    }
    if official_backend.is_some()
        && (args.windows(2).any(|pair| {
            matches!(pair, [first, second] if (first == "login" || first == "auth") && (second == "status" || second == "list"))
        }))
    {
        if fixture_state == "auth_required" {
            eprintln!("not logged in");
            std::process::exit(1);
        }
        println!("authenticated fixture");
        return;
    }
    // `agy -p /usage --output-format json`: the login check that also reports quota.
    if official_backend == Some("antigravity")
        && args
            .windows(2)
            .any(|pair| pair[0] == "-p" && pair[1] == "/usage")
    {
        if fixture_state == "auth_required" {
            eprintln!("error: You are not logged into Antigravity.");
            std::process::exit(1);
        }
        println!(
            "{}",
            include_str!("../../fixtures/cli/antigravity/current/usage.json").trim()
        );
        return;
    }
    if official_backend == Some("antigravity")
        && args.first().is_some_and(|argument| argument == "models")
    {
        if fixture_state == "catalog_failed" {
            eprintln!("fixture model catalog failed");
            std::process::exit(3);
        }
        print!(
            "{}",
            include_str!("../../fixtures/cli/antigravity/current/models.txt")
        );
        return;
    }
    if official_backend == Some("opencode")
        && args.first().is_some_and(|argument| argument == "models")
    {
        if fixture_state == "catalog_failed" {
            eprintln!("fixture model catalog failed");
            std::process::exit(3);
        }
        print!(
            "{}",
            include_str!("../../fixtures/cli/opencode/current/models-verbose.txt")
        );
        return;
    }
    // A self-updated CLI whose stream no longer matches the adapter contract: the prompt run
    // prints prose instead of frames and fails, so a contract validation must reject it.
    if fixture_state == "newer_broken" && official_backend.is_some() {
        let mut ignored = String::new();
        let _ = std::io::stdin().read_to_string(&mut ignored);
        println!("Welcome to the new CLI! Run `init` to get started.");
        eprintln!("unknown option: --output-format");
        std::process::exit(7);
    }
    let mode = value_after(&args, "--mode").unwrap_or("success");
    let heartbeat = value_after(&args, "--heartbeat").map(PathBuf::from);
    if mode == "child" {
        heartbeat_loop(heartbeat.expect("child requires --heartbeat"));
    }

    let mut stdin = String::new();
    std::io::stdin().read_to_string(&mut stdin).unwrap();
    if let Some(expected) = value_after(&args, "--expect-stdin")
        && !stdin.contains(expected)
    {
        eprintln!("stdin mismatch fixture");
        std::process::exit(12);
    }
    // The Notes Scribe and the Documentation Reconciler (ADR-034 M5) run
    // under the same Companion restrictions as discovery.
    if stdin.contains("feature-discovery-fixture")
        || stdin.contains("notes-scribe-fixture")
        || stdin.contains("documentation-reconciler-fixture")
    {
        assert!(
            !std::path::Path::new("src/lib.rs").exists(),
            "Discovery must only receive host-selected context"
        );
        match official_backend {
            Some("codex") => {
                assert!(args.iter().any(|s| s == "--ignore-user-config"));
                assert!(args.iter().any(|s| s == "--ignore-rules"));
                assert!(
                    !args.iter().any(|s| s == "--ephemeral"),
                    "Automatic agents need a persisted Codex rollout for model verification"
                );
                assert!(
                    args.windows(2)
                        .any(|pair| pair == ["--sandbox", "read-only"])
                );
            }
            Some("claude") => assert!(args.iter().any(|s| s == "--strict-mcp-config")),
            Some("gemini") => assert!(env::var_os("GEMINI_CLI_SYSTEM_SETTINGS_PATH").is_some()),
            Some("opencode") => {
                assert_eq!(env::var("OPENCODE_PERMISSION").unwrap(), r#"{"*":"deny"}"#)
            }
            Some("antigravity") => {
                let directory = args
                    .iter()
                    .find_map(|arg| arg.strip_prefix("--gemini_dir="))
                    .expect("Antigravity discovery needs a private config directory");
                let settings = std::fs::read_to_string(
                    std::path::Path::new(directory).join("antigravity-cli/settings.json"),
                )
                .unwrap();
                assert!(
                    settings.contains("read_file(*)"),
                    "Discovery must deny reads"
                );
            }
            _ => panic!("Discovery cannot use Custom"),
        }
    }
    // The composed Integrator reads host-selected context only (ADR-032):
    // record what its working directory holds and the hardening it got.
    if stdin.contains("`integration_reviewer` worker")
        && let Ok(path) = env::current_exe()
    {
        let entries = std::fs::read_dir(".").map_or(0, |entries| entries.count());
        let _ = std::fs::write(
            path.with_file_name("integrator.fixture-scope"),
            format!(
                "entries={entries};project_files={};read_tools={}",
                std::path::Path::new("src").exists(),
                args.iter().any(|arg| arg == "Read,Glob,Grep")
            ),
        );
    }
    if let Some(backend) = official_backend {
        if stdin.contains("Create one new native Canvas explanation") {
            // A real CLI process in the host's empty scratch directory, with the
            // host's Canvas schema. No file or repository tool is needed.
            assert!(!std::path::Path::new("src").exists());
            let answer = if stdin.contains("canvas-batch") {
                serde_json::json!({"type":"tool_calls","calls":[{
                    "id":"finish","name":"canvas_finish",
                    "arguments":{"operation_id":"publish","expected_revision":1}
                }]})
            } else {
                serde_json::json!({"type":"tool_calls","calls":[{
                    "id":"layout","name":"canvas_apply_operations",
                    "arguments":{"operation_id":"canvas-batch","expected_revision":0,"operations":[
                        {"op":"upsert_node","id":"ui","kind":"component","label":"Interface","x":0,"y":0,"width":160,"height":80,"participant_id":null,"lane_id":null},
                        {"op":"upsert_node","id":"core","kind":"component","label":"Core","x":300,"y":0,"width":160,"height":80,"participant_id":null,"lane_id":null},
                        {"op":"connect","id":"calls","kind":"relationship","source_id":"ui","target_id":"core","label":"Comandos"},
                        {"op":"set_narrative","narrative":"A interface envia comandos ao Core."}
                    ]}
                },{"id":"validate","name":"canvas_validate","arguments":{}}]})
            };
            emit_companion_envelope(backend, answer);
            return;
        }
        if backend == "claude" && fixture_state == "synthetic_auth_error" {
            println!(
                r#"{{"type":"system","subtype":"init","session_id":"fixture-auth-session","model":"claude-haiku-4-5-20251001"}}"#
            );
            println!(
                r#"{{"type":"assistant","error":"authentication_failed","is_api_error_message":true,"message":{{"id":"fixture-auth-error","model":"<synthetic>","content":[{{"type":"text","text":"Failed to authenticate: OAuth session expired and could not be refreshed"}}]}}}}"#
            );
            println!(
                r#"{{"type":"result","is_error":true,"result":"Failed to authenticate: OAuth session expired and could not be refreshed"}}"#
            );
            std::process::exit(1);
        }
        if stdin.contains("<control_envelope_schema>") {
            if backend == "codex" {
                assert!(
                    args.iter().any(|arg| arg == "--ephemeral"),
                    "Actual Companion rounds must remain ephemeral"
                );
                let path = value_after(&args, "--output-schema").expect("Codex needs a schema");
                let schema: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
                assert_eq!(
                    schema["type"], "object",
                    "Codex rejects a union at the root"
                );
                assert_eq!(schema["additionalProperties"], false);
                assert!(schema["properties"]["response"]["anyOf"].is_array());
                if stdin.contains("companion-error-fixture") {
                    let provider = serde_json::json!({"error":{"message":"The requested model is not supported when using Codex with a ChatGPT account."}});
                    println!(
                        "{}",
                        serde_json::json!({"type":"turn.failed","error":{"message":provider.to_string()}})
                    );
                    std::process::exit(1);
                }
            }
            if stdin.contains("canvas-authoring-fixture") {
                emit_companion_envelope(
                    backend,
                    serde_json::json!({
                        "type":"final_with_canvas_offer",
                        "text":"canvas-authoring-fixture: A interface chama o Core.",
                        "offer":{"profile":"architecture","language":"pt-BR","title":"Arquitetura do sistema",
                            "request_text":"Posso explicar com o Canvas?"}
                    }),
                );
            } else {
                emit_companion_success(backend);
            }
        } else {
            emit_official_success(backend, &args);
        }
        return;
    }

    if let Some(model) = value_after(&args, "--fixture-model") {
        println!(
            "{}",
            serde_json::json!({
                "type": "hvc_model_attestation_v1",
                "model": model,
                "messageId": "custom-fixture-generation",
                "agentId": "custom-fixture-agent"
            })
        );
    }

    match mode {
        "success" => println!("fixture success"),
        // Composed-stage transport bridge (ADR-022): the prompt the host
        // wrote to stdin is forwarded to the test's scripted provider as a
        // chat request, and the provider's answer is printed verbatim. The
        // test thus scripts CLI replies with the same responder it uses for
        // the API transport, and can inspect exactly what reached the CLI.
        "bridge" => {
            let url = value_after(&args, "--bridge-url").expect("bridge requires --bridge-url");
            let system = between(&stdin, "<system>\n", "\n</system>").unwrap_or_default();
            let user = between(&stdin, "<user>\n", "\n</user>").unwrap_or_default();
            let body = serde_json::json!({
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": user}
                ],
                "bridge": {
                    "stdin_chars": stdin.chars().count(),
                    "schema_mentions": stdin.matches("Return only valid JSON that satisfies this schema").count(),
                    "argv": args,
                }
            })
            .to_string();
            let response = http_post(url, &body);
            let parsed: serde_json::Value =
                serde_json::from_str(&response).expect("bridge response JSON");
            let content = parsed["choices"][0]["message"]["content"]
                .as_str()
                .expect("bridge reply content");
            println!("{content}");
        }
        "stream" => {
            print!("first line\nfragment ");
            std::io::stdout().flush().unwrap();
            let utf8 = "final action\n".as_bytes();
            std::io::stdout().write_all(&utf8[..2]).unwrap();
            std::io::stdout().flush().unwrap();
            thread::sleep(Duration::from_millis(20));
            std::io::stdout().write_all(&utf8[2..]).unwrap();
            std::io::stdout().flush().unwrap();
            eprintln!("fixture stderr zero");
        }
        "paced" => {
            for _ in 0..10 {
                eprintln!("fixture progress");
                thread::sleep(Duration::from_millis(100));
            }
            println!("fixture completed after initial budget");
        }
        // A worker that computes silently - no byte on either pipe for
        // `--quiet-ms` - and then answers (ADR-025 observation trial).
        "quiet" => {
            let quiet = value_after(&args, "--quiet-ms")
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(1_000);
            thread::sleep(Duration::from_millis(quiet));
            println!("fixture completed after silence");
        }
        "hang" => loop {
            thread::sleep(Duration::from_secs(1));
        },
        "descendant" => {
            let heartbeat = heartbeat.expect("descendant requires --heartbeat");
            let child = env::current_exe().unwrap();
            let mut command = Command::new(child);
            command
                .args(["--mode", "child", "--heartbeat"])
                .arg(&heartbeat)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            #[cfg(windows)]
            command.creation_flags(0x0800_0000);
            let spawned = command.spawn().unwrap();
            println!("descendant_pid={}", spawned.id());
            std::io::stdout().flush().unwrap();
            loop {
                thread::sleep(Duration::from_secs(1));
            }
        }
        "exit" => {
            eprintln!(
                "{}",
                value_after(&args, "--stderr").unwrap_or("fixture failure")
            );
            let code = value_after(&args, "--code")
                .and_then(|value| value.parse::<i32>().ok())
                .unwrap_or(1);
            std::process::exit(code);
        }
        other => {
            eprintln!("unknown fixture mode: {other}");
            std::process::exit(2);
        }
    }
}

fn emit_companion_success(backend: &str) {
    emit_companion_envelope(
        backend,
        serde_json::json!({"type":"final","text":"Companion fixture response"}),
    );
}

fn emit_companion_envelope(backend: &str, response: serde_json::Value) {
    let envelope = response.to_string();
    match backend {
        "codex" => println!(
            "{}",
            serde_json::json!({"type":"item.completed","item":{
                "type":"agent_message", "text": serde_json::json!({"response":response}).to_string()
            }})
        ),
        "claude" => println!(
            "{}",
            serde_json::json!({"type":"result","result":envelope,"modelUsage":{"claude-fixture":{"inputTokens":8,"outputTokens":5,"cacheReadInputTokens":12}}})
        ),
        "gemini" => println!(
            "{}",
            serde_json::json!({"type":"message","content":envelope})
        ),
        "opencode" => println!(
            "{}",
            serde_json::json!({"type":"text","part":{"text":envelope}})
        ),
        "antigravity" => {
            println!(
                r#"{{"event":"init","conversation_id":"fixture-companion","init":{{"model":"gemini-fixture","permission_mode":"request-review"}}}}"#
            );
            println!(
                "{}",
                serde_json::json!({"event":"result","result":{"conversation_id":"fixture-companion","status":"SUCCESS","response":envelope,"usage":{"input_tokens":8,"output_tokens":5,"thinking_tokens":0,"cache_read_tokens":12,"total_tokens":13}}})
            );
        }
        _ => unreachable!(),
    }
    if backend == "codex" {
        println!(
            r#"{{"type":"turn.completed","usage":{{"input_tokens":20,"output_tokens":5,"cached_input_tokens":12}}}}"#
        );
    }
}

fn emit_official_success(backend: &str, args: &[String]) {
    let requested_model = value_after(args, "--model").unwrap_or("fixture-model");
    let session = ["resume", "--resume", "--session", "--conversation"]
        .into_iter()
        .find_map(|flag| value_after(args, flag))
        .unwrap_or("fixture-native-session");
    match backend {
        "codex" => {
            println!(r#"{{"type":"thread.started","thread_id":"{session}"}}"#);
            println!(
                r#"{{"type":"item.completed","model":"{requested_model}","item":{{"id":"codex-message-1","type":"agent_message","text":"ok codex fixture"}}}}"#
            );
            println!(
                r#"{{"type":"turn.completed","usage":{{"input_tokens":20,"output_tokens":5,"cached_input_tokens":12}}}}"#
            );
        }
        "claude" => {
            let observed_model = if fixture_model_mismatch() {
                "claude-sonnet-5"
            } else {
                "claude-haiku-4-5-20251001"
            };
            println!(
                r#"{{"type":"system","subtype":"init","session_id":"{session}","model":"{requested_model}"}}"#
            );
            if fixture_model_unverified() {
                println!(
                    r#"{{"type":"stream_event","agent_id":"subagent-1","event":{{"type":"message_start","parent_tool_use_id":"tool-parent-1","message":{{"id":"claude-message-1"}}}}}}"#
                );
                thread::sleep(Duration::from_secs(2));
                if let Ok(path) = env::current_exe() {
                    let _ = std::fs::write(
                        path.with_file_name("claude.fixture-after-mismatch"),
                        b"still-running",
                    );
                }
                return;
            }
            if fixture_subagent_mismatch() {
                println!(
                    r#"{{"type":"stream_event","agent_id":"subagent-1","event":{{"type":"message_start","parent_tool_use_id":"tool-parent-1","message":{{"id":"claude-message-1","model":"{observed_model}"}}}}}}"#
                );
                thread::sleep(Duration::from_secs(2));
                if let Ok(path) = env::current_exe() {
                    let _ = std::fs::write(
                        path.with_file_name("claude.fixture-after-mismatch"),
                        b"still-running",
                    );
                }
                return;
            }
            println!(
                r#"{{"type":"stream_event","event":{{"type":"message_start","message":{{"id":"claude-message-1","model":"{observed_model}"}}}}}}"#
            );
            if fixture_output_runaway() {
                // Two 20,000-token messages, then a long pause: a host that
                // does not stop the run at its output ceiling lets the marker appear.
                println!(
                    r#"{{"type":"stream_event","event":{{"type":"message_delta","usage":{{"output_tokens":20000}}}}}}"#
                );
                println!(
                    r#"{{"type":"stream_event","event":{{"type":"message_start","message":{{"id":"claude-message-2","model":"{observed_model}"}}}}}}"#
                );
                println!(
                    r#"{{"type":"stream_event","event":{{"type":"message_delta","usage":{{"output_tokens":20000}}}}}}"#
                );
                std::io::stdout().flush().unwrap();
                thread::sleep(Duration::from_secs(3));
                if let Ok(path) = env::current_exe() {
                    let _ = std::fs::write(
                        path.with_file_name("claude.fixture-after-mismatch"),
                        b"still-running",
                    );
                }
                return;
            }
            if fixture_model_mismatch() {
                thread::sleep(Duration::from_secs(2));
                if let Ok(path) = env::current_exe() {
                    let _ = std::fs::write(
                        path.with_file_name("claude.fixture-after-mismatch"),
                        b"still-running",
                    );
                }
                return;
            }
            println!(
                r#"{{"type":"assistant","message":{{"id":"claude-message-1","model":"{observed_model}","content":[{{"type":"text","text":"ok claude fixture"}}]}}}}"#
            );
            println!(
                r#"{{"type":"result","result":"ok claude fixture","session_id":"{session}","model":"{observed_model}","modelUsage":{{"claude-fixture":{{"inputTokens":8,"outputTokens":5,"cacheReadInputTokens":12}}}}}}"#
            );
        }
        "gemini" => {
            println!(r#"{{"type":"init","session_id":"{session}"}}"#);
            println!(
                r#"{{"type":"message","model":"{requested_model}","content":"ok gemini fixture"}}"#
            );
            println!(
                r#"{{"type":"result","result":"ok gemini fixture","stats":{{"models":{{"gemini-fixture":{{"input_tokens":20,"output_tokens":5,"total_tokens":25,"cached":12}}}}}}}}"#
            );
        }
        "opencode" => {
            println!(r#"{{"type":"session.created","sessionID":"{session}"}}"#);
            println!(
                r#"{{"type":"text","model":"{requested_model}","part":{{"id":"opencode-message-1","text":"ok opencode fixture"}}}}"#
            );
            println!(
                r#"{{"type":"step_finish","part":{{"id":"fixture-step","tokens":{{"input":8,"output":3,"reasoning":2,"cache":{{"read":12,"write":0}}}}}}}}"#
            );
            println!(
                r#"{{"type":"session.idle","sessionID":"{session}","result":"ok opencode fixture"}}"#
            );
        }
        "antigravity" => {
            println!(
                r#"{{"event":"init","conversation_id":"{session}","init":{{"model":"{requested_model}","permission_mode":"request-review"}}}}"#
            );
            println!(
                r#"{{"event":"step_update","step_update":{{"conversation_id":"{session}","step_index":1,"state":"DONE","step_type":"agent_response","text_delta":"ok antigravity fixture","usage":{{"input_tokens":8,"output_tokens":5,"thinking_tokens":0,"cache_read_tokens":12,"total_tokens":13}}}}}}"#
            );
            println!(
                r#"{{"event":"result","result":{{"conversation_id":"{session}","status":"SUCCESS","response":"ok antigravity fixture","num_turns":1,"usage":{{"input_tokens":8,"output_tokens":5,"thinking_tokens":0,"cache_read_tokens":12,"total_tokens":13}}}}}}"#
            );
        }
        _ => unreachable!(),
    }
}

fn fixture_model_mismatch() -> bool {
    env::current_exe()
        .ok()
        .and_then(|path| std::fs::read_to_string(path.with_file_name("claude.fixture-state")).ok())
        .is_some_and(|state| matches!(state.trim(), "sonnet_mismatch" | "subagent_sonnet_mismatch"))
}

fn fixture_model_unverified() -> bool {
    env::current_exe()
        .ok()
        .and_then(|path| std::fs::read_to_string(path.with_file_name("claude.fixture-state")).ok())
        .is_some_and(|state| state.trim() == "missing_generation_identity")
}

fn fixture_output_runaway() -> bool {
    env::current_exe()
        .ok()
        .and_then(|path| std::fs::read_to_string(path.with_file_name("claude.fixture-state")).ok())
        .is_some_and(|state| state.trim() == "output_runaway")
}

fn fixture_subagent_mismatch() -> bool {
    env::current_exe()
        .ok()
        .and_then(|path| std::fs::read_to_string(path.with_file_name("claude.fixture-state")).ok())
        .is_some_and(|state| state.trim() == "subagent_sonnet_mismatch")
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

/// Minimal HTTP/1.1 POST over a loopback socket; the fixture carries no HTTP
/// client dependency on purpose.
fn http_post(url: &str, body: &str) -> String {
    let rest = url
        .strip_prefix("http://")
        .expect("bridge URL must be plain http");
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let mut stream = std::net::TcpStream::connect(authority).expect("bridge connect");
    let request = format!(
        "POST /{path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("bridge write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("bridge read");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (headers, payload) = text
        .split_once("\r\n\r\n")
        .expect("bridge response headers");
    let chunked = headers.lines().any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("transfer-encoding") && value.trim().contains("chunked")
        })
    });
    if chunked {
        return decode_chunked(payload);
    }
    let length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    match length {
        Some(length) if payload.len() >= length => payload[..length].to_owned(),
        _ => payload.to_owned(),
    }
}

fn decode_chunked(payload: &str) -> String {
    let mut rest = payload;
    let mut out = String::new();
    while let Some((size_line, after)) = rest.split_once(
        "
",
    ) {
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        out.push_str(&after[..size]);
        rest = after[size..].trim_start_matches(
            "
",
        );
    }
    out
}

fn value_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].as_str())
}

fn heartbeat_loop(path: PathBuf) -> ! {
    loop {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "tick").unwrap();
        file.flush().unwrap();
        thread::sleep(Duration::from_millis(50));
    }
}
