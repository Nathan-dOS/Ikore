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
    let mut suppressed_raw_tag: Option<&'static str> = None;

    for event in parser {
        if let Some(tag) = suppressed_raw_tag {
            if let Event::Html(html) | Event::InlineHtml(html) = &event
                && closes_raw_tag(html, tag)
            {
                suppressed_raw_tag = None;
            }
            continue;
        }
        if let Event::Html(html) | Event::InlineHtml(html) = &event {
            suppressed_raw_tag = opens_executable_raw_tag(html);
            continue;
        }
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
            Event::TaskListMarker(checked) => {
                current.push_str(if checked { "[x] " } else { "[ ] " });
            }
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

fn opens_executable_raw_tag(html: &str) -> Option<&'static str> {
    let normalized = html.trim().to_ascii_lowercase();
    for tag in ["script", "style"] {
        if normalized.starts_with(&format!("<{tag}"))
            && !normalized.contains(&format!("</{tag}>"))
            && !normalized.ends_with("/>")
        {
            return Some(tag);
        }
    }
    None
}

fn closes_raw_tag(html: &str, tag: &str) -> bool {
    html.trim()
        .to_ascii_lowercase()
        .starts_with(&format!("</{tag}"))
}

pub fn sanitize_link_destination(destination: &str) -> Option<String> {
    let destination = destination.trim();
    if destination.is_empty() || destination.chars().any(|character| character.is_control()) {
        return None;
    }
    let normalized = destination.to_ascii_lowercase();
    let scheme = normalized
        .find(':')
        .map(|separator| &normalized[..separator]);
    match scheme {
        None => Some(destination.to_owned()),
        Some("http" | "https" | "mailto") => Some(destination.to_owned()),
        Some(_) => None,
    }
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

        let tasks = parse_markdown("- [x] concluído\n- [ ] pendente");
        assert!(tasks.iter().any(
            |block| matches!(block, MarkdownBlock::ListItem(text) if text == "[x] concluído")
        ));
    }

    #[test]
    fn rejects_unsafe_link_protocols() {
        assert_eq!(sanitize_link_destination("javascript:alert(1)"), None);
        assert_eq!(
            sanitize_link_destination("data:text/html;base64,eA=="),
            None
        );
        assert_eq!(
            sanitize_link_destination("https://example.com"),
            Some("https://example.com".into())
        );
        assert_eq!(
            sanitize_link_destination("notes/roadmap.md#stage-2"),
            Some("notes/roadmap.md#stage-2".into())
        );
    }

    #[test]
    fn sanitizes_executable_markup_and_urls() {
        let blocks =
            parse_markdown("# Safe\n<script>alert(1)</script>\n[bad](javascript:alert(1))");
        let rendered = format!("{blocks:?}");
        assert!(rendered.contains("Safe"));
        assert!(!rendered.contains("<script"));
        assert!(!rendered.contains("javascript:"));
        assert!(sanitize_link_destination("javascript:alert(1)").is_none());

        let inline = parse_markdown("Seguro<script>alert(1)</script> depois");
        let rendered = format!("{inline:?}");
        assert!(rendered.contains("Seguro"));
        assert!(!rendered.contains("alert"));
    }
}
