use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownBlock {
    Paragraph(String),
    Heading {
        level: u8,
        text: String,
    },
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    Quote(String),
    ListItem(String),
    Rule,
}

pub fn parse_markdown(source: &str) -> Vec<MarkdownBlock> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let parser = Parser::new_ext(source, options);
    let mut blocks = Vec::new();
    let mut current = String::new();
    let mut block = BlockKind::Paragraph;

    for event in parser {
        match event {
            Event::Start(Tag::Paragraph) => {
                current.clear();
                block = BlockKind::Paragraph;
            }
            Event::Start(Tag::Heading { level, .. }) => {
                current.clear();
                block = BlockKind::Heading(heading_level(level));
            }
            Event::Start(Tag::BlockQuote(_)) => {
                current.clear();
                block = BlockKind::Quote;
            }
            Event::Start(Tag::Item) => {
                current.clear();
                block = BlockKind::ListItem;
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                current.clear();
                block = BlockKind::CodeBlock(match kind {
                    CodeBlockKind::Indented => None,
                    CodeBlockKind::Fenced(language) if language.is_empty() => None,
                    CodeBlockKind::Fenced(language) => Some(language.into_string()),
                });
            }
            Event::Text(text) | Event::Code(text) => current.push_str(&text),
            Event::SoftBreak | Event::HardBreak => current.push('\n'),
            Event::Rule => blocks.push(MarkdownBlock::Rule),
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::BlockQuote(_)
                | TagEnd::Item
                | TagEnd::CodeBlock,
            ) if !current.is_empty() => {
                blocks.push(block.finish(std::mem::take(&mut current)));
            }
            _ => {}
        }
    }
    blocks
}

#[derive(Debug, Clone)]
enum BlockKind {
    Paragraph,
    Heading(u8),
    CodeBlock(Option<String>),
    Quote,
    ListItem,
}

impl BlockKind {
    fn finish(&self, text: String) -> MarkdownBlock {
        match self {
            Self::Paragraph => MarkdownBlock::Paragraph(text),
            Self::Heading(level) => MarkdownBlock::Heading {
                level: *level,
                text,
            },
            Self::CodeBlock(language) => MarkdownBlock::CodeBlock {
                language: language.clone(),
                code: text,
            },
            Self::Quote => MarkdownBlock::Quote(text),
            Self::ListItem => MarkdownBlock::ListItem(text),
        }
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gfm_blocks_and_ignores_raw_html() {
        let blocks = parse_markdown(
            "# Title\n\nParagraph ~~old~~.\n\n<script>alert('x')</script>\n\n~~~rust\nfn main() {}\n~~~",
        );
        assert!(matches!(
            &blocks[0],
            MarkdownBlock::Heading { level: 1, text } if text == "Title"
        ));
        assert!(blocks.iter().any(
            |block| matches!(block, MarkdownBlock::Paragraph(text) if text == "Paragraph old.")
        ));
        assert!(
            !blocks
                .iter()
                .any(|block| format!("{block:?}").contains("alert"))
        );
        assert!(blocks.iter().any(|block| matches!(
            block,
            MarkdownBlock::CodeBlock { language: Some(language), code }
                if language == "rust" && code.contains("fn main")
        )));
    }
}
