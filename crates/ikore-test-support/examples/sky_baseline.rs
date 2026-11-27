//! Run through scripts/sky-baseline.ps1 to include the child process working set.
use ikore_test_support::sky_bench::{self as bench, Corpus, Result, experiments, legacy};
use std::{env, fs, path::Path};
fn main() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    let [phase, out] = args.as_slice() else {
        return Err("usage: sky_baseline <retrieval|scale-100|scale-1000|scale-10000|mutations|pulsations|e7|sqlite> <output.json>".into());
    };
    let manifest = bench::manifest()?;
    let corpus = Corpus::load()?;
    let raw = match phase.as_str() {
        "sqlite" => experiments::database::run()?,
        "e7" => tokio::runtime::Runtime::new()?.block_on(experiments::e7(&corpus))?,
        "retrieval" => legacy::run(&corpus, None)?,
        "mutations" => legacy::mutations(&corpus)?,
        "pulsations" | "live" => legacy::pulsations(&corpus, phase == "live")?,
        scale if scale.starts_with("scale-") => {
            let n: usize = scale[6..].parse()?;
            let configs: Vec<bench::scale::Scale> = bench::data("scale.yaml")?;
            let config = configs
                .iter()
                .find(|c| c.notes == n)
                .ok_or("unknown scale")?;
            legacy::run(&config.generate()?, Some(n))?
        }
        _ => return Err("unknown measurement".into()),
    };
    let output = serde_json::json!({"phase":phase,"manifest_sha256":manifest,"results":raw});
    let path = Path::new(out);
    fs::write(path, serde_json::to_vec(&output)?)?;
    Ok(())
}
