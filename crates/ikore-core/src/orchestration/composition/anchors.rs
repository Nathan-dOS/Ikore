//! Deterministic anchor partitions of a delivered file (SC-003, SC-009).
//!
//! An anchor is an exact byte interval the worker may replace with
//! `replace_anchor`. How the intervals are chosen never affects correctness
//! (every edit is applied on exact bytes and hashes) - it only decides how
//! much of the file a cheap model has to reproduce. Rust, TypeScript and TSX
//! are partitioned along Tree-sitter symbols; Markdown along ATX headings;
//! everything else along blank-line blocks, with an explicit partition kind
//! so the projection can say which structural support applied. Every
//! non-whitespace byte of the file belongs to exactly one interval, so any
//! change is expressible; whitespace-only gaps are folded into a neighbour.

use tokio_util::sync::CancellationToken;

use super::RelPath;
use super::references::line_span;
use crate::orchestration::RelativeSourcePath;
use crate::orchestration::context::SourceSnapshot;
use crate::orchestration::tree_sitter_context::{SourceLanguage, TreeSitterExtractor};

/// Default cap on anchors issued for one file; the plan admits 256 in total.
pub const DEFAULT_MAX_ANCHORS_PER_FILE: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionKind {
    /// Tree-sitter symbols (Rust/TypeScript/TSX) with the gaps between them.
    Symbols,
    /// Markdown sections split at ATX headings.
    Headings,
    /// Blank-line separated blocks (bounded context for unsupported languages).
    Blocks,
    /// One interval covering the whole file.
    Whole,
}

impl PartitionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Symbols => "symbols",
            Self::Headings => "headings",
            Self::Blocks => "blocks",
            Self::Whole => "whole_file",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub kind: PartitionKind,
    /// Byte intervals `[start, end)`, ascending and non-overlapping.
    pub intervals: Vec<(usize, usize)>,
}

/// Partitions `content` (the exact bytes registered for `path`) into at most
/// `max_anchors` intervals. An empty file yields no interval.
pub fn partition_file(path: &RelPath, content: &str, max_anchors: usize) -> Partition {
    if content.is_empty() {
        return Partition {
            kind: PartitionKind::Whole,
            intervals: Vec::new(),
        };
    }
    let max_anchors = max_anchors.max(1);
    let language = RelativeSourcePath::new(path.as_str())
        .map(|p| SourceLanguage::detect(&p))
        .unwrap_or(SourceLanguage::Unsupported);
    let lower = path.as_str().to_ascii_lowercase();
    let (kind, raw) = match language {
        SourceLanguage::Rust | SourceLanguage::TypeScript | SourceLanguage::Tsx => {
            match symbol_intervals(path, content) {
                Some(intervals) if !intervals.is_empty() => (PartitionKind::Symbols, intervals),
                _ => (PartitionKind::Blocks, block_intervals(content)),
            }
        }
        SourceLanguage::Unsupported if lower.ends_with(".md") || lower.ends_with(".markdown") => {
            (PartitionKind::Headings, heading_intervals(content))
        }
        SourceLanguage::Unsupported => (PartitionKind::Blocks, block_intervals(content)),
    };
    let mut intervals = fill_gaps(content, &raw);
    if intervals.is_empty() {
        intervals = vec![(0, content.len())];
    }
    let intervals = coarsen(intervals, max_anchors);
    let kind = if intervals.len() == 1 && intervals[0] == (0, content.len()) {
        PartitionKind::Whole
    } else {
        kind
    };
    Partition { kind, intervals }
}

/// Leaf symbol intervals: a symbol that contains another symbol (an `impl`
/// method inside its owner, a nested function) is dropped so the file is cut
/// at the innermost declarations; the enclosing header/footer bytes become
/// gaps between them.
fn symbol_intervals(path: &RelPath, content: &str) -> Option<Vec<(usize, usize)>> {
    let relative = RelativeSourcePath::new(path.as_str()).ok()?;
    let snapshot = SourceSnapshot::from_text("composition", relative, content.to_owned()).ok()?;
    let facts = TreeSitterExtractor
        .extract_source(&snapshot, &CancellationToken::new())
        .ok()?;
    let bytes = content.as_bytes();
    let mut spans = facts
        .symbols
        .iter()
        .filter_map(|symbol| line_span(bytes, symbol.start_line, symbol.end_line))
        .filter(|(start, end)| start < end)
        .collect::<Vec<_>>();
    spans.sort_unstable();
    spans.dedup();
    let leaves = spans
        .iter()
        .copied()
        .filter(|(start, end)| {
            !spans
                .iter()
                .any(|(s, e)| (s, e) != (start, end) && s >= start && e <= end)
        })
        .collect::<Vec<_>>();
    // Overlapping leaves cannot happen for a well-formed tree; drop any that
    // would overlap a previous one to keep the partition sound regardless.
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(leaves.len());
    for (start, end) in leaves {
        if out
            .last()
            .is_none_or(|(_, previous_end)| start >= *previous_end)
        {
            out.push((start, end));
        }
    }
    Some(out)
}

