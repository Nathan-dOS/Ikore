//! Frozen parcel clearinghouse corpus and measurement-only adapters.
pub mod experiments;
#[cfg(feature = "sky-baseline")]
pub mod legacy;
pub mod metrics;
pub mod scale;
mod vault;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
pub use vault::{materialize, tree_hashes};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub const MODEL: &str = "nomic-embed-text:137m-v1.5-fp16";
pub fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/sky-bench")
}
pub fn hash(bytes: &[u8]) -> String {
    ikore_core::orchestration::ContentSha256::of(bytes)
        .as_str()
        .to_owned()
}
pub fn manifest() -> Result<String> {
    let root = fixtures();
    let file = fs::read(root.join("MANIFEST.sha256"))?;
    let mut expected = BTreeMap::new();
    for line in std::str::from_utf8(&file)?.lines() {
        let (digest, path) = line.split_once("  ").ok_or("invalid manifest row")?;
        if expected
            .insert(path.to_owned(), digest.to_owned())
            .is_some()
        {
            return Err("duplicate manifest path".into());
        }
    }
    let mut actual = tree_hashes(&root)?;
    actual.remove("MANIFEST.sha256");
    if expected != actual {
        return Err("frozen data changed; create a new baseline".into());
    }
    Ok(hash(&file))
}
pub fn data<T: serde::de::DeserializeOwned>(name: &str) -> Result<T> {
    Ok(serde_yaml::from_slice(&fs::read(fixtures().join(name))?)?)
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Note {
    pub path: String,
    pub r#type: String,
    pub heading: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Code {
    pub path: String,
    pub symbol: Option<String>,
    pub lines: [usize; 2],
    pub fp: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Relation {
    pub r#type: String,
    pub target: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Fact {
    pub id: String,
    pub text: String,
    pub kind: String,
    pub feature: String,
    pub constellation: Note,
    pub code: Option<Code>,
    pub relations: Vec<Relation>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Location {
    pub path: String,
    pub line: usize,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Question {
    pub id: String,
    pub text: String,
    pub required_facts: Vec<String>,
    pub expected_location: Option<Location>,
    pub feature: String,
    pub stage: String,
    pub category: String,
    pub chain: Vec<serde_json::Value>,
}
pub struct Corpus {
    pub facts: Vec<Fact>,
    pub questions: Vec<Question>,
}
impl Corpus {
    pub fn load() -> Result<Self> {
        manifest()?;
        Ok(Self {
            facts: data("facts.yaml")?,
            questions: data("questions.yaml")?,
        })
    }
}
#[cfg(test)]
mod tests;
