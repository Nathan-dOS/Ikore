//! Recompute file-only hits without repeating timed calls. Fail on any unverified byte.
use super::{Corpus, Result, Value, fs, hash, metrics};
use std::{
    collections::BTreeMap,
    path::{Component, Path},
};

pub fn file_hits(corpus: &Corpus, input: &Path, output: &Path) -> Result<()> {
    let mut raw: Value = serde_json::from_slice(&fs::read(input)?)?;
    if raw["phase"] != "retrieval" || raw["manifest_sha256"] != crate::sky_bench::manifest()? {
        return Err("expected a retrieval measurement from the frozen corpus".into());
    }
    let tmp = tempfile::tempdir()?;
    corpus.render_formats(tmp.path(), true, false)?;
    let questions: BTreeMap<_, _> = corpus
        .questions
        .iter()
        .map(|q| (q.id.as_str(), q))
        .collect();
    let samples = raw["results"]["samples"]
        .as_array_mut()
        .ok_or("missing samples")?;
    let mut cache = BTreeMap::new();
    for row in samples.iter_mut() {
        let q = questions
            .get(row["question"].as_str().ok_or("missing question")?)
            .ok_or("unknown question")?;
        let mut hit = row["metrics"]["line_hit"].as_bool().unwrap_or(false);
        for item in row["delivered"]
            .as_array_mut()
            .ok_or("missing delivered items")?
        {
            let path = item["path"].as_str().ok_or("missing item path")?;
            if path == "vault://exact-code-context" {
                return Err("replay requires retained bytes for a derived projection".into());
            }
            if !path.starts_with("knowledge/obsidian/")
                || !Path::new(path)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_)))
            {
                return Err("invalid legacy note path".into());
            }
            let key = (
                path.to_owned(),
                item["sha256"]
                    .as_str()
                    .ok_or("missing item digest")?
                    .to_owned(),
            );
            if !cache.contains_key(&key) {
                let source = fs::read_to_string(tmp.path().join(path))?;
                let len = item["characters"]
                    .as_u64()
                    .ok_or("missing delivered length")? as usize;
                let body: String = source.chars().take(len).collect();
                if body.chars().count() != len || hash(body.as_bytes()) != key.1 {
                    return Err(format!("could not verify captured body of {path}").into());
                }
                cache.insert(key.clone(), metrics::file_references(&body));
            }
            let paths = &cache[&key];
            hit |= q
                .expected_location
                .as_ref()
                .is_some_and(|loc| paths.contains(&loc.path));
            item["code_files"] = serde_json::to_value(paths)?;
        }
        row["metrics"]["file_hit"] =
            serde_json::to_value(q.expected_location.as_ref().map(|_| hit))?;
    }
    let mut summary = serde_json::json!({});
    for mode in ["native", "question"] {
        let rows: Vec<_> = samples
            .iter()
            .filter(|r| r["mode"] == mode)
            .cloned()
            .collect();
        summary[mode] = metrics::summarize(&rows);
    }
    raw["results"]["summary"] = summary;
    fs::write(output, serde_json::to_vec(&raw)?)?;
    Ok(())
}
