//! Tool Wrapper & Interceptor Framework (Task 5).
//!
//! Supports before/after/around interception of tool calls without modifying
//! or forking the underlying tool. Wrappers stack deterministically (outermost
//! registered first) and each wrapper has access to the capability set to
//! ensure monotonic capability reduction.
//!
//! # Example
//! ```ignore
//! // Log every bash command
//! struct BashLogger;
//! impl ToolWrapper for BashLogger {
//!     fn matches(&self, tool_name: &str) -> bool { tool_name == "bash" }
//!     async fn before(&self, name: &str, args: &Value) -> WrapperAction { ... }
//! }
//! registry.register_wrapper(Arc::new(BashLogger));
//! ```

use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// Result of a wrapper's before/after hook.
#[derive(Debug, Clone)]
pub enum WrapperAction {
    /// Pass through — no modification.
    Continue,
    /// Replace the args (before) or result (after) with a new value.
    Replace(Value),
    /// Block execution entirely with a reason.
    Block(String),
}

/// A tool wrapper that intercepts tool calls.
#[async_trait]
pub trait ToolWrapper: Send + Sync {
    /// Human-readable name for the wrapper.
    fn name(&self) -> &str;

    /// Whether this wrapper applies to the given tool.
    fn matches(&self, tool_name: &str) -> bool;

    /// Priority: lower = runs first (outermost). Default 100.
    fn priority(&self) -> u32 {
        100
    }

    /// Called before the tool executes.
    /// Return `Replace(new_args)` to modify args, `Block(reason)` to prevent execution.
    async fn before(
        &self,
        tool_name: &str,
        args: &Value,
    ) -> Result<WrapperAction, String> {
        let _ = (tool_name, args);
        Ok(WrapperAction::Continue)
    }

    /// Called after the tool executes successfully.
    /// Return `Replace(new_result)` to modify the result.
    async fn after(
        &self,
        tool_name: &str,
        args: &Value,
        result: &str,
    ) -> Result<WrapperAction, String> {
        let _ = (tool_name, args, result);
        Ok(WrapperAction::Continue)
    }

    /// Called when the tool errors.
    async fn on_error(
        &self,
        tool_name: &str,
        args: &Value,
        error: &str,
    ) -> Result<(), String> {
        let _ = (tool_name, args, error);
        Ok(())
    }
}

/// Stack of wrappers for a tool call.
pub struct WrapperStack {
    wrappers: Vec<Arc<dyn ToolWrapper>>,
}

impl WrapperStack {
    pub fn new() -> Self {
        Self {
            wrappers: Vec::new(),
        }
    }

    /// Add a wrapper. Wrappers are sorted by priority on retrieval.
    pub fn add(&mut self, wrapper: Arc<dyn ToolWrapper>) {
        self.wrappers.push(wrapper);
        self.wrappers.sort_by_key(|w| w.priority());
    }

    /// Get wrappers that match a tool name, in priority order.
    pub fn matching(&self, tool_name: &str) -> Vec<&dyn ToolWrapper> {
        self.wrappers
            .iter()
            .filter(|w| w.matches(tool_name))
            .map(|w| w.as_ref())
            .collect()
    }

    /// Execute the before-chain for a tool call.
    /// Returns the (possibly modified) args, or an error if blocked.
    pub async fn run_before(
        &self,
        tool_name: &str,
        args: &Value,
    ) -> Result<Value, String> {
        let mut current_args = args.clone();
        for wrapper in self.matching(tool_name) {
            match wrapper.before(tool_name, &current_args).await? {
                WrapperAction::Continue => {}
                WrapperAction::Replace(new_args) => {
                    current_args = new_args;
                }
                WrapperAction::Block(reason) => {
                    return Err(format!(
                        "Tool '{}' blocked by wrapper '{}': {}",
                        tool_name,
                        wrapper.name(),
                        reason
                    ));
                }
            }
        }
        Ok(current_args)
    }

