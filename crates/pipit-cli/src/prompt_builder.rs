//! Composable system prompt builder.
//!
//! Task 11 (the 10/10 fix): wires the existing `ArchitectureIR` synthesizer
//! into the system prompt and adds strategy-aware augmentation.
//!
//! Cache stability contract:
//!   - The IR is synthesized ONCE per session from the first user prompt
//!   - It is cached on the session and reused across all turns (byte-stable)
//!   - The strategy section is selected per-session from the initial plan
//!   - Everything else is unchanged from the existing prompt kernel
//!
//! This means the FULL prompt (system + tools + domain + strategy) hits the
//! prompt cache after turn 1 just like before. We are adding signal, not
//! invalidating the cache.

use pipit_config::{ApprovalMode, ProviderKind};
use pipit_context::cache_optimizer::{
    CacheBreakpoint, CacheContentType, CacheOptimizer, PromptSection,
};
use pipit_context::knowledge_injection;
use pipit_core::domain_architect::{self, ArchitectureIR, ProjectArchetype};
use pipit_core::planner::StrategyKind;
use pipit_core::prompt_kernel::{self, PromptInputs, ToolDecl};
use pipit_skills::SkillRegistry;
use pipit_tools::ToolRegistry;
use std::path::Path;

use crate::workflow::WorkflowAssets;

/// **Deprecated: use `build_composed_prompt()` instead, which routes through
/// the typed prompt kernel for section-level caching and selective replacement.**
///
/// Returns `(system_prompt, boot_listing)`. Internally delegates to
/// `build_composed_prompt()` so both paths produce identical output.
#[deprecated(note = "use build_composed_prompt() which routes through the typed prompt kernel")]
#[allow(deprecated)]
pub fn build_system_prompt(
    project_root: &Path,
    tools: &ToolRegistry,
    approval_mode: ApprovalMode,
    provider: ProviderKind,
    skills: &SkillRegistry,
    workflow_assets: &WorkflowAssets,
) -> (String, String) {
    let (assembled, boot_listing) = build_composed_prompt(
        project_root,
        tools,
        approval_mode,
        provider,
        skills,
        workflow_assets,
    );
    (assembled.materialize(), boot_listing)
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
/// **Deprecated: use `build_composed_prompt()` and convert its `AssembledPrompt`
/// sections to cache sections instead of re-parsing the monolithic string.**
///
/// Returns `(sections, full_prompt, boot_listing)`. Pass `sections` to
/// `CacheOptimizer::analyze_request()` to get cache breakpoint placements
/// for the Anthropic `cache_control` API parameter.
#[deprecated(note = "use build_composed_prompt() and convert AssembledPrompt sections directly")]
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

/// Inputs for the new composable prompt path.
///
/// `initial_user_prompt` and `selected_strategy` are the NEW fields that enable
/// domain synthesis and strategy-aware guidance. Both are Optional so existing
/// callers that don't yet have them degrade gracefully to the pre-Task-11 behavior.
pub struct BuildPromptInputs<'a> {
    pub project_root: &'a Path,
    pub tools: &'a ToolRegistry,
    pub approval_mode: ApprovalMode,
    pub provider: ProviderKind,
    pub skills: &'a SkillRegistry,
    pub workflow_assets: &'a WorkflowAssets,
    /// The user's first prompt in this session. Used for ArchitectureIR synthesis.
    /// If `None`, domain synthesis is skipped (graceful fallback).
    pub initial_user_prompt: Option<&'a str>,
    /// The strategy selected by the planner. Drives strategy-aware augmentation.
    /// If `None`, generic guidance is used.
    pub selected_strategy: Option<StrategyKind>,
    /// Model context window in tokens. Passed through to the prompt kernel so
    /// it can emit compact guidelines for constrained models.
    pub context_window: Option<u64>,
}

