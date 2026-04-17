//! Task #15: Rule evolution tracking.
//!
//! Rule changes participate in pipit's existing evolution tracking,
//! giving rules the same historical-drift visibility as code.

use crate::rule::RuleId;
use serde::{Deserialize, Serialize};

/// An evolution event for a rule change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleEvolutionEvent {
    /// The rule that changed.
    pub rule_id: RuleId,
    /// Content hash before the change (None for new rules).
    pub before_hash: Option<String>,
    /// Content hash after the change (None for deleted rules).
    pub after_hash: Option<String>,
    /// Author of the change (from VCS if available).
    pub author: Option<String>,
    /// Timestamp of the change (unix ms).
    pub timestamp_ms: u64,
    /// Reason for the change (from commit message or manual annotation).
    pub reason: Option<String>,
    /// The kind of change.
    pub change_kind: RuleChangeKind,
}

/// The kind of rule change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RuleChangeKind {
    Created,
    Modified,
    Deleted,
    KindChanged { from: String, to: String },
    TierChanged { from: String, to: String },
}

/// History of a single rule.
#[derive(Debug, Clone, Default)]
pub struct RuleHistory {
    pub events: Vec<RuleEvolutionEvent>,
}

impl RuleHistory {
    /// Add an event.
    pub fn record(&mut self, event: RuleEvolutionEvent) {
        self.events.push(event);
    }

    /// Most recent change.
    pub fn last_change(&self) -> Option<&RuleEvolutionEvent> {
        self.events.last()
    }

    /// Days since last modification (from a reference timestamp).
    pub fn days_since_last_change(&self, now_ms: u64) -> Option<u64> {
        self.events
            .last()
            .map(|e| (now_ms.saturating_sub(e.timestamp_ms)) / (1000 * 60 * 60 * 24))
    }
}

/// Store for rule evolution events.
#[derive(Debug, Clone, Default)]
pub struct RuleEvolutionStore {
    history: std::collections::HashMap<RuleId, RuleHistory>,
}

impl RuleEvolutionStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a rule change.
    pub fn record(&mut self, event: RuleEvolutionEvent) {
        self.history
            .entry(event.rule_id.clone())
            .or_default()
            .record(event);
    }

    /// Get history for a specific rule.
    pub fn history(&self, id: &RuleId) -> Option<&RuleHistory> {
        self.history.get(id)
    }

    /// Find stale rules (no changes in N days).
    pub fn stale_rules(&self, now_ms: u64, threshold_days: u64) -> Vec<&RuleId> {
        self.history
            .iter()
            .filter_map(|(id, h)| {
                h.days_since_last_change(now_ms)
                    .filter(|d| *d > threshold_days)
                    .map(|_| id)
            })
            .collect()
    }

    /// All rules with history.
    pub fn all_rules(&self) -> impl Iterator<Item = (&RuleId, &RuleHistory)> {
        self.history.iter()
    }
}
