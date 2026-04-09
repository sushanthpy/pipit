//! Tool output noise reduction pipeline.
//!
//! Applies a series of fast, string-only transformations to tool results
//! before they enter the context window. Inspired by RTK's 8-stage pipeline
//! but tailored for agent-loop tool results (not CLI proxying).
//!
//! Pipeline stages (applied in order):
//!   1. strip_ansi       — remove terminal escape codes
//!   2. strip_noise_lines — remove known low-signal lines (progress bars, download logs, etc.)
//!   3. collapse_blanks   — normalize whitespace runs
//!   4. head_tail_split   — smart truncation preserving head + tail

/// Strip ANSI escape codes (SGR, cursor, OSC, etc.) from tool output.
///
/// Terminal commands produce colored output that wastes tokens and confuses
/// the model. This is stage 1 of the noise pipeline.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == 0x1b && i + 1 < len {
            match bytes[i + 1] {
                // CSI sequences: ESC [ ... <letter>
                b'[' => {
                    i += 2;
                    while i < len && !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'~') {
                        i += 1;
                    }
                    if i < len {
                        i += 1; // skip the terminal character
                    }
                    continue;
                }
                // OSC sequences: ESC ] ... (terminated by BEL or ST)
                b']' => {
                    i += 2;
                    while i < len {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < len && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                    continue;
                }
                // Two-character sequences: ESC <letter>
                _ if bytes[i + 1].is_ascii_alphabetic() => {
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        // Also strip lone CSI (0x9b) — 8-bit control
        if bytes[i] == 0x9b {
            i += 1;
            while i < len && !(bytes[i].is_ascii_alphabetic() || bytes[i] == b'~') {
                i += 1;
            }
            if i < len {
                i += 1;
            }
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }

    out
}

/// Known noise patterns in tool output — lines matching these are stripped.
/// Each pattern is checked as a prefix on the trimmed line.
const NOISE_PREFIXES: &[&str] = &[
    // Cargo / Rust
    "Compiling ",
    "Downloading ",
    "Downloaded ",
    "Updating crates.io",
    "Blocking waiting for",
    "Locking ",
    "Fetching ",
    // npm / Node
    "npm warn ",
    "npm WARN ",
    "npm notice ",
    "added ",
    "removed ",
    "up to date",
    // pip / Python
    "Collecting ",
    "Using cached ",
    "Requirement already satisfied",
    "Installing collected packages",
    "Successfully installed",
    // Git
    "remote: Counting objects",
    "remote: Compressing objects",
    "Receiving objects:",
    "Resolving deltas:",
    "Unpacking objects:",
    // General progress
    "Resolving dependencies",
    "Downloading dependencies",
    "Progress: ",
];

/// Lines that are noise when they appear AND the output has >20 lines.
/// In short output, these might be the only informative content.
const NOISE_PREFIXES_LONG_OUTPUT: &[&str] = &[
    "warning: ",
    "Warning: ",
    "   Compiling ",
    "    Finished ",
    "    Blocking ",
];

/// Patterns matched as substrings anywhere in the line.
const NOISE_CONTAINS: &[&str] = &[
    // Progress indicators
    "━━━━",
    "████",
    "▓▓▓▓",
    "░░░░",
    "====",
    // Spinner characters (repeated)
    "⠋⠙⠹",
    "⣾⣽⣻",
];

/// Remove known low-signal lines from tool output.
///
/// On short outputs (<20 lines), only strips NOISE_PREFIXES.
/// On longer outputs, also strips NOISE_PREFIXES_LONG_OUTPUT.
pub fn strip_noise_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let is_long = total > 20;

    let mut out = Vec::with_capacity(total);
    let mut stripped_count: usize = 0;

    for line in &lines {
        let trimmed = line.trim();

        // Skip empty lines (handled by collapse_blanks later)
        // but keep them for structure preservation
        if trimmed.is_empty() {
            out.push(*line);
            continue;
        }

        // Check prefix noise
        let is_prefix_noise = NOISE_PREFIXES.iter().any(|p| trimmed.starts_with(p));
        let is_long_noise =
            is_long && NOISE_PREFIXES_LONG_OUTPUT.iter().any(|p| trimmed.starts_with(p));
        let is_contains_noise = NOISE_CONTAINS.iter().any(|p| trimmed.contains(p));

        if is_prefix_noise || is_long_noise || is_contains_noise {
            stripped_count += 1;
            continue;
        }

        out.push(*line);
    }

    let mut result = out.join("\n");

    // If we stripped anything, add a note
    if stripped_count > 0 && total > 10 {
        result.push_str(&format!(
            "\n[{} noise lines stripped]",
            stripped_count
        ));
    }

    result
}

