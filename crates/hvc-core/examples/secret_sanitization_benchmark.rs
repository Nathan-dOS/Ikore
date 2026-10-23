//! Hermetic cost probe. Run with cargo run --locked -p hvc-core --example `secret_sanitization_benchmark`.
use hvc_core::secret_redactor::{MAX_SANITIZATION_BYTES, redact_context};
use serde_json::json;
use std::time::Instant;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut measurements = Vec::new();
    for bytes in [1024, 64 * 1024, 1024 * 1024, MAX_SANITIZATION_BYTES] {
        let mut input = "pub fn pure() {}\n".repeat(bytes / 17 + 1);
        let suffix = "\nghp_syntheticabcdefghijklmnop\n";
        input.truncate(bytes - suffix.len());
        input.push_str(suffix);
        let _ = redact_context(&input)?;
        let mut samples = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            let projection = redact_context(&input)?;
            assert_eq!(projection.spans.len(), 1);
            samples.push(started.elapsed().as_micros() as u64);
            std::hint::black_box(projection);
        }
        samples.sort_unstable();
        measurements.push(json!({"kind":"text","bytes":input.len(),"samples":samples.len(),"p50_us":samples[2],"p95_us":samples[4]}));
    }
    let mut input = "pub fn pure() {}\n".repeat(1024 * 1024 / 17 + 1);
    input.truncate(1024 * 1024);
    let started = Instant::now();
    for _ in 0..64 {
        std::hint::black_box(redact_context(&input)?);
    }
    measurements.push(json!({"kind":"snapshot_sequential","files":64,"bytes":input.len()*64,"elapsed_us":started.elapsed().as_micros() as u64}));
    println!(
        "{}",
        json!({"policy":hvc_core::secret_redactor::SANITIZATION_POLICY_VERSION,"profile":if cfg!(debug_assertions) { "debug" } else { "release" },"measurements":measurements})
    );
    Ok(())
}
