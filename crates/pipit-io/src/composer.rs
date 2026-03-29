//! Composer — a rich input widget for pipit's TUI.
//!
//! Replaces the bare `input_buffer: String` + `cursor_pos: usize` with:
//!   - Tab-completion for slash commands, @file paths, and !shell history
//!   - Input history recall with ↑/↓ (when the buffer is empty)
//!   - Multiline editing (Ctrl-J inserts newline; Enter submits)
//!   - Attachment chips for @file and image mentions
//!   - Ghost-text preview for completions
//!   - Word-level navigation (Ctrl-Left/Right, Ctrl-Backspace)
//!
//! Layout (3–6 lines depending on content):
//!   ┌──────────────────────────────────────────────────────┐
//!   │ 📎 src/main.rs  📎 lib.rs  🖼 screenshot.png         │  ← attachment chips
//!   │ you› fix the panic on line 42 _                      │  ← input line(s)
//!   │      where the unwrap fails on None                  │  ← continuation
//!   │  ┌─────────────────────────────────┐                 │
//!   │  │ /plan    Plan before editing     │                 │  ← completion popup
//!   │  │ /plans   Show proof-packet plans │                 │
//!   │  └─────────────────────────────────┘                 │
//!   │ /help · @file · !shell · Ctrl-J newline · Esc cancel │  ← hint bar
//!   └──────────────────────────────────────────────────────┘

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthStr;

// ═══════════════════════════════════════════════════════════════════════════
//  Attachment model
// ═══════════════════════════════════════════════════════════════════════════

/// A file or image attached to the current input via `@path` or drag-and-drop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub path: String,
    pub kind: AttachmentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    File,
    Image,
}

impl Attachment {
    pub fn from_path(path: &str) -> Self {
        let lower = path.to_lowercase();
        let kind = if IMAGE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
            AttachmentKind::Image
        } else {
            AttachmentKind::File
        };
        Self {
            path: path.to_string(),
            kind,
        }
    }

    pub fn chip_icon(&self) -> &str {
        match self.kind {
            AttachmentKind::File => "📎",
            AttachmentKind::Image => "🖼",
        }
    }

    /// Short display name: just the filename, not the full path.
    pub fn display_name(&self) -> &str {
        Path::new(&self.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&self.path)
    }
}

const IMAGE_EXTENSIONS: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".svg"];

// ═══════════════════════════════════════════════════════════════════════════
//  Completion engine
// ═══════════════════════════════════════════════════════════════════════════

/// A single completion candidate.
#[derive(Debug, Clone)]
pub struct CompletionItem {
    pub insert_text: String,
    pub description: String,
    pub kind: CompletionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    SlashCommand,
    FilePath,
    ShellHistory,
}

impl CompletionItem {
    pub fn icon(&self) -> &str {
        match self.kind {
            CompletionKind::SlashCommand => "/",
            CompletionKind::FilePath => "@",
            CompletionKind::ShellHistory => "!",
        }
    }
}

/// Tracks the state of the completion popup.
#[derive(Debug, Default)]
pub struct CompletionState {
    pub candidates: Vec<CompletionItem>,
    pub selected: usize,
    pub active: bool,
    pub trigger_prefix: String,
    pub trigger_start: usize,
}

impl CompletionState {
    pub fn clear(&mut self) {
        self.candidates.clear();
        self.selected = 0;
        self.active = false;
        self.trigger_prefix.clear();
        self.trigger_start = 0;
    }

