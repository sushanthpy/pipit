//! Runtime Extension Model
//!
//! A first-class runtime extension API that can declare and register commands,
//! tools, hooks, and UI affordances. Goes beyond the install-only plugin
//! registry to provide runtime capability registration with discovery and
//! isolation semantics.
//!
//! Architecture: capability registry + event bus, not only a manifest store.
//! Lookup: O(1) by name via HashMap, O(h) fan-out over subscribed handlers.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// A runtime extension — the unit of programmability.
///
/// Extensions register capabilities (commands, tools, hooks, renderers)
/// and receive lifecycle events. They are loaded at session start from
/// project-local, global, and explicit paths.
#[async_trait]
pub trait RuntimeExtension: Send + Sync {
    /// Unique extension identifier (e.g., "pipit-ext-github-pr").
    fn id(&self) -> &str;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// Called when the extension is loaded into the runtime.
    /// Register commands, tools, hooks, and shortcuts here.
    async fn activate(&self, ctx: &mut ActivationContext) -> Result<(), ExtensionActivateError>;

    /// Called when the extension is being unloaded.
    async fn deactivate(&self) -> Result<(), ExtensionActivateError>;
}

/// Context provided during extension activation for capability registration.
pub struct ActivationContext {
    /// Commands registered by this extension.
    pub commands: Vec<ExtensionCommand>,
    /// Tools registered by this extension.
    pub tools: Vec<ExtensionTool>,
    /// Event subscriptions registered by this extension.
    pub subscriptions: Vec<EventSubscription>,
    /// Shortcuts (slash-commands) registered by this extension.
    pub shortcuts: Vec<ExtensionShortcut>,
    /// Message renderers registered by this extension.
    pub renderers: Vec<ExtensionRenderer>,
    /// Flags/settings declared by this extension.
    pub flags: Vec<ExtensionFlag>,
}

impl ActivationContext {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            tools: Vec::new(),
            subscriptions: Vec::new(),
            shortcuts: Vec::new(),
            renderers: Vec::new(),
            flags: Vec::new(),
        }
    }

    /// Register a command that can be invoked by name.
    pub fn register_command(&mut self, cmd: ExtensionCommand) {
        self.commands.push(cmd);
    }

    /// Register a tool that becomes available to the agent.
    pub fn register_tool(&mut self, tool: ExtensionTool) {
        self.tools.push(tool);
    }

    /// Subscribe to runtime events.
    pub fn subscribe(&mut self, sub: EventSubscription) {
        self.subscriptions.push(sub);
    }

    /// Register a slash-command shortcut.
    pub fn register_shortcut(&mut self, shortcut: ExtensionShortcut) {
        self.shortcuts.push(shortcut);
    }

    /// Register a message renderer.
    pub fn register_renderer(&mut self, renderer: ExtensionRenderer) {
        self.renderers.push(renderer);
    }

    /// Declare a flag/setting.
    pub fn declare_flag(&mut self, flag: ExtensionFlag) {
        self.flags.push(flag);
    }
}

impl Default for ActivationContext {
    fn default() -> Self {
        Self::new()
    }
}

/// A command registered by an extension.
#[derive(Clone)]
pub struct ExtensionCommand {
    /// Command name (e.g., "github:create-pr").
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// The handler — called when the command is invoked.
    pub handler: Arc<dyn CommandHandler>,
}

/// Handler for an extension command.
#[async_trait]
pub trait CommandHandler: Send + Sync {
    async fn execute(&self, args: Value) -> Result<Value, String>;
}

/// A tool registered by an extension (injected into the agent's tool registry).
#[derive(Clone)]
pub struct ExtensionTool {
    /// Tool name.
    pub name: String,
    /// JSON Schema for the tool's parameters.
    pub schema: Value,
    /// Description.
    pub description: String,
    /// Whether this tool mutates state.
    pub is_mutating: bool,
    /// The handler.
    pub handler: Arc<dyn ToolHandler>,
}

/// Handler for an extension-provided tool.
#[async_trait]
pub trait ToolHandler: Send + Sync {
    async fn execute(&self, args: Value) -> Result<ToolHandlerResult, String>;
}

/// Result from an extension tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolHandlerResult {
    pub content: String,
    pub mutated: bool,
}

/// Runtime event types that extensions can subscribe to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuntimeEvent {
    SessionStart,
    SessionEnd,
    TurnStart,
    TurnEnd,
    BeforeToolUse,
    AfterToolUse,
    BeforeRequest,
    AfterResponse,
    BeforeCompact,
    AfterCompact,
    FileModified,
    PlanSelected,
    VerifierVerdict,
    Custom,
}

/// An event subscription from an extension.
#[derive(Clone)]
pub struct EventSubscription {
    pub event: RuntimeEvent,
    /// Optional filter (e.g., tool name pattern for tool events).
    pub filter: Option<String>,
    pub handler: Arc<dyn EventHandler>,
}

/// Handler for runtime events.
#[async_trait]
pub trait EventHandler: Send + Sync {
    async fn handle(&self, event: RuntimeEvent, payload: Value) -> Result<(), String>;
}

