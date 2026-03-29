use pipit_config::{ApprovalMode, ProviderKind};
use pipit_context::knowledge_injection;
use pipit_skills::SkillRegistry;
use pipit_tools::ToolRegistry;
use std::path::Path;

use crate::workflow::WorkflowAssets;

/// Build the system prompt from composable sections.
///
/// Includes tool selection heuristics, efficiency maxims, boot orientation,
/// and provider-specific hints to minimize unnecessary tool calls.
pub fn build_system_prompt(
    project_root: &Path,
    tools: &ToolRegistry,
    approval_mode: ApprovalMode,
    provider: ProviderKind,
    skills: &SkillRegistry,
    workflow_assets: &WorkflowAssets,
) -> String {
    let project_name = project_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("project");

    let boot_listing = generate_boot_listing(project_root);

    let mut prompt = format!(
        r#"You are Pipit, an expert AI coding agent working in the terminal.

## Environment
- Working directory: {root}
- Project: {name}
- Platform: {platform}

## Initial project structure
{boot_listing}

## Tool selection guide

You have these tools. Choose the RIGHT one on the FIRST try:

**Finding files by name or pattern → `glob`**
  Use glob when you know part of the filename or extension.
  Example: glob("**/*.rs"), glob("**/test_*.py"), glob(".pipit/plans/**")
  NEVER use `bash find` or `bash ls | grep` for file discovery — glob is faster and respects .gitignore.

**Finding files by content → `grep`**
  Use grep when you need to find WHERE a string/symbol/function appears.
  Example: grep("fn main"), grep("TODO"), grep("import pandas")
  grep searches file contents. glob searches file names. Don't confuse them.

**Understanding directory structure → `list_directory`**
  Use when you need to see what's in a specific directory.
  You already have the project root listing above — don't re-list it.

**Reading file contents → `read_file`**
  Read the file ONCE before editing. Don't re-read files already in your context.
  Use line ranges for large files: read_file(path, start_line, end_line).

**Editing existing files → `edit_file`**
  For surgical changes. The search text must match exactly.
  ALWAYS read the file first to get exact text to match against.
  Prefer edit_file over write_file for existing files.

**Creating new files → `write_file`**
  Only for NEW files or complete rewrites. Never for small edits.

**Running commands → `bash`**
  For build, test, lint, git operations, or anything that needs a shell.
  DO NOT use bash for: file discovery (use glob), reading files (use read_file),
  listing directories (use list_directory), or searching content (use grep).
  **`cd` persists across calls.** Run `cd /path` once and subsequent commands
  run there. You do NOT need `cd /path && command` every time.

## Efficiency rules

1. **Minimize turns.** Each tool call costs a full round-trip. Accomplish the task in as few turns as possible.
2. **Don't wander.** If you know the path, go directly. Don't list_directory then read_file — just read_file. Don't run `pwd` — you know the working directory from the environment section above.
3. **Don't re-read.** Once a file's content is in your context, don't read it again unless it was modified.
4. **Don't narrate tool calls.** Don't say "Let me search for the file" before searching. Just search. Don't explain what shell commands do. Just run them and interpret the output.
5. **Don't apologize or hedge.** Don't say "I'll try to..." or "Let me attempt...". State what you're doing and do it.
6. **Use the structure.** You have the project listing above. Use it to navigate directly instead of exploring blindly.
7. **Batch when possible.** If you need to read multiple files, call read_file for each one in the same turn.
8. **Don't verify cd.** After `cd /path`, don't run `pwd` to check — the tool confirms the directory change.

## Behavioral rules

1. Read before editing — always understand the full context before making changes.
2. Make minimal, focused changes. Don't refactor code you weren't asked to change.
3. Use edit_file for surgical edits, not write_file (which rewrites the whole file).
4. If you encounter an error, analyze it and try a different approach.
5. Prefer existing patterns and conventions found in the codebase.
6. When asked a QUESTION (not a task), answer directly from what you know or can quickly look up. Don't create plans or strategies for Q&A.
"#,
        root = project_root.display(),
        name = project_name,
        platform = std::env::consts::OS,
        boot_listing = boot_listing,
    );

    // Tool declarations with approval annotations
    prompt.push_str("\n## Available tools\n");
    for (decl, needs_approval) in tools.declarations_annotated(approval_mode) {
        if needs_approval {
            prompt.push_str(&format!(
                "- **{}** *(requires approval)*: {}\n",
                decl.name, decl.description
            ));
        } else {
            prompt.push_str(&format!("- **{}**: {}\n", decl.name, decl.description));
        }
    }

    // Edit format
    prompt.push_str("\n## Edit format\n");
    prompt.push_str(
        "Use edit_file with exact search text and replacement. \
         Whitespace-normalized fuzzy matching is used as fallback.\n",
    );

    // Provider-specific hints
    match provider {
        ProviderKind::Anthropic | ProviderKind::AnthropicCompatible => {
            prompt.push_str("\n## Model hints\n");
            prompt.push_str(
                "You support parallel tool use — call multiple tools in a single response when possible.\n",
            );
        }
        ProviderKind::OpenAi | ProviderKind::OpenAiCompatible => {
            prompt.push_str("\n## Model hints\n");
            prompt.push_str(
                "You support parallel function calling. Use it to batch reads and searches.\n",
            );
        }
        ProviderKind::DeepSeek => {
            prompt.push_str("\n## Model hints\n");
            prompt.push_str(
                "When using your thinking capability, plan your tool calls before executing. \
                 Prefer a single well-chosen tool over a sequence of exploratory ones.\n",
            );
        }
        _ => {}
    }

    // Project conventions
    let conventions_path = project_root.join(".pipit").join("CONVENTIONS.md");
    if conventions_path.exists() {
        if let Ok(conventions) = std::fs::read_to_string(&conventions_path) {
            prompt.push_str("\n## Project conventions\n");
            prompt.push_str(&conventions);
            prompt.push_str("\n");
        }
    }

    // Skills + workflow assets
    prompt.push_str(&skills.prompt_section());
    prompt.push_str(&workflow_assets.prompt_section());

    // Knowledge injection — past experience from completed tasks
    // Injected automatically if knowledge units are available
    let knowledge_dir = project_root.join(".pipit").join("knowledge");
    if knowledge_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&knowledge_dir) {
            let mut units: Vec<knowledge_injection::InjectedKnowledge> = Vec::new();
            for entry in entries.flatten() {
                if entry.path().extension().map(|e| e == "json").unwrap_or(false) {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        if let Ok(unit) = serde_json::from_str::<knowledge_injection::InjectedKnowledge>(&content) {
                            units.push(unit);
                        }
                    }
                }
            }
            if !units.is_empty() {
                let preamble = knowledge_injection::format_knowledge_preamble(
                    &units,
                    knowledge_injection::DEFAULT_KNOWLEDGE_BUDGET_TOKENS,
                );
                if !preamble.is_empty() {
                    prompt.push_str(&preamble);
                }
            }
        }
    }

    prompt
}

