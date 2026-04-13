//! Session Export — HTML & Markdown rendering of session ledger events.
//!
//! Replays a `SessionLedger` and projects it into human-readable formats.
//! Supports two renderers:
//! - Markdown (`.md`) — portable, version-controllable
//! - HTML — self-contained single file with syntax highlighting
//!
//! Usage:
//! ```ignore
//! let events = SessionLedger::replay(&ledger_path)?;
//! let md = export_markdown(&events, &ExportOptions::default());
//! let html = export_html(&events, &ExportOptions::default());
//! ```

use crate::ledger::{LedgerEvent, SessionEvent};

/// Options controlling export output.
#[derive(Debug, Clone)]
pub struct ExportOptions {
    /// Include tool call details (args + results).
    pub include_tools: bool,
    /// Include thinking/reasoning output.
    pub include_thinking: bool,
    /// Include timestamps on each event.
    pub include_timestamps: bool,
    /// Include cost/token statistics.
    pub include_stats: bool,
    /// Title override (default: session ID).
    pub title: Option<String>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            include_tools: true,
            include_thinking: false,
            include_timestamps: true,
            include_stats: true,
            title: None,
        }
    }
}

/// Export session events to Markdown.
pub fn export_markdown(events: &[LedgerEvent], opts: &ExportOptions) -> String {
    let mut out = String::with_capacity(events.len() * 256);
    let mut session_id = String::from("unknown");
    let mut model = String::new();
    let mut provider = String::new();
    let mut current_turn = 0u32;
    let mut total_tokens = 0u64;
    let mut total_cost = 0.0f64;

    // Extract metadata from first event
    for e in events {
        if let SessionEvent::SessionStarted {
            session_id: sid,
            model: m,
            provider: p,
        } = &e.payload
        {
            session_id = sid.clone();
            model = m.clone();
            provider = p.clone();
            break;
        }
    }

    let title = opts.title.as_deref().unwrap_or(&session_id);
    out.push_str(&format!("# Session: {}\n\n", title));

    if !model.is_empty() {
        out.push_str(&format!("**Model:** {} ({})\n\n", model, provider));
    }

    out.push_str("---\n\n");

    for event in events {
        let ts = if opts.include_timestamps {
            format_timestamp(event.timestamp_ms)
        } else {
            String::new()
        };

        match &event.payload {
            SessionEvent::UserMessageAccepted { content } => {
                if !ts.is_empty() {
                    out.push_str(&format!("*{}*\n\n", ts));
                }
                out.push_str(&format!("## 🧑 User\n\n{}\n\n", content));
            }
            SessionEvent::AssistantResponseStarted { turn } => {
                current_turn = *turn;
            }
            SessionEvent::AssistantResponseCompleted {
                text,
                thinking,
                tokens_used,
            } => {
                if !ts.is_empty() {
                    out.push_str(&format!("*{}*\n\n", ts));
                }
                out.push_str(&format!("## 🤖 Assistant (turn {})\n\n", current_turn));
                if opts.include_thinking && !thinking.is_empty() {
                    out.push_str("<details>\n<summary>Thinking</summary>\n\n");
                    out.push_str(thinking);
                    out.push_str("\n\n</details>\n\n");
                }
                out.push_str(text);
                out.push_str("\n\n");
                total_tokens += tokens_used;
            }
            SessionEvent::ToolCallProposed {
                call_id: _,
                tool_name,
                args,
            } if opts.include_tools => {
                out.push_str(&format!("### 🔧 Tool: `{}`\n\n", tool_name));
                let args_str = serde_json::to_string_pretty(args).unwrap_or_default();
                if !args_str.is_empty() && args_str != "{}" {
                    out.push_str("```json\n");
                    out.push_str(&args_str);
                    out.push_str("\n```\n\n");
                }
            }
            SessionEvent::ToolCompleted {
                call_id: _,
                success,
                result_summary,
                ..
            } if opts.include_tools => {
                let status = if *success { "✅" } else { "❌" };
                out.push_str(&format!("{} Result:\n\n", status));
                if !result_summary.is_empty() {
                    out.push_str("```\n");
                    // Truncate very long results
                    if result_summary.len() > 2000 {
                        out.push_str(&result_summary[..2000]);
                        out.push_str("\n... (truncated)");
                    } else {
                        out.push_str(result_summary);
                    }
                    out.push_str("\n```\n\n");
                }
            }
            SessionEvent::PlanSelected {
                strategy,
                rationale,
            } => {
                out.push_str(&format!(
                    "### 📋 Plan: {}\n\n{}\n\n",
                    strategy, rationale
                ));
            }
            SessionEvent::SessionEnded {
                turns,
                total_tokens: tt,
                cost,
            } => {
                total_tokens = *tt;
                total_cost = *cost;
                if opts.include_stats {
                    out.push_str("---\n\n");
                    out.push_str(&format!(
                        "**Session complete** — {} turns, {} tokens, ${:.4}\n",
                        turns, total_tokens, total_cost
                    ));
                }
            }
            SessionEvent::ContextCompressed {
                messages_removed,
                tokens_freed,
                strategy,
            } => {
                out.push_str(&format!(
                    "> 📦 Context compressed: removed {} messages, freed {} tokens ({})\n\n",
                    messages_removed, tokens_freed, strategy
                ));
            }
            SessionEvent::SubagentSpawned {
                child_id, task, ..
            } => {
                out.push_str(&format!(
                    "### 🔀 Subagent spawned: `{}`\n\nTask: {}\n\n",
                    &child_id[..8.min(child_id.len())],
                    task
                ));
            }
            SessionEvent::SubagentCompleted {
                child_id, success, output, ..
            } => {
                let status = if *success { "✅" } else { "❌" };
                out.push_str(&format!(
                    "{} Subagent `{}` completed",
                    status,
                    &child_id[..8.min(child_id.len())]
                ));
                if let Some(o) = output {
                    if !o.is_empty() {
                        out.push_str("\n\n```\n");
                        if o.len() > 1000 {
                            out.push_str(&o[..1000]);
                            out.push_str("\n... (truncated)");
                        } else {
                            out.push_str(o);
                        }
                        out.push_str("\n```");
                    }
                }
                out.push_str("\n\n");
            }
            _ => {} // Skip internal events
        }
    }

    out
}

