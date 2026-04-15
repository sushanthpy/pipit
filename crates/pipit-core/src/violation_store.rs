//! # Violation Store — Structured Security Event Tracking
//!
//! Every permission/sandbox denial is a first-class, queryable, exportable event.
//! Tracks violations with enough structure to:
//!   - Surface "you tried X 3 times and it was denied — stop trying" to the planner
//!   - Compute "top denied domains last 30 days" for policy learning
//!   - Enable `pipit sessions inspect --denials` for post-hoc analysis
//!   - Feed a future policy-learning loop
//!
//! Storage: in-memory for the session, with optional sled-backed persistence.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// A structured security violation event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViolationEvent {
    /// Unique event ID.
    pub id: String,
    /// Session ID this violation belongs to.
    pub session_id: String,
    /// Turn number within the session.
    pub turn: u32,
    /// Sequence within the turn (for multiple violations in one turn).
    pub seq: u32,
    /// The tool that was denied.
    pub tool: String,
    /// The rule that triggered the denial.
    pub rule_id: String,
    /// Category of the violation.
    pub category: String,
    /// The command or action that was denied.
    pub command: String,
    /// Human-readable reason for the denial.
    pub reason: String,
    /// Timestamp of the violation (seconds since epoch).
    pub timestamp_epoch: u64,
}

/// Aggregated statistics for a specific rule.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RuleStats {
    pub rule_id: String,
    pub count: u64,
    pub last_seen_epoch: Option<u64>,
    pub categories: Vec<String>,
}

/// The violation store — collects and queries security violations.
#[derive(Debug, Clone)]
pub struct ViolationStore {
    inner: Arc<Mutex<ViolationStoreInner>>,
}

#[derive(Debug)]
struct ViolationStoreInner {
    session_id: String,
    events: Vec<ViolationEvent>,
    /// Per-rule aggregate: rule_id → (count, last_seen_epoch)
    rule_counts: HashMap<String, (u64, u64)>,
    /// Per-turn sequence counter
    turn_seq: HashMap<u32, u32>,
}

impl ViolationStore {
    /// Create a new violation store for a session.
    pub fn new(session_id: String) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ViolationStoreInner {
                session_id,
                events: Vec::new(),
                rule_counts: HashMap::new(),
                turn_seq: HashMap::new(),
            })),
        }
    }

    /// Record a violation event.
    pub fn record(
        &self,
        turn: u32,
        tool: &str,
        rule_id: &str,
        category: &str,
        command: &str,
        reason: &str,
    ) {
        let mut inner = self.inner.lock().unwrap();
        let seq = inner.turn_seq.entry(turn).or_insert(0);
        *seq += 1;
        let current_seq = *seq;

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let event = ViolationEvent {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: inner.session_id.clone(),
            turn,
            seq: current_seq,
            tool: tool.to_string(),
            rule_id: rule_id.to_string(),
            category: category.to_string(),
            command: if command.len() > 1000 {
                format!("{}...", &command[..1000])
            } else {
                command.to_string()
            },
            reason: reason.to_string(),
            timestamp_epoch: now,
        };

        // Update aggregates
        let entry = inner.rule_counts.entry(rule_id.to_string()).or_insert((0, now));
        entry.0 += 1;
        entry.1 = now;

        inner.events.push(event);
    }

    /// Get violation count for a specific rule in this session.
    pub fn rule_count(&self, rule_id: &str) -> u64 {
        let inner = self.inner.lock().unwrap();
        inner.rule_counts.get(rule_id).map(|(c, _)| *c).unwrap_or(0)
    }

    /// Get total violation count for this session.
    pub fn total_count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.events.len()
    }

    /// Get the top-N most frequently denied rules.
    pub fn top_denied_rules(&self, n: usize) -> Vec<RuleStats> {
        let inner = self.inner.lock().unwrap();
        let mut rules: Vec<RuleStats> = inner
            .rule_counts
            .iter()
            .map(|(rule_id, &(count, last_seen))| RuleStats {
                rule_id: rule_id.clone(),
                count,
                last_seen_epoch: Some(last_seen),
                categories: vec![],
            })
            .collect();
        rules.sort_by(|a, b| b.count.cmp(&a.count));
        rules.truncate(n);
        rules
    }

    /// Generate a planner-friendly summary of recent denials for this session.
    /// Returns None if no violations have occurred.
    pub fn planner_summary(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        if inner.events.is_empty() {
            return None;
        }

        let mut summary = String::from("Previous security denials this session:\n");
        let mut rules: Vec<(&String, &(u64, u64))> =
            inner.rule_counts.iter().collect();
        rules.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));

        for (rule_id, (count, _)) in rules.iter().take(5) {
            summary.push_str(&format!("  - {} denied {} time(s)\n", rule_id, count));
        }

        if inner.events.len() > 5 {
            summary.push_str(&format!(
                "  ({} total violations — stop retrying denied commands)\n",
                inner.events.len()
            ));
        }

        Some(summary)
    }

    /// Get all events for export/inspection.
    pub fn all_events(&self) -> Vec<ViolationEvent> {
        let inner = self.inner.lock().unwrap();
        inner.events.clone()
    }

    /// Get events for a specific turn.
    pub fn events_for_turn(&self, turn: u32) -> Vec<ViolationEvent> {
        let inner = self.inner.lock().unwrap();
        inner
            .events
            .iter()
            .filter(|e| e.turn == turn)
            .cloned()
            .collect()
    }

    /// Check if a specific command pattern has been denied repeatedly (3+ times).
    /// Used by the planner to suppress repeated attempts.
    pub fn is_repeated_denial(&self, rule_id: &str) -> bool {
        self.rule_count(rule_id) >= 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_query() {
        let store = ViolationStore::new("test-session".into());

        store.record(1, "bash", "ifs_injection", "IfsInjection", "IFS=/ cmd", "blocked IFS");
        store.record(1, "bash", "ifs_injection", "IfsInjection", "IFS=. cmd", "blocked IFS");
        store.record(2, "bash", "eval", "ShellQuoteEscape", "eval $(foo)", "blocked eval");

        assert_eq!(store.total_count(), 3);
        assert_eq!(store.rule_count("ifs_injection"), 2);
        assert_eq!(store.rule_count("eval"), 1);
        assert!(!store.is_repeated_denial("ifs_injection")); // only 2, need 3
    }

    #[test]
    fn repeated_denial_detection() {
        let store = ViolationStore::new("test-session".into());

        for i in 0..5 {
            store.record(i, "bash", "path_hijack", "PathHijack", "PATH=/tmp", "blocked");
        }

        assert!(store.is_repeated_denial("path_hijack")); // 5 >= 3
    }

    #[test]
    fn planner_summary() {
        let store = ViolationStore::new("test-session".into());
        assert!(store.planner_summary().is_none());

        store.record(1, "bash", "eval", "ShellQuote", "eval foo", "blocked");
        let summary = store.planner_summary().unwrap();
        assert!(summary.contains("eval"));
    }

    #[test]
    fn top_denied() {
        let store = ViolationStore::new("test-session".into());
        store.record(1, "bash", "eval", "ShellQuote", "eval", "blocked");
        store.record(1, "bash", "eval", "ShellQuote", "eval", "blocked");
        store.record(1, "bash", "ifs", "IFS", "IFS=", "blocked");

        let top = store.top_denied_rules(10);
        assert_eq!(top[0].rule_id, "eval");
        assert_eq!(top[0].count, 2);
    }
}
