//! Ambient Triage Coprocessor
//!
//! A kernel-side observer that watches latency, uncertainty spikes,
//! permission prompts, background completions, and plan drift.
//! Emits terse status annotations — never conversational roleplay.
//!
//! Event ranking: p(e) = w_risk·r_risk + w_latency·r_latency
//!                     + w_relevance·r_relevance - w_noise·r_noise
//! Bounded priority queue: O(log n) insert, O(1) top-event read.

use serde::{Deserialize, Serialize};
use std::collections::BinaryHeap;
use std::cmp::Ordering;

// ─── Triage Events ──────────────────────────────────────────────────────

/// An event the coprocessor may surface to the operator.
#[derive(Debug, Clone)]
pub struct TriageEvent {
    /// Unique event identifier.
    pub id: u64,
    /// Event kind.
    pub kind: TriageEventKind,
    /// Priority score (higher = more important). Range 0.0–1.0.
    pub priority: f64,
    /// When this event was generated (unix timestamp ms).
    pub timestamp_ms: u64,
    /// Short annotation text (1–2 sentences max).
    pub annotation: String,
    /// Whether this event has been displayed to the user.
    pub displayed: bool,
    /// Time-to-live: event expires after this many ms (0 = permanent).
    pub ttl_ms: u64,
}

/// Kinds of events the coprocessor monitors.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TriageEventKind {
    /// A tool call is taking longer than expected.
    ToolPending {
        tool_name: String,
        elapsed_ms: u64,
        expected_ms: u64,
    },
    /// Risk level has increased (e.g., approaching destructive operation).
    RiskEscalation {
        from_level: f64,
        to_level: f64,
        trigger: String,
    },
    /// The agent's plan has diverged from the original objective.
    PlanDivergence {
        original_strategy: String,
        current_strategy: String,
        confidence_delta: f64,
    },
    /// Token/cost budget is running low.
    BudgetWarning {
        resource: String,
        used_pct: f64,
        threshold_pct: f64,
    },
    /// A background or delegated task has completed.
    TaskCompleted {
        task_id: String,
        success: bool,
        summary: String,
    },
    /// Verification failed — agent may be stuck.
    VerificationFailure {
        attempt: u32,
        reason: String,
    },
    /// Loop detected — agent is repeating actions.
    LoopWarning {
        tool_name: String,
        count: u32,
    },
    /// Permission prompt pending — user action needed.
    ApprovalPending {
        tool_name: String,
        reason: String,
    },
    /// Context pressure — approaching token limit.
    ContextPressure {
        used_pct: f64,
        recommendation: String,
    },
}

// ─── Priority Queue ─────────────────────────────────────────────────────

/// Wrapper for BinaryHeap ordering (highest priority first).
#[derive(Debug, Clone)]
struct PrioritizedEvent(TriageEvent);

impl PartialEq for PrioritizedEvent {
    fn eq(&self, other: &Self) -> bool {
        self.0.priority == other.0.priority
    }
}

impl Eq for PrioritizedEvent {}

impl PartialOrd for PrioritizedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.0.priority.partial_cmp(&other.0.priority)
    }
}

impl Ord for PrioritizedEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partial_cmp(other).unwrap_or(Ordering::Equal)
    }
}

// ─── Coprocessor ────────────────────────────────────────────────────────

/// The ambient triage coprocessor. Maintains a priority queue of
/// events and decides what to surface to the operator.
pub struct TriageCoprocessor {
    /// Bounded priority queue of pending events.
    queue: BinaryHeap<PrioritizedEvent>,
    /// Maximum events to keep in the queue.
    max_events: usize,
    /// Next event ID counter.
    next_id: u64,
    /// Weight configuration for priority scoring.
    weights: TriageWeights,
    /// Events that have been displayed (for dedup).
    displayed_kinds: Vec<String>,
    /// Maximum displayed history to track.
    max_displayed: usize,
}

/// Weights for the priority scoring function.
#[derive(Debug, Clone)]
pub struct TriageWeights {
    pub risk: f64,
    pub latency: f64,
    pub relevance: f64,
    pub noise_penalty: f64,
}

impl Default for TriageWeights {
    fn default() -> Self {
        Self {
            risk: 0.35,
            latency: 0.25,
            relevance: 0.30,
            noise_penalty: 0.10,
        }
    }
}

impl TriageCoprocessor {
    pub fn new(max_events: usize) -> Self {
        Self {
            queue: BinaryHeap::with_capacity(max_events),
            max_events,
            next_id: 1,
            weights: TriageWeights::default(),
            displayed_kinds: Vec::new(),
            max_displayed: 50,
        }
    }

    /// Emit a triage event. Computes priority and inserts into queue.
    /// Cost: O(log n) for heap insertion.
    pub fn emit(&mut self, kind: TriageEventKind, annotation: &str) {
        let priority = self.compute_priority(&kind);
        let ttl_ms = self.default_ttl(&kind);

        let event = TriageEvent {
            id: self.next_id,
            kind,
            priority,
            timestamp_ms: now_ms(),
            annotation: annotation.to_string(),
            displayed: false,
            ttl_ms,
        };
        self.next_id += 1;

        // Evict lowest-priority event if full
        if self.queue.len() >= self.max_events {
            // BinaryHeap is a max-heap, so we need to check if new event
            // is higher priority than the minimum
            let events: Vec<_> = std::mem::take(&mut self.queue).into_vec();
            let mut sorted = events;
            sorted.sort_by(|a, b| a.0.priority.partial_cmp(&b.0.priority).unwrap_or(Ordering::Equal));
            // Remove the lowest-priority event
            if let Some(lowest) = sorted.first() {
                if event.priority > lowest.0.priority {
                    sorted.remove(0);
                }
            }
            sorted.push(PrioritizedEvent(event));
            self.queue = sorted.into_iter().collect();
        } else {
            self.queue.push(PrioritizedEvent(event));
        }
    }

