#![allow(
    clippy::unwrap_used,
    reason = "Test-only harnesses fail immediately when fixture setup is invalid"
)]
use ikore_test_support::brownfield_benchmark::{
    BrownfieldSample, FIXTURE_VERSION, TASK_DESCRIPTION, VaultMode, percentile_u64, run_sample,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

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

fn task_summary(samples: &[BrownfieldSample], mode: VaultMode) -> Value {
    let group: Vec<_> = samples
        .iter()
        .filter(|sample| !sample.warmup && sample.mode == mode)
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
        .map(|sample| sample.task.specification_quality_score as u64)
        .collect();
    let questions: Vec<_> = group
        .iter()
        .map(|sample| sample.task.clarification_questions as u64)
        .collect();
    json!({
        "mode": mode,
        "samples": group.len(),
        "specification_checkpoint_ms": {
            "p50": ikore_test_support::pulsation_benchmark::percentile(&latency, 50),
            "p95": ikore_test_support::pulsation_benchmark::percentile(&latency, 95)
        },
        "simulated_input_tokens": {
            "p50": percentile_u64(&input_tokens, 50),
            "p95": percentile_u64(&input_tokens, 95)
        },
        "simulated_output_tokens": {
            "p50": percentile_u64(&output_tokens, 50),
            "p95": percentile_u64(&output_tokens, 95)
        },
        "specification_quality_score": {
            "min": quality.iter().min().unwrap(),
            "max": quality.iter().max().unwrap(),
            "rubric_max": group[0].task.specification_quality_max
        },
        "clarification_questions": {
            "min": questions.iter().min().unwrap(),
            "max": questions.iter().max().unwrap()
        }
    })
}

fn mapping_summary(samples: &[BrownfieldSample]) -> Value {
    let mapped: Vec<_> = samples
        .iter()
        .filter(|sample| !sample.warmup)
        .filter_map(|sample| sample.mapping.as_ref())
        .collect();
    let cold: Vec<_> = mapped.iter().map(|item| item.cold.elapsed_us).collect();
    let warm: Vec<_> = mapped.iter().map(|item| item.warm.elapsed_us).collect();
    let invalidation: Vec<_> = mapped
        .iter()
        .map(|item| item.exact_invalidation.elapsed_us)
        .collect();
    let mut language_timings: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for observation in &mapped {
        for language in &observation.languages {
            language_timings
                .entry(language.language.clone())
                .or_default()
                .extend(&language.parse_elapsed_us);
        }
    }
    let languages: Vec<_> = language_timings
        .into_iter()
        .map(|(language, values)| {
            json!({
                "language": language,
                "observations": values.len(),
                "parse_elapsed_us": {
                    "p50": percentile_u64(&values, 50),
                    "p95": percentile_u64(&values, 95)
                }
            })
        })
        .collect();
    let representative = mapped[0];
    let resolution_ratio = if representative.resolution.references == 0 {
        0.0
    } else {
        f64::from(representative.resolution.resolved)
            / f64::from(representative.resolution.references)
    };
    json!({
        "samples": mapped.len(),
        "cold_elapsed_us": {"p50": percentile_u64(&cold, 50), "p95": percentile_u64(&cold, 95)},
        "warm_elapsed_us": {"p50": percentile_u64(&warm, 50), "p95": percentile_u64(&warm, 95)},
        "exact_invalidation_elapsed_us": {"p50": percentile_u64(&invalidation, 50), "p95": percentile_u64(&invalidation, 95)},
        "cold_sources": representative.cold.cache_misses,
        "warm_processed_sources": representative.warm.cache_hits + representative.warm.cache_misses,
        "exact_invalidation_processed_sources": representative.exact_invalidation.cache_hits + representative.exact_invalidation.cache_misses,
        "warm_resolution_reused": representative.warm_resolution_reused,
        "inventory_known_sources": representative.inventory_known_sources,
        "eligible_source_denominator": representative.eligible_source_denominator,
        "extracted_valid_sources": representative.extracted_valid_sources,
        "extracted_partial_sources": representative.extracted_partial_sources,
        "extraction_progress_basis_points": representative.extraction_progress_basis_points,
        "resolution": {
            "references": representative.resolution.references,
            "resolved": representative.resolution.resolved,
            "ambiguous": representative.resolution.ambiguous,
            "unresolved": representative.resolution.unresolved,
            "external": representative.resolution.external,
            "resolved_ratio": resolution_ratio,
            "state": representative.resolution.state,
            "resolver_version": representative.resolution.resolver_version,
            "elapsed_us": representative.resolution.elapsed_us,
            "precision_basis_points": representative.resolution.precision_basis_points,
            "recall_basis_points": representative.resolution.recall_basis_points,
            "expected_resolved": representative.resolution.expected_resolved,
            "true_resolved": representative.resolution.true_resolved,
            "false_resolved": representative.resolution.false_resolved,
            "classification_mismatches": representative.resolution.classification_mismatches
        },
        "languages": languages
    })
}

fn decision(task: &[Value]) -> Value {
    let empty = &task[0];
    let mapped = &task[1];
    let empty_latency = empty["specification_checkpoint_ms"]["p50"]
        .as_f64()
        .unwrap();
    let mapped_latency = mapped["specification_checkpoint_ms"]["p50"]
        .as_f64()
        .unwrap();
    let empty_tokens = empty["simulated_input_tokens"]["p50"].as_u64().unwrap();
    let mapped_tokens = mapped["simulated_input_tokens"]["p50"].as_u64().unwrap();
    let empty_quality = empty["specification_quality_score"]["min"]
        .as_u64()
        .unwrap();
    let mapped_quality = mapped["specification_quality_score"]["min"]
        .as_u64()
        .unwrap();
    let empty_questions = empty["clarification_questions"]["max"].as_u64().unwrap();
    let mapped_questions = mapped["clarification_questions"]["max"].as_u64().unwrap();
    let latency_delta_percent = (mapped_latency / empty_latency - 1.0) * 100.0;
    let token_delta_percent = if empty_tokens == 0 {
        0.0
    } else {
        (mapped_tokens as f64 / empty_tokens as f64 - 1.0) * 100.0
    };
    let material_benefit = mapped_quality > empty_quality
        || mapped_questions < empty_questions
        || token_delta_percent <= -5.0
        || latency_delta_percent <= -10.0;
    let regression = mapped_quality < empty_quality
        || mapped_questions > empty_questions
        || token_delta_percent > 5.0
        || latency_delta_percent > 10.0;
    let activate = material_benefit && !regression;
    json!({
        "automatic_activation": activate,
        "mode": if activate {"eligible_for_activation"} else {"keep_opt_in_disabled_by_default"},
        "rule": "Ativar automaticamente somente com benefício material (qualidade maior, menos perguntas, >=5% menos tokens ou >=10% menor latência) e sem regressão simétrica.",
        "evidence": {
            "quality_min_delta": mapped_quality as i64 - empty_quality as i64,
            "clarification_max_delta": mapped_questions as i64 - empty_questions as i64,
            "simulated_input_token_p50_delta_percent": token_delta_percent,
            "specification_latency_p50_delta_percent": latency_delta_percent,
            "material_benefit": material_benefit,
            "regression": regression
        },
        "architectural_observation": "A coleta segura da Specification permanece on-demand e não reutiliza o cache estrutural brownfield; portanto igualdade de qualidade/tokens é esperada até uma integração explicitamente projetada e cercada."
    })
}

fn main() {
    let mut args = std::env::args().skip(1);
    let output = args
        .next()
        .expect("usage: brownfield_benchmark OUTPUT.json [pairs=5]");
    let pairs: usize = args
        .next()
        .map(|value| value.parse().expect("pairs integer"))
        .unwrap_or(5);
    assert!((1..=32).contains(&pairs));
    let mut samples = vec![];
    for iteration in 0..=pairs {
        for mode in if iteration % 2 == 0 {
            [VaultMode::EmptyVault, VaultMode::PartialCoverage]
        } else {
            [VaultMode::PartialCoverage, VaultMode::EmptyVault]
        } {
            samples.push(run_sample(mode, iteration, iteration == 0));
        }
        let pair = &samples[samples.len() - 2..];
        assert_eq!(
            pair[0].initial_fixture_sha256,
            pair[1].initial_fixture_sha256
        );
        assert_eq!(
            pair[0].task_repository_sha256,
            pair[1].task_repository_sha256
        );
    }
    let task = vec![
        task_summary(&samples, VaultMode::EmptyVault),
        task_summary(&samples, VaultMode::PartialCoverage),
    ];
    let mapping = mapping_summary(&samples);
    let decision = decision(&task);
    let calibrated_limits = json!({
        "decision": "retain_bounded_defaults",
        "max_files": 2000,
        "max_total_read_bytes": 16 * 1024 * 1024,
        "max_file_bytes": 1024 * 1024,
        "max_duration_ms": 2000,
        "reason": "O corpus RS1 permanece abaixo do orçamento temporal e a invalidação é exata; o corpus não sustenta reduzir os limites de segurança nem ampliá-los sem evidência de repositórios maiores."
    });
    let report = json!({
        "schema_version": 1,
        "fixture_version": FIXTURE_VERSION,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "code_base": command("git", &["rev-parse", "HEAD"]),
        "working_diff": command("git", &["-c", "core.excludesFile=", "diff", "--stat"]),
        "rustc": command("rustc", &["-Vv"]),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "release": !cfg!(debug_assertions),
        "pairs": pairs,
        "pair_order": "counterbalanced; one excluded warmup per mode",
        "task_description": TASK_DESCRIPTION,
        "provider": "loopback synthetic provider with fixed 10 ms delay",
        "token_measurement": "provider-reported fixture usage: prompt chars/4; not a tokenizer or cost estimate",
        "percentile_method": "nearest_rank",
        "scope": "real AppCore worker, watcher and SQLite; isolated equivalent repositories; no external service, profile or arbitrary generated write",
        "mapping_summary": mapping,
        "task_summary": task,
        "calibrated_limits": calibrated_limits,
        "activation_decision": decision,
        "samples": samples
    });
    std::fs::write(&output, serde_json::to_string_pretty(&report).unwrap())
        .expect("write RS1 report");
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "mapping_summary": report["mapping_summary"],
            "task_summary": report["task_summary"],
            "calibrated_limits": report["calibrated_limits"],
            "activation_decision": report["activation_decision"]
        }))
        .unwrap()
    );
}