/// Build a system prompt using the composable prompt assembly kernel.
///
/// Returns `(assembled_prompt, boot_listing, synthesized_ir)`.
/// The `synthesized_ir` is returned so the AgentLoop can cache it and pass
/// excerpts to subagent briefings.
pub fn build_composed_prompt_v2(
    inputs: BuildPromptInputs<'_>,
) -> (prompt_kernel::AssembledPrompt, String, Option<ArchitectureIR>) {
    let boot_listing = generate_boot_listing(inputs.project_root);
    let project_instructions = load_project_instructions(inputs.project_root);

    // Load conventions
    let conventions_path = inputs.project_root.join(".pipit").join("CONVENTIONS.md");
    let conventions = if conventions_path.exists() {
        std::fs::read_to_string(&conventions_path).ok()
    } else {
        None
    };

    // Build tool declarations
    let tool_decls: Vec<ToolDecl> = inputs
        .tools
        .declarations_annotated(inputs.approval_mode)
        .into_iter()
        .map(|(decl, needs_approval)| ToolDecl {
            name: decl.name,
            description: decl.description,
            requires_approval: needs_approval,
        })
        .collect();

    // Load knowledge
    let knowledge_section = load_knowledge_section(inputs.project_root);

    // ────────────────────────────────────────────────────────────────────
    // THE FIX: synthesize the ArchitectureIR from the initial user prompt
    // and render it as the DomainArchitecture section.
    //
    // This is the line that closes the integration gap. pipit already has
    // domain_architect::synthesize() — it was just never called here.
    // ────────────────────────────────────────────────────────────────────
    let (domain_architecture_section, synthesized_ir) = match inputs.initial_user_prompt {
        Some(prompt) if !prompt.trim().is_empty() => {
            let ir = domain_architect::synthesize(prompt);
            // Only inject the section if synthesis found actual structure.
            // Empty IR (no entities, no interfaces) adds noise without signal.
            let section = if ir.entities.is_empty() && ir.interfaces.is_empty() {
                None
            } else {
                Some(render_architecture_ir(&ir))
            };
            (section, Some(ir))
        }
        _ => (None, None),
    };

    // ────────────────────────────────────────────────────────────────────
    // Strategy guidance — translate the StrategyKind label into concrete
    // behavioral rules.
    // ────────────────────────────────────────────────────────────────────
    let strategy_section = inputs
        .selected_strategy
        .as_ref()
        .map(strategy_guidance_for)
        .filter(|s| !s.is_empty())
        .map(|s| prompt_kernel::PromptSection::new(
            prompt_kernel::SectionId::Custom("strategy_guidance".into()),
            s,
        ));

    let mut custom_sections = Vec::new();
    if let Some(s) = strategy_section {
        custom_sections.push(s);
    }

    let prompt_inputs = PromptInputs {
        project_root: Some(inputs.project_root.to_path_buf()),
        project_name: None,
        tools: tool_decls,
        provider_hint: provider_hint_text(inputs.provider).map(String::from),
        project_instructions,
        conventions,
        skills_section: Some({
            // Use 1% of context window for skill listing budget (4 chars ≈ 1 token)
            let budget_chars = inputs
                .context_window
                .map(|cw| (cw as usize / 100) * 4) // 1% of tokens × 4 chars/token
                .unwrap_or(3200);
            inputs.skills.prompt_section_with_budget(budget_chars.max(800))
        }),
        workflow_section: Some(inputs.workflow_assets.prompt_section()),
        knowledge_section,
        memory_section: None,
        domain_architecture_section,
        custom_sections,
        exclude_sections: Vec::new(),
        override_sections: std::collections::HashMap::new(),
        context_window: inputs.context_window,
    };

    let assembled = prompt_kernel::assemble(&prompt_inputs);
    (assembled, boot_listing, synthesized_ir)
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
    build_composed_prompt_with_context(
        project_root, tools, approval_mode, provider, skills, workflow_assets, None,
    )
}

