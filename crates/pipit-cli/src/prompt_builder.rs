use pipit_config::{ApprovalMode, ProviderKind};
use pipit_context::cache_optimizer::{
    CacheBreakpoint, CacheContentType, CacheOptimizer, PromptSection,
};
use pipit_context::knowledge_injection;
use pipit_core::prompt_kernel::{self, PromptInputs, SectionId, ToolDecl};
use pipit_skills::SkillRegistry;
use pipit_tools::ToolRegistry;
use std::path::Path;

use crate::workflow::WorkflowAssets;

/// Sanitize user-controlled content before injection into the system prompt.
/// Wraps content in XML delimiter tags and escapes XML special characters
/// to prevent prompt injection via memory, skill, or knowledge files.
fn sanitize_injected_content(content: &str, source: &str) -> String {
    let escaped = content
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<injected_content source=\"{}\">\n{}\n</injected_content>",
        source, escaped
    )
}

/// Build the system prompt from composable sections.
///
/// Includes tool selection heuristics, efficiency maxims, boot orientation,
/// and provider-specific hints to minimize unnecessary tool calls.
///
/// Returns `(system_prompt, boot_listing)`. The boot listing should be
/// injected as the first user message (turn-1 context) rather than baked
/// into the system prompt — this keeps the system prompt cache-stable
/// across sessions within the same project.
pub fn build_system_prompt(
    project_root: &Path,
    tools: &ToolRegistry,
    approval_mode: ApprovalMode,
    provider: ProviderKind,
    skills: &SkillRegistry,
    workflow_assets: &WorkflowAssets,
) -> (String, String) {
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

**Tracking multi-step work → `todo`**
  Use todo for any task with multiple concrete steps, files, or verification tasks.
  Create a short checklist as soon as the work stops being trivial.
  Keep statuses accurate (`pending` → `in_progress` → `done`) as you progress.
  Do NOT use todo for one-shot answers or single quick edits.

**Delegating independent work → `subagent`**
  Use subagent only for bounded, parallelizable side tasks that do NOT block your
  immediate next step. Good examples: isolated investigation, independent test
  authoring, or reviewing a separate module while you continue locally.
  Do NOT spawn a subagent just to do your first read, to replace normal tool use,
  or when the very next action depends on its answer.
  Prefer at most 1-2 active subagents at a time, and record delegated work in todo.

## Efficiency rules

1. **Minimize turns.** Each tool call costs a full round-trip. Accomplish the task in as few turns as possible.
2. **Don't wander.** If you know the path, go directly. Don't list_directory then read_file — just read_file. Don't run `pwd` — you know the working directory from the environment section above.
3. **Don't re-read.** Once a file's content is in your context, don't read it again unless it was modified.
4. **Don't narrate tool calls.** Don't say "Let me search for the file" before searching. Just search. Don't explain what shell commands do. Just run them and interpret the output.
5. **Don't apologize or hedge.** Don't say "I'll try to..." or "Let me attempt...". State what you're doing and do it.
6. **Use the structure.** You have the project listing above. Use it to navigate directly instead of exploring blindly.
7. **Batch when possible.** If you need to read multiple files, call read_file for each one in the same turn.
8. **Don't verify cd.** After `cd /path`, don't run `pwd` to check — the tool confirms the directory change.
9. **Track real work.** For non-trivial implementation tasks, use `todo` instead of keeping the plan implicit in prose.
10. **Delegate surgically.** Spawn `subagent` only when the subtask is truly independent and worth parallelizing.

## Response formatting

Use markdown in your responses for readability:
- Use **bold** for emphasis and `backticks` for code symbols, paths, and commands.
- Use headers (## / ###) to organize multi-section responses.
- Use bullet lists or numbered lists for sequential steps or multiple items.
- Use fenced code blocks (```) with language tags for code snippets.
- Keep paragraphs short and separated by blank lines.
- Your response must be the user-facing answer only, not a work log.
- Do not write internal planning like "I need to check", "I'll inspect", or "I've read".
- Do not narrate turns, tool calls, or file reads in the response body unless the user explicitly asks for that transcript.
- For simple Q&A, prefer a short answer with bullets or short paragraphs over a step-by-step narrative.

## Behavioral rules

1. Read before editing — always understand the full context before making changes.
2. Make minimal, focused changes. Don't refactor code you weren't asked to change.
3. Use edit_file for surgical edits, not write_file (which rewrites the whole file).
4. If you encounter an error, analyze it and try a different approach.
5. Prefer existing patterns and conventions found in the codebase.
6. When asked a QUESTION (not a task), answer directly from what you know or can quickly look up. Don't create plans or strategies for Q&A.
7. When the task spans several steps, create and maintain a `todo` list before diving into execution.
8. Before delegating with `subagent`, verify that you can keep making progress locally while it runs.
"#,
        root = project_root.display(),
        name = project_name,
        platform = std::env::consts::OS,
    );

    // PIPIT.md or CLAUDE.md — hierarchical instruction loading.
    //
    // Walk up from project_root to the filesystem root (or $HOME, whichever
    // comes first) collecting instruction files. Files closer to the project
    // root take priority. This supports monorepo structures where a root-level
    // CLAUDE.md provides org-wide instructions and subdirectory PIPIT.md adds
    // project-specific context.
    //
    // Order: project root is injected LAST (highest priority, closest to code).
    // Parent instructions are injected first (broadest context).
    let home = std::env::var("HOME").ok().map(std::path::PathBuf::from);
    let instruction_names = ["PIPIT.md", "CLAUDE.md"];
    let instruction_dirs = [".pipit", ".claude"];

    let mut ancestor_instructions: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut current = project_root.to_path_buf();
    loop {
        for name in &instruction_names {
            let candidate = current.join(name);
            if candidate.exists() {
                if let Ok(content) = std::fs::read_to_string(&candidate) {
                    ancestor_instructions.push((candidate, content));
                    break; // Only one instruction file per directory level
                }
            }
        }
        // Also check dotdirs (.pipit/PIPIT.md, .claude/CLAUDE.md)
        if ancestor_instructions
            .last()
            .map(|(p, _)| p.parent() != Some(&current))
            .unwrap_or(true)
        {
            for dir in &instruction_dirs {
                for name in &instruction_names {
                    let candidate = current.join(dir).join(name);
                    if candidate.exists() {
                        if let Ok(content) = std::fs::read_to_string(&candidate) {
                            ancestor_instructions.push((candidate, content));
                            break;
                        }
                    }
                }
                if ancestor_instructions
                    .last()
                    .map(|(p, _)| p.starts_with(&current.join(dir)))
                    .unwrap_or(false)
                {
                    break;
                }
            }
        }

        // Stop at home directory or filesystem root
        if let Some(ref home_dir) = home {
            if current == *home_dir {
                break;
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => break,
        }
    }

    // Also check user-global instruction file (~/.config/pipit/PIPIT.md)
    if let Some(ref home_dir) = home {
        let global_candidate = home_dir.join(".config").join("pipit").join("PIPIT.md");
        if global_candidate.exists() {
            if let Ok(content) = std::fs::read_to_string(&global_candidate) {
                ancestor_instructions.push((global_candidate, content));
            }
        }
    }

    // Inject in reverse order: broadest (most distant) first, project-local last
    ancestor_instructions.reverse();
    // Deduplicate by canonical path
    let mut seen_paths = std::collections::HashSet::new();
    // Cap instruction content to prevent unbounded prompt inflation.
    // 8000 chars ≈ ~2000 tokens — enough for meaningful instructions without starving context.
    const INSTRUCTION_MAX_CHARS: usize = 8000;
    for (path, content) in &ancestor_instructions {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen_paths.insert(canonical) {
            let rel = path
                .strip_prefix(project_root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| path.display().to_string());
            prompt.push_str(&format!("\n## Project instructions ({})\n\n", rel));
            let truncated = if content.len() > INSTRUCTION_MAX_CHARS {
                &content[..INSTRUCTION_MAX_CHARS]
            } else {
                content.as_str()
            };
            prompt.push_str(&sanitize_injected_content(truncated, &rel));
            prompt.push_str("\n");
        }
    }

    // Tool declarations — removed from system prompt.
    // Tool names and descriptions are already in the `tools` array of the API request.
    // Duplicating them here wastes tokens without adding signal.

    // Edit format
    prompt.push_str("\n## Edit format\n");
    prompt.push_str(
        "Use edit_file with exact search text and replacement. \
         Whitespace-normalized fuzzy matching is used as fallback.\n",
    );

    // Provider-specific hints
    // Provider-specific hints — covers all major providers and compatibility modes.
    // Providers using the same transport share the same hints.
    let hint = match provider {
        ProviderKind::Anthropic
        | ProviderKind::AnthropicCompatible
        | ProviderKind::AmazonBedrock
        | ProviderKind::MiniMax
        | ProviderKind::MiniMaxCn => Some(
            "You support parallel tool use — call multiple tools in a single response when possible.",
        ),
        ProviderKind::OpenAi
        | ProviderKind::OpenAiCompatible
        | ProviderKind::AzureOpenAi
        | ProviderKind::GitHubCopilot
        | ProviderKind::OpenRouter
        | ProviderKind::VercelAiGateway
        | ProviderKind::HuggingFace
        | ProviderKind::Cerebras
        | ProviderKind::Groq
        | ProviderKind::Mistral
        | ProviderKind::XAi
        | ProviderKind::ZAi
        | ProviderKind::Ollama
        | ProviderKind::OpenAiCodex
        | ProviderKind::Opencode
        | ProviderKind::OpencodeGo
        | ProviderKind::KimiCoding => Some(
            "You support parallel function calling. Use it to batch reads and searches.",
        ),
        ProviderKind::Google
        | ProviderKind::GoogleGeminiCli
        | ProviderKind::GoogleAntigravity
        | ProviderKind::Vertex => Some(
            "You support parallel function calling. Batch tools aggressively — reads, searches, and edits can all be issued together.",
        ),
        ProviderKind::DeepSeek => Some(
            "When using your thinking capability, plan your tool calls before executing. \
             Prefer a single well-chosen tool over a sequence of exploratory ones.",
        ),
    };
    if let Some(hint_text) = hint {
        prompt.push_str("\n## Model hints\n");
        prompt.push_str(hint_text);
        prompt.push('\n');
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
                if entry
                    .path()
                    .extension()
                    .map(|e| e == "json")
                    .unwrap_or(false)
                {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        if let Ok(unit) =
                            serde_json::from_str::<knowledge_injection::InjectedKnowledge>(&content)
                        {
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

    (prompt, boot_listing)
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
            "node_modules"
                | "target"
                | "__pycache__"
                | ".next"
                | "dist"
                | "build"
                | "venv"
                | ".venv"
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

/// Decompose the system prompt into sections for cache optimizer analysis.
///
/// Returns `(sections, full_prompt, boot_listing)`. Pass `sections` to
/// `CacheOptimizer::analyze_request()` to get cache breakpoint placements
/// for the Anthropic `cache_control` API parameter.
pub fn build_prompt_with_cache_sections(
    project_root: &Path,
    tools: &ToolRegistry,
    approval_mode: ApprovalMode,
    provider: ProviderKind,
    skills: &SkillRegistry,
    workflow_assets: &WorkflowAssets,
) -> (Vec<PromptSection>, String, String) {
    // Build the full prompt first
    let (full_prompt, boot_listing) = build_system_prompt(
        project_root,
        tools,
        approval_mode,
        provider,
        skills,
        workflow_assets,
    );

    // Decompose into sections for cache analysis
    let mut sections = Vec::new();

    // Section 1: System prompt (everything before edit format)
    // The tool declarations section has been removed from the system prompt
    // (tools are in the API request body). Split on Edit format instead.
    let tool_marker = "\n## Edit format\n";
    let (system_part, remaining) = match full_prompt.find(tool_marker) {
        Some(pos) => (&full_prompt[..pos], &full_prompt[pos..]),
        None => (full_prompt.as_str(), ""),
    };
    sections.push(PromptSection {
        content_type: CacheContentType::SystemPrompt,
        content: system_part.to_string(),
    });

    // Section 2: Remaining sections (edit format, memory, knowledge, etc.)
    if !remaining.is_empty() {
        sections.push(PromptSection {
            content_type: CacheContentType::Memory,
            content: remaining.to_string(),
        });
    }

    (sections, full_prompt, boot_listing)
}

/// Build a system prompt using the composable prompt assembly kernel.
///
/// This is the preferred API for new surfaces. It returns typed sections
/// that support section-level replacement, exclusion, and cache invalidation.
pub fn build_composed_prompt(
    project_root: &Path,
    tools: &ToolRegistry,
    approval_mode: ApprovalMode,
    provider: ProviderKind,
    skills: &SkillRegistry,
    workflow_assets: &WorkflowAssets,
) -> (prompt_kernel::AssembledPrompt, String) {
    let boot_listing = generate_boot_listing(project_root);

    // Load project instructions
    let project_instructions = load_project_instructions(project_root);

    // Load conventions
    let conventions_path = project_root.join(".pipit").join("CONVENTIONS.md");
    let conventions = if conventions_path.exists() {
        std::fs::read_to_string(&conventions_path).ok()
    } else {
        None
    };

    // Build tool declarations
    let tool_decls: Vec<ToolDecl> = tools
        .declarations_annotated(approval_mode)
        .into_iter()
        .map(|(decl, needs_approval)| ToolDecl {
            name: decl.name,
            description: decl.description,
            requires_approval: needs_approval,
        })
        .collect();

    // Load knowledge
    let knowledge_section = load_knowledge_section(project_root);

    let inputs = PromptInputs {
        project_root: Some(project_root.to_path_buf()),
        project_name: None,
        tools: tool_decls,
        provider_hint: provider_hint_text(provider).map(String::from),
        project_instructions,
        conventions,
        skills_section: Some(skills.prompt_section()),
        workflow_section: Some(workflow_assets.prompt_section()),
        knowledge_section,
        memory_section: None,
        custom_sections: Vec::new(),
        exclude_sections: Vec::new(),
        override_sections: std::collections::HashMap::new(),
    };

    let assembled = prompt_kernel::assemble(&inputs);
    (assembled, boot_listing)
}

/// Load project instruction files (PIPIT.md / CLAUDE.md) walking up the directory tree.
fn load_project_instructions(project_root: &Path) -> Vec<(String, String)> {
    let home = std::env::var("HOME").ok().map(std::path::PathBuf::from);
    let instruction_names = ["PIPIT.md", "CLAUDE.md"];
    let instruction_dirs = [".pipit", ".claude"];

    let mut ancestor_instructions: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut current = project_root.to_path_buf();
    loop {
        for name in &instruction_names {
            let candidate = current.join(name);
            if candidate.exists() {
                if let Ok(content) = std::fs::read_to_string(&candidate) {
                    ancestor_instructions.push((candidate, content));
                    break;
                }
            }
        }
        if ancestor_instructions
            .last()
            .map(|(p, _)| p.parent() != Some(&current))
            .unwrap_or(true)
        {
            for dir in &instruction_dirs {
                for name in &instruction_names {
                    let candidate = current.join(dir).join(name);
                    if candidate.exists() {
                        if let Ok(content) = std::fs::read_to_string(&candidate) {
                            ancestor_instructions.push((candidate, content));
                            break;
                        }
                    }
                }
                if ancestor_instructions
                    .last()
                    .map(|(p, _)| p.starts_with(&current.join(dir)))
                    .unwrap_or(false)
                {
                    break;
                }
            }
        }

        if let Some(ref home_dir) = home {
            if current == *home_dir {
                break;
            }
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent.to_path_buf(),
            _ => break,
        }
    }

    if let Some(ref home_dir) = home {
        let global_candidate = home_dir.join(".config").join("pipit").join("PIPIT.md");
        if global_candidate.exists() {
            if let Ok(content) = std::fs::read_to_string(&global_candidate) {
                ancestor_instructions.push((global_candidate, content));
            }
        }
    }

    ancestor_instructions.reverse();
    let mut seen_paths = std::collections::HashSet::new();
    let mut result = Vec::new();
    for (path, content) in &ancestor_instructions {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if seen_paths.insert(canonical) {
            let rel = path
                .strip_prefix(project_root)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| path.display().to_string());
            result.push((rel, content.clone()));
        }
    }
    result
}

/// Load knowledge injection section from .pipit/knowledge/*.json.
fn load_knowledge_section(project_root: &Path) -> Option<String> {
    let knowledge_dir = project_root.join(".pipit").join("knowledge");
    if !knowledge_dir.exists() {
        return None;
    }
    let entries = std::fs::read_dir(&knowledge_dir).ok()?;
    let mut units: Vec<knowledge_injection::InjectedKnowledge> = Vec::new();
    for entry in entries.flatten() {
        if entry
            .path()
            .extension()
            .map(|e| e == "json")
            .unwrap_or(false)
        {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if let Ok(unit) =
                    serde_json::from_str::<knowledge_injection::InjectedKnowledge>(&content)
                {
                    units.push(unit);
                }
            }
        }
    }
    if units.is_empty() {
        return None;
    }
    let preamble = knowledge_injection::format_knowledge_preamble(
        &units,
        knowledge_injection::DEFAULT_KNOWLEDGE_BUDGET_TOKENS,
    );
    if preamble.is_empty() {
        None
    } else {
        Some(preamble)
    }
}

/// Get the provider-specific hint text.
fn provider_hint_text(provider: ProviderKind) -> Option<&'static str> {
    match provider {
        ProviderKind::Anthropic
        | ProviderKind::AnthropicCompatible
        | ProviderKind::AmazonBedrock
        | ProviderKind::MiniMax
        | ProviderKind::MiniMaxCn => Some(
            "You support parallel tool use — call multiple tools in a single response when possible.",
        ),
        ProviderKind::OpenAi
        | ProviderKind::OpenAiCompatible
        | ProviderKind::AzureOpenAi
        | ProviderKind::GitHubCopilot
        | ProviderKind::OpenRouter
        | ProviderKind::VercelAiGateway
        | ProviderKind::HuggingFace
        | ProviderKind::Cerebras
        | ProviderKind::Groq
        | ProviderKind::Mistral
        | ProviderKind::XAi
        | ProviderKind::ZAi
        | ProviderKind::Ollama
        | ProviderKind::OpenAiCodex
        | ProviderKind::Opencode
        | ProviderKind::OpencodeGo
        | ProviderKind::KimiCoding => Some(
            "You support parallel function calling. Use it to batch reads and searches.",
        ),
        ProviderKind::Google
        | ProviderKind::GoogleGeminiCli
        | ProviderKind::GoogleAntigravity
        | ProviderKind::Vertex => Some(
            "You support parallel function calling. Batch tools aggressively — reads, searches, and edits can all be issued together.",
        ),
        ProviderKind::DeepSeek => Some(
            "When using your thinking capability, plan your tool calls before executing. \
             Prefer a single well-chosen tool over a sequence of exploratory ones.",
        ),
    }
}

/// Analyze prompt sections and return cache breakpoints.
///
/// Call this once per turn, passing the same optimizer across turns.
/// Returns breakpoints suitable for Anthropic's `cache_control` parameter.
pub fn compute_cache_breakpoints(
    optimizer: &mut CacheOptimizer,
    sections: &[PromptSection],
) -> Vec<CacheBreakpoint> {
    optimizer.analyze_request(sections)
}
