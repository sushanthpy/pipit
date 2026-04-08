//! Denial Tracker — Exponential backoff for repeated permission denials.
//!
//! When a user denies a tool call, we record the denial and apply exponential
//! backoff before re-prompting. This prevents the agent from repeatedly
//! asking for the same dangerous operation.
//!
//! Backoff schedule: 1, 2, 4, 8, 16, 32 turns (capped at 32).
//!
//! Key: SHA-256(tool_name || command || sorted_paths). This groups similar
//! calls (same tool, same command pattern) under one backoff counter.

use crate::{Decision, PermissionMode, PermissionResult, ToolCallDescriptor};
use dashmap::DashMap;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

/// A single denial record.
#[derive(Debug)]
struct DenialRecord {
    /// Number of consecutive denials.
    count: u32,
    /// Turn number of the last denial.
    last_denial_turn: u64,
    /// Turns to skip before re-prompting.
    backoff_turns: u32,
}

impl DenialRecord {
    fn new(turn: u64) -> Self {
        Self {
            count: 1,
            last_denial_turn: turn,
            backoff_turns: 1,
        }
    }

    fn increment(&mut self, turn: u64) {
        self.count += 1;
        self.last_denial_turn = turn;
        // Exponential backoff: 1, 2, 4, 8, 16, 32 (capped)
        self.backoff_turns = (1u32 << self.count.min(5)).min(32);
    }

    fn should_suppress(&self, current_turn: u64) -> bool {
        let turns_since_denial = current_turn.saturating_sub(self.last_denial_turn);
        turns_since_denial < self.backoff_turns as u64
    }
}

/// Thread-safe denial tracker using DashMap.
pub struct DenialTracker {
    records: DashMap<String, DenialRecord>,
    current_turn: AtomicU64,
}

impl DenialTracker {
    pub fn new() -> Self {
        Self {
            records: DashMap::new(),
            current_turn: AtomicU64::new(0),
        }
    }

    /// Advance the turn counter. Call this at the start of each agent turn.
    pub fn advance_turn(&self) {
        self.current_turn.fetch_add(1, Ordering::Relaxed);
    }

    /// Check if a tool call should be suppressed due to recent denials.
    /// Returns Some(PermissionResult) if suppressed, None if not.
    pub fn check(&self, descriptor: &ToolCallDescriptor) -> Option<PermissionResult> {
        let key = denial_key(descriptor);
        let turn = self.current_turn.load(Ordering::Relaxed);

        if let Some(record) = self.records.get(&key) {
            if record.should_suppress(turn) {
                return Some(PermissionResult {
                    decision: Decision::Deny,
                    mode: PermissionMode::Default,
                    classifier_verdicts: HashMap::new(),
                    matched_rule: Some("denial-backoff".to_string()),
                    explanation: format!(
                        "Tool '{}' denied (backoff: {} more turns). Previously denied {} time(s).",
                        descriptor.tool_name,
                        record
                            .backoff_turns
                            .saturating_sub((turn - record.last_denial_turn) as u32),
                        record.count,
                    ),
                });
            }
        }

        None
    }

    /// Record a user denial.
    pub fn record(&self, descriptor: &ToolCallDescriptor) {
        let key = denial_key(descriptor);
        let turn = self.current_turn.load(Ordering::Relaxed);

        self.records
            .entry(key)
            .and_modify(|record| record.increment(turn))
            .or_insert_with(|| DenialRecord::new(turn));
    }

    /// Clear denial record (user explicitly approved after denial).
    pub fn clear(&self, descriptor: &ToolCallDescriptor) {
        let key = denial_key(descriptor);
        self.records.remove(&key);
    }

    /// Number of tracked denial patterns.
    pub fn active_denials(&self) -> usize {
        self.records.len()
    }
}

/// Compute a stable key for a tool call pattern.
/// SHA-256(tool_name || ":" || command || ":" || sorted_paths)
fn denial_key(descriptor: &ToolCallDescriptor) -> String {
    let mut hasher = Sha256::new();
    hasher.update(descriptor.tool_name.as_bytes());
    hasher.update(b":");
    if let Some(ref cmd) = descriptor.command {
        hasher.update(cmd.as_bytes());
    }
    hasher.update(b":");
    let mut paths: Vec<String> = descriptor
        .paths
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    paths.sort();
    for p in &paths {
        hasher.update(p.as_bytes());
        hasher.update(b",");
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_desc() -> ToolCallDescriptor {
        ToolCallDescriptor {
            tool_name: "bash".to_string(),
            args: serde_json::json!({}),
            paths: vec![],
            command: Some("rm -rf important/".to_string()),
            is_mutating: true,
            project_root: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn first_call_not_suppressed() {
        let tracker = DenialTracker::new();
        assert!(tracker.check(&test_desc()).is_none());
    }

    #[test]
    fn denial_triggers_backoff() {
        let tracker = DenialTracker::new();
        let desc = test_desc();
        tracker.record(&desc);
        // Same turn: should be suppressed
        assert!(tracker.check(&desc).is_some());
    }

    #[test]
    fn backoff_expires() {
        let tracker = DenialTracker::new();
        let desc = test_desc();
        tracker.record(&desc);
        // Advance past backoff (1 turn for first denial)
        tracker.advance_turn();
        tracker.advance_turn();
        assert!(tracker.check(&desc).is_none());
    }

    #[test]
    fn exponential_backoff_grows() {
        let tracker = DenialTracker::new();
        let desc = test_desc();
        // Deny 3 times → backoff should be 2^3 = 8 turns
        tracker.record(&desc);
        tracker.advance_turn();
        tracker.advance_turn();
        tracker.record(&desc);
        tracker.advance_turn();
        tracker.advance_turn();
        tracker.advance_turn();
        tracker.advance_turn();
        tracker.record(&desc);
        // After 3 denials, backoff = 8 turns. We're at turn ~7 since last denial.
        // Should still be suppressed at turn 7
        for _ in 0..6 {
            tracker.advance_turn();
        }
        assert!(tracker.check(&desc).is_some());
    }

    #[test]
    fn clear_resets_backoff() {
        let tracker = DenialTracker::new();
        let desc = test_desc();
        tracker.record(&desc);
        assert!(tracker.check(&desc).is_some());
        tracker.clear(&desc);
        assert!(tracker.check(&desc).is_none());
    }
}