/// Sections cut at ATX headings (`#`..`######` followed by a space).
fn heading_intervals(content: &str) -> Vec<(usize, usize)> {
    let mut cuts = Vec::new();
    let mut offset = 0usize;
    for line in content.split_inclusive('\n') {
        let trimmed = line.trim_start_matches('#');
        let hashes = line.len() - trimmed.len();
        if (1..=6).contains(&hashes) && trimmed.starts_with(' ') && offset != 0 {
            cuts.push(offset);
        }
        offset += line.len();
    }
    cuts_to_intervals(content.len(), &cuts)
}

/// Blocks separated by blank lines.
fn block_intervals(content: &str) -> Vec<(usize, usize)> {
    let mut cuts = Vec::new();
    let mut offset = 0usize;
    let mut previous_blank = false;
    for line in content.split_inclusive('\n') {
        let blank = line.trim().is_empty();
        if previous_blank && !blank && offset != 0 {
            cuts.push(offset);
        }
        previous_blank = blank;
        offset += line.len();
    }
    cuts_to_intervals(content.len(), &cuts)
}

fn cuts_to_intervals(len: usize, cuts: &[usize]) -> Vec<(usize, usize)> {
    let mut intervals = Vec::with_capacity(cuts.len() + 1);
    let mut start = 0usize;
    for cut in cuts {
        if *cut > start {
            intervals.push((start, *cut));
        }
        start = *cut;
    }
    if len > start {
        intervals.push((start, len));
    }
    intervals
}

/// Makes the partition total: every gap between (and around) the raw
/// intervals with non-whitespace bytes becomes an interval of its own;
/// whitespace-only gaps are attached to the following interval (or the
/// preceding one at EOF) so no byte is left uncovered. Also snaps interval
/// ends to UTF-8 boundaries, which line/block cuts already guarantee.
fn fill_gaps(content: &str, raw: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(raw.len() * 2 + 1);
    let mut cursor = 0usize;
    for &(start, end) in raw {
        if start < cursor || end > content.len() || start >= end {
            continue;
        }
        if start > cursor {
            if content[cursor..start].trim().is_empty() {
                out.push((cursor, end));
            } else {
                out.push((cursor, start));
                out.push((start, end));
            }
        } else {
            out.push((start, end));
        }
        cursor = end;
    }
    if cursor < content.len() {
        if content[cursor..].trim().is_empty() {
            match out.last_mut() {
                Some(last) => last.1 = content.len(),
                None => out.push((cursor, content.len())),
            }
        } else {
            out.push((cursor, content.len()));
        }
    }
    out.retain(|(start, end)| {
        content.is_char_boundary(*start) && content.is_char_boundary(*end) && start < end
    });
    out
}

