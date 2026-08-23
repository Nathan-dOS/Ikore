use std::cmp::Ordering;

use ropey::Rope;
use serde::{Deserialize, Serialize};

const HISTORY_LIMIT: usize = 128;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPosition {
    pub line: usize,
    pub column: usize,
}

impl Ord for TextPosition {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.line, self.column).cmp(&(other.line, other.column))
    }
}

impl PartialOrd for TextPosition {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorSelection {
    pub anchor: TextPosition,
    pub head: TextPosition,
}

impl EditorSelection {
    pub fn caret(position: TextPosition) -> Self {
        Self {
            anchor: position,
            head: position,
        }
    }

    pub fn is_empty(self) -> bool {
        self.anchor == self.head
    }

    pub fn ordered(self) -> (TextPosition, TextPosition) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorSnapshot {
    pub text: String,
    pub revision: String,
    pub selection: EditorSelection,
    pub scroll_top: f32,
    pub dirty: bool,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EditorError {
    #[error("posição fora dos limites do documento")]
    PositionOutOfBounds,
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    text: String,
    selection: EditorSelection,
}

#[derive(Debug, Clone)]
pub struct EditorBuffer {
    rope: Rope,
    revision: String,
    selection: EditorSelection,
    scroll_top: f32,
    dirty: bool,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
}

impl EditorBuffer {
    pub fn new(text: impl AsRef<str>, revision: impl Into<String>) -> Self {
        Self {
            rope: Rope::from_str(text.as_ref()),
            revision: revision.into(),
            selection: EditorSelection::default(),
            scroll_top: 0.0,
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn selection(&self) -> EditorSelection {
        self.selection
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            text: self.text(),
            revision: self.revision.clone(),
            selection: self.selection,
            scroll_top: self.scroll_top,
            dirty: self.dirty,
        }
    }

    pub fn set_selection(&mut self, selection: EditorSelection) -> Result<(), EditorError> {
        self.position_to_char(selection.anchor)?;
        self.position_to_char(selection.head)?;
        self.selection = selection;
        Ok(())
    }

    pub fn set_scroll_top(&mut self, scroll_top: f32) {
        self.scroll_top = scroll_top.max(0.0);
    }

    pub fn insert(&mut self, text: &str) -> Result<(), EditorError> {
        self.replace_selection(text)
    }

    pub fn replace_selection(&mut self, replacement: &str) -> Result<(), EditorError> {
        let (start_position, end_position) = self.selection.ordered();
        let start = self.position_to_char(start_position)?;
        let end = self.position_to_char(end_position)?;
        self.push_undo();
        self.rope.remove(start..end);
        self.rope.insert(start, replacement);
        let caret = self.char_to_position(start + replacement.chars().count());
        self.selection = EditorSelection::caret(caret);
        self.dirty = true;
        self.redo.clear();
        Ok(())
    }

    pub fn backspace(&mut self) -> Result<(), EditorError> {
        if !self.selection.is_empty() {
            return self.replace_selection("");
        }
        let caret = self.position_to_char(self.selection.head)?;
        if caret == 0 {
            return Ok(());
        }
        self.push_undo();
        self.rope.remove(caret - 1..caret);
        self.selection = EditorSelection::caret(self.char_to_position(caret - 1));
        self.dirty = true;
        self.redo.clear();
        Ok(())
    }

    pub fn delete_forward(&mut self) -> Result<(), EditorError> {
        if !self.selection.is_empty() {
            return self.replace_selection("");
        }
        let caret = self.position_to_char(self.selection.head)?;
        if caret >= self.rope.len_chars() {
            return Ok(());
        }
        self.push_undo();
        self.rope.remove(caret..caret + 1);
        self.selection = EditorSelection::caret(self.char_to_position(caret));
        self.dirty = true;
        self.redo.clear();
        Ok(())
    }

    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.history_entry());
        self.restore(entry);
        self.dirty = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(entry) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.history_entry());
        self.restore(entry);
        self.dirty = true;
        true
    }

    pub fn mark_saved(&mut self, revision: impl Into<String>) {
        self.revision = revision.into();
        self.dirty = false;
    }

    pub fn reload(&mut self, text: impl AsRef<str>, revision: impl Into<String>) {
        self.rope = Rope::from_str(text.as_ref());
        self.revision = revision.into();
        self.selection = EditorSelection::default();
        self.scroll_top = 0.0;
        self.dirty = false;
        self.undo.clear();
        self.redo.clear();
    }

