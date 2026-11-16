#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffMark {
    Neutral,
    Remove,
    Add,
    Change,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffSide {
    pub line: Option<usize>,
    pub text: String,
    pub marker: char,
    pub mark: DiffMark,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub left: DiffSide,
    pub right: DiffSide,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatContentPart {
    Markdown(String),
    UnifiedDiff { content: String, file_path: String },
}

pub fn parse_chat_content(content: &str) -> Vec<ChatContentPart> {
    const OPENING_FENCE: &str = "```diff";
    const CLOSING_FENCE: &str = "```";

    let mut parts = Vec::new();
    let mut cursor = 0;
    let mut search_cursor = 0;
    while let Some(relative_opening) = content[search_cursor..].find(OPENING_FENCE) {
        let opening = search_cursor + relative_opening;
        let after_label = opening + OPENING_FENCE.len();
        let Some(relative_newline) = content[after_label..].find('\n') else {
            break;
        };
        if !content[after_label..after_label + relative_newline]
            .chars()
            .all(char::is_whitespace)
        {
            search_cursor = after_label;
            continue;
        }
        let body_start = after_label + relative_newline + 1;
        let Some(relative_closing) = content[body_start..].find(CLOSING_FENCE) else {
            break;
        };
        let closing = body_start + relative_closing;
        if opening > cursor {
            parts.push(ChatContentPart::Markdown(
                content[cursor..opening].to_string(),
            ));
        }
        let diff = content[body_start..closing].trim_end().to_string();
        if let Some(file_path) = matching_unified_diff_path(&diff) {
            parts.push(ChatContentPart::UnifiedDiff {
                content: diff,
                file_path,
            });
        } else {
            parts.push(ChatContentPart::Markdown(
                content[opening..closing + CLOSING_FENCE.len()].to_string(),
            ));
        }
        cursor = closing + CLOSING_FENCE.len();
        search_cursor = cursor;
    }
    if cursor < content.len() {
        parts.push(ChatContentPart::Markdown(content[cursor..].to_string()));
    }
    if parts.is_empty() {
        parts.push(ChatContentPart::Markdown(content.to_string()));
    }
    parts
}

fn matching_unified_diff_path(diff: &str) -> Option<String> {
    let old_path = diff.lines().find_map(|line| line.strip_prefix("--- a/"));
    let new_path = diff.lines().find_map(|line| line.strip_prefix("+++ b/"));
    match (old_path, new_path) {
        (Some(old), Some(new)) if !old.is_empty() && old == new => Some(new.to_string()),
        _ => None,
    }
}

/// How many lines a change added and removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
}

/// The lines `generated` added to `input` and the lines it removed, by the
/// same line diff as [`build_diff_rows`].
pub fn diff_stats(input: &str, generated: &str) -> DiffStats {
    let edits = line_edits(&split_lines(input), &split_lines(generated));
    DiffStats {
        added: edits
            .iter()
            .filter(|edit| matches!(edit, Edit::Insert(_)))
            .count(),
        removed: edits
            .iter()
            .filter(|edit| matches!(edit, Edit::Delete(_)))
            .count(),
    }
}

/// The two texts side by side, aligned by a line diff: kept lines face each
/// other; in each changed run of lines, removed and added lines pair up as
/// changes and the rest stand alone.
pub fn build_diff_rows(input: &str, generated: &str) -> Vec<DiffRow> {
    let left_lines = split_lines(input);
    let right_lines = split_lines(generated);
    let mut rows = Vec::with_capacity(left_lines.len().max(right_lines.len()));
    let mut removed = Vec::new();
    let mut added = Vec::new();
    for edit in line_edits(&left_lines, &right_lines) {
        match edit {
            Edit::Keep(left, right) => {
                pair(
                    &mut rows,
                    &left_lines,
                    &right_lines,
                    &mut removed,
                    &mut added,
                );
                rows.push(DiffRow {
                    left: side(Some(left), &left_lines, ' ', DiffMark::Neutral),
                    right: side(Some(right), &right_lines, ' ', DiffMark::Neutral),
                });
            }
            Edit::Delete(left) => removed.push(left),
            Edit::Insert(right) => added.push(right),
        }
    }
    pair(
        &mut rows,
        &left_lines,
        &right_lines,
        &mut removed,
        &mut added,
    );
    rows
}

fn side(index: Option<usize>, lines: &[&str], marker: char, mark: DiffMark) -> DiffSide {
    DiffSide {
        line: index.map(|index| index + 1),
        text: index.map_or_else(String::new, |index| lines[index].to_owned()),
        marker,
        mark,
    }
}

