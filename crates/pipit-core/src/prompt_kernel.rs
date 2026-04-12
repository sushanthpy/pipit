//! Composable Prompt Assembly Kernel
//!
//! Decouples prompt construction into typed sections with explicit composition
//! inputs. The same substrate serves CLI, SDK, background, and extension-driven
//! surfaces without string surgery.
//!
//! Design: immutable section composition with content-addressed invalidation.
//! Change propagation is O(k) over changed sections rather than O(n) over
//! total prompt length.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// Identity of a prompt section — used for cache invalidation and selective replacement.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SectionId {
    /// Core identity and behavioral rules (stable across turns).
    CoreIdentity,
    /// Environment block (cwd, platform, project name).
    Environment,
    /// Tool selection heuristics.
    ToolGuide,
    /// Tool declarations list.
    ToolDeclarations,
    /// Efficiency maxims.
    EfficiencyRules,
    /// Response formatting rules.
    ResponseFormatting,
    /// Behavioral rules.
    BehavioralRules,
    /// Edit format guidance.
    EditFormat,
    /// Provider-specific model hints.
    ProviderHints,
    /// Project instructions (PIPIT.md / CLAUDE.md).
    ProjectInstructions,
    /// Project conventions (.pipit/CONVENTIONS.md).
    ProjectConventions,
    /// Skills and workflow assets.
    Skills,
    /// Knowledge injection (past experience).
    Knowledge,
    /// Memory context.
    Memory,
    /// Domain architecture analysis (synthesized from requirements).
    DomainArchitecture,
    /// Custom section injected by an embedder or extension.
    Custom(String),
}

impl fmt::Display for SectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoreIdentity => write!(f, "core_identity"),
            Self::Environment => write!(f, "environment"),
            Self::ToolGuide => write!(f, "tool_guide"),
            Self::ToolDeclarations => write!(f, "tool_declarations"),
            Self::EfficiencyRules => write!(f, "efficiency_rules"),
            Self::ResponseFormatting => write!(f, "response_formatting"),
            Self::BehavioralRules => write!(f, "behavioral_rules"),
            Self::EditFormat => write!(f, "edit_format"),
            Self::ProviderHints => write!(f, "provider_hints"),
            Self::ProjectInstructions => write!(f, "project_instructions"),
            Self::ProjectConventions => write!(f, "project_conventions"),
            Self::Skills => write!(f, "skills"),
            Self::Knowledge => write!(f, "knowledge"),
            Self::Memory => write!(f, "memory"),
            Self::DomainArchitecture => write!(f, "domain_architecture"),
            Self::Custom(name) => write!(f, "custom:{}", name),
        }
    }
}

/// A single typed prompt section with content-addressed identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSection {
    pub id: SectionId,
    pub content: String,
    /// Content hash for cache invalidation — computed lazily.
    #[serde(skip)]
    content_hash: Option<u64>,
}

impl PromptSection {
    pub fn new(id: SectionId, content: String) -> Self {
        Self {
            id,
            content,
            content_hash: None,
        }
    }

