use tree_sitter::{InputEdit, Parser, Point, Tree};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownSyntaxSnapshot {
    pub root_kind: String,
    pub has_error: bool,
    pub changed_ranges: usize,
}

pub struct MarkdownSyntax {
    parser: Parser,
    tree: Option<Tree>,
    source: String,
}

impl MarkdownSyntax {
    pub fn new(source: impl Into<String>) -> Result<Self, tree_sitter::LanguageError> {
        let mut parser = Parser::new();
        parser.set_language(&tree_sitter_md::LANGUAGE.into())?;
        let mut syntax = Self {
            parser,
            tree: None,
            source: String::new(),
        };
        syntax.update(source.into());
        Ok(syntax)
    }

    pub fn update(&mut self, new_source: impl Into<String>) -> MarkdownSyntaxSnapshot {
        let new_source = new_source.into();
        let old_tree = self.tree.take();
        let mut edited_tree = old_tree.clone();

        if let Some(tree) = edited_tree.as_mut() {
            tree.edit(&minimal_edit(&self.source, &new_source));
        }

        let parsed = self
            .parser
            .parse(&new_source, edited_tree.as_ref())
            .expect("o parser Tree-sitter Markdown não deve ser cancelado");
        let changed_ranges = old_tree
            .as_ref()
            .map(|old| old.changed_ranges(&parsed).count())
            .unwrap_or(1);
        let snapshot = MarkdownSyntaxSnapshot {
            root_kind: parsed.root_node().kind().to_string(),
            has_error: parsed.root_node().has_error(),
            changed_ranges,
        };
        self.source = new_source;
        self.tree = Some(parsed);
        snapshot
    }

    pub fn snapshot(&self) -> Option<MarkdownSyntaxSnapshot> {
        self.tree.as_ref().map(|tree| MarkdownSyntaxSnapshot {
            root_kind: tree.root_node().kind().to_string(),
            has_error: tree.root_node().has_error(),
            changed_ranges: 0,
        })
    }
}

fn minimal_edit(old: &str, new: &str) -> InputEdit {
    let mut start = old
        .as_bytes()
        .iter()
        .zip(new.as_bytes())
        .take_while(|(left, right)| left == right)
        .count();
    while start > 0 && (!old.is_char_boundary(start) || !new.is_char_boundary(start)) {
        start -= 1;
    }

    let max_suffix = old.len().min(new.len()).saturating_sub(start);
    let mut suffix = old
        .as_bytes()
        .iter()
        .rev()
        .zip(new.as_bytes().iter().rev())
        .take(max_suffix)
        .take_while(|(left, right)| left == right)
        .count();
    while suffix > 0
        && (!old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix))
    {
        suffix -= 1;
    }

    let old_end = old.len() - suffix;
    let new_end = new.len() - suffix;
    InputEdit {
        start_byte: start,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point_at(old, start),
        old_end_position: point_at(old, old_end),
        new_end_position: point_at(new, new_end),
    }
}

fn point_at(source: &str, byte: usize) -> Point {
    let prefix = &source[..byte];
    let row = prefix.bytes().filter(|byte| *byte == b'\n').count();
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix.len(), |(_, line)| line.len());
    Point::new(row, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incrementally_reparses_markdown_after_unicode_edit() {
        let mut syntax = MarkdownSyntax::new("# Título\n\nTexto").unwrap();
        let snapshot = syntax.update("# Título\n\nTexto com café");
        assert_eq!(snapshot.root_kind, "document");
        assert!(!snapshot.has_error);
        assert!(snapshot.changed_ranges > 0);
    }

    #[test]
    fn plain_text_does_not_require_a_syntax_tree() {
        let text = "arquivo.txt permanece plain text";
        assert_eq!(text.lines().count(), 1);
    }
}