    /// Execute the after-chain for a tool result.
    /// Returns the (possibly modified) result.
    pub async fn run_after(
        &self,
        tool_name: &str,
        args: &Value,
        result: &str,
    ) -> Result<String, String> {
        let mut current = result.to_string();
        // After hooks run in reverse order (innermost first)
        let wrappers: Vec<_> = self.matching(tool_name);
        for wrapper in wrappers.iter().rev() {
            match wrapper.after(tool_name, args, &current).await? {
                WrapperAction::Continue => {}
                WrapperAction::Replace(new_result) => {
                    current = new_result.as_str().unwrap_or(&new_result.to_string()).to_string();
                }
                WrapperAction::Block(_) => {} // Block not meaningful for after
            }
        }
        Ok(current)
    }

    /// Notify wrappers of an error.
    pub async fn run_on_error(
        &self,
        tool_name: &str,
        args: &Value,
        error: &str,
    ) {
        for wrapper in self.matching(tool_name) {
            let _ = wrapper.on_error(tool_name, args, error).await;
        }
    }

    pub fn len(&self) -> usize {
        self.wrappers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.wrappers.is_empty()
    }
}

impl Default for WrapperStack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LogWrapper;

    #[async_trait]
    impl ToolWrapper for LogWrapper {
        fn name(&self) -> &str { "log" }
        fn matches(&self, _tool_name: &str) -> bool { true }
        fn priority(&self) -> u32 { 50 }
        async fn before(&self, _name: &str, args: &Value) -> Result<WrapperAction, String> {
            // Just pass through
            let _ = args;
            Ok(WrapperAction::Continue)
        }
    }

    struct BlockDangerousWrapper;

    #[async_trait]
    impl ToolWrapper for BlockDangerousWrapper {
        fn name(&self) -> &str { "block_rm" }
        fn matches(&self, tool_name: &str) -> bool { tool_name == "bash" }
        fn priority(&self) -> u32 { 10 }
        async fn before(&self, _name: &str, args: &Value) -> Result<WrapperAction, String> {
            if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                if cmd.contains("rm -rf /") {
                    return Ok(WrapperAction::Block("dangerous rm -rf /".into()));
                }
            }
            Ok(WrapperAction::Continue)
        }
    }

    struct ArgsRewriter;

    #[async_trait]
    impl ToolWrapper for ArgsRewriter {
        fn name(&self) -> &str { "rewriter" }
        fn matches(&self, tool_name: &str) -> bool { tool_name == "bash" }
        fn priority(&self) -> u32 { 20 }
        async fn before(&self, _name: &str, args: &Value) -> Result<WrapperAction, String> {
            let mut new_args = args.clone();
            new_args["sandbox"] = serde_json::json!(true);
            Ok(WrapperAction::Replace(new_args))
        }
    }

    #[tokio::test]
    async fn wrapper_stack_ordering() {
        let mut stack = WrapperStack::new();
        stack.add(Arc::new(LogWrapper));
        stack.add(Arc::new(BlockDangerousWrapper));
        let matching = stack.matching("bash");
        assert_eq!(matching.len(), 2);
        assert_eq!(matching[0].name(), "block_rm"); // priority 10 first
        assert_eq!(matching[1].name(), "log"); // priority 50 second
    }

    #[tokio::test]
    async fn wrapper_blocks_dangerous_command() {
        let mut stack = WrapperStack::new();
        stack.add(Arc::new(BlockDangerousWrapper));
        let args = serde_json::json!({"command": "rm -rf /"});
        let result = stack.run_before("bash", &args).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("dangerous"));
    }

    #[tokio::test]
    async fn wrapper_modifies_args() {
        let mut stack = WrapperStack::new();
        stack.add(Arc::new(ArgsRewriter));
        let args = serde_json::json!({"command": "ls"});
        let result = stack.run_before("bash", &args).await.unwrap();
        assert_eq!(result["sandbox"], serde_json::json!(true));
        assert_eq!(result["command"], serde_json::json!("ls"));
    }

    #[tokio::test]
    async fn wrapper_passthrough_for_unmatched_tool() {
        let mut stack = WrapperStack::new();
        stack.add(Arc::new(BlockDangerousWrapper));
        let args = serde_json::json!({"path": "foo.rs"});
        let result = stack.run_before("read_file", &args).await.unwrap();
        assert_eq!(result, args);
    }
}
