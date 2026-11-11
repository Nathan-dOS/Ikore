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

pub fn build_diff_rows(input: &str, generated: &str) -> Vec<DiffRow> {
    let left_lines = split_lines(input);
    let right_lines = split_lines(generated);
    let mut rows = Vec::with_capacity(left_lines.len().max(right_lines.len()));
    let mut left_line = 0;
    let mut right_line = 0;

    for index in 0..left_lines.len().max(right_lines.len()) {
        let left = left_lines.get(index).copied();
        let right = right_lines.get(index).copied();
        let left_number = left.map(|_| {
            left_line += 1;
            left_line
        });
        let right_number = right.map(|_| {
            right_line += 1;
            right_line
        });

        let (left_mark, right_mark, left_marker, right_marker) = match (left, right) {
            (Some(left), Some(right)) if left == right => {
                (DiffMark::Neutral, DiffMark::Neutral, ' ', ' ')
            }
            (None, Some(_)) => (DiffMark::Empty, DiffMark::Add, ' ', '+'),
            (Some(_), None) => (DiffMark::Remove, DiffMark::Empty, '-', ' '),
            _ => (DiffMark::Change, DiffMark::Change, '-', '+'),
        };

        rows.push(DiffRow {
            left: DiffSide {
                line: left_number,
                text: left.unwrap_or_default().into(),
                marker: left_marker,
                mark: left_mark,
            },
            right: DiffSide {
                line: right_number,
                text: right.unwrap_or_default().into(),
                marker: right_marker,
                mark: right_mark,
            },
        });
    }
    rows
}

fn split_lines(content: &str) -> Vec<&str> {
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