    /// Get the content hash (computed once, cached).
    pub fn content_hash(&mut self) -> u64 {
        if let Some(h) = self.content_hash {
            return h;
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.content.hash(&mut hasher);
        let h = hasher.finish();
        self.content_hash = Some(h);
        h
    }

    pub fn is_empty(&self) -> bool {
        self.content.trim().is_empty()
    }
}

/// Inputs to the prompt assembly kernel. Each field maps to one or more
/// sections. Surfaces provide only the fields they need; missing fields
/// produce empty sections that are omitted from the final prompt.
#[derive(Debug, Clone, Default)]
pub struct PromptInputs {
    /// Project root path.
    pub project_root: Option<PathBuf>,
    /// Project name override (derived from project_root if absent).
    pub project_name: Option<String>,
    /// Tool declarations: Vec<(name, description, requires_approval)>.
    pub tools: Vec<ToolDecl>,
    /// Provider kind string (for model hints).
    pub provider_hint: Option<String>,
    /// Project instruction files (path, content) — already loaded.
    pub project_instructions: Vec<(String, String)>,
    /// Project conventions text.
    pub conventions: Option<String>,
    /// Skills prompt section (pre-rendered).
    pub skills_section: Option<String>,
    /// Workflow assets prompt section (pre-rendered).
    pub workflow_section: Option<String>,
    /// Knowledge preamble (pre-rendered).
    pub knowledge_section: Option<String>,
    /// Memory context (pre-rendered).
    pub memory_section: Option<String>,
    /// Domain architecture analysis (pre-rendered from ArchitectureIR).
    pub domain_architecture_section: Option<String>,
    /// Custom appended sections from embedders.
    pub custom_sections: Vec<PromptSection>,
    /// Sections to explicitly exclude.
    pub exclude_sections: Vec<SectionId>,
    /// Override sections — replace the default content for a section ID.
    pub override_sections: HashMap<SectionId, String>,
    /// Model context window in tokens. When ≤128K, the assembler emits a
    /// compact fused prompt instead of verbose separate sections. This saves
    /// ~3K tokens of system prompt overhead for smaller/local models.
    pub context_window: Option<u64>,
}

/// A tool declaration for prompt rendering.
#[derive(Debug, Clone)]
pub struct ToolDecl {
    pub name: String,
    pub description: String,
    pub requires_approval: bool,
}

/// Sanitize user-controlled content before injection into the system prompt.
/// Wraps content in XML delimiter tags and escapes XML special characters
/// to prevent prompt injection via memory, skill, or knowledge files.
pub fn sanitize_injected_content(content: &str, source: &str) -> String {
    let escaped = content
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<injected_content source=\"{}\">\n{}\n</injected_content>",
        source, escaped
    )
}

/// The assembled prompt — a vector of typed sections with materialization.
#[derive(Debug, Clone)]
pub struct AssembledPrompt {
    sections: Vec<PromptSection>,
}

impl AssembledPrompt {
    /// Materialize the full prompt string by joining all non-empty sections.
    pub fn materialize(&self) -> String {
        let mut out = String::with_capacity(
            self.sections.iter().map(|s| s.content.len() + 2).sum(),
        );
        for section in &self.sections {
            if !section.is_empty() {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&section.content);
            }
        }
        out
    }

    /// Get sections by ID (for cache breakpoint analysis).
    pub fn sections(&self) -> &[PromptSection] {
        &self.sections
    }

    /// Get a specific section by ID.
    pub fn section(&self, id: SectionId) -> Option<&PromptSection> {
        self.sections.iter().find(|s| s.id == id)
    }

    /// Replace a section's content in-place. Returns true if the section existed.
    pub fn replace_section(&mut self, id: SectionId, content: String) -> bool {
        if let Some(section) = self.sections.iter_mut().find(|s| s.id == id) {
            section.content = content;
            section.content_hash = None;
            true
        } else {
            false
        }
    }

    /// Append a custom section.
    pub fn append_section(&mut self, section: PromptSection) {
        self.sections.push(section);
    }

    /// Remove a section by ID.
    pub fn remove_section(&mut self, id: SectionId) {
        self.sections.retain(|s| s.id != id);
    }

    /// Compute content hashes for all sections (for delta detection).
    pub fn content_hashes(&mut self) -> Vec<(SectionId, u64)> {
        self.sections
            .iter_mut()
            .map(|s| {
                let h = s.content_hash();
                (s.id.clone(), h)
            })
            .collect()
    }
}