/// Emits one changed run: removed and added lines in pairs, then the rest.
fn pair(
    rows: &mut Vec<DiffRow>,
    left_lines: &[&str],
    right_lines: &[&str],
    removed: &mut Vec<usize>,
    added: &mut Vec<usize>,
) {
    for index in 0..removed.len().max(added.len()) {
        let left = removed.get(index).copied();
        let right = added.get(index).copied();
        let (left_side, right_side) = match (left, right) {
            (Some(_), Some(_)) => (
                side(left, left_lines, '-', DiffMark::Change),
                side(right, right_lines, '+', DiffMark::Change),
            ),
            (Some(_), None) => (
                side(left, left_lines, '-', DiffMark::Remove),
                side(None, right_lines, ' ', DiffMark::Empty),
            ),
            _ => (
                side(None, left_lines, ' ', DiffMark::Empty),
                side(right, right_lines, '+', DiffMark::Add),
            ),
        };
        rows.push(DiffRow {
            left: left_side,
            right: right_side,
        });
    }
    removed.clear();
    added.clear();
}

/// One step of a line edit script, by line index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edit {
    Keep(usize, usize),
    Delete(usize),
    Insert(usize),
}

/// Beyond this many differing lines in the middle of two texts, the middle
/// counts as replaced as a whole: the search keeps O(D²) state.
const MAX_EDIT_DISTANCE: usize = 1_000;

