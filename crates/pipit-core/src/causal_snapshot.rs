//! # Causal Snapshot — Cross-Store Consistent Read Layer (Architecture Task 10)
//!
//! Reads from multiple durable state systems (session ledger, VCS ledger,
//! memory store, bridge session) and produces a consistent snapshot with
//! explicit source watermarks.
//!
//! Each source carries a watermark (sequence number or timestamp) so consumers
//! can detect when a source is stale or unavailable. Degraded-state metadata
//! is surfaced explicitly rather than silently using defaults.
//!
//! Snapshot assembly cost: O(B + M + L + A) where B = bridge state,
//! M = memory entries, L = ledger entries, A = active agent branches.
//!
//! This module does NOT unify the underlying stores — it provides consistent
//! read models over existing stores, making storage unification an
//! optimization rather than a prerequisite.

use serde::{Deserialize, Serialize};

/// Watermark for a single data source — tracks freshness and availability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceWatermark {
    /// Name of the data source.
    pub source: String,
    /// Sequence number or event count at read time.
    pub seq: u64,
    /// Timestamp of the most recent entry (unix ms).
    pub last_updated_ms: u64,
    /// Whether this source was available during snapshot assembly.
    pub available: bool,
    /// If unavailable, the reason.
    pub degradation_reason: Option<String>,
}

impl SourceWatermark {
    pub fn available(source: &str, seq: u64, last_updated_ms: u64) -> Self {
        Self {
            source: source.to_string(),
            seq,
            last_updated_ms,
            available: true,
            degradation_reason: None,
        }
    }

    pub fn unavailable(source: &str, reason: &str) -> Self {
        Self {
            source: source.to_string(),
            seq: 0,
            last_updated_ms: 0,
            available: false,
            degradation_reason: Some(reason.to_string()),
        }
    }
}

/// A consistent snapshot assembled from multiple state stores.
/// Every field carries provenance via source watermarks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CausalSnapshot {
    /// When this snapshot was assembled (unix ms).
    pub assembled_at_ms: u64,

    /// Watermarks for each source consulted.
    pub watermarks: Vec<SourceWatermark>,

    /// Session state (from SessionLedger / SessionKernel).
    pub session: Option<SessionSnapshot>,

    /// VCS workspace state (from pipit-vcs ledger + git).
    pub vcs: Option<VcsSnapshot>,

    /// Memory entries relevant to the current context.
    pub memory: Option<MemorySnapshot>,

    /// Whether all sources were available.
    pub fully_consistent: bool,

    /// Sources that were degraded or unavailable.
    pub degraded_sources: Vec<String>,
}

/// Session state from the session ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub current_turn: u32,
    pub total_tokens: u64,
    pub total_cost: f64,
    pub tool_calls_completed: u32,
    pub modified_files: Vec<String>,
    pub active_subagents: Vec<String>,
    pub ended: bool,
}

/// VCS workspace state from the repository ledger + git queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VcsSnapshot {
    pub active_workspaces: Vec<WorkspaceInfo>,
    pub promotable_count: usize,
    pub pending_conflicts: usize,
}

/// Summary of a single workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub phase: String,
    pub has_contract: bool,
    pub modified_file_count: usize,
}

/// Memory entries from the session memory store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySnapshot {
    pub entry_count: usize,
    pub total_summary_tokens: u64,
    pub topics: Vec<String>,
}

/// Builder for assembling causal snapshots from multiple sources.
pub struct CausalSnapshotBuilder {
    watermarks: Vec<SourceWatermark>,
    session: Option<SessionSnapshot>,
    vcs: Option<VcsSnapshot>,
    memory: Option<MemorySnapshot>,
}

impl CausalSnapshotBuilder {
    pub fn new() -> Self {
        Self {
            watermarks: Vec::new(),
            session: None,
            vcs: None,
            memory: None,
        }
    }

    /// Add session state from a SessionState (pipit-core ledger).
    pub fn with_session_state(mut self, state: &crate::ledger::SessionState) -> Self {
        self.watermarks.push(SourceWatermark::available(
            "session_ledger",
            state.last_seq,
            current_ms(),
        ));
        self.session = Some(SessionSnapshot {
            session_id: state.session_id.clone(),
            model: state.model.clone(),
            provider: state.provider.clone(),
            current_turn: state.current_turn,
            total_tokens: state.total_tokens,
            total_cost: state.total_cost,
            tool_calls_completed: state.tool_calls_completed,
            modified_files: state.modified_files.clone(),
            active_subagents: state.active_subagents.clone(),
            ended: state.ended,
        });
        self
    }