/// Assemble a prompt from typed inputs.
///
/// This is the kernel function — it maps `PromptInputs` to a vector of
/// typed `PromptSection`s. Each section has a stable identity for cache
/// invalidation and selective replacement.
///
/// Complexity: O(k) over provided sections, not O(n) over total prompt length.
pub fn assemble(inputs: &PromptInputs) -> AssembledPrompt {
    let mut sections = Vec::with_capacity(16);
    let excluded = &inputs.exclude_sections;

    let project_name = inputs
        .project_name
        .clone()
        .or_else(|| {
            inputs
                .project_root
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(String::from)
        })
        .unwrap_or_else(|| "project".to_string());

    let project_root_display = inputs
        .project_root
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| ".".to_string());

    // ── Compact context detection ──
    // When the model's context window is ≤128K, fuse the verbose guidance
    // sections into one compact block. This trades redundant instruction
    // tokens for more room for actual conversation and tool results.
    let use_compact = inputs
        .context_window
        .map(|cw| cw <= COMPACT_CONTEXT_THRESHOLD)
        .unwrap_or(false);

    // Section: Core identity + Environment (always emitted, compact or not)
    if !excluded.contains(&SectionId::CoreIdentity) {
        let content = inputs
            .override_sections
            .get(&SectionId::CoreIdentity)
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "You are Pipit, an expert AI coding agent.\nWorking directory: {}\nProject: {}\nPlatform: {}",
                    project_root_display,
                    project_name,
                    std::env::consts::OS,
                )
            });
        sections.push(PromptSection::new(SectionId::CoreIdentity, content));
    }

    if use_compact {
        // ── COMPACT PATH: one fused section replaces 5 verbose ones ──
        // Saves ~3K tokens of system prompt overhead.
        if !excluded.contains(&SectionId::ToolGuide) {
            let content = inputs
                .override_sections
                .get(&SectionId::ToolGuide)
                .cloned()
                .unwrap_or_else(default_compact_guidelines);
            sections.push(PromptSection::new(SectionId::ToolGuide, content));
        }
    } else {
        // ── VERBOSE PATH: full sections for large-context models ──

    // Section: Environment
    if !excluded.contains(&SectionId::Environment) {
        let content = inputs
            .override_sections
            .get(&SectionId::Environment)
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "\n## Environment\n- Working directory: {}\n- Project: {}\n- Platform: {}",
                    project_root_display,
                    project_name,
                    std::env::consts::OS,
                )
            });
        sections.push(PromptSection::new(SectionId::Environment, content));
    }

    // Section: Tool guide
    if !excluded.contains(&SectionId::ToolGuide) {
        let content = inputs
            .override_sections
            .get(&SectionId::ToolGuide)
            .cloned()
            .unwrap_or_else(default_tool_guide);
        sections.push(PromptSection::new(SectionId::ToolGuide, content));
    }

    // Section: Efficiency rules
    if !excluded.contains(&SectionId::EfficiencyRules) {
        let content = inputs
            .override_sections
            .get(&SectionId::EfficiencyRules)
            .cloned()
            .unwrap_or_else(default_efficiency_rules);
        sections.push(PromptSection::new(SectionId::EfficiencyRules, content));
    }

    // Section: Response formatting
    if !excluded.contains(&SectionId::ResponseFormatting) {
        let content = inputs
            .override_sections
            .get(&SectionId::ResponseFormatting)
            .cloned()
            .unwrap_or_else(default_response_formatting);
        sections.push(PromptSection::new(SectionId::ResponseFormatting, content));
    }

    // Section: Behavioral rules
    if !excluded.contains(&SectionId::BehavioralRules) {
        let content = inputs
            .override_sections
            .get(&SectionId::BehavioralRules)
            .cloned()
            .unwrap_or_else(default_behavioral_rules);
        sections.push(PromptSection::new(SectionId::BehavioralRules, content));
    }

    } // ── end verbose path else block ──

    // ── Shared sections (emitted for both compact and verbose paths) ──

    // Section: Project instructions (from PIPIT.md / CLAUDE.md)
    if !excluded.contains(&SectionId::ProjectInstructions) && !inputs.project_instructions.is_empty()
    {
        let mut content = String::new();
        for (rel_path, instruction_content) in &inputs.project_instructions {
            content.push_str(&format!("\n## Project instructions ({})\n\n", rel_path));
            // Cap instruction content to prevent unbounded prompt inflation.
            const INSTRUCTION_MAX_CHARS: usize = 8000;
            let truncated = if instruction_content.len() > INSTRUCTION_MAX_CHARS {
                &instruction_content[..INSTRUCTION_MAX_CHARS]
            } else {
                instruction_content.as_str()
            };
            content.push_str(&sanitize_injected_content(truncated, rel_path));
            content.push('\n');
        }
        sections.push(PromptSection::new(SectionId::ProjectInstructions, content));
    }

    // Section: Tool declarations — REMOVED
    // Tool names and descriptions are already carried in the `tools` array
    // of the OpenAI/Anthropic API request. Listing them again in the system
    // prompt wastes ~1-2K tokens and adds no signal for the model.
    // The Tool Selection Guide (above) already explains WHEN to use each
    // tool category, which is the important part.

    // Section: Edit format (verbose path only — compact already includes it)
    if !use_compact {
    if !excluded.contains(&SectionId::EditFormat) {
        let content = inputs
            .override_sections
            .get(&SectionId::EditFormat)
            .cloned()
            .unwrap_or_else(|| {
                "\n## Edit format\nUse edit_file with exact search text and replacement. \
                 Whitespace-normalized fuzzy matching is used as fallback.\n"
                    .to_string()
            });
        sections.push(PromptSection::new(SectionId::EditFormat, content));
    }
    }

    // Section: Provider hints
    if !excluded.contains(&SectionId::ProviderHints) {
        if let Some(hint) = inputs.provider_hint.as_deref() {
            let content = format!("\n## Model hints\n{}\n", hint);
            sections.push(PromptSection::new(SectionId::ProviderHints, content));
        }
    }

    // Section: Project conventions
    if !excluded.contains(&SectionId::ProjectConventions) {
        if let Some(ref conventions) = inputs.conventions {
            let content = format!("\n## Project conventions\n{}\n", conventions);
            sections.push(PromptSection::new(SectionId::ProjectConventions, content));
        }
    }

    // Section: Skills
    if !excluded.contains(&SectionId::Skills) {
        if let Some(ref skills) = inputs.skills_section {
            if !skills.is_empty() {
                sections.push(PromptSection::new(SectionId::Skills, skills.clone()));
            }
        }
        if let Some(ref workflow) = inputs.workflow_section {
            if !workflow.is_empty() {
                // Append workflow to skills section or create new
                if let Some(last) = sections.last_mut().filter(|s| s.id == SectionId::Skills) {
                    last.content.push_str(workflow);
                    last.content_hash = None;
                } else {
                    sections.push(PromptSection::new(SectionId::Skills, workflow.clone()));
                }
            }
        }
    }

    // Section: Knowledge
    if !excluded.contains(&SectionId::Knowledge) {
        if let Some(ref knowledge) = inputs.knowledge_section {
            if !knowledge.is_empty() {
                sections.push(PromptSection::new(SectionId::Knowledge, knowledge.clone()));
            }
        }
    }

    // Section: Memory
    if !excluded.contains(&SectionId::Memory) {
        if let Some(ref memory) = inputs.memory_section {
            if !memory.is_empty() {
                sections.push(PromptSection::new(SectionId::Memory, memory.clone()));
            }
        }
    }

    // Section: Domain Architecture (synthesized from requirements)
    if !excluded.contains(&SectionId::DomainArchitecture) {
        if let Some(ref arch) = inputs.domain_architecture_section {
            if !arch.is_empty() {
                sections.push(PromptSection::new(SectionId::DomainArchitecture, arch.clone()));
            }
        }
    }

    // Custom sections from embedders
    for custom in &inputs.custom_sections {
        if !excluded.contains(&custom.id) {
            sections.push(custom.clone());
        }
    }

    AssembledPrompt { sections }
}

