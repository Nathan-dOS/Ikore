#[cfg(test)]
use super::Corpus;
use super::{Fact, Question, Result};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::collections::BTreeSet;

#[derive(Debug, Serialize)]
pub struct Snippet {
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub text: String,
}
#[derive(Debug, Serialize)]
pub struct Item {
    pub path: String,
    pub text: String,
    pub snippets: Vec<Snippet>,
    pub similarity: Option<f64>,
}
/// File-only references visible to the agent in an actual delivered legacy note.
pub fn file_references(body: &str) -> Vec<String> {
    let Some((metadata, _)) = body
        .strip_prefix("---\n")
        .and_then(|s| s.split_once("\n---\n"))
    else {
        return Vec::new();
    };
    let Ok(metadata) = serde_yaml::from_str::<Value>(metadata) else {
        return Vec::new();
    };
    ["source-files", "test-files"]
        .iter()
        .filter_map(|field| metadata[*field].as_array())
        .flatten()
        .filter_map(|entry| entry.as_str().map(str::to_owned))
        .collect()
}
#[derive(Debug, Serialize)]
pub struct Delivery {
    pub items: Vec<Item>,
    pub latency_ms: f64,
    pub strategy: String,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Native,
    Question,
}
pub trait RetrievalSystem {
    fn retrieve(&mut self, question: &Question, mode: Mode) -> Result<Delivery>;
}
#[derive(Default)]
pub struct Sky {
    pub ablations: SkyAblations,
}
#[derive(Default)]
pub struct SkyAblations {
    pub routing_only: bool,
    pub flat_only: bool,
    pub bm25_only: bool,
    pub fusion: bool,
    pub expand_lines: bool,
    pub boost_title: bool,
    pub line_weights: BTreeMap<String, f64>,
    pub backlink_weight: Option<f64>,
    pub candidate_lines: Vec<String>,
    pub derived_lines: Vec<String>,
    pub v4: bool,
    pub v5: bool,
}
#[derive(Debug)]
pub struct Unimplemented(pub &'static str);
impl std::fmt::Display for Unimplemented {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unimplemented: slice {}", self.0)
    }
}
impl std::error::Error for Unimplemented {}
impl RetrievalSystem for Sky {
    fn retrieve(&mut self, _: &Question, _: Mode) -> Result<Delivery> {
        Err(Unimplemented("4d").into())
    }
}
pub fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect()
}
pub fn overlap(q: &Question, facts: &[Fact]) -> Result<Option<f64>> {
    let words = tokens(&q.text);
    let a: BTreeSet<_> = words.iter().cloned().collect();
    let mut maximum: Option<f64> = None;
    for id in &q.required_facts {
        let fact = facts
            .iter()
            .find(|f| f.id == *id)
            .ok_or("missing reference fact")?;
        let terms = tokens(&fact.text);
        if words
            .windows(5)
            .any(|window| terms.windows(5).any(|other| window == other))
        {
            return Err(format!("{} copies five words from {id}", q.id).into());
        }
        let b: BTreeSet<_> = terms.into_iter().collect();
        let score = a.intersection(&b).count() as f64 / a.union(&b).count() as f64;
        maximum = Some(maximum.unwrap_or(0.).max(score));
    }
    Ok(maximum)
}
pub fn percentile(xs: &[f64], p: usize) -> Option<f64> {
    if xs.is_empty() {
        return None;
    }
    let mut sorted = xs.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(sorted[(sorted.len() * p).div_ceil(100).saturating_sub(1)])
}
pub fn score(q: &Question, facts: &[Fact], d: &Delivery) -> Value {
    let mut ordered = Vec::new();
    let mut first = BTreeSet::new();
    let mut ranks = Vec::new();
    let mut relevant_chars = 0;
    let mut path_hit = false;
    let mut line_hit = false;
    let chars: usize = d.items.iter().map(|it| it.text.chars().count()).sum();
    for item in &d.items {
        if let Some(loc) = &q.expected_location {
            path_hit |= file_references(&item.text).contains(&loc.path);
        }
        let mut hits: Vec<_> = facts
            .iter()
            .filter_map(|fact| item.text.find(&fact.text).map(|position| (position, fact)))
            .collect();
        hits.sort_by_key(|hit| hit.0);
        for (_, fact) in hits {
            let relevant = q.required_facts.contains(&fact.id);
            if relevant {
                relevant_chars += item.text.matches(&fact.text).count() * fact.text.chars().count();
            }
            if first.insert(&fact.id) {
                ordered.push(&fact.id);
                if relevant {
                    ranks.push(first.len());
                }
            }
        }
        for snippet in &item.snippets {
            if let Some(loc) = &q.expected_location
                && snippet.path == loc.path
            {
                path_hit = true;
                if (snippet.start..=snippet.end).contains(&loc.line) {
                    line_hit = true;
                    relevant_chars += snippet.text.chars().count();
                }
            }
        }
    }
    let count = q.required_facts.len();
    let recall =
        |k| (count != 0).then(|| ranks.iter().filter(|r| **r <= k).count() as f64 / count as f64);
    let discount = |rank: usize| 1. / ((rank + 1) as f64).log2();
    let gain: f64 = ranks
        .iter()
        .filter(|r| **r <= 10)
        .map(|r| discount(*r))
        .sum();
    let total_tokens = chars.div_ceil(4);
    json!({"facts_in_order":ordered,
        "relevant_ranks":ranks,"recall_5":recall(5),"recall_10":recall(10),"recall_all":recall(usize::MAX),
        "mrr":(count>0).then(||ranks.first().map_or(0.,|r|1.0 / *r as f64)),
        "ndcg_10":(count>0).then(||gain/(1..=count.min(10)).map(discount).sum::<f64>()),
        "characters":chars,"relevant_characters":relevant_chars,"context_precision":if chars==0{0.}else{relevant_chars as f64/chars as f64},
        "tokens":total_tokens,"tokens_per_hit":(!ranks.is_empty()).then(||total_tokens as f64/ranks.len() as f64),
        "file_hit":q.expected_location.as_ref().map(|_|path_hit),"line_hit":q.expected_location.as_ref().map(|_|line_hit),
        "correct_refusal":(count==0).then_some(d.items.is_empty()),"unanswered_tokens":(count==0).then_some(total_tokens),
        "max_delivered_similarity":d.items.iter().filter_map(|i|i.similarity).reduce(f64::max),"latency_ms":d.latency_ms})
}
pub fn summarize(rows: &[Value]) -> Value {
    let mut out = json!({});
    for key in [
        "recall_5",
        "recall_10",
        "recall_all",
        "mrr",
        "ndcg_10",
        "context_precision",
        "tokens",
        "tokens_per_hit",
        "file_hit",
        "line_hit",
        "correct_refusal",
        "unanswered_tokens",
        "max_delivered_similarity",
        "latency_ms",
    ] {
        let xs: Vec<_> = rows
            .iter()
            .filter(|r| key != "max_delivered_similarity" || r["category"] == "unanswered")
            .filter_map(|r| {
                r["metrics"][key]
                    .as_f64()
                    .or_else(|| r["metrics"][key].as_bool().map(|b| f64::from(u8::from(b))))
            })
            .collect();
        out[key] = json!({"n":xs.len(),"mean":(!xs.is_empty()).then(||xs.iter().sum::<f64>()/xs.len() as f64),"p50":percentile(&xs,50),"p95":percentile(&xs,95)});
    }
    out
}
#[cfg(test)]
pub(super) fn check_examples() {
    let corpus = Corpus::load().unwrap();
    let q = &corpus.questions[0];
    let f = &corpus.facts[0];
    let d = Delivery {
        latency_ms: 1.,
        strategy: "fixture".into(),
        items: vec![Item {
            path: "a".into(),
            text: format!("{} {}", f.text, f.text),
            snippets: vec![],
            similarity: None,
        }],
    };
    let result = score(q, &corpus.facts, &d);
    assert_eq!(result["recall_5"], 1.);
    assert_eq!(result["relevant_ranks"], json!([1]));
    let mut reference_only = Delivery {
        latency_ms: 0.,
        strategy: "fixture".into(),
        items: Vec::new(),
    };
    let expected = &q.expected_location.as_ref().unwrap().path;
    reference_only.items.push(Item {
        path: "feature.md".into(),
        text: format!("---\nsource-files: [{expected}]\n---\nA fact."),
        snippets: Vec::new(),
        similarity: None,
    });
    let file_score = score(q, &corpus.facts, &reference_only);
    assert_eq!(file_score["file_hit"], true);
    assert_eq!(file_score["line_hit"], false);
    let empty = Delivery { items: vec![], ..d };
    let result = score(&corpus.questions[6], &corpus.facts, &empty);
    assert_eq!(result["correct_refusal"], true);
    assert!(result["mrr"].is_null());
    let low = corpus
        .questions
        .iter()
        .filter(|q| overlap(q, &corpus.facts).unwrap().is_some_and(|v| v < 0.2))
        .count();
    assert!(low * 10 >= corpus.questions.len() * 3);
}
