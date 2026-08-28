//! Versioned, CPU-only warning policy. A low score does not certify feasibility.
use std::sync::OnceLock;

use icu_normalizer::ComposingNormalizer;
use serde::Deserialize;

use super::{SchemaV1, ScoreContribution, VaguenessAssessment, VaguenessScore};

#[derive(Deserialize)]
struct Dictionary {
    version: String,
    stopwords: Vec<String>,
    verbs: Vec<String>,
    generic: Vec<String>,
    open_phrases: Vec<String>,
    expected: Vec<String>,
    actual: Vec<String>,
    condition: Vec<String>,
    outcome: Vec<String>,
    errors: Vec<String>,
    reproduction: Vec<String>,
    metrics: Vec<String>,
    comparators: Vec<String>,
}

fn dictionary() -> &'static Dictionary {
    static DICTIONARY: OnceLock<Dictionary> = OnceLock::new();
    DICTIONARY.get_or_init(|| {
        serde_json::from_str(include_str!("task_refine_v1.json")).expect("versioned dictionary")
    })
}

pub fn ruleset_version() -> &'static str {
    &dictionary().version
}

pub(crate) fn tokens(text: &str) -> Vec<String> {
    ComposingNormalizer::new_nfc()
        .normalize(&text.to_lowercase())
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(crate) fn search_terms(text: &str) -> Vec<String> {
    let dict = dictionary();
    let mut terms: Vec<_> = tokens(text)
        .into_iter()
        .filter(|t| {
            t.chars().count() >= 2 && !dict.stopwords.contains(t) && !dict.verbs.contains(t)
        })
        .collect();
    terms.sort();
    terms.dedup();
    terms.truncate(64);
    terms
}

pub fn assess(
    description: &str,
    input_revision: u64,
    threshold: VaguenessScore,
) -> VaguenessAssessment {
    let dict = dictionary();
    let words = tokens(description);
    let has = |list: &[String]| words.iter().any(|w| list.contains(w));
    let observable = (has(&dict.expected) && has(&dict.actual))
        || (has(&dict.condition) && has(&dict.outcome))
        || (has(&dict.errors) && has(&dict.reproduction))
        || (has(&dict.metrics)
            && words.iter().any(|w| w.chars().any(|c| c.is_numeric()))
            && (has(&dict.comparators) || description.contains(['<', '>', '=', '%'])));
    let mut contributions = Vec::new();
    let mut add = |rule: &str, weight, reason: &str| {
        contributions.push(ScoreContribution {
            rule_id: rule.into(),
            weight,
            reason: reason.into(),
        })
    };
    if words.iter().filter(|w| !dict.stopwords.contains(w)).count() < 3 {
        add(
            "few_informative_tokens",
            25,
            "Menos de três palavras informativas.",
        );
    }
    if !has(&dict.verbs) {
        add(
            "missing_verb",
            15,
            "Ação não reconhecida pelo dicionário v1.",
        );
    }
    if has(&dict.generic) && !observable {
        add(
            "generic_without_evidence",
            40,
            "Melhoria/correção genérica sem resultado observável.",
        );
    }
    if !observable
        && dict.open_phrases.iter().any(|phrase| {
            let phrase = tokens(phrase);
            words.windows(phrase.len()).any(|window| window == phrase)
        })
    {
        add(
            "open_ended",
            20,
            "Pedido explicitamente aberto sem resultado observável.",
        );
    }
    let score = contributions
        .iter()
        .map(|c| u16::from(c.weight))
        .sum::<u16>()
        .min(100) as u8;
    VaguenessAssessment {
        schema_version: SchemaV1,
        ruleset_version: ruleset_version().into(),
        input_revision,
        score: score.try_into().expect("bounded score"),
        threshold,
        contributions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_corpus_and_unicode_equivalence() {
        let corpus: Vec<(String, u8)> =
            serde_json::from_str(include_str!("task_refine_corpus_v1.json")).unwrap();
        for (text, expected) in corpus {
            assert_eq!(
                assess(&text, 1, 40.try_into().unwrap()).score.get(),
                expected,
                "{text}"
            );
        }
        assert_eq!(
            tokens("CORREÇÃO ação"),
            tokens("correc\u{327}a\u{303}o ac\u{327}a\u{303}o")
        );
        assert_eq!(
            assess("prefixfix anythingelse", 1, 40.try_into().unwrap())
                .score
                .get(),
            40
        );
    }
}
