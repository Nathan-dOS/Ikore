use super::metrics::percentile;
use super::{Corpus, MODEL, Result, scale};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Instant;
pub mod database;
pub const DIGEST: &str = "0a109f422b47e3a30ba2b10eca18548e944e8a23073ee3f3e947efcf3c45e59f";
const ENDPOINT: &str = "http://127.0.0.1:11434";
#[derive(Deserialize)]
struct Vectors {
    embeddings: Vec<Vec<f64>>,
}
async fn embed(client: &reqwest::Client, text: &[String], keep: &str) -> Result<Vec<Vec<f64>>> {
    let output: Vectors = client
        .post(format!("{ENDPOINT}/api/embed"))
        .json(&json!({"model":MODEL,"input":text,"keep_alive":keep,"truncate":false}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    if output.embeddings.len() != text.len()
        || output
            .embeddings
            .iter()
            .any(|v| v.len() != 768 || v.iter().any(|x| !x.is_finite()))
    {
        return Err("invalid embedding response".into());
    }
    Ok(output.embeddings)
}
pub async fn model_digest(client: &reqwest::Client) -> Result<String> {
    let tags: Value = client
        .get(format!("{ENDPOINT}/api/tags"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let model = tags["models"]
        .as_array()
        .ok_or("no installed model list")?
        .iter()
        .find(|m| m["name"] == MODEL)
        .ok_or("pinned model not installed")?;
    let observed = model["digest"].as_str().ok_or("missing model digest")?;
    if observed != DIGEST {
        return Err(format!("model tag mismatch: {observed}").into());
    }
    Ok(observed.into())
}
fn cosine(a: &[f64], b: &[f64]) -> Result<f64> {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f64]| v.iter().map(|x| x * x).sum::<f64>().sqrt();
    let value = dot / (norm(a) * norm(b));
    if !value.is_finite() {
        return Err("undefined cosine".into());
    }
    Ok(value)
}
fn time(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.
}
pub async fn e7(corpus: &Corpus) -> Result<Value> {
    let client = reqwest::Client::new();
    let digest = model_digest(&client).await?;
    let mut results = Vec::new();
    let queries: Vec<_> = corpus
        .questions
        .iter()
        .map(|q| format!("search_query: {}", q.text))
        .collect();
    let stars: Vec<_> = (0..2000)
        .map(|i| {
            format!(
                "search_document: District {}\nBehavior\n{}",
                i / 8,
                scale::synthetic_text(i / 8, i % 8)
            )
        })
        .collect();
    let mut latency = Vec::new();
    for (kind, texts) in [("questions", &queries), ("stars", &stars)] {
        let mut individual = Vec::new();
        for input in texts {
            individual.push(
                embed(&client, std::slice::from_ref(input), "10m")
                    .await?
                    .remove(0),
            );
        }
        for size in [15, 32] {
            let mut cosines = Vec::new();
            for (i, chunk) in texts.chunks(size).enumerate() {
                for (j, vector) in embed(&client, chunk, "10m").await?.iter().enumerate() {
                    cosines.push(cosine(&individual[i * size + j], vector)?);
                }
            }
            let min = cosines.iter().copied().reduce(f64::min);
            results.push(json!({"corpus":kind,"n":texts.len(),"batch_size":size,"min":min,"p1":percentile(&cosines,1),"median":percentile(&cosines,50),"pass":min.is_some_and(|v|v>=0.9999),"cosines":cosines}));
        }
    }
    for round in 0..6 {
        let mut modes = ["parallel_15", "batch_15", "reindex_16", "reindex_32"];
        modes.rotate_left(round % 4);
        for mode in modes {
            let start = Instant::now();
            match mode {
                "parallel_15" => {
                    let mut set = tokio::task::JoinSet::new();
                    for q in queries.iter().take(15).cloned() {
                        let client = client.clone();
                        set.spawn(async move { embed(&client, &[q], "10m").await });
                    }
                    while let Some(res) = set.join_next().await {
                        res??;
                    }
                }
                "batch_15" => {
                    embed(&client, &queries[..15], "10m").await?;
                }
                other => {
                    let width = if other == "reindex_16" { 16 } else { 32 };
                    for chunk in stars[..96].chunks(width) {
                        embed(&client, chunk, "10m").await?;
                    }
                }
            }
            if round > 0 {
                latency.push(json!({"round":round,"mode":mode,"ms":time(start)}));
            }
        }
        client
            .post(format!("{ENDPOINT}/api/embed"))
            .json(&json!({"model":MODEL,"input":[],"keep_alive":0}))
            .send()
            .await?
            .error_for_status()?;
        for mode in ["cold", "warm"] {
            let start = Instant::now();
            embed(&client, &queries[..1], "10m").await?;
            if round > 0 {
                latency.push(json!({"round":round,"mode":mode,"ms":time(start)}));
            }
        }
    }
    Ok(json!({"model":MODEL,"digest":digest,"equivalence":results,"latency":latency}))
}
