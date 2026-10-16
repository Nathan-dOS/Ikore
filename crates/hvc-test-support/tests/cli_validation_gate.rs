//! A gated CLI (Gemini) installed above the adapter's validated ceiling parks the job on a CLI
//! fallback checkpoint before anything launches. "Update & validate" runs the CLI for real and
//! resumes the pulsation when the adapter contract still holds; a CLI that no longer answers the
//! adapter's flags downgrades the fallback so only switching provider or aborting remain.
mod composition_support;

use composition_support::*;
use hvc_core::persistence_service::{
    CLI_VALIDATION_FAILED, CLI_VERSION_UNVALIDATED, CliFallbackDecision, PulsationStatus,
    ResolveCliFallbackRequest, ValidateCliFallbackRequest,
};

static PATH_TEST_LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();

struct PathRestore(Option<std::ffi::OsString>);

impl Drop for PathRestore {
    fn drop(&mut self) {
        // SAFETY: serialized by PATH_TEST_LOCK and restored to the exact prior value.
        unsafe {
            if let Some(path) = self.0.take() {
                std::env::set_var("PATH", path);
            } else {
                std::env::remove_var("PATH");
            }
        }
    }
}

/// Puts a `cli_fixture` copy named `gemini.exe` first on PATH, in `state`.
fn install_gemini_fixture(state: &str) -> (tempfile::TempDir, PathRestore) {
    let binaries = tempfile::tempdir().unwrap();
    std::fs::copy(
        env!("CARGO_BIN_EXE_cli_fixture"),
        binaries.path().join("gemini.exe"),
    )
    .unwrap();
    std::fs::write(binaries.path().join("gemini.fixture-state"), state).unwrap();
    let original_path = std::env::var_os("PATH");
    let restore = PathRestore(original_path.clone());
    let mut paths = vec![binaries.path().to_path_buf()];
    if let Some(original_path) = original_path {
        paths.extend(std::env::split_paths(&original_path));
    }
    // SAFETY: guarded by PATH_TEST_LOCK and restored by `PathRestore`.
    unsafe { std::env::set_var("PATH", std::env::join_paths(paths).unwrap()) };
    (binaries, restore)
}

struct Fallback {
    checkpoint_id: String,
    error_code: String,
    diagnostic: String,
    state_version: u64,
}

fn wait_for_cli_fallback(harness: &Harness) -> Fallback {
    let mut found = None;
    wait_until(&harness.service, &harness.pulse, |db| {
        found = db
            .query_row(
                "SELECT c.id,f.error_code,f.diagnostic,p.state_version FROM checkpoints c JOIN cli_fallback_requests f ON f.checkpoint_id=c.id JOIN pulsations p ON p.id=c.pulsation_id WHERE p.id=?1 AND p.status='checkpoint' AND c.status='waiting' AND c.purpose='cli_fallback' AND f.decision IS NULL",
                [&harness.pulse],
                |r| {
                    Ok(Fallback {
                        checkpoint_id: r.get(0)?,
                        error_code: r.get(1)?,
                        diagnostic: r.get(2)?,
                        state_version: r.get(3)?,
                    })
                },
            )
            .ok();
        found.is_some()
    });
    found.unwrap()
}

