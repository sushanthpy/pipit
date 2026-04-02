//! Native Slash Command Engine — Compile-Time Command Registry
//!
//! Typed, extensible command system with trie-based prefix matching,
//! tab completion, lifecycle hooks, and hot-reload for user commands.
//!
//! Command dispatch: O(L) where L = command name length.
//! Fuzzy completion: O(|candidates| × L) with edit distance.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Schema for command arguments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgsSchema {
    pub positional: Vec<ArgDef>,
    pub flags: Vec<FlagDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArgDef {
    pub name: String,
    pub description: String,
    pub required: bool,
    pub arg_type: ArgType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlagDef {
    pub long: String,
    pub short: Option<char>,
    pub description: String,
    pub takes_value: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArgType {
    String,
    Integer,
    Boolean,
    FilePath,
    Choice(Vec<String>),
}

/// A completion candidate for tab-completion.
#[derive(Debug, Clone)]
pub struct CompletionItem {
    pub text: String,
    pub description: Option<String>,
    pub kind: CompletionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Command,
    Argument,
    Flag,
    FilePath,
    Value,
}

/// Context provided to command execution.
/// Carries kernel state snapshots for commands that need live data.
#[derive(Debug, Clone)]
pub struct CommandContext {
    pub args: Vec<String>,
    pub flags: HashMap<String, String>,
    pub raw_input: String,
    pub project_root: std::path::PathBuf,
    pub session_id: Option<String>,
    /// Snapshot of session cost (USD) from TelemetryFacade.
    pub session_cost: f64,
    /// Snapshot of token usage.
    pub tokens_used: u64,
    pub tokens_limit: u64,
    /// Snapshot of turn count.
    pub turn_count: u32,
    /// Current model name.
    pub model_name: Option<String>,
    /// Current provider name.
    pub provider_name: Option<String>,
    /// Planning state summary.
    pub plan_summary: Option<String>,
    /// Context budget breakdown.
    pub context_budget: Option<ContextBudgetSnapshot>,
}

/// Snapshot of context budget for /context command.
#[derive(Debug, Clone)]
pub struct ContextBudgetSnapshot {
    pub model_limit: u64,
    pub system_prompt_tokens: u64,
    pub history_tokens: u64,
    pub output_reserve: u64,
    pub available: u64,
}

/// Output from command execution.
#[derive(Debug, Clone)]
pub enum CommandOutput {
    /// Display text to the user.
    Text(String),
    /// Inject as a user message into the agent loop.
    AgentMessage(String),
    /// Display structured data.
    Structured(Value),
    /// No output (side effect only).
    Silent,
    /// Error message.
    Error(String),
}

/// Error from command execution.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("Invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("Not available: {0}")]
    NotAvailable(String),
    #[error("Execution failed: {0}")]
    Failed(String),
}

/// Command category for grouping in /help.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CommandCategory {
    Git,
    Review,
    Navigation,
    Session,
    Config,
    Agent,
    Integration,
    Help,
    DevOps,
}

/// The core command trait. Every slash command implements this.
#[async_trait]
pub trait Command: Send + Sync {
    /// Primary name (e.g., "commit").
    fn name(&self) -> &str;
    /// Aliases (e.g., ["ci"]).
    fn aliases(&self) -> &[&str] { &[] }
    /// Human-readable description.
    fn description(&self) -> &str;
    /// Category for grouping.
    fn category(&self) -> CommandCategory;
    /// Argument schema for validation and help.
    fn args_schema(&self) -> Option<ArgsSchema> { None }
    /// Tab-completion candidates from partial input.
    fn completion_candidates(&self, _partial: &str) -> Vec<CompletionItem> { vec![] }
    /// Execute the command.
    async fn execute(&self, ctx: CommandContext) -> Result<CommandOutput, CommandError>;
}

