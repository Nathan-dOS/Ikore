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
}
