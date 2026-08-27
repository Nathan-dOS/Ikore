use std::fs;
use std::time::Duration;

use hvc_core::agent_runtime::{AgentRuntimeConfig, StageExecutionInput, execute_stage};
use hvc_core::orchestration::{
    ContentSha256, ContractError, ExecutionFence, ExpectedFileContent, PublicationReplayDecision,
    RelativeSourcePath, SourceSnapshot, publication_replay_decision, validate_result_acceptance,
};
use hvc_core::persistence_service::JobStatus;
use hvc_core::repository_context::{ReadRepositoryFileArgs, RepositoryContext};
use hvc_test_support::{
    ControlledLlmGateway, FixedEmbeddingGateway, LegacyPulseLayout, PulsationFixture,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn input(fixture: &PulsationFixture) -> StageExecutionInput {
    StageExecutionInput {
        handoff: None,
        job_id: "job-pulse-a".into(),
        pulsation_id: "pulse-a".into(),
        project_id: "project-a".into(),
        feature_name: "Descrever a condição de acesso".into(),
        stage_name: "SPECIFICATION".into(),
        workspace_path: fixture.project_a_root().to_string_lossy().into_owned(),
        runtime_db_path: fixture.database_path().to_string_lossy().into_owned(),
    }
}

fn valid_output() -> String {
    json!({
        "summary":"Specification fixture", "statusUpdate":"checkpoint_ready", "notes":[],
        "artifacts":[{"name":"spec","kind":"specification","content":"# Specification fixture\n"}]
    })
    .to_string()
}

fn fence() -> ExecutionFence {
    ExecutionFence {
        project_id: "project-a".into(),
        pulsation_id: Some("pulse-a".into()),
        input_revision: Some(1),
        job_id: "job-pulse-a".into(),
        generation: 1,
        state_version: 1,
    }
}

#[test]
fn v9_fixtures_preserve_equal_timestamps_and_project_isolation() {
    for (layout, expected) in [
        (LegacyPulseLayout::TwoQueued, 2),
        (LegacyPulseLayout::ThreeQueued, 3),
    ] {
        let fixture = PulsationFixture::new(layout);
        let connection = fixture.connection();
        let rows: (i64, i64) = connection.query_row(
            "SELECT COUNT(*), COUNT(DISTINCT created_at) FROM pulsations WHERE project_id='project-a'",
            [], |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(rows, (expected, 1));
        let other: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pulsations WHERE project_id='project-b'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(other, 1);
        let version: i64 = connection
            .query_row(
                "SELECT MAX(version) FROM refinery_schema_history",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, 9);
        assert_ne!(fixture.project_a_root(), fixture.project_b_root());
    }
}

#[test]
fn v9_fixtures_retain_active_checkpoints_and_legacy_artifact_content() {
    for (layout, expected) in [
        (LegacyPulseLayout::OneActive, 1),
        (LegacyPulseLayout::MultipleActive, 2),
    ] {
        let fixture = PulsationFixture::new(layout);
        let connection = fixture.connection();
        let active: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM checkpoints WHERE status='waiting'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(active, expected);
        let content: String = connection
            .query_row(
                "SELECT structured_output FROM agent_run_outputs WHERE pulsation_id='pulse-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["artifacts"][0]["content"],
            "# Legacy specification\n"
        );
        let new_tables: i64 = connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE name IN ('project_pipeline_state','artifact_publications','brownfield_ast_symbols')", [], |row| row.get(0)).unwrap();
        assert_eq!(new_tables, 0, "M0 must not create the future schema");
    }
}

#[test]
fn existing_safe_reader_handles_code_without_vault_and_sources_stay_project_scoped() {
    let fixture = PulsationFixture::new(LegacyPulseLayout::TwoQueued);
    let mut snapshots = Vec::new();
    for (project, root) in [
        ("project-a", fixture.project_a_root()),
        ("project-b", fixture.project_b_root()),
    ] {
        assert!(!root.join("knowledge").exists());
        let reader = RepositoryContext::new(&root).unwrap();
        let result = reader
            .read_file(ReadRepositoryFileArgs {
                path: "src/auth.rs".into(),
                start_line: Some(1),
                end_line: Some(1),
            })
            .unwrap();
        assert!(result.content.contains("pub fn allowed"));
        let bytes = fs::read_to_string(root.join("src/auth.rs")).unwrap();
        snapshots.push(
            SourceSnapshot::from_text(
                project,
                RelativeSourcePath::new("src/auth.rs").unwrap(),
                bytes,
            )
            .unwrap(),
        );
    }
    assert_eq!(
        snapshots[0].revision().content_sha256,
        snapshots[1].revision().content_sha256
    );
    assert_ne!(snapshots[0].revision(), snapshots[1].revision());
}

#[tokio::test]
async fn production_runtime_uses_injected_gateway_and_validates_real_output() {
    let fixture = PulsationFixture::new(LegacyPulseLayout::TwoQueued);
    let gateway = ControlledLlmGateway::new(valid_output());
    gateway.release_next();
    let result = execute_stage(
        &FixedEmbeddingGateway::default(),
        &gateway,
        &AgentRuntimeConfig::default(),
        &input(&fixture),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        gateway.call_count(),
        1,
        "synthetic fallback must not bypass this integration test"
    );
    assert_eq!(result.provider_used, "fixture-provider");
    // M1 preserves new bytes exactly. Historical V1 trimming is isolated in the upgrade adapter.
    assert_eq!(result.artifacts[0].content, "# Specification fixture\n");
    assert_eq!(
        result.structured_output["artifacts"][0]["content"],
        "# Specification fixture\n"
    );
    assert!(!fixture.project_a_root().join("knowledge").exists());
}

#[tokio::test]
async fn production_runtime_rejects_invalid_schema_from_the_provider() {
    let fixture = PulsationFixture::new(LegacyPulseLayout::TwoQueued);
    let gateway = ControlledLlmGateway::new(
        r#"{"summary":"bad","statusUpdate":"unknown","notes":[],"artifacts":[]}"#,
    );
    gateway.release_next();
    let result = execute_stage(
        &FixedEmbeddingGateway::default(),
        &gateway,
        &AgentRuntimeConfig::default(),
        &input(&fixture),
        &CancellationToken::new(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(gateway.call_count(), 1);
}

#[tokio::test]
async fn controlled_late_response_is_rejected_after_input_revision_changes() {
    let fixture = PulsationFixture::new(LegacyPulseLayout::TwoQueued);
    let gateway = ControlledLlmGateway::new(valid_output());
    let embeddings = FixedEmbeddingGateway::default();
    let config = AgentRuntimeConfig::default();
    let request = input(&fixture);
    let cancellation = CancellationToken::new();
    let submitted = fence();
    // Timeout is a deadlock guard only; the semaphore establishes the ordering.
    let (result, current) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            execute_stage(&embeddings, &gateway, &config, &request, &cancellation),
            async {
                gateway.wait_until_called().await;
                let mut current = submitted.clone();
                current.input_revision = Some(2);
                gateway.release_next();
                current
            }
        )
    })
    .await
    .expect("controlled response must finish");
    assert!(result.is_ok());
    assert_eq!(
        validate_result_acceptance(&submitted, &current, JobStatus::Running, None),
        Err(ContractError::StaleExecution)
    );
}

#[tokio::test]
async fn controlled_provider_observes_cancellation_without_a_response_release() {
    let fixture = PulsationFixture::new(LegacyPulseLayout::TwoQueued);
    let gateway = ControlledLlmGateway::new(valid_output());
    let embeddings = FixedEmbeddingGateway::default();
    let config = AgentRuntimeConfig::default();
    let request = input(&fixture);
    let cancellation = CancellationToken::new();
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(
            execute_stage(&embeddings, &gateway, &config, &request, &cancellation),
            async {
                gateway.wait_until_called().await;
                cancellation.cancel();
            }
        )
    })
    .await
    .expect("controlled cancellation must finish");
    assert!(result.is_err());
    assert_eq!(gateway.call_count(), 1);
}

#[test]
fn fixture_models_publication_crash_windows_without_claiming_a_real_publisher() {
    let fixture = PulsationFixture::new(LegacyPulseLayout::OneActive);
    let path = fixture.project_a_root().join("src/auth.rs");
    let original = ContentSha256::of(&fs::read(&path).unwrap());
    let desired = ContentSha256::of(b"new bytes");
    let expected = ExpectedFileContent::Present(original.clone());
    assert_eq!(
        publication_replay_decision(&expected, &desired, Some(&original)),
        PublicationReplayDecision::Apply
    );
    fs::write(&path, b"new bytes").unwrap();
    let observed = ContentSha256::of(&fs::read(&path).unwrap());
    assert_eq!(
        publication_replay_decision(&expected, &desired, Some(&observed)),
        PublicationReplayDecision::AcknowledgeAlreadyApplied
    );
    fs::write(&path, b"human change").unwrap();
    let observed = ContentSha256::of(&fs::read(&path).unwrap());
    assert_eq!(
        publication_replay_decision(&expected, &desired, Some(&observed)),
        PublicationReplayDecision::Conflict
    );
    assert_eq!(fs::read(&path).unwrap(), b"human change");
}