/// Export session events to a self-contained HTML file.
pub fn export_html(events: &[LedgerEvent], opts: &ExportOptions) -> String {
    let md = export_markdown(events, opts);
    let title = opts.title.as_deref().unwrap_or("Pipit Session");

    // Simple self-contained HTML with GitHub-like styling
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
  body {{
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif;
    max-width: 820px;
    margin: 2em auto;
    padding: 0 1em;
    line-height: 1.6;
    color: #24292f;
    background: #fff;
  }}
  h1 {{ border-bottom: 1px solid #d0d7de; padding-bottom: .3em; }}
  h2 {{ margin-top: 1.5em; border-bottom: 1px solid #d0d7de; padding-bottom: .2em; }}
  h3 {{ margin-top: 1em; }}
  pre {{
    background: #f6f8fa;
    border: 1px solid #d0d7de;
    border-radius: 6px;
    padding: 1em;
    overflow-x: auto;
    font-size: 0.85em;
  }}
  code {{
    background: #f6f8fa;
    padding: 0.2em 0.4em;
    border-radius: 3px;
    font-size: 0.85em;
  }}
  pre code {{ background: none; padding: 0; }}
  blockquote {{
    border-left: 4px solid #d0d7de;
    padding: 0 1em;
    color: #57606a;
    margin: 1em 0;
  }}
  details {{
    border: 1px solid #d0d7de;
    border-radius: 6px;
    padding: 0.5em 1em;
    margin: 0.5em 0;
  }}
  summary {{ cursor: pointer; font-weight: 600; }}
  hr {{ border: none; border-top: 1px solid #d0d7de; margin: 2em 0; }}
  @media (prefers-color-scheme: dark) {{
    body {{ background: #0d1117; color: #c9d1d9; }}
    h1, h2 {{ border-bottom-color: #30363d; }}
    pre {{ background: #161b22; border-color: #30363d; }}
    code {{ background: #161b22; }}
    blockquote {{ border-left-color: #30363d; color: #8b949e; }}
    details {{ border-color: #30363d; }}
    hr {{ border-top-color: #30363d; }}
  }}
</style>
</head>
<body>
{md}
</body>
</html>
"#,
        title = html_escape(title),
        md = minimal_md_to_html(&md)
    )
}

/// Format a unix-millis timestamp to a readable string.
fn format_timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    let mins = (secs / 60) % 60;
    let hours = (secs / 3600) % 24;
    format!("{:02}:{:02}:{:02}", hours, mins, secs % 60)
}

/// Minimal HTML escaping.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Very minimal Markdown-to-HTML converter for the export.
/// Handles headings, code blocks, bold, blockquotes, hr, paragraphs.
fn minimal_md_to_html(md: &str) -> String {
    let mut html = String::with_capacity(md.len() * 2);
    let mut in_code_block = false;
    let mut in_paragraph = false;

    for line in md.lines() {
        if line.starts_with("```") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            if in_code_block {
                html.push_str("</code></pre>\n");
                in_code_block = false;
            } else {
                let lang = line.trim_start_matches('`').trim();
                if lang.is_empty() {
                    html.push_str("<pre><code>");
                } else {
                    html.push_str(&format!("<pre><code class=\"language-{}\">", html_escape(lang)));
                }
                in_code_block = true;
            }
            continue;
        }

        if in_code_block {
            html.push_str(&html_escape(line));
            html.push('\n');
            continue;
        }

        let trimmed = line.trim();

        if trimmed.is_empty() {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            continue;
        }

        if trimmed == "---" || trimmed == "***" {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str("<hr>\n");
            continue;
        }

        if trimmed.starts_with("### ") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(&format!("<h3>{}</h3>\n", html_escape(&trimmed[4..])));
            continue;
        }

        if trimmed.starts_with("## ") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(&format!("<h2>{}</h2>\n", html_escape(&trimmed[3..])));
            continue;
        }

        if trimmed.starts_with("# ") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(&format!("<h1>{}</h1>\n", html_escape(&trimmed[2..])));
            continue;
        }

        if trimmed.starts_with("> ") {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(&format!(
                "<blockquote>{}</blockquote>\n",
                html_escape(&trimmed[2..])
            ));
            continue;
        }

        if trimmed.starts_with("<details>") || trimmed.starts_with("</details>")
            || trimmed.starts_with("<summary>") || trimmed.starts_with("</summary>")
        {
            if in_paragraph {
                html.push_str("</p>\n");
                in_paragraph = false;
            }
            html.push_str(trimmed);
            html.push('\n');
            continue;
        }

        // Regular text — wrap in paragraph
        if !in_paragraph {
            html.push_str("<p>");
            in_paragraph = true;
        } else {
            html.push_str("<br>\n");
        }

        // Inline formatting: **bold**, `code`
        let escaped = html_escape(trimmed);
        let formatted = inline_format(&escaped);
        html.push_str(&formatted);
    }

    if in_paragraph {
        html.push_str("</p>\n");
    }
    if in_code_block {
        html.push_str("</code></pre>\n");
    }

    html
}

/// Apply inline Markdown formatting: **bold** and `code`.
fn inline_format(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            // Find closing **
            if let Some(end) = find_pattern(&chars, i + 2, &['*', '*']) {
                result.push_str("<strong>");
                result.extend(&chars[i + 2..end]);
                result.push_str("</strong>");
                i = end + 2;
                continue;
            }
        }
        if chars[i] == '`' {
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == '`') {
                result.push_str("<code>");
                result.extend(&chars[i + 1..i + 1 + end]);
                result.push_str("</code>");
                i = i + 1 + end + 1;
                continue;
            }
        }
        result.push(chars[i]);
        i += 1;
    }

    result
}

fn find_pattern(chars: &[char], start: usize, pattern: &[char]) -> Option<usize> {
    if pattern.len() > chars.len() {
        return None;
    }
    for i in start..=chars.len() - pattern.len() {
        if chars[i..i + pattern.len()] == *pattern {
            return Some(i);
        }
    }
    None
}

// ─── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{LedgerEvent, SessionEvent};

    fn make_event(seq: u64, payload: SessionEvent) -> LedgerEvent {
        LedgerEvent {
            seq,
            prev_hash: 0,
            hash: seq,
            timestamp_ms: 1700000000000 + seq * 1000,
            payload,
        }
    }

    #[test]
    fn test_markdown_basic_conversation() {
        let events = vec![
            make_event(1, SessionEvent::SessionStarted {
                session_id: "test-123".into(),
                model: "gpt-4".into(),
                provider: "openai".into(),
            }),
            make_event(2, SessionEvent::UserMessageAccepted {
                content: "Hello".into(),
            }),
            make_event(3, SessionEvent::AssistantResponseStarted { turn: 1 }),
            make_event(4, SessionEvent::AssistantResponseCompleted {
                text: "Hi there!".into(),
                thinking: String::new(),
                tokens_used: 50,
            }),
            make_event(5, SessionEvent::SessionEnded {
                turns: 1,
                total_tokens: 50,
                cost: 0.001,
            }),
        ];

        let md = export_markdown(&events, &ExportOptions::default());
        assert!(md.contains("# Session: test-123"));
        assert!(md.contains("## 🧑 User"));
        assert!(md.contains("Hello"));
        assert!(md.contains("## 🤖 Assistant (turn 1)"));
        assert!(md.contains("Hi there!"));
        assert!(md.contains("1 turns"));
    }

    #[test]
    fn test_html_contains_structure() {
        let events = vec![
            make_event(1, SessionEvent::SessionStarted {
                session_id: "html-test".into(),
                model: "claude".into(),
                provider: "anthropic".into(),
            }),
            make_event(2, SessionEvent::UserMessageAccepted {
                content: "Test <script>alert(1)</script>".into(),
            }),
        ];

        let html = export_html(&events, &ExportOptions::default());
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("html-test"));
        // Script tags should be escaped
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn test_export_with_tools() {
        let events = vec![
            make_event(1, SessionEvent::SessionStarted {
                session_id: "t".into(),
                model: "m".into(),
                provider: "p".into(),
            }),
            make_event(2, SessionEvent::ToolCallProposed {
                call_id: "c1".into(),
                tool_name: "read_file".into(),
                args: serde_json::json!({"path": "/foo.rs"}),
            }),
            make_event(3, SessionEvent::ToolCompleted {
                call_id: "c1".into(),
                success: true,
                mutated: false,
                result_summary: "file contents".into(),
                result_blob_hash: None,
            }),
        ];

        let md = export_markdown(&events, &ExportOptions::default());
        assert!(md.contains("🔧 Tool: `read_file`"));
        assert!(md.contains("file contents"));

        // Without tools
        let opts = ExportOptions {
            include_tools: false,
            ..Default::default()
        };
        let md_no_tools = export_markdown(&events, &opts);
        assert!(!md_no_tools.contains("read_file"));
    }

    #[test]
    fn test_timestamp_format() {
        assert_eq!(format_timestamp(3661000), "01:01:01");
        assert_eq!(format_timestamp(0), "00:00:00");
    }
}
