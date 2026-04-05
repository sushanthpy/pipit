//! Permission-Denial Accumulator & Analytics Fanout
//!
//! A session-scoped accumulator that captures every tool-authorization denial
//! with structured metadata: tool name, denial source, timestamp, and optional
//! user feedback. Fans out to both the `TelemetryFacade` (OTel counters) and
//! the `SessionLedger` (replay integrity).
//!
//! Allocation strategy: zero cost on the Allow path. `DenialSource` is a
//! stack-allocated enum (≤48 bytes). Accumulator is a `Vec` with amortized
//! O(1) push.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Why a tool invocation was denied.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DenialSource {
    /// Denied by configuration rule (approval mode insufficient).
    Config {
        rule: String,
    },
    /// User explicitly rejected the approval prompt.
    UserReject {
        has_feedback: bool,
        feedback: Option<String>,
    },
    /// User aborted (Ctrl-C during approval prompt).
    UserAbort,
    /// Extension hook returned deny.
    Hook {
        hook_name: String,
    },
    /// Governor risk-score threshold exceeded.
    GovernorRisk {
        risk_score: f64,
        threshold: f64,
    },
    /// PolicyKernel lattice check failed (requested ⊄ granted).
    LatticeViolation {
        requested: String,
        granted: String,
    },
    /// Resource scope violation (path outside project, blocked command).
    ScopeViolation {
        resource: String,
        reason: String,
    },
}

/// A single recorded permission denial.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionDenialRecord {
    /// Which tool was denied.
    pub tool_name: String,
    /// The tool call ID (for correlation).
    pub call_id: String,
    /// Why it was denied.
    pub source: DenialSource,
    /// When the denial occurred (unix millis).
    pub timestamp_ms: u64,
    /// Turn number in the session.
    pub turn: u32,
}

/// Session-scoped accumulator for permission denials.
///
/// Thread-safe via interior `Mutex`. The hot path (Allow) does not touch
/// this structure. Only the Deny/Ask-rejected path appends.
pub struct PermissionLedger {
    records: Mutex<Vec<PermissionDenialRecord>>,
}

impl PermissionLedger {
    pub fn new() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
        }
    }

    /// Record a denial. O(1) amortized (Vec push).
    pub fn record_denial(
        &self,
        tool_name: &str,
        call_id: &str,
        source: DenialSource,
        turn: u32,
    ) {
        let record = PermissionDenialRecord {
            tool_name: tool_name.to_string(),
            call_id: call_id.to_string(),
            source,
            timestamp_ms: current_millis(),
            turn,
        };

        if let Ok(mut records) = self.records.lock() {
            records.push(record);
        }
    }

    /// Drain all records (ownership transfer to caller).
    pub fn drain(&self) -> Vec<PermissionDenialRecord> {
        self.records
            .lock()
            .map(|mut r| std::mem::take(&mut *r))
            .unwrap_or_default()
    }

    /// Snapshot without draining (for mid-session telemetry).
    pub fn snapshot(&self) -> Vec<PermissionDenialRecord> {
        self.records
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Count denials by source type.
    pub fn counts_by_source(&self) -> DenialCounts {
        let records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let mut counts = DenialCounts::default();
        for r in records.iter() {
            match &r.source {
                DenialSource::Config { .. } => counts.config += 1,
                DenialSource::UserReject { .. } => counts.user_reject += 1,
                DenialSource::UserAbort => counts.user_abort += 1,
                DenialSource::Hook { .. } => counts.hook += 1,
                DenialSource::GovernorRisk { .. } => counts.governor += 1,
                DenialSource::LatticeViolation { .. } => counts.lattice += 1,
                DenialSource::ScopeViolation { .. } => counts.scope += 1,
            }
        }
        counts
    }

    /// Total number of denials in this session.
    pub fn total(&self) -> usize {
        self.records
            .lock()
            .map(|r| r.len())
            .unwrap_or(0)
    }
}

impl Default for PermissionLedger {
    fn default() -> Self {
        Self::new()
    }
}

/// Aggregated denial counts by source type (for telemetry fanout).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DenialCounts {
    pub config: u32,
    pub user_reject: u32,
    pub user_abort: u32,
    pub hook: u32,
    pub governor: u32,
    pub lattice: u32,
    pub scope: u32,
}

impl DenialCounts {
    pub fn total(&self) -> u32 {
        self.config + self.user_reject + self.user_abort
            + self.hook + self.governor + self.lattice + self.scope
    }
}

fn current_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_and_drain() {
        let ledger = PermissionLedger::new();

        ledger.record_denial(
            "bash",
            "call-1",
            DenialSource::Config { rule: "suggest_mode".into() },
            1,
        );
        ledger.record_denial(
            "write_file",
            "call-2",
            DenialSource::UserReject { has_feedback: true, feedback: Some("too risky".into()) },
            2,
        );
        ledger.record_denial(
            "bash",
            "call-3",
            DenialSource::GovernorRisk { risk_score: 0.95, threshold: 0.8 },
            3,
        );

        assert_eq!(ledger.total(), 3);

        let counts = ledger.counts_by_source();
        assert_eq!(counts.config, 1);
        assert_eq!(counts.user_reject, 1);
        assert_eq!(counts.governor, 1);

        let drained = ledger.drain();
        assert_eq!(drained.len(), 3);
        assert_eq!(ledger.total(), 0);
    }

    #[test]
    fn test_snapshot_does_not_drain() {
        let ledger = PermissionLedger::new();
        ledger.record_denial("bash", "c1", DenialSource::UserAbort, 1);

        let snap = ledger.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(ledger.total(), 1); // still there
    }
}
