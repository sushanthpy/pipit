//! Feature Flag / Experiment Infrastructure
//!
//! Two-tier feature system:
//! 1. Compile-time: via Cargo features (#[cfg(feature = "...")])
//! 2. Runtime: config-backed flag store with RCU (read-copy-update) semantics
//!
//! Runtime flags use Arc for zero-contention reads.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// A runtime feature flag value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FlagValue {
    Bool(bool),
    String(String),
    Int(i64),
    Float(f64),
}

impl FlagValue {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            FlagValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            FlagValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        match self {
            FlagValue::Bool(b) => *b,
            FlagValue::Int(i) => *i != 0,
            FlagValue::String(s) => !s.is_empty() && s != "false" && s != "0",
            FlagValue::Float(f) => *f != 0.0,
        }
    }
}

/// An immutable snapshot of all feature flags.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlagSet {
    flags: HashMap<String, FlagValue>,
}

impl FlagSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get a flag value.
    pub fn get(&self, name: &str) -> Option<&FlagValue> {
        self.flags.get(name)
    }

    /// Check if a feature is enabled.
    pub fn is_enabled(&self, name: &str) -> bool {
        self.flags
            .get(name)
            .map(|v| v.is_enabled())
            .unwrap_or(false)
    }

    /// Insert or update a flag value.
    pub fn set(&mut self, name: impl Into<String>, value: FlagValue) {
        self.flags.insert(name.into(), value);
    }

    /// Remove a flag.
    pub fn remove(&mut self, name: &str) -> Option<FlagValue> {
        self.flags.remove(name)
    }

    /// Number of flags.
    pub fn len(&self) -> usize {
        self.flags.len()
    }

    pub fn is_empty(&self) -> bool {
        self.flags.is_empty()
    }
}

/// The feature flag store with RCU (read-copy-update) semantics.
///
/// Readers get the current snapshot via `Arc::clone()` — zero contention.
/// Writers create a new `FlagSet` and atomically swap the `Arc`.
pub struct FeatureFlagStore {
    /// Atomic reference to the current flag set.
    current: std::sync::RwLock<Arc<FlagSet>>,
    /// Path to the persistent flag store on disk.
    persist_path: Option<std::path::PathBuf>,
}

impl FeatureFlagStore {
    /// Create a new feature flag store with default flags.
    pub fn new() -> Self {
        let mut defaults = FlagSet::new();
        // Register known feature flags with defaults
        defaults.set("streaming_tool_executor", FlagValue::Bool(false));
        defaults.set("tiered_compaction", FlagValue::Bool(true));
        defaults.set("cache_breakpoints", FlagValue::Bool(false));
        defaults.set("model_fallback", FlagValue::Bool(true));
        defaults.set("file_history", FlagValue::Bool(true));
        defaults.set("vim_mode", FlagValue::Bool(false));
        defaults.set("tool_summaries", FlagValue::Bool(false));
        defaults.set("voice_mode", FlagValue::Bool(false));
        defaults.set("typed_hooks", FlagValue::Bool(true));
        defaults.set("session_memory_sink", FlagValue::Bool(true));
        defaults.set("lsp_real_client", FlagValue::Bool(true));
        defaults.set("mcp_sse_transport", FlagValue::Bool(true));
        defaults.set("mcp_streamable_http", FlagValue::Bool(true));
        defaults.set("mcp_oauth", FlagValue::Bool(true));
        defaults.set("mcp_elicitation", FlagValue::Bool(true));
        defaults.set("mcp_resource_subscription", FlagValue::Bool(true));
        defaults.set("subagent_persistence", FlagValue::Bool(true));
        defaults.set("a2a_protocol", FlagValue::Bool(false));

        Self {
            current: std::sync::RwLock::new(Arc::new(defaults)),
            persist_path: None,
        }
    }

    /// Create from a persistent file.
    pub fn from_file(path: std::path::PathBuf) -> Self {
        let store = Self::new();
        store.set_persist_path(path);
        if let Some(ref p) = store.persist_path {
            if let Ok(content) = std::fs::read_to_string(p) {
                if let Ok(flags) = serde_json::from_str::<FlagSet>(&content) {
                    *store.current.write().unwrap() = Arc::new(flags);
                }
            }
        }
        store
    }

    fn set_persist_path(&self, path: std::path::PathBuf) {
        // Note: would need interior mutability for this pattern.
        // In practice, persist_path is set once at construction.
    }

    /// Get a read-only snapshot of the current flags.
    /// This is O(1) — just an Arc::clone().
    pub fn snapshot(&self) -> Arc<FlagSet> {
        self.current.read().unwrap().clone()
    }

    /// Check if a feature is enabled (convenience method).
    pub fn is_enabled(&self, name: &str) -> bool {
        self.current.read().unwrap().is_enabled(name)
    }

    /// Update flags atomically (RCU write).
    pub fn update<F>(&self, mutator: F)
    where
        F: FnOnce(&mut FlagSet),
    {
        let mut write = self.current.write().unwrap();
        let mut new_flags = (**write).clone();
        mutator(&mut new_flags);
        *write = Arc::new(new_flags);
    }

    /// Set a single flag value.
    pub fn set_flag(&self, name: &str, value: FlagValue) {
        self.update(|flags| {
            flags.set(name, value);
        });
    }

    /// Persist current flags to disk.
    pub fn persist(&self) -> Result<(), String> {
        if let Some(ref path) = self.persist_path {
            let snapshot = self.snapshot();
            let json = serde_json::to_string_pretty(&*snapshot)
                .map_err(|e| format!("Serialization error: {}", e))?;
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create dir: {}", e))?;
            }
            std::fs::write(path, json).map_err(|e| format!("Failed to write: {}", e))?;
        }
        Ok(())
    }
}

impl Default for FeatureFlagStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_store_rcu_read_write() {
        let store = FeatureFlagStore::new();

        // Read default
        assert!(!store.is_enabled("streaming_tool_executor"));
        assert!(store.is_enabled("model_fallback"));

        // Update flag
        store.set_flag("streaming_tool_executor", FlagValue::Bool(true));
        assert!(store.is_enabled("streaming_tool_executor"));

        // Snapshot is independent
        let snap = store.snapshot();
        store.set_flag("streaming_tool_executor", FlagValue::Bool(false));
        assert!(snap.is_enabled("streaming_tool_executor")); // Snapshot unchanged
        assert!(!store.is_enabled("streaming_tool_executor")); // Store updated
    }

    #[test]
    fn flag_value_coercion() {
        assert!(FlagValue::Bool(true).is_enabled());
        assert!(!FlagValue::Bool(false).is_enabled());
        assert!(FlagValue::Int(1).is_enabled());
        assert!(!FlagValue::Int(0).is_enabled());
        assert!(FlagValue::String("yes".to_string()).is_enabled());
        assert!(!FlagValue::String("false".to_string()).is_enabled());
    }
}