    pub fn select_next(&mut self) {
        if !self.candidates.is_empty() {
            self.selected = (self.selected + 1) % self.candidates.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.candidates.is_empty() {
            self.selected = if self.selected == 0 {
                self.candidates.len() - 1
            } else {
                self.selected - 1
            };
        }
    }

    pub fn current(&self) -> Option<&CompletionItem> {
        if self.active {
            self.candidates.get(self.selected)
        } else {
            None
        }
    }

    /// Ghost text to show inline (the part after the trigger prefix).
    pub fn ghost_text(&self) -> Option<&str> {
        self.current().map(|item| {
            let prefix_len = self.trigger_prefix.len();
            if item.insert_text.len() > prefix_len {
                &item.insert_text[prefix_len..]
            } else {
                ""
            }
        })
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Composer state
// ═══════════════════════════════════════════════════════════════════════════

/// The rich input composer that replaces TuiState's bare input_buffer.
#[derive(Debug)]
pub struct Composer {
    /// Lines of text in the editor. Single-line mode = 1 element.
    pub lines: Vec<String>,
    /// Row of the cursor (0-indexed, into `lines`).
    pub cursor_row: usize,
    /// Column of the cursor (character offset within the current line).
    pub cursor_col: usize,

    /// Files and images attached to this input.
    pub attachments: Vec<Attachment>,

    pub completion: CompletionState,

    history: VecDeque<String>,
    history_cursor: Option<usize>,
    stashed_input: Option<String>,
    max_history: usize,

    project_root: PathBuf,
    slash_commands: Vec<(String, String)>,
    shell_history: Vec<String>,

    /// When set, the composer has submitted and the consumer should drain this.
    pub submitted: Option<SubmittedInput>,
}

/// What the composer produces on Enter.
#[derive(Debug, Clone)]
pub struct SubmittedInput {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

impl Composer {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            lines: vec![String::new()],
            cursor_row: 0,
            cursor_col: 0,
            attachments: Vec::new(),
            completion: CompletionState::default(),
            history: VecDeque::new(),
            history_cursor: None,
            stashed_input: None,
            max_history: 500,
            project_root,
            slash_commands: default_slash_commands(),
            shell_history: Vec::new(),
            submitted: None,
        }
    }

    // ── Public API ──────────────────────────────────────────────────────

    /// The full text content as a single string.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// Whether the buffer is completely empty (no text, no attachments).
    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.is_empty()) && self.attachments.is_empty()
    }

    /// Is the editor in multiline mode?
    pub fn is_multiline(&self) -> bool {
        self.lines.len() > 1
    }