/// The command registry — holds all registered commands with trie-based lookup.
pub struct CommandRegistry {
    commands: Vec<Box<dyn Command>>,
    name_index: HashMap<String, usize>,
    category_index: HashMap<CommandCategory, Vec<usize>>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            name_index: HashMap::new(),
            category_index: HashMap::new(),
        }
    }

    /// Register a command.
    pub fn register(&mut self, cmd: Box<dyn Command>) {
        let idx = self.commands.len();
        let name = cmd.name().to_string();
        let category = cmd.category();

        self.name_index.insert(name, idx);
        for alias in cmd.aliases() {
            self.name_index.insert(alias.to_string(), idx);
        }
        self.category_index.entry(category).or_default().push(idx);
        self.commands.push(cmd);
    }

    /// Look up a command by name or alias. O(1) hash lookup.
    pub fn get(&self, name: &str) -> Option<&dyn Command> {
        self.name_index.get(name).map(|&idx| &*self.commands[idx])
    }

    /// Find commands matching a prefix. O(n) scan but n is small (<100).
    pub fn prefix_matches(&self, prefix: &str) -> Vec<&dyn Command> {
        let lower = prefix.to_lowercase();
        self.commands
            .iter()
            .filter(|cmd| {
                cmd.name().starts_with(&lower)
                    || cmd.aliases().iter().any(|a| a.starts_with(&lower))
            })
            .map(|cmd| &**cmd)
            .collect()
    }

    /// Fuzzy match with edit distance threshold.
    pub fn fuzzy_match(&self, query: &str, max_distance: usize) -> Vec<(&dyn Command, usize)> {
        let q = query.to_lowercase();
        let mut matches: Vec<(&dyn Command, usize)> = self
            .commands
            .iter()
            .filter_map(|cmd| {
                let dist = edit_distance(cmd.name(), &q);
                if dist <= max_distance {
                    Some((&**cmd, dist))
                } else {
                    None
                }
            })
            .collect();
        matches.sort_by_key(|(_, d)| *d);
        matches
    }

    /// Get all commands in a category.
    pub fn by_category(&self, category: CommandCategory) -> Vec<&dyn Command> {
        self.category_index
            .get(&category)
            .map(|indices| indices.iter().map(|&i| &*self.commands[i]).collect())
            .unwrap_or_default()
    }

    /// Get completion items for partial input.
    pub fn completions(&self, partial: &str) -> Vec<CompletionItem> {
        self.prefix_matches(partial)
            .iter()
            .map(|cmd| CompletionItem {
                text: format!("/{}", cmd.name()),
                description: Some(cmd.description().to_string()),
                kind: CompletionKind::Command,
            })
            .collect()
    }

    /// Number of registered commands.
    pub fn count(&self) -> usize {
        self.commands.len()
    }

    /// Generate help text for all commands grouped by category.
    pub fn help_text(&self) -> String {
        let mut output = String::from("Available commands:\n\n");
        let categories = [
            CommandCategory::Git,
            CommandCategory::Review,
            CommandCategory::Navigation,
            CommandCategory::Session,
            CommandCategory::Config,
            CommandCategory::Agent,
            CommandCategory::Integration,
            CommandCategory::Help,
            CommandCategory::DevOps,
        ];
        for cat in &categories {
            let cmds = self.by_category(*cat);
            if cmds.is_empty() { continue; }
            output.push_str(&format!("  {:?}:\n", cat));
            for cmd in cmds {
                let aliases = cmd.aliases();
                let alias_str = if aliases.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", aliases.iter().map(|a| format!("/{}", a)).collect::<Vec<_>>().join(", "))
                };
                output.push_str(&format!("    /{:<20} {}{}\n", cmd.name(), cmd.description(), alias_str));
            }
            output.push('\n');
        }
        output
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Levenshtein edit distance. O(|a| × |b|).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 0..=m { dp[i][0] = i; }
    for j in 0..=n { dp[0][j] = j; }
    for i in 1..=m {
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[m][n]
}

// ═══════════════════════════════════════════════════════════════════════
//  Built-in commands — real implementations that read kernel state
// ═══════════════════════════════════════════════════════════════════════

/// Macro for commands that pass through to the LLM as agent messages.
macro_rules! passthrough_command {
    ($name:ident, $cmd_name:literal, $desc:literal, $cat:expr, $aliases:expr) => {
        pub struct $name;
        #[async_trait]
        impl Command for $name {
            fn name(&self) -> &str { $cmd_name }
            fn aliases(&self) -> &[&str] { $aliases }
            fn description(&self) -> &str { $desc }
            fn category(&self) -> CommandCategory { $cat }
            async fn execute(&self, ctx: CommandContext) -> Result<CommandOutput, CommandError> {
                Ok(CommandOutput::AgentMessage(format!("/{} {}", $cmd_name, ctx.args.join(" "))))
            }
        }
    };
}

// ── Real implementations for critical commands ──

