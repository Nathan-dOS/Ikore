//! A stand-in for a project validation command (compile/test tool) driven by
//! its first argument, so the composition runner can be exercised without
//! a toolchain: exit codes, cargo-style diagnostics, timeouts, child
//! processes, literal argv, environment visibility and oversized output.
use std::io::Write;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("pass");
    match mode {
        "pass" => {
            println!("fixture ok");
        }
        "fail-json" => {
            // One cargo `--message-format=json` compiler message naming the
            // file in the second argument.
            let file = args.get(1).map(String::as_str).unwrap_or("src/lib.rs");
            println!(
                r#"{{"reason":"compiler-message","message":{{"level":"error","code":{{"code":"E0308"}},"message":"mismatched types","spans":[{{"file_name":"{file}","line_start":2,"is_primary":true}}]}}}}"#
            );
            std::process::exit(1);
        }
        "fail-unless" => {
            // Passes only when the file named by the second argument (relative
            // to the working directory, i.e. the candidate) contains the
            // marker in the third; otherwise one cargo-style diagnostic.
            let file = args.get(1).map(String::as_str).unwrap_or("src/lib.rs");
            let marker = args.get(2).map(String::as_str).unwrap_or("");
            let content = std::fs::read_to_string(file).unwrap_or_default();
            if content.contains(marker) {
                println!("fixture ok");
            } else {
                println!(
                    r#"{{"reason":"compiler-message","message":{{"level":"error","code":{{"code":"E0308"}},"message":"expected `{marker}`","spans":[{{"file_name":"{file}","line_start":1,"is_primary":true}}]}}}}"#
                );
                std::process::exit(1);
            }
        }
        "fail-text" => {
            eprintln!("error: fixture failure");
            std::process::exit(2);
        }
        "sleep" => {
            let seconds: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(30);
            std::thread::sleep(Duration::from_secs(seconds));
        }
        "spawn-child" => {
            // The child heartbeats into the file named by the second argument
            // until it is killed; the parent then sleeps as if hung.
            let heartbeat = args.get(1).cloned().unwrap_or_default();
            let exe = std::env::current_exe().expect("current exe");
            // Deliberately hung: the runner must terminate this whole tree.
            let mut child = std::process::Command::new(exe)
                .args(["heartbeat", &heartbeat])
                .spawn()
                .expect("spawn child");
            std::thread::sleep(Duration::from_secs(30));
            let _ = child.wait();
        }
        "heartbeat" => {
            let path = args.get(1).cloned().unwrap_or_default();
            loop {
                let _ = std::fs::write(&path, format!("{}", chrono::Utc::now().timestamp_millis()));
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        "echo-argv" => {
            for argument in &args[1..] {
                println!("{argument}");
            }
        }
        "dump-env" => {
            let mut names: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
            names.sort();
            for name in names {
                println!("{name}");
            }
        }
        "big-output" => {
            let bytes: usize = args
                .get(1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(4 * 1024 * 1024);
            let line = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde\n";
            let mut out = std::io::stdout().lock();
            let mut written = 0usize;
            while written < bytes {
                out.write_all(line.as_bytes()).expect("write");
                written += line.len();
            }
            out.flush().expect("flush");
        }
        "cwd" => {
            println!("{}", std::env::current_dir().expect("cwd").display());
        }
        other => {
            eprintln!("unknown fixture mode `{other}`");
            std::process::exit(64);
        }
    }
}
