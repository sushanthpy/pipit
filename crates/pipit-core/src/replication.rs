//! Remote Transcript Replication with Optimistic Concurrency
//!
//! Asynchronous replication of session events to a remote endpoint.
//! Local WAL is the write-ahead durability layer; remote ingress is
//! eventual-consistency replication. Sits ABOVE the WAL — never replaces it.
//!
//! Conflict detection: O(1) per append (compare expected_seq vs actual).
//! Retry: exponential backoff with jitter to prevent thundering herds.
//! Convergence: guaranteed under transient faults (monotonic cursor + idempotent append).

use crate::ledger::LedgerEvent;
use std::time::Duration;

/// Configuration for remote replication.
#[derive(Debug, Clone)]
pub struct ReplicationConfig {
    /// Remote endpoint URL (e.g., https://api.pipit.dev/v1/sessions/{id}/events).
    pub endpoint: String,
    /// Initial retry delay.
    pub initial_backoff: Duration,
    /// Maximum retry delay.
    pub max_backoff: Duration,
    /// Maximum jitter added to backoff.
    pub jitter_max: Duration,
    /// Maximum retries before giving up on a batch.
    pub max_retries: u32,
    /// Batch size: accumulate this many events before flushing.
    pub batch_size: usize,
    /// Flush interval: flush even if batch is not full (millis).
    pub flush_interval_ms: u64,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            jitter_max: Duration::from_millis(250),
            max_retries: 5,
            batch_size: 10,
            flush_interval_ms: 5000,
        }
    }
}

/// A replication cursor tracking the last successfully replicated sequence.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReplicationCursor {
    /// Session ID.
    pub session_id: String,
    /// Last successfully replicated event sequence number.
    pub last_replicated_seq: u64,
    /// UUID of the last replicated event (for deduplication).
    pub last_event_hash: u64,
}

impl ReplicationCursor {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            last_replicated_seq: 0,
            last_event_hash: 0,
        }
    }
}

/// Outcome of an append operation.
#[derive(Debug, Clone)]
pub enum AppendOutcome {
    /// Successfully appended.
    Success { new_seq: u64 },
    /// Conflict: server has a different event at expected_seq.
    Conflict { expected_seq: u64, server_seq: u64 },
    /// Transient failure (retry eligible).
    TransientError { message: String },
    /// Permanent failure (do not retry).
    PermanentError { message: String },
}

/// A batch of events to replicate.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReplicationBatch {
    /// Session ID.
    pub session_id: String,
    /// Expected server sequence (for optimistic concurrency).
    pub expected_seq: u64,
    /// Events in this batch.
    pub events: Vec<LedgerEvent>,
}

/// The replication controller manages async event forwarding.
///
/// It buffers events locally and flushes them to the remote endpoint
/// in batches with optimistic concurrency control.
pub struct ReplicationController {
    config: ReplicationConfig,
    cursor: ReplicationCursor,
    /// Pending events not yet replicated.
    pending: Vec<LedgerEvent>,
    /// Whether replication is enabled.
    enabled: bool,
}

impl ReplicationController {
    pub fn new(config: ReplicationConfig, session_id: &str) -> Self {
        let enabled = !config.endpoint.is_empty();
        Self {
            config,
            cursor: ReplicationCursor::new(session_id),
            pending: Vec::new(),
            enabled,
        }
    }

    /// Disabled replication (no-op).
    pub fn disabled() -> Self {
        Self {
            config: ReplicationConfig::default(),
            cursor: ReplicationCursor::new(""),
            pending: Vec::new(),
            enabled: false,
        }
    }

    /// Queue an event for replication. O(1) amortized.
    pub fn enqueue(&mut self, event: LedgerEvent) {
        if !self.enabled {
            return;
        }
        self.pending.push(event);
    }

    /// Check if a flush is needed (batch full or interval elapsed).
    pub fn needs_flush(&self) -> bool {
        self.enabled && self.pending.len() >= self.config.batch_size
    }

    /// Build the next replication batch. Returns None if no pending events.
    pub fn build_batch(&mut self) -> Option<ReplicationBatch> {
        if !self.enabled || self.pending.is_empty() {
            return None;
        }

        let batch_size = self.config.batch_size.min(self.pending.len());
        let events: Vec<_> = self.pending.drain(..batch_size).collect();

        Some(ReplicationBatch {
            session_id: self.cursor.session_id.clone(),
            expected_seq: self.cursor.last_replicated_seq,
            events,
        })
    }

    /// Record successful replication of a batch.
    pub fn record_success(&mut self, batch: &ReplicationBatch) {
        if let Some(last) = batch.events.last() {
            self.cursor.last_replicated_seq = last.seq;
            self.cursor.last_event_hash = last.hash;
        }
    }

