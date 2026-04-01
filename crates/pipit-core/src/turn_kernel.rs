//! TurnKernel — Pure Turn-State Transition Logic
//!
//! Factors the agent loop into a Mealy machine: (state, input) -> (state', outputs).
//! The transition function is pure and side-effect free, enabling:
//! - Deterministic simulation tests
//! - Model-based replay checking
//! - Clean pause/resume semantics
//! - Better bug isolation

use serde::{Deserialize, Serialize};

/// The phase of a single turn in the agent lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnPhase {
    /// Idle — waiting for user input.
    Idle,
    /// Planning — selecting strategy.
    Planning,
    /// Requesting — building and sending LLM request.
    Requesting,
    /// Streaming — receiving LLM response tokens.
    Streaming,
    /// ToolInput — tool calls received, preparing arguments.
    ToolInput,
    /// ToolRunning — tools are executing.
    ToolRunning,
    /// Verifying — running post-edit verification.
    Verifying,
    /// Done — turn is complete.
    Done,
}

/// Typed turn events — inputs to the state machine.
#[derive(Debug, Clone)]
pub enum TurnInput {
    /// User submitted a message.
    UserMessage(String),
    /// LLM response started streaming.
    StreamStarted,
    /// LLM response chunk received.
    StreamChunk { text: String },
    /// LLM response finished with tool calls.
    ToolCallsReceived { call_count: usize },
    /// LLM response finished without tool calls.
    ResponseComplete,
    /// A tool call completed.
    ToolCompleted {
        call_id: String,
        success: bool,
        mutated: bool,
    },
    /// All tool calls completed.
    AllToolsCompleted { modified_files: Vec<String> },
    /// Verification completed.
    VerificationCompleted { passed: bool },
    /// Context compression triggered.
    CompressionTriggered,
    /// User cancelled.
    Cancelled,
    /// Error occurred.
    Error(String),
}

/// Typed turn outputs — side effects emitted by the state machine.
#[derive(Debug, Clone)]
pub enum TurnOutput {
    /// Transition to a new phase.
    PhaseChange(TurnPhase),
    /// Emit a status label for the UI.
    Status(String),
    /// Emit an event for subscribers.
    Event(String),
    /// Request LLM completion.
    RequestCompletion,
    /// Execute scheduled tool batches.
    ExecuteTools,
    /// Run verification commands.
    RunVerification,
    /// Compress context.
    CompressContext,
    /// Turn is complete — yield control.
    Yield,
}

/// The pure turn kernel state.
#[derive(Debug, Clone)]
pub struct TurnKernel {
    pub phase: TurnPhase,
    pub turn_number: u32,
    pub tool_calls_pending: usize,
    pub tool_calls_completed: usize,
    pub had_mutation: bool,
    pub verification_needed: bool,
    pub consecutive_errors: u32,
    pub max_turns: u32,
}

impl TurnKernel {
    pub fn new(max_turns: u32) -> Self {
        Self {
            phase: TurnPhase::Idle,
            turn_number: 0,
            tool_calls_pending: 0,
            tool_calls_completed: 0,
            had_mutation: false,
            verification_needed: false,
            consecutive_errors: 0,
            max_turns,
        }
    }

