use super::{
    Arc, Connection, Corpus, Duration, Instant, LegacyRag, Result, ScriptedHttpServer, Value, data,
    json, pulsation_benchmark,
};
use crate::ScriptedResponse;
use ikore_core::persistence_service::{
    ApplyHitlDecisionRequest, CancelPulsationJobsRequest, CreatePulsationRequest, HitlDecision,
    RiskAcknowledgement,
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

#[derive(Deserialize)]
struct Task {
    feature: String,
    description: String,
}
#[derive(Clone, Serialize)]
struct Turn {
    stage: String,
    transport: &'static str,
    input_tokens: usize,
    output_tokens: usize,
}

pub(super) fn run(corpus: &Corpus, live: bool) -> Result<Value> {
    let context = LegacyRag::open(corpus)?;
    let service = context.core.persistence();
    service
        .update_preparation_settings(ikore_core::orchestration::PreparationSettings {
            preparation_enabled: false,
            learning_enabled: false,
            ..Default::default()
        })
        .map_err(|e| format!("{e:?}"))?;
    let turns = Arc::new(Mutex::new(Vec::new()));
    let observed = turns.clone();
    let active = Arc::new(Mutex::new(String::new()));
    let current_task = active.clone();
    let server = ScriptedHttpServer::respond_with(move |request| {
        let body: Value = serde_json::from_str(&request.body).unwrap();
        let messages = body["messages"].as_array().unwrap();
        let prompt = messages
            .iter()
            .filter_map(|m| m["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let stage = prompt
            .lines()
            .find_map(|l| l.strip_prefix("- stage: "))
            .unwrap_or("unknown");
        let lane = current_task.lock().unwrap().clone();
        let artifact = match stage {
            "Code" => {
                json!({"name":"Admission boundary guide","kind":"code","relativePath":format!("docs/{lane}-admission.md"),"content":format!("# {} admission\n\nThe minimum and capacity are inclusive. The ledger rejects counts outside that interval. Split tickets must independently satisfy both boundaries.\n",lane)})
            }
            "Test" => {
                json!({"name":"Admission boundary test","kind":"test","relativePath":format!("tests/{lane}_admission.rs"),"content":format!("use parcel_clearinghouse::{lane}::{{CAPACITY, MINIMUM, Ticket}};\n\n#[test]\nfn admission_limits_are_inclusive() {{\n    assert!(Ticket::open(MINIMUM).is_some());\n    assert!(Ticket::open(CAPACITY).is_some());\n    assert!(Ticket::open(CAPACITY + 1).is_none());\n}}\n")})
            }
            "Contract" => {
                json!({"name":"Admission contract","kind":"contract","relativePath":null,"content":"# Contract\n\nThe admission interface preserves both inclusive bounds and returns no ticket outside the interval."})
            }
            "Integrator" => {
                json!({"name":"Integration report","kind":"integration_report","relativePath":null,"content":"# Integration report\n\nThe boundary guide and regression test describe the same closed interval."})
            }
            _ => {
                json!({"name":"Parcel admission review","kind":"specification","relativePath":null,"content":format!("# Parcel admission\n\nDocument the inclusive bounds for {lane} and cover the rejection above capacity.")})
            }
        };
        let mut reply = json!({"outputLanguage":"en","summary":"Parcel admission review","statusUpdate":if matches!(stage,"Intake"|"Code"){"advance"}else{"checkpoint_ready"},"contextUsage":[],"notes":[],"artifacts":if stage=="Intake"{vec![]}else{vec![artifact]}});
        if stage == "SPECIFICATION" {
            reply["componentProposals"] = json!([]);
        }
        let content = reply.to_string();
        let input = body["bridge"]["stdin_chars"]
            .as_u64()
            .map_or(prompt.chars().count(), |n| n as usize)
            .div_ceil(4);
        let output = content.chars().count().div_ceil(4);
        observed.lock().unwrap().push(Turn {
            stage: stage.into(),
            transport: if body["bridge"].is_object() {
                "cli_fixture"
            } else {
                "loopback_api"
            },
            input_tokens: input,
            output_tokens: output,
        });
        let model = body["model"].as_str().unwrap_or("gpt-4.1-mini");
        ScriptedResponse::json(200,json!({"model":model,"choices":[{"message":{"content":content},"finish_reason":"stop"}],"usage":{"prompt_tokens":input,"completion_tokens":output}}).to_string())
    });
    pulsation_benchmark::configure_loopback(service, &server);
    pulsation_benchmark::force_api_direct(service, &context.project);
    let db = Connection::open(service.database_path())?;
    let fixture = std::env::current_exe()?
        .parent()
        .and_then(Path::parent)
        .ok_or("missing Cargo profile")?
        .join(format!("cli_fixture{}", std::env::consts::EXE_SUFFIX));
    if !live && !fixture.is_file() {
        return Err("build the cli_fixture binary in the same Cargo profile".into());
    }
    for role in [
        "intake_router",
        "task_specifier",
        "contract_agent",
        "backend_agent",
        "test_agent",
        "integrator",
    ] {
        if live {
            db.execute("UPDATE cli_agent_configs SET execution_mode='cli_process',backend='claude',model='sonnet',custom_args_json='[]',custom_executable=NULL,version=version+1 WHERE project_id=?1 AND agent_role=?2",rusqlite::params![context.project,role])?;
        } else if matches!(role, "backend_agent" | "test_agent") {
            let args = json!([
                "--mode",
                "bridge",
                "--bridge-url",
                server.url("/v1/chat/completions"),
                "--fixture-model",
                "gpt-4.1-mini"
            ]);
            db.execute("UPDATE cli_agent_configs SET execution_mode='cli_process',backend='custom',model='gpt-4.1-mini',custom_executable=?3,custom_args_json=?4,custom_risk_ack=1,version=version+1 WHERE project_id=?1 AND agent_role=?2",rusqlite::params![context.project,role,fixture.to_string_lossy(),args.to_string()])?;
        }
    }
    // A headless project needs the same canonical terminal slots as an opened GUI project.
    context
        .core
        .terminals()
        .repository()
        .ensure_project_configs(&context.project, false)?;
    let mut rows = Vec::new();
    let tasks: Vec<Task> = data("pulsations.yaml")?;
    for task in tasks.into_iter().take(if live { 1 } else { 12 }) {
        *active.lock().unwrap() = task.feature.clone();
        turns.lock().unwrap().clear();
        let start = Instant::now();
        let pulse = service
            .create_pulsation(CreatePulsationRequest {
                project_id: context.project.clone(),
                feature_name: task.description.clone(),
            })
            .map_err(|e| format!("{e:?}"))?
            .pulsation
            .id;
        let mut last_stage = String::new();
        let terminal = loop {
            let (stage,status,checkpoint):(String,String,Option<String>)=db.query_row("SELECT stage,status,(SELECT kind FROM checkpoints WHERE pulsation_id=p.id AND status='waiting' ORDER BY created_at DESC LIMIT 1) FROM pulsations p WHERE id=?1",[&pulse],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if stage != last_stage {
                super::progress(
                    json!({"phase":"pulsations","feature":task.feature,"stage":stage,"completed":rows.len()}),
                );
                last_stage.clone_from(&stage);
            }
            if status == "done" {
                break "done".to_owned();
            }
            if checkpoint.as_deref() == Some("artifact_review") {
                let review = service
                    .get_hitl_checkpoint(&pulse)
                    .map_err(|e| format!("{e:?}"))?;
                service
                    .apply_hitl_decision_with_acknowledgements(
                        ApplyHitlDecisionRequest {
                            pulsation_id: pulse.clone(),
                            checkpoint_id: review.checkpoint_id,
                            expected_state_version: review.state_version,
                            feedback: None,
                            decision: HitlDecision::Approve,
                        },
                        None,
                        Some(RiskAcknowledgement {
                            reason: "Review of synthetic parcel workload.".into(),
                        }),
                    )
                    .map_err(|e| format!("{e:?}"))?;
                continue;
            }
            if let Some(kind) = checkpoint {
                break format!("{stage}/{kind}");
            }
            if status == "failed"
                || start.elapsed() > Duration::from_secs(if live { 600 } else { 180 })
            {
                break format!("{stage}/{status}");
            }
            std::thread::sleep(Duration::from_millis(30));
        };
        let elapsed = start.elapsed().as_secs_f64() * 1000.;
        let stages: Vec<Value> = db
            .prepare("SELECT kind, (julianday(finished_at)-julianday(started_at))*86400000
                      FROM jobs WHERE pulsation_id=?1 AND kind LIKE 'run_stage::%' ORDER BY created_at")?
            .query_map([&pulse], |r| {
                Ok(json!({"stage":r.get::<_,String>(0)?, "elapsed_ms":r.get::<_,Option<f64>>(1)?}))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let succeeded = terminal == "done";
        if !live && !succeeded {
            let reason: Option<String> = db.query_row(
                "SELECT group_concat(last_error) FROM jobs WHERE pulsation_id=?1",
                [&pulse],
                |r| r.get(0),
            )?;
            let first_error: Option<String> = db.query_row(
                "SELECT group_concat(backend || ': ' || diagnostic) FROM cli_backend_attempts WHERE pulsation_id=?1",
                [&pulse], |r| r.get(0),
            )?;
            return Err(format!(
                "fixture pulse stopped at {terminal}: {first_error:?}; {reason:?}"
            )
            .into());
        }
        let recorded = turns.lock().unwrap().clone();
        if !live && !recorded.iter().any(|turn| turn.transport == "cli_fixture") {
            return Err("completed fixture task without invoking the CLI bridge".into());
        }
        let cli_runs: Vec<Value> = db.prepare(
            "SELECT agent_role,backend,status,error_code,exit_code,observed_model FROM terminal_runs
             WHERE pulsation_id=?1 ORDER BY created_at"
        )?.query_map([&pulse], |r| Ok(json!({"role":r.get::<_,String>(0)?,"backend":r.get::<_,String>(1)?,
            "status":r.get::<_,String>(2)?,"error_code":r.get::<_,Option<String>>(3)?,
            "exit_code":r.get::<_,Option<i32>>(4)?,"observed_model":r.get::<_,Option<String>>(5)?})))?
            .collect::<rusqlite::Result<_>>()?;
        rows.push(json!({"feature":task.feature,"task":task.description,"success":succeeded,"endpoint":terminal,"elapsed_ms":elapsed,"stages":stages,"turns":recorded,"cli_runs":cli_runs}));
        super::progress(
            json!({"phase":"pulsations","completed":rows.len(),"last_endpoint":terminal}),
        );
        if !succeeded {
            service
                .cancel_pulsation_jobs(CancelPulsationJobsRequest {
                    pulsation_id: pulse,
                })
                .map_err(|e| format!("{e:?}"))?;
        }
    }
    context.runtime.block_on(context.core.shutdown());
    Ok(
        json!({"transport":if live{"authenticated Claude CLI"}else{"cli_fixture bridge"},"target":"done after all stage reviews","tasks":rows}),
    )
}
