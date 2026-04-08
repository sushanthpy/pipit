/// Streaming markdown renderer — converts incremental text deltas
/// into rendered terminal output.
///
/// Uses syntect for syntax-highlighted code blocks when a language is specified.
use once_cell::sync::Lazy;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::{LinesWithEndings, as_24_bit_terminal_escaped};

static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);

pub struct StreamingMarkdownRenderer {
    buffer: String,
    in_code_block: bool,
    code_lang: Option<String>,
    code_content: String,
}

impl StreamingMarkdownRenderer {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            in_code_block: false,
            code_lang: None,
            code_content: String::new(),
        }
    }

    /// Feed a text delta, return lines to render.
    pub fn push(&mut self, delta: &str) -> Vec<RenderedLine> {
        self.buffer.push_str(delta);
        let mut lines = Vec::new();

        // Process complete lines
        while let Some(newline_pos) = self.buffer.find('\n') {
            let line = self.buffer[..newline_pos].to_string();
            self.buffer = self.buffer[newline_pos + 1..].to_string();

            if line.starts_with("```") {
                if self.in_code_block {
                    // End code block
                    lines.push(RenderedLine::CodeBlockEnd);
                    self.in_code_block = false;
                    self.code_lang = None;
                    self.code_content.clear();
                } else {
                    // Start code block
                    let lang = line[3..].trim().to_string();
                    self.code_lang = if lang.is_empty() {
                        None
                    } else {
                        Some(lang.clone())
                    };
                    self.in_code_block = true;
                    lines.push(RenderedLine::CodeBlockStart {
                        language: self.code_lang.clone(),
                    });
                }
            } else if self.in_code_block {
                self.code_content.push_str(&line);
                self.code_content.push('\n');
                lines.push(RenderedLine::Code {
                    text: line,
                    language: self.code_lang.clone(),
                });
            } else if line.starts_with("# ") {
                lines.push(RenderedLine::Heading {
                    level: 1,
                    text: line[2..].to_string(),
                });
            } else if line.starts_with("## ") {
                lines.push(RenderedLine::Heading {
                    level: 2,
                    text: line[3..].to_string(),
                });
            } else if line.starts_with("### ") {
                lines.push(RenderedLine::Heading {
                    level: 3,
                    text: line[4..].to_string(),
                });
            } else if line.starts_with("- ") || line.starts_with("* ") {
                lines.push(RenderedLine::ListItem {
                    text: line[2..].to_string(),
                });
            } else if line.starts_with("> ") {
                lines.push(RenderedLine::BlockQuote {
                    text: line[2..].to_string(),
                });
            } else if line.trim().is_empty() {
                lines.push(RenderedLine::Empty);
            } else {
                lines.push(RenderedLine::Text {
                    text: render_inline_formatting(&line),
                });
            }
        }

        lines
    }

    /// Flush any remaining content.
    pub fn flush(&mut self) -> Vec<RenderedLine> {
        if self.buffer.is_empty() {
            return vec![];
        }
        let remaining = std::mem::take(&mut self.buffer);
        if self.in_code_block {
            vec![RenderedLine::Code {
                text: remaining,
                language: self.code_lang.clone(),
            }]
        } else {
            vec![RenderedLine::Text { text: remaining }]
        }
    }
}

#[derive(Debug, Clone)]
pub enum RenderedLine {
    Text {
        text: String,
    },
    Heading {
        level: u8,
        text: String,
    },
    CodeBlockStart {
        language: Option<String>,
    },
    Code {
        text: String,
        language: Option<String>,
    },
    CodeBlockEnd,
    ListItem {
        text: String,
    },
    BlockQuote {
        text: String,
    },
    Empty,
}

impl RenderedLine {
    /// Convert to a styled terminal string.
    pub fn to_terminal_string(&self) -> String {
        match self {
            RenderedLine::Text { text } => text.clone(),
            RenderedLine::Heading { level, text } => {
                let prefix = "#".repeat(*level as usize);
                format!("\x1b[1;36m{} {}\x1b[0m", prefix, text)
            }
            RenderedLine::CodeBlockStart { language } => {
                let lang = language.as_deref().unwrap_or("");
                format!("\x1b[2m┌─ {}\x1b[0m", lang)
            }
            RenderedLine::Code { text, language } => {
                // Syntax-highlight using syntect if language is known
                if let Some(highlighted) = highlight_code_line(text, language.as_deref()) {
                    format!("│ {}\x1b[0m", highlighted)
                } else {
                    format!("\x1b[33m│ {}\x1b[0m", text)
                }
            }
            RenderedLine::CodeBlockEnd => "\x1b[2m└────\x1b[0m".to_string(),
            RenderedLine::ListItem { text } => format!("  • {}", text),
            RenderedLine::BlockQuote { text } => {
                format!("\x1b[2m▎ {}\x1b[0m", text)
            }
            RenderedLine::Empty => String::new(),
        }
    }
}

/// Syntax-highlight a single line of code using syntect.
/// Returns ANSI-escaped string, or None if language is unknown.
fn highlight_code_line(text: &str, language: Option<&str>) -> Option<String> {
    let lang = language?;
    let syntax = SYNTAX_SET
        .find_syntax_by_token(lang)
        .or_else(|| SYNTAX_SET.find_syntax_by_extension(lang))?;
    let theme = &THEME_SET.themes["base16-ocean.dark"];
    let mut h = HighlightLines::new(syntax, theme);
    let line_with_newline = format!("{}\n", text);
    let ranges = h.highlight_line(&line_with_newline, &SYNTAX_SET).ok()?;
    let escaped = as_24_bit_terminal_escaped(&ranges[..], false);
    // Strip trailing newline from the escaped output
    Some(escaped.trim_end_matches('\n').to_string())
}

/// Simple inline formatting: **bold**, *italic*, `code`
fn render_inline_formatting(text: &str) -> String {
    let mut result = text.to_string();
    // Bold: **text** → ANSI bold
    while let Some(start) = result.find("**") {
        if let Some(end) = result[start + 2..].find("**") {
            let bold_text = &result[start + 2..start + 2 + end].to_string();
            result = format!(
                "{}\x1b[1m{}\x1b[0m{}",
                &result[..start],
                bold_text,
                &result[start + 2 + end + 2..]
            );
        } else {
            break;
        }
    }
    // Inline code: `text` → ANSI yellow
    while let Some(start) = result.find('`') {
        if let Some(end) = result[start + 1..].find('`') {
            let code_text = &result[start + 1..start + 1 + end].to_string();
            result = format!(
                "{}\x1b[33m{}\x1b[0m{}",
                &result[..start],
                code_text,
                &result[start + 1 + end + 1..]
            );
        } else {
            break;
        }
    }
    result
}