/// A slash-command shortcut registered by an extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionShortcut {
    /// The slash command (e.g., "/pr").
    pub command: String,
    /// Description shown in help.
    pub description: String,
    /// The handler command name to dispatch to.
    pub dispatch_to: String,
}

/// A message renderer registered by an extension.
#[derive(Clone)]
pub struct ExtensionRenderer {
    /// MIME type or content tag this renderer handles.
    pub content_type: String,
    pub handler: Arc<dyn RenderHandler>,
}

/// Handler for custom message rendering.
#[async_trait]
pub trait RenderHandler: Send + Sync {
    async fn render(&self, content: &str) -> Result<String, String>;
}

/// A flag/setting declared by an extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionFlag {
    pub name: String,
    pub description: String,
    pub default_value: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtensionActivateError {
    #[error("activation failed: {0}")]
    Failed(String),
    #[error("dependency not available: {0}")]
    MissingDependency(String),
}

/// The runtime extension registry — manages loaded extensions and dispatches events.
///
/// Indexed dispatch: O(1) lookup by command/tool name, O(h) fan-out for events
/// over h subscribed handlers.
pub struct ExtensionRuntime {
    extensions: Vec<Arc<dyn RuntimeExtension>>,
    /// All registered commands indexed by name.
    commands: HashMap<String, (String, Arc<dyn CommandHandler>)>,
    /// All registered tools indexed by name, with owner extension ID.
    tools: HashMap<String, (String, ExtensionTool)>,
    /// Event subscriptions indexed by event type.
    subscriptions: HashMap<RuntimeEvent, Vec<(String, Option<String>, Arc<dyn EventHandler>)>>,
    /// Shortcuts indexed by command string, with owner extension ID.
    shortcuts: HashMap<String, (String, ExtensionShortcut)>,
    /// Renderers indexed by content type, with owner extension ID.
    renderers: HashMap<String, (String, Arc<dyn RenderHandler>)>,
    /// Flags indexed by name.
    flags: HashMap<String, (String, ExtensionFlag)>,
}

impl ExtensionRuntime {
    pub fn new() -> Self {
        Self {
            extensions: Vec::new(),
            commands: HashMap::new(),
            tools: HashMap::new(),
            subscriptions: HashMap::new(),
            shortcuts: HashMap::new(),
            renderers: HashMap::new(),
            flags: HashMap::new(),
        }
    }

    /// Load and activate an extension.
    pub async fn load(&mut self, ext: Arc<dyn RuntimeExtension>) -> Result<LoadResult, ExtensionActivateError> {
        let ext_id = ext.id().to_string();
        let mut ctx = ActivationContext::new();
        ext.activate(&mut ctx).await?;

        let mut result = LoadResult {
            extension_id: ext_id.clone(),
            commands_registered: Vec::new(),
            tools_registered: Vec::new(),
            subscriptions_registered: 0,
            shortcuts_registered: Vec::new(),
        };

        // Index commands
        for cmd in ctx.commands {
            result.commands_registered.push(cmd.name.clone());
            self.commands.insert(cmd.name.clone(), (ext_id.clone(), cmd.handler));
        }

        // Index tools
        for tool in ctx.tools {
            result.tools_registered.push(tool.name.clone());
            self.tools.insert(tool.name.clone(), (ext_id.clone(), tool));
        }

        // Index subscriptions
        for sub in ctx.subscriptions {
            result.subscriptions_registered += 1;
            self.subscriptions
                .entry(sub.event)
                .or_default()
                .push((ext_id.clone(), sub.filter, sub.handler));
        }

        // Index shortcuts
        for shortcut in ctx.shortcuts {
            result.shortcuts_registered.push(shortcut.command.clone());
            self.shortcuts.insert(shortcut.command.clone(), (ext_id.clone(), shortcut));
        }

        // Index renderers
        for renderer in ctx.renderers {
            self.renderers.insert(renderer.content_type.clone(), (ext_id.clone(), renderer.handler));
        }

        // Index flags
        for flag in ctx.flags {
            self.flags.insert(flag.name.clone(), (ext_id.clone(), flag));
        }

        self.extensions.push(ext);
        Ok(result)
    }

    /// Unload an extension by ID.
    pub async fn unload(&mut self, ext_id: &str) -> Result<(), ExtensionActivateError> {
        // Find and deactivate
        let ext = self.extensions.iter().find(|e| e.id() == ext_id).cloned();
        if let Some(ext) = ext {
            ext.deactivate().await?;
        }

        // Remove all registrations for this extension
        self.commands.retain(|_, (owner, _)| owner != ext_id);
        self.tools.retain(|_, (owner, _)| owner != ext_id);
        self.subscriptions.values_mut().for_each(|subs| {
            subs.retain(|(owner, _, _)| owner != ext_id);
        });
        self.shortcuts.retain(|_, (owner, _)| owner != ext_id);
        self.renderers.retain(|_, (owner, _)| owner != ext_id);
        self.flags.retain(|_, (owner, _)| owner != ext_id);
        self.extensions.retain(|e| e.id() != ext_id);
        Ok(())
    }

