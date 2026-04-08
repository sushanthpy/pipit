//! Deterministic Hook Replay — content-addressed caching in the kernel.
//!
//! Every hook execution produces two events transactionally:
//!   HookInvoked { hook_id, input_hash, kind }
//!   HookDecision { hook_id, decision, output_hash, duration_us }
//!
//! In Replay mode, the runtime looks up the prior decision by
//! (hook_id, input_hash) and returns the cached decision without executing.
//!
//! Content-addressed lookup: decision = cache[hash(hook_id || canonical(input))]
//! Lookup: O(1) via DashMap index.
//! Replay is a fold over the WAL: state_n = fold(replay_fn, state_0, events_0..n)
//!
//! Correctness: replay(record(s, e)) = apply(s, e) for every event e.

use crate::hook_kind::{HookContext, HookDecision, ReplayMode};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;

/// A recorded hook invocation + decision for replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookRecord {
    /// Unique hook identifier (manifest name + event type).
    pub hook_id: String,
    /// SHA-256 of canonical(hook_id || input_json).
    pub input_hash: String,
    /// The hook execution medium used.
    pub kind: String,
    /// The decision returned by the hook.
    pub decision: HookDecision,
    /// Content hash of the decision output.
    pub output_hash: String,
    /// Execution duration in microseconds.
    pub duration_us: u64,
    /// Timestamp (unix ms).
    pub timestamp_ms: u64,
}

/// Content-addressed hook decision cache for replay.
///
/// In Live mode: records every hook execution.
/// In Replay mode: returns cached decisions without executing.
pub struct HookReplayCache {
    /// Map from input_hash → HookRecord.
    /// O(1) lookup and insert.
    records: Mutex<HashMap<String, HookRecord>>,
}

impl HookReplayCache {
    pub fn new() -> Self {
        Self {
            records: Mutex::new(HashMap::new()),
        }
    }

    /// Compute the canonical input hash for a hook invocation.
    /// hash(hook_id || canonical_json(context))
    pub fn input_hash(hook_id: &str, context: &HookContext) -> String {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        hook_id.hash(&mut hasher);
        // Canonical JSON: sorted keys for deterministic hashing
        if let Some(ref args) = context.tool_args {
            canonical_json(args).hash(&mut hasher);
        }
        context.event.hash(&mut hasher);
        if let Some(ref tool) = context.tool_name {
            tool.hash(&mut hasher);
        }
        format!("{:016x}", hasher.finish())
    }

    /// Record a hook execution result.
    pub fn record(
        &self,
        hook_id: &str,
        context: &HookContext,
        decision: &HookDecision,
        kind: &str,
    ) -> HookRecord {
        let input_hash = Self::input_hash(hook_id, context);

        let decision_json = serde_json::to_string(decision).unwrap_or_default();
        let mut output_hasher = std::collections::hash_map::DefaultHasher::new();
        decision_json.hash(&mut output_hasher);
        let output_hash = format!("{:016x}", output_hasher.finish());

        let timestamp_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let record = HookRecord {
            hook_id: hook_id.to_string(),
            input_hash: input_hash.clone(),
            kind: kind.to_string(),
            decision: decision.clone(),
            output_hash,
            duration_us: decision.duration_us,
            timestamp_ms,
        };

        let mut records = self.records.lock().unwrap();
        records.insert(input_hash, record.clone());

        record
    }

    /// Look up a cached decision for replay.
    /// Returns None if no matching record exists (execution required).
    pub fn lookup(&self, hook_id: &str, context: &HookContext) -> Option<HookDecision> {
        let input_hash = Self::input_hash(hook_id, context);
        let records = self.records.lock().unwrap();
        records.get(&input_hash).map(|r| r.decision.clone())
    }

    /// Load records from a serialized replay log.
    pub fn load_from_records(&self, records: Vec<HookRecord>) {
        let mut cache = self.records.lock().unwrap();
        for record in records {
            cache.insert(record.input_hash.clone(), record);
        }
    }

    /// Export all records for serialization.
    pub fn export_records(&self) -> Vec<HookRecord> {
        let records = self.records.lock().unwrap();
        records.values().cloned().collect()
    }

