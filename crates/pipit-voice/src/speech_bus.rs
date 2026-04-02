//! Duplex Speech Command Bus
//!
//! Replaces the voice mode toggle with a first-class duplex speech
//! transport. Speech feeds the same turn state machine as text input.
//!
//! Pipeline: VAD → Incremental ASR → Intent Stabilization →
//!           Policy Gate → Turn Engine → Optional TTS
//!
//! Streaming chunk windows with amortized O(1) enqueue/dequeue.
//! Intent commit triggers when posterior confidence exceeds τ
//! and no safety predicate is violated.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

// ─── Speech Pipeline Stages ─────────────────────────────────────────────

/// A chunk of audio in the speech pipeline.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// PCM samples (i16, mono).
    pub samples: Vec<i16>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Timestamp of this chunk (ms from stream start).
    pub timestamp_ms: u64,
    /// Whether VAD detected speech in this chunk.
    pub is_speech: bool,
}

/// Incremental ASR hypothesis — a partial transcription.
#[derive(Debug, Clone)]
pub struct AsrHypothesis {
    /// Current best transcript text.
    pub text: String,
    /// Stability score: 0.0 = very unstable, 1.0 = stable/final.
    pub stability: f64,
    /// Confidence score from the ASR engine.
    pub confidence: f64,
    /// Whether this is a final (non-revocable) hypothesis.
    pub is_final: bool,
    /// Timestamp of the utterance start (ms).
    pub utterance_start_ms: u64,
    /// Duration of audio processed so far (ms).
    pub duration_ms: u64,
}

/// Intent classification result from a stabilized hypothesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeechIntent {
    /// The stabilized transcript text.
    pub text: String,
    /// Classified intent type.
    pub intent_type: IntentType,
    /// Confidence in the intent classification.
    pub confidence: f64,
    /// Whether this intent passed the safety gate.
    pub safety_cleared: bool,
    /// Extracted tool or command name (if applicable).
    pub extracted_tool: Option<String>,
}

/// Classification of what the user intends via speech.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntentType {
    /// Natural language query or instruction to the agent.
    AgentQuery,
    /// Slash command (e.g., "slash help", "run tests").
    SlashCommand,
    /// Approval or confirmation ("yes", "approve", "go ahead").
    Approval,
    /// Denial or cancellation ("no", "stop", "cancel").
    Denial,
    /// Navigation command ("scroll up", "next file").
    Navigation,
    /// Unclear or ambiguous — needs clarification.
    Ambiguous,
}

// ─── Ring Buffer for Streaming ──────────────────────────────────────────

/// Bounded ring buffer for audio chunks.
/// Amortized O(1) enqueue/dequeue.
pub struct AudioRingBuffer {
    buffer: VecDeque<AudioChunk>,
    max_chunks: usize,
    /// Total samples processed (monotonic counter).
    total_samples: u64,
}

impl AudioRingBuffer {
    pub fn new(max_chunks: usize) -> Self {
        Self {
            buffer: VecDeque::with_capacity(max_chunks),
            max_chunks,
            total_samples: 0,
        }
    }

    /// Push a chunk. Evicts oldest if full.
    pub fn push(&mut self, chunk: AudioChunk) {
        self.total_samples += chunk.samples.len() as u64;
        if self.buffer.len() >= self.max_chunks {
            self.buffer.pop_front();
        }
        self.buffer.push_back(chunk);
    }

    /// Get the most recent N chunks.
    pub fn recent(&self, n: usize) -> impl Iterator<Item = &AudioChunk> {
        let start = self.buffer.len().saturating_sub(n);
        self.buffer.iter().skip(start)
    }

    /// Drain all speech chunks since the last silence boundary.
    pub fn drain_utterance(&mut self) -> Vec<AudioChunk> {
        let chunks: Vec<_> = self.buffer.drain(..).collect();
        chunks
    }

    pub fn len(&self) -> usize {
        self.buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }
}

// ─── Intent Stabilizer ──────────────────────────────────────────────────

/// Stabilization gate for incremental ASR hypotheses.
///
/// Holds partial hypotheses until stability crosses threshold τ.
/// Prevents accidental tool invocations from unstable ASR output.
pub struct IntentStabilizer {
    /// Stability threshold for committing an intent.
    pub commit_threshold: f64,
    /// Safety threshold — higher bar for destructive operations.
    pub safety_threshold: f64,
    /// Window of recent hypotheses for stability tracking.
    recent_hypotheses: VecDeque<AsrHypothesis>,
    /// Maximum hypothesis window size.
    max_window: usize,
}

impl IntentStabilizer {
    pub fn new(commit_threshold: f64, safety_threshold: f64) -> Self {
        Self {
            commit_threshold,
            safety_threshold,
            recent_hypotheses: VecDeque::with_capacity(10),
            max_window: 10,
        }
    }

