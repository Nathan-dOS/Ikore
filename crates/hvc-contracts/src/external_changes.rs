//! What changed in the workspace since a CLI session's last turn (ADR-037
//! M3): computed by the host when the developer resumes the session, shown
//! to the agent as a host notice, or turned into a refusal past the budget.

use serde::{Deserialize, Serialize};

use crate::exchange::{ExchangeItemV1, ExchangeKind, HostComponent, Producer};

/// The budget of a notice (D9): at most this many files listed...
pub const MAX_NOTICE_FILES: usize = 40;
/// ...and at most this many characters of notice text.
pub const MAX_NOTICE_CHARS: usize = 8_000;
/// Hunks are shown only for text files up to this size on both sides.
pub const MAX_HUNK_FILE_BYTES: usize = 32 * 1024;

/// The order entries are listed in: modified, renamed, created, deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Modified,
    Renamed,
    Created,
    Deleted,
}

impl ChangeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Modified => "modified",
            Self::Renamed => "renamed",
            Self::Created => "created",
            Self::Deleted => "deleted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalChangesDecision {
    /// Nothing changed: resume as before.
    Unchanged,
    /// Changes within the budget: resume, with the notice before the prompt.
    Notice,
    /// Too many changes to describe: the resume is refused with the count.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalChangeV1 {
    pub path: String,
    pub change: ChangeKind,
    /// The old path of a rename.
    pub from_path: Option<String>,
    pub added_lines: Option<u32>,
    pub removed_lines: Option<u32>,
    /// Unified hunks of the sanitized content, for small text files only.
    pub hunks: Option<String>,
}

