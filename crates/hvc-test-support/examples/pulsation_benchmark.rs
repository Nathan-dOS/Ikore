use hvc_test_support::pulsation_benchmark::*;
use serde_json::json;

fn main() {
    let mut args = std::env::args().skip(1);
    let output = args
        .next()
        .expect("usage: pulsation_benchmark OUTPUT.json [pairs=12]");
    let pairs: usize = args
        .next()
        .map(|v| v.parse().expect("pairs integer"))
        .unwrap_or(12);
    assert!((1..=64).contains(&pairs));
    let mut samples = vec![];
    for scenario in [
        Scenario::NoQueue,
        Scenario::QueueAbc,
        Scenario::EditWhileWaiting,
    ] {
        for iteration in 0..=pairs {
            // Counterbalanced pair order; one excluded warmup per variant/scenario.
            for enabled in if iteration % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            } {
                eprintln!("{scenario:?} pair {iteration}/{pairs} preparation={enabled}");
                samples.push(run_sample(scenario, enabled, iteration, iteration == 0));
            }
        }
    }
    let mut summary = vec![];
    for scenario in [
        Scenario::NoQueue,
        Scenario::QueueAbc,
        Scenario::EditWhileWaiting,
    ] {
        for enabled in [false, true] {
            let group: Vec<_> = samples
                .iter()
                .filter(|s| !s.warmup && s.scenario == scenario && s.preparation_enabled == enabled)
                .collect();
            let labels: &[&str] = if scenario == Scenario::NoQueue {
                &["A"]
            } else {
                &["B", "C"]
            };
            for (position, label) in labels.iter().enumerate() {
                let latencies: Vec<_> = group.iter().map(|s| s.base_to_spec_ms[position]).collect();
                // Base commit lies between initiating approval and observing the committed epoch.
                // Keep that uncertainty visible instead of calling the observation an exact commit.
                let upper: Vec<_> = group
                    .iter()
                    .map(|s| {
                        s.base_to_spec_ms[position]
                            + s.approval_to_base_ms.get(position).copied().unwrap_or(0.0)
                    })
                    .collect();
                let collection: Vec<_> = group
                    .iter()
                    .flat_map(|s| &s.context_metrics)
                    .filter(|m| m["phase"] == "specification" && m["pulse_label"] == *label)
                    .map(|m| m["elapsed_us"].as_u64().unwrap() as f64 / 1000.0)
                    .collect();
                summary.push(json!({"scenario":scenario,"pulse":label,"preparation_enabled":enabled,"samples":latencies.len(),"base_to_spec_observed_ms":{"p50":percentile(&latencies,50),"p95":percentile(&latencies,95)},"approval_to_spec_upper_ms":{"p50":percentile(&upper,50),"p95":percentile(&upper,95)},"spec_collection_ms":{"p50":percentile(&collection,50),"p95":percentile(&collection,95)}}));
            }
        }
    }
    let command = |program: &str, arguments: &[&str]| {
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
    };
    let report = json!({"schema_version":1,"fixture_version":FIXTURE_VERSION,"generated_at":chrono::Utc::now().to_rfc3339(),"code_base":command("git", &["rev-parse","HEAD"]),"working_diff":command("git", &["-c","core.excludesFile=","diff","--stat"]),"rustc":command("rustc", &["-Vv"]),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"release":!cfg!(debug_assertions),"pairs":pairs,"idle_window_ms":IDLE_WINDOW_MS,"provider_delay_ms":PROVIDER_DELAY_MS,"observer_poll_ms":2,"learning_enabled":false,"embedding_enabled":false,"percentile_method":"nearest_rank","notes":"Loopback synthetic provider/no-op delivery. Monotonic committed-state observations (poll + DB latency); no queue origin includes creation on an already-ready base. On/off OS cache counterbalanced, not cold-start. Token usage simulated chars/4; no claim about real-provider cost. No user profile or external service.","summary":summary,"samples":samples});
    std::fs::write(output, serde_json::to_string_pretty(&report).unwrap()).expect("write report");
    println!(
        "{}",
        serde_json::to_string_pretty(&report["summary"]).unwrap()
    );
}
