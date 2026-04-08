//! Voice Mode Integration
//!
//! Provider-agnostic voice input via speech-to-text APIs (Whisper, etc.).
//! Uses Voice Activity Detection (VAD) to segment audio into utterances,
//! which are batched and sent for transcription.
//!
//! Architecture:
//! - VAD front-end: classifies 10ms audio frames as speech/non-speech
//! - Utterance batching: collects speech frames into complete utterances
//! - Transcription backend: sends audio to STT API
//! - Text delivery: surfaces transcribed text as user input

pub mod native_capture;
pub mod speech_bus;
pub mod transcription;
pub mod vad;

pub use transcription::{TranscriptionConfig, TranscriptionProvider, TranscriptionResult};
pub use vad::{VadConfig, VadEvent, VoiceActivityDetector};
