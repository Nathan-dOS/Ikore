#![allow(
    clippy::unwrap_used,
    reason = "Test-only harnesses fail immediately when fixture setup is invalid"
)]
//! Cost/quality qualification of pipeline v4 (ADR-022, M7).
//!
//! Fixture mode (no cost, default):
//!   cargo run -p hvc-test-support --example `composition_benchmark` -- --mode fixture
//! Pilot mode (real provider, explicit spend limit, never in CI):
//!   cargo run -p hvc-test-support --example `composition_benchmark` -- --mode pilot \
//!     --provider-id openai --endpoint-url <https://api.openai.com/v1> \
//!     --model-id <model> --api-key-env `OPENAI_API_KEY` --spend-limit-micro-usd 500000
//! Optional filters: --arm A,B,C,D  --fixture <id-substring>  --held-out (include held-out set)
//!   --output-dir quality
use hvc_test_support::composition_benchmark::{
    Arm, FIXTURE_VERSION, Fixture, Mode, PilotConfig, fixtures_sha256, load_fixtures,
    qualification, render_markdown, run_observation, summarize,
};
use serde_json::json;
use std::collections::BTreeMap;

fn command(program: &str, arguments: &[&str]) -> String {
    std::process::Command::new(program)
        .args(arguments)
        .output()
        .ok()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut options: BTreeMap<String, String> = BTreeMap::new();
    let mut flags: Vec<String> = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let key = args[index].trim_start_matches("--").to_owned();
        if index + 1 < args.len() && !args[index + 1].starts_with("--") {
            options.insert(key, args[index + 1].clone());
            index += 2;
        } else {
            flags.push(key);
            index += 1;
        }
    }
    let mode = match options.get("mode").map(String::as_str).unwrap_or("fixture") {
        "fixture" => Mode::Fixture,
        "pilot" => {
            let required = |name: &str| {
                options.get(name).cloned().unwrap_or_else(|| {
                    eprintln!("pilot mode requires --{name}");
                    std::process::exit(2);
                })
            };
            let key_env = required("api-key-env");
            let api_key = std::env::var(&key_env).unwrap_or_else(|_| {
                eprintln!("environment variable {key_env} is not set");
                std::process::exit(2);
            });
            let spend_limit_micro_usd: u64 = required("spend-limit-micro-usd")
                .parse()
                .unwrap_or_else(|_| {
                    eprintln!("--spend-limit-micro-usd must be an integer");
                    std::process::exit(2);
                });
            if spend_limit_micro_usd == 0 {
                eprintln!("--spend-limit-micro-usd must be positive");
                std::process::exit(2);
            }
            if std::env::var_os("CI").is_some() {
                eprintln!("pilot mode never runs in CI");
                std::process::exit(2);
            }
            Mode::Pilot(PilotConfig {
                provider_id: required("provider-id"),
                endpoint_url: required("endpoint-url"),
                model_id: required("model-id"),
                api_key,
                spend_limit_micro_usd,
            })
        }
        other => {
            eprintln!("unknown mode `{other}` (fixture | pilot)");
            std::process::exit(2);
        }
    };
    let arms: Vec<Arm> = options
        .get("arm")
        .map(|list| {
            list.split(',')
                .map(|a| Arm::parse(a).unwrap_or_else(|| panic!("unknown arm `{a}`")))
                .collect()
        })
        .unwrap_or_else(|| Arm::ALL.to_vec());
    let include_held_out = flags.iter().any(|f| f == "held-out");
    let filter = options.get("fixture").cloned();
    let fixtures: Vec<Fixture> = load_fixtures()
        .into_iter()
        .filter(|f| include_held_out || !f.held_out)
        .filter(|f| {
            filter
                .as_ref()
                .is_none_or(|needle| f.id.contains(needle.as_str()))
        })
        .collect();
    let fixtures_sha256 = fixtures_sha256(&fixtures);

    let mut observations = Vec::new();
    let mut spent_micro_usd: u64 = 0;
    'outer: for fixture in &fixtures {
        for arm in &arms {
            eprintln!("[{:?}] {}", arm, fixture.id);
            let observation = run_observation(*arm, fixture, &mode);
            eprintln!(
                "  completed={} correct={} calls={} rejected={} interventions={} input={} output={} wall={}ms {}",
                observation.completed,
                observation.correct,
                observation.provider_calls,
                observation.rejected_attempts,
                observation.interventions,
                observation.input_tokens,
                observation.output_tokens,
                observation.wall_ms,
                observation.diagnostics.join("; ")
            );
            if let Mode::Pilot(pilot) = &mode {
                spent_micro_usd += observation.estimated_cost_micro_usd.unwrap_or(0);
                let unknown = observation.estimated_cost_micro_usd.is_none();
                observations.push(observation);
                if spent_micro_usd >= pilot.spend_limit_micro_usd {
                    eprintln!("spend limit reached ({spent_micro_usd} µUSD); stopping");
                    break 'outer;
                }
                if unknown {
                    eprintln!(
                        "provider reported no cost; stopping after one pulsation to respect the spend limit"
                    );
                    break 'outer;
                }
            } else {
                observations.push(observation);
            }
        }
    }

    let summary = summarize(&observations);
    let qualification = qualification(&observations);
    let generated_at = chrono::Utc::now();
    let meta = json!({
        "generated_at": generated_at.to_rfc3339(),
        "mode": mode.label(),
        "code_base": command("git", &["rev-parse", "HEAD"]),
        "branch": command("git", &["rev-parse", "--abbrev-ref", "HEAD"]),
        "fixture_version": FIXTURE_VERSION,
        "fixtures_sha256": fixtures_sha256,
        "fixtures": fixtures.iter().map(|f| json!({"id": f.id, "family": f.family, "held_out": f.held_out, "tasks": f.tasks.len()})).collect::<Vec<_>>(),
        "arms": arms,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "model_id": match &mode { Mode::Pilot(p) => json!(p.model_id), Mode::Fixture => json!(null) },
        "spend_limit_micro_usd": match &mode { Mode::Pilot(p) => json!(p.spend_limit_micro_usd), Mode::Fixture => json!(null) },
        "spent_micro_usd": match &mode { Mode::Pilot(_) => json!(spent_micro_usd), Mode::Fixture => json!(null) },
    });
    let output_dir = options
        .get("output-dir")
        .cloned()
        .unwrap_or_else(|| "quality".into());
    std::fs::create_dir_all(&output_dir).unwrap();
    let stem = format!(
        "{output_dir}/stage-composition-benchmark-{}{}",
        generated_at.format("%Y-%m-%d"),
        if matches!(mode, Mode::Pilot(_)) {
            "-pilot"
        } else {
            ""
        }
    );
    let report = json!({
        "meta": meta,
        "summary": summary,
        "qualification": qualification,
        "observations": observations,
    });
    std::fs::write(
        format!("{stem}.json"),
        serde_json::to_string_pretty(&report).unwrap(),
    )
    .unwrap();
    std::fs::write(
        format!("{stem}.md"),
        render_markdown(&report["meta"], &summary, &qualification, &observations),
    )
    .unwrap();
    println!("{stem}.json\n{stem}.md");
    let failed = observations.iter().filter(|o| !o.correct).count();
    if failed > 0 {
        eprintln!("{failed} observation(s) did not pass the oracle");
        std::process::exit(1);
    }
}