    /// Get the highest-priority event to display.
    /// Cost: O(1) — top of the max-heap.
    pub fn top_event(&self) -> Option<&TriageEvent> {
        self.queue.peek().map(|pe| &pe.0)
    }

    /// Pop and mark the highest-priority event as displayed.
    pub fn pop_display(&mut self) -> Option<TriageEvent> {
        self.queue.pop().map(|pe| {
            let kind_str = format!("{:?}", pe.0.kind).chars().take(30).collect::<String>();
            self.displayed_kinds.push(kind_str);
            if self.displayed_kinds.len() > self.max_displayed {
                self.displayed_kinds.drain(..10);
            }
            let mut event = pe.0;
            event.displayed = true;
            event
        })
    }

    /// Drain expired events. Call periodically.
    pub fn gc_expired(&mut self) {
        let now = now_ms();
        let events: Vec<_> = std::mem::take(&mut self.queue).into_vec();
        self.queue = events
            .into_iter()
            .filter(|pe| pe.0.ttl_ms == 0 || pe.0.timestamp_ms + pe.0.ttl_ms > now)
            .collect();
    }

    /// Number of pending events.
    pub fn pending_count(&self) -> usize {
        self.queue.len()
    }

    /// Compute priority score for an event kind.
    ///
    /// p(e) = w_risk·r_risk + w_latency·r_latency + w_relevance·r_relevance - w_noise·r_noise
    fn compute_priority(&self, kind: &TriageEventKind) -> f64 {
        let (risk, latency, relevance, noise) = match kind {
            TriageEventKind::RiskEscalation { to_level, .. } => {
                (*to_level, 0.3, 0.9, 0.05)
            }
            TriageEventKind::ApprovalPending { .. } => {
                (0.5, 0.8, 1.0, 0.0)
            }
            TriageEventKind::VerificationFailure { attempt, .. } => {
                (0.6, 0.4, 0.8, if *attempt > 2 { 0.2 } else { 0.0 })
            }
            TriageEventKind::BudgetWarning { used_pct, .. } => {
                (0.3, 0.2, *used_pct, 0.1)
            }
            TriageEventKind::LoopWarning { count, .. } => {
                (0.4, 0.3, 0.7, if *count > 5 { 0.3 } else { 0.0 })
            }
            TriageEventKind::PlanDivergence { confidence_delta, .. } => {
                (0.3, 0.2, confidence_delta.abs(), 0.1)
            }
            TriageEventKind::ToolPending { elapsed_ms, expected_ms, .. } => {
                let ratio = *elapsed_ms as f64 / (*expected_ms as f64).max(1.0);
                (0.1, ratio.min(1.0), 0.5, 0.2)
            }
            TriageEventKind::TaskCompleted { success, .. } => {
                (0.0, 0.1, 0.6, if *success { 0.3 } else { 0.0 })
            }
            TriageEventKind::ContextPressure { used_pct, .. } => {
                (0.2, 0.1, *used_pct, 0.15)
            }
        };

        let w = &self.weights;
        let score = w.risk * risk + w.latency * latency + w.relevance * relevance
            - w.noise_penalty * noise;
        score.clamp(0.0, 1.0)
    }

    /// Default TTL for each event kind.
    fn default_ttl(&self, kind: &TriageEventKind) -> u64 {
        match kind {
            TriageEventKind::ToolPending { .. } => 30_000,       // 30s
            TriageEventKind::ApprovalPending { .. } => 0,        // permanent until handled
            TriageEventKind::BudgetWarning { .. } => 60_000,     // 1 min
            TriageEventKind::TaskCompleted { .. } => 15_000,     // 15s
            TriageEventKind::LoopWarning { .. } => 20_000,       // 20s
            TriageEventKind::ContextPressure { .. } => 45_000,   // 45s
            _ => 30_000,
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_risk_event_has_higher_priority() {
        let mut cp = TriageCoprocessor::new(10);
        cp.emit(
            TriageEventKind::TaskCompleted {
                task_id: "t1".into(),
                success: true,
                summary: "done".into(),
            },
            "task completed",
        );
        cp.emit(
            TriageEventKind::RiskEscalation {
                from_level: 0.2,
                to_level: 0.9,
                trigger: "rm -rf".into(),
            },
            "risk escalation detected",
        );

        let top = cp.top_event().unwrap();
        assert!(matches!(top.kind, TriageEventKind::RiskEscalation { .. }));
    }

    #[test]
    fn queue_evicts_when_full() {
        let mut cp = TriageCoprocessor::new(2);
        for i in 0..5 {
            cp.emit(
                TriageEventKind::ToolPending {
                    tool_name: format!("tool_{}", i),
                    elapsed_ms: i * 1000,
                    expected_ms: 5000,
                },
                &format!("pending {}", i),
            );
        }
        assert!(cp.pending_count() <= 3);
    }
}
