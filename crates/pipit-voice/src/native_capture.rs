//! Native Audio Capture — cpal backend with SoX/arecord fallback.
//!
//! Pipeline: Mic → PCM frames → Ring buffer → VAD → Utterance → STT
//!
//! Platform backends:
//!   macOS: CoreAudio (via cpal)
//!   Linux: ALSA or PulseAudio (via cpal)
//!   Windows: WASAPI (via cpal)
//!   Fallback: SoX `rec` or ALSA `arecord`
//!
//! Audio format: 16kHz, 16-bit signed PCM, mono.
//! Ring buffer: 30 seconds at 32KB/s = 960KB bounded.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::mpsc;

/// Audio capture configuration.
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub buffer_duration_secs: u32,
    pub push_to_talk: bool,
    pub backend: CaptureBackend,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            channels: 1,
            buffer_duration_secs: 30,
            push_to_talk: true,
            backend: CaptureBackend::Auto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackend {
    Auto,
    Native,
    Sox,
    Arecord,
}

/// Audio capture handle — controls recording lifecycle.
pub struct AudioCapture {
    config: CaptureConfig,
    pub audio_rx: mpsc::Receiver<Vec<i16>>,
    stop_flag: Arc<AtomicBool>,
    pub active_backend: CaptureBackend,
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("No audio device available")]
    NoDevice,
    #[error("Failed to open audio stream: {0}")]
    StreamError(String),
    #[error("Backend not available: {0}")]
    BackendUnavailable(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

impl AudioCapture {
    /// Start audio capture with the configured backend.
    /// Tries: cpal → SoX → arecord.
    pub async fn start(config: CaptureConfig) -> Result<Self, CaptureError> {
        let (tx, rx) = mpsc::channel::<Vec<i16>>(64);
        let stop_flag = Arc::new(AtomicBool::new(false));

        let backend = match config.backend {
            CaptureBackend::Auto => {
                if is_cpal_available() {
                    CaptureBackend::Native
                } else if is_sox_available() {
                    CaptureBackend::Sox
                } else if is_arecord_available() {
                    CaptureBackend::Arecord
                } else {
                    return Err(CaptureError::NoDevice);
                }
            }
            other => other,
        };

        let stop = stop_flag.clone();
        let sample_rate = config.sample_rate;
        let channels = config.channels;

        match backend {
            CaptureBackend::Native => {
                tokio::task::spawn_blocking(move || {
                    start_cpal_capture(sample_rate, channels, tx, stop);
                });
            }
            CaptureBackend::Sox => {
                tokio::spawn(async move {
                    start_sox_capture(sample_rate, channels, tx, stop).await;
                });
            }
            CaptureBackend::Arecord => {
                tokio::spawn(async move {
                    start_arecord_capture(sample_rate, channels, tx, stop).await;
                });
            }
            CaptureBackend::Auto => unreachable!(),
        }

        tracing::info!(backend = ?backend, "Audio capture started");
        Ok(Self {
            config,
            audio_rx: rx,
            stop_flag,
            active_backend: backend,
        })
    }

    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }
    pub fn is_recording(&self) -> bool {
        !self.stop_flag.load(Ordering::Relaxed)
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

// ─── Backend detection ──────────────────────────────────────────────────

fn is_cpal_available() -> bool {
    cfg!(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "windows"
    ))
}

fn is_sox_available() -> bool {
    std::process::Command::new("rec")
        .arg("--version")
        .output()
        .is_ok()
}

fn is_arecord_available() -> bool {
    std::process::Command::new("arecord")
        .arg("--version")
        .output()
        .is_ok()
}

// ─── cpal backend ───────────────────────────────────────────────────────

fn start_cpal_capture(
    sample_rate: u32,
    channels: u16,
    tx: mpsc::Sender<Vec<i16>>,
    stop: Arc<AtomicBool>,
) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = match host.default_input_device() {
        Some(d) => d,
        None => {
            tracing::error!("No audio input device available");
            return;
        }
    };

    let config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    let tx_clone = tx.clone();
    let stop_clone = stop.clone();

    let stream = match device.build_input_stream(
        &config,
        move |data: &[i16], _: &cpal::InputCallbackInfo| {
            if stop_clone.load(Ordering::Relaxed) {
                return;
            }
            let _ = tx_clone.try_send(data.to_vec());
        },
        move |err| {
            tracing::error!("Audio stream error: {}", err);
        },
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to build audio input stream: {}", e);
            return;
        }
    };

    if let Err(e) = stream.play() {
        tracing::error!("Failed to start audio stream: {}", e);
        return;
    }

    tracing::info!(
        "cpal audio capture started ({}Hz, {} ch)",
        sample_rate,
        channels
    );

    // Keep the stream alive until stop flag is set
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    drop(stream);
    tracing::info!("cpal audio capture stopped");
}

