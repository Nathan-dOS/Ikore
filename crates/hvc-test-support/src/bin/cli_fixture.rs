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
    let official_backend = ["codex", "claude", "gemini", "opencode"]
        .into_iter()
        .find(|backend| executable_name == *backend);
    let fixture_state = official_backend
        .and_then(|backend| {
            env::var(format!(
                "HVC_CLI_FIXTURE_{}_STATE",
                backend.to_ascii_uppercase()
            ))
            .ok()
        })
        .unwrap_or_default();
    if args.iter().any(|arg| arg == "--version") {
        if fixture_state == "probe_failed" {
            eprintln!("fixture probe failed");
            std::process::exit(2);
        }
        if fixture_state == "unsupported" {
            println!("0.0.0-fixture");
        } else {
            println!(
                "{}",
                match official_backend.expect("backend fixture must be known") {
                    "codex" => "codex-cli 0.153.0-fixture",
                    "claude" => "2.1.81 (Claude Code)",
                    "gemini" => "0.59.0",
                    "opencode" => "1.18.17",
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
    if stdin.contains("feature-discovery-fixture") {
        assert!(
            !std::path::Path::new("src/lib.rs").exists(),
            "Discovery must only receive host-selected context"
        );
        match official_backend {
            Some("codex") => assert!(args.iter().any(|s| s == "--ignore-rules")),
            Some("claude") => assert!(args.iter().any(|s| s == "--strict-mcp-config")),
            Some("gemini") => assert!(env::var_os("GEMINI_CLI_SYSTEM_SETTINGS_PATH").is_some()),
            Some("opencode") => {
                assert_eq!(env::var("OPENCODE_PERMISSION").unwrap(), r#"{"*":"deny"}"#)
            }
            _ => panic!("Discovery cannot use Custom"),
        }
    }
    if let Some(backend) = official_backend {
        if stdin.contains("<control_envelope_schema>") {
            if backend == "codex" {
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
            emit_companion_success(backend);
        } else {
            emit_official_success(backend, &args);
        }
        return;
    }

    match mode {
        "success" => println!("fixture success"),
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
    let envelope = r#"{\"type\":\"final\",\"text\":\"Companion fixture response\"}"#;
    match backend {
        "codex" => println!(
            "{}",
            serde_json::json!({"type":"item.completed","item":{
                "type":"agent_message", "text": serde_json::json!({"response":{
                    "type":"final","text":"Companion fixture response"
                }}).to_string()
            }})
        ),
        "claude" => println!(
            r#"{{"type":"result","result":"{envelope}","modelUsage":{{"claude-fixture":{{"inputTokens":8,"outputTokens":5,"cacheReadInputTokens":12}}}}}}"#
        ),
        "gemini" => println!(r#"{{"type":"message","content":"{envelope}"}}"#),
        "opencode" => println!(r#"{{"type":"text","part":{{"text":"{envelope}"}}}}"#),
        _ => unreachable!(),
    }
    if backend == "codex" {
        println!(
            r#"{{"type":"turn.completed","usage":{{"input_tokens":20,"output_tokens":5,"cached_input_tokens":12}}}}"#
        );
    }
}

fn emit_official_success(backend: &str, args: &[String]) {
    let session = ["resume", "--resume", "--session"]
        .into_iter()
        .find_map(|flag| value_after(args, flag))
        .unwrap_or("fixture-native-session");
    match backend {
        "codex" => {
            println!(r#"{{"type":"thread.started","thread_id":"{session}"}}"#);
            println!(
                r#"{{"type":"item.completed","item":{{"type":"agent_message","text":"ok codex fixture"}}}}"#
            );
            println!(
                r#"{{"type":"turn.completed","usage":{{"input_tokens":20,"output_tokens":5,"cached_input_tokens":12}}}}"#
            );
        }
        "claude" => {
            println!(r#"{{"type":"system","subtype":"init","session_id":"{session}"}}"#);
            println!(
                r#"{{"type":"result","result":"ok claude fixture","session_id":"{session}","modelUsage":{{"claude-fixture":{{"inputTokens":8,"outputTokens":5,"cacheReadInputTokens":12}}}}}}"#
            );
        }
        "gemini" => {
            println!(r#"{{"type":"init","session_id":"{session}"}}"#);
            println!(
                r#"{{"type":"result","result":"ok gemini fixture","stats":{{"models":{{"gemini-fixture":{{"input_tokens":20,"output_tokens":5,"total_tokens":25,"cached":12}}}}}}}}"#
            );
        }
        "opencode" => {
            println!(r#"{{"type":"session.created","sessionID":"{session}"}}"#);
            println!(
                r#"{{"type":"step_finish","part":{{"id":"fixture-step","tokens":{{"input":8,"output":3,"reasoning":2,"cache":{{"read":12,"write":0}}}}}}}}"#
            );
            println!(
                r#"{{"type":"session.idle","sessionID":"{session}","result":"ok opencode fixture"}}"#
            );
        }
        _ => unreachable!(),
    }
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
