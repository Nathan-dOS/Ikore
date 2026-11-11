//! Bounded line diff: the Integrator's base → candidate section (ADR-032), the
//! documentation reconciliation and the resume notice (ADR-037 M3).
//!
//! Myers' O((N+M)·D) shortest edit script over lines, after trimming the
//! common prefix and suffix. Lines keep their terminator while comparing, so a
//! missing final newline or a CRLF change is a real difference. Inputs beyond
//! the line or edit-distance limits are reported as too large instead of
//! degrading into an approximate diff.

use std::fmt::Write as _;

pub const MAX_LINES: usize = 4_000;
pub const MAX_EDIT_DISTANCE: usize = 1_000;
pub const CONTEXT_LINES: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineDiff {
    Identical,
    /// Unified hunks (`@@ -a,b +c,d @@`), cut at a line boundary when
    /// `truncated`.
    Unified {
        text: String,
        truncated: bool,
    },
    TooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Equal,
    Delete,
    Insert,
}

fn split_lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// Unified diff of `base` → `candidate`, at most `max_chars` Unicode scalar
/// values of hunk text.
pub fn unified(base: &str, candidate: &str, max_chars: usize) -> LineDiff {
    if base == candidate {
        return LineDiff::Identical;
    }
    let a = split_lines(base);
    let b = split_lines(candidate);
    if a.len() > MAX_LINES || b.len() > MAX_LINES {
        return LineDiff::TooLarge;
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let Some(middle) = shortest_edit(&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix])
    else {
        return LineDiff::TooLarge;
    };
    let mut ops = vec![Op::Equal; prefix];
    ops.extend(middle);
    ops.extend(std::iter::repeat_n(Op::Equal, suffix));
    let (text, truncated) = render_hunks(&ops, &a, &b, max_chars);
    LineDiff::Unified { text, truncated }
}

/// Myers' greedy forward search with a stored trace for backtracking.
fn shortest_edit(a: &[&str], b: &[&str]) -> Option<Vec<Op>> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    let limit = (a.len() + b.len()).min(MAX_EDIT_DISTANCE) as isize;
    let offset = limit + 1;
    let mut v = vec![0isize; (2 * limit + 3) as usize];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    for d in 0..=limit {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let index = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
                v[index + 1]
            } else {
                v[index - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[index] = x;
            if x >= n && y >= m {
                return Some(backtrack(&trace, n, m, offset));
            }
            k += 2;
        }
    }
    None
}

fn backtrack(trace: &[Vec<isize>], n: isize, m: isize, offset: isize) -> Vec<Op> {
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for (d, v) in trace.iter().enumerate().rev() {
        let d = d as isize;
        let k = x - y;
        if d == 0 {
            while x > 0 && y > 0 {
                ops.push(Op::Equal);
                x -= 1;
                y -= 1;
            }
            break;
        }
        let previous_k =
            if k == -d || (k != d && v[(k - 1 + offset) as usize] < v[(k + 1 + offset) as usize]) {
                k + 1
            } else {
                k - 1
            };
        let previous_x = v[(previous_k + offset) as usize];
        let previous_y = previous_x - previous_k;
        while x > previous_x && y > previous_y {
            ops.push(Op::Equal);
            x -= 1;
            y -= 1;
        }
        ops.push(if previous_k == k + 1 {
            Op::Insert
        } else {
            Op::Delete
        });
        x = previous_x;
        y = previous_y;
    }
    ops.reverse();
    ops
}

