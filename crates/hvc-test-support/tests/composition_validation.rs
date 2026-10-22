//! The host validation runner (ADR-022, M4; SC-011, SC-023): direct argv
//! launch of a fixture command against a disposable candidate, bounded
//! logs, reduced environment, timeout and process-tree termination, and
//! receipts whose status is never inferred.
use hvc_core::orchestration::SchemaV1;
use hvc_core::orchestration::composition::validation::{
    ReceiptContext, materialize_candidate, profile::ProfileOrigin, resolve_profile, run_check,
};
use hvc_core::orchestration::composition::{
    CheckKind, Handle, ReceiptStatus, RelPath, Uuid, ValidationCommandProfileV1,
};
use std::path::Path;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const FIXTURE: &str = env!("CARGO_BIN_EXE_check_fixture");

fn profile(argv: &[&str], timeout_seconds: u32, max_log_bytes: u32) -> ValidationCommandProfileV1 {
    ValidationCommandProfileV1 {
        schema_version: SchemaV1,
        profile_id: Uuid::generate(),
        profile_version: 1,
        check_ref: Handle::new("fixture_check").unwrap(),
        kind: CheckKind::Custom,
        required: true,
        executable: FIXTURE.to_owned(),
        argv: argv.iter().map(|s| (*s).to_owned()).collect(),
        cwd_relative_path: ".".into(),
        input_globs: vec!["**/*".into()],
        environment_names: vec!["HVC_FIXTURE_ALLOWED".into()],
        toolchain_identity: "fixture".into(),
        timeout_seconds,
        max_log_bytes,
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    _data: tempfile::TempDir,
    candidate: hvc_core::orchestration::composition::validation::Candidate,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(root.path().join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let candidate = materialize_candidate(
        root.path(),
        &[(
            RelPath::new("src/lib.rs").unwrap(),
            "pub fn a() -> u8 { 1 }\n".to_owned(),
        )],
        &data.path().join("build"),
    )
    .unwrap();
    Fixture {
        _root: root,
        _data: data,
        candidate,
    }
}

fn context() -> ReceiptContext {
    ReceiptContext {
        stage_execution_id: Uuid::generate(),
        parent_generation: 1,
        profile_snapshot_id: Uuid::generate(),
        extra_environment: vec![("HVC_FIXTURE_CACHE".into(), "cache".into())],
    }
}

fn resolver(path: &str) -> Option<Handle> {
    path.replace('\\', "/")
        .ends_with("src/lib.rs")
        .then(|| Handle::new("src_1").unwrap())
}

async fn run(
    argv: &[&str],
    timeout: u32,
    max_log: u32,
) -> hvc_core::orchestration::composition::ValidationReceiptV1 {
    let fixture = fixture();
    let snapshot =
        resolve_profile(&profile(argv, timeout, max_log), ProfileOrigin::Custom).unwrap();
    let receipt = run_check(
        &snapshot,
        &fixture.candidate,
        &context(),
        &resolver,
        &CancellationToken::new(),
    )
    .await;
    receipt.check_execution_semantics().unwrap();
    receipt
}

#[tokio::test]
async fn a_passing_command_yields_a_passed_receipt_bound_to_the_candidate() {
    let fixture = fixture();
    let snapshot = resolve_profile(&profile(&["pass"], 30, 65_536), ProfileOrigin::Custom).unwrap();
    let receipt = run_check(
        &snapshot,
        &fixture.candidate,
        &context(),
        &resolver,
        &CancellationToken::new(),
    )
    .await;
    receipt.check_execution_semantics().unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Passed);
    assert_eq!(receipt.exit_code, Some(0));
    assert!(receipt.stdout.text.contains("fixture ok"));
    assert!(!receipt.stdout.truncated);
    assert!(receipt.diagnostics.is_empty());
    assert_eq!(receipt.candidate_sha256, fixture.candidate.candidate_sha256);
    assert_eq!(receipt.profile_sha256, snapshot.profile_sha256);
    assert_eq!(receipt.toolchain_sha256, snapshot.toolchain_sha256);
    assert!(receipt.started_at.is_some() && receipt.finished_at.is_some());
    // The command ran inside the candidate, not the live root.
    let cwd = run(&["cwd"], 30, 65_536).await;
    assert!(
        cwd.stdout.text.trim().ends_with("build"),
        "{}",
        cwd.stdout.text
    );
}

#[tokio::test]
async fn failures_carry_parsed_or_exit_diagnostics_and_never_pass() {
    let json = run(&["fail-json", "src/lib.rs"], 30, 65_536).await;
    assert_eq!(json.status, ReceiptStatus::Failed);
    assert_eq!(json.exit_code, Some(1));
    // Generic parsing (custom profile): the JSON line still names an error.
    assert!(!json.diagnostics.is_empty());
    let text = run(&["fail-text"], 30, 65_536).await;
    assert_eq!(text.status, ReceiptStatus::Failed);
    assert_eq!(text.exit_code, Some(2));
    assert!(
        text.diagnostics
            .iter()
            .any(|d| d.message.contains("fixture failure"))
    );
    // A silent nonzero exit still produces one diagnostic.
    let unknown = run(&["no-such-mode"], 30, 65_536).await;
    assert_eq!(unknown.status, ReceiptStatus::Failed);
    assert_eq!(unknown.exit_code, Some(64));
    assert!(!unknown.diagnostics.is_empty());
}

#[tokio::test]
async fn argv_is_literal_and_the_environment_is_reduced() {
    // SAFETY: test-local variables read only by this test's child process.
    unsafe {
        std::env::set_var("HVC_FIXTURE_ALLOWED", "yes");
        std::env::set_var("HVC_FIXTURE_SECRET", "must-not-leak");
    }
    let hostile = "\"; rm -rf / & echo %PATH% $HOME `id` |";
    let echoed = run(&["echo-argv", hostile, "--flag=a b"], 30, 65_536).await;
    assert_eq!(echoed.status, ReceiptStatus::Passed);
    let lines = echoed.stdout.text.lines().collect::<Vec<_>>();
    assert_eq!(
        lines,
        vec![hostile, "--flag=a b"],
        "argv reaches the process byte for byte"
    );
    let env = run(&["dump-env"], 30, 65_536).await;
    let names = env.stdout.text.lines().collect::<Vec<_>>();
    assert!(
        names.contains(&"HVC_FIXTURE_ALLOWED"),
        "selected project variables are forwarded"
    );
    assert!(
        names.contains(&"HVC_FIXTURE_CACHE"),
        "host build-cache variables are forwarded"
    );
    assert!(names.contains(&"PATH"));
    assert!(
        !names.contains(&"HVC_FIXTURE_SECRET"),
        "unselected host variables never reach the command"
    );
    assert!(
        !names
            .iter()
            .any(|n| n.starts_with("OPENAI") || n.starts_with("ANTHROPIC"))
    );
}

#[tokio::test]
async fn oversized_output_is_capped_and_marked_truncated_without_deadlock() {
    let receipt = tokio::time::timeout(
        Duration::from_secs(20),
        run(&["big-output", "4194304"], 60, 16_384),
    )
    .await
    .expect("the runner must drain all 4 MiB after reaching its retained-log limit");
    assert_eq!(
        receipt.status,
        ReceiptStatus::Error,
        "an incomplete observation cannot qualify a successful validation"
    );
    assert_eq!(receipt.exit_code, Some(0), "the process finishes draining");
    assert!(receipt.stdout.truncated);
    assert_eq!(
        receipt.stdout.text,
        "[output withheld: validation log limit exceeded]"
    );
    assert!(!receipt.stdout.text.contains("0123456789abcdef"));
    assert!(receipt.stdout.text.len() <= 16_384);
    assert!(!receipt.stderr.truncated);
    assert!(receipt.stderr.text.is_empty());
    assert!(
        receipt
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("evidence is incomplete"))
    );
}

