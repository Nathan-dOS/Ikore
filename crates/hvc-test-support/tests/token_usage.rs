use hvc_core::{
    persistence_service::PersistenceService,
    usage::{UsageQuery, query_usage},
};
use rusqlite::Connection;

mod migrations {
    refinery::embed_migrations!("../hvc-core/migrations");
}

#[tokio::test]
async fn adapters_persist_native_usage_on_complete_streamed_and_failed_responses() {
    use hvc_core::llm::{
        AgentRole, LlmRequest, ResolvedConnection, ResponseFormat,
        adapter::{AccountingSink, AdapterCall, LlmAdapter, ProtocolAdapter, TokenDeltaSink},
        profiles::profile_by_id,
    };
    use hvc_core::usage::{UsageContext, UsageTransport, api::UsageRecorder};
    use hvc_test_support::{ScriptedHttpServer, ScriptedResponse};
    use std::time::{Duration, Instant};
    use tokio_util::sync::CancellationToken;
    struct Sink;
    impl TokenDeltaSink for Sink {
        fn push(&self, _: &str) {}
    }
    let server = ScriptedHttpServer::start([
        ScriptedResponse::json(
            200,
            r#"{"model":"actual-a","choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":10,"completion_tokens":3}}"#,
        ),
        ScriptedResponse::sse([
            "data: {\"model\":\"actual-b\",\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":20,\"completion_tokens\":5,\"total_tokens\":25,\"prompt_tokens_details\":{\"cached_tokens\":12}}}\n\ndata: [DONE]\n\n",
        ]),
        ScriptedResponse::json(
            500,
            r#"{"error":{"message":"fixture failed"},"usage":{"prompt_tokens":7,"completion_tokens":1}}"#,
        ),
    ]);
    let root = tempfile::tempdir().unwrap();
    let persistence = PersistenceService::initialize(root.path()).unwrap();
    let profile = profile_by_id("openai_legacy").unwrap();
    let connection = ResolvedConnection {
        id: "fixture".into(),
        provider_id: profile.id.clone().into(),
        credential_ref: "unused".into(),
        endpoint_url: server.url("/chat/completions"),
        default_model: "requested".into(),
        enabled: true,
        state: "active".into(),
        cooldown_until: None,
        backoff_level: 0,
        max_concurrency: 1,
    };
    let adapter = ProtocolAdapter::new(Default::default(), profile.protocol);
    for index in 0..3 {
        let id = format!("adapter-{index}");
        let request = LlmRequest {
            execution_id: id.clone().into(),
            generation_id: id.clone().into(),
            project_id: "fixture".into(),
            pulsation_id: None,
            job_id: None,
            job_chain: id.clone(),
            agent_role: AgentRole::BackendAgent,
            stage: "Code".into(),
            system_prompt: "Fixture".into(),
            user_prompt: "Fixture".into(),
            response_format: ResponseFormat::Text,
            max_output_tokens: 32,
            temperature: 0.0,
            deadline: Instant::now() + Duration::from_secs(10),
            stream: index == 1,
            legacy_provider: profile.id.clone(),
            legacy_model: "requested".into(),
        };
        let recorder = UsageRecorder::new(
            persistence.database_path(),
            UsageContext {
                source_kind: "test_adapter".into(),
                source_id: id,
                pulsation_id: None,
                project_id: None,
                transport: UsageTransport::Api,
                provider_id: profile.id.clone(),
                requested_model: Some("requested".into()),
                started_at: chrono::Utc::now().to_rfc3339(),
            },
        );
        let sink = AccountingSink {
            inner: &Sink,
            recorder: &recorder,
        };
        let result = adapter
            .execute(
                AdapterCall {
                    profile: &profile,
                    connection: &connection,
                    model: "requested",
                    credential: Some("fixture"),
                    request: &request,
                },
                &CancellationToken::new(),
                &sink,
            )
            .await;
        assert_eq!(result.is_ok(), index != 2);
        recorder.finish(result.is_ok()).unwrap();
    }
    let usage = query_usage(persistence.database_path(), &UsageQuery::default()).unwrap();
    assert_eq!(usage.totals.total_tokens, Some(46));
    assert_eq!(usage.totals.records, 3);
    assert_eq!(usage.totals.partial_records, 1);
    assert_eq!(usage.totals.cache_read_tokens, Some(12));
    assert!(usage.models.iter().any(|m| m.model_id == "actual-b"));
    let requests = server.wait_for_requests(3, Duration::from_secs(2));
    let payload: serde_json::Value = serde_json::from_str(&requests[1].body).unwrap();
    assert_eq!(
        payload.pointer("/stream_options/include_usage"),
        Some(&serde_json::json!(true))
    );
}