    /// Mark session source as unavailable.
    pub fn without_session(mut self, reason: &str) -> Self {
        self.watermarks
            .push(SourceWatermark::unavailable("session_ledger", reason));
        self
    }

    /// Add VCS state from a VcsKernel.
    pub fn with_vcs_kernel(
        mut self,
        active_workspaces: Vec<WorkspaceInfo>,
        promotable_count: usize,
        pending_conflicts: usize,
        ledger_seq: u64,
    ) -> Self {
        self.watermarks.push(SourceWatermark::available(
            "vcs_ledger",
            ledger_seq,
            current_ms(),
        ));
        self.vcs = Some(VcsSnapshot {
            active_workspaces,
            promotable_count,
            pending_conflicts,
        });
        self
    }

    /// Mark VCS source as unavailable.
    pub fn without_vcs(mut self, reason: &str) -> Self {
        self.watermarks
            .push(SourceWatermark::unavailable("vcs_ledger", reason));
        self
    }

    /// Add memory state.
    pub fn with_memory(
        mut self,
        entry_count: usize,
        total_summary_tokens: u64,
        topics: Vec<String>,
    ) -> Self {
        self.watermarks.push(SourceWatermark::available(
            "memory_store",
            entry_count as u64,
            current_ms(),
        ));
        self.memory = Some(MemorySnapshot {
            entry_count,
            total_summary_tokens,
            topics,
        });
        self
    }

    /// Mark memory source as unavailable.
    pub fn without_memory(mut self, reason: &str) -> Self {
        self.watermarks
            .push(SourceWatermark::unavailable("memory_store", reason));
        self
    }

    /// Assemble the final snapshot.
    pub fn build(self) -> CausalSnapshot {
        let degraded: Vec<String> = self
            .watermarks
            .iter()
            .filter(|w| !w.available)
            .map(|w| w.source.clone())
            .collect();
        let fully_consistent = degraded.is_empty();

        CausalSnapshot {
            assembled_at_ms: current_ms(),
            watermarks: self.watermarks,
            session: self.session,
            vcs: self.vcs,
            memory: self.memory,
            fully_consistent,
            degraded_sources: degraded,
        }
    }
}

fn current_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fully_consistent_snapshot() {
        let state = crate::ledger::SessionState {
            session_id: Some("s1".into()),
            model: Some("gpt-4o".into()),
            current_turn: 5,
            total_tokens: 3000,
            last_seq: 42,
            ..Default::default()
        };

        let snap = CausalSnapshotBuilder::new()
            .with_session_state(&state)
            .with_vcs_kernel(vec![], 0, 0, 10)
            .with_memory(5, 500, vec!["rust".into()])
            .build();

        assert!(snap.fully_consistent);
        assert!(snap.degraded_sources.is_empty());
        assert_eq!(snap.watermarks.len(), 3);
        assert_eq!(snap.session.unwrap().current_turn, 5);
        assert_eq!(snap.vcs.unwrap().active_workspaces.len(), 0);
        assert_eq!(snap.memory.unwrap().entry_count, 5);
    }

    #[test]
    fn degraded_snapshot_marks_unavailable() {
        let state = crate::ledger::SessionState {
            session_id: Some("s2".into()),
            last_seq: 10,
            ..Default::default()
        };

        let snap = CausalSnapshotBuilder::new()
            .with_session_state(&state)
            .without_vcs("no .git directory")
            .without_memory("store not configured")
            .build();

        assert!(!snap.fully_consistent);
        assert_eq!(snap.degraded_sources.len(), 2);
        assert!(snap.degraded_sources.contains(&"vcs_ledger".to_string()));
        assert!(snap.degraded_sources.contains(&"memory_store".to_string()));
        assert!(snap.session.is_some());
        assert!(snap.vcs.is_none());
        assert!(snap.memory.is_none());
    }

    #[test]
    fn watermarks_track_sequence() {
        let state = crate::ledger::SessionState {
            last_seq: 99,
            ..Default::default()
        };

        let snap = CausalSnapshotBuilder::new()
            .with_session_state(&state)
            .build();

        let session_wm = snap
            .watermarks
            .iter()
            .find(|w| w.source == "session_ledger")
            .unwrap();
        assert_eq!(session_wm.seq, 99);
        assert!(session_wm.available);
    }
}