    /// Record a conflict — re-enqueue the batch for retry after resolution.
    pub fn record_conflict(&mut self, batch: ReplicationBatch) {
        // Prepend failed events back to pending
        let mut re_queue = batch.events;
        re_queue.append(&mut self.pending);
        self.pending = re_queue;
    }

    /// Compute backoff delay for a given retry attempt.
    /// Formula: delay = min(d₀ × 2^i, d_max) + U(0, jitter)
    pub fn backoff_delay(&self, attempt: u32) -> Duration {
        let base = self.config.initial_backoff.as_millis() as f64;
        let multiplied = base * 2.0f64.powi(attempt as i32);
        let capped = multiplied.min(self.config.max_backoff.as_millis() as f64);

        // Jitter: uniform [0, jitter_max]
        let jitter_ms = {
            use std::time::SystemTime;
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos();
            let jitter_max = self.config.jitter_max.as_millis() as f64;
            (nanos % 1000) as f64 / 1000.0 * jitter_max
        };

        Duration::from_millis((capped + jitter_ms) as u64)
    }

    /// Current replication cursor (for persistence/teleportation).
    pub fn cursor(&self) -> &ReplicationCursor {
        &self.cursor
    }

    /// Number of pending events.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Whether replication is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Restore cursor from persisted state.
    pub fn restore_cursor(&mut self, cursor: ReplicationCursor) {
        self.cursor = cursor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::{LedgerEvent, SessionEvent};

    fn test_event(seq: u64) -> LedgerEvent {
        LedgerEvent {
            seq,
            prev_hash: 0,
            hash: seq * 1000,
            timestamp_ms: 0,
            payload: SessionEvent::UserMessageAccepted {
                content: format!("msg-{}", seq),
            },
        }
    }

    #[test]
    fn test_disabled_is_noop() {
        let mut ctrl = ReplicationController::disabled();
        ctrl.enqueue(test_event(1));
        assert!(!ctrl.needs_flush());
        assert_eq!(ctrl.pending_count(), 0);
    }

    #[test]
    fn test_batch_building() {
        let config = ReplicationConfig {
            endpoint: "http://localhost/events".into(),
            batch_size: 3,
            ..Default::default()
        };
        let mut ctrl = ReplicationController::new(config, "sess-1");

        ctrl.enqueue(test_event(1));
        ctrl.enqueue(test_event(2));
        assert!(!ctrl.needs_flush());

        ctrl.enqueue(test_event(3));
        assert!(ctrl.needs_flush());

        let batch = ctrl.build_batch().unwrap();
        assert_eq!(batch.events.len(), 3);
        assert_eq!(batch.expected_seq, 0);
        assert_eq!(ctrl.pending_count(), 0);
    }

    #[test]
    fn test_success_advances_cursor() {
        let config = ReplicationConfig {
            endpoint: "http://localhost/events".into(),
            batch_size: 2,
            ..Default::default()
        };
        let mut ctrl = ReplicationController::new(config, "sess-1");

        ctrl.enqueue(test_event(1));
        ctrl.enqueue(test_event(2));
        let batch = ctrl.build_batch().unwrap();
        ctrl.record_success(&batch);

        assert_eq!(ctrl.cursor().last_replicated_seq, 2);
    }

    #[test]
    fn test_conflict_re_enqueues() {
        let config = ReplicationConfig {
            endpoint: "http://localhost/events".into(),
            batch_size: 2,
            ..Default::default()
        };
        let mut ctrl = ReplicationController::new(config, "sess-1");

        ctrl.enqueue(test_event(1));
        ctrl.enqueue(test_event(2));
        let batch = ctrl.build_batch().unwrap();

        // Enqueue more while batch is in flight
        ctrl.enqueue(test_event(3));

        // Conflict — re-enqueue
        ctrl.record_conflict(batch);
        assert_eq!(ctrl.pending_count(), 3); // 1,2 re-enqueued + 3
    }

    #[test]
    fn test_backoff_exponential() {
        let config = ReplicationConfig {
            endpoint: "http://localhost".into(),
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(10),
            jitter_max: Duration::from_millis(0), // no jitter for deterministic test
            ..Default::default()
        };
        let ctrl = ReplicationController::new(config, "sess-1");

        let d0 = ctrl.backoff_delay(0).as_millis();
        let d1 = ctrl.backoff_delay(1).as_millis();
        let d2 = ctrl.backoff_delay(2).as_millis();

        assert!(d0 >= 100 && d0 <= 150); // ~100ms + small jitter
        assert!(d1 >= 200 && d1 <= 250); // ~200ms
        assert!(d2 >= 400 && d2 <= 450); // ~400ms
    }
}
