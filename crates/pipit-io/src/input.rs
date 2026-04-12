use std::io::{self, BufRead};

/// Read user input with readline-style editing.
pub fn read_input() -> Option<String> {
    let mut input = String::new();
    let stdin = io::stdin();

    match stdin.lock().read_line(&mut input) {
        Ok(0) => None, // EOF
        Ok(_) => {
            let trimmed = input.trim_end().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }
        Err(_) => None,
    }
}

/// Read multiline input (ends with empty line).
pub fn read_multiline_input() -> Option<String> {
    let mut lines = Vec::new();
    let stdin = io::stdin();
    let mut reader = stdin.lock();

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                if line.trim().is_empty() && !lines.is_empty() {
                    break;
                }
                lines.push(line);
            }
            Err(_) => break,
        }
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.concat().trim_end().to_string())
    }
}

// ─── Input classification ────────────────────────────────────────────────────

/// Classify raw user input into a structured `UserInput`.
pub fn classify_input(raw: &str) -> UserInput {
    let trimmed = raw.trim();

    // Slash commands: /help, /context, etc.
    if let Some(cmd) = parse_slash_command(trimmed) {
        return UserInput::Command(cmd);
    }

    // Shell passthrough: !ls, !cargo test, etc.
    if let Some(shell) = trimmed.strip_prefix('!') {
        return UserInput::ShellPassthrough(shell.trim().to_string());
    }

    // Keyboard shortcuts
    if trimmed == "?" {
        return UserInput::Command(SlashCommand::Help);
    }

    // File mentions: extract @file references and the remaining prompt
    let (files, prompt) = extract_file_mentions(trimmed);
    if !files.is_empty() {
        return UserInput::PromptWithFiles { prompt, files };
    }

    // Image file detection: if the input contains a path to an image file,
    // treat it as a prompt with image attachment
    let (image_paths, remaining) = extract_image_paths(trimmed);
    if !image_paths.is_empty() {
        return UserInput::PromptWithImages {
            prompt: remaining,
            image_paths,
        };
    }

    UserInput::Prompt(trimmed.to_string())
}

/// Structured representation of user input.
#[derive(Debug)]
pub enum UserInput {
    /// A free-form prompt to the agent.
    Prompt(String),
    /// A prompt with `@file` mentions that should be added to context.
    PromptWithFiles { prompt: String, files: Vec<String> },
    /// A prompt with image attachments (file paths to images).
    PromptWithImages {
        prompt: String,
        image_paths: Vec<String>,
    },
    /// A slash command.
    Command(SlashCommand),
    /// Shell passthrough (!command).
    ShellPassthrough(String),
}

/// Extract `@path/to/file` mentions from input text.
/// Returns (file_paths, remaining_prompt_text).
fn extract_file_mentions(input: &str) -> (Vec<String>, String) {
    let mut files = Vec::new();
    let mut prompt_parts = Vec::new();

    for token in input.split_whitespace() {
        if let Some(path) = token.strip_prefix('@') {
            if !path.is_empty() {
                files.push(path.to_string());
            } else {
                prompt_parts.push(token);
            }
        } else {
            prompt_parts.push(token);
        }
    }

    (files, prompt_parts.join(" "))
}

const IMAGE_EXTENSIONS: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".svg"];

/// Extract image file paths from input text.
/// Detects tokens that look like paths to image files.
fn extract_image_paths(input: &str) -> (Vec<String>, String) {
    let mut images = Vec::new();
    let mut prompt_parts = Vec::new();

    for token in input.split_whitespace() {
        let lower = token.to_lowercase();
        if IMAGE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext)) {
            // Check if it looks like a real file path
            if token.contains('/') || token.contains('\\') || token.starts_with('.') {
                images.push(token.to_string());
            } else {
                prompt_parts.push(token);
            }
        } else {
            prompt_parts.push(token);
        }
    }

    (images, prompt_parts.join(" "))
}

/// Check if a file path is an image.
pub fn is_image_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    IMAGE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

/// Read an image file and return (media_type, data).
pub fn read_image_file(path: &str) -> Result<(String, Vec<u8>), String> {
    let data = std::fs::read(path).map_err(|e| format!("Cannot read image {}: {}", path, e))?;

    let lower = path.to_lowercase();
    let media_type = if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".bmp") {
        "image/bmp"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    };

    Ok((media_type.to_string(), data))
}

// ─── Slash commands ──────────────────────────────────────────────────────────

/// All supported slash commands.
#[derive(Debug)]
pub enum SlashCommand {
    Help,
    Status,
    Plans,
    Clear,
    Model(String),
    Compact,
    Undo,
    Branch(Option<String>),
    BranchList,
    BranchSwitch(String),
    Cost,
    Quit,