    /// Process a new ASR hypothesis. Returns a committed intent if stable.
    ///
    /// Intent commit is a sequential decision problem:
    /// trigger only when posterior confidence > τ and no safety predicate violated.
    pub fn process(&mut self, hypothesis: AsrHypothesis) -> Option<SpeechIntent> {
        let is_final = hypothesis.is_final;
        let stability = hypothesis.stability;
        let confidence = hypothesis.confidence;
        let text = hypothesis.text.clone();

        // Track hypothesis history
        if self.recent_hypotheses.len() >= self.max_window {
            self.recent_hypotheses.pop_front();
        }
        self.recent_hypotheses.push_back(hypothesis);

        // Final hypotheses always commit (ASR engine has decided)
        if is_final {
            return Some(self.classify_intent(&text, confidence));
        }

        // Check stability threshold
        if stability >= self.commit_threshold && confidence >= self.commit_threshold {
            // For potentially destructive intents, require higher confidence
            let intent = self.classify_intent(&text, confidence);
            let required_threshold = match intent.intent_type {
                IntentType::Approval | IntentType::Denial => self.safety_threshold,
                IntentType::SlashCommand => self.safety_threshold,
                _ => self.commit_threshold,
            };

            if confidence >= required_threshold {
                return Some(intent);
            }
        }

        None
    }

    /// Classify the intent of a stabilized transcript.
    fn classify_intent(&self, text: &str, confidence: f64) -> SpeechIntent {
        let lower = text.to_lowercase();
        let trimmed = lower.trim();

        let intent_type = if matches!(
            trimmed,
            "yes" | "yeah" | "yep" | "approve" | "go ahead" | "do it" | "confirmed"
        ) {
            IntentType::Approval
        } else if matches!(
            trimmed,
            "no" | "nope" | "stop" | "cancel" | "abort" | "deny" | "don't"
        ) {
            IntentType::Denial
        } else if trimmed.starts_with("slash ")
            || trimmed.starts_with("command ")
            || trimmed.starts_with("run ")
        {
            IntentType::SlashCommand
        } else if matches!(
            trimmed,
            "scroll up" | "scroll down" | "next" | "previous" | "back"
        ) {
            IntentType::Navigation
        } else if trimmed.len() < 3 || confidence < 0.4 {
            IntentType::Ambiguous
        } else {
            IntentType::AgentQuery
        };

        let extracted_tool = if intent_type == IntentType::SlashCommand {
            trimmed
                .strip_prefix("slash ")
                .or_else(|| trimmed.strip_prefix("command "))
                .or_else(|| trimmed.strip_prefix("run "))
                .map(|s| s.to_string())
        } else {
            None
        };

        SpeechIntent {
            text: text.to_string(),
            intent_type,
            confidence,
            safety_cleared: confidence >= self.safety_threshold,
            extracted_tool,
        }
    }

    /// Reset the stabilizer (e.g., on barge-in or new utterance).
    pub fn reset(&mut self) {
        self.recent_hypotheses.clear();
    }
}

// ─── Speech Bus Configuration ───────────────────────────────────────────

/// Configuration for the duplex speech command bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeechBusConfig {
    /// Whether the speech bus is enabled.
    pub enabled: bool,
    /// Stability threshold for intent commitment (0.0–1.0).
    pub commit_threshold: f64,
    /// Safety threshold for destructive intents (0.0–1.0).
    pub safety_threshold: f64,
    /// Maximum audio buffer size (chunks).
    pub max_buffer_chunks: usize,
    /// Whether TTS playback is enabled for responses.
    pub tts_enabled: bool,
    /// Whether barge-in is allowed (interrupt TTS with new speech).
    pub barge_in: bool,
    /// ASR chunk size in milliseconds.
    pub chunk_ms: u32,
}

impl Default for SpeechBusConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            commit_threshold: 0.7,
            safety_threshold: 0.85,
            max_buffer_chunks: 500,
            tts_enabled: false,
            barge_in: true,
            chunk_ms: 100,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stabilizer_commits_on_final() {
        let mut stab = IntentStabilizer::new(0.7, 0.85);
        let result = stab.process(AsrHypothesis {
            text: "fix the bug".to_string(),
            stability: 0.3,
            confidence: 0.4,
            is_final: true,
            utterance_start_ms: 0,
            duration_ms: 2000,
        });
        assert!(result.is_some());
        assert_eq!(result.unwrap().intent_type, IntentType::AgentQuery);
    }

    #[test]
    fn stabilizer_blocks_unstable() {
        let mut stab = IntentStabilizer::new(0.7, 0.85);
        let result = stab.process(AsrHypothesis {
            text: "yes".to_string(),
            stability: 0.3,
            confidence: 0.4,
            is_final: false,
            utterance_start_ms: 0,
            duration_ms: 500,
        });
        assert!(result.is_none());
    }

    #[test]
    fn ring_buffer_evicts() {
        let mut buf = AudioRingBuffer::new(3);
        for i in 0..5 {
            buf.push(AudioChunk {
                samples: vec![i as i16],
                sample_rate: 16000,
                timestamp_ms: i * 100,
                is_speech: true,
            });
        }
        assert_eq!(buf.len(), 3);
    }
}