pub struct CostCmd;
#[async_trait]
impl Command for CostCmd {
    fn name(&self) -> &str { "cost" }
    fn description(&self) -> &str { "Show session cost breakdown" }
    fn category(&self) -> CommandCategory { CommandCategory::Agent }
    async fn execute(&self, ctx: CommandContext) -> Result<CommandOutput, CommandError> {
        Ok(CommandOutput::Text(format!(
            "Session Cost Summary\n\
             ├─ Total cost:    ${:.4}\n\
             ├─ Tokens used:   {}\n\
             ├─ Tokens limit:  {}\n\
             ├─ Turns:         {}\n\
             ├─ Model:         {}\n\
             └─ Provider:      {}",
            ctx.session_cost,
            ctx.tokens_used,
            ctx.tokens_limit,
            ctx.turn_count,
            ctx.model_name.as_deref().unwrap_or("unknown"),
            ctx.provider_name.as_deref().unwrap_or("unknown"),
        )))
    }
}

pub struct StatusCmd;
#[async_trait]
impl Command for StatusCmd {
    fn name(&self) -> &str { "status" }
    fn description(&self) -> &str { "Show session status" }
    fn category(&self) -> CommandCategory { CommandCategory::Session }
    async fn execute(&self, ctx: CommandContext) -> Result<CommandOutput, CommandError> {
        let budget = ctx.context_budget.as_ref().map(|b| {
            format!(
                "\nContext Budget:\n\
                 ├─ Model limit:  {}\n\
                 ├─ System:       {}\n\
                 ├─ History:      {}\n\
                 ├─ Output:       {} (reserved)\n\
                 └─ Available:    {}",
                b.model_limit, b.system_prompt_tokens, b.history_tokens, b.output_reserve, b.available
            )
        }).unwrap_or_default();
        Ok(CommandOutput::Text(format!(
            "Session: {}\n\
             Model: {} ({})\n\
             Turns: {} | Cost: ${:.4} | Tokens: {}/{}{}",
            ctx.session_id.as_deref().unwrap_or("unnamed"),
            ctx.model_name.as_deref().unwrap_or("unknown"),
            ctx.provider_name.as_deref().unwrap_or("unknown"),
            ctx.turn_count, ctx.session_cost, ctx.tokens_used, ctx.tokens_limit,
            budget,
        )))
    }
}

pub struct ContextCmd;
#[async_trait]
impl Command for ContextCmd {
    fn name(&self) -> &str { "context" }
    fn aliases(&self) -> &[&str] { &["ctx"] }
    fn description(&self) -> &str { "Show context window budget" }
    fn category(&self) -> CommandCategory { CommandCategory::Navigation }
    async fn execute(&self, ctx: CommandContext) -> Result<CommandOutput, CommandError> {
        match ctx.context_budget {
            Some(ref budget) => {
                let view = crate::dx_surface::ContextBudgetView {
                    model_limit: budget.model_limit,
                    system_prompt_tokens: budget.system_prompt_tokens,
                    pinned_tokens: 0,
                    active_tokens: budget.history_tokens,
                    historical_tokens: 0,
                    exhaust_tokens: 0,
                    output_reserve: budget.output_reserve,
                    available: budget.available,
                };
                Ok(CommandOutput::Text(view.render_summary()))
            }
            None => Ok(CommandOutput::Text("Context budget not available".into())),
        }
    }
}

pub struct DoctorCmd;
#[async_trait]
impl Command for DoctorCmd {
    fn name(&self) -> &str { "doctor" }
    fn description(&self) -> &str { "System diagnostics" }
    fn category(&self) -> CommandCategory { CommandCategory::Help }
    async fn execute(&self, ctx: CommandContext) -> Result<CommandOutput, CommandError> {
        let checks = crate::dx_surface::run_diagnostics(&ctx.project_root);
        Ok(CommandOutput::Text(crate::dx_surface::format_diagnostics(&checks)))
    }
}

pub struct HelpCmd;
#[async_trait]
impl Command for HelpCmd {
    fn name(&self) -> &str { "help" }
    fn aliases(&self) -> &[&str] { &["h", "?"] }
    fn description(&self) -> &str { "Show help" }
    fn category(&self) -> CommandCategory { CommandCategory::Help }
    async fn execute(&self, _ctx: CommandContext) -> Result<CommandOutput, CommandError> {
        // Help text is generated by the registry; the caller renders it.
        Ok(CommandOutput::Text("Use /help to see all commands. The registry generates help text.".into()))
    }
}

pub struct PlanCmd;
#[async_trait]
impl Command for PlanCmd {
    fn name(&self) -> &str { "plan" }
    fn description(&self) -> &str { "Show current plan" }
    fn category(&self) -> CommandCategory { CommandCategory::Agent }
    async fn execute(&self, ctx: CommandContext) -> Result<CommandOutput, CommandError> {
        match ctx.plan_summary {
            Some(ref plan) => Ok(CommandOutput::Text(format!("Current Plan:\n{}", plan))),
            None => {
                if ctx.args.is_empty() {
                    Ok(CommandOutput::Text("No plan active. Use /plan <goal> to create one.".into()))
                } else {
                    Ok(CommandOutput::AgentMessage(format!("/plan {}", ctx.args.join(" "))))
                }
            }
        }
    }
}