    /// Clear the editor and attachments.
    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.attachments.clear();
        self.completion.clear();
        self.history_cursor = None;
        self.stashed_input = None;
    }

    /// Add a file attachment (from @mention or drag-drop).
    pub fn add_attachment(&mut self, path: &str) {
        let att = Attachment::from_path(path);
        if !self.attachments.contains(&att) {
            self.attachments.push(att);
        }
    }

    /// Push a command to shell history (for !-completion).
    pub fn push_shell_history(&mut self, cmd: &str) {
        self.shell_history.push(cmd.to_string());
        if self.shell_history.len() > 100 {
            self.shell_history.drain(..50);
        }
    }

    fn push_to_history(&mut self) {
        let text = self.text();
        if !text.trim().is_empty() {
            if self.history.back().map(|h| h.as_str()) != Some(text.as_str()) {
                self.history.push_back(text);
                if self.history.len() > self.max_history {
                    self.history.pop_front();
                }
            }
        }
    }

    // ── Key handling ────────────────────────────────────────────────────

    /// Process a key event. Returns true if consumed.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.kind == KeyEventKind::Release {
            return false;
        }

        // Completion popup active: intercept Tab, Up/Down, Esc, Enter
        if self.completion.active {
            match key.code {
                KeyCode::Tab => { self.accept_completion(); return true; }
                KeyCode::Enter => { self.accept_completion(); return true; }
                KeyCode::Down => { self.completion.select_next(); return true; }
                KeyCode::Up => { self.completion.select_prev(); return true; }
                KeyCode::Esc => { self.completion.clear(); return true; }
                _ => {} // fall through
            }
        }

        match key.code {
            // Submit
            KeyCode::Enter => {
                if self.lines.iter().all(|l| l.is_empty()) {
                    return true;
                }
                self.extract_inline_attachments();
                self.push_to_history();
                self.submitted = Some(SubmittedInput {
                    text: self.text(),
                    attachments: self.attachments.clone(),
                });
                self.clear();
                return true;
            }

            // Newline (multiline mode)
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_newline();
                return true;
            }

            // Let parent handle Ctrl-C/D
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => false,
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => false,

            // Word navigation
            KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_word_left();
                true
            }
            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.move_word_right();
                true
            }

            // Word delete
            KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.delete_word_left();
                self.trigger_completion();
                true
            }

            // Line kill
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let line = &mut self.lines[self.cursor_row];
                let byte_pos = char_to_byte(line, self.cursor_col);
                line.drain(..byte_pos);
                self.cursor_col = 0;
                self.completion.clear();
                true
            }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let line = &mut self.lines[self.cursor_row];
                let byte_pos = char_to_byte(line, self.cursor_col);
                line.truncate(byte_pos);
                self.completion.clear();
                true
            }

            // History navigation (only from single-line mode)
            KeyCode::Up if self.cursor_row == 0 && self.lines.len() == 1 => {
                self.history_prev();
                true
            }
            KeyCode::Down if self.cursor_row == 0 && self.history_cursor.is_some() => {
                self.history_next();
                true
            }

            // Cursor movement (multiline)
            KeyCode::Up if self.cursor_row > 0 => {
                self.cursor_row -= 1;
                let line_len = self.lines[self.cursor_row].chars().count();
                self.cursor_col = self.cursor_col.min(line_len);
                true
            }
            KeyCode::Down if self.cursor_row < self.lines.len() - 1 => {
                self.cursor_row += 1;
                let line_len = self.lines[self.cursor_row].chars().count();
                self.cursor_col = self.cursor_col.min(line_len);
                true
            }

            // Tab: trigger or cycle completion
            KeyCode::Tab => {
                if self.completion.active {
                    self.completion.select_next();
                } else {
                    self.trigger_completion();
                }
                true
            }
            KeyCode::BackTab => {
                if self.completion.active {
                    self.completion.select_prev();
                }
                true
            }

            // Character input
            KeyCode::Char(c) => {
                self.insert_char(c);
                self.trigger_completion();
                true
            }

            KeyCode::Backspace => {
                self.backspace();
                self.trigger_completion();
                true
            }

            KeyCode::Delete => {
                self.delete_forward();
                self.trigger_completion();
                true
            }

            // Navigation
            KeyCode::Left => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                } else if self.cursor_row > 0 {
                    self.cursor_row -= 1;
                    self.cursor_col = self.lines[self.cursor_row].chars().count();
                }
                self.completion.clear();
                true
            }
            KeyCode::Right => {
                let line_len = self.lines[self.cursor_row].chars().count();
                if self.cursor_col < line_len {
                    self.cursor_col += 1;
                } else if self.cursor_row < self.lines.len() - 1 {
                    self.cursor_row += 1;
                    self.cursor_col = 0;
                }
                self.completion.clear();
                true
            }
            KeyCode::Home => { self.cursor_col = 0; self.completion.clear(); true }
            KeyCode::End => {
                self.cursor_col = self.lines[self.cursor_row].chars().count();
                self.completion.clear();
                true
            }

            KeyCode::Esc => {
                self.completion.clear();
                false // let parent handle Esc for cancel
            }

            _ => false,
        }
    }

    /// Handle a bracketed paste event.
    pub fn handle_paste(&mut self, text: &str) {
        let paste_lines: Vec<&str> = text.lines().collect();
        if paste_lines.is_empty() {
            return;
        }

        if paste_lines.len() == 1 {
            let line = &mut self.lines[self.cursor_row];
            let byte_pos = char_to_byte(line, self.cursor_col);
            line.insert_str(byte_pos, paste_lines[0]);
            self.cursor_col += paste_lines[0].chars().count();
        } else {
            let current_line = &self.lines[self.cursor_row];
            let byte_pos = char_to_byte(current_line, self.cursor_col);
            let after_cursor = current_line[byte_pos..].to_string();
            let before_cursor = current_line[..byte_pos].to_string();

            self.lines[self.cursor_row] = format!("{}{}", before_cursor, paste_lines[0]);

            for (i, &pasted_line) in paste_lines[1..paste_lines.len() - 1].iter().enumerate() {
                self.lines
                    .insert(self.cursor_row + 1 + i, pasted_line.to_string());
            }

            let last_idx = paste_lines.len() - 1;
            let last_line = format!("{}{}", paste_lines[last_idx], after_cursor);
            if last_idx > 0 {
                self.lines.insert(self.cursor_row + last_idx, last_line);
            } else {
                self.lines[self.cursor_row].push_str(&after_cursor);
            }

            self.cursor_row += last_idx;
            self.cursor_col = paste_lines[last_idx].chars().count();
        }
    }

    // ── Private editing operations ──────────────────────────────────────

    fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.cursor_row];
        let byte_pos = char_to_byte(line, self.cursor_col);
        line.insert(byte_pos, c);
        self.cursor_col += 1;
        self.history_cursor = None;
    }

    fn insert_newline(&mut self) {
        let line = &self.lines[self.cursor_row];
        let byte_pos = char_to_byte(line, self.cursor_col);
        let after = line[byte_pos..].to_string();
        self.lines[self.cursor_row].truncate(byte_pos);
        self.cursor_row += 1;
        self.lines.insert(self.cursor_row, after);
        self.cursor_col = 0;
        self.completion.clear();
    }

    fn backspace(&mut self) {
        if self.cursor_col > 0 {
            let line = &mut self.lines[self.cursor_row];
            let new_col = self.cursor_col - 1;
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
    }

    fn delete_forward(&mut self) {
        let line_len = self.lines[self.cursor_row].chars().count();
        if self.cursor_col < line_len {
            let line = &mut self.lines[self.cursor_row];
            let byte_start = char_to_byte(line, self.cursor_col);
            let byte_end = char_to_byte(line, self.cursor_col + 1);
            line.drain(byte_start..byte_end);
        } else if self.cursor_row < self.lines.len() - 1 {
            let next = self.lines.remove(self.cursor_row + 1);
            self.lines[self.cursor_row].push_str(&next);
        }
    }

    fn move_word_left(&mut self) {
        let line = &self.lines[self.cursor_row];
        let chars: Vec<char> = line.chars().collect();
        if self.cursor_col == 0 { return; }
        let mut i = self.cursor_col - 1;
        while i > 0 && chars[i].is_whitespace() { i -= 1; }
        while i > 0 && !chars[i - 1].is_whitespace() { i -= 1; }
        self.cursor_col = i;
        self.completion.clear();
    }

    fn move_word_right(&mut self) {
        let line = &self.lines[self.cursor_row];
        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();
        if self.cursor_col >= len { return; }
        let mut i = self.cursor_col;
        while i < len && !chars[i].is_whitespace() { i += 1; }
        while i < len && chars[i].is_whitespace() { i += 1; }
        self.cursor_col = i;
        self.completion.clear();
    }

    fn delete_word_left(&mut self) {
        let chars: Vec<char> = self.lines[self.cursor_row].chars().collect();
        if self.cursor_col == 0 { return; }
        let end = self.cursor_col;
        let mut i = end - 1;
        while i > 0 && chars[i].is_whitespace() { i -= 1; }
        while i > 0 && !chars[i - 1].is_whitespace() { i -= 1; }
        let start_byte = char_to_byte(&self.lines[self.cursor_row], i);
        let end_byte = char_to_byte(&self.lines[self.cursor_row], end);
        self.lines[self.cursor_row].drain(start_byte..end_byte);
        self.cursor_col = i;
    }

    // ── History ─────────────────────────────────────────────────────────

    fn history_prev(&mut self) {
        if self.history.is_empty() { return; }
        if self.history_cursor.is_none() {
            self.stashed_input = Some(self.text());
            self.history_cursor = Some(self.history.len() - 1);
        } else if let Some(idx) = self.history_cursor {
            if idx > 0 { self.history_cursor = Some(idx - 1); } else { return; }
        }
        if let Some(idx) = self.history_cursor {
            if let Some(entry) = self.history.get(idx).cloned() {
                self.set_text(&entry);
            }
        }
    }

    fn history_next(&mut self) {
        if let Some(idx) = self.history_cursor {
            if idx + 1 < self.history.len() {
                self.history_cursor = Some(idx + 1);
                if let Some(entry) = self.history.get(idx + 1).cloned() {
                    self.set_text(&entry);
                }
            } else {
                self.history_cursor = None;
                if let Some(stashed) = self.stashed_input.take() {
                    self.set_text(&stashed);
                } else {
                    self.set_text("");
                }
            }
        }
    }

    fn set_text(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.lines().map(|l| l.to_string()).collect()
        };
        if self.lines.is_empty() { self.lines.push(String::new()); }
        self.cursor_row = self.lines.len() - 1;
        self.cursor_col = self.lines[self.cursor_row].chars().count();
    }

    // ── Completion engine ───────────────────────────────────────────────

    fn trigger_completion(&mut self) {
        let line = &self.lines[self.cursor_row];
        let byte_pos = char_to_byte(line, self.cursor_col);
        let before_cursor = &line[..byte_pos];

        let token_start = before_cursor
            .rfind(|c: char| c.is_whitespace())
            .map(|i| i + 1)
            .unwrap_or(0);
        let token = &before_cursor[token_start..];

        if token.is_empty() {
            self.completion.clear();
            return;
        }

        let mut candidates = Vec::new();

        // Slash command completion
        if token.starts_with('/') && self.cursor_row == 0 && token_start == 0 {
            let prefix = &token[1..].to_lowercase();
            for (name, desc) in &self.slash_commands {
                if name.starts_with(prefix) {
                    candidates.push(CompletionItem {
                        insert_text: format!("/{}", name),
                        description: desc.clone(),
                        kind: CompletionKind::SlashCommand,
                    });
                }
            }
        }

        // Git branch completion for /switch and /branch arguments
        if candidates.is_empty() && self.cursor_row == 0 {
            let full_line = &self.lines[0];
            let needs_branch = full_line.starts_with("/switch ") || full_line.starts_with("/branch ");
            if needs_branch {
                if let Ok(output) = std::process::Command::new("git")
                    .args(["branch", "--no-color", "-a"])
                    .current_dir(&self.project_root)
                    .output()
                {
                    let branches = String::from_utf8_lossy(&output.stdout);
                    let branch_prefix = token.to_lowercase();
                    for line in branches.lines() {
                        let branch = line.trim().trim_start_matches("* ");
                        if branch.to_lowercase().starts_with(&branch_prefix) {
                            candidates.push(CompletionItem {
                                insert_text: branch.to_string(),
                                description: "branch".to_string(),
                                kind: CompletionKind::FilePath,
                            });
                        }
                    }
                }
            }
        }

        // @file completion
        if token.starts_with('@') {
            let path_prefix = &token[1..];
            if let Ok(entries) = self.list_files_with_prefix(path_prefix) {
                for entry in entries.into_iter().take(12) {
                    let display = entry
                        .strip_prefix(&self.project_root)
                        .unwrap_or(&entry)
                        .to_string_lossy()
                        .to_string();
                    candidates.push(CompletionItem {
                        insert_text: format!("@{}", display),
                        description: if entry.is_dir() {
                            "directory".to_string()
                        } else {
                            format_file_size(&entry)
                        },
                        kind: CompletionKind::FilePath,
                    });
                }
            }
        }

        // !shell completion
        if token.starts_with('!') && self.cursor_row == 0 && token_start == 0 {
            let prefix = &token[1..].to_lowercase();
            for cmd in self.shell_history.iter().rev() {
                if cmd.to_lowercase().starts_with(prefix) {
                    if !candidates.iter().any(|c| c.insert_text == format!("!{}", cmd)) {
                        candidates.push(CompletionItem {
                            insert_text: format!("!{}", cmd),
                            description: "history".to_string(),
                            kind: CompletionKind::ShellHistory,
                        });
                    }
                }
                if candidates.len() >= 8 { break; }
            }
        }

        if candidates.is_empty() {
            self.completion.clear();
        } else {
            self.completion.candidates = candidates;
            self.completion.selected = 0;
            self.completion.active = true;
            self.completion.trigger_prefix = token.to_string();
            self.completion.trigger_start = token_start;
        }
    }

    fn accept_completion(&mut self) {
        let Some(item) = self.completion.current().cloned() else {
            self.completion.clear();
            return;
        };

        let trigger_start = self.completion.trigger_start;
        let byte_start = char_to_byte(&self.lines[self.cursor_row], trigger_start);
        let byte_end = char_to_byte(&self.lines[self.cursor_row], self.cursor_col);

        self.lines[self.cursor_row].replace_range(byte_start..byte_end, &item.insert_text);
        self.cursor_col = trigger_start + item.insert_text.chars().count();

        // Trailing space
        let new_byte_pos = char_to_byte(&self.lines[self.cursor_row], self.cursor_col);
        if self.lines[self.cursor_row]
            .get(new_byte_pos..new_byte_pos + 1)
            .map(|s| s != " ")
            .unwrap_or(true)
        {
            self.lines[self.cursor_row].insert(new_byte_pos, ' ');
            self.cursor_col += 1;
        }

        self.completion.clear();
    }

    fn list_files_with_prefix(&self, prefix: &str) -> std::io::Result<Vec<PathBuf>> {
        let search_path = self.project_root.join(prefix);
        let (dir, file_prefix) = if search_path.is_dir() {
            (search_path, String::new())
        } else {
            let parent = search_path.parent().unwrap_or(&self.project_root);
            let stem = search_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            (parent.to_path_buf(), stem)
        };

        let mut entries = Vec::new();
        if let Ok(read_dir) = std::fs::read_dir(&dir) {
            for entry in read_dir.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name == "node_modules" || name == "target" {
                    continue;
                }
                if file_prefix.is_empty() || name.starts_with(&file_prefix) {
                    entries.push(entry.path());
                }
            }
        }
        entries.sort();
        Ok(entries)
    }

    fn extract_inline_attachments(&mut self) {
        let text = self.text();
        for token in text.split_whitespace() {
            if let Some(path) = token.strip_prefix('@') {
                if !path.is_empty() {
                    self.add_attachment(path);
                }
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Drawing
// ═══════════════════════════════════════════════════════════════════════════

/// Draw the composer widget into the given area.
pub fn draw_composer(frame: &mut Frame, area: Rect, composer: &Composer, is_working: bool) {
    let mut y = area.y;

    // Attachment chips
    if !composer.attachments.is_empty() {
        let mut spans = Vec::new();
        spans.push(Span::styled(" ", Style::default()));
        for (i, att) in composer.attachments.iter().enumerate() {
            if i > 0 { spans.push(Span::styled("  ", Style::default())); }
            let chip_style = match att.kind {
                AttachmentKind::File => Style::default().fg(Color::Cyan),
                AttachmentKind::Image => Style::default().fg(Color::Magenta),
            };
            spans.push(Span::styled(
                format!("{} {}", att.chip_icon(), att.display_name()),
                chip_style,
            ));
            spans.push(Span::styled(" ×", Style::default().fg(Color::DarkGray)));
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
    }

    // Input line(s)
    let prompt = "you› ";
    let prompt_width = UnicodeWidthStr::width(prompt);

    for (row_idx, line) in composer.lines.iter().enumerate() {
        let prefix = if row_idx == 0 {
            Span::styled(prompt, Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))
        } else {
            Span::styled("     ", Style::default())
        };

        let mut spans = vec![prefix];
        spans.push(Span::raw(line.as_str()));

        // Ghost text on the cursor line
        if row_idx == composer.cursor_row {
            if let Some(ghost) = composer.completion.ghost_text() {
                if !ghost.is_empty() {
                    spans.push(Span::styled(ghost, Style::default().fg(Color::DarkGray)));
                }
            }
        }

        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(area.x, y, area.width, 1),
        );

        if row_idx == composer.cursor_row {
            let display_col = char_to_display_col(line, composer.cursor_col);
            let cursor_x = area.x + prompt_width as u16 + display_col as u16;
            frame.set_cursor_position((cursor_x.min(area.x + area.width - 1), y));
        }

        y += 1;
        if y >= area.y + area.height - 1 { break; }
    }

    // Hint bar
    let hint_y = area.y + area.height - 1;
    let hint_text = if is_working {
        " Esc stop · /help · Ctrl-C quit"
    } else if composer.is_multiline() {
        " Enter submit · Ctrl-J newline · /help · Esc cancel"
    } else {
        " /help · @file · !shell · Ctrl-J newline · Esc cancel · Ctrl-C quit"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(hint_text, Style::default().fg(Color::DarkGray)))),
        Rect::new(area.x, hint_y, area.width, 1),
    );
}

