use super::{
    Arc, Connection, Corpus, Duration, Instant, LegacyRag, Result, ScriptedHttpServer, Value, data,
    json, pulsation_benchmark,
};
use crate::ScriptedResponse;
use ikore_core::persistence_service::{CancelPulsationJobsRequest, CreatePulsationRequest};
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
        let kind = if stage == "SPECIFICATION" {
            "specification"
        } else {
            "stage_note"
        };
        let current = prompt
            .lines()
            .find_map(|l| l.strip_prefix("- feature: "))
            .unwrap_or("Parcel admission");
        let artifact = json!({"name":"Parcel policy review","kind":kind,"relativePath":null,"content":format!("# Parcel policy review\n\n{current}\n\nReview admission at the documented lower and upper bounds before publishing a revised policy.")});
        let mut reply = json!({"outputLanguage":"en","summary":"Parcel admission review","statusUpdate":if stage=="Intake"{"advance"}else{"checkpoint_ready"},"contextUsage":[],"notes":[],"artifacts":if stage=="Intake"{vec![]}else{vec![artifact]}});
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
            input_tokens: input,
            output_tokens: output,
        });
        ScriptedResponse::json(200,json!({"model":"gpt-4.1-mini","choices":[{"message":{"content":content},"finish_reason":"stop"}],"usage":{"prompt_tokens":input,"completion_tokens":output}}).to_string())
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
    for role in ["intake_router", "task_specifier"] {
        if live {
            db.execute("UPDATE cli_agent_configs SET execution_mode='cli_process',backend='claude',model='sonnet',custom_args_json='[]',custom_executable=NULL,version=version+1 WHERE project_id=?1 AND agent_role=?2",rusqlite::params![context.project,role])?;
        } else {
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
    let mut rows = Vec::new();
    let tasks: Vec<Task> = data("pulsations.yaml")?;
    for task in tasks.into_iter().take(if live { 1 } else { 12 }) {
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
        let terminal = loop {
            let (stage,status,checkpoint):(String,String,Option<String>)=db.query_row("SELECT stage,status,(SELECT kind FROM checkpoints WHERE pulsation_id=p.id AND status='waiting' ORDER BY created_at DESC LIMIT 1) FROM pulsations p WHERE id=?1",[&pulse],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if let Some(kind) = checkpoint {
                break format!("{stage}/{kind}");
            }
            if status == "failed"
                || start.elapsed() > Duration::from_secs(if live { 300 } else { 90 })
            {
                break format!("{stage}/{status}");
            }
            std::thread::sleep(Duration::from_millis(30));
        };
        let elapsed = start.elapsed().as_secs_f64() * 1000.;
        let stages:Vec<Value>=db.prepare("SELECT kind,(julianday(finished_at)-julianday(started_at))*86400000 FROM jobs WHERE pulsation_id=?1 AND kind LIKE 'run_stage::%' ORDER BY created_at")?.query_map([&pulse],|r|Ok(json!({"stage":r.get::<_,String>(0)?,"elapsed_ms":r.get::<_,Option<f64>>(1)?})))?.collect::<rusqlite::Result<_>>()?;
        let succeeded = terminal == "SPECIFICATION/artifact_review";
        if !live && !succeeded {
            let reason: Option<String> = db.query_row(
                "SELECT group_concat(last_error) FROM jobs WHERE pulsation_id=?1",
                [&pulse],
                |r| r.get(0),
            )?;
            return Err(format!("fixture pulse stopped at {terminal}: {reason:?}").into());
        }
        rows.push(json!({"feature":task.feature,"task":task.description,"success":succeeded,"endpoint":terminal,"elapsed_ms":elapsed,"stages":stages,"turns":turns.lock().unwrap().clone()}));
        service
            .cancel_pulsation_jobs(CancelPulsationJobsRequest {
                pulsation_id: pulse,
            })
            .map_err(|e| format!("{e:?}"))?;
    }
    context.runtime.block_on(context.core.shutdown());
    Ok(
        json!({"transport":if live{"authenticated Claude CLI"}else{"cli_fixture bridge"},"target":"specification delivered at first review checkpoint","tasks":rows}),
    )
}