#[test]
fn v28_imports_canonical_api_history_once_and_retains_it_after_deletion() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    migrations::migrations::runner()
        .set_target(refinery::Target::Version(27))
        .run(&mut db)
        .unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON;
        INSERT INTO projects(id,name,workspace_path,created_at,updated_at)
        VALUES('p','Usage migration','C:/usage-migration','2026-01-01','2026-01-01');
        INSERT INTO provider_connections(id,provider_id,display_name,credential_ref,default_model,created_at,updated_at)
        VALUES('c','openai','Fixture','fixture-reference','model-a','2026-01-01','2026-01-01');
        INSERT INTO provider_routes(id,name,created_at,updated_at) VALUES('r','Usage fixture','2026-01-01','2026-01-01');
        INSERT INTO jobs(id,kind,status,created_at,updated_at) VALUES
        ('routed-job','code','completed','2026-01-01','2026-01-01'),
        ('legacy-job','code','completed','2026-01-01','2026-01-01'),
        ('cli-job','code','completed','2026-01-01','2026-01-01');
        INSERT INTO llm_route_executions(id,generation_id,project_id,job_id,route_id,agent_role,stage,strategy,mode,status,decision_json,started_at)
        VALUES('e','g','p','routed-job','r','backend_agent','Code','priority','on','completed','{}','2026-01-01');
        INSERT INTO llm_attempts(id,execution_id,ordinal,connection_id,provider_id,model_id,outcome,input_tokens,output_tokens,started_at)
        VALUES('a','e',1,'c','openai','model-a','success',10,3,'2026-01-01'),
        ('missing','e',2,'c','openai','model-b','failed',NULL,NULL,'2026-01-01');
        INSERT INTO agent_runs(id,job_id,agent_name,status,provider,model,input_tokens,output_tokens,created_at,updated_at,llm_execution_id)
        VALUES('duplicate','routed-job','backend_agent','completed','openai','model-a',1000,1000,'2026-01-01','2026-01-01','e'),
        ('legacy','legacy-job','backend_agent','completed','openai','model-a',7,2,'2026-01-01','2026-01-01',NULL),
        ('legacy-cli','cli-job','backend_agent','completed','codex','model-a',999,999,'2026-01-01','2026-01-01',NULL);
        INSERT INTO terminal_runs(id,project_id,agent_role,origin,execution_mode,backend,agent_run_id,status,created_at,updated_at)
        VALUES('terminal','p','backend_agent','pipeline','cli_process','codex','legacy-cli','completed','2026-01-01','2026-01-01');
        INSERT INTO chat_conversations(id,project_id,provider_id_snapshot,model_id_snapshot,title,created_at,updated_at)
        VALUES('conversation','p','openai','model-a','Usage fixture','2026-01-01','2026-01-01');
        INSERT INTO chat_messages(id,conversation_id,ordinal,role,status,input_tokens,output_tokens,created_at,updated_at)
        VALUES('message','conversation',1,'assistant','completed',9999,9999,'2026-01-01','2026-01-01');
        INSERT INTO chat_model_calls(id,assistant_message_id,round_index,provider_id,model_id,started_at,status,input_tokens,output_tokens,transport_kind)
        VALUES('chat-api','message',1,'openai','model-a','2026-01-01','completed',20,4,'api'),
        ('chat-cli','message',2,'codex','default','2026-01-01','completed',999,999,'cli');
    ").unwrap();
    drop(db);
    PersistenceService::initialize(root.path()).unwrap();
    let totals = query_usage(&path, &UsageQuery::default()).unwrap().totals;
    assert_eq!(totals.total_tokens, Some(46));
    assert_eq!(totals.records, 4);
    assert_eq!(totals.partial_records, 3);
    assert_eq!(totals.unavailable_records, 1);
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM token_usage_records WHERE historical=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        4
    );
    db.execute_batch("PRAGMA foreign_keys=ON; DELETE FROM chat_conversations; DELETE FROM llm_route_executions; DELETE FROM agent_runs;").unwrap();
    drop(db);
    PersistenceService::initialize(root.path()).unwrap();
    assert_eq!(
        query_usage(&path, &UsageQuery::default()).unwrap().totals,
        totals
    );
}

