//! Character-safe multiline text buffer with Vim editing primitives.
//!
//! All cursor positions are in **character indices** (not byte offsets).
//! This is critical for correct Unicode handling.

use crate::command::TextObject;
use crate::motion::Motion;

/// A multiline text buffer with character-indexed cursor.
pub struct TextBuffer {
    /// Lines of text (at least one empty line always present).
    pub lines: Vec<String>,
    /// Cursor row (0-indexed into `lines`).
    cursor_row: usize,
    /// Cursor column (character index, not byte offset).
    cursor_col: usize,
    /// Preferred column for vertical movement.
    preferred_col: Option<usize>,
    /// Simple undo stack: snapshots of (lines, row, col).
    undo_stack: Vec<(Vec<String>, usize, usize)>,
}

impl TextBuffer {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            preferred_col: None,
            undo_stack: Vec::new(),
        }
    }

    // ── Accessors ───────────────────────────────────────────────────────

    /// Returns (row, col) in character indices.
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    /// Set cursor position, clamped to valid range.
    pub fn set_cursor(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.lines.len().saturating_sub(1));
        let line_len = self.line_len(self.cursor_row);
        self.cursor_col = col.min(line_len);
        self.preferred_col = None;
    }

    /// Number of characters in the given line.
    pub fn line_len(&self, row: usize) -> usize {
        self.lines.get(row).map(|l| l.chars().count()).unwrap_or(0)
    }

    /// Number of lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Full text as a single string.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Replace all text and reset cursor.
    pub fn set_text(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.lines().map(|l| l.to_string()).collect()
        };
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.preferred_col = None;
    }

    /// Column of first non-blank character on the given line.
    pub fn first_non_blank(&self, row: usize) -> usize {
        if let Some(line) = self.lines.get(row) {
            for (i, ch) in line.chars().enumerate() {
                if !ch.is_whitespace() {
                    return i;
                }
            }
        }
        0
    }

    // ── Snapshot for undo ───────────────────────────────────────────────

    fn push_undo(&mut self) {
        if self.undo_stack.len() > 100 {
            self.undo_stack.remove(0);
        }
        self.undo_stack
            .push((self.lines.clone(), self.cursor_row, self.cursor_col));
    }

    pub fn undo(&mut self) {
        if let Some((lines, row, col)) = self.undo_stack.pop() {
            self.lines = lines;
            self.cursor_row = row;
            self.cursor_col = col;
        }
    }

    // ── Basic editing ───────────────────────────────────────────────────

    /// Insert a character at the cursor.
    pub fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_row];
        let byte_pos = char_to_byte(line, self.cursor_col);
        line.insert(byte_pos, c);
        self.cursor_col += 1;
        self.preferred_col = None;
    }

    /// Insert a string at the cursor.
    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            if c == '\n' {
                self.insert_newline();
            } else {
                self.insert_char(c);
            }
        }
    }

    /// Insert a newline, splitting the current line.
    pub fn insert_newline(&mut self) {
        let line = &self.lines[self.cursor_row];
        let byte_pos = char_to_byte(line, self.cursor_col);
        let after = line[byte_pos..].to_string();
        self.lines[self.cursor_row].truncate(byte_pos);
        self.cursor_row += 1;
        self.lines.insert(self.cursor_row, after);
        self.cursor_col = 0;
        self.preferred_col = None;
    }

    /// Delete the character before the cursor (backspace).
    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let new_col = self.cursor_col - 1;
            let line = &mut self.lines[self.cursor_row];
            let byte_start = char_to_byte(line, new_col);
            let byte_end = char_to_byte(line, self.cursor_col);
            line.drain(byte_start..byte_end);
            self.cursor_col = new_col;
        } else if self.cursor_row > 0 {
            let current = self.lines.remove(self.cursor_row);
            self.cursor_row -= 1;
            self.cursor_col = self.lines[self.cursor_row].chars().count();
            self.lines[self.cursor_row].push_str(&current);
        }
        self.preferred_col = None;
    }

    /// Delete the character under the cursor (forward delete).
    pub fn delete_forward(&mut self) {
        let line_len = self.line_len(self.cursor_row);
        if self.cursor_col < line_len {
            let line = &mut self.lines[self.cursor_row];
            let byte_start = char_to_byte(line, self.cursor_col);
            let byte_end = char_to_byte(line, self.cursor_col + 1);
            line.drain(byte_start..byte_end);
        } else if self.cursor_row < self.lines.len() - 1 {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next);
        }
        self.preferred_col = None;
    }

    /// Replace the character under the cursor.
    pub fn replace_char(&mut self, ch: char) {
        self.push_undo();
        let line_len = self.line_len(self.cursor_row);
        if self.cursor_col < line_len {
            let line = &mut self.lines[self.cursor_row];
            let byte_start = char_to_byte(line, self.cursor_col);
            let byte_end = char_to_byte(line, self.cursor_col + 1);
            line.replace_range(byte_start..byte_end, &ch.to_string());
        }
    }

    /// Toggle case of character under cursor and advance.
    pub fn toggle_case(&mut self) {
        let line_len = self.line_len(self.cursor_row);
        if self.cursor_col < line_len {
            let line = &mut self.lines[self.cursor_row];
            let byte_start = char_to_byte(line, self.cursor_col);
            let byte_end = char_to_byte(line, self.cursor_col + 1);
            let ch = line[byte_start..byte_end].chars().next().unwrap();
            let toggled: String = if ch.is_uppercase() {
                ch.to_lowercase().collect()
            } else {
                ch.to_uppercase().collect()
            };
            line.replace_range(byte_start..byte_end, &toggled);
            if self.cursor_col + 1 < self.line_len(self.cursor_row) {
                self.cursor_col += 1;
            }
        }
    }

    /// Join current line with the next line.
    pub fn join_lines(&mut self) {
        self.push_undo();
        if self.cursor_row < self.lines.len() - 1 {
            let next = self.lines.remove(self.cursor_row + 1);
            let trimmed = next.trim_start();
            let join_col = self.lines[self.cursor_row].chars().count();
            if !self.lines[self.cursor_row].is_empty() && !trimmed.is_empty() {
                self.lines[self.cursor_row].push(' ');
                self.cursor_col = join_col;
            }
            self.lines[self.cursor_row].push_str(trimmed);
        }
    }

    /// Indent the current line right by 2 spaces.
    pub fn indent_right(&mut self) {
        self.push_undo();
        self.lines[self.cursor_row].insert_str(0, "  ");
        self.cursor_col += 2;
    }

    /// Indent the current line left by removing up to 2 leading spaces.
    pub fn indent_left(&mut self) {
        self.push_undo();
        let line = &self.lines[self.cursor_row];
        let spaces = line.chars().take_while(|c| *c == ' ').count().min(2);
        if spaces > 0 {
            let byte_end = char_to_byte(&self.lines[self.cursor_row], spaces);
            self.lines[self.cursor_row].drain(..byte_end);
            self.cursor_col = self.cursor_col.saturating_sub(spaces);
        }
    }

    /// Open a new line below and move cursor there.
    pub fn open_line_below(&mut self) {
        self.push_undo();
        self.cursor_row += 1;
        self.lines.insert(self.cursor_row, String::new());
        self.cursor_col = 0;
        self.preferred_col = None;
    }

    /// Open a new line above and move cursor there.
    pub fn open_line_above(&mut self) {
        self.push_undo();
        self.lines.insert(self.cursor_row, String::new());
        self.cursor_col = 0;
        self.preferred_col = None;
    }

    /// Paste text as a new line below.
    pub fn paste_line_below(&mut self, text: &str) {
        self.push_undo();
        for (i, line) in text.lines().enumerate() {
            self.lines
                .insert(self.cursor_row + 1 + i, line.to_string());
        }
        self.cursor_row += 1;
        self.cursor_col = self.first_non_blank(self.cursor_row);
    }

    /// Paste text as a new line above.
    pub fn paste_line_above(&mut self, text: &str) {
        self.push_undo();
        for (i, line) in text.lines().enumerate() {
            self.lines.insert(self.cursor_row + i, line.to_string());
        }
        self.cursor_col = self.first_non_blank(self.cursor_row);
    }

    // ── Range operations ────────────────────────────────────────────────

    /// Delete text from (from_row, from_col) to (to_row, to_col) exclusive.
    /// Returns the deleted text.
    pub fn delete_range(
        &mut self,
        from_row: usize,
        from_col: usize,
        to_row: usize,
        to_col: usize,
    ) -> String {
        self.push_undo();

        if from_row == to_row {
            // Single line delete.
            let line = &mut self.lines[from_row];
            let byte_start = char_to_byte(line, from_col);
            let byte_end = char_to_byte(line, to_col);
            let deleted: String = line[byte_start..byte_end].to_string();
            line.drain(byte_start..byte_end);
            self.cursor_col = from_col;
            return deleted;
        }

        // Multi-line delete.
        let mut deleted = String::new();

        // Capture first line fragment.
        let first_byte = char_to_byte(&self.lines[from_row], from_col);
        deleted.push_str(&self.lines[from_row][first_byte..]);

        // Capture and remove middle lines.
        for _ in (from_row + 1)..to_row {
            deleted.push('\n');
            deleted.push_str(&self.lines[from_row + 1]);
            self.lines.remove(from_row + 1);
        }

        // Capture last line fragment and merge.
        let last_row = from_row + 1;
        if last_row < self.lines.len() {
            let last_byte = char_to_byte(&self.lines[last_row], to_col);
            deleted.push('\n');
            deleted.push_str(&self.lines[last_row][..last_byte]);
            let remainder = self.lines[last_row][last_byte..].to_string();
            self.lines.remove(last_row);
            self.lines[from_row].truncate(first_byte);
            self.lines[from_row].push_str(&remainder);
        } else {
            self.lines[from_row].truncate(first_byte);
        }

        self.cursor_row = from_row;
        self.cursor_col = from_col;
        deleted
    }

    /// Delete entire lines from start_row to end_row inclusive. Returns deleted text.
    pub fn delete_lines(&mut self, start_row: usize, end_row: usize) -> String {
        self.push_undo();
        let end = end_row.min(self.lines.len() - 1);
        let deleted: Vec<String> = self.lines.drain(start_row..=end).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_row = start_row.min(self.lines.len() - 1);
        self.cursor_col = self.first_non_blank(self.cursor_row);
        deleted.join("\n")
    }

    // ── Cursor movement ─────────────────────────────────────────────────

    pub fn move_left(&mut self, count: usize) {
        for _ in 0..count {
            if self.cursor_col > 0 {
                self.cursor_col -= 1;
            }
        }
        self.preferred_col = None;
    }

    pub fn move_right(&mut self, count: usize) {
        let line_len = self.line_len(self.cursor_row);
        for _ in 0..count {
            if self.cursor_col < line_len {
                self.cursor_col += 1;
            }
        }
        self.preferred_col = None;
    }

    pub fn move_up(&mut self, count: usize) {
        if self.preferred_col.is_none() {
            self.preferred_col = Some(self.cursor_col);
        }
        for _ in 0..count {
            if self.cursor_row > 0 {
                self.cursor_row -= 1;
            }
        }
        let line_len = self.line_len(self.cursor_row);
        self.cursor_col = self.preferred_col.unwrap_or(self.cursor_col).min(line_len);
    }

    pub fn move_down(&mut self, count: usize) {
        if self.preferred_col.is_none() {
            self.preferred_col = Some(self.cursor_col);
        }
        for _ in 0..count {
            if self.cursor_row < self.lines.len() - 1 {
                self.cursor_row += 1;
            }
        }
        let line_len = self.line_len(self.cursor_row);
        self.cursor_col = self.preferred_col.unwrap_or(self.cursor_col).min(line_len);
    }

    // ── Word motions ────────────────────────────────────────────────────

    /// Move to the start of the next word.
    pub fn move_word_forward(&mut self) {
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        let len = chars.len();
        let mut col = self.cursor_col;

        if col >= len {
            // Move to next line.
            if self.cursor_row < self.lines.len() - 1 {
                self.cursor_row += 1;
                self.cursor_col = 0;
                // Skip to first non-blank on next line.
                let next_chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
                let mut i = 0;
                while i < next_chars.len() && next_chars[i].is_whitespace() {
                    i += 1;
                }
                self.cursor_col = if i < next_chars.len() { i } else { 0 };
            }
            self.preferred_col = None;
            return;
        }

        // Skip current word.
        if is_word_char(chars[col]) {
            while col < len && is_word_char(chars[col]) {
                col += 1;
            }
        } else if !chars[col].is_whitespace() {
            // Punctuation word.
            while col < len && !is_word_char(chars[col]) && !chars[col].is_whitespace() {
                col += 1;
            }
        }

        // Skip whitespace.
        while col < len && chars[col].is_whitespace() {
            col += 1;
        }

        if col >= len && self.cursor_row < self.lines.len() - 1 {
            self.cursor_row += 1;
            let next_chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
            let mut i = 0;
            while i < next_chars.len() && next_chars[i].is_whitespace() {
                i += 1;
            }
            self.cursor_col = if i < next_chars.len() { i } else { 0 };
        } else {
            self.cursor_col = col.min(len);
        }
        self.preferred_col = None;
    }

    /// Move to the start of the previous word.
    pub fn move_word_backward(&mut self) {
        let mut col = self.cursor_col;

        if col == 0 {
            if self.cursor_row > 0 {
                self.cursor_row -= 1;
                col = self.line_len(self.cursor_row);
            } else {
                return;
            }
        }

        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();

        // Skip whitespace backward.
        while col > 0 && chars[col - 1].is_whitespace() {
            col -= 1;
        }

        if col == 0 {
            self.cursor_col = 0;
            self.preferred_col = None;
            return;
        }

        // Skip word backward.
        if is_word_char(chars[col - 1]) {
            while col > 0 && is_word_char(chars[col - 1]) {
                col -= 1;
            }
        } else {
            while col > 0 && !is_word_char(chars[col - 1]) && !chars[col - 1].is_whitespace() {
                col -= 1;
            }
        }

        self.cursor_col = col;
        self.preferred_col = None;
    }

    /// Move to end of current/next word.
    pub fn move_word_end(&mut self) {
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        let len = chars.len();
        let mut col = self.cursor_col;

        if len == 0 || col >= len.saturating_sub(1) {
            if self.cursor_row < self.lines.len() - 1 {
                self.cursor_row += 1;
                let next_chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
                col = 0;
                // Skip whitespace.
                while col < next_chars.len() && next_chars[col].is_whitespace() {
                    col += 1;
                }
                // Skip to end of word.
                let next_len = next_chars.len();
                if col < next_len {
                    if is_word_char(next_chars[col]) {
                        while col + 1 < next_len && is_word_char(next_chars[col + 1]) {
                            col += 1;
                        }
                    } else {
                        while col + 1 < next_len
                            && !is_word_char(next_chars[col + 1])
                            && !next_chars[col + 1].is_whitespace()
                        {
                            col += 1;
                        }
                    }
                }
                self.cursor_col = col;
            }
            self.preferred_col = None;
            return;
        }

        // Advance past current char.
        col += 1;

        // Skip whitespace.
        while col < len && chars[col].is_whitespace() {
            col += 1;
        }

        if col >= len {
            self.cursor_col = len.saturating_sub(1);
            self.preferred_col = None;
            return;
        }

        // Skip to end of word.
        if is_word_char(chars[col]) {
            while col + 1 < len && is_word_char(chars[col + 1]) {
                col += 1;
            }
        } else {
            while col + 1 < len
                && !is_word_char(chars[col + 1])
                && !chars[col + 1].is_whitespace()
            {
                col += 1;
            }
        }

        self.cursor_col = col;
        self.preferred_col = None;
    }

    /// Big-word forward (WORD: split on whitespace only).
    pub fn move_big_word_forward(&mut self) {
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        let len = chars.len();
        let mut col = self.cursor_col;

        // Skip non-whitespace.
        while col < len && !chars[col].is_whitespace() {
            col += 1;
        }
        // Skip whitespace.
        while col < len && chars[col].is_whitespace() {
            col += 1;
        }

        if col >= len && self.cursor_row < self.lines.len() - 1 {
            self.cursor_row += 1;
            self.cursor_col = 0;
        } else {
            self.cursor_col = col.min(len);
        }
        self.preferred_col = None;
    }

    /// Big-word backward.
    pub fn move_big_word_backward(&mut self) {
        let mut col = self.cursor_col;
        if col == 0 {
            if self.cursor_row > 0 {
                self.cursor_row -= 1;
                col = self.line_len(self.cursor_row);
            } else {
                return;
            }
        }
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        while col > 0 && chars[col - 1].is_whitespace() {
            col -= 1;
        }
        while col > 0 && !chars[col - 1].is_whitespace() {
            col -= 1;
        }
        self.cursor_col = col;
        self.preferred_col = None;
    }

    /// Big-word end.
    pub fn move_big_word_end(&mut self) {
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        let len = chars.len();
        let mut col = self.cursor_col;

        if col + 1 >= len {
            if self.cursor_row < self.lines.len() - 1 {
                self.cursor_row += 1;
                let next = &self.lines[self.cursor_row];
                let nc: Vec<char> = next.chars().collect();
                col = 0;
                while col < nc.len() && nc[col].is_whitespace() {
                    col += 1;
                }
                while col + 1 < nc.len() && !nc[col + 1].is_whitespace() {
                    col += 1;
                }
                self.cursor_col = col;
            }
            self.preferred_col = None;
            return;
        }

        col += 1;
        while col < len && chars[col].is_whitespace() {
            col += 1;
        }
        while col + 1 < len && !chars[col + 1].is_whitespace() {
            col += 1;
        }
        self.cursor_col = col.min(len.saturating_sub(1));
        self.preferred_col = None;
    }

    // ── Find motions ────────────────────────────────────────────────────

    fn find_char_forward(&mut self, target: char, inclusive: bool) {
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        for i in (self.cursor_col + 1)..chars.len() {
            if chars[i] == target {
                self.cursor_col = if inclusive { i } else { i.saturating_sub(1) };
                self.preferred_col = None;
                return;
            }
        }
    }

    fn find_char_reverse(&mut self, target: char, inclusive: bool) {
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        if self.cursor_col == 0 {
            return;
        }
        for i in (0..self.cursor_col).rev() {
            if chars[i] == target {
                self.cursor_col = if inclusive { i } else { i + 1 };
                self.preferred_col = None;
                return;
            }
        }
    }

    // ── Text objects ────────────────────────────────────────────────────

    /// Find the range for a text object. Returns (start_row, start_col, end_row, end_col).
    pub fn find_text_object(&self, obj: &TextObject) -> Option<(usize, usize, usize, usize)> {
        match obj {
            TextObject::InnerWord => self.find_word_object(true),
            TextObject::AWord => self.find_word_object(false),
            TextObject::InnerQuote(q) => self.find_quote_object(*q, true),
            TextObject::AQuote(q) => self.find_quote_object(*q, false),
            TextObject::InnerParen | TextObject::AParen => {
                self.find_pair_object('(', ')', matches!(obj, TextObject::InnerParen))
            }
            TextObject::InnerBrace | TextObject::ABrace => {
                self.find_pair_object('{', '}', matches!(obj, TextObject::InnerBrace))
            }
            TextObject::InnerBracket | TextObject::ABracket => {
                self.find_pair_object('[', ']', matches!(obj, TextObject::InnerBracket))
            }
        }
    }

    fn find_word_object(&self, inner: bool) -> Option<(usize, usize, usize, usize)> {
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        if chars.is_empty() {
            return None;
        }
        let col = self.cursor_col.min(chars.len() - 1);

        let mut start = col;
        let mut end = col;

        if is_word_char(chars[col]) {
            while start > 0 && is_word_char(chars[start - 1]) {
                start -= 1;
            }
            while end + 1 < chars.len() && is_word_char(chars[end + 1]) {
                end += 1;
            }
        } else if !chars[col].is_whitespace() {
            while start > 0 && !is_word_char(chars[start - 1]) && !chars[start - 1].is_whitespace()
            {
                start -= 1;
            }
            while end + 1 < chars.len()
                && !is_word_char(chars[end + 1])
                && !chars[end + 1].is_whitespace()
            {
                end += 1;
            }
        } else {
            while start > 0 && chars[start - 1].is_whitespace() {
                start -= 1;
            }
            while end + 1 < chars.len() && chars[end + 1].is_whitespace() {
                end += 1;
            }
        }

        if !inner {
            // "a word" includes trailing whitespace.
            while end + 1 < chars.len() && chars[end + 1].is_whitespace() {
                end += 1;
            }
        }

        Some((self.cursor_row, start, self.cursor_row, end + 1))
    }

    fn find_quote_object(
        &self,
        quote: char,
        inner: bool,
    ) -> Option<(usize, usize, usize, usize)> {
        let line = &self.lines[self.cursor_row];
        let chars: Vec<char> = line.chars().collect();

        // Find opening quote before or at cursor.
        let mut open = None;
        for i in (0..=self.cursor_col.min(chars.len().saturating_sub(1))).rev() {
            if chars[i] == quote {
                open = Some(i);
                break;
            }
        }

        let open_idx = open?;

        // Find closing quote after open.
        let mut close = None;
        for i in (open_idx + 1)..chars.len() {
            if chars[i] == quote {
                close = Some(i);
                break;
            }
        }

        let close_idx = close?;

        if inner {
            Some((
                self.cursor_row,
                open_idx + 1,
                self.cursor_row,
                close_idx,
            ))
        } else {
            Some((
                self.cursor_row,
                open_idx,
                self.cursor_row,
                close_idx + 1,
            ))
        }
    }

    fn find_pair_object(
        &self,
        open: char,
        close: char,
        inner: bool,
    ) -> Option<(usize, usize, usize, usize)> {
        // Simple single-line implementation for now.
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        let col = self.cursor_col.min(chars.len().saturating_sub(1));

        // Find opening delimiter.
        let mut open_idx = None;
        let mut depth = 0i32;
        for i in (0..=col).rev() {
            if chars[i] == close && i != col {
                depth += 1;
            } else if chars[i] == open {
                if depth == 0 {
                    open_idx = Some(i);
                    break;
                }
                depth -= 1;
            }
        }

        let oi = open_idx?;

        // Find closing delimiter.
        let mut close_idx = None;
        depth = 0;
        for i in (oi + 1)..chars.len() {
            if chars[i] == open {
                depth += 1;
            } else if chars[i] == close {
                if depth == 0 {
                    close_idx = Some(i);
                    break;
                }
                depth -= 1;
            }
        }

        let ci = close_idx?;

        if inner {
            Some((self.cursor_row, oi + 1, self.cursor_row, ci))
        } else {
            Some((self.cursor_row, oi, self.cursor_row, ci + 1))
        }
    }

    // ── Motion dispatch ─────────────────────────────────────────────────

    /// Apply a motion to the cursor.
    pub fn apply_motion(&mut self, motion: &Motion) {
        match motion {
            Motion::Left => self.move_left(1),
            Motion::Right => self.move_right(1),
            Motion::Up => self.move_up(1),
            Motion::Down => self.move_down(1),
            Motion::WordForward => self.move_word_forward(),
            Motion::WordBackward => self.move_word_backward(),
            Motion::WordEnd => self.move_word_end(),
            Motion::BigWordForward => self.move_big_word_forward(),
            Motion::BigWordBackward => self.move_big_word_backward(),
            Motion::BigWordEnd => self.move_big_word_end(),
            Motion::LineStart => {
                self.cursor_col = 0;
                self.preferred_col = None;
            }
            Motion::FirstNonBlank => {
                self.cursor_col = self.first_non_blank(self.cursor_row);
                self.preferred_col = None;
            }
            Motion::LineEnd => {
                let len = self.line_len(self.cursor_row);
                self.cursor_col = len;
                self.preferred_col = None;
            }
            Motion::FindChar(ch) => self.find_char_forward(*ch, true),
            Motion::FindCharReverse(ch) => self.find_char_reverse(*ch, true),
            Motion::TilChar(ch) => self.find_char_forward(*ch, false),
            Motion::TilCharReverse(ch) => self.find_char_reverse(*ch, false),
        }
    }
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Convert a character index to byte offset.
fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

