//! # Deterministic Replay Harness (Architecture Task 2)
//!
//! Test harness for verifying that state derivation from event streams is
//! deterministic and reproducible. Supports:
//! - Full replay verification (event stream → state)
//! - Snapshot-accelerated replay verification
//! - Projection regression testing
//! - Cross-surface consistency (CLI/TUI/daemon produce same state from same events)
//!
//! Replay cost: O(N) in event count, O(S + Δ) with snapshot acceleration.
//! Canonical serialization avoids false mismatches from key ordering.

use crate::ledger::{LedgerEvent, SessionEvent, SessionState};
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// A recorded event stream that can be replayed deterministically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayFixture {
    /// Human-readable name for this fixture.
    pub name: String,
    /// The event stream to replay.
    pub events: Vec<LedgerEvent>,
    /// Expected final state after replaying all events.
    pub expected_state: SessionState,
    /// Optional intermediate checkpoints: (after_seq, expected_state_hash).
    pub checkpoints: Vec<(u64, u64)>,
}

/// Result of a single replay verification run.
#[derive(Debug)]
pub struct ReplayResult {
    pub fixture_name: String,
    pub events_replayed: usize,
    pub state_matches: bool,
    pub checkpoints_passed: usize,
    pub checkpoints_failed: usize,
    pub details: Vec<String>,
}

/// The replay harness — runs fixtures and verifies determinism.
pub struct ReplayHarness {
    fixtures: Vec<ReplayFixture>,
}

impl ReplayHarness {
    pub fn new() -> Self {
        Self {
            fixtures: Vec::new(),
        }
    }

    /// Add a fixture to the harness.
    pub fn add_fixture(&mut self, fixture: ReplayFixture) {
        self.fixtures.push(fixture);
    }

    /// Record a fixture from a live event stream and current state.
    pub fn record_fixture(name: impl Into<String>, events: Vec<LedgerEvent>) -> ReplayFixture {
        let state = SessionState::from_events(&events);
        let checkpoints = Self::extract_checkpoints(&events);
        ReplayFixture {
            name: name.into(),
            events,
            expected_state: state,
            checkpoints,
        }
    }

    /// Run all fixtures and return results.
    pub fn run_all(&self) -> Vec<ReplayResult> {
        self.fixtures.iter().map(|f| self.run_fixture(f)).collect()
    }

    /// Run a single fixture.
    pub fn run_fixture(&self, fixture: &ReplayFixture) -> ReplayResult {
        let mut state = SessionState::new();
        let mut details = Vec::new();
        let mut checkpoints_passed = 0;
        let mut checkpoints_failed = 0;

        for event in &fixture.events {
            state.reduce(event);

            // Check intermediate checkpoints
            for (checkpoint_seq, expected_hash) in &fixture.checkpoints {
                if event.seq == *checkpoint_seq {
                    let actual_hash = state_hash(&state);
                    if actual_hash == *expected_hash {
                        checkpoints_passed += 1;
                    } else {
                        checkpoints_failed += 1;
                        details.push(format!(
                            "checkpoint at seq {}: expected hash {}, got {}",
                            checkpoint_seq, expected_hash, actual_hash
                        ));
                    }
                }
            }
        }

        // Verify final state matches
        let state_matches = states_equivalent(&state, &fixture.expected_state);
        if !state_matches {
            details.push(format!(
                "final state mismatch: turn={} (expected {}), tokens={} (expected {})",
                state.current_turn,
                fixture.expected_state.current_turn,
                state.total_tokens,
                fixture.expected_state.total_tokens,
            ));
        }

        ReplayResult {
            fixture_name: fixture.name.clone(),
            events_replayed: fixture.events.len(),
            state_matches,
            checkpoints_passed,
            checkpoints_failed,
            details,
        }
    }

    /// Verify that full replay and snapshot-accelerated replay produce
    /// identical state (cross-path consistency).
    pub fn verify_snapshot_equivalence(events: &[LedgerEvent]) -> bool {
        // Full replay
        let full_state = SessionState::from_events(events);

        // Find the last snapshot event
        let snapshot_idx = events
            .iter()
            .rposition(|e| matches!(e.payload, SessionEvent::Snapshot { .. }));

        if let Some(idx) = snapshot_idx {
            // Replay up to snapshot, then apply suffix
            let prefix_state = SessionState::from_events(&events[..=idx]);
            let suffix_state =
                SessionState::from_snapshot_and_suffix(prefix_state, &events[idx + 1..]);
            states_equivalent(&full_state, &suffix_state)
        } else {
            // No snapshots — full replay is the only path
            true
        }
    }

    /// Extract checkpoint hashes at snapshot boundaries.
    fn extract_checkpoints(events: &[LedgerEvent]) -> Vec<(u64, u64)> {
        let mut state = SessionState::new();
        let mut checkpoints = Vec::new();

        for event in events {
            state.reduce(event);
            if matches!(event.payload, SessionEvent::Snapshot { .. })
                || matches!(event.payload, SessionEvent::TurnCompleted { .. })
            {
                checkpoints.push((event.seq, state_hash(&state)));
            }
        }

        checkpoints
    }
}