fn current_fallback(harness: &Harness, checkpoint_id: &str) -> (String, String, Option<String>) {
    harness
        .db()
        .query_row(
            "SELECT c.status,f.error_code,f.decision FROM checkpoints c JOIN cli_fallback_requests f ON f.checkpoint_id=c.id WHERE c.id=?1",
            [checkpoint_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

fn pulsation_status(harness: &Harness) -> String {
    harness
        .db()
        .query_row(
            "SELECT status FROM pulsations WHERE id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap()
}

/// Lets Intake and Specification finish on the scripted API, routes the Contract role to the
/// Gemini fixture while the Specification review waits, then approves so the gate sees the
/// Contract job the moment it is claimed.
fn park_contract_on_gemini(harness: &Harness) -> Fallback {
    harness.wait_for_checkpoint("SPECIFICATION");
    harness.route_role_to_official_cli("contract_agent", "gemini");
    harness.approve();
    let fallback = wait_for_cli_fallback(harness);
    assert_eq!(fallback.error_code, CLI_VERSION_UNVALIDATED);
    assert!(
        fallback.diagnostic.contains("99.0.0"),
        "diagnostic names the installed version: {}",
        fallback.diagnostic
    );
    // Nothing was launched: no terminal run exists for the pulsation.
    let runs: i64 = harness
        .db()
        .query_row(
            "SELECT COUNT(*) FROM terminal_runs WHERE pulsation_id=?1",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(runs, 0);
    fallback
}

#[test]
fn gemini_above_the_validated_ceiling_parks_the_job_until_the_developer_validates_it() {
    let _lock = PATH_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (_binaries, _restore) = install_gemini_fixture("newer");
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    let fallback = park_contract_on_gemini(&harness);

    // "Update & validate": the fixture answers the adapter's flags with init + result frames.
    let service = harness.service.clone();
    let request = ValidateCliFallbackRequest {
        checkpoint_id: fallback.checkpoint_id.clone(),
        expected_state_version: fallback.state_version,
    };
    let response = harness
        .runtime_handle()
        .block_on(async move { service.validate_cli_fallback(request).await })
        .unwrap();
    assert!(response.report.validated, "{}", response.report.diagnostic);
    assert_eq!(response.report.version.as_deref(), Some("99.0.0"));
    let resumed = response
        .pulsation
        .expect("a passing probe resumes the pulsation");
    assert_ne!(resumed.status, PulsationStatus::Checkpoint);

    let (status, code, decision) = current_fallback(&harness, &fallback.checkpoint_id);
    assert_eq!(status, "approved");
    assert_eq!(code, CLI_VERSION_UNVALIDATED);
    assert_eq!(decision.as_deref(), Some("retry_cli"));
    let validated: String = harness
        .db()
        .query_row(
            "SELECT value FROM runtime_settings WHERE key='cli.validated_version.gemini'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(validated, "99.0.0");

    // The gate does not fire again for the validated version: the requeued job launches the
    // CLI (the fixture's prose reply is not a Specification, so the stage parks on a normal
    // stage-failure checkpoint — but a terminal run now exists).
    wait_until(&harness.service, &harness.pulse, |db| {
        db.query_row(
            "SELECT COUNT(*) FROM terminal_runs WHERE pulsation_id=?1",
            [&harness.pulse],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    });
    let fallbacks: i64 = harness
        .db()
        .query_row(
            "SELECT COUNT(*) FROM cli_fallback_requests WHERE pulsation_id=?1 AND decision IS NULL",
            [&harness.pulse],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        fallbacks, 0,
        "no second validation fallback for the same version"
    );
}

#[test]
fn a_cli_that_fails_validation_leaves_only_switching_provider_or_aborting() {
    let _lock = PATH_TEST_LOCK
        .get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (_binaries, _restore) = install_gemini_fixture("newer_broken");
    let harness = Harness::start(|facts, _, _| completed_reply(facts));
    let fallback = park_contract_on_gemini(&harness);

    let service = harness.service.clone();
    let request = ValidateCliFallbackRequest {
        checkpoint_id: fallback.checkpoint_id.clone(),
        expected_state_version: fallback.state_version,
    };
    let response = harness
        .runtime_handle()
        .block_on(async move { service.validate_cli_fallback(request).await })
        .unwrap();
    assert!(!response.report.validated);
    assert!(response.pulsation.is_none());
    assert!(
        response.report.diagnostic.contains("exit 7"),
        "{}",
        response.report.diagnostic
    );
    let (status, code, decision) = current_fallback(&harness, &fallback.checkpoint_id);
    assert_eq!(status, "waiting");
    assert_eq!(code, CLI_VALIDATION_FAILED);
    assert!(decision.is_none());
    assert_eq!(pulsation_status(&harness), "checkpoint");
    assert!(
        harness
            .db()
            .query_row(
                "SELECT COUNT(*) FROM runtime_settings WHERE key='cli.validated_version.gemini'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 0,
        "a failed probe validates nothing"
    );

    // A second attempt is still allowed (the developer may have fixed the install)...
    let service = harness.service.clone();
    let request = ValidateCliFallbackRequest {
        checkpoint_id: fallback.checkpoint_id.clone(),
        expected_state_version: fallback.state_version,
    };
    let again = harness
        .runtime_handle()
        .block_on(async move { service.validate_cli_fallback(request).await })
        .unwrap();
    assert!(!again.report.validated);

    // ...and switching the provider resumes the pulsation on the API transport.
    let resolved = harness
        .service
        .resolve_cli_fallback(ResolveCliFallbackRequest {
            checkpoint_id: fallback.checkpoint_id.clone(),
            expected_state_version: fallback.state_version,
            decision: CliFallbackDecision::UseApi,
        })
        .unwrap();
    assert_ne!(resolved.pulsation.status, PulsationStatus::Checkpoint);
    let (status, _, decision) = current_fallback(&harness, &fallback.checkpoint_id);
    assert_eq!(status, "approved");
    assert_eq!(decision.as_deref(), Some("use_api"));
}