// ── Default section content ─────────────────────────────────────────────

/// Context window threshold below which the assembler emits the compact fused
/// prompt. Models with ≤128K tokens benefit from a tighter system prompt because
/// every byte of system prompt competes with conversation history and tool results.
const COMPACT_CONTEXT_THRESHOLD: u64 = 131_072;

fn default_tool_guide() -> String {
    // Tool guide: describes what each tool does and key usage patterns.
    // Operational misuse (e.g. using bash for grep) is also caught at runtime
    // by validate_tool_semantics() — but the guide here helps the model pick
    // the right tool on the first try, especially for smaller models.
    r#"
## Tool selection guide

You have these tools. Choose the RIGHT one on the FIRST try:

**Finding files by name or pattern → `glob`**
  Example: glob("**/*.rs"), glob("**/test_*.py")

**Finding files by content → `grep`**
  Example: grep("fn main"), grep("TODO")
  grep searches file contents. glob searches file names. Don't confuse them.

**Understanding directory structure → `list_directory`**
  Use when you need to see what's in a specific directory.

**Reading file contents → `read_file`**
  Read the file ONCE before editing. Don't re-read files already in context.
  Use line ranges for large files: read_file(path, start_line, end_line).

**Editing existing files → `edit_file`**
  For surgical changes. Read the file first to get exact text to match.
  Prefer edit_file over write_file for existing files.

**Creating new files → `write_file`**
  Only for NEW files or complete rewrites.

**Running commands → `bash`**
  For build, test, lint, git operations.
  `cd` persists across calls — you don't need `cd /path && command` every time.

**Tracking multi-step work → `todo`**
  Use for tasks with multiple concrete steps.

**Delegating independent work → `subagent`**
  For bounded, parallelizable subtasks only."#
        .to_string()
}