/// Generate a compact top-level listing of the project root.
/// This gives the model immediate orientation without needing a list_directory call.
fn generate_boot_listing(project_root: &Path) -> String {
    let mut entries = Vec::new();

    let Ok(read_dir) = std::fs::read_dir(project_root) else {
        return "(could not read project root)".to_string();
    };

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip hidden files except important ones
        if name.starts_with('.')
            && !matches!(
                name.as_str(),
                ".pipit" | ".github" | ".gitignore" | ".env" | ".env.example"
            )
        {
            continue;
        }
        // Skip build artifacts
        if matches!(
            name.as_str(),
            "node_modules" | "target" | "__pycache__" | ".next" | "dist" | "build" | "venv" | ".venv"
        ) {
            continue;
        }

        if entry.path().is_dir() {
            let subcount = std::fs::read_dir(entry.path())
                .map(|rd| rd.count())
                .unwrap_or(0);
            dirs.push(format!("  {}/  ({} items)", name, subcount));
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let size_str = if size < 1024 {
                format!("{}B", size)
            } else if size < 1024 * 1024 {
                format!("{:.0}K", size as f64 / 1024.0)
            } else {
                format!("{:.1}M", size as f64 / (1024.0 * 1024.0))
            };
            files.push(format!("  {}  ({})", name, size_str));
        }
    }

    dirs.sort();
    files.sort();

    let max_entries = 40;
    let total = dirs.len() + files.len();

    entries.push("```".to_string());
    for d in dirs.iter().take(max_entries / 2) {
        entries.push(d.clone());
    }
    for f in files.iter().take(max_entries / 2) {
        entries.push(f.clone());
    }
    if total > max_entries {
        entries.push(format!("  ... and {} more entries", total - max_entries));
    }
    entries.push("```".to_string());

    entries.join("\n")
}