    // ── New commands ──
    /// View current working set / context.
    Context,
    /// Show token usage and context pressure.
    Tokens,
    /// Show or switch approval mode.
    Permissions(Option<String>),
    /// Discuss before editing — enter plan-first mode.
    Plan(Option<String>),
    /// Add a file to the working set.
    Add(String),
    /// Drop a file from the working set.
    Drop(String),
    /// Summarize / compress context.
    Summarize,
    /// Rewind to a previous state.
    Rewind,
    /// Run verification pipeline (build, lint, test).
    Verify(Option<String>),
    /// Save current session to a file for later resumption.
    SaveSession(Option<String>),
    /// Resume a previously saved session.
    ResumeSession(Option<String>),
    /// Quick side question without losing context.
    Aside(String),
    /// Create or verify a workflow checkpoint.
    Checkpoint(Option<String>),
    /// Test-driven development workflow.
    Tdd(Option<String>),
    /// Code review of uncommitted changes.
    CodeReview,
    /// Fix build errors incrementally.
    BuildFix,

    /// Run threat analysis on current files.
    Threat,
    /// Run evolutionary variant comparison.
    Evolve(Option<String>),
    /// Environment fingerprint and diagnostics.
    Env(Option<String>),
    /// Spec-driven development: decompose spec into tasks.
    Spec(Option<String>),

    /// Re-run interactive setup wizard.
    Setup,
    /// Show current config or edit a key.
    Config(Option<String>),

    /// Run provider connectivity and health check.
    Doctor,
    /// List available skills.
    Skills,
    /// List active hooks.
    Hooks,
    /// Show MCP server status.
    Mcp,

    /// Show uncommitted changes (git diff).
    Diff,
    /// AI-authored commit with generated message.
    Commit(Option<String>),
    /// Search the codebase.
    Search(String),
    /// Persistent cross-session memory.
    Memory(Option<String>),
    /// Continuous polling mode: re-run a prompt at intervals.
    Loop(Option<String>),
    /// Background a task to the daemon.
    Background(Option<String>),

    /// Benchmark runner.
    Bench(Option<String>),
    /// Headless browser control.
    Browse(Option<String>),
    /// Mesh network management.
    Mesh(Option<String>),
    /// Ambient watch mode.
    Watch(Option<String>),
    /// Dependency health check.
    Deps(Option<String>),
    /// Browse or search the plugin registry.
    Registry(Option<String>),

    /// Toggle Vim modal editing in the composer.
    Vim,

    /// Switch provider/model profile or list available profiles.
    Provider(Option<String>),

    Unknown(String),
}