fn default_efficiency_rules() -> String {
    // Slim version: only model-guidance that influences LLM decision-making.
    // Runtime-enforced policies (turn limits, tool authorization, context budgets)
    // are handled by PolicyKernel / TurnKernel / ContextManager and omitted here.
    r#"
## Efficiency rules

1. **Minimize turns.** Accomplish the task in as few tool calls as possible.
2. **Don't wander.** If you know the path, go directly — don't list_directory then read_file.
3. **Don't re-read.** Once a file's content is in your context, don't read it again unless it was modified.
4. **Don't narrate.** Don't say "Let me search for the file" — just search.
5. **Batch when possible.** Call multiple tools in the same turn when they are independent.
6. **Use the structure.** Use the project listing to navigate directly instead of exploring blindly."#
        .to_string()
}

fn default_response_formatting() -> String {
    r#"
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
- For simple Q&A, prefer a short answer with bullets or short paragraphs over a step-by-step narrative."#
        .to_string()
}

fn default_behavioral_rules() -> String {
    // Model-facing guidance only. Runtime enforcement (authorization, turn budget,
    // verification gates) is handled by PolicyKernel / TurnKernel.
    r#"
## Behavioral rules

1. Read before editing — understand the full context before making changes.
2. Make minimal, focused changes. Don't refactor code you weren't asked to change.
3. Use edit_file for surgical edits, not write_file (which rewrites the whole file).
4. Prefer existing patterns and conventions found in the codebase.
5. When asked a QUESTION, answer directly. Don't create plans or strategies for Q&A."#
        .to_string()
}