/// Like `build_composed_prompt` but accepts a context_window size so that the
/// prompt kernel can emit a compact prompt for constrained models.
pub fn build_composed_prompt_with_context(
    project_root: &Path,
    tools: &ToolRegistry,
    approval_mode: ApprovalMode,
    provider: ProviderKind,
    skills: &SkillRegistry,
    workflow_assets: &WorkflowAssets,
    context_window: Option<u64>,
) -> (prompt_kernel::AssembledPrompt, String) {
    let (assembled, boot, _) = build_composed_prompt_v2(BuildPromptInputs {
        project_root,
        tools,
        approval_mode,
        provider,
        skills,
        workflow_assets,
        initial_user_prompt: None,
        selected_strategy: None,
        context_window,
    });
    (assembled, boot)
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
        ProviderKind::OpenAiResponses
        | ProviderKind::CodexOAuth => Some(
            "You support parallel function calling. Use it to batch reads and searches.",
        ),
        ProviderKind::CopilotOAuth => Some(
            "You support parallel function calling. Use it to batch reads and searches.",
        ),
        ProviderKind::Faux => None,
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

// ═══════════════════════════════════════════════════════════════════════════
//  Rendering — ArchitectureIR to prompt section
// ═══════════════════════════════════════════════════════════════════════════

/// Render an ArchitectureIR into a cache-stable markdown section.
///
/// Output is sorted by (entity name), (relation.from, relation.to), etc. so
/// identical IRs always produce byte-identical output. This preserves the
/// prompt cache across turns.
pub fn render_architecture_ir(ir: &ArchitectureIR) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str("## Domain architecture (canonical contract)\n\n");
    out.push_str(
        "This is the ONE canonical domain model for this session. Every artifact \
         you produce — schema, API, frontend, seeds, admin — must conform to these \
         entities, relations, and invariants. Do NOT invent alternative shapes in \
         different parts of the codebase.\n\n",
    );

    // Archetype
    if let Some(arch) = &ir.archetype {
        out.push_str(&format!("**Archetype:** {}\n\n", archetype_name(arch)));
        out.push_str(&domain_architect::archetype_guidance(arch));
        out.push_str("\n\n");
    }

    // Entities (sorted for cache stability)
    if !ir.entities.is_empty() {
        out.push_str("### Entities\n\n");
        let mut sorted_entities = ir.entities.clone();
        sorted_entities.sort_by(|a, b| a.name.cmp(&b.name));
        for e in &sorted_entities {
            out.push_str(&format!("- **{}**", e.name));
            if e.is_primary {
                out.push_str(" (primary)");
            }
            if !e.attributes.is_empty() {
                let mut attrs = e.attributes.clone();
                attrs.sort();
                out.push_str(&format!(" — attributes: {}", attrs.join(", ")));
            }
            out.push('\n');
        }
        out.push('\n');
    }

    // Relations
    if !ir.relations.is_empty() {
        out.push_str("### Relations\n\n");
        let mut sorted_rels = ir.relations.clone();
        sorted_rels.sort_by(|a, b| {
            (a.from.clone(), a.to.clone()).cmp(&(b.from.clone(), b.to.clone()))
        });
        for r in &sorted_rels {
            out.push_str(&format!(
                "- `{}` {} `{}` — {}\n",
                r.from, r.kind, r.to, r.description
            ));
        }
        out.push('\n');
    }

    // Invariants
    if !ir.invariants.is_empty() {
        out.push_str("### Invariants (must hold across all artifacts)\n\n");
        let mut sorted_inv = ir.invariants.clone();
        sorted_inv.sort_by(|a, b| a.description.cmp(&b.description));
        for inv in &sorted_inv {
            out.push_str(&format!("- {}\n", inv.description));
        }
        out.push('\n');
    }

    // Interfaces
    if !ir.interfaces.is_empty() {
        out.push_str("### API surface\n\n");
        let mut sorted_ifaces = ir.interfaces.clone();
        sorted_ifaces.sort_by(|a, b| {
            (a.method.clone(), a.path.clone()).cmp(&(b.method.clone(), b.path.clone()))
        });
        for iface in &sorted_ifaces {
            out.push_str(&format!(
                "- `{} {}` — {}\n",
                iface.method, iface.path, iface.description
            ));
        }
        out.push('\n');
    }

    // Workflows
    if !ir.workflows.is_empty() {
        out.push_str("### Workflows\n\n");
        let mut sorted_wf = ir.workflows.clone();
        sorted_wf.sort_by(|a, b| a.name.cmp(&b.name));
        for wf in &sorted_wf {
            out.push_str(&format!("- **{}**: {}\n", wf.name, wf.steps.join(" → ")));
        }
        out.push('\n');
    }

    // Integration rules
    out.push_str(
        "### Integration rules (non-negotiable)\n\n\
         1. The seed data MUST conform to the entity schemas above. A seeded `User` \
            with a `createdAt` field but a schema column `created_at` is a bug.\n\
         2. The frontend fetch paths MUST match the API paths above verbatim. \
            `/api/users` and `/api/user` are NOT interchangeable.\n\
         3. The admin auth middleware MUST use the same token shape as the public auth.\n\
         4. Every entity listed above gets ONE table with a consistent primary key \
            type across the whole schema.\n\
         5. Before declaring a task complete, run the integration verification step \
            that checks artifact-to-artifact consistency (see strategy guidance).\n\n",
    );

    out
}

