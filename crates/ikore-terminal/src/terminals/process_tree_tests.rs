// Included inside process_tree::win::tests; no new crate or external executable.
use super::*;
use std::io::{BufRead as _, Write as _};
use std::os::windows::io::BorrowedHandle;
use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use windows_sys::Win32::System::JobObjects::IsProcessInJob;
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, SuspendThread};

const MODE: &str = "IKORE_PROCESS_TREE_TEST_MODE";
const MARKER: &str = "IKORE_PROCESS_TREE_TEST_MARKER";
const VALUE: &str = "IKORE_PROCESS_TREE_TEST_VALUE";
const PREFIX: &str = "IKORE_PROCESS_TREE_FIXTURE ";
const DEADLINE: Duration = Duration::from_secs(15);

fn fixture_name() -> String {
    // libtest's name omits the crate component of module_path!().
    let (_, module) = module_path!().split_once("::").expect("nested test module");
    format!("{module}::subprocess_fixture")
}

fn fixture_command(mode: &str, marker: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().expect("test executable"));
    command.args([
        "--exact",
        &fixture_name(),
        "--nocapture",
        "--test-threads=1",
    ]);
    command.env_clear();
    // Only the Windows loader's non-secret root is inherited. No user profile,
    // provider credential, PATH search or network is used by these fixtures.
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command.env(MODE, mode).env(MARKER, marker);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn duplicate_child(child: &Child) -> OwnedHandle {
    let process = child.raw_handle().expect("owned live process handle");
    // SAFETY: Child owns the handle throughout duplication; BorrowedHandle does
    // not close it. try_clone_to_owned creates a separate owner for observation.
    unsafe { BorrowedHandle::borrow_raw(process) }
        .try_clone_to_owned()
        .expect("duplicate process")
}

fn assert_exited(process: &OwnedHandle) {
    // SAFETY: the independently owned process handle remains valid, with wait
    // access. A bounded kernel wait verifies exit, rather than assuming a drop.
    let state = unsafe { WaitForSingleObject(raw(process), 5_000) };
    assert_eq!(
        state, WAIT_OBJECT_0,
        "process exit must be confirmed by its retained handle"
    );
}

fn control_error(result: Result<(Child, ProcessTreeJob), ProcessTreeSpawnError>) -> io::Error {
    match result {
        Err(ProcessTreeSpawnError::Control(error)) => error,
        Err(other) => panic!("expected control failure, got {other}"),
        Ok((_child, _job)) => panic!("expected fail-closed control error"),
    }
}

async fn record(reader: &mut BufReader<tokio::process::ChildStdout>) -> serde_json::Value {
    tokio::time::timeout(DEADLINE, async {
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(
                reader.read_line(&mut line).await.expect("read fixture"),
                0,
                "fixture exited without its readiness record"
            );
            // libtest may print its test-name prefix on this same line.
            if let Some(index) = line.find(PREFIX) {
                return serde_json::from_str(&line[index + PREFIX.len()..]).expect("fixture JSON");
            }
        }
    })
    .await
    .expect("fixture readiness deadline")
}

fn write_record(value: serde_json::Value) {
    let mut output = std::io::stdout().lock();
    writeln!(output, "{PREFIX}{value}").expect("write fixture record");
    output.flush().expect("flush fixture record");
}

fn await_release() {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .expect("fixture stdin");
}

#[test]
fn subprocess_fixture() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    let marker = PathBuf::from(std::env::var_os(MARKER).expect("fixture marker"));
    std::fs::write(&marker, b"user code executed").expect("write marker");
    if mode == "immediate" {
        return;
    }
    if mode == "tree" {
        let mut leaf =
            std::process::Command::new(std::env::current_exe().expect("test executable"));
        leaf.creation_flags(CREATE_NO_WINDOW);
        leaf.args([
            "--exact",
            &fixture_name(),
            "--nocapture",
            "--test-threads=1",
        ]);
        leaf.env(MODE, "leaf")
            .env(MARKER, marker.with_extension("leaf"));
        leaf.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut descendant = leaf.spawn().expect("spawn descendant fixture");
        let output = descendant.stdout.take().expect("descendant stdout");
        let mut output = std::io::BufReader::new(output);
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(output.read_line(&mut line).expect("read descendant"), 0);
            if line.contains(PREFIX) {
                break;
            }
        }
        write_record(serde_json::json!({"descendant_pid": descendant.id()}));
        await_release();
        drop(descendant.stdin.take());
        assert!(
            descendant
                .wait()
                .expect("wait released descendant")
                .success()
        );
        std::fs::write(marker.with_extension("completed"), b"normal completion")
            .expect("write completion marker");
        return;
    }
    write_record(serde_json::json!({
        "argv": std::env::args().collect::<Vec<_>>(),
        "cwd": std::env::current_dir().expect("fixture cwd"),
        "value": std::env::var(VALUE).ok(),
        "pid": std::process::id(),
    }));
    writeln!(std::io::stderr(), "IKORE_FIXTURE_STDERR").expect("fixture stderr");
    await_release();
}