pub fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    if !input.starts_with('/') {
        return None;
    }

    // Distinguish slash commands from file paths:
    // /help is a command, /var/folders/... is a file path.
    // Commands start with / followed by a letter; paths start with / followed by a directory name.
    let after_slash = &input[1..];
    if after_slash.is_empty() {
        return None;
    }
    // If the second character is also '/' or the text looks like a path, it's not a command
    if after_slash.starts_with('/')
        || after_slash.starts_with("var/")
        || after_slash.starts_with("tmp/")
        || after_slash.starts_with("usr/")
        || after_slash.starts_with("home/")
        || after_slash.starts_with("Users/")
        || after_slash.starts_with("etc/")
        || after_slash.starts_with("opt/")
    {
        return None;
    }

    let parts: Vec<&str> = input[1..].splitn(2, ' ').collect();
    let cmd = parts[0].to_lowercase();
    let arg = parts.get(1).map(|s| s.trim().to_string());

    Some(match cmd.as_str() {
        "help" | "h" => SlashCommand::Help,
        "status" => SlashCommand::Status,
        "plans" => SlashCommand::Plans,
        "clear" | "c" => SlashCommand::Clear,
        "model" | "m" => SlashCommand::Model(arg.unwrap_or_default()),
        "compact" => SlashCommand::Compact,
        "undo" | "u" => SlashCommand::Undo,
        "branch" | "b" => SlashCommand::Branch(arg),
        "branches" => SlashCommand::BranchList,
        "switch" => SlashCommand::BranchSwitch(arg.unwrap_or_default()),
        "cost" => SlashCommand::Cost,
        "quit" | "q" | "exit" => SlashCommand::Quit,

        // New commands
        "context" | "ctx" => SlashCommand::Context,
        "tokens" | "tok" => SlashCommand::Tokens,
        "permissions" | "perm" | "perms" => SlashCommand::Permissions(arg),
        "plan" | "p" => SlashCommand::Plan(arg),
        "add" => SlashCommand::Add(arg.unwrap_or_default()),
        "drop" | "remove" => SlashCommand::Drop(arg.unwrap_or_default()),
        "summarize" | "sum" => SlashCommand::Summarize,
        "rewind" | "rw" => SlashCommand::Rewind,
        "verify" | "v" => SlashCommand::Verify(arg),
        "save-session" | "save" => SlashCommand::SaveSession(arg),
        "resume-session" | "resume" => SlashCommand::ResumeSession(arg),
        "aside" => SlashCommand::Aside(arg.unwrap_or_default()),
        "checkpoint" | "cp" => SlashCommand::Checkpoint(arg),
        "tdd" => SlashCommand::Tdd(arg),
        "code-review" | "review" => SlashCommand::CodeReview,
        "build-fix" | "fix" => SlashCommand::BuildFix,

        "threat" | "threats" | "security" => SlashCommand::Threat,
        "evolve" | "evo" => SlashCommand::Evolve(arg),
        "env" | "environment" => SlashCommand::Env(arg),
        "spec" | "sdd" => SlashCommand::Spec(arg),

        "setup" | "init" => SlashCommand::Setup,
        "config" | "cfg" | "settings" => SlashCommand::Config(arg),

        "doctor" | "health" => SlashCommand::Doctor,
        "skills" | "skill" => SlashCommand::Skills,
        "hooks" | "hook" => SlashCommand::Hooks,
        "mcp" | "servers" => SlashCommand::Mcp,

        "diff" | "d" => SlashCommand::Diff,
        "commit" | "ci" => SlashCommand::Commit(arg),
        "search" | "find" | "s" => SlashCommand::Search(arg.unwrap_or_default()),
        "loop" => SlashCommand::Loop(arg),
        "memory" | "mem" => SlashCommand::Memory(arg),
        "bg" | "background" => SlashCommand::Background(arg),

        "bench" | "benchmark" => SlashCommand::Bench(arg),
        "browse" | "browser" => SlashCommand::Browse(arg),
        "mesh" | "cluster" => SlashCommand::Mesh(arg),
        "watch" => SlashCommand::Watch(arg),
        "deps" | "dependencies" | "audit" => SlashCommand::Deps(arg),
        "registry" | "plugins" => SlashCommand::Registry(arg),
        "vim" => SlashCommand::Vim,
        "provider" | "prov" | "providers" => SlashCommand::Provider(arg),

        "doc" => SlashCommand::Unknown("doc".to_string()), // Reserved for future /doc [topic]

        _ => SlashCommand::Unknown(cmd),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_commands() {
        assert!(matches!(
            parse_slash_command("/help"),
            Some(SlashCommand::Help)
        ));
        assert!(matches!(
            parse_slash_command("/quit"),
            Some(SlashCommand::Quit)
        ));
        assert!(matches!(
            parse_slash_command("/context"),
            Some(SlashCommand::Context)
        ));
        assert!(matches!(
            parse_slash_command("/tokens"),
            Some(SlashCommand::Tokens)
        ));
    }

    #[test]
    fn parse_permissions_with_arg() {
        match parse_slash_command("/permissions plan") {
            Some(SlashCommand::Permissions(Some(mode))) => assert_eq!(mode, "plan"),
            other => panic!("expected Permissions(Some(\"plan\")), got {:?}", other),
        }
    }

    #[test]
    fn parse_permissions_without_arg() {
        assert!(matches!(
            parse_slash_command("/permissions"),
            Some(SlashCommand::Permissions(None))
        ));
    }

    #[test]
    fn classify_shell_passthrough() {
        match classify_input("!cargo test") {
            UserInput::ShellPassthrough(cmd) => assert_eq!(cmd, "cargo test"),
            other => panic!("expected ShellPassthrough, got {:?}", other),
        }
    }

    #[test]
    fn classify_file_mentions() {
        match classify_input("fix the bug in @src/main.rs and @lib.rs") {
            UserInput::PromptWithFiles { prompt, files } => {
                assert_eq!(prompt, "fix the bug in and");
                assert_eq!(files, vec!["src/main.rs", "lib.rs"]);
            }
            other => panic!("expected PromptWithFiles, got {:?}", other),
        }
    }

    #[test]
    fn classify_plain_prompt() {
        match classify_input("explain this code") {
            UserInput::Prompt(text) => assert_eq!(text, "explain this code"),
            other => panic!("expected Prompt, got {:?}", other),
        }
    }

    #[test]
    fn classify_question_mark_as_help() {
        assert!(matches!(
            classify_input("?"),
            UserInput::Command(SlashCommand::Help)
        ));
    }

    #[test]
    fn parse_bg_command() {
        match parse_slash_command("/bg fix the tests") {
            Some(SlashCommand::Background(Some(prompt))) => {
                assert_eq!(prompt, "fix the tests");
            }
            other => panic!("expected Background(Some(...)), got {:?}", other),
        }
        assert!(matches!(
            parse_slash_command("/bg"),
            Some(SlashCommand::Background(None))
        ));
        assert!(matches!(
            parse_slash_command("/background run lints"),
            Some(SlashCommand::Background(Some(_)))
        ));
    }

    #[test]
    fn parse_registry_command() {
        match parse_slash_command("/registry search foo") {
            Some(SlashCommand::Registry(Some(query))) => {
                assert_eq!(query, "search foo");
            }
            other => panic!("expected Registry(Some(...)), got {:?}", other),
        }
        assert!(matches!(
            parse_slash_command("/registry"),
            Some(SlashCommand::Registry(None))
        ));
        assert!(matches!(
            parse_slash_command("/plugins"),
            Some(SlashCommand::Registry(None))
        ));
    }
}
