use crate::Tool;
use pipit_config::ApprovalMode;
use pipit_provider::ToolDeclaration;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// The tool registry holds all available tools.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    /// Monotonically increasing generation counter. Bumped on any registry
    /// mutation (register, unregister, MCP reconnect). Consumers cache
    /// declarations keyed by this generation to avoid stale tool lists.
    generation: Arc<AtomicU64>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Create a registry with core built-in tools only.
    /// These are the essential tools for coding tasks:
    /// read_file, write_file, edit_file, multi_edit_file,
    /// list_directory, grep, glob, bash.
    ///
    /// Extended tools (browser, MCP, A2A, powershell, etc.) are registered
    /// separately via `register_extended` to keep the default tool set lean
    /// and avoid overwhelming smaller models with 30+ tool schemas.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry.register(Arc::new(crate::builtins::ReadFileTool));
        registry.register(Arc::new(crate::builtins::WriteFileTool));
        registry.register(Arc::new(crate::builtins::EditFileTool));
        registry.register(Arc::new(crate::builtins::MultiEditTool));
        registry.register(Arc::new(crate::builtins::ListDirectoryTool));
        registry.register(Arc::new(crate::builtins::GrepTool));
        registry.register(Arc::new(crate::builtins::GlobTool));
        registry.register(Arc::new(crate::builtins::BashTool));
        // Register essential typed tools (task tracking + ask_user)
        crate::register_typed(&mut registry, crate::builtins::typed::task::UnifiedTaskTool::new());
        crate::register_typed(&mut registry, crate::builtins::typed::agent_tools::AskUserTool);
        registry
    }

    /// Create a registry with ALL tools (core + extended).
    /// Use this when the model is large enough to handle 30+ tools,
    /// or when features like browser, MCP, A2A are needed.
    pub fn with_all_tools() -> Self {
        let mut registry = Self::with_builtins();
        crate::builtins::extended::register_extended_tools(&mut registry);
        crate::builtins::typed::register_all_typed_tools(&mut registry);
        registry
    }

    /// Register extended tools on top of an existing registry.
    /// Called when --tools=all or specific extended tools are requested.
    pub fn register_extended(&mut self) {
        crate::builtins::extended::register_extended_tools(self);
        crate::builtins::typed::register_all_typed_tools(self);
    }

    /// Register the subagent tool with a provided executor.
    pub fn register_subagent(
        &mut self,
        executor: Arc<dyn crate::builtins::subagent::SubagentExecutor>,
    ) {
        self.register(Arc::new(crate::builtins::SubagentTool::new(executor)));
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Current generation counter. Consumers should re-fetch declarations
    /// when this value changes (e.g. after MCP reconnect).
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Unregister a tool by name. Used when MCP servers disconnect.
    pub fn unregister(&mut self, name: &str) -> bool {
        let removed = self.tools.remove(name).is_some();
        if removed {
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
        removed
    }

    /// Invalidate the declaration cache (bump generation) without changing tools.
    /// Call this when MCP servers reconnect and their tool schemas may have changed.
    pub fn invalidate_cache(&self) {
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Get all tool declarations for the LLM — includes ALL tools regardless
    /// of approval mode. Approval gating is enforced at execution time.
    pub fn declarations(&self) -> Vec<ToolDeclaration> {
        self.tools
            .values()
            .map(|t| ToolDeclaration {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.schema(),
            })
            .collect()
    }

    /// Get tool declarations annotated with approval requirements for the
    /// system prompt. This tells the model which tools need human approval.
    pub fn declarations_annotated(&self, mode: ApprovalMode) -> Vec<(ToolDeclaration, bool)> {
        self.tools
            .values()
            .map(|t| {
                let needs_approval = t.requires_approval(mode);
                let decl = ToolDeclaration {
                    name: t.name().to_string(),
                    description: t.description().to_string(),
                    input_schema: t.schema(),
                };
                (decl, needs_approval)
            })
            .collect()
    }

    pub fn tool_names(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }
}