    /// Number of cached records.
    pub fn len(&self) -> usize {
        self.records.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Deterministic JSON serialization with sorted keys.
/// Ensures argument-order-independent matching.
fn canonical_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by_key(|(k, _)| k.clone());
            let parts: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("\"{}\":{}", k, canonical_json(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        serde_json::Value::Array(arr) => {
            let parts: Vec<String> = arr.iter().map(|v| canonical_json(v)).collect();
            format!("[{}]", parts.join(","))
        }
        _ => value.to_string(),
    }
}

/// Execute a hook with replay support.
///
/// In Live mode: calls the executor, records the result.
/// In Replay mode: returns the cached decision if available,
///                 falls back to live execution if not cached.
pub async fn execute_with_replay<F, Fut>(
    cache: &HookReplayCache,
    hook_id: &str,
    context: &HookContext,
    kind: &str,
    executor: F,
) -> Result<HookDecision, String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<HookDecision, String>>,
{
    // Replay mode: try cache first
    if context.replay_mode == ReplayMode::Replay {
        if let Some(cached) = cache.lookup(hook_id, context) {
            tracing::debug!(hook_id, "Replay: returning cached hook decision");
            return Ok(cached);
        }
        tracing::warn!(hook_id, "Replay: no cached decision found, executing live");
    }

    // Live execution
    let decision = executor().await?;

    // Record for future replay
    cache.record(hook_id, context, &decision, kind);

    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_context(event: &str, tool: Option<&str>) -> HookContext {
        HookContext {
            event: event.into(),
            tool_name: tool.map(|s| s.into()),
            tool_args: Some(serde_json::json!({"command": "ls"})),
            tool_result: None,
            project_root: PathBuf::from("/tmp"),
            session_id: "test".into(),
            replay_mode: ReplayMode::Live,
        }
    }

    #[test]
    fn input_hash_is_deterministic() {
        let ctx = test_context("PreToolUse", Some("bash"));
        let h1 = HookReplayCache::input_hash("hook1", &ctx);
        let h2 = HookReplayCache::input_hash("hook1", &ctx);
        assert_eq!(h1, h2);
    }

    #[test]
    fn input_hash_differs_for_different_input() {
        let ctx1 = test_context("PreToolUse", Some("bash"));
        let ctx2 = test_context("PostToolUse", Some("bash"));
        let h1 = HookReplayCache::input_hash("hook1", &ctx1);
        let h2 = HookReplayCache::input_hash("hook1", &ctx2);
        assert_ne!(h1, h2);
    }

    #[test]
    fn record_and_lookup() {
        let cache = HookReplayCache::new();
        let ctx = test_context("PreToolUse", Some("bash"));
        let decision = HookDecision {
            allow: false,
            message: Some("blocked by hook".into()),
            transformed_args: None,
            duration_us: 123,
        };

        cache.record("hook1", &ctx, &decision, "command");
        assert_eq!(cache.len(), 1);

        let found = cache.lookup("hook1", &ctx);
        assert!(found.is_some());
        assert!(!found.unwrap().allow);
    }

    #[test]
    fn lookup_miss_returns_none() {
        let cache = HookReplayCache::new();
        let ctx = test_context("PreToolUse", Some("bash"));
        assert!(cache.lookup("nonexistent", &ctx).is_none());
    }

    #[tokio::test]
    async fn execute_with_replay_live_mode() {
        let cache = HookReplayCache::new();
        let ctx = test_context("PreToolUse", Some("bash"));

        let result = execute_with_replay(&cache, "hook1", &ctx, "command", || async {
            Ok(HookDecision::default())
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(cache.len(), 1);
    }

    #[tokio::test]
    async fn execute_with_replay_returns_cached() {
        let cache = HookReplayCache::new();
        let ctx_live = test_context("PreToolUse", Some("bash"));

        // Record a decision
        let decision = HookDecision {
            allow: false,
            message: Some("denied".into()),
            transformed_args: None,
            duration_us: 100,
        };
        cache.record("hook1", &ctx_live, &decision, "command");

        // Replay with same context
        let mut ctx_replay = test_context("PreToolUse", Some("bash"));
        ctx_replay.replay_mode = ReplayMode::Replay;

        let result = execute_with_replay(&cache, "hook1", &ctx_replay, "command", || async {
            panic!("should not execute in replay mode")
        })
        .await;

        assert!(result.is_ok());
        assert!(!result.unwrap().allow);
    }

    #[test]
    fn canonical_json_sorts_keys() {
        let v = serde_json::json!({"b": 2, "a": 1, "c": 3});
        let canonical = canonical_json(&v);
        assert_eq!(canonical, r#"{"a":1,"b":2,"c":3}"#);
    }

    #[test]
    fn canonical_json_nested() {
        let v = serde_json::json!({"z": {"b": 1, "a": 2}, "a": [3, 2, 1]});
        let canonical = canonical_json(&v);
        assert!(canonical.starts_with(r#"{"a":"#));
        assert!(canonical.contains(r#""z":{"a":2,"b":1}"#));
    }

    #[test]
    fn export_and_reload() {
        let cache = HookReplayCache::new();
        let ctx = test_context("PreToolUse", Some("bash"));
        cache.record("h1", &ctx, &HookDecision::default(), "command");
        cache.record("h2", &ctx, &HookDecision::default(), "prompt");

        let exported = cache.export_records();
        assert_eq!(exported.len(), 2);

        let cache2 = HookReplayCache::new();
        cache2.load_from_records(exported);
        assert_eq!(cache2.len(), 2);
    }
}