/// Collapse runs of 3+ blank lines to 2, and strip trailing whitespace.
pub fn collapse_blanks(text: &str) -> String {
    let mut out = Vec::new();
    let mut consecutive_blanks: u32 = 0;

    for line in text.lines() {
        let trimmed_end = line.trim_end();
        if trimmed_end.is_empty() {
            consecutive_blanks += 1;
            if consecutive_blanks <= 2 {
                out.push("");
            }
        } else {
            consecutive_blanks = 0;
            out.push(trimmed_end);
        }
    }

    // Trim trailing blank lines
    while out.last() == Some(&"") {
        out.pop();
    }

    out.join("\n")
}

/// Smart head/tail truncation that preserves error-relevant content.
///
/// When output exceeds `max_lines`:
/// - If it looks like an error (is_error=true), keep more tail (tracebacks)
/// - Otherwise, balanced head/tail split
/// - Always preserves lines containing "error", "Error", "FAILED", etc.
pub fn head_tail_split(text: &str, max_lines: usize, is_error: bool) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();

    if total <= max_lines {
        return text.to_string();
    }

    // Adaptive split: errors get more tail for tracebacks
    let (head_n, tail_n) = if is_error {
        let tail = (max_lines * 3) / 4; // 75% tail
        let head = max_lines - tail;
        (head.max(5), tail)
    } else {
        let head = max_lines / 2;
        let tail = max_lines - head;
        (head, tail)
    };

    let head_n = head_n.min(total);
    let tail_n = tail_n.min(total.saturating_sub(head_n));
    let omitted = total - head_n - tail_n;

    if omitted == 0 {
        return text.to_string();
    }

    // Scan the omitted region for error-like lines and rescue them
    let omitted_start = head_n;
    let omitted_end = total - tail_n;
    let mut rescued: Vec<&str> = Vec::new();

    for line in &lines[omitted_start..omitted_end] {
        let lower = line.to_lowercase();
        if lower.contains("error")
            || lower.contains("failed")
            || lower.contains("panic")
            || lower.contains("exception")
            || lower.contains("traceback")
        {
            rescued.push(line);
            if rescued.len() >= 10 {
                break; // Cap rescued lines
            }
        }
    }

    let mut result = lines[..head_n].join("\n");
    result.push_str(&format!(
        "\n\n[...{} of {} lines omitted",
        omitted, total
    ));
    if !rescued.is_empty() {
        result.push_str(&format!(
            "; {} error-relevant lines preserved",
            rescued.len()
        ));
    }
    result.push_str("...]\n");
    if !rescued.is_empty() {
        result.push('\n');
        result.push_str(&rescued.join("\n"));
        result.push('\n');
    }
    result.push('\n');
    result.push_str(&lines[total - tail_n..].join("\n"));

    result
}

