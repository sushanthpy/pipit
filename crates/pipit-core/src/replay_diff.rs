//! # Replay-Differential Testing Harness (B5)
//!
//! Captures and replays LLM conversation traces for regression testing.
//! Records (prompt, response, tool_calls) tuples and replays them with
//! a mock provider to verify deterministic behavior of the planning and
//! execution pipeline.
//!
//! ## Usage
//!
//! 1. **Record**: Set `PIPIT_RECORD_TRACE=1` to capture a session trace
//! 2. **Replay**: Feed the trace to `ReplayRunner` which replays with mock responses
//! 3. **Diff**: Compare tool call sequences, file mutations, and final state

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A single recorded turn in a conversation trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceTurn {
    pub turn_number: u32,
    pub prompt_hash: String,
    pub response_text: String,
    pub tool_calls: Vec<TraceToolCall>,
    pub files_modified: Vec<String>,
    pub timestamp_ms: u64,
}

/// A recorded tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceToolCall {
    pub tool_name: String,
    pub arguments: serde_json::Value,
    pub result_summary: String,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
}

/// A complete session trace for replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTrace {
    pub session_id: String,
    pub model: String,
    pub task_description: String,
    pub turns: Vec<TraceTurn>,
    pub final_state: FinalState,
}

/// The final state of a traced session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalState {
    pub files_modified: Vec<String>,
    pub total_turns: u32,
    pub total_cost: f64,
    pub exit_reason: String,
}

/// Difference detected during replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayDiff {
    pub turn: u32,
    pub diff_type: DiffType,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffType {
    ToolCallMismatch,
    ToolArgumentMismatch,
    FileModificationMismatch,
    TurnCountMismatch,
    ToolSequenceMismatch,
}

/// Recorder that captures session turns into a trace.
pub struct TraceRecorder {
    session_id: String,
    model: String,
    task: String,
    turns: Vec<TraceTurn>,
}

impl TraceRecorder {
    pub fn new(session_id: &str, model: &str, task: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            model: model.to_string(),
            task: task.to_string(),
            turns: Vec::new(),
        }
    }

    /// Record a completed turn.
    pub fn record_turn(&mut self, turn: TraceTurn) {
        self.turns.push(turn);
    }

    /// Finalize and produce the session trace.
    pub fn finalize(self, final_state: FinalState) -> SessionTrace {
        SessionTrace {
            session_id: self.session_id,
            model: self.model,
            task_description: self.task,
            turns: self.turns,
            final_state,
        }
    }

    /// Save the trace to a file.
    pub fn save_to_file(&self, path: &Path) -> Result<(), String> {
        let trace = SessionTrace {
            session_id: self.session_id.clone(),
            model: self.model.clone(),
            task_description: self.task.clone(),
            turns: self.turns.clone(),
            final_state: FinalState {
                files_modified: self
                    .turns
                    .iter()
                    .flat_map(|t| t.files_modified.clone())
                    .collect(),
                total_turns: self.turns.len() as u32,
                total_cost: 0.0,
                exit_reason: "recording".to_string(),
            },
        };
        let json = serde_json::to_string_pretty(&trace).map_err(|e| e.to_string())?;
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn turn_count(&self) -> usize {
        self.turns.len()
    }
}

/// Replay runner that replays a trace and detects differences.
pub struct ReplayRunner {
    trace: SessionTrace,
    diffs: Vec<ReplayDiff>,
    current_turn: usize,
}

impl ReplayRunner {
    pub fn new(trace: SessionTrace) -> Self {
        Self {
            trace,
            diffs: Vec::new(),
            current_turn: 0,
        }
    }