#[test]
fn v29_attributes_existing_ledger_without_recounting_and_survives_source_deletion() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("hard-vibe-code.db");
    let mut db = Connection::open(&path).unwrap();
    migrations::migrations::runner()
        .set_target(refinery::Target::Version(28))
        .run(&mut db)
        .unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON;
        INSERT INTO projects(id,name,workspace_path,created_at,updated_at) VALUES('p','Project','C:/v29-fixture','2026-01-01','2026-01-01');
        INSERT INTO pulsations(id,project_id,feature_name,stage,status,created_at,updated_at) VALUES('pulse','p','Feature','Code','failed','2026-01-01','2026-01-01');
        INSERT INTO jobs(id,pulsation_id,kind,status,created_at,updated_at) VALUES('job','pulse','code','completed','2026-01-01','2026-01-01');
        INSERT INTO agent_runs(id,job_id,agent_name,status,created_at,updated_at) VALUES('legacy','job','backend_agent','completed','2026-01-01','2026-01-01');
        INSERT INTO provider_connections(id,provider_id,display_name,credential_ref,default_model,created_at,updated_at) VALUES('c','openai','Fixture','fixture','m','2026-01-01','2026-01-01');
        INSERT INTO provider_routes(id,name,created_at,updated_at) VALUES('r','Fixture','2026-01-01','2026-01-01');
        INSERT INTO llm_route_executions(id,generation_id,project_id,pulsation_id,job_id,route_id,agent_role,stage,strategy,mode,status,decision_json,started_at)
        VALUES('e','g','p','pulse','job','r','backend_agent','Code','priority','on','completed','{}','2026-01-01');
        INSERT INTO llm_attempts(id,execution_id,ordinal,connection_id,provider_id,model_id,outcome,started_at) VALUES('attempt','e',1,'c','openai','m','success','2026-01-01');
        INSERT INTO terminal_runs(id,project_id,pulsation_id,agent_role,origin,execution_mode,backend,status,created_at,updated_at)
        VALUES('terminal','p','pulse','backend_agent','pipeline','cli_process','codex','failed','2026-01-01','2026-01-01');
        INSERT INTO token_usage_records(source_kind,source_id,unit_key,project_id,transport,provider_id,input_tokens,output_tokens,total_tokens,quality,started_at,updated_at) VALUES
        ('llm_attempt','attempt','response','p','api','openai',10,2,12,'complete','2026-01-01','2026-01-01'),
        ('terminal_run','terminal','turn','p','cli','codex',5,1,6,'complete','2026-01-01','2026-01-01'),
        ('legacy_agent_run','legacy','response','p','api','openai',3,1,4,'partial','2026-01-01','2026-01-01'),
        ('llm_attempt','deleted-attempt','response','p','api','openai',1,1,2,'complete','2026-01-01','2026-01-01');
    ").unwrap();
    migrations::migrations::runner().run(&mut db).unwrap();
    let result = query_usage(&path, &UsageQuery::default()).unwrap();
    assert_eq!(result.totals.total_tokens, Some(24));
    assert_eq!(result.totals.records, 4);
    assert_eq!(result.pulsations.len(), 1);
    assert_eq!(result.pulsations[0].totals.total_tokens, Some(22));
    assert_eq!(result.pulsations[0].models.len(), 2);
    assert_eq!(result.without_pulsation.total_tokens, Some(2));
    db.execute_batch("DELETE FROM llm_route_executions; DELETE FROM terminal_runs; DELETE FROM jobs; DELETE FROM projects;").unwrap();
    migrations::migrations::runner().run(&mut db).unwrap();
    let retained = query_usage(&path, &UsageQuery::default()).unwrap();
    assert_eq!(retained.totals, result.totals);
    assert_eq!(retained.pulsations[0].totals, result.pulsations[0].totals);
    assert_eq!(retained.pulsations[0].name, "Deleted pulsation");
    assert_eq!(retained.pulsations[0].pulsation_id, "pulse");
}
