//! Scheduler Boundary Guard — Throughput Preservation
//!
//! Ensures that reliability features (persistence, permission recording,
//! resume bookkeeping) remain orthogonal to the tool scheduling hot path.
//! Durability sits at turn boundaries, not inside the core scheduling logic.
//!
//! The scheduler's inner loop is a conflict-graph partitioner: O(n²) worst case,
//! O(n log n) typical. Adding persistence inside that loop would inflate the
//! inner complexity. This module defines the boundary markers where persistence
//! is safe to inject, and provides hooks for the agent loop to call persistence
//! at the correct positions.
//!
//! Turn lifecycle with persistence boundaries:
//! ```text
//! ┌──────────── Persistence Zone ────────────┐
//! │ InputAccepted → WAL flush                │
//! │ Plan selected → Ledger append            │
//! └──────────────────────────────────────────┘
//!
//! ┌──────────── Hot Zone (NO persistence) ───┐
//! │ Scheduler: build conflict graph          │
//! │ Scheduler: compute independent sets      │
//! │ Scheduler: dispatch concurrent batch     │
//! │ Tool execution (parallel reads)          │
//! │ Tool execution (sequential writes)       │
//! └──────────────────────────────────────────┘
//!
//! ┌──────────── Persistence Zone ────────────┐
//! │ Batch completed → Ledger tool outcomes   │
//! │ Verification → Ledger verdict            │
//! │ Turn finalized  → WAL + ledger flush     │
//! └──────────────────────────────────────────┘
//! ```

use serde::{Deserialize, Serialize};

/// Phases of the turn where persistence operations are safe.
///
/// The key invariant: no persistence inside `SchedulerHot`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceZone {
    /// Before tool scheduling — safe for input recording, plan logging.
    PreSchedule,
    /// Inside the scheduler's hot path — NO persistence allowed.
    SchedulerHot,
    /// After tool execution — safe for outcome recording.
    PostExecution,
    /// After verification — safe for verdict recording.
    PostVerification,
    /// Turn finalization — safe for comprehensive flush.
    TurnFinalize,
}

/// A persistence action to be executed at a zone boundary.
#[derive(Debug, Clone)]
pub enum PersistenceAction {
    /// Flush a user message to WAL (pre-schedule zone).
    FlushUserMessage,
    /// Record plan selection to ledger (pre-schedule zone).
    RecordPlan { strategy: String },
    /// Record tool outcomes to ledger (post-execution zone).
    RecordToolOutcomes { tool_count: usize },
    /// Record verification verdict (post-verification zone).
    RecordVerdict { passed: bool },
    /// Full turn finalization (turn-finalize zone).
    FinalizeTurn { turn_number: u32 },
}

impl PersistenceAction {
    /// Which zone this action belongs to.
    pub fn zone(&self) -> PersistenceZone {
        match self {
            PersistenceAction::FlushUserMessage => PersistenceZone::PreSchedule,
            PersistenceAction::RecordPlan { .. } => PersistenceZone::PreSchedule,
            PersistenceAction::RecordToolOutcomes { .. } => PersistenceZone::PostExecution,
            PersistenceAction::RecordVerdict { .. } => PersistenceZone::PostVerification,
            PersistenceAction::FinalizeTurn { .. } => PersistenceZone::TurnFinalize,
        }
    }

    /// Validate that this action is being executed in the correct zone.
    pub fn validate_zone(&self, current: PersistenceZone) -> bool {
        // SchedulerHot should NEVER have persistence actions
        if current == PersistenceZone::SchedulerHot {
            return false;
        }
        self.zone() == current
    }
}

/// Deferred persistence queue — collects actions during the hot zone
/// and executes them at the next boundary.
///
/// This ensures zero persistence overhead inside the scheduler's inner loop.
pub struct DeferredPersistence {
    /// Pending actions to execute at the next zone boundary.
    pending: Vec<PersistenceAction>,
    /// Current zone.
    current_zone: PersistenceZone,
}

impl DeferredPersistence {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            current_zone: PersistenceZone::PreSchedule,
        }
    }

    /// Enter a new persistence zone.
    pub fn enter_zone(&mut self, zone: PersistenceZone) {
        self.current_zone = zone;
    }

    /// Enqueue an action. If we're in the correct zone, it can be
    /// executed immediately. If in SchedulerHot, it's deferred.
    pub fn enqueue(&mut self, action: PersistenceAction) {
        self.pending.push(action);
    }

    /// Drain all pending actions that are valid for the current zone.
    /// Returns them in FIFO order.
    pub fn drain_for_zone(&mut self, zone: PersistenceZone) -> Vec<PersistenceAction> {
        // Never drain during hot zone
        if zone == PersistenceZone::SchedulerHot {
            return Vec::new();
        }

        self.current_zone = zone;

        let mut ready = Vec::new();
        let mut keep = Vec::new();

        for action in self.pending.drain(..) {
            // Actions whose zone has passed are also drained (not lost)
            if action.zone() as u8 <= zone as u8 {
                ready.push(action);
            } else {
                keep.push(action);
            }
        }

        self.pending = keep;
        ready
    }

    /// Check if there are deferred actions waiting.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Current zone.
    pub fn current_zone(&self) -> PersistenceZone {
        self.current_zone
    }
}

impl Default for DeferredPersistence {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_persistence_in_hot_zone() {
        let action = PersistenceAction::RecordToolOutcomes { tool_count: 3 };
        assert!(!action.validate_zone(PersistenceZone::SchedulerHot));
    }

    #[test]
    fn test_deferred_drain() {
        let mut dp = DeferredPersistence::new();

        // Queue actions
        dp.enqueue(PersistenceAction::FlushUserMessage);
        dp.enqueue(PersistenceAction::RecordPlan { strategy: "MinimalPatch".into() });
        dp.enqueue(PersistenceAction::RecordToolOutcomes { tool_count: 5 });

        // Enter hot zone — nothing should drain
        dp.enter_zone(PersistenceZone::SchedulerHot);
        let drained = dp.drain_for_zone(PersistenceZone::SchedulerHot);
        assert!(drained.is_empty());
        assert!(dp.has_pending());

        // Post-execution — should drain pre-schedule AND post-execution actions
        let drained = dp.drain_for_zone(PersistenceZone::PostExecution);
        assert_eq!(drained.len(), 3);
        assert!(!dp.has_pending());
    }

    #[test]
    fn test_zone_ordering() {
        // Verify zone ordering is correct for the drain logic
        assert!((PersistenceZone::PreSchedule as u8) < (PersistenceZone::SchedulerHot as u8));
        assert!((PersistenceZone::SchedulerHot as u8) < (PersistenceZone::PostExecution as u8));
        assert!((PersistenceZone::PostExecution as u8) < (PersistenceZone::PostVerification as u8));
        assert!((PersistenceZone::PostVerification as u8) < (PersistenceZone::TurnFinalize as u8));
    }
}
