#![allow(
    clippy::unwrap_used,
    reason = "Test-only harnesses fail immediately when fixture setup is invalid"
)]
use hvc_test_support::{
    brownfield_benchmark::percentile_u64,
    brownfield_graph_benchmark::{
        BrownfieldGraphSample, FIXTURE_VERSION, RelationalCase, TaskContextMode, run_sample,
    },
    pulsation_benchmark::percentile,
};
use serde_json::{Value, json};

fn command(program: &str, arguments: &[&str]) -> String {
    String::from_utf8(
        std::process::Command::new(program)
            .args(arguments)
            .output()
            .expect("environment command")
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string()
}

fn summary(
    samples: &[BrownfieldGraphSample],
    case: RelationalCase,
    mode: TaskContextMode,
) -> Value {
    let group: Vec<_> = samples
        .iter()
        .filter(|sample| !sample.warmup && sample.case == case && sample.mode == mode)
        .collect();
    let latency: Vec<_> = group
        .iter()
        .map(|sample| sample.task.specification_checkpoint_ms)
        .collect();
    let input_tokens: Vec<_> = group
        .iter()
        .map(|sample| sample.task.simulated_input_tokens)
        .collect();
    let output_tokens: Vec<_> = group
        .iter()
        .map(|sample| sample.task.simulated_output_tokens)
        .collect();
    let quality: Vec<_> = group
        .iter()
        .map(|sample| u64::from(sample.task.quality_score))
        .collect();
    let questions: Vec<_> = group
        .iter()
        .map(|sample| u64::from(sample.task.clarification_questions))
        .collect();
    let source_counts: Vec<_> = group
        .iter()
        .map(|sample| sample.task.context_sources.len() as u64)
        .collect();
    json!({
        "case": case,
        "mode": mode,
        "samples": group.len(),
        "quality": {
            "min": quality.iter().min().unwrap(),
            "max": quality.iter().max().unwrap(),
            "rubric_max": group[0].task.quality_max
        },
        "clarification_questions": {
            "min": questions.iter().min().unwrap(),
            "max": questions.iter().max().unwrap()
        },
        "specification_checkpoint_ms": {
            "p50": percentile(&latency, 50),
            "p95": percentile(&latency, 95)
        },
        "simulated_input_tokens": {
            "p50": percentile_u64(&input_tokens, 50),
            "p95": percentile_u64(&input_tokens, 95)
        },
        "simulated_output_tokens": {
            "p50": percentile_u64(&output_tokens, 50),
            "p95": percentile_u64(&output_tokens, 95)
        },
        "context_sources": {
            "min": source_counts.iter().min().unwrap(),
            "max": source_counts.iter().max().unwrap()
        },
        "all_grounded": group.iter().all(|sample| sample.task.specification_grounded),
        "all_graph_context_current": group.iter().all(|sample| sample.task.graph_resolution_context.is_some()),
        "representative_sources": group[0].task.context_sources,
        "representative_quality_evidence": group[0].task.quality_evidence
    })
}

fn delta_percent(after: f64, before: f64) -> f64 {
    if before == 0.0 {
        0.0
    } else {
        (after / before - 1.0) * 100.0
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let output = args.next().expect(
        "usage: brownfield_graph_benchmark OUTPUT.json [pairs=5] [evaluate_only|consented_default]",
    );
    let pairs: usize = args
        .next()
        .map(|value| value.parse().expect("pairs integer"))
        .unwrap_or(5);
    let activation_policy = args.next().unwrap_or_else(|| "evaluate_only".into());
    assert!((1..=16).contains(&pairs));
    assert!(matches!(
        activation_policy.as_str(),
        "evaluate_only" | "consented_default"
    ));
    let mut samples = Vec::new();
    for iteration in 0..=pairs {
        for case in RelationalCase::ALL {
            let modes = if (iteration + case as usize).is_multiple_of(2) {
                [TaskContextMode::OnDemand, TaskContextMode::ResolvedGraph]
            } else {
                [TaskContextMode::ResolvedGraph, TaskContextMode::OnDemand]
            };
            let start = samples.len();
            for mode in modes {
                samples.push(run_sample(mode, case, iteration, iteration == 0));
            }
            assert_eq!(
                samples[start].fixture_sha256,
                samples[start + 1].fixture_sha256
            );
        }
    }
    let mut case_summaries = Vec::new();
    let mut quality_improved_cases = Vec::new();
    let mut quality_regressed_cases = Vec::new();
    let mut total_on_demand_tokens = 0_u64;
    let mut total_graph_tokens = 0_u64;
    let mut total_on_demand_latency = 0.0;
    let mut total_graph_latency = 0.0;
    for case in RelationalCase::ALL {
        let on_demand = summary(&samples, case, TaskContextMode::OnDemand);
        let graph = summary(&samples, case, TaskContextMode::ResolvedGraph);
        let on_quality = on_demand["quality"]["min"].as_u64().unwrap();
        let graph_quality = graph["quality"]["min"].as_u64().unwrap();
        if graph_quality > on_quality {
            quality_improved_cases.push(case);
        } else if graph_quality < on_quality {
            quality_regressed_cases.push(case);
        }
        total_on_demand_tokens += on_demand["simulated_input_tokens"]["p50"].as_u64().unwrap();
        total_graph_tokens += graph["simulated_input_tokens"]["p50"].as_u64().unwrap();
        total_on_demand_latency += on_demand["specification_checkpoint_ms"]["p50"]
            .as_f64()
            .unwrap();
        total_graph_latency += graph["specification_checkpoint_ms"]["p50"]
            .as_f64()
            .unwrap();
        case_summaries.push(json!({"case": case, "on_demand": on_demand, "resolved_graph": graph}));
    }
    let token_delta = delta_percent(total_graph_tokens as f64, total_on_demand_tokens as f64);
    let latency_delta = delta_percent(total_graph_latency, total_on_demand_latency);
    let ambiguous_safe = samples
        .iter()
        .filter(|sample| {
            !sample.warmup
                && sample.mode == TaskContextMode::ResolvedGraph
                && sample.case == RelationalCase::AmbiguousDoesNotExpand
        })
        .all(|sample| {
            sample.task.context_sources == ["src/ambiguous.ts"]
                && sample.task.resolved_references == 0
                && sample.task.persisted_ambiguous_edges > 0
        });
    let control_no_regression = {
        let item = case_summaries
            .iter()
            .find(|value| value["case"] == json!(RelationalCase::SimpleControl))
            .unwrap();
        item["resolved_graph"]["quality"]["min"].as_u64().unwrap()
            >= item["on_demand"]["quality"]["min"].as_u64().unwrap()
    };
    let relational_quality_benefit = RelationalCase::ALL
        .iter()
        .filter(|case| case.is_relational())
        .all(|case| quality_improved_cases.contains(case));
    let no_regression = quality_regressed_cases.is_empty()
        && ambiguous_safe
        && control_no_regression
        && token_delta <= 5.0
        && latency_delta <= 10.0;
    let material_benefit =
        relational_quality_benefit || token_delta <= -5.0 || latency_delta <= -10.0;
    let gate_passed = material_benefit && no_regression;
    let default_changed = gate_passed && activation_policy == "consented_default";
    let report = json!({
        "schema_version": 1,
        "experiment_version": if activation_policy == "consented_default" {"s3-context-efficiency-v1"} else {"rs2-structural-benefit-v1"},
        "fixture_version": FIXTURE_VERSION,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "code_base": command("git", &["rev-parse", "HEAD"]),
        "working_diff": command("git", &["-c", "core.excludesFile=", "diff", "--stat"]),
        "rustc": command("rustc", &["-Vv"]),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "release": !cfg!(debug_assertions),
        "pairs": pairs,
        "pair_order": "counterbalanced per case; one excluded warmup per mode",
        "provider": "loopback deterministic provider with fixed 10 ms delay; identical response policy per paired mode",
        "token_measurement": "provider-reported fixture usage: prompt chars/4; not a tokenizer or cost estimate",
        "percentile_method": "nearest_rank",
        "scope": "real AppCore worker, SQLite and repository rereads; isolated repositories with identical hashes; graph mapping excluded from task latency",
        "case_summaries": case_summaries,
        "gate": {
            "passed": gate_passed,
            "material_benefit": material_benefit,
            "no_regression": no_regression,
            "relational_quality_benefit": relational_quality_benefit,
            "quality_improved_cases": quality_improved_cases,
            "quality_regressed_cases": quality_regressed_cases,
            "ambiguous_case_did_not_expand": ambiguous_safe,
            "simple_control_no_quality_regression": control_no_regression,
            "aggregate_input_token_p50_delta_percent": token_delta,
            "aggregate_specification_latency_p50_delta_percent": latency_delta,
            "rule": "benefit by quality, fewer questions, >=5% token reduction or >=10% latency reduction, with no quality/safety regression and <=5% token / <=10% latency increase"
        },
        "activation_decision": {
            "default_changed": default_changed,
            "brownfield_consent_required": true,
            "mode": if default_changed {
                "resolved_graph_default_after_project_consent"
            } else if gate_passed {
                "eligible_for_separate_activation_decision"
            } else {
                "keep_opt_in_disabled_by_default"
            }
        },
        "samples": samples
    });
    std::fs::write(&output, serde_json::to_string_pretty(&report).unwrap())
        .expect("write RS2 report");
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "gate": report["gate"],
            "activation_decision": report["activation_decision"],
            "case_summaries": report["case_summaries"]
        }))
        .unwrap()
    );
}
