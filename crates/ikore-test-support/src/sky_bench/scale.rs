use super::{Corpus, Fact, Location, Note, Question, Relation, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Deserialize, Serialize)]
pub struct Scale {
    pub notes: usize,
    pub stars_per_note: Vec<usize>,
    pub degree: Vec<usize>,
    pub clusters: usize,
    pub cluster_size: usize,
    pub edges: usize,
    pub seed: usize,
}
impl Scale {
    pub fn generate(&self) -> Result<Corpus> {
        if self.notes < 2
            || self.stars_per_note.is_empty()
            || self.stars_per_note.contains(&0)
            || self.degree.is_empty()
            || self.cluster_size >= self.notes
            || self.clusters > self.notes
        {
            return Err("invalid scale parameters".into());
        }
        let mut degrees: Vec<_> = (0..self.notes)
            .map(|n| {
                if n < self.clusters {
                    self.cluster_size
                } else {
                    self.degree[n % self.degree.len()].min(self.notes - 1)
                }
            })
            .collect();
        let mut actual: usize = degrees.iter().sum();
        if self.edges < self.clusters * self.cluster_size
            || self.edges
                > self.notes * (self.notes - 1)
                    - (self.notes - 1 - self.cluster_size) * self.clusters
        {
            return Err("infeasible edge target".into());
        }
        while actual != self.edges {
            for d in degrees.iter_mut().skip(self.clusters) {
                if actual == self.edges {
                    break;
                }
                if actual < self.edges && *d + 1 < self.notes {
                    *d += 1;
                    actual += 1;
                } else if actual > self.edges && *d > 0 {
                    *d -= 1;
                    actual -= 1;
                }
            }
        }
        let mut facts = Vec::new();
        let mut questions = Vec::new();
        for (n, degree) in degrees.into_iter().enumerate() {
            let feature = format!("district{n:05}");
            let path = format!("features/{feature}.md");
            let count = self.stars_per_note[n % self.stars_per_note.len()];
            let mut links = BTreeSet::new();
            let mut state = (n + self.seed) as u64;
            while links.len() < degree {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let next = ((state >> 32) as usize) % self.notes;
                if next != n {
                    links.insert(next);
                }
            }
            let mut relations = vec![Vec::new(); count];
            for (i, target) in links.into_iter().enumerate() {
                relations[i % count].push(Relation {
                    r#type: "link".into(),
                    target: format!("S{target:05}-0"),
                });
            }
            for (slot, relations) in relations.into_iter().enumerate() {
                let text = synthetic_text(n, slot);
                facts.push(Fact {
                    id: format!("S{n:05}-{slot}"),
                    feature: feature.clone(),
                    text,
                    kind: "decision".into(),
                    constellation: Note {
                        path: path.clone(),
                        r#type: "feature".into(),
                        heading: "Behavior".into(),
                    },
                    code: None,
                    relations,
                });
            }
            if n == 0 || n == self.notes / 2 || n == self.notes - 1 {
                questions.push(Question {
                    id: format!("L{n}"),
                    text: format!(
                        "When are {} shipments reviewed in district {n}?",
                        ACTIVITIES[0]
                    ),
                    feature,
                    stage: "SPECIFICATION".into(),
                    category: "behavior".into(),
                    required_facts: vec![format!("S{n:05}-0")],
                    expected_location: None,
                    chain: vec![],
                });
            }
        }
        Ok(Corpus { facts, questions })
    }
}
const ACTIVITIES: [&str; 8] = [
    "outbound",
    "recalled",
    "insured",
    "express",
    "damaged",
    "returned",
    "late",
    "oversized",
];
pub fn synthetic_text(n: usize, k: usize) -> String {
    format!(
        "District {n} reviews {} shipments after {} hours and sends exceptions to ledger {}.",
        ACTIVITIES[k % 8],
        (n * 7 + k * 11) % 96 + 1,
        (n + 11 * k) % 137
    )
}
#[derive(Debug, Deserialize)]
pub struct Mutation {
    pub id: String,
    pub fact: String,
    pub star: String,
    pub language: String,
    pub operation: String,
    pub patch: String,
    pub expected: serde_json::Value,
    pub legacy_file_correct: bool,
}
impl Mutation {
    pub fn apply(&self, root: &Path) -> Result<()> {
        use std::{
            io::Write,
            process::{Command, Stdio},
        };
        let mut cmd = Command::new("git")
            .current_dir(root)
            .args(["apply", "--whitespace=nowarn", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        cmd.stdin
            .take()
            .ok_or("no git stdin")?
            .write_all(self.patch.as_bytes())?;
        let out = cmd.wait_with_output()?;
        if !out.status.success() {
            return Err(format!("{}: {}", self.id, String::from_utf8_lossy(&out.stderr)).into());
        }
        Ok(())
    }
}
pub fn mutation_rates(observations: &[(Option<Location>, Option<Location>)]) -> serde_json::Value {
    let n = observations.len();
    let mut correct = 0;
    let mut wrong = 0;
    let mut stale = 0;
    for (actual, expected) in observations {
        match actual {
            None => stale += 1,
            Some(a)
                if expected
                    .as_ref()
                    .is_some_and(|b| a.path == b.path && a.line == b.line) =>
            {
                correct += 1
            }
            Some(_) => wrong += 1,
        }
    }
    let rate = |x| (n > 0).then(|| x as f64 / n as f64);
    serde_json::json!({"n":n,"correct_relocation":rate(correct),"wrong_relocation":rate(wrong),"stale":rate(stale)})
}
