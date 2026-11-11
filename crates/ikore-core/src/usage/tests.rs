use super::*;
use crate::persistence_service::PersistenceService;

fn database() -> (tempfile::TempDir, Connection) {
    let root = tempfile::tempdir().unwrap();
    let service = PersistenceService::initialize(root.path()).unwrap();
    let db = Connection::open(service.database_path()).unwrap();
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
         INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES
         ('project-a','Project A','C:/usage-fixture/a','2026-01-01','2026-01-01'),
         ('project-b','Project B','C:/usage-fixture/b','2026-01-01','2026-01-01');",
    )
    .unwrap();
    (root, db)
}

fn context(id: &str) -> UsageContext {
    UsageContext {
        source_kind: "llm_attempt".into(),
        source_id: id.into(),
        pulsation_id: None,
        project_id: Some("project-a".into()),
        transport: UsageTransport::Api,
        provider_id: "openai".into(),
        requested_model: Some("requested-model".into()),
        started_at: "2026-09-09T10:00:00Z".into(),
    }
}

fn measurement(input: i64, output: i64) -> UsageMeasurement {
    UsageMeasurement {
        counts: TokenCounts {
            input_tokens: Some(input),
            output_tokens: Some(output),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn now() -> DateTime<Utc> {
    "2026-09-09T12:00:00Z".parse().unwrap()
}

fn summary(db: &Connection) -> UsageSummary {
    query_usage_at(db, &UsageQuery::default(), now()).unwrap()
}

#[test]
fn repeated_snapshots_replace_one_unit_and_keep_other_units() {
    let (_root, db) = database();
    let source = context("request-1");
    record_usage_on(&db, &source, "response", &UsageMeasurement::default()).unwrap();
    record_usage_on(&db, &source, "response", &measurement(12, 3)).unwrap();
    let final_snapshot = UsageMeasurement {
        observed_model: Some("actual-model".into()),
        ..measurement(12, 8)
    };
    for _ in 0..3 {
        record_usage_on(&db, &source, "response", &final_snapshot).unwrap();
    }
    // A later frame without usage cannot erase the last supplier snapshot.
    record_usage_on(&db, &source, "response", &UsageMeasurement::default()).unwrap();
    record_usage_on(&db, &source, "second-step", &measurement(2, 1)).unwrap();

    let result = summary(&db);
    assert_eq!(result.totals.records, 2);
    assert_eq!(result.totals.input_tokens, Some(14));
    assert_eq!(result.totals.output_tokens, Some(9));
    assert_eq!(result.totals.total_tokens, Some(23));
    assert_eq!(result.totals.partial_records, 0);
    assert_eq!(result.totals.unavailable_records, 0);
    let observed = result
        .models
        .iter()
        .find(|m| m.model_source == "observed")
        .unwrap();
    assert_eq!(observed.model_id, "actual-model");
    assert_eq!(observed.totals.total_tokens, Some(20));
}

#[test]
fn absent_usage_is_distinct_from_supplier_reported_zero() {
    let (_root, db) = database();
    let empty = summary(&db);
    assert_eq!(empty.totals.records, 0);
    assert_eq!(empty.totals.total_tokens, None);

    record_usage_on(
        &db,
        &context("missing"),
        "response",
        &UsageMeasurement::default(),
    )
    .unwrap();
    let absent = summary(&db);
    assert_eq!(absent.totals.records, 1);
    assert_eq!(absent.totals.total_tokens, None);
    assert_eq!(absent.totals.unavailable_records, 1);

    record_usage_on(&db, &context("zero"), "response", &measurement(0, 0)).unwrap();
    let known_zero = summary(&db);
    assert_eq!(known_zero.totals.total_tokens, Some(0));
    assert_eq!(known_zero.totals.input_tokens, Some(0));
    assert_eq!(known_zero.totals.output_tokens, Some(0));
    assert_eq!(known_zero.totals.records, 2);
    assert_eq!(known_zero.totals.unavailable_records, 1);
    assert_eq!(known_zero.totals.partial_records, 0);
}

#[test]
fn api_usage_is_attributed_to_the_persisted_canonical_or_free_form_target() {
    let (_root, db) = database();
    let connection_id: String = db
        .query_row(
            "SELECT id FROM provider_connections ORDER BY id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let route_id: String = db
        .query_row(
            "SELECT id FROM provider_routes ORDER BY id LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    db.execute(
        "INSERT INTO llm_route_executions(
            id,generation_id,project_id,route_id,agent_role,free_form_slot_id,
            stage,strategy,mode,status,decision_json,started_at)
         VALUES('execution-canonical','generation-canonical','project-a',?1,
                'backend_agent',NULL,'Code','priority','on','completed','{}',?2),
               ('execution-free-form','generation-free-form','project-a',?1,
                NULL,'slot-api','FreeForm','priority','on','completed','{}',?2)",
        params![route_id, "2026-09-09T10:00:00Z"],
    )
    .unwrap();
    for (attempt, execution, cost) in [
        ("attempt-canonical", "execution-canonical", 0.001),
        ("attempt-free-form", "execution-free-form", 0.0025),
    ] {
        db.execute(
            "INSERT INTO llm_attempts(id,execution_id,ordinal,connection_id,provider_id,
                    model_id,outcome,estimated_cost_usd,started_at)
             VALUES(?1,?2,1,?3,'openai','fixture-model','success',?4,?5)",
            params![
                attempt,
                execution,
                connection_id,
                cost,
                "2026-09-09T10:00:00Z"
            ],
        )
        .unwrap();
        record_usage_on(&db, &context(attempt), "response", &measurement(10, 5)).unwrap();
    }

    let result = summary(&db);
    assert_eq!(result.agent_targets.len(), 2);
    let free_form = result
        .agent_targets
        .iter()
        .find(|entry| entry.target_key == "free_form:slot-api")
        .unwrap();
    assert_eq!(free_form.attempts, 1);
    assert_eq!(free_form.totals.total_tokens, Some(15));
    assert_eq!(free_form.estimated_cost_usd_micros, Some(2_500));
    assert!(
        result
            .agent_targets
            .iter()
            .any(|entry| entry.target_key == "canonical:backend_agent")
    );
}

#[test]
fn normalized_breakdowns_do_not_inflate_totals_and_invalid_counts_are_partial() {
    let (_root, db) = database();
    let normalized = UsageMeasurement {
        counts: TokenCounts {
            input_tokens: Some(100),
            output_tokens: Some(30),
            cache_read_tokens: Some(60),
            cache_write_tokens: Some(10),
            reasoning_tokens: Some(20),
            ..Default::default()
        },
        ..Default::default()
    };
    record_usage_on(&db, &context("normalized"), "response", &normalized).unwrap();
    let result = summary(&db);
    assert_eq!(result.totals.total_tokens, Some(130));
    assert_eq!(result.totals.cache_read_tokens, Some(60));
    assert_eq!(result.totals.cache_write_tokens, Some(10));
    assert_eq!(result.totals.reasoning_tokens, Some(20));

    record_usage_on(&db, &context("invalid"), "response", &measurement(-10, 4)).unwrap();
    let result = summary(&db);
    assert_eq!(result.totals.total_tokens, Some(134));
    assert_eq!(result.totals.input_tokens, Some(100));
    assert_eq!(result.totals.output_tokens, Some(34));
    assert_eq!(result.totals.partial_records, 1);
}

#[test]
fn model_breakdown_keeps_provider_transport_and_attribution() {
    let (_root, db) = database();
    let mut observed = measurement(10, 2);
    observed.observed_model = Some("actual-model".into());
    record_usage_on(&db, &context("api-observed"), "response", &observed).unwrap();
    record_usage_on(
        &db,
        &context("api-requested"),
        "response",
        &measurement(5, 1),
    )
    .unwrap();

    let mut other_provider = context("other-provider");
    other_provider.provider_id = "custom-provider".into();
    record_usage_on(&db, &other_provider, "response", &observed).unwrap();
    let mut cli = context("cli");
    cli.transport = UsageTransport::Cli;
    cli.source_kind = "terminal_run".into();
    cli.requested_model = Some("default".into());
    record_usage_on(&db, &cli, "turn:1", &measurement(3, 1)).unwrap();

    let result = summary(&db);
    assert_eq!(result.models.len(), 4);
    assert_eq!(result.totals.total_tokens, Some(34));
    assert!(result.models.iter().any(|m| m.provider_id == "openai"
        && m.model_id == "actual-model"
        && m.model_source == "observed"));
    assert!(
        result
            .models
            .iter()
            .any(|m| m.provider_id == "custom-provider" && m.model_id == "actual-model")
    );
    assert!(
        result
            .models
            .iter()
            .any(|m| m.model_id == "requested-model" && m.model_source == "requested")
    );
    assert!(
        result
            .models
            .iter()
            .any(|m| m.transport == UsageTransport::Cli
                && m.model_id == "Unknown model"
                && m.model_source == "unknown")
    );
}

#[test]
fn project_transport_and_utc_period_filters_keep_cards_and_series_consistent() {
    let (_root, db) = database();
    let rows = [
        (
            "before-week",
            "2026-09-02T23:59:59Z",
            "project-a",
            UsageTransport::Api,
            1,
        ),
        (
            "week-start",
            "2026-09-03T00:00:00Z",
            "project-a",
            UsageTransport::Api,
            2,
        ),
        (
            "offset",
            "2026-09-02T21:00:00-03:00",
            "project-a",
            UsageTransport::Api,
            3,
        ),
        (
            "cli",
            "2026-09-09T11:00:00Z",
            "project-a",
            UsageTransport::Cli,
            5,
        ),
        (
            "other-project",
            "2026-09-09T11:00:00Z",
            "project-b",
            UsageTransport::Api,
            7,
        ),
        (
            "future",
            "2026-09-09T12:00:01Z",
            "project-a",
            UsageTransport::Api,
            11,
        ),
        (
            "before-month",
            "2026-08-10T23:59:59Z",
            "project-a",
            UsageTransport::Api,
            13,
        ),
        (
            "month-start",
            "2026-08-11T00:00:00Z",
            "project-a",
            UsageTransport::Api,
            17,
        ),
    ];
    for (id, started_at, project, transport, tokens) in rows {
        let mut source = context(id);
        source.started_at = started_at.into();
        source.project_id = Some(project.into());
        source.transport = transport;
        record_usage_on(&db, &source, "response", &measurement(tokens, 0)).unwrap();
    }

    let mut filter = UsageQuery {
        pulsation_id: None,
        project_id: Some("project-a".into()),
        transport: Some(UsageTransport::Api),
        period: UsagePeriod::Last7Days,
    };
    let week = query_usage_at(&db, &filter, now()).unwrap();
    assert_eq!(week.totals.total_tokens, Some(5));
    assert_eq!(week.totals.records, 2);
    assert_eq!(week.models.len(), 1);
    assert_eq!(week.models[0].totals, week.totals);
    let active_days: Vec<_> = week
        .series
        .iter()
        .filter(|b| b.totals.records > 0)
        .collect();
    assert_eq!(active_days.len(), 1);
    assert_eq!(active_days[0].period, "2026-09-03");
    assert_eq!(active_days[0].totals, week.totals);

    filter.period = UsagePeriod::Last30Days;
    let month = query_usage_at(&db, &filter, now()).unwrap();
    assert_eq!(month.totals.total_tokens, Some(23));
    assert_eq!(month.totals.records, 4);
    filter.period = UsagePeriod::AllTime;
    let all_time = query_usage_at(&db, &filter, now()).unwrap();
    assert_eq!(all_time.totals.total_tokens, Some(36));
    assert_eq!(all_time.series.len(), 2);
    assert_eq!(all_time.series[0].period, "2026-08");
    assert_eq!(all_time.series[0].totals.total_tokens, Some(30));
    assert_eq!(all_time.series[1].period, "2026-09");
    assert_eq!(all_time.series[1].totals.total_tokens, Some(6));
    assert_eq!(summary(&db).totals.total_tokens, Some(48));
}

#[test]
fn deleting_project_does_not_delete_its_usage_or_filter_identity() {
    let (_root, db) = database();
    record_usage_on(&db, &context("retained"), "response", &measurement(25, 5)).unwrap();
    db.execute("DELETE FROM projects WHERE id='project-a'", [])
        .unwrap();

    let result = query_usage_at(
        &db,
        &UsageQuery {
            project_id: Some("project-a".into()),
            ..Default::default()
        },
        now(),
    )
    .unwrap();
    assert_eq!(result.totals.total_tokens, Some(30));
    assert_eq!(result.totals.records, 1);
    assert!(
        result
            .projects
            .iter()
            .any(|p| p.id == "project-a" && p.name == "Deleted project")
    );
    assert!(
        result
            .projects
            .iter()
            .any(|p| p.id == "project-b" && p.name == "Project B")
    );
}

#[test]
fn read_only_query_does_not_create_a_missing_database() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("missing.db");
    assert!(query_usage(&path, &UsageQuery::default()).is_err());
    assert!(!path.exists());
}

#[test]
fn interrupted_stream_retains_the_last_reported_snapshot_without_content() {
    use crate::llm::{domain::ProviderProtocol, streaming::SseParser};
    let (root, db) = database();
    let recorder = api::UsageRecorder::new(&root.path().join("ikore.db"), context("interrupted"));
    let mut parser = SseParser::new(ProviderProtocol::AnthropicMessages);
    let stream=b"event: message_start\ndata: {\"message\":{\"model\":\"claude-test\",\"usage\":{\"input_tokens\":4,\"output_tokens\":0,\"cache_read_input_tokens\":10}}}\n\nevent: message_delta\ndata: {\"usage\":{\"output_tokens\":3}}\n\n";
    for chunk in stream.chunks(7) {
        parser.push(chunk).unwrap();
        recorder.observe(parser.usage());
    }
    // Dropping a cancelled future keeps supplier evidence as partial.
    drop(recorder);
    let result = summary(&db);
    assert_eq!(result.totals.total_tokens, Some(17));
    assert_eq!(result.totals.partial_records, 1);
    assert_eq!(result.models[0].model_id, "claude-test");
}

#[test]
fn cli_large_final_snapshot_is_not_cut_at_the_visible_log_limit() {
    use crate::terminals::CliBackend;
    use serde_json::json;
    let (root, db) = database();
    let source = UsageContext {
        transport: UsageTransport::Cli,
        ..context("cli-result")
    };
    let recorder =
        cli::CliUsageRecorder::new(&root.path().join("ikore.db"), source, CliBackend::Claude)
            .unwrap();
    let snapshot = json!({"type":"result","result":"x".repeat(80_000),"modelUsage":{
        "claude-test":{"inputTokens":4,"outputTokens":2,"cacheReadInputTokens":10}}});
    for _ in 0..2 {
        recorder
            .observe_line(&serde_json::to_vec(&snapshot).unwrap())
            .unwrap();
    }
    let result = summary(&db);
    assert_eq!(result.totals.total_tokens, Some(16));
    assert_eq!(result.totals.records, 1);
    assert_eq!(result.totals.unavailable_records, 0);
    recorder.observe_line(b"{truncated").unwrap();
    recorder
        .observe_line(&serde_json::to_vec(&snapshot).unwrap())
        .unwrap();
    assert_eq!(summary(&db).totals.partial_records, 1);
}

#[test]
fn provider_owned_identifiers_cannot_persist_secret_shaped_text() {
    use crate::terminals::CliBackend;
    use serde_json::json;
    let (root, db) = database();
    let source = UsageContext {
        transport: UsageTransport::Cli,
        requested_model: None,
        ..context("cli-private")
    };
    let recorder =
        cli::CliUsageRecorder::new(&root.path().join("ikore.db"), source, CliBackend::Claude)
            .unwrap();
    let snapshot = json!({"type":"result","modelUsage":{
        "sk-fixturesecret123456789":{"inputTokens":4,"outputTokens":2}}});
    recorder
        .observe_line(&serde_json::to_vec(&snapshot).unwrap())
        .unwrap();
    let (unit, model): (String, Option<String>) = db
        .query_row(
            "SELECT unit_key,observed_model FROM token_usage_records WHERE source_id='cli-private'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(!unit.contains("fixturesecret"));
    assert!(model.is_none());
    assert_eq!(summary(&db).models[0].model_id, "Unknown model");
}

#[test]
fn pulsation_totals_reconcile_with_models_filters_and_unassigned_usage() {
    let (_root, db) = database();
    db.execute_batch("INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at) VALUES
        ('pulse-a','project-a','First feature','Code','running','2026-01-01','2026-01-01'),
        ('pulse-b','project-b','Second feature','Code','failed','2026-01-01','2026-01-01');").unwrap();
    let mut a = context("attempt-a");
    a.pulsation_id = Some("pulse-a".into());
    record_usage_on(&db, &a, "response", &measurement(10, 2)).unwrap();
    record_usage_on(&db, &a, "response", &measurement(10, 2)).unwrap();
    let mut retry = a.clone();
    retry.source_id = "retry-a".into();
    retry.requested_model = Some("retry-model".into());
    record_usage_on(
        &db,
        &retry,
        "response",
        &UsageMeasurement {
            partial: true,
            ..measurement(3, 1)
        },
    )
    .unwrap();
    let mut cli = a.clone();
    cli.source_kind = "terminal_run".into();
    cli.source_id = "cli-a".into();
    cli.transport = UsageTransport::Cli;
    record_usage_on(&db, &cli, "turn", &measurement(5, 1)).unwrap();
    let mut learning = a.clone();
    learning.source_id = "learning-a".into();
    learning.started_at = "2026-08-01T10:00:00Z".into();
    record_usage_on(&db, &learning, "response", &measurement(1, 1)).unwrap();
    let mut b = context("attempt-b");
    b.pulsation_id = Some("pulse-b".into());
    b.project_id = Some("project-b".into());
    record_usage_on(&db, &b, "response", &measurement(40, 10)).unwrap();
    record_usage_on(&db, &context("unassigned"), "response", &measurement(7, 0)).unwrap();
    let result = summary(&db);
    assert_eq!(result.totals.total_tokens, Some(81));
    assert_eq!(result.without_pulsation.total_tokens, Some(7));
    assert_eq!(result.pulsations[0].pulsation_id, "pulse-b");
    let pulse = &result.pulsations[1];
    assert_eq!(pulse.name, "First feature");
    assert_eq!(pulse.totals.total_tokens, Some(24));
    assert_eq!(pulse.totals.records, 4);
    assert_eq!(pulse.totals.partial_records, 1);
    assert_eq!(
        pulse
            .models
            .iter()
            .filter_map(|m| m.totals.total_tokens)
            .sum::<i64>(),
        24
    );
    let mut query = UsageQuery {
        pulsation_id: a.pulsation_id.clone(),
        ..Default::default()
    };
    assert_eq!(
        query_usage_at(&db, &query, now()).unwrap().totals,
        pulse.totals
    );
    query.period = UsagePeriod::Last7Days;
    query.transport = Some(UsageTransport::Api);
    assert_eq!(
        query_usage_at(&db, &query, now())
            .unwrap()
            .totals
            .total_tokens,
        Some(16)
    );
    query.project_id = Some("project-b".into());
    assert_eq!(
        query_usage_at(&db, &query, now()).unwrap().totals.records,
        0
    );
    db.execute("DELETE FROM pulsations WHERE id='pulse-a'", [])
        .unwrap();
    let retained = summary(&db);
    assert_eq!(retained.totals, result.totals);
    assert_eq!(retained.pulsations[1].name, "Deleted pulsation");
    assert_eq!(retained.pulsations[1].totals, pulse.totals);
    // A missing attribution in a later snapshot cannot erase a durable link.
    a.pulsation_id = None;
    record_usage_on(&db, &a, "response", &measurement(10, 2)).unwrap();
    assert_eq!(summary(&db).pulsations[1].totals, pulse.totals);
}

#[test]
fn pulsation_missing_partial_and_reported_zero_remain_distinct() {
    let (_root, db) = database();
    for (id, evidence) in [
        ("missing", UsageMeasurement::default()),
        ("zero", measurement(0, 0)),
        (
            "partial",
            UsageMeasurement {
                counts: TokenCounts {
                    input_tokens: Some(8),
                    ..Default::default()
                },
                ..Default::default()
            },
        ),
    ] {
        let mut source = context(id);
        source.pulsation_id = Some(id.into());
        record_usage_on(&db, &source, "response", &evidence).unwrap();
    }
    let result = summary(&db);
    let missing = result
        .pulsations
        .iter()
        .find(|p| p.pulsation_id == "missing")
        .unwrap();
    assert_eq!(missing.totals.total_tokens, None);
    assert_eq!(missing.totals.unavailable_records, 1);
    let zero = result
        .pulsations
        .iter()
        .find(|p| p.pulsation_id == "zero")
        .unwrap();
    assert_eq!(zero.totals.total_tokens, Some(0));
    assert_eq!(zero.totals.unavailable_records, 0);
    let partial = result
        .pulsations
        .iter()
        .find(|p| p.pulsation_id == "partial")
        .unwrap();
    assert_eq!(partial.totals.total_tokens, Some(8));
    assert_eq!(partial.totals.partial_records, 1);
}