/// Compute a deterministic hash of session state for comparison.
pub fn state_hash(state: &SessionState) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Hash deterministic fields
    state.current_turn.hash(&mut hasher);
    state.user_messages.hash(&mut hasher);
    state.assistant_messages.hash(&mut hasher);
    state.tool_calls_completed.hash(&mut hasher);
    state.tool_calls_denied.hash(&mut hasher);
    state.total_tokens.hash(&mut hasher);
    state.compressions.hash(&mut hasher);
    state.plan_pivots.hash(&mut hasher);
    state.ended.hash(&mut hasher);
    for f in &state.modified_files {
        f.hash(&mut hasher);
    }
    for c in &state.checkpoints {
        c.hash(&mut hasher);
    }
    hasher.finish()
}

/// Compare two session states for equivalence (ignoring floating-point cost).
pub fn states_equivalent(a: &SessionState, b: &SessionState) -> bool {
    a.session_id == b.session_id
        && a.model == b.model
        && a.provider == b.provider
        && a.current_turn == b.current_turn
        && a.user_messages == b.user_messages
        && a.assistant_messages == b.assistant_messages
        && a.tool_calls_completed == b.tool_calls_completed
        && a.tool_calls_denied == b.tool_calls_denied
        && a.total_tokens == b.total_tokens
        && a.compressions == b.compressions
        && a.plan_pivots == b.plan_pivots
        && a.modified_files == b.modified_files
        && a.active_subagents == b.active_subagents
        && a.completed_subagents == b.completed_subagents
        && a.checkpoints == b.checkpoints
        && a.ended == b.ended
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::SessionEvent;

    fn make_event(seq: u64, payload: SessionEvent) -> LedgerEvent {
        LedgerEvent {
            seq,
            prev_hash: 0,
            hash: seq, // Simplified for tests
            timestamp_ms: 1000 * seq,
            payload,
        }
    }

    #[test]
    fn replay_produces_deterministic_state() {
        let events = vec![
            make_event(
                1,
                SessionEvent::SessionStarted {
                    session_id: "s1".into(),
                    model: "gpt-4o".into(),
                    provider: "openai".into(),
                },
            ),
            make_event(
                2,
                SessionEvent::UserMessageAccepted {
                    content: "hello".into(),
                },
            ),
            make_event(
                3,
                SessionEvent::AssistantResponseCompleted {
                    text: "hi there".into(),
                    thinking: String::new(),
                    tokens_used: 50,
                },
            ),
            make_event(4, SessionEvent::TurnCompleted { turn: 1 }),
        ];

        // Two independent replays must produce identical state
        let state_a = SessionState::from_events(&events);
        let state_b = SessionState::from_events(&events);
        assert!(states_equivalent(&state_a, &state_b));
    }

    #[test]
    fn fixture_round_trip() {
        let events = vec![
            make_event(
                1,
                SessionEvent::SessionStarted {
                    session_id: "s1".into(),
                    model: "claude".into(),
                    provider: "anthropic".into(),
                },
            ),
            make_event(
                2,
                SessionEvent::UserMessageAccepted {
                    content: "fix bug".into(),
                },
            ),
            make_event(
                3,
                SessionEvent::ToolCallProposed {
                    call_id: "c1".into(),
                    tool_name: "read_file".into(),
                    args: serde_json::json!({"path": "src/main.rs"}),
                },
            ),
            make_event(
                4,
                SessionEvent::ToolCompleted {
                    call_id: "c1".into(),
                    success: true,
                    mutated: false,
                    result_summary: "file contents".into(),
                    result_blob_hash: None,
                },
            ),
            make_event(5, SessionEvent::TurnCompleted { turn: 1 }),
        ];

        let fixture = ReplayHarness::record_fixture("test-fixture", events);
        let harness = ReplayHarness {
            fixtures: vec![fixture],
        };
        let results = harness.run_all();

        assert_eq!(results.len(), 1);
        assert!(results[0].state_matches);
        assert_eq!(results[0].events_replayed, 5);
    }

    #[test]
    fn snapshot_equivalence_holds() {
        let events = vec![
            make_event(
                1,
                SessionEvent::SessionStarted {
                    session_id: "s1".into(),
                    model: "m".into(),
                    provider: "p".into(),
                },
            ),
            make_event(
                2,
                SessionEvent::UserMessageAccepted {
                    content: "a".into(),
                },
            ),
            make_event(3, SessionEvent::TurnCompleted { turn: 1 }),
            make_event(
                4,
                SessionEvent::Snapshot {
                    at_seq: 3,
                    message_count: 2,
                    state_hash: 0,
                },
            ),
            make_event(
                5,
                SessionEvent::UserMessageAccepted {
                    content: "b".into(),
                },
            ),
            make_event(6, SessionEvent::TurnCompleted { turn: 2 }),
        ];

        assert!(ReplayHarness::verify_snapshot_equivalence(&events));
    }
}