/// Apply the full noise reduction pipeline to a tool result.
///
/// Pipeline: strip_ansi → strip_noise_lines → collapse_blanks → head_tail_split
pub fn clean_tool_output(content: &str, max_lines: usize, is_error: bool) -> String {
    // Stage 1: Strip ANSI escape codes
    let cleaned = strip_ansi(content);

    // Stage 2: Remove known noise lines
    let cleaned = strip_noise_lines(&cleaned);

    // Stage 3: Normalize whitespace
    let cleaned = collapse_blanks(&cleaned);

    // Stage 4: Smart head/tail truncation
    head_tail_split(&cleaned, max_lines, is_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_color_codes() {
        let input = "\x1b[32mOK\x1b[0m: test passed";
        assert_eq!(strip_ansi(input), "OK: test passed");
    }

    #[test]
    fn strip_ansi_removes_cursor_sequences() {
        let input = "\x1b[2J\x1b[HHello\x1b[1A";
        assert_eq!(strip_ansi(input), "Hello");
    }

    #[test]
    fn strip_ansi_preserves_plain_text() {
        let input = "no escape codes here";
        assert_eq!(strip_ansi(input), input);
    }

    #[test]
    fn strip_ansi_handles_osc_sequences() {
        let input = "\x1b]0;title\x07content";
        assert_eq!(strip_ansi(input), "content");
    }

    #[test]
    fn strip_noise_removes_cargo_compiling() {
        let input = "Compiling serde v1.0\nCompiling tokio v1.0\nerror[E0308]: mismatched types";
        let result = strip_noise_lines(input);
        assert!(result.contains("error[E0308]"));
        assert!(!result.contains("Compiling serde"));
    }

    #[test]
    fn strip_noise_removes_npm_warnings() {
        let input = "npm warn deprecated\nnpm WARN old package\nfound 0 vulnerabilities";
        let result = strip_noise_lines(input);
        assert!(result.contains("found 0 vulnerabilities"));
        assert!(!result.contains("npm warn"));
    }

    #[test]
    fn strip_noise_removes_pip_collecting() {
        let input = "Collecting requests\nUsing cached requests-2.28.0\nDone";
        let result = strip_noise_lines(input);
        assert!(result.contains("Done"));
        assert!(!result.contains("Collecting requests"));
    }

    #[test]
    fn strip_noise_preserves_short_output() {
        // In short output (<= 20 lines), we strip prefixes but don't add summary
        let input = "Compiling foo\nerror: something broke";
        let result = strip_noise_lines(input);
        assert!(result.contains("error: something broke"));
        assert!(!result.contains("noise lines stripped"));
    }

    #[test]
    fn strip_noise_removes_progress_bars() {
        let input = "Downloading...\n━━━━━━━━━━━━━━━━ 100%\nDone";
        let result = strip_noise_lines(input);
        assert!(result.contains("Done"));
        assert!(!result.contains("━━━━"));
    }

    #[test]
    fn collapse_blanks_normalizes_runs() {
        let input = "line1\n\n\n\n\nline2\n\n\n\nline3";
        let result = collapse_blanks(input);
        assert_eq!(result, "line1\n\n\nline2\n\n\nline3");
    }

    #[test]
    fn collapse_blanks_strips_trailing() {
        let input = "line1\nline2\n\n\n";
        let result = collapse_blanks(input);
        assert_eq!(result, "line1\nline2");
    }

    #[test]
    fn head_tail_short_output_passthrough() {
        let input = "line1\nline2\nline3";
        assert_eq!(head_tail_split(input, 10, false), input);
    }

    #[test]
    fn head_tail_balanced_truncation() {
        let lines: Vec<String> = (0..100).map(|i| format!("line {}", i)).collect();
        let input = lines.join("\n");
        let result = head_tail_split(&input, 20, false);
        assert!(result.contains("line 0"));
        assert!(result.contains("line 99"));
        assert!(result.contains("lines omitted"));
    }

    #[test]
    fn head_tail_error_mode_more_tail() {
        let mut lines: Vec<String> = (0..100).map(|i| format!("line {}", i)).collect();
        lines[95] = "Traceback (most recent call last):".to_string();
        let input = lines.join("\n");
        let result = head_tail_split(&input, 20, true);
        // With is_error, should keep 75% tail = 15 tail lines
        assert!(result.contains("Traceback"));
        assert!(result.contains("line 99"));
    }

    #[test]
    fn head_tail_rescues_error_lines() {
        let mut lines: Vec<String> = (0..100).map(|i| format!("ok line {}", i)).collect();
        lines[50] = "FATAL ERROR: disk full".to_string();
        let input = lines.join("\n");
        let result = head_tail_split(&input, 20, false);
        // The error at line 50 is in the omitted region — should be rescued
        assert!(result.contains("FATAL ERROR: disk full"));
        assert!(result.contains("error-relevant lines preserved"));
    }

    #[test]
    fn full_pipeline_cargo_build() {
        let input = "\x1b[32mCompiling\x1b[0m serde v1.0.0\n\
                     \x1b[32mCompiling\x1b[0m tokio v1.0.0\n\
                     \x1b[32mCompiling\x1b[0m my-crate v0.1.0\n\
                     \x1b[31merror[E0308]\x1b[0m: mismatched types\n\
                      --> src/main.rs:10:5\n\
                     \x1b[31merror\x1b[0m: aborting due to 1 previous error";
        let result = clean_tool_output(input, 100, true);
        assert!(result.contains("error[E0308]: mismatched types"));
        assert!(result.contains("src/main.rs:10:5"));
        assert!(!result.contains("\x1b["));
        assert!(!result.contains("Compiling serde"));
    }

    #[test]
    fn full_pipeline_npm_install() {
        let input = "npm warn deprecated inflight@1.0.6\n\
                     npm warn deprecated glob@7.2.3\n\
                     added 542 packages in 12s\n\
                     73 packages are looking for funding";
        let result = clean_tool_output(input, 100, false);
        assert!(!result.contains("npm warn"));
        assert!(result.contains("73 packages are looking for funding"));
    }
}
