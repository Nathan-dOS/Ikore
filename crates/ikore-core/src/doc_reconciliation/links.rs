//! The explicit links note -> code file that documentation reconciliation
//! follows (ADR-034 M5). There is no semantic inference: a note without
//! `source-files` or `test-files` never raises a signal.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Source,
    Test,
}

impl LinkKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Test => "test",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "source" => Some(Self::Source),
            "test" => Some(Self::Test),
            _ => None,
        }
    }
}

/// What the repository rules say about a linked path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkCheck {
    /// A visible, readable file inside the workspace.
    Present,
    /// A valid relative path whose file does not exist (yet, or any more).
    Absent,
    /// Outside the workspace, ignored, sensitive, a symlink or a directory.
    Excluded,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NoteLink {
    pub note_path: String,
    pub code_path: String,
    pub kind: LinkKind,
    pub present: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DerivedLinks {
    /// Sorted by note, then path; one entry per note -> path.
    pub links: Vec<NoteLink>,
    /// Notes with no usable link at all.
    pub unlinked_notes: usize,
}

/// The Vault folder: a link to another note is not code drift.
const VAULT_PREFIX: &str = "knowledge/";

/// Normalizes a frontmatter path the way the publication paths are written:
/// forward slashes, no leading `./`. `None` for an empty value.
pub fn normalize(path: &str) -> Option<String> {
    let path = path.trim().replace('\\', "/");
    let path = path.trim_start_matches("./").to_owned();
    (!path.is_empty()).then_some(path)
}

/// Whether a path can never be a reconciliation link, before the
/// repository rules are asked.
fn excluded_by_name(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    name == "review.md" || lower.starts_with(VAULT_PREFIX)
}

/// Derives the links of every note. `check` applies the repository rules to
/// one normalized path; it is called once per distinct path.
pub fn derive_links<'a>(
    notes: impl IntoIterator<Item = (&'a str, &'a [String], &'a [String])>,
    mut check: impl FnMut(&str) -> LinkCheck,
) -> DerivedLinks {
    let mut cache: BTreeMap<String, LinkCheck> = BTreeMap::new();
    let mut links = BTreeMap::new();
    let mut unlinked_notes = 0usize;
    for (note_path, source_files, test_files) in notes {
        let mut linked = false;
        for (kind, paths) in [
            (LinkKind::Source, source_files),
            (LinkKind::Test, test_files),
        ] {
            for raw in paths {
                let Some(path) = normalize(raw) else {
                    continue;
                };
                if excluded_by_name(&path) {
                    continue;
                }
                let state = *cache.entry(path.clone()).or_insert_with(|| check(&path));
                if state == LinkCheck::Excluded {
                    continue;
                }
                linked = true;
                // A path under both fields is tracked once, as source.
                links
                    .entry((note_path.to_owned(), path.clone()))
                    .or_insert(NoteLink {
                        note_path: note_path.to_owned(),
                        code_path: path,
                        kind,
                        present: state == LinkCheck::Present,
                    });
            }
        }
        if !linked {
            unlinked_notes += 1;
        }
    }
    DerivedLinks {
        links: links.into_values().collect(),
        unlinked_notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn links_are_explicit_normalized_and_filtered() {
        let first_sources = strings(&[
            "src/lib.rs",
            ".\\src\\api.rs",
            "REVIEW.md",
            "docs/REVIEW.md",
        ]);
        let first_tests = strings(&[
            "tests/api.rs",
            "src/lib.rs",
            "knowledge/obsidian/tests/a.md",
        ]);
        let second_sources = strings(&["../outside.rs", "src/missing.rs", " "]);
        let empty: Vec<String> = Vec::new();
        let mut asked = Vec::new();
        let derived = derive_links(
            [
                (
                    "knowledge/obsidian/features/a.md",
                    first_sources.as_slice(),
                    first_tests.as_slice(),
                ),
                (
                    "knowledge/obsidian/features/b.md",
                    second_sources.as_slice(),
                    empty.as_slice(),
                ),
                (
                    "knowledge/obsidian/features/c.md",
                    empty.as_slice(),
                    empty.as_slice(),
                ),
            ],
            |path| {
                asked.push(path.to_owned());
                match path {
                    "../outside.rs" => LinkCheck::Excluded,
                    "src/missing.rs" => LinkCheck::Absent,
                    _ => LinkCheck::Present,
                }
            },
        );
        let pairs = derived
            .links
            .iter()
            .map(|link| {
                (
                    link.note_path.as_str(),
                    link.code_path.as_str(),
                    link.kind,
                    link.present,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            pairs,
            vec![
                (
                    "knowledge/obsidian/features/a.md",
                    "src/api.rs",
                    LinkKind::Source,
                    true
                ),
                (
                    "knowledge/obsidian/features/a.md",
                    "src/lib.rs",
                    LinkKind::Source,
                    true
                ),
                (
                    "knowledge/obsidian/features/a.md",
                    "tests/api.rs",
                    LinkKind::Test,
                    true
                ),
                (
                    "knowledge/obsidian/features/b.md",
                    "src/missing.rs",
                    LinkKind::Source,
                    false
                ),
            ]
        );
        // `b` keeps a link to a missing file; only `c` has none.
        assert_eq!(derived.unlinked_notes, 1);
        // REVIEW.md and Vault notes never reach the repository check, and a
        // path shared by two fields is checked once.
        assert!(
            !asked
                .iter()
                .any(|path| path.ends_with("REVIEW.md") || path.starts_with("knowledge/"))
        );
        assert_eq!(asked.iter().filter(|path| *path == "src/lib.rs").count(), 1);
    }

    #[test]
    fn a_note_with_hundreds_of_links_is_derived_in_one_pass() {
        let sources = (0..500)
            .map(|i| format!("src/m{i:03}.rs"))
            .collect::<Vec<_>>();
        let empty: Vec<String> = Vec::new();
        let mut calls = 0;
        let derived = derive_links(
            [(
                "knowledge/obsidian/features/big.md",
                sources.as_slice(),
                empty.as_slice(),
            )],
            |_| {
                calls += 1;
                LinkCheck::Present
            },
        );
        assert_eq!(derived.links.len(), 500);
        assert_eq!(calls, 500);
        assert!(derived.links.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
