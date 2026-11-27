//! Run through scripts/sky-baseline.ps1 to include the child process working set.
use ikore_test_support::sky_bench::{self as bench, Corpus, Result, experiments, legacy};
use std::{env, fs, path::Path};
fn main() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    if let [command, input, output] = args.as_slice()
        && command == "replay-files"
    {
        return legacy::replay::file_hits(&Corpus::load()?, Path::new(input), Path::new(output));
    }
    if let [command, config, directory] = args.as_slice()
        && command == "generate-scale"
    {
        let config: bench::scale::Scale = serde_yaml::from_slice(&fs::read(config)?)?;
        let directory = empty_destination(directory)?;
        return config.generate()?.render(directory);
    }
    let [phase, out] = args.as_slice() else {
        return Err("expected a measurement and output file, generate <empty-directory>, or generate-scale <config.yaml> <empty-directory>".into());
    };
    let manifest = bench::manifest()?;
    let corpus = Corpus::load()?;
    if phase == "generate" {
        let directory = empty_destination(out)?;
        bench::materialize(directory)?;
        corpus.render(directory)?;
        return corpus.validate(directory);
    }
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

fn empty_destination(directory: &str) -> Result<&Path> {
    let path = Path::new(directory);
    fs::create_dir_all(path)?;
    if fs::read_dir(path)?.next().is_some() {
        return Err("generation requires an empty destination".into());
    }
    Ok(path)
}
