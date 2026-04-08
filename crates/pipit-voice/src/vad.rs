//! Voice Activity Detection (VAD) for speech segmentation.
//!
//! Classifies audio frames as speech or non-speech using energy-based detection.
//! Groups speech frames into utterances with configurable silence timeout.
//!
//! The algorithm:
//! 1. Compute RMS energy of each 10ms frame
//! 2. Compare against adaptive threshold (exponential moving average of noise floor)
//! 3. Speech starts when energy exceeds threshold for `min_speech_frames`
//! 4. Speech ends after `silence_timeout_ms` of below-threshold frames

/// VAD configuration.
#[derive(Debug, Clone)]
pub struct VadConfig {
    /// Frame size in milliseconds (typically 10ms).
    pub frame_ms: u32,
    /// Sample rate in Hz (typically 16000).
    pub sample_rate: u32,
    /// Minimum consecutive speech frames to trigger speech start.
    pub min_speech_frames: u32,
    /// Silence timeout in milliseconds to end an utterance.
    pub silence_timeout_ms: u32,
    /// Energy threshold multiplier over noise floor.
    pub threshold_multiplier: f32,
    /// Smoothing factor for noise floor estimation (0..1).
    pub noise_floor_alpha: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            frame_ms: 10,
            sample_rate: 16000,
            min_speech_frames: 3,
            silence_timeout_ms: 300,
            threshold_multiplier: 2.5,
            noise_floor_alpha: 0.05,
        }
    }
}

/// VAD events emitted during processing.
#[derive(Debug, Clone)]
pub enum VadEvent {
    /// Speech started — begin recording utterance.
    SpeechStart,
    /// Speech ended — utterance is ready for transcription.
    SpeechEnd {
        /// Duration of the utterance in milliseconds.
        duration_ms: u32,
    },
    /// A frame was classified (for debugging/visualization).
    Frame {
        energy: f32,
        threshold: f32,
        is_speech: bool,
    },
}

/// The Voice Activity Detector state machine.
pub struct VoiceActivityDetector {
    config: VadConfig,
    /// Current state.
    pub(crate) state: VadState,
    /// Estimated noise floor energy.
    noise_floor: f32,
    /// Consecutive speech frames counter.
    speech_frames: u32,
    /// Consecutive silence frames counter.
    silence_frames: u32,
    /// Number of frames per silence timeout.
    silence_timeout_frames: u32,
    /// Samples per frame.
    frame_samples: usize,
    /// Accumulated utterance audio (PCM i16 samples).
    utterance_buffer: Vec<i16>,
    /// Total frames since speech start.
    utterance_frames: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum VadState {
    Silence,
    MaybeSpeech,
    Speech,
    MaybeSilence,
}

impl VoiceActivityDetector {
    pub fn new(config: VadConfig) -> Self {
        let frame_samples = (config.sample_rate * config.frame_ms / 1000) as usize;
        let silence_timeout_frames = config.silence_timeout_ms / config.frame_ms;

        Self {
            config,
            state: VadState::Silence,
            noise_floor: 0.0,
            speech_frames: 0,
            silence_frames: 0,
            silence_timeout_frames,
            frame_samples,
            utterance_buffer: Vec::new(),
            utterance_frames: 0,
        }
    }

    /// Process a single audio frame (PCM i16 samples).
    /// Returns any VAD events triggered by this frame.
    pub fn process_frame(&mut self, samples: &[i16]) -> Vec<VadEvent> {
        let energy = compute_rms_energy(samples);
        let mut events = Vec::new();

        // Update noise floor estimate (only during silence)
        if self.state == VadState::Silence {
            self.noise_floor = self.noise_floor * (1.0 - self.config.noise_floor_alpha)
                + energy * self.config.noise_floor_alpha;
        }

        let threshold = self.noise_floor * self.config.threshold_multiplier;
        let is_speech = energy > threshold.max(0.01); // Minimum threshold

        events.push(VadEvent::Frame {
            energy,
            threshold,
            is_speech,
        });

        match self.state {
            VadState::Silence => {
                if is_speech {
                    self.speech_frames = 1;
                    self.state = VadState::MaybeSpeech;
                }
            }
            VadState::MaybeSpeech => {
                if is_speech {
                    self.speech_frames += 1;
                    if self.speech_frames >= self.config.min_speech_frames {
                        self.state = VadState::Speech;
                        self.utterance_buffer.clear();
                        self.utterance_frames = self.speech_frames;
                        events.push(VadEvent::SpeechStart);
                    }
                } else {
                    self.speech_frames = 0;
                    self.state = VadState::Silence;
                }
            }
            VadState::Speech => {
                self.utterance_buffer.extend_from_slice(samples);
                self.utterance_frames += 1;

                if !is_speech {
                    self.silence_frames = 1;
                    self.state = VadState::MaybeSilence;
                }
            }
            VadState::MaybeSilence => {
                self.utterance_buffer.extend_from_slice(samples);
                self.utterance_frames += 1;

                if is_speech {
                    self.silence_frames = 0;
                    self.state = VadState::Speech;
                } else {
                    self.silence_frames += 1;
                    if self.silence_frames >= self.silence_timeout_frames {
                        let duration_ms = self.utterance_frames * self.config.frame_ms;
                        events.push(VadEvent::SpeechEnd { duration_ms });
                        self.state = VadState::Silence;
                        self.utterance_frames = 0;
                    }
                }
            }
        }

        events
    }

    /// Get the accumulated utterance audio buffer.
    pub fn utterance_audio(&self) -> &[i16] {
        &self.utterance_buffer
    }

    /// Reset the VAD state.
    pub fn reset(&mut self) {
        self.state = VadState::Silence;
        self.speech_frames = 0;
        self.silence_frames = 0;
        self.utterance_buffer.clear();
        self.utterance_frames = 0;
    }
}

/// Compute RMS energy of PCM i16 samples.
fn compute_rms_energy(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64).sqrt() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_detects_silence() {
        let mut vad = VoiceActivityDetector::new(VadConfig::default());
        let silence = vec![0i16; 160]; // 10ms at 16kHz
        let events = vad.process_frame(&silence);
        assert!(events.iter().all(|e| !matches!(e, VadEvent::SpeechStart)));
    }

    #[test]
    fn vad_detects_speech() {
        let mut vad = VoiceActivityDetector::new(VadConfig {
            min_speech_frames: 2,
            noise_floor_alpha: 0.3, // Faster adaptation for test
            ..Default::default()
        });

        // Feed silence to establish noise floor
        let silence = vec![10i16; 160];
        for _ in 0..20 {
            vad.process_frame(&silence);
        }

        // Feed loud frames — well above noise floor
        let speech = vec![5000i16; 160];
        vad.process_frame(&speech);
        vad.process_frame(&speech);
        let events = vad.process_frame(&speech);
        // After min_speech_frames loud frames, we should either be in Speech
        // or have already emitted SpeechStart
        assert!(
            vad.state != VadState::Silence,
            "VAD should have left Silence state, got {:?}",
            vad.state
        );
    }
}