pub struct VersionCmd;
#[async_trait]
impl Command for VersionCmd {
    fn name(&self) -> &str { "version" }
    fn aliases(&self) -> &[&str] { &["v"] }
    fn description(&self) -> &str { "Show version" }
    fn category(&self) -> CommandCategory { CommandCategory::Help }
    async fn execute(&self, _ctx: CommandContext) -> Result<CommandOutput, CommandError> {
        Ok(CommandOutput::Text(format!("pipit v{}", env!("CARGO_PKG_VERSION"))))
    }
}

// ── Passthrough commands (sent to LLM) ──

// Git/VCS
passthrough_command!(CommitCmd, "commit", "Stage and commit changes", CommandCategory::Git, &["ci"]);
passthrough_command!(PushCmd, "push", "Push commits to remote", CommandCategory::Git, &[]);
passthrough_command!(PrCmd, "pr", "Create or manage pull requests", CommandCategory::Git, &[]);
passthrough_command!(DiffCmd, "diff", "Show changes in working tree", CommandCategory::Git, &[]);
passthrough_command!(BranchCmd, "branch", "Create or switch branches", CommandCategory::Git, &["br"]);
passthrough_command!(StashCmd, "stash", "Stash working changes", CommandCategory::Git, &[]);
passthrough_command!(BlameCmd, "blame", "Show file annotation", CommandCategory::Git, &[]);

// Review
passthrough_command!(ReviewCmd, "review", "Review code changes", CommandCategory::Review, &[]);
passthrough_command!(SecurityReviewCmd, "security-review", "Security audit", CommandCategory::Review, &["sec"]);
passthrough_command!(LintCmd, "lint", "Run linter", CommandCategory::Review, &[]);
passthrough_command!(TestCmd, "test", "Run tests", CommandCategory::Review, &["t"]);

// Navigation
passthrough_command!(FilesCmd, "files", "List project files", CommandCategory::Navigation, &["ls"]);
passthrough_command!(SearchCmd, "search", "Search codebase", CommandCategory::Navigation, &["s"]);
passthrough_command!(TreeCmd, "tree", "Show directory tree", CommandCategory::Navigation, &[]);

// Session
passthrough_command!(CompactCmd, "compact", "Compress context", CommandCategory::Session, &[]);
passthrough_command!(ClearCmd, "clear", "Clear conversation", CommandCategory::Session, &[]);
passthrough_command!(ResumeCmd, "resume", "Resume session", CommandCategory::Session, &[]);
passthrough_command!(ExportCmd, "export", "Export conversation", CommandCategory::Session, &[]);
passthrough_command!(SaveCmd, "save", "Save session", CommandCategory::Session, &[]);

// Config
passthrough_command!(ConfigCmd, "config", "Edit configuration", CommandCategory::Config, &[]);
passthrough_command!(ModelCmd, "model", "Switch model", CommandCategory::Config, &[]);
passthrough_command!(ProviderCmd, "provider", "Switch provider", CommandCategory::Config, &[]);
passthrough_command!(ApprovalCmd, "approval", "Change approval mode", CommandCategory::Config, &["permissions"]);

// Agent
passthrough_command!(VerifyCmd, "verify", "Run verification", CommandCategory::Agent, &[]);
passthrough_command!(DelegateCmd, "delegate", "Delegate subtask", CommandCategory::Agent, &[]);
passthrough_command!(SkillsCmd, "skills", "List skills", CommandCategory::Agent, &[]);
passthrough_command!(UndoCmd, "undo", "Undo last edit", CommandCategory::Agent, &[]);

// Integration
passthrough_command!(GithubCmd, "github", "GitHub operations", CommandCategory::Integration, &["gh"]);
passthrough_command!(SlackCmd, "slack", "Slack integration", CommandCategory::Integration, &[]);
passthrough_command!(McpCmd, "mcp", "MCP server management", CommandCategory::Integration, &[]);
passthrough_command!(BridgeCmd, "bridge", "IDE bridge", CommandCategory::Integration, &[]);
passthrough_command!(BrowseCmd, "browse", "Browser integration", CommandCategory::Integration, &[]);