/// Reduces the interval count to `cap` by repeatedly merging the adjacent
/// pair with the smallest combined length (stable on ties: earliest pair).
fn coarsen(mut intervals: Vec<(usize, usize)>, cap: usize) -> Vec<(usize, usize)> {
    while intervals.len() > cap {
        let mut best = 0usize;
        let mut best_len = usize::MAX;
        for index in 0..intervals.len() - 1 {
            let len = intervals[index + 1].1 - intervals[index].0;
            if len < best_len {
                best_len = len;
                best = index;
            }
        }
        let merged = (intervals[best].0, intervals[best + 1].1);
        intervals[best] = merged;
        intervals.remove(best + 1);
    }
    intervals
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(text: &str) -> RelPath {
        RelPath::new(text).unwrap()
    }

    fn covers_everything(content: &str, partition: &Partition) {
        let mut cursor = 0usize;
        for (start, end) in &partition.intervals {
            assert!(*start >= cursor && start < end, "{:?}", partition.intervals);
            // Only whitespace may sit between two intervals.
            assert!(content[cursor..*start].trim().is_empty());
            cursor = *end;
        }
        assert!(content[cursor..].trim().is_empty());
    }

    #[test]
    fn rust_files_are_cut_at_leaf_symbols_with_header_and_footer_gaps() {
        let content = "use std::fmt;\n\npub struct Token {\n    pub expires_at: u64,\n}\n\nimpl Token {\n    pub fn valid(&self, now: u64) -> bool {\n        self.expires_at > now\n    }\n\n    pub fn other(&self) {}\n}\n\npub fn helper() -> u8 {\n    1\n}\n";
        let partition = partition_file(&path("src/token.rs"), content, 48);
        assert_eq!(partition.kind, PartitionKind::Symbols);
        covers_everything(content, &partition);
        let texts = partition
            .intervals
            .iter()
            .map(|(s, e)| &content[*s..*e])
            .collect::<Vec<_>>();
        // The import header is its own anchor; the impl header and closing
        // brace are gaps around the two method leaves; the struct and the
        // free function are leaves.
        assert!(texts[0].starts_with("use std::fmt;"));
        assert!(texts.iter().any(|t| t.starts_with("pub struct Token")));
        assert!(
            texts
                .iter()
                .any(|t| t.trim_start().starts_with("pub fn valid"))
        );
        assert!(
            texts
                .iter()
                .any(|t| t.trim_start().starts_with("pub fn other"))
        );
        assert!(texts.iter().any(|t| t.starts_with("pub fn helper")));
        assert!(
            texts
                .iter()
                .any(|t| t.trim_start().starts_with("impl Token {"))
        );
        // No leaf contains another symbol.
        assert!(
            !texts
                .iter()
                .any(|t| t.contains("fn valid") && t.contains("fn other"))
        );
    }

    #[test]
    fn markdown_is_cut_at_headings_and_other_files_at_blank_lines() {
        let md = "intro line\n\n# One\ntext\n\n## Two\nmore\n";
        let partition = partition_file(&path("docs/a.md"), md, 48);
        assert_eq!(partition.kind, PartitionKind::Headings);
        covers_everything(md, &partition);
        assert_eq!(partition.intervals.len(), 3);
        assert!(md[partition.intervals[1].0..].starts_with("# One"));
        assert!(md[partition.intervals[2].0..].starts_with("## Two"));

        let py = "import os\n\n\ndef a():\n    pass\n\n\ndef b():\n    pass\n";
        let partition = partition_file(&path("pkg/mod.py"), py, 48);
        assert_eq!(partition.kind, PartitionKind::Blocks);
        covers_everything(py, &partition);
        assert_eq!(partition.intervals.len(), 3);
        // Blank lines stay with the preceding block so nothing is dropped.
        assert_eq!(partition.intervals[1].0, "import os\n\n\n".len());
        assert!(py[partition.intervals[1].0..].starts_with("def a"));
    }

    #[test]
    fn the_cap_merges_the_smallest_neighbours_and_a_tiny_file_is_whole() {
        let py = (0..20)
            .map(|i| format!("def f{i}():\n    return {i}\n"))
            .collect::<Vec<_>>()
            .join("\n");
        let partition = partition_file(&path("pkg/many.py"), &py, 5);
        assert_eq!(partition.intervals.len(), 5);
        covers_everything(&py, &partition);
        assert_eq!(partition.intervals[0].0, 0);
        assert_eq!(partition.intervals.last().unwrap().1, py.len());

        let tiny = partition_file(&path("x.txt"), "one block only\n", 48);
        assert_eq!(tiny.kind, PartitionKind::Whole);
        assert_eq!(tiny.intervals, vec![(0, "one block only\n".len())]);
        assert!(partition_file(&path("x.txt"), "", 48).intervals.is_empty());
    }

    #[test]
    fn partitions_are_deterministic_and_respect_utf8_boundaries() {
        let content = "// héllo\nfn a() {\n    let s = \"ção\";\n}\n\nfn b() {}\n";
        let first = partition_file(&path("src/u.rs"), content, 48);
        let second = partition_file(&path("src/u.rs"), content, 48);
        assert_eq!(first, second);
        for (start, end) in &first.intervals {
            assert!(content.is_char_boundary(*start) && content.is_char_boundary(*end));
        }
        covers_everything(content, &first);
    }
}