/// A shortest line edit script from `left` to `right` (Myers), after the
/// common prefix and suffix, where most changes leave the bulk of a file.
fn line_edits(left: &[&str], right: &[&str]) -> Vec<Edit> {
    let prefix = left
        .iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = left[prefix..]
        .iter()
        .rev()
        .zip(right[prefix..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let middle_left = &left[prefix..left.len() - suffix];
    let middle_right = &right[prefix..right.len() - suffix];
    let mut edits = Vec::with_capacity(left.len().max(right.len()));
    edits.extend((0..prefix).map(|index| Edit::Keep(index, index)));
    match myers(middle_left, middle_right) {
        Some(middle) => edits.extend(middle.into_iter().map(|edit| match edit {
            Edit::Keep(left, right) => Edit::Keep(prefix + left, prefix + right),
            Edit::Delete(left) => Edit::Delete(prefix + left),
            Edit::Insert(right) => Edit::Insert(prefix + right),
        })),
        None => {
            edits.extend((0..middle_left.len()).map(|index| Edit::Delete(prefix + index)));
            edits.extend((0..middle_right.len()).map(|index| Edit::Insert(prefix + index)));
        }
    }
    edits.extend(
        (0..suffix)
            .map(|index| Edit::Keep(left.len() - suffix + index, right.len() - suffix + index)),
    );
    edits
}

/// Myers' greedy shortest edit script, or `None` past [`MAX_EDIT_DISTANCE`].
/// Each step keeps only the diagonals it can reach, `-d - 1..=d + 1`.
fn myers(left: &[&str], right: &[&str]) -> Option<Vec<Edit>> {
    let (n, m) = (left.len() as isize, right.len() as isize);
    let limit = (left.len() + right.len()).min(MAX_EDIT_DISTANCE) as isize;
    let offset = limit + 1;
    let mut frontier = vec![0_isize; (2 * limit + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'search: for d in 0..=limit {
        let low = (offset - d - 1) as usize;
        let high = (offset + d + 1) as usize;
        trace.push(frontier[low..=high].to_vec());
        for k in (-d..=d).step_by(2) {
            let at = |k: isize| frontier[(offset + k) as usize];
            let mut x = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
                at(k + 1)
            } else {
                at(k - 1) + 1
            };
            let mut y = x - k;
            while x < n && y < m && left[x as usize] == right[y as usize] {
                x += 1;
                y += 1;
            }
            frontier[(offset + k) as usize] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'search;
            }
        }
    }
    found?;
    let mut edits = Vec::new();
    let (mut x, mut y) = (n, m);
    for (d, window) in trace.iter().enumerate().rev() {
        let d = d as isize;
        // The window of step `d` starts at diagonal `-d - 1`.
        let at = |k: isize| window[(k + d + 1) as usize];
        let k = x - y;
        let previous = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = at(previous);
        let previous_y = previous_x - previous;
        while x > previous_x && y > previous_y {
            edits.push(Edit::Keep((x - 1) as usize, (y - 1) as usize));
            x -= 1;
            y -= 1;
        }
        if d > 0 {
            if x == previous_x {
                edits.push(Edit::Insert(previous_y as usize));
            } else {
                edits.push(Edit::Delete(previous_x as usize));
            }
        }
        x = previous_x;
        y = previous_y;
    }
    edits.reverse();
    Some(edits)
}

/// The lines of `content`. A final newline ends the last line instead of
/// starting an empty one.
fn split_lines(content: &str) -> Vec<&str> {
    let content = content.strip_suffix('\n').unwrap_or(content);
    if content.is_empty() {
        Vec::new()
    } else {
        content
            .split('\n')
            .map(|line| line.trim_end_matches('\r'))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_the_existing_side_by_side_contract() {
        let rows = build_diff_rows("same\nold", "same\nnew\nadded");
        assert_eq!(rows[0].left.mark, DiffMark::Neutral);
        assert_eq!(rows[1].left.mark, DiffMark::Change);
        assert_eq!(rows[1].right.mark, DiffMark::Change);
        assert_eq!(rows[2].right.mark, DiffMark::Add);
        assert_eq!(rows[2].right.line, Some(3));
    }

    /// A line added at the top shifts nothing else: the rest stays aligned.
    #[test]
    fn an_insertion_keeps_the_following_lines_aligned() {
        let rows = build_diff_rows("one\ntwo\nthree", "zero\none\ntwo\nthree");
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].left.mark, DiffMark::Empty);
        assert_eq!(rows[0].right.mark, DiffMark::Add);
        assert_eq!(rows[0].right.text, "zero");
        for row in &rows[1..] {
            assert_eq!(row.left.mark, DiffMark::Neutral);
            assert_eq!(row.left.text, row.right.text);
        }
        assert_eq!(rows[3].left.line, Some(3));
        assert_eq!(rows[3].right.line, Some(4));
        assert_eq!(
            diff_stats("one\ntwo\nthree", "zero\none\ntwo\nthree"),
            DiffStats {
                added: 1,
                removed: 0
            }
        );
    }

    /// A changed line in the middle pairs its removal with its addition; a
    /// removed line stands alone.
    #[test]
    fn changes_and_removals_pair_up_within_a_run() {
        let rows = build_diff_rows("a\nb\nc\nd\ne", "a\nB\nc\ne");
        let marks: Vec<_> = rows
            .iter()
            .map(|row| (row.left.mark, row.right.mark))
            .collect();
        assert_eq!(
            marks,
            [
                (DiffMark::Neutral, DiffMark::Neutral),
                (DiffMark::Change, DiffMark::Change),
                (DiffMark::Neutral, DiffMark::Neutral),
                (DiffMark::Remove, DiffMark::Empty),
                (DiffMark::Neutral, DiffMark::Neutral),
            ]
        );
        assert_eq!(
            diff_stats("a\nb\nc\nd\ne", "a\nB\nc\ne"),
            DiffStats {
                added: 1,
                removed: 2
            }
        );
        // Nothing before and nothing after.
        assert_eq!(
            diff_stats("", "x\ny"),
            DiffStats {
                added: 2,
                removed: 0
            }
        );
        assert_eq!(diff_stats("same\n", "same\n"), DiffStats::default());
    }

    /// Past the edit distance bound the middle counts as replaced, and the
    /// common start and end still line up.
    #[test]
    fn a_rewrite_past_the_bound_replaces_the_middle() {
        let old: String = (0..1_200).map(|index| format!("old {index}\n")).collect();
        let new: String = (0..1_200).map(|index| format!("new {index}\n")).collect();
        let input = format!("head\n{old}tail");
        let generated = format!("head\n{new}tail");
        let stats = diff_stats(&input, &generated);
        assert_eq!(
            stats,
            DiffStats {
                added: 1_200,
                removed: 1_200
            }
        );
        let rows = build_diff_rows(&input, &generated);
        assert_eq!(rows.first().unwrap().left.mark, DiffMark::Neutral);
        assert_eq!(rows.last().unwrap().right.text, "tail");
        assert_eq!(rows.len(), 1_202);
    }

    #[test]
    fn recognizes_only_matching_unified_diff_headers() {
        let valid = parse_chat_content(
            "```diff\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-old\n+new\n```",
        );
        let invalid = parse_chat_content("```diff\n-old\n+new\n```");

        assert!(matches!(
            &valid[0],
            ChatContentPart::UnifiedDiff { file_path, .. } if file_path == "src/a.rs"
        ));
        assert!(matches!(&invalid[0], ChatContentPart::Markdown(_)));
    }
}