    /// Dispatch a command by name.
    pub async fn dispatch_command(&self, name: &str, args: Value) -> Result<Value, String> {
        let (_, handler) = self.commands.get(name)
            .ok_or_else(|| format!("command '{}' not found", name))?;
        handler.execute(args).await
    }

    /// Execute an extension-provided tool by name.
    pub async fn execute_tool(&self, name: &str, args: Value) -> Result<ToolHandlerResult, String> {
        let (_, tool) = self.tools.get(name)
            .ok_or_else(|| format!("extension tool '{}' not found", name))?;
        tool.handler.execute(args).await
    }

    /// Fan out a runtime event to all subscribed handlers.
    /// Complexity: O(h) where h is the number of handlers subscribed to this event.
    pub async fn emit(&self, event: RuntimeEvent, payload: Value) {
        if let Some(handlers) = self.subscriptions.get(&event) {
            for (ext_id, filter, handler) in handlers {
                if let Err(e) = handler.handle(event, payload.clone()).await {
                    tracing::warn!(
                        ext_id = ext_id.as_str(),
                        event = ?event,
                        error = e.as_str(),
                        "extension event handler failed"
                    );
                }
            }
        }
    }

    /// Resolve a shortcut to its dispatch target.
    pub fn resolve_shortcut(&self, command: &str) -> Option<&ExtensionShortcut> {
        self.shortcuts.get(command).map(|(_, s)| s)
    }

    /// List all registered commands.
    pub fn list_commands(&self) -> Vec<(&str, &str)> {
        self.commands
            .iter()
            .map(|(name, (owner, _))| (name.as_str(), owner.as_str()))
            .collect()
    }

    /// List all extension-provided tools.
    pub fn list_tools(&self) -> Vec<&ExtensionTool> {
        self.tools.values().map(|(_, t)| t).collect()
    }

    /// List loaded extensions.
    pub fn list_extensions(&self) -> Vec<(&str, &str)> {
        self.extensions
            .iter()
            .map(|e| (e.id(), e.name()))
            .collect()
    }

    /// Get the value of a flag.
    pub fn flag_value(&self, name: &str) -> Option<&Value> {
        self.flags.get(name).map(|(_, f)| &f.default_value)
    }
}

impl Default for ExtensionRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of what an extension registered during activation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadResult {
    pub extension_id: String,
    pub commands_registered: Vec<String>,
    pub tools_registered: Vec<String>,
    pub subscriptions_registered: usize,
    pub shortcuts_registered: Vec<String>,
}

/// Discover extensions from standard directories.
///
/// Search order:
/// 1. Project-local: `<project_root>/.pipit/extensions/`
/// 2. User-global: `~/.config/pipit/extensions/`
/// 3. Explicit paths provided by the caller.
pub fn discover_extension_dirs(project_root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut dirs = Vec::new();

    // Project-local
    let local = project_root.join(".pipit").join("extensions");
    if local.is_dir() {
        dirs.push(local);
    }

    // User-global
    if let Ok(home) = std::env::var("HOME") {
        let global = std::path::PathBuf::from(home)
            .join(".config")
            .join("pipit")
            .join("extensions");
        if global.is_dir() {
            dirs.push(global);
        }
    }

    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestExtension;

    #[async_trait]
    impl RuntimeExtension for TestExtension {
        fn id(&self) -> &str { "test-ext" }
        fn name(&self) -> &str { "Test Extension" }

        async fn activate(&self, ctx: &mut ActivationContext) -> Result<(), ExtensionActivateError> {
            ctx.register_shortcut(ExtensionShortcut {
                command: "/test".to_string(),
                description: "Test shortcut".to_string(),
                dispatch_to: "test:run".to_string(),
            });
            ctx.declare_flag(ExtensionFlag {
                name: "test.enabled".to_string(),
                description: "Enable test mode".to_string(),
                default_value: Value::Bool(true),
            });
            Ok(())
        }

        async fn deactivate(&self) -> Result<(), ExtensionActivateError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn load_and_query_extension() {
        let mut runtime = ExtensionRuntime::new();
        let ext = Arc::new(TestExtension);
        let result = runtime.load(ext).await.unwrap();

        assert_eq!(result.extension_id, "test-ext");
        assert_eq!(result.shortcuts_registered, vec!["/test"]);

        let shortcut = runtime.resolve_shortcut("/test").unwrap();
        assert_eq!(shortcut.dispatch_to, "test:run");

        let exts = runtime.list_extensions();
        assert_eq!(exts.len(), 1);
        assert_eq!(exts[0].0, "test-ext");

        assert_eq!(runtime.flag_value("test.enabled"), Some(&Value::Bool(true)));
    }

    #[tokio::test]
    async fn unload_extension() {
        let mut runtime = ExtensionRuntime::new();
        let ext = Arc::new(TestExtension);
        runtime.load(ext).await.unwrap();
        assert_eq!(runtime.list_extensions().len(), 1);

        runtime.unload("test-ext").await.unwrap();
        assert_eq!(runtime.list_extensions().len(), 0);
        assert!(runtime.resolve_shortcut("/test").is_none());
    }
}
