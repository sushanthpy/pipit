use crate::Tool;
use pipit_config::ApprovalMode;
use pipit_provider::ToolDeclaration;
use std::collections::HashMap;
use std::sync::Arc;

/// The tool registry holds all available tools.
#[derive(Clone)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Create a registry with all built-in tools.
    /// Subagent must be registered separately via `register_subagent`
    /// because it requires an executor callback.
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
        // Register extended tools
        crate::builtins::extended::register_extended_tools(&mut registry);
        registry
    }

    /// Register the subagent tool with a provided executor.
    pub fn register_subagent(&mut self, executor: Arc<dyn crate::builtins::subagent::SubagentExecutor>) {
        self.register(Arc::new(crate::builtins::SubagentTool::new(executor)));
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
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
