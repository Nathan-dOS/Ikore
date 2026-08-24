mod buffer;
mod diff;
mod markdown;
mod syntax;

pub use buffer::{EditorBuffer, EditorError, EditorSelection, EditorSnapshot, TextPosition};
pub use diff::{DiffMark, DiffRow, DiffSide, build_diff_rows};
pub use markdown::{MarkdownBlock, parse_markdown, sanitize_link_destination};
pub use syntax::{MarkdownSyntax, MarkdownSyntaxSnapshot};
