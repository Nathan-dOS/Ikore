mod buffer;
mod diff;
mod markdown;
mod syntax;

pub use buffer::{EditorBuffer, EditorError, EditorSelection, EditorSnapshot, TextPosition};
pub use diff::{ChatContentPart, DiffMark, DiffRow, DiffSide, build_diff_rows, parse_chat_content};
pub use markdown::{MarkdownBlock, parse_markdown, sanitize_link_destination};
pub use syntax::{MarkdownSyntax, MarkdownSyntaxSnapshot};