    /// Load a trace from a file.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let trace: SessionTrace = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        Ok(Self::new(trace))
    }

    /// Get the expected response for the current turn (mock provider).
    pub fn next_response(&mut self) -> Option<&TraceTurn> {
        if self.current_turn < self.trace.turns.len() {
            let turn = &self.trace.turns[self.current_turn];
            self.current_turn += 1;
            Some(turn)
        } else {
            None
        }
    }

    /// Compare an actual turn against the expected trace.
    pub fn compare_turn(&mut self, actual: &TraceTurn) {
        let expected_turn = self.trace.turns.get(actual.turn_number as usize);
        let Some(expected) = expected_turn else {
            self.diffs.push(ReplayDiff {
                turn: actual.turn_number,
                diff_type: DiffType::TurnCountMismatch,
                expected: format!("max {} turns", self.trace.turns.len()),
                actual: format!("turn {}", actual.turn_number),
            });
            return;
        };

        // Compare tool call sequences
        if actual.tool_calls.len() != expected.tool_calls.len() {
            self.diffs.push(ReplayDiff {
                turn: actual.turn_number,
                diff_type: DiffType::ToolSequenceMismatch,
                expected: format!("{} tool calls", expected.tool_calls.len()),
                actual: format!("{} tool calls", actual.tool_calls.len()),
            });
        }

        // Compare individual tool calls
        for (i, (exp, act)) in expected.tool_calls.iter().zip(&actual.tool_calls).enumerate() {
            if exp.tool_name != act.tool_name {
                self.diffs.push(ReplayDiff {
                    turn: actual.turn_number,
                    diff_type: DiffType::ToolCallMismatch,
                    expected: exp.tool_name.clone(),
                    actual: act.tool_name.clone(),
                });
            }
        }

        // Compare file modifications
        let mut exp_files: Vec<_> = expected.files_modified.clone();
        let mut act_files: Vec<_> = actual.files_modified.clone();
        exp_files.sort();
        act_files.sort();
        if exp_files != act_files {
            self.diffs.push(ReplayDiff {
                turn: actual.turn_number,
                diff_type: DiffType::FileModificationMismatch,
                expected: format!("{:?}", exp_files),
                actual: format!("{:?}", act_files),
            });
        }
    }

    /// Get all detected diffs.
    pub fn diffs(&self) -> &[ReplayDiff] {
        &self.diffs
    }

    /// Check if replay was identical.
    pub fn is_identical(&self) -> bool {
        self.diffs.is_empty()
    }

    /// Get the trace being replayed.
    pub fn trace(&self) -> &SessionTrace {
        &self.trace
    }

    /// Get a summary of replay differences.
    pub fn diff_summary(&self) -> String {
        if self.diffs.is_empty() {
            return "replay: identical".to_string();
        }
        let mut summary = format!("replay: {} difference(s)\n", self.diffs.len());
        for diff in &self.diffs {
            summary.push_str(&format!(
                "  turn {}: {:?} — expected: {}, actual: {}\n",
                diff.turn, diff.diff_type, diff.expected, diff.actual
            ));
        }
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_trace() -> SessionTrace {
        SessionTrace {
            session_id: "test-session".into(),
            model: "gpt-4".into(),
            task_description: "fix bug".into(),
            turns: vec![
                TraceTurn {
                    turn_number: 0,
                    prompt_hash: "abc".into(),
                    response_text: "I'll read the file".into(),
                    tool_calls: vec![TraceToolCall {
                        tool_name: "read_file".into(),
                        arguments: serde_json::json!({"path": "src/main.rs"}),
                        result_summary: "200 lines".into(),
                        exit_code: None,
                        duration_ms: 50,
                    }],
                    files_modified: vec![],
                    timestamp_ms: 1000,
                },
                TraceTurn {
                    turn_number: 1,
                    prompt_hash: "def".into(),
                    response_text: "I'll fix the bug".into(),
                    tool_calls: vec![TraceToolCall {
                        tool_name: "edit_file".into(),
                        arguments: serde_json::json!({"path": "src/main.rs"}),
                        result_summary: "edited".into(),
                        exit_code: None,
                        duration_ms: 100,
                    }],
                    files_modified: vec!["src/main.rs".into()],
                    timestamp_ms: 2000,
                },
            ],
            final_state: FinalState {
                files_modified: vec!["src/main.rs".into()],
                total_turns: 2,
                total_cost: 0.05,
                exit_reason: "completed".into(),
            },
        }
    }

    #[test]
    fn identical_replay_no_diffs() {
        let trace = make_trace();
        let mut runner = ReplayRunner::new(trace.clone());

        for turn in &trace.turns {
            runner.compare_turn(turn);
        }

        assert!(runner.is_identical());
    }

    #[test]
    fn tool_call_mismatch_detected() {
        let trace = make_trace();
        let mut runner = ReplayRunner::new(trace);

        let mut modified_turn = TraceTurn {
            turn_number: 0,
            prompt_hash: "abc".into(),
            response_text: "I'll grep instead".into(),
            tool_calls: vec![TraceToolCall {
                tool_name: "grep".into(), // Different tool!
                arguments: serde_json::json!({"pattern": "bug"}),
                result_summary: "found".into(),
                exit_code: None,
                duration_ms: 30,
            }],
            files_modified: vec![],
            timestamp_ms: 1000,
        };

        runner.compare_turn(&modified_turn);
        assert!(!runner.is_identical());
        assert_eq!(runner.diffs()[0].diff_type, DiffType::ToolCallMismatch);
    }

    #[test]
    fn file_modification_diff_detected() {
        let trace = make_trace();
        let mut runner = ReplayRunner::new(trace);

        let turn = TraceTurn {
            turn_number: 1,
            prompt_hash: "def".into(),
            response_text: "fix".into(),
            tool_calls: vec![TraceToolCall {
                tool_name: "edit_file".into(),
                arguments: serde_json::json!({"path": "src/main.rs"}),
                result_summary: "edited".into(),
                exit_code: None,
                duration_ms: 100,
            }],
            files_modified: vec!["src/main.rs".into(), "src/extra.rs".into()], // Extra file
            timestamp_ms: 2000,
        };

        runner.compare_turn(&turn);
        assert!(!runner.is_identical());
    }

    #[test]
    fn recorder_captures_turns() {
        let mut rec = TraceRecorder::new("s1", "gpt-4", "fix bug");
        rec.record_turn(TraceTurn {
            turn_number: 0,
            prompt_hash: "h1".into(),
            response_text: "reading".into(),
            tool_calls: vec![],
            files_modified: vec![],
            timestamp_ms: 1000,
        });
        assert_eq!(rec.turn_count(), 1);

        let trace = rec.finalize(FinalState {
            files_modified: vec![],
            total_turns: 1,
            total_cost: 0.01,
            exit_reason: "done".into(),
        });
        assert_eq!(trace.turns.len(), 1);
    }

    #[test]
    fn trace_serialization_roundtrip() {
        let trace = make_trace();
        let json = serde_json::to_string(&trace).unwrap();
        let deserialized: SessionTrace = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.session_id, trace.session_id);
        assert_eq!(deserialized.turns.len(), trace.turns.len());
    }

    #[test]
    fn diff_summary_format() {
        let trace = make_trace();
        let mut runner = ReplayRunner::new(trace);
        assert_eq!(runner.diff_summary(), "replay: identical");

        let bad_turn = TraceTurn {
            turn_number: 0,
            prompt_hash: "x".into(),
            response_text: "y".into(),
            tool_calls: vec![],
            files_modified: vec![],
            timestamp_ms: 0,
        };
        runner.compare_turn(&bad_turn);
        assert!(runner.diff_summary().contains("difference(s)"));
    }
}