fn archetype_name(a: &ProjectArchetype) -> &'static str {
    match a {
        ProjectArchetype::CrudWebApp => "CRUD Web App",
        ProjectArchetype::FullStackWeb => "Full-Stack Web",
        ProjectArchetype::RestApi => "REST API",
        ProjectArchetype::CliTool => "CLI Tool",
        ProjectArchetype::Library => "Library",
        ProjectArchetype::EventDriven => "Event-Driven",
        ProjectArchetype::DataPipeline => "Data Pipeline",
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Strategy guidance — behavioral rules per StrategyKind
// ═══════════════════════════════════════════════════════════════════════════

/// Concrete behavioral rules for each strategy.
fn strategy_guidance_for(strategy: &StrategyKind) -> String {
    match strategy {
        StrategyKind::Greenfield => greenfield_guidance(),
        StrategyKind::MinimalPatch => minimal_patch_guidance(),
        StrategyKind::RootCauseRepair => root_cause_guidance(),
        StrategyKind::CharacterizationFirst => characterization_guidance(),
        StrategyKind::ArchitecturalRepair => architectural_repair_guidance(),
        StrategyKind::DiagnosticOnly => diagnostic_only_guidance(),
    }
}

fn greenfield_guidance() -> String {
    r#"## Strategy: Greenfield (build from scratch)

### Integration discipline comes first

This is a greenfield build. The canonical domain model in the section above is
the contract. Before you write any code, internalize it — you will be judged on
whether the pieces fit together, not on whether individual pieces are polished.

### Execution order (non-negotiable)

1. **Schema first.** Create the database schema/migrations matching the Entities
   section exactly. Use one table per entity, one consistent primary-key type,
   snake_case columns, `created_at`/`updated_at` timestamps.

2. **API contract second.** Implement every endpoint in the API surface section.
   Use the EXACT paths and methods listed. Do not add or rename endpoints.

3. **Seed data third.** Generate seed data that EXERCISES every relation.
   If User 1→N Post, seed at least one user with multiple posts. If seeds
   don't prove the relations work, they don't count.

4. **Frontend fourth.** Fetch from the API paths above verbatim. If the API
   returns `{ id, created_at }`, the frontend reads `created_at`, not `createdAt`.

5. **Admin fifth.** Reuse the auth middleware from step 2. Do not implement a
   second auth mechanism.

6. **Integration verification last.** Run the end-to-end check: seed the DB,
   start the server, hit every endpoint, load the frontend, verify admin access.

### Rules specific to greenfield

- **Do NOT delegate structural work to subagents.** Subagent contexts don't see
  your running schema decisions. Delegate only: (a) enumeration, (b) bulk file
  writes that follow a shape you've already committed to, (c) independent
  parallel work where the contract is already fixed.
- **When you DO delegate, include the relevant excerpt of the domain architecture
  in the subagent briefing.** Never say "implement the User endpoints" — say
  "implement the User endpoints per this schema: { ... }".
- **Hold the schema in your own head, not in subagents' heads.** The coordinator
  is the source of truth.
- **Use the `todo` tool for the six-step plan above.** Mark the integration
  verification as a task; do not ship a done list without it.
- **Keep files small and cohesive.** One route file per resource, one model file
  per entity. Greenfield is the moment to set good structure.
"#
    .to_string()
}

fn minimal_patch_guidance() -> String {
    r#"## Strategy: MinimalPatch (smallest change that satisfies the goal)

- Make the narrowest change that could possibly work.
- Do not refactor adjacent code, rename things, or "improve" unrelated files.
- Every file you touch must be essential to the objective.
- Verify with the narrowest test or command that exercises the behavior.
- If the minimal patch doesn't work after one try, STOP. Don't escalate to
  wider rewrites — ask for guidance or switch strategies explicitly.
"#
    .to_string()
}

fn root_cause_guidance() -> String {
    r#"## Strategy: RootCauseRepair (understand before editing)

- Read the implementation and surrounding tests BEFORE mutating anything.
- State the root cause in one sentence before you write the fix.
- If you cannot articulate the root cause, you haven't understood the problem
  yet — keep reading.
- The fix addresses the cause, not the symptom. A 500 becoming a 401 is a
  symptom fix; fixing the JWT validator is a root-cause fix.
- Verify by reproducing the original failure, applying the fix, and confirming
  the failure no longer reproduces.
"#
    .to_string()
}

fn characterization_guidance() -> String {
    r#"## Strategy: CharacterizationFirst (stabilize behavior, then repair)

- Run the documented examples or existing tests FIRST to capture current behavior.
- Write down what passes and what fails BEFORE changing anything.
- Make the change.
- Re-run the same checks. The delta is your evidence.
- If the characterization step reveals the bug doesn't reproduce, STOP and
  re-read the original report — you may be looking at the wrong thing.
"#
    .to_string()
}

fn architectural_repair_guidance() -> String {
    r#"## Strategy: ArchitecturalRepair (structural change)

- The objective requires a structural change. This is higher-risk than a patch.
- Plan the new structure BEFORE making any edits — write it to a todo list.
- Do the structural change in one coherent commit, not drip-fed edits.
- Verify that public behavior is unchanged: the same inputs produce the same
  outputs.
- Isolated worktree is strongly recommended for architectural repair — pass
  `isolated: true` to any subagent doing the work.
"#
    .to_string()
}

fn diagnostic_only_guidance() -> String {
    r#"## Strategy: DiagnosticOnly (collect evidence, do not mutate)

- You are in diagnostic mode. DO NOT edit or write files.
- Gather evidence with read-only tools (read, grep, list, bash read-only).
- Report your findings as a structured diagnosis with:
  - What the symptom is
  - What you observed
  - What the likely root cause is
  - What options exist to fix it
  - Which option you would recommend and why
- The user (or a subsequent agent) decides whether to proceed.
"#
    .to_string()
}