#[tokio::test]
async fn controlled_spawn_preserves_command_and_proves_suspended_initial_thread() {
    let directory = tempfile::tempdir().expect("isolated fixture directory");
    let marker = directory.path().join("executed");
    let mut command = fixture_command("leaf", &marker);
    let odd_argument = "spaces Ω quote\" trailing\\";
    // --skip is a valid libtest option and carries this exact arbitrary value.
    command.args(["--skip", odd_argument]);
    command
        .current_dir(directory.path())
        .env(VALUE, "synthetic value Ω");
    let (mut child, job) = ProcessTreeJob::spawn_inner(&mut command, |child, job, thread| {
        // SAFETY: the helper supplies an owned handle to this child's sole
        // thread with SUSPEND_RESUME rights. This temporary extra suspension is
        // balanced immediately, before the helper performs the actual release.
        let previous = unsafe { SuspendThread(raw(thread)) };
        if previous == u32::MAX {
            return Err(last_error("SuspendThread(test probe)"));
        }
        // SAFETY: same owned thread; removes only the suspension just added.
        let balanced = unsafe { ResumeThread(raw(thread)) };
        assert_eq!(
            balanced,
            previous + 1,
            "test probe must balance its own suspension"
        );
        assert_eq!(
            previous, 1,
            "child must already be suspended before user code can run"
        );
        let mut assigned = 0;
        // SAFETY: both process and job handles remain owned by spawn_inner;
        // assigned points to initialized storage for this synchronous query.
        let queried =
            unsafe { IsProcessInJob(process_handle(child)?, raw(&job.handle), &mut assigned) };
        assert_ne!(queried, 0);
        assert_ne!(
            assigned, 0,
            "specific KILL_ON_JOB_CLOSE job precedes thread release"
        );
        assert!(
            !marker.exists(),
            "fixture cannot have run while primary is suspended"
        );
        Ok(())
    })
    .expect("controlled spawn");
    let mut output = BufReader::new(child.stdout.take().expect("stdout preserved"));
    let value = record(&mut output).await;
    assert_eq!(value["value"], "synthetic value Ω");
    assert_eq!(
        std::fs::canonicalize(value["cwd"].as_str().expect("cwd string"))
            .expect("canonical observed cwd"),
        std::fs::canonicalize(directory.path()).expect("canonical fixture cwd")
    );
    assert!(
        value["argv"]
            .as_array()
            .expect("argv array")
            .iter()
            .any(|arg| arg == odd_argument)
    );
    assert!(
        marker.exists(),
        "fixture runs after the helper releases the thread"
    );
    child
        .stdin
        .as_mut()
        .expect("stdin preserved")
        .write_all(b"release\n")
        .await
        .expect("write stdin");
    let status = tokio::time::timeout(DEADLINE, child.wait())
        .await
        .expect("exit deadline")
        .expect("wait");
    assert!(status.success());
    let mut error_output = String::new();
    let mut stderr = child.stderr.take().expect("stderr preserved");
    tokio::time::timeout(DEADLINE, stderr.read_to_string(&mut error_output))
        .await
        .expect("stderr deadline")
        .expect("read stderr");
    assert!(error_output.contains("IKORE_FIXTURE_STDERR"));
    let _ = job
        .observation()
        .expect("job observation remains available after child exit");
}

#[tokio::test]
async fn immediate_success_exits_without_assignment_race() {
    let directory = tempfile::tempdir().expect("isolated directory");
    let marker = directory.path().join("immediate");
    let (mut child, _job) = ProcessTreeJob::spawn(&mut fixture_command("immediate", &marker))
        .expect("controlled immediate child");
    assert!(
        tokio::time::timeout(DEADLINE, child.wait())
            .await
            .expect("exit deadline")
            .expect("wait")
            .success()
    );
    assert!(marker.exists());
}

#[tokio::test]
async fn before_resume_error_confirms_exit_without_running_user_code() {
    let directory = tempfile::tempdir().expect("isolated directory");
    let marker = directory.path().join("must-not-execute");
    let mut process = None;
    let error = control_error(ProcessTreeJob::spawn_inner(
        &mut fixture_command("immediate", &marker),
        |child, _, _| {
            process = Some(duplicate_child(child));
            Err(io::Error::from_raw_os_error(5))
        },
    ));
    assert!(error.to_string().contains("before_resume"));
    assert_eq!(
        ProcessTreeSpawnError::Control(error).raw_os_error(),
        Some(5)
    );
    assert_exited(process.as_ref().expect("observed child"));
    assert!(
        !marker.exists(),
        "exit is confirmed before checking absence of execution"
    );
}