/// Draw the completion popup as an overlay above the input area.
pub fn draw_completion_popup(frame: &mut Frame, composer_area: Rect, composer: &Composer) {
    let completion = &composer.completion;
    if !completion.active || completion.candidates.is_empty() { return; }

    let max_visible = 6.min(completion.candidates.len());
    let popup_height = max_visible as u16 + 2;

    let max_width = completion
        .candidates
        .iter()
        .take(max_visible)
        .map(|c| c.insert_text.len() + c.description.len() + 6)
        .max()
        .unwrap_or(20)
        .min(composer_area.width as usize - 4) as u16;

    let prompt_offset = UnicodeWidthStr::width("you› ") as u16;
    let trigger_display_col = char_to_display_col(
        &composer.lines[composer.cursor_row],
        completion.trigger_start,
    ) as u16;
    let popup_x = (composer_area.x + prompt_offset + trigger_display_col)
        .min(composer_area.x + composer_area.width - max_width - 2);
    let popup_y = composer_area.y.saturating_sub(popup_height);

    let popup_rect = Rect::new(popup_x, popup_y, max_width + 2, popup_height);

    frame.render_widget(Clear, popup_rect);

    let mut lines = Vec::new();
    for (i, candidate) in completion.candidates.iter().take(max_visible).enumerate() {
        let is_selected = i == completion.selected;
        let style = if is_selected {
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let desc_style = if is_selected {
            Style::default().fg(Color::DarkGray).bg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        lines.push(Line::from(vec![
            Span::styled(format!(" {} ", candidate.icon()), desc_style),
            Span::styled(&candidate.insert_text, style),
            Span::styled(format!("  {}", candidate.description), desc_style),
        ]));
    }

    let popup_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    frame.render_widget(Paragraph::new(lines).block(popup_block), popup_rect);
}

/// Calculate how many rows the composer needs.
pub fn composer_height(composer: &Composer) -> u16 {
    let attachment_row = if composer.attachments.is_empty() { 0 } else { 1 };
    let input_rows = composer.lines.len().min(4) as u16;
    let hint_row = 1;
    attachment_row + input_rows + hint_row
}

// ═══════════════════════════════════════════════════════════════════════════
//  Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Convert a character index to its display column width.
/// This accounts for CJK double-width, emoji, and multi-byte characters.
fn char_to_display_col(s: &str, char_idx: usize) -> usize {
    let byte_offset = char_to_byte(s, char_idx);
    UnicodeWidthStr::width(&s[..byte_offset])
}

fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

fn format_file_size(path: &Path) -> String {
    match std::fs::metadata(path) {
        Ok(m) => {
            let size = m.len();
            if size < 1024 { format!("{}B", size) }
            else if size < 1024 * 1024 { format!("{:.1}KB", size as f64 / 1024.0) }
            else { format!("{:.1}MB", size as f64 / (1024.0 * 1024.0)) }
        }
        Err(_) => String::new(),
    }
}

fn default_slash_commands() -> Vec<(String, String)> {
    vec![
        ("help".into(), "Show available commands".into()),
        ("status".into(), "Show repo, model, tokens, cost".into()),
        ("plans".into(), "Show proof-packet plan history".into()),
        ("clear".into(), "Reset context and chat history".into()),
        ("quit".into(), "Exit pipit".into()),
        ("cost".into(), "Show token cost summary".into()),
        ("tokens".into(), "Token usage breakdown".into()),
        ("compact".into(), "Compress context to free tokens".into()),
        ("context".into(), "Show files in working set".into()),
        ("add".into(), "Add file to working set".into()),
        ("drop".into(), "Remove file from working set".into()),
        ("plan".into(), "Enter plan-first mode".into()),
        ("verify".into(), "Run build/lint/test checks".into()),
        ("aside".into(), "Quick side question".into()),
        ("permissions".into(), "Show or switch approval mode".into()),
        ("save".into(), "Save current session".into()),
        ("resume".into(), "Resume a saved session".into()),
        ("checkpoint".into(), "Create git checkpoint".into()),
        ("tdd".into(), "Test-driven development workflow".into()),
        ("code-review".into(), "Review uncommitted changes".into()),
        ("build-fix".into(), "Fix build errors incrementally".into()),
        ("model".into(), "Switch model".into()),
        ("diff".into(), "Show uncommitted changes".into()),
        ("commit".into(), "AI-authored commit".into()),
        ("search".into(), "Search codebase".into()),
        ("branch".into(), "Create or show branch".into()),
        ("branches".into(), "List all branches".into()),
        ("switch".into(), "Switch branch".into()),
        ("undo".into(), "Undo last agent edits".into()),
        ("memory".into(), "Persistent knowledge store".into()),
        ("loop".into(), "Continuous polling mode".into()),
        ("doctor".into(), "System health check".into()),
        ("config".into(), "Show configuration".into()),
        ("spec".into(), "Spec-driven development".into()),
        ("skills".into(), "List available skills".into()),
        ("hooks".into(), "List active hooks".into()),
        ("mcp".into(), "MCP server status".into()),
        ("bench".into(), "Benchmark runner".into()),
        ("browse".into(), "Headless browser testing".into()),
        ("mesh".into(), "Distributed mesh management".into()),
        ("watch".into(), "Ambient file watcher".into()),
        ("deps".into(), "Dependency health scan".into()),
    ]
}

// ═══════════════════════════════════════════════════════════════════════════
//  Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventState;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent { code, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn key_ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent { code, modifiers: KeyModifiers::CONTROL, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn make_composer() -> Composer {
        Composer::new(PathBuf::from("/tmp/test-project"))
    }

    #[test]
    fn basic_text_input() {
        let mut c = make_composer();
        c.handle_key(key(KeyCode::Char('h')));
        c.handle_key(key(KeyCode::Char('i')));
        assert_eq!(c.text(), "hi");
        assert_eq!(c.cursor_col, 2);
    }

    #[test]
    fn backspace_deletes_character() {
        let mut c = make_composer();
        c.handle_key(key(KeyCode::Char('a')));
        c.handle_key(key(KeyCode::Char('b')));
        c.handle_key(key(KeyCode::Backspace));
        assert_eq!(c.text(), "a");
        assert_eq!(c.cursor_col, 1);
    }

    #[test]
    fn ctrl_j_inserts_newline() {
        let mut c = make_composer();
        c.handle_key(key(KeyCode::Char('a')));
        c.handle_key(key_ctrl(KeyCode::Char('j')));
        c.handle_key(key(KeyCode::Char('b')));
        assert_eq!(c.lines, vec!["a", "b"]);
        assert_eq!(c.cursor_row, 1);
        assert_eq!(c.cursor_col, 1);
    }

    #[test]
    fn enter_submits_input() {
        let mut c = make_composer();
        c.handle_key(key(KeyCode::Char('x')));
        c.handle_key(key(KeyCode::Enter));
        assert!(c.submitted.is_some());
        assert_eq!(c.submitted.as_ref().unwrap().text, "x");
        assert!(c.is_empty());
    }

    #[test]
    fn empty_enter_does_not_submit() {
        let mut c = make_composer();
        c.handle_key(key(KeyCode::Enter));
        assert!(c.submitted.is_none());
    }

    #[test]
    fn history_recall() {
        let mut c = make_composer();
        c.handle_key(key(KeyCode::Char('a')));
        c.handle_key(key(KeyCode::Enter));
        c.submitted.take();
        c.handle_key(key(KeyCode::Char('b')));
        c.handle_key(key(KeyCode::Enter));
        c.submitted.take();
        c.handle_key(key(KeyCode::Up));
        assert_eq!(c.text(), "b");
        c.handle_key(key(KeyCode::Up));
        assert_eq!(c.text(), "a");
        c.handle_key(key(KeyCode::Down));
        assert_eq!(c.text(), "b");
        c.handle_key(key(KeyCode::Down));
        assert_eq!(c.text(), "");
    }

    #[test]
    fn ctrl_u_kills_to_start() {
        let mut c = make_composer();
        for ch in "hello world".chars() { c.handle_key(key(KeyCode::Char(ch))); }
        c.cursor_col = 5;
        c.handle_key(key_ctrl(KeyCode::Char('u')));
        assert_eq!(c.text(), " world");
        assert_eq!(c.cursor_col, 0);
    }

    #[test]
    fn word_navigation() {
        let mut c = make_composer();
        for ch in "foo bar baz".chars() { c.handle_key(key(KeyCode::Char(ch))); }
        c.handle_key(key_ctrl(KeyCode::Left));
        assert_eq!(c.cursor_col, 8);
        c.handle_key(key_ctrl(KeyCode::Left));
        assert_eq!(c.cursor_col, 4);
    }

    #[test]
    fn slash_completion_triggers() {
        let mut c = make_composer();
        c.handle_key(key(KeyCode::Char('/')));
        c.handle_key(key(KeyCode::Char('p')));
        c.handle_key(key(KeyCode::Char('l')));
        assert!(c.completion.active);
        assert!(c.completion.candidates.len() >= 2);
        assert!(c.completion.candidates.iter().any(|c| c.insert_text == "/plan"));
    }

    #[test]
    fn tab_accepts_completion() {
        let mut c = make_composer();
        c.handle_key(key(KeyCode::Char('/')));
        c.handle_key(key(KeyCode::Char('h')));
        c.handle_key(key(KeyCode::Char('e')));
        assert!(c.completion.active);
        c.handle_key(key(KeyCode::Tab));
        assert_eq!(c.lines[0], "/help ");
        assert!(!c.completion.active);
    }

    #[test]
    fn attachments_from_at_mention() {
        let mut c = make_composer();
        for ch in "@src/main.rs fix it".chars() { c.handle_key(key(KeyCode::Char(ch))); }
        c.handle_key(key(KeyCode::Enter));
        let submitted = c.submitted.as_ref().unwrap();
        assert_eq!(submitted.attachments.len(), 1);
        assert_eq!(submitted.attachments[0].path, "src/main.rs");
    }

    #[test]
    fn paste_multiline() {
        let mut c = make_composer();
        c.handle_paste("line one\nline two\nline three");
        assert_eq!(c.lines.len(), 3);
        assert_eq!(c.lines[0], "line one");
        assert_eq!(c.lines[2], "line three");
        assert_eq!(c.cursor_row, 2);
    }
}