/// Compact fused guidelines — replaces ToolGuide + EfficiencyRules +
/// ResponseFormatting + BehavioralRules + EditFormat for models with
/// constrained context windows. ~950 chars vs ~4200 chars (77% reduction).
///
/// Design principle: every token in the system prompt must directly improve
/// the model's next action. Verbose explanations that a 7B+ model already
/// knows from pretraining are wasted context.
fn default_compact_guidelines() -> String {
    r#"
## Guidelines

- Be concise. Use markdown for formatting.
- Read files before editing. Make minimal, focused changes.
- Minimize tool calls — go directly to files if you know the path.
- Don't re-read files already in context.
- Use edit_file for surgical edits, write_file only for new files.
- Prefer grep/find/ls tools over bash for file exploration.
- Batch independent tool calls in a single response.
- Answer questions directly — no plans or preamble for Q&A.
- Show file paths clearly when referencing code."#
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_inputs_produce_core_sections() {
        let inputs = PromptInputs::default();
        let assembled = assemble(&inputs);
        let text = assembled.materialize();
        assert!(text.contains("Pipit"));
        assert!(text.contains("## Environment"));
        assert!(text.contains("## Tool selection guide"));
    }

    #[test]
    fn exclude_sections() {
        let inputs = PromptInputs {
            exclude_sections: vec![SectionId::ToolGuide, SectionId::EfficiencyRules],
            ..Default::default()
        };
        let assembled = assemble(&inputs);
        let text = assembled.materialize();
        assert!(!text.contains("## Tool selection guide"));
        assert!(!text.contains("## Efficiency rules"));
        assert!(text.contains("Pipit"));
    }

    #[test]
    fn override_section() {
        let mut overrides = HashMap::new();
        overrides.insert(SectionId::CoreIdentity, "You are a custom agent.".to_string());
        let inputs = PromptInputs {
            override_sections: overrides,
            ..Default::default()
        };
        let assembled = assemble(&inputs);
        let text = assembled.materialize();
        assert!(text.contains("You are a custom agent."));
        assert!(!text.contains("Pipit"));
    }

    #[test]
    fn custom_sections_appended() {
        let inputs = PromptInputs {
            custom_sections: vec![PromptSection::new(
                SectionId::Custom("test".to_string()),
                "## Custom\nHello from extension".to_string(),
            )],
            ..Default::default()
        };
        let assembled = assemble(&inputs);
        let text = assembled.materialize();
        assert!(text.contains("Hello from extension"));
    }

    #[test]
    fn tool_declarations_not_in_system_prompt() {
        // Tool declarations were removed from the system prompt in v0.3.1:
        // they are already carried in the API request's `tools` array.
        let inputs = PromptInputs {
            tools: vec![
                ToolDecl {
                    name: "read_file".to_string(),
                    description: "Read a file".to_string(),
                    requires_approval: false,
                },
                ToolDecl {
                    name: "bash".to_string(),
                    description: "Run shell commands".to_string(),
                    requires_approval: true,
                },
            ],
            ..Default::default()
        };
        let assembled = assemble(&inputs);
        let text = assembled.materialize();
        assert!(!text.contains("- **read_file**: Read a file"),
            "Tool declarations should not be duplicated in system prompt");
        assert!(!text.contains("## Available tools"),
            "Available tools section should not be in system prompt");
    }

    #[test]
    fn content_hash_invalidation() {
        let mut assembled = assemble(&PromptInputs::default());
        let hashes1 = assembled.content_hashes();
        // Hashes should be stable
        let hashes2 = assembled.content_hashes();
        assert_eq!(hashes1, hashes2);

        // Replace a section — hash should change
        assembled.replace_section(SectionId::CoreIdentity, "Changed".to_string());
        let hashes3 = assembled.content_hashes();
        let id_hash_1 = hashes1.iter().find(|(id, _)| *id == SectionId::CoreIdentity).unwrap().1;
        let id_hash_3 = hashes3.iter().find(|(id, _)| *id == SectionId::CoreIdentity).unwrap().1;
        assert_ne!(id_hash_1, id_hash_3);
    }

    #[test]
    fn sanitize_injected_content_escapes_xml() {
        let result = sanitize_injected_content("<script>alert(1)</script>", "test");
        assert!(result.contains("&lt;script&gt;"));
        assert!(!result.contains("<script>"));
    }

    // ── Section-level policy tests ──────────────────────────────────────

    #[test]
    fn behavioral_rules_inclusion_and_exclusion() {
        // Default: BehavioralRules included
        let default_prompt = assemble(&PromptInputs::default());
        assert!(
            default_prompt.section(SectionId::BehavioralRules).is_some(),
            "BehavioralRules should be present by default"
        );
        let text = default_prompt.materialize();
        assert!(text.contains("## Behavioral rules"));

        // Excluded: BehavioralRules absent
        let inputs = PromptInputs {
            exclude_sections: vec![SectionId::BehavioralRules],
            ..Default::default()
        };
        let excluded_prompt = assemble(&inputs);
        assert!(
            excluded_prompt.section(SectionId::BehavioralRules).is_none(),
            "BehavioralRules should be absent when excluded"
        );
        assert!(!excluded_prompt.materialize().contains("## Behavioral rules"));
    }

    #[test]
    fn provider_hint_composition() {
        // With provider hint
        let inputs = PromptInputs {
            provider_hint: Some("You support parallel tool use.".to_string()),
            ..Default::default()
        };
        let assembled = assemble(&inputs);
        let text = assembled.materialize();
        assert!(text.contains("## Model hints"));
        assert!(text.contains("parallel tool use"));

        // Without provider hint — no Model hints section
        let inputs_no_hint = PromptInputs::default();
        let assembled_no_hint = assemble(&inputs_no_hint);
        assert!(
            assembled_no_hint.section(SectionId::ProviderHints).is_none(),
            "ProviderHints section should be absent when no hint is provided"
        );
    }

    #[test]
    fn project_instruction_truncation() {
        // Instruction content exceeding 8000 chars should be truncated
        let long_content = "x".repeat(10_000);
        let inputs = PromptInputs {
            project_instructions: vec![("PIPIT.md".to_string(), long_content.clone())],
            ..Default::default()
        };
        let assembled = assemble(&inputs);
        let section = assembled.section(SectionId::ProjectInstructions).unwrap();
        // The raw content (10000 chars) should be truncated to 8000 within the section
        assert!(
            !section.content.contains(&"x".repeat(10_000)),
            "Project instructions should be truncated at 8000 chars"
        );
        assert!(
            section.content.contains(&"x".repeat(8000)),
            "Truncated content should include up to 8000 chars"
        );
    }

    #[test]
    fn custom_override_takes_precedence() {
        // Override CoreIdentity + add Custom section — both should appear
        let mut overrides = HashMap::new();
        overrides.insert(SectionId::CoreIdentity, "Custom identity.".to_string());
        let inputs = PromptInputs {
            override_sections: overrides,
            custom_sections: vec![PromptSection::new(
                SectionId::Custom("extra".to_string()),
                "## Extra\nExtension content here.".to_string(),
            )],
            ..Default::default()
        };
        let assembled = assemble(&inputs);
        let text = assembled.materialize();
        assert!(text.contains("Custom identity."), "Override should replace default");
        assert!(!text.contains("Pipit"), "Default identity should be gone");
        assert!(text.contains("Extension content here."), "Custom section should appear");
    }

    #[test]
    fn boot_context_not_in_system_prompt() {
        // The prompt kernel should never include boot_context — it belongs in
        // the turn-1 user message for cache stability. Verify there's no
        // "Initial project structure" or boot listing in assembled output.
        let inputs = PromptInputs {
            project_root: Some(std::path::PathBuf::from("/tmp/test-project")),
            ..Default::default()
        };
        let assembled = assemble(&inputs);
        let text = assembled.materialize();
        assert!(
            !text.contains("Initial project structure"),
            "Boot context must not appear in system prompt"
        );
    }

    #[test]
    fn efficiency_rules_are_model_guidance_only() {
        // After slimming, efficiency rules should not contain runtime-enforced
        // policies like "verify cd", "track real work", or "delegate surgically".
        let inputs = PromptInputs::default();
        let assembled = assemble(&inputs);
        let section = assembled.section(SectionId::EfficiencyRules).unwrap();
        assert!(!section.content.contains("Don't verify cd"),
            "cd verification is a runtime guarantee, not model guidance");
        assert!(!section.content.contains("Track real work"),
            "todo tracking is tool-specific advice moved to ToolGuide");
        assert!(!section.content.contains("Delegate surgically"),
            "subagent policy is tool-specific advice moved to ToolGuide");
    }

    #[test]
    fn behavioral_rules_are_model_guidance_only() {
        // Behavioral rules should not include runtime-enforced policies.
        let inputs = PromptInputs::default();
        let assembled = assemble(&inputs);
        let section = assembled.section(SectionId::BehavioralRules).unwrap();
        assert!(!section.content.contains("analyze it and try a different approach"),
            "Error recovery is handled by the agent loop, not prompt prose");
        assert!(!section.content.contains("create and maintain a `todo` list"),
            "todo management is tool-level guidance in ToolGuide");
    }
}