#[tokio::test]
async fn a_hung_command_and_its_children_are_terminated_on_timeout() {
    let heartbeat = tempfile::tempdir().unwrap();
    let path = heartbeat.path().join("beat");
    let path_text = path.to_string_lossy().into_owned();
    let started = std::time::Instant::now();
    let receipt = run(&["spawn-child", &path_text], 2, 65_536).await;
    assert_eq!(receipt.status, ReceiptStatus::TimedOut);
    assert!(receipt.exit_code.is_none());
    assert!(started.elapsed() < Duration::from_secs(20));
    assert!(
        receipt
            .diagnostics
            .iter()
            .any(|d| d.message.contains("process tree was terminated"))
    );
    // The grandchild stopped heart-beating: the whole tree died with the job.
    let read = |p: &Path| {
        std::fs::read_to_string(p)
            .ok()
            .and_then(|t| t.trim().parse::<i64>().ok())
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    let first = read(&path);
    tokio::time::sleep(Duration::from_millis(700)).await;
    let second = read(&path);
    assert_eq!(first, second, "the child keeps writing if it survived");
}

#[tokio::test]
async fn cancellation_stops_the_command_and_a_missing_executable_is_unavailable() {
    let fixture = fixture();
    let snapshot = resolve_profile(
        &profile(&["sleep", "30"], 60, 65_536),
        ProfileOrigin::Custom,
    )
    .unwrap();
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });
    let started = std::time::Instant::now();
    let receipt = run_check(
        &snapshot,
        &fixture.candidate,
        &context(),
        &resolver,
        &cancellation,
    )
    .await;
    receipt.check_execution_semantics().unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(10));

    let mut missing = profile(&["pass"], 30, 65_536);
    missing.executable = "hvc-tool-that-does-not-exist".into();
    let snapshot = resolve_profile(&missing, ProfileOrigin::Custom).unwrap();
    let receipt = run_check(
        &snapshot,
        &fixture.candidate,
        &context(),
        &resolver,
        &CancellationToken::new(),
    )
    .await;
    receipt.check_execution_semantics().unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Unavailable);
    assert!(receipt.started_at.is_none() && receipt.exit_code.is_none());
    assert!(receipt.diagnostics[0].message.contains("installs nothing"));
}