fn display(line: &str) -> &str {
    line.strip_suffix('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .unwrap_or(line)
}

/// Groups the script into hunks with `CONTEXT_LINES` lines of context and
/// renders them until `max_chars` would be exceeded.
fn render_hunks(ops: &[Op], a: &[&str], b: &[&str], max_chars: usize) -> (String, bool) {
    // (op, base index, candidate index) for every step of the script.
    let mut steps = Vec::with_capacity(ops.len());
    let (mut i, mut j) = (0usize, 0usize);
    for op in ops {
        steps.push((*op, i, j));
        match op {
            Op::Equal => {
                i += 1;
                j += 1;
            }
            Op::Delete => i += 1,
            Op::Insert => j += 1,
        }
    }
    let changed = steps
        .iter()
        .enumerate()
        .filter(|(_, (op, _, _))| *op != Op::Equal)
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for index in changed {
        let start = index.saturating_sub(CONTEXT_LINES);
        let end = (index + CONTEXT_LINES + 1).min(steps.len());
        match ranges.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => ranges.push((start, end)),
        }
    }
    let mut out = String::new();
    let mut chars = 0usize;
    for (start, end) in ranges {
        let slice = &steps[start..end];
        let base_len = slice.iter().filter(|(op, _, _)| *op != Op::Insert).count();
        let candidate_len = slice.iter().filter(|(op, _, _)| *op != Op::Delete).count();
        let base_start = if base_len == 0 {
            slice[0].1
        } else {
            slice[0].1 + 1
        };
        let candidate_start = if candidate_len == 0 {
            slice[0].2
        } else {
            slice[0].2 + 1
        };
        let mut hunk =
            format!("@@ -{base_start},{base_len} +{candidate_start},{candidate_len} @@\n");
        for (op, i, j) in slice {
            let (marker, line) = match op {
                Op::Equal => (' ', a[*i]),
                Op::Delete => ('-', a[*i]),
                Op::Insert => ('+', b[*j]),
            };
            let _ = writeln!(hunk, "{marker}{}", display(line));
            if !line.ends_with('\n') {
                hunk.push_str("\\ No newline at end of file\n");
            }
        }
        let hunk_chars = hunk.chars().count();
        if chars + hunk_chars > max_chars {
            // Keep whole lines of the first hunk that does not fit.
            for line in hunk.split_inclusive('\n') {
                let line_chars = line.chars().count();
                if chars + line_chars > max_chars {
                    break;
                }
                out.push_str(line);
                chars += line_chars;
            }
            return (out, true);
        }
        out.push_str(&hunk);
        chars += hunk_chars;
    }
    (out, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Applies unified hunks to `base`; a test-only inverse of `unified`.
    fn apply(base: &str, diff: &str) -> String {
        let lines = split_lines(base);
        let mut out = String::new();
        let mut cursor = 0usize;
        let mut pending_newline_strip = false;
        for line in diff.split_inclusive('\n') {
            if let Some(header) = line.strip_prefix("@@ -") {
                let start = header
                    .split([',', ' '])
                    .next()
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap();
                let len = header
                    .split(',')
                    .nth(1)
                    .and_then(|value| value.split(' ').next())
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap();
                let begin = if len == 0 { start } else { start - 1 };
                for line in &lines[cursor..begin] {
                    out.push_str(line);
                }
                cursor = begin;
                continue;
            }
            if line.starts_with('\\') {
                if pending_newline_strip && out.ends_with('\n') {
                    out.pop();
                }
                continue;
            }
            let (marker, rest) = line.split_at(1);
            match marker {
                " " => {
                    out.push_str(lines[cursor]);
                    cursor += 1;
                    pending_newline_strip = false;
                }
                "-" => {
                    cursor += 1;
                    pending_newline_strip = false;
                }
                "+" => {
                    out.push_str(rest);
                    pending_newline_strip = true;
                }
                _ => unreachable!(),
            }
        }
        for line in &lines[cursor..] {
            out.push_str(line);
        }
        out
    }

    fn roundtrip(base: &str, candidate: &str) {
        match unified(base, candidate, usize::MAX) {
            LineDiff::Identical => assert_eq!(base, candidate),
            LineDiff::Unified { text, truncated } => {
                assert!(!truncated);
                assert_eq!(apply(base, &text), candidate, "diff:\n{text}");
            }
            LineDiff::TooLarge => panic!("unexpected limit"),
        }
    }

    #[test]
    fn identical_inputs_have_no_hunks() {
        assert_eq!(unified("a\nb\n", "a\nb\n", 100), LineDiff::Identical);
    }

    #[test]
    fn a_changed_line_is_reported_with_its_context() {
        let base = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let candidate = "1\n2\n3\n4\nfive\n6\n7\n8\n9\n";
        let LineDiff::Unified { text, truncated } = unified(base, candidate, usize::MAX) else {
            panic!("expected a diff");
        };
        assert!(!truncated);
        assert_eq!(text, "@@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+five\n 6\n 7\n 8\n");
    }

    #[test]
    fn creation_deletion_and_missing_final_newline_round_trip() {
        roundtrip("", "fn main() {}\n");
        roundtrip("fn main() {}\n", "");
        roundtrip("a\nb", "a\nb\n");
        roundtrip("a\r\nb\r\n", "a\nb\n");
        roundtrip("x\ny\nz\n", "y\nz\nw\n");
    }

    #[test]
    fn generated_edits_round_trip() {
        // Deterministic pseudo-random edits: insert, delete and replace lines.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..200 {
            let base = (0..(next() % 40))
                .map(|index| format!("line {}\n", (index + next() % 5) % 13))
                .collect::<String>();
            let mut lines = base.lines().map(str::to_owned).collect::<Vec<_>>();
            for _ in 0..(next() % 6) {
                let position = if lines.is_empty() {
                    0
                } else {
                    (next() as usize) % lines.len()
                };
                match next() % 3 {
                    0 => lines.insert(position, format!("new {}", next() % 7)),
                    1 if !lines.is_empty() => {
                        lines.remove(position);
                    }
                    _ if !lines.is_empty() => lines[position] = format!("changed {}", next() % 7),
                    _ => {}
                }
            }
            let candidate = lines
                .iter()
                .map(|line| format!("{line}\n"))
                .collect::<String>();
            roundtrip(&base, &candidate);
        }
    }

    #[test]
    fn output_is_cut_at_a_line_boundary() {
        let base = (0..200).map(|i| format!("{i}\n")).collect::<String>();
        let candidate = (0..200).map(|i| format!("{}\n", i * 2)).collect::<String>();
        let LineDiff::Unified { text, truncated } = unified(&base, &candidate, 120) else {
            panic!("expected a diff");
        };
        assert!(truncated);
        assert!(text.chars().count() <= 120);
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn oversized_inputs_are_refused() {
        let large = "x\n".repeat(MAX_LINES + 1);
        assert_eq!(unified(&large, "x\n", 100), LineDiff::TooLarge);
        let base = (0..1_200).map(|i| format!("a{i}\n")).collect::<String>();
        let candidate = (0..1_200).map(|i| format!("b{i}\n")).collect::<String>();
        assert_eq!(unified(&base, &candidate, 100), LineDiff::TooLarge);
    }
}