    /// Pure transition function: (state, input) -> (state', vec of outputs).
    /// This is the core Mealy machine — no side effects, no I/O.
    pub fn transition(&mut self, input: TurnInput) -> Vec<TurnOutput> {
        let mut outputs = Vec::new();

        match (&self.phase, input) {
            (TurnPhase::Idle, TurnInput::UserMessage(_)) => {
                self.turn_number += 1;
                self.phase = TurnPhase::Planning;
                outputs.push(TurnOutput::PhaseChange(TurnPhase::Planning));
                outputs.push(TurnOutput::Status("Selecting strategy…".into()));
                // Planning is immediate — transition to requesting
                self.phase = TurnPhase::Requesting;
                outputs.push(TurnOutput::PhaseChange(TurnPhase::Requesting));
                outputs.push(TurnOutput::RequestCompletion);
            }

            (TurnPhase::Requesting, TurnInput::StreamStarted) => {
                self.phase = TurnPhase::Streaming;
                outputs.push(TurnOutput::PhaseChange(TurnPhase::Streaming));
            }

            (TurnPhase::Streaming, TurnInput::ToolCallsReceived { call_count }) => {
                self.phase = TurnPhase::ToolInput;
                self.tool_calls_pending = call_count;
                self.tool_calls_completed = 0;
                self.had_mutation = false;
                outputs.push(TurnOutput::PhaseChange(TurnPhase::ToolInput));
                // Immediately begin execution
                self.phase = TurnPhase::ToolRunning;
                outputs.push(TurnOutput::PhaseChange(TurnPhase::ToolRunning));
                outputs.push(TurnOutput::ExecuteTools);
            }

            (TurnPhase::Streaming, TurnInput::ResponseComplete) => {
                self.phase = TurnPhase::Done;
                outputs.push(TurnOutput::PhaseChange(TurnPhase::Done));
                outputs.push(TurnOutput::Yield);
            }

            (TurnPhase::ToolRunning, TurnInput::ToolCompleted { mutated, .. }) => {
                self.tool_calls_completed += 1;
                if mutated {
                    self.had_mutation = true;
                    self.verification_needed = true;
                }
            }

            (TurnPhase::ToolRunning, TurnInput::AllToolsCompleted { modified_files }) => {
                if self.verification_needed && !modified_files.is_empty() {
                    self.phase = TurnPhase::Verifying;
                    outputs.push(TurnOutput::PhaseChange(TurnPhase::Verifying));
                    outputs.push(TurnOutput::RunVerification);
                } else {
                    // No verification needed — back to requesting
                    self.phase = TurnPhase::Requesting;
                    self.verification_needed = false;
                    outputs.push(TurnOutput::PhaseChange(TurnPhase::Requesting));
                    outputs.push(TurnOutput::RequestCompletion);
                }
            }

            (TurnPhase::Verifying, TurnInput::VerificationCompleted { .. }) => {
                self.verification_needed = false;
                // Continue to next turn
                self.phase = TurnPhase::Requesting;
                outputs.push(TurnOutput::PhaseChange(TurnPhase::Requesting));
                outputs.push(TurnOutput::RequestCompletion);
            }

            (_, TurnInput::CompressionTriggered) => {
                outputs.push(TurnOutput::CompressContext);
            }

            (_, TurnInput::Cancelled) => {
                self.phase = TurnPhase::Done;
                outputs.push(TurnOutput::PhaseChange(TurnPhase::Done));
                outputs.push(TurnOutput::Yield);
            }

            (_, TurnInput::Error(msg)) => {
                self.consecutive_errors += 1;
                if self.consecutive_errors >= 3 {
                    self.phase = TurnPhase::Done;
                    outputs.push(TurnOutput::PhaseChange(TurnPhase::Done));
                    outputs.push(TurnOutput::Yield);
                } else {
                    outputs.push(TurnOutput::Status(format!("Error: {}", msg)));
                }
            }

            _ => {
                // Invalid transition — log and ignore
                tracing::debug!(
                    "TurnKernel: invalid transition from {:?}",
                    self.phase
                );
            }
        }

        outputs
    }

    /// Check if the kernel has exceeded the turn limit.
    pub fn exceeds_turn_limit(&self) -> bool {
        self.turn_number > self.max_turns
    }

    /// Reset for a new turn.
    pub fn reset_turn(&mut self) {
        self.tool_calls_pending = 0;
        self.tool_calls_completed = 0;
        self.had_mutation = false;
        self.verification_needed = false;
        self.consecutive_errors = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_turn_lifecycle() {
        let mut kernel = TurnKernel::new(100);
        assert_eq!(kernel.phase, TurnPhase::Idle);

        // User message → planning → requesting
        let outputs = kernel.transition(TurnInput::UserMessage("fix the bug".into()));
        assert_eq!(kernel.turn_number, 1);
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::RequestCompletion)));

        // Stream starts
        let outputs = kernel.transition(TurnInput::StreamStarted);
        assert_eq!(kernel.phase, TurnPhase::Streaming);

        // Response complete (no tools)
        let outputs = kernel.transition(TurnInput::ResponseComplete);
        assert_eq!(kernel.phase, TurnPhase::Done);
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::Yield)));
    }

    #[test]
    fn tool_execution_flow() {
        let mut kernel = TurnKernel::new(100);
        kernel.transition(TurnInput::UserMessage("edit file".into()));
        kernel.transition(TurnInput::StreamStarted);

        // Tool calls received
        let outputs = kernel.transition(TurnInput::ToolCallsReceived { call_count: 2 });
        assert_eq!(kernel.phase, TurnPhase::ToolRunning);
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::ExecuteTools)));

        // Tools complete with mutation
        kernel.transition(TurnInput::ToolCompleted {
            call_id: "1".into(), success: true, mutated: true,
        });
        kernel.transition(TurnInput::AllToolsCompleted {
            modified_files: vec!["foo.rs".into()],
        });
        assert_eq!(kernel.phase, TurnPhase::Verifying);
    }

    #[test]
    fn cancellation_from_any_phase() {
        let mut kernel = TurnKernel::new(100);
        kernel.transition(TurnInput::UserMessage("test".into()));
        kernel.transition(TurnInput::StreamStarted);

        let outputs = kernel.transition(TurnInput::Cancelled);
        assert_eq!(kernel.phase, TurnPhase::Done);
        assert!(outputs.iter().any(|o| matches!(o, TurnOutput::Yield)));
    }
}