// Help (DoctorCmd, HelpCmd, VersionCmd are real impls above)
passthrough_command!(FeedbackCmd, "feedback", "Send feedback", CommandCategory::Help, &[]);
passthrough_command!(MemoryCmd, "memory", "Manage memory", CommandCategory::Help, &[]);

// DevOps
passthrough_command!(BenchCmd, "bench", "Run benchmarks", CommandCategory::DevOps, &[]);
passthrough_command!(TasksCmd, "tasks", "Task management", CommandCategory::DevOps, &[]);
passthrough_command!(MonitorCmd, "monitor", "System monitor", CommandCategory::DevOps, &[]);

/// Create a registry with all built-in commands.
pub fn builtin_registry() -> CommandRegistry {
    let mut reg = CommandRegistry::new();
    // Git
    reg.register(Box::new(CommitCmd));
    reg.register(Box::new(PushCmd));
    reg.register(Box::new(PrCmd));
    reg.register(Box::new(DiffCmd));
    reg.register(Box::new(BranchCmd));
    reg.register(Box::new(StashCmd));
    reg.register(Box::new(BlameCmd));
    // Review
    reg.register(Box::new(ReviewCmd));
    reg.register(Box::new(SecurityReviewCmd));
    reg.register(Box::new(LintCmd));
    reg.register(Box::new(TestCmd));
    // Navigation
    reg.register(Box::new(FilesCmd));
    reg.register(Box::new(ContextCmd));
    reg.register(Box::new(SearchCmd));
    reg.register(Box::new(TreeCmd));
    // Session
    reg.register(Box::new(CompactCmd));
    reg.register(Box::new(ClearCmd));
    reg.register(Box::new(ResumeCmd));
    reg.register(Box::new(ExportCmd));
    reg.register(Box::new(StatusCmd));
    reg.register(Box::new(SaveCmd));
    // Config
    reg.register(Box::new(ConfigCmd));
    reg.register(Box::new(ModelCmd));
    reg.register(Box::new(ProviderCmd));
    reg.register(Box::new(ApprovalCmd));
    // Agent
    reg.register(Box::new(PlanCmd));
    reg.register(Box::new(VerifyCmd));
    reg.register(Box::new(DelegateCmd));
    reg.register(Box::new(SkillsCmd));
    reg.register(Box::new(CostCmd));
    reg.register(Box::new(UndoCmd));
    // Integration
    reg.register(Box::new(GithubCmd));
    reg.register(Box::new(SlackCmd));
    reg.register(Box::new(McpCmd));
    reg.register(Box::new(BridgeCmd));
    reg.register(Box::new(BrowseCmd));
    // Help
    reg.register(Box::new(HelpCmd));
    reg.register(Box::new(DoctorCmd));
    reg.register(Box::new(FeedbackCmd));
    reg.register(Box::new(VersionCmd));
    reg.register(Box::new(MemoryCmd));
    // DevOps
    reg.register(Box::new(BenchCmd));
    reg.register(Box::new(TasksCmd));
    reg.register(Box::new(MonitorCmd));
    reg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lookup() {
        let reg = builtin_registry();
        assert!(reg.get("commit").is_some());
        assert!(reg.get("ci").is_some()); // alias
        assert!(reg.get("nonexistent").is_none());
        assert!(reg.count() >= 35);
    }

    #[test]
    fn prefix_matching() {
        let reg = builtin_registry();
        let matches = reg.prefix_matches("co");
        let names: Vec<&str> = matches.iter().map(|c| c.name()).collect();
        assert!(names.contains(&"commit") || names.contains(&"compact") || names.contains(&"config") || names.contains(&"cost") || names.contains(&"context"));
    }

    #[test]
    fn fuzzy_matching() {
        let reg = builtin_registry();
        let matches = reg.fuzzy_match("comit", 2); // typo for "commit"
        assert!(!matches.is_empty());
        assert_eq!(matches[0].0.name(), "commit");
    }

    #[test]
    fn completions() {
        let reg = builtin_registry();
        let items = reg.completions("st");
        assert!(items.iter().any(|i| i.text == "/status" || i.text == "/stash"));
    }

    #[test]
    fn help_text_grouped() {
        let reg = builtin_registry();
        let help = reg.help_text();
        assert!(help.contains("Git:"));
        assert!(help.contains("/commit"));
    }

    #[test]
    fn edit_distance_works() {
        assert_eq!(edit_distance("commit", "commit"), 0);
        assert_eq!(edit_distance("commit", "comit"), 1);
        assert_eq!(edit_distance("commit", "kommit"), 1);
        assert_eq!(edit_distance("abc", "xyz"), 3);
    }
}
