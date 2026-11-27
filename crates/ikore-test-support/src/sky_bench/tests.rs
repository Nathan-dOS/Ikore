use super::metrics::RetrievalSystem;
use super::*;
use std::{collections::BTreeMap, fs};
use tempfile::tempdir;

#[test]
fn frozen_corpus_survives_both_encodings_and_the_real_vault_parser() {
    let corpus = Corpus::load().unwrap();
    let a = tempdir().unwrap();
    let b = tempdir().unwrap();
    vault::materialize_with_history(a.path(), false).unwrap();
    corpus.render(a.path()).unwrap();
    corpus.render(b.path()).unwrap();
    corpus.validate(a.path()).unwrap();
    for root in ["observatory", "knowledge"] {
        assert_eq!(
            tree_hashes(&a.path().join(root)).unwrap(),
            tree_hashes(&b.path().join(root)).unwrap()
        );
    }
    let mut cache = ikore_core::workspace_service::VaultIndexCache::default();
    let index = ikore_core::workspace_service::get_workspace_vault_index(
        b.path(),
        &mut cache,
        ikore_core::workspace_service::GetWorkspaceVaultIndexRequest {
            force_rebuild: true,
        },
    )
    .unwrap();
    let parsed: BTreeMap<_, _> = index
        .notes
        .iter()
        .map(|n| (n.relative_path.as_str(), n))
        .collect();
    for fact in &corpus.facts {
        let path = format!("knowledge/obsidian/{}", fact.constellation.path);
        let note = parsed[path.as_str()];
        assert!(note.frontmatter.is_some());
        if let Some(code) = &fact.code {
            let paths = if fact.kind == "test" {
                &note.test_files
            } else {
                &note.source_files
            };
            assert!(paths.contains(&code.path));
        }
    }
    let mut categories = BTreeMap::new();
    for q in &corpus.questions {
        *categories.entry(&q.category).or_insert(0) += 1;
    }
    assert_eq!(categories.len(), 7);
    assert!(categories.values().all(|n| *n >= 8));
    assert!(
        fs::read_to_string(a.path().join("Cargo.toml"))
            .unwrap()
            .contains("[package]")
    );
}

#[test]
fn metrics_handle_unanswered_questions_and_duplicate_evidence() {
    metrics::check_examples();
    assert_eq!(
        vault::sentence_count(
            "Use a carrier, e.g. Atlas. Check its ID, i.e. the ledger ID. Save changes!"
        ),
        3
    );
    assert_eq!(vault::sentence_count("At the end, e.g."), 1);
    assert_eq!(vault::sentence_count("Wait... Really?!"), 2);
}

#[test]
fn reference_labels_resolve_and_scale_respects_the_requested_density() {
    let corpus = Corpus::load().unwrap();
    let catalog: BTreeMap<_, _> = corpus.facts.iter().map(|f| (f.id.as_str(), f)).collect();
    assert!((250..=400).contains(&catalog.len()));
    let mut types = BTreeMap::new();
    for f in &corpus.facts {
        for r in &f.relations {
            assert!(catalog.contains_key(r.target.as_str()));
            *types.entry(r.r#type.as_str()).or_insert(0usize) += 1;
        }
        if let Some(c) = &f.code {
            assert!(c.lines[0] > 0 && c.lines[0] <= c.lines[1]);
            assert!(
                std::path::Path::new(&c.path)
                    .components()
                    .all(|p| matches!(p, std::path::Component::Normal(_)))
            );
            let name = c.path.rsplit('/').next().unwrap().to_ascii_lowercase();
            assert!(
                !name.starts_with(".env")
                    && !name.starts_with("secrets.")
                    && !name.ends_with(".pem")
                    && !name.ends_with(".key")
            );
            assert!(
                c.fp.split(|ch: char| !ch.is_alphanumeric() && ch != '_')
                    .any(|word| word.len() >= 3
                        && word.starts_with(|ch: char| ch.is_alphabetic() || ch == '_'))
            );
        }
    }
    for ty in [
        "contracts",
        "tested-by",
        "affects",
        "link",
        "implements",
        "depends-on",
        "configures",
        "reads",
        "writes",
        "emits",
        "consumes",
        "exposes",
        "decided-by",
        "supersedes",
        "part-of",
        "risks",
    ] {
        assert!(types[ty] >= 8);
    }
    for q in &corpus.questions {
        if let Some(loc) = &q.expected_location {
            assert!(q.required_facts.iter().any(|id| {
                catalog[id.as_str()]
                    .code
                    .as_ref()
                    .is_some_and(|c| c.path == loc.path && c.lines.contains(&loc.line))
            }));
        }
        for edge in &q.chain {
            assert!(
                catalog[edge["source"].as_str().unwrap()]
                    .relations
                    .iter()
                    .any(|r| r.r#type == edge["type"] && r.target == edge["target"])
            );
        }
    }
    let configs: Vec<scale::Scale> = data("scale.yaml").unwrap();
    let sample = configs[0].generate().unwrap();
    assert_eq!(
        sample
            .facts
            .iter()
            .map(|f| f.relations.len())
            .sum::<usize>(),
        configs[0].edges
    );
    let large = &configs[2];
    assert!(
        (0..large.notes)
            .map(|n| large.stars_per_note[n % large.stars_per_note.len()])
            .sum::<usize>()
            > 50000
    );
    let mutations: Vec<scale::Mutation> = data("mutations.yaml").unwrap();
    for case in mutations {
        assert_eq!(case.star, vault::id("star", &case.fact));
    }
    assert!(matches!(
        metrics::Sky::default()
            .retrieve(&corpus.questions[0], metrics::Mode::Native)
            .unwrap_err()
            .downcast_ref::<metrics::Unimplemented>(),
        Some(metrics::Unimplemented("4d"))
    ));
}