/// Is this character part of a "word" (alphanumeric or underscore)?
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_buffer_has_one_empty_line() {
        let buf = TextBuffer::new();
        assert_eq!(buf.lines, vec![""]);
        assert_eq!(buf.cursor(), (0, 0));
    }

    #[test]
    fn set_text_multiline() {
        let mut buf = TextBuffer::new();
        buf.set_text("hello\nworld");
        assert_eq!(buf.lines, vec!["hello", "world"]);
        assert_eq!(buf.cursor(), (0, 0));
    }

    #[test]
    fn insert_char_basic() {
        let mut buf = TextBuffer::new();
        buf.insert_char('h');
        buf.insert_char('i');
        assert_eq!(buf.text(), "hi");
        assert_eq!(buf.cursor(), (0, 2));
    }

    #[test]
    fn insert_char_unicode() {
        let mut buf = TextBuffer::new();
        buf.insert_char('🎉');
        buf.insert_char('日');
        assert_eq!(buf.text(), "🎉日");
        assert_eq!(buf.cursor(), (0, 2));
        assert_eq!(buf.line_len(0), 2);
    }

    #[test]
    fn backspace_basic() {
        let mut buf = TextBuffer::new();
        buf.set_text("abc");
        buf.set_cursor(0, 3);
        buf.backspace();
        assert_eq!(buf.text(), "ab");
        assert_eq!(buf.cursor(), (0, 2));
    }

    #[test]
    fn backspace_joins_lines() {
        let mut buf = TextBuffer::new();
        buf.set_text("hello\nworld");
        buf.set_cursor(1, 0);
        buf.backspace();
        assert_eq!(buf.text(), "helloworld");
        assert_eq!(buf.cursor(), (0, 5));
    }

    #[test]
    fn delete_forward_basic() {
        let mut buf = TextBuffer::new();
        buf.set_text("abc");
        buf.set_cursor(0, 0);
        buf.delete_forward();
        assert_eq!(buf.text(), "bc");
    }

    #[test]
    fn delete_forward_joins_lines() {
        let mut buf = TextBuffer::new();
        buf.set_text("ab\ncd");
        buf.set_cursor(0, 2);
        buf.delete_forward();
        assert_eq!(buf.text(), "abcd");
    }

    #[test]
    fn insert_newline() {
        let mut buf = TextBuffer::new();
        buf.set_text("abcd");
        buf.set_cursor(0, 2);
        buf.insert_newline();
        assert_eq!(buf.lines, vec!["ab", "cd"]);
        assert_eq!(buf.cursor(), (1, 0));
    }

    #[test]
    fn word_forward_basic() {
        let mut buf = TextBuffer::new();
        buf.set_text("hello world foo");
        buf.set_cursor(0, 0);
        buf.move_word_forward();
        assert_eq!(buf.cursor().1, 6); // "world"
        buf.move_word_forward();
        assert_eq!(buf.cursor().1, 12); // "foo"
    }

    #[test]
    fn word_forward_punctuation() {
        let mut buf = TextBuffer::new();
        buf.set_text("foo.bar");
        buf.set_cursor(0, 0);
        buf.move_word_forward();
        assert_eq!(buf.cursor().1, 3); // "."
    }

    #[test]
    fn word_backward_basic() {
        let mut buf = TextBuffer::new();
        buf.set_text("hello world");
        buf.set_cursor(0, 8);
        buf.move_word_backward();
        assert_eq!(buf.cursor().1, 6);
    }

    #[test]
    fn word_end_basic() {
        let mut buf = TextBuffer::new();
        buf.set_text("hello world");
        buf.set_cursor(0, 0);
        buf.move_word_end();
        assert_eq!(buf.cursor().1, 4); // end of "hello"
    }

    #[test]
    fn move_up_down() {
        let mut buf = TextBuffer::new();
        buf.set_text("abc\ndef\nghi");
        buf.set_cursor(0, 2);
        buf.move_down(1);
        assert_eq!(buf.cursor(), (1, 2));
        buf.move_down(1);
        assert_eq!(buf.cursor(), (2, 2));
        buf.move_up(1);
        assert_eq!(buf.cursor(), (1, 2));
    }

    #[test]
    fn preferred_col_across_short_line() {
        let mut buf = TextBuffer::new();
        buf.set_text("long line\na\nlong line");
        buf.set_cursor(0, 8);
        buf.move_down(1);
        assert_eq!(buf.cursor(), (1, 1)); // short line clamps
        buf.move_down(1);
        assert_eq!(buf.cursor(), (2, 8)); // preferred col restored
    }

    #[test]
    fn delete_range_single_line() {
        let mut buf = TextBuffer::new();
        buf.set_text("hello world");
        let deleted = buf.delete_range(0, 5, 0, 11);
        assert_eq!(deleted, " world");
        assert_eq!(buf.text(), "hello");
    }

    #[test]
    fn delete_range_multi_line() {
        let mut buf = TextBuffer::new();
        buf.set_text("aaa\nbbb\nccc");
        let deleted = buf.delete_range(0, 2, 2, 1);
        assert_eq!(deleted, "a\nbbb\nc");
        assert_eq!(buf.text(), "aacc");
    }

    #[test]
    fn delete_lines() {
        let mut buf = TextBuffer::new();
        buf.set_text("one\ntwo\nthree\nfour");
        let deleted = buf.delete_lines(1, 2);
        assert_eq!(deleted, "two\nthree");
        assert_eq!(buf.lines, vec!["one", "four"]);
    }

    #[test]
    fn undo_restores_state() {
        let mut buf = TextBuffer::new();
        buf.set_text("hello");
        buf.set_cursor(0, 5);
        buf.delete_range(0, 0, 0, 5);
        assert_eq!(buf.text(), "");
        buf.undo();
        assert_eq!(buf.text(), "hello");
    }

    #[test]
    fn find_char_forward_test() {
        let mut buf = TextBuffer::new();
        buf.set_text("abcdefg");
        buf.set_cursor(0, 0);
        buf.find_char_forward('d', true);
        assert_eq!(buf.cursor().1, 3);
    }

    #[test]
    fn find_char_til() {
        let mut buf = TextBuffer::new();
        buf.set_text("abcdefg");
        buf.set_cursor(0, 0);
        buf.find_char_forward('d', false);
        assert_eq!(buf.cursor().1, 2);
    }

    #[test]
    fn find_word_object_inner() {
        let buf = TextBuffer {
            lines: vec!["hello world".to_string()],
            cursor_row: 0,
            cursor_col: 7,
            preferred_col: None,
            undo_stack: Vec::new(),
        };
        let obj = buf.find_text_object(&TextObject::InnerWord);
        assert_eq!(obj, Some((0, 6, 0, 11)));
    }

    #[test]
    fn find_quote_object() {
        let buf = TextBuffer {
            lines: vec!["say \"hello\" end".to_string()],
            cursor_row: 0,
            cursor_col: 6,
            preferred_col: None,
            undo_stack: Vec::new(),
        };
        let obj = buf.find_text_object(&TextObject::InnerQuote('"'));
        assert_eq!(obj, Some((0, 5, 0, 10)));
    }

    #[test]
    fn find_paren_object() {
        let buf = TextBuffer {
            lines: vec!["fn(a, b)".to_string()],
            cursor_row: 0,
            cursor_col: 4,
            preferred_col: None,
            undo_stack: Vec::new(),
        };
        let obj = buf.find_text_object(&TextObject::InnerParen);
        assert_eq!(obj, Some((0, 3, 0, 7)));
    }

    #[test]
    fn open_line_below() {
        let mut buf = TextBuffer::new();
        buf.set_text("line1\nline2");
        buf.set_cursor(0, 3);
        buf.open_line_below();
        assert_eq!(buf.lines, vec!["line1", "", "line2"]);
        assert_eq!(buf.cursor(), (1, 0));
    }

    #[test]
    fn open_line_above() {
        let mut buf = TextBuffer::new();
        buf.set_text("line1\nline2");
        buf.set_cursor(1, 0);
        buf.open_line_above();
        assert_eq!(buf.lines, vec!["line1", "", "line2"]);
        assert_eq!(buf.cursor(), (1, 0));
    }

    #[test]
    fn replace_char_test() {
        let mut buf = TextBuffer::new();
        buf.set_text("abc");
        buf.set_cursor(0, 1);
        buf.replace_char('X');
        assert_eq!(buf.text(), "aXc");
    }

    #[test]
    fn join_lines_test() {
        let mut buf = TextBuffer::new();
        buf.set_text("hello\n  world");
        buf.set_cursor(0, 0);
        buf.join_lines();
        assert_eq!(buf.text(), "hello world");
    }

    #[test]
    fn toggle_case_test() {
        let mut buf = TextBuffer::new();
        buf.set_text("Hello");
        buf.set_cursor(0, 0);
        buf.toggle_case();
        assert_eq!(buf.text(), "hello");
        assert_eq!(buf.cursor().1, 1);
    }

    #[test]
    fn indent_right_left() {
        let mut buf = TextBuffer::new();
        buf.set_text("hello");
        buf.set_cursor(0, 0);
        buf.indent_right();
        assert_eq!(buf.text(), "  hello");
        buf.indent_left();
        assert_eq!(buf.text(), "hello");
    }

    #[test]
    fn first_non_blank_test() {
        let mut buf = TextBuffer::new();
        buf.set_text("   hello");
        assert_eq!(buf.first_non_blank(0), 3);
    }

    #[test]
    fn line_end_on_empty_line() {
        let mut buf = TextBuffer::new();
        buf.set_text("");
        buf.apply_motion(&Motion::LineEnd);
        assert_eq!(buf.cursor(), (0, 0));
    }

    #[test]
    fn word_forward_crosses_line() {
        let mut buf = TextBuffer::new();
        buf.set_text("end\nstart");
        buf.set_cursor(0, 0);
        buf.move_word_forward(); // to end of "end" / start of next
        // Should end on row 0 at 3, or on row 1
        // "end" → past end → next line
        buf.move_word_forward();
        assert_eq!(buf.cursor().0, 1);
    }

    #[test]
    fn delete_lines_leaves_empty_buffer() {
        let mut buf = TextBuffer::new();
        buf.set_text("only");
        let deleted = buf.delete_lines(0, 0);
        assert_eq!(deleted, "only");
        assert_eq!(buf.lines, vec![""]);
    }
}