    pub fn utf16_offset(&self, position: TextPosition) -> Result<usize, EditorError> {
        let char_index = self.position_to_char(position)?;
        Ok(self
            .rope
            .chars_at(0)
            .take(char_index)
            .map(char::len_utf16)
            .sum())
    }

    pub fn position_from_utf16(&self, offset: usize) -> TextPosition {
        let mut utf16 = 0;
        let mut chars = 0;
        for character in self.rope.chars() {
            let next = utf16 + character.len_utf16();
            if next > offset {
                break;
            }
            utf16 = next;
            chars += 1;
        }
        self.char_to_position(chars)
    }

    pub fn utf16_len(&self) -> usize {
        self.rope.chars().map(char::len_utf16).sum()
    }

    pub fn set_selection_utf16(&mut self, anchor: usize, head: usize) -> Result<(), EditorError> {
        self.set_selection(EditorSelection {
            anchor: self.position_from_utf16(anchor),
            head: self.position_from_utf16(head),
        })
    }

    pub fn text_for_utf16_range(
        &self,
        range: std::ops::Range<usize>,
    ) -> Result<String, EditorError> {
        let start = self.position_to_char(self.position_from_utf16(range.start))?;
        let end = self.position_to_char(self.position_from_utf16(range.end))?;
        Ok(self.rope.slice(start..end).to_string())
    }

    fn position_to_char(&self, position: TextPosition) -> Result<usize, EditorError> {
        if position.line >= self.rope.len_lines() {
            return Err(EditorError::PositionOutOfBounds);
        }
        let line_start = self.rope.line_to_char(position.line);
        let line = self.rope.line(position.line);
        let content_chars = line
            .chars()
            .take_while(|character| *character != '\n' && *character != '\r')
            .count();
        if position.column > content_chars {
            return Err(EditorError::PositionOutOfBounds);
        }
        Ok(line_start + position.column)
    }

    fn char_to_position(&self, char_index: usize) -> TextPosition {
        let bounded = char_index.min(self.rope.len_chars());
        let line = self.rope.char_to_line(bounded);
        TextPosition {
            line,
            column: bounded.saturating_sub(self.rope.line_to_char(line)),
        }
    }

    fn history_entry(&self) -> HistoryEntry {
        HistoryEntry {
            text: self.text(),
            selection: self.selection,
        }
    }

    fn push_undo(&mut self) {
        self.undo.push(self.history_entry());
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
    }

    fn restore(&mut self, entry: HistoryEntry) {
        self.rope = Rope::from_str(&entry.text);
        self.selection = entry.selection;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_unicode_selection_and_round_trips_utf16() {
        let mut buffer = EditorBuffer::new("Olá 👋 mundo", "r1");
        buffer
            .set_selection(EditorSelection {
                anchor: TextPosition { line: 0, column: 4 },
                head: TextPosition { line: 0, column: 5 },
            })
            .unwrap();
        buffer.insert("🌎").unwrap();
        assert_eq!(buffer.text(), "Olá 🌎 mundo");
        let position = TextPosition { line: 0, column: 5 };
        assert_eq!(
            buffer.position_from_utf16(buffer.utf16_offset(position).unwrap()),
            position
        );
    }

    #[test]
    fn undo_and_redo_preserve_crlf_content() {
        let mut buffer = EditorBuffer::new("one\r\ntwo", "r1");
        buffer
            .set_selection(EditorSelection::caret(TextPosition { line: 1, column: 3 }))
            .unwrap();
        buffer.insert("!").unwrap();
        assert_eq!(buffer.text(), "one\r\ntwo!");
        assert!(buffer.undo());
        assert_eq!(buffer.text(), "one\r\ntwo");
        assert!(buffer.redo());
        assert_eq!(buffer.text(), "one\r\ntwo!");
    }

    #[test]
    fn handles_large_documents_without_changing_line_count() {
        let text = (0..20_000)
            .map(|line| format!("line {line}\n"))
            .collect::<String>();
        let buffer = EditorBuffer::new(&text, "r1");
        assert_eq!(buffer.snapshot().text, text);
    }

    #[test]
    fn mark_saved_and_reload_reset_dirty_state() {
        let mut buffer = EditorBuffer::new("draft", "r1");
        buffer.insert("!").unwrap();
        assert!(buffer.is_dirty());
        buffer.mark_saved("r2");
        assert!(!buffer.is_dirty());
        buffer.reload("server", "r3");
        assert_eq!(buffer.text(), "server");
        assert!(!buffer.is_dirty());
    }
}