// ─── SoX backend ────────────────────────────────────────────────────────

async fn start_sox_capture(
    sample_rate: u32,
    channels: u16,
    tx: mpsc::Sender<Vec<i16>>,
    stop: Arc<AtomicBool>,
) {
    let mut child = match tokio::process::Command::new("rec")
        .args([
            "-q",
            "-r",
            &sample_rate.to_string(),
            "-c",
            &channels.to_string(),
            "-b",
            "16",
            "-e",
            "signed-integer",
            "-t",
            "raw",
            "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to start SoX rec: {e}");
            return;
        }
    };

    let mut stdout = child.stdout.take().unwrap();
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill().await;
            break;
        }
        let mut buf = vec![0u8; 3200]; // 100ms at 16kHz 16-bit
        match tokio::io::AsyncReadExt::read(&mut stdout, &mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let samples: Vec<i16> = buf[..n]
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                let _ = tx.try_send(samples);
            }
            Err(_) => break,
        }
    }
}

// ─── arecord backend ────────────────────────────────────────────────────

async fn start_arecord_capture(
    sample_rate: u32,
    channels: u16,
    tx: mpsc::Sender<Vec<i16>>,
    stop: Arc<AtomicBool>,
) {
    let mut child = match tokio::process::Command::new("arecord")
        .args([
            "-f",
            "S16_LE",
            "-r",
            &sample_rate.to_string(),
            "-c",
            &channels.to_string(),
            "-t",
            "raw",
            "-q",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Failed to start arecord: {e}");
            return;
        }
    };

    let mut stdout = child.stdout.take().unwrap();
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = child.kill().await;
            break;
        }
        let mut buf = vec![0u8; 3200];
        match tokio::io::AsyncReadExt::read(&mut stdout, &mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let samples: Vec<i16> = buf[..n]
                    .chunks_exact(2)
                    .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
                    .collect();
                let _ = tx.try_send(samples);
            }
            Err(_) => break,
        }
    }
}

// ─── Push-to-Talk ───────────────────────────────────────────────────────

/// Push-to-talk state machine.
///
/// TUI layer calls start_recording() on keydown, stop_recording() on keyup.
/// Audio flows: keydown → AudioCapture::start → frames → VAD → STT
pub struct PushToTalk {
    capture: Option<AudioCapture>,
    config: CaptureConfig,
}

impl PushToTalk {
    pub fn new(config: CaptureConfig) -> Self {
        Self {
            capture: None,
            config,
        }
    }

    pub async fn start_recording(&mut self) -> Result<(), CaptureError> {
        if self.capture.is_some() {
            return Ok(());
        }
        self.capture = Some(AudioCapture::start(self.config.clone()).await?);
        Ok(())
    }

    pub fn stop_recording(&mut self) {
        if let Some(ref cap) = self.capture {
            cap.stop();
        }
        self.capture = None;
    }

    pub fn is_recording(&self) -> bool {
        self.capture
            .as_ref()
            .map(|c| c.is_recording())
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_detection_doesnt_crash() {
        let _ = is_cpal_available();
        let _ = is_sox_available();
        let _ = is_arecord_available();
    }

    #[test]
    fn push_to_talk_state_machine() {
        let ptt = PushToTalk::new(CaptureConfig::default());
        assert!(!ptt.is_recording());
    }

    #[test]
    fn default_config() {
        let c = CaptureConfig::default();
        assert_eq!(c.sample_rate, 16000);
        assert_eq!(c.channels, 1);
        assert!(c.push_to_talk);
    }
}