impl ExternalChangeV1 {
    fn bare(path: &str, change: ChangeKind, from_path: Option<&str>) -> Self {
        Self {
            path: path.to_owned(),
            change,
            from_path: from_path.map(str::to_owned),
            added_lines: None,
            removed_lines: None,
            hunks: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalChangesV1 {
    pub schema_version: u8,
    pub parent_run_id: String,
    pub entries: Vec<ExternalChangeV1>,
    pub omitted_count: u32,
    pub decision: ExternalChangesDecision,
}

impl ExternalChangesV1 {
    /// Every changed file, listed or omitted.
    pub fn changed_files(&self) -> usize {
        self.entries.len() + self.omitted_count as usize
    }
}

/// One file of a snapshot manifest: its path and the SHA-256 (lowercase hex)
/// of its original content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    pub original_sha256: String,
}

/// The changes between two manifests, by content digest, in listing order
/// (kind, then path bytes). A rename is one removed and one created file with
/// the same digest, when that digest is removed and created exactly once; a
/// tie is reported as separate deletions and creations.
pub fn diff_manifests(
    parent: &[ManifestEntry],
    current: &[ManifestEntry],
) -> Vec<ExternalChangeV1> {
    use std::collections::BTreeMap;
    let before: BTreeMap<&str, &str> = parent
        .iter()
        .map(|entry| (entry.path.as_str(), entry.original_sha256.as_str()))
        .collect();
    let after: BTreeMap<&str, &str> = current
        .iter()
        .map(|entry| (entry.path.as_str(), entry.original_sha256.as_str()))
        .collect();
    let mut changes = Vec::new();
    let mut removed: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut created: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (path, sha) in &before {
        match after.get(path) {
            Some(now) if now == sha => {}
            Some(_) => changes.push(ExternalChangeV1::bare(path, ChangeKind::Modified, None)),
            None => removed.entry(sha).or_default().push(path),
        }
    }
    for (path, sha) in &after {
        if !before.contains_key(path) {
            created.entry(sha).or_default().push(path);
        }
    }
    for (sha, from) in &removed {
        match (from.as_slice(), created.get(sha).map(Vec::as_slice)) {
            ([from], Some([to])) => {
                changes.push(ExternalChangeV1::bare(to, ChangeKind::Renamed, Some(from)));
            }
            _ => changes.extend(
                from.iter()
                    .map(|path| ExternalChangeV1::bare(path, ChangeKind::Deleted, None)),
            ),
        }
    }
    for (sha, to) in &created {
        let renamed = matches!(
            (removed.get(sha).map(Vec::as_slice), to.as_slice()),
            (Some([_]), [_])
        );
        if !renamed {
            changes.extend(
                to.iter()
                    .map(|path| ExternalChangeV1::bare(path, ChangeKind::Created, None)),
            );
        }
    }
    changes.sort_by(|a, b| (a.change, a.path.as_bytes()).cmp(&(b.change, b.path.as_bytes())));
    changes
}

/// A deterministic text of what changed (kind and paths, never content);
/// its digest binds a resume's notice to the snapshot the session starts on.
pub fn canonical_change_list(entries: &[ExternalChangeV1]) -> String {
    let mut text = String::from("external-changes-v1\n");
    for entry in entries {
        text.push_str(entry.change.as_str());
        text.push('\0');
        text.push_str(&entry.path);
        text.push('\0');
        text.push_str(entry.from_path.as_deref().unwrap_or_default());
        text.push('\n');
    }
    text
}

fn entry_line(entry: &ExternalChangeV1) -> String {
    let mut line = match (&entry.change, &entry.from_path) {
        (ChangeKind::Renamed, Some(from)) => format!("- renamed `{from}` -> `{}`", entry.path),
        (kind, _) => format!("- {} `{}`", kind.as_str(), entry.path),
    };
    if let (Some(added), Some(removed)) = (entry.added_lines, entry.removed_lines) {
        line.push_str(&format!(" (+{added} -{removed} lines)"));
    }
    line.push('\n');
    if let Some(hunks) = &entry.hunks {
        let fence = "`".repeat(longest_backtick_run(hunks).max(2) + 1);
        line.push_str(&format!("  {fence}diff\n"));
        for hunk_line in hunks.lines() {
            line.push_str("  ");
            line.push_str(hunk_line);
            line.push('\n');
        }
        line.push_str(&format!("  {fence}\n"));
    }
    line
}

fn longest_backtick_run(text: &str) -> usize {
    let (mut longest, mut run) = (0, 0);
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

const NOTICE_TITLE: &str = "## External changes since your last turn in this session";
const NOTICE_LEAD: &str = "Files changed outside this session. Re-read a file before editing it.";

fn render(entries: &[ExternalChangeV1], omitted: u32) -> String {
    let line = ExchangeItemV1::new(
        ExchangeKind::ExternalChanges,
        Producer::Host(HostComponent::TerminalResume),
    )
    .provenance_line();
    let mut text = format!("{NOTICE_TITLE}\n{line}\n{NOTICE_LEAD}\n");
    for entry in entries {
        text.push_str(&entry_line(entry));
    }
    if omitted > 0 {
        text.push_str(&format!(
            "- {omitted} more changed files are not listed; read them from the workspace.\n"
        ));
    }
    text
}

/// Applies the budget (D9) to the changes, largest hunks dropped first, then
/// entries past the fortieth folded into the omitted count. Refused when
/// more than [`MAX_NOTICE_FILES`] files changed, or when even the paths alone
/// exceed [`MAX_NOTICE_CHARS`].
pub fn within_budget(parent_run_id: &str, mut entries: Vec<ExternalChangeV1>) -> ExternalChangesV1 {
    let total = entries.len();
    let decision = |entries: Vec<ExternalChangeV1>, omitted: u32, decision| ExternalChangesV1 {
        schema_version: 1,
        parent_run_id: parent_run_id.to_owned(),
        entries,
        omitted_count: omitted,
        decision,
    };
    if total == 0 {
        return decision(Vec::new(), 0, ExternalChangesDecision::Unchanged);
    }
    if total > MAX_NOTICE_FILES {
        return decision(Vec::new(), total as u32, ExternalChangesDecision::Refused);
    }
    while render(&entries, 0).chars().count() > MAX_NOTICE_CHARS {
        let largest = entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                entry
                    .hunks
                    .as_ref()
                    .map(|hunks| (hunks.chars().count(), std::cmp::Reverse(index)))
            })
            .max()
            .map(|(_, std::cmp::Reverse(index))| index);
        match largest {
            Some(index) => entries[index].hunks = None,
            None => {
                return decision(Vec::new(), total as u32, ExternalChangesDecision::Refused);
            }
        }
    }
    decision(entries, 0, ExternalChangesDecision::Notice)
}

/// The notice the agent reads before the developer's message, or `None`
/// when nothing changed or the resume is refused.
pub fn render_notice(changes: &ExternalChangesV1) -> Option<String> {
    (changes.decision == ExternalChangesDecision::Notice)
        .then(|| render(&changes.entries, changes.omitted_count))
}

/// The refusal the developer sees when the changes exceed the budget.
pub fn refusal_message(changes: &ExternalChangesV1) -> String {
    format!(
        "{} files changed since the chosen session; start a new session",
        changes.changed_files()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(entries: &[(&str, &str)]) -> Vec<ManifestEntry> {
        entries
            .iter()
            .map(|(path, sha)| ManifestEntry {
                path: (*path).into(),
                original_sha256: (*sha).into(),
            })
            .collect()
    }

    fn summary(changes: &[ExternalChangeV1]) -> Vec<(ChangeKind, &str, Option<&str>)> {
        changes
            .iter()
            .map(|change| {
                (
                    change.change,
                    change.path.as_str(),
                    change.from_path.as_deref(),
                )
            })
            .collect()
    }

    #[test]
    fn changes_are_listed_by_kind_then_path() {
        let parent = manifest(&[
            ("src/a.rs", "1"),
            ("src/b.rs", "2"),
            ("docs/old.md", "3"),
            ("x.md", "4"),
        ]);
        let current = manifest(&[
            ("src/a.rs", "1x"),
            ("src/b.rs", "2"),
            ("docs/new.md", "3"),
            ("c.rs", "5"),
        ]);
        assert_eq!(
            summary(&diff_manifests(&parent, &current)),
            vec![
                (ChangeKind::Modified, "src/a.rs", None),
                (ChangeKind::Renamed, "docs/new.md", Some("docs/old.md")),
                (ChangeKind::Created, "c.rs", None),
                (ChangeKind::Deleted, "x.md", None),
            ]
        );
        assert!(diff_manifests(&parent, &parent).is_empty());
    }

    #[test]
    fn a_digest_tie_is_not_a_rename() {
        let parent = manifest(&[("a.md", "same"), ("b.md", "same")]);
        let current = manifest(&[("c.md", "same")]);
        assert_eq!(
            summary(&diff_manifests(&parent, &current)),
            vec![
                (ChangeKind::Created, "c.md", None),
                (ChangeKind::Deleted, "a.md", None),
                (ChangeKind::Deleted, "b.md", None),
            ]
        );
    }

    #[test]
    fn the_notice_follows_the_specified_text() {
        let mut entries = diff_manifests(
            &manifest(&[("src/a.rs", "1"), ("docs/x.md", "2")]),
            &manifest(&[("src/a.rs", "1x"), ("src/b.rs", "3")]),
        );
        entries[0].added_lines = Some(12);
        entries[0].removed_lines = Some(3);
        entries[0].hunks = Some("@@ -1 +1 @@\n-old\n+new".into());
        let changes = within_budget("run-1", entries);
        assert_eq!(changes.decision, ExternalChangesDecision::Notice);
        assert_eq!(
            render_notice(&changes).unwrap(),
            "## External changes since your last turn in this session\n\
             > provenance: host \u{b7} terminal/resume\n\
             Files changed outside this session. Re-read a file before editing it.\n\
             - modified `src/a.rs` (+12 -3 lines)\n  ```diff\n  @@ -1 +1 @@\n  -old\n  +new\n  ```\n\
             - created `src/b.rs`\n\
             - deleted `docs/x.md`\n"
        );
    }

    #[test]
    fn the_budget_drops_the_largest_hunks_then_refuses() {
        let entries = |count: usize, hunk: usize| {
            (0..count)
                .map(|index| ExternalChangeV1 {
                    hunks: Some("+".repeat(hunk + index)),
                    added_lines: Some(1),
                    removed_lines: Some(0),
                    ..ExternalChangeV1::bare(
                        &format!("src/f{index:02}.rs"),
                        ChangeKind::Modified,
                        None,
                    )
                })
                .collect::<Vec<_>>()
        };
        let trimmed = within_budget("run", entries(3, 3_000));
        assert_eq!(trimmed.decision, ExternalChangesDecision::Notice);
        assert_eq!(
            trimmed
                .entries
                .iter()
                .filter(|entry| entry.hunks.is_some())
                .count(),
            2,
            "only the largest hunk goes"
        );
        assert!(trimmed.entries[2].hunks.is_none());
        let refused = within_budget("run", entries(60, 1));
        assert_eq!(refused.decision, ExternalChangesDecision::Refused);
        assert_eq!(
            refusal_message(&refused),
            "60 files changed since the chosen session; start a new session"
        );
        assert_eq!(
            within_budget("run", Vec::new()).decision,
            ExternalChangesDecision::Unchanged
        );
    }

    #[test]
    fn the_canonical_list_ignores_content() {
        let mut entry = ExternalChangeV1::bare("src/a.rs", ChangeKind::Modified, None);
        let plain = canonical_change_list(std::slice::from_ref(&entry));
        entry.hunks = Some("+secret".into());
        entry.added_lines = Some(1);
        assert_eq!(canonical_change_list(&[entry]), plain);
    }
}
