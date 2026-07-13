//! A minimal single-line text field with a movable cursor.
//!
//! Tracks the cursor as a *character* index (not a byte offset) so editing is
//! correct for multi-byte UTF-8, and edits happen at the cursor rather than only
//! at the end.

#[derive(Debug, Default, Clone)]
pub struct TextInput {
    text: String,
    /// Cursor position as a char index in `0..=char_count`.
    cursor: usize,
}

impl TextInput {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Replace the contents and place the cursor at the end.
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.char_count();
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte_of(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    /// Delete the character before the cursor (Backspace).
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let start = self.byte_of(self.cursor - 1);
            let end = self.byte_of(self.cursor);
            self.text.replace_range(start..end, "");
            self.cursor -= 1;
        }
    }

    /// Delete the character at the cursor (Delete).
    pub fn delete(&mut self) {
        if self.cursor < self.char_count() {
            let start = self.byte_of(self.cursor);
            let end = self.byte_of(self.cursor + 1);
            self.text.replace_range(start..end, "");
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.cursor < self.char_count() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.char_count();
    }

    fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    /// Byte offset of the given char index (or the string length at the end).
    fn byte_of(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_at_cursor() {
        let mut input = TextInput::new();
        input.set("disk.img");
        // Move to just before ".img": 8 chars, ".img" is 4 → cursor to 4.
        for _ in 0..4 {
            input.left();
        }
        assert_eq!(input.cursor(), 4);
        // Backspacing here removes "disk", keeping ".img".
        for _ in 0..4 {
            input.backspace();
        }
        assert_eq!(input.text(), ".img");
        assert_eq!(input.cursor(), 0);
        input.insert('a');
        assert_eq!(input.text(), "a.img");
        assert_eq!(input.cursor(), 1);
    }

    #[test]
    fn delete_and_bounds() {
        let mut input = TextInput::new();
        input.set("ab");
        input.home();
        input.delete();
        assert_eq!(input.text(), "b");
        input.left(); // already at 0, stays
        assert_eq!(input.cursor(), 0);
        input.right();
        input.right(); // clamps at end (len 1)
        assert_eq!(input.cursor(), 1);
    }
}