#[tokio::test]
async fn before_resume_panic_confirms_exit_without_running_user_code() {
    let directory = tempfile::tempdir().expect("isolated directory");
    let marker = directory.path().join("must-not-execute");
    let mut process = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = ProcessTreeJob::spawn_inner(
            &mut fixture_command("immediate", &marker),
            |child, _, _| {
                process = Some(duplicate_child(child));
                panic!("injected before-resume panic");
            },
        );
    }));
    assert!(result.is_err());
    assert_exited(process.as_ref().expect("observed child"));
    assert!(!marker.exists());
}

#[tokio::test]
async fn unexpected_resume_count_fails_closed_and_confirms_exit() {
    let directory = tempfile::tempdir().expect("isolated directory");
    let marker = directory.path().join("must-not-execute");
    let mut process = None;
    let error = control_error(ProcessTreeJob::spawn_inner(
        &mut fixture_command("immediate", &marker),
        |child, _, thread| {
            process = Some(duplicate_child(child));
            // SAFETY: test deliberately retains one extra suspension on this owned
            // child. The helper must reject ResumeThread's previous count of two.
            assert_eq!(unsafe { SuspendThread(raw(thread)) }, 1);
            Ok(())
        },
    ));
    assert!(error.to_string().contains("suspend count 2"));
    assert_exited(process.as_ref().expect("observed child"));
    assert!(!marker.exists());
}

#[tokio::test]
async fn pending_child_drop_before_assignment_confirms_exit() {
    let directory = tempfile::tempdir().expect("isolated directory");
    let marker = directory.path().join("must-not-execute");
    let mut command = fixture_command("immediate", &marker);
    command
        .kill_on_drop(true)
        .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    let child = command.spawn().expect("create own suspended fixture");
    let process = duplicate_child(&child);
    drop(PendingChild(Some(child)));
    assert_exited(&process);
    assert!(!marker.exists());
}

#[tokio::test]
async fn dropping_job_confirms_child_and_descendant_exit() {
    let directory = tempfile::tempdir().expect("isolated directory");
    let marker = directory.path().join("tree");
    let (mut child, job) =
        ProcessTreeJob::spawn(&mut fixture_command("tree", &marker)).expect("controlled tree");
    let process = duplicate_child(&child);
    let mut output = BufReader::new(child.stdout.take().expect("stdout"));
    let value = record(&mut output).await;
    let pid = u32::try_from(value["descendant_pid"].as_u64().expect("descendant PID"))
        .expect("PID range");
    // SAFETY: this PID was emitted by our own live child only after its own
    // descendant sent readiness. Open only non-inheritable wait access, before
    // closing the job; the owning parent keeps the descendant alive meanwhile.
    let descendant = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(
        !descendant.is_null(),
        "open own live descendant for exit confirmation"
    );
    // SAFETY: non-null newly opened handle has exactly this owner.
    let descendant = unsafe { OwnedHandle::from_raw_handle(descendant.cast()) };
    // SAFETY: independently owned live process handles support zero-time wait.
    let parent_state = unsafe { WaitForSingleObject(raw(&process), 0) };
    assert_eq!(parent_state, WAIT_TIMEOUT);
    // SAFETY: independently owned descendant handle supports zero-time wait.
    let descendant_state = unsafe { WaitForSingleObject(raw(&descendant), 0) };
    assert_eq!(descendant_state, WAIT_TIMEOUT);
    drop(job);
    assert_exited(&process);
    assert_exited(&descendant);
    let _status = tokio::time::timeout(DEADLINE, child.wait())
        .await
        .expect("wait deadline")
        .expect("wait");
    assert!(
        !marker.with_extension("completed").exists(),
        "the Job ended both live processes before any normal completion; exit code alone is not evidence"
    );
}

#[tokio::test]
async fn creation_failure_keeps_spawn_classification_and_io_conversion() {
    let directory = tempfile::tempdir().expect("isolated directory");
    let missing = directory.path().join("deliberately-missing-fixture.exe");
    let error = match ProcessTreeJob::spawn(&mut Command::new(missing)) {
        Err(error @ ProcessTreeSpawnError::Spawn(_)) => error,
        Err(other) => panic!("creation failure misclassified: {other}"),
        Ok((_child, _job)) => panic!("missing executable unexpectedly spawned"),
    };
    assert!(error.raw_os_error().is_some());
    let converted: io::Error = error.into();
    assert_eq!(converted.kind(), io::ErrorKind::NotFound);
    assert!(matches!(
        converted
            .get_ref()
            .and_then(|error| error.downcast_ref::<ProcessTreeSpawnError>()),
        Some(ProcessTreeSpawnError::Spawn(_))
    ));
}
