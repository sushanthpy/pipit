//! Transcription backend — speech-to-text API integration.
//!
//! Provider-agnostic: supports OpenAI Whisper API and compatible endpoints.

use serde::{Deserialize, Serialize};

/// Configuration for the transcription backend.
#[derive(Debug, Clone)]
pub struct TranscriptionConfig {
    /// API endpoint URL.
    pub api_url: String,
    /// API key for authentication.
    pub api_key: String,
    /// Model to use (e.g., "whisper-1").
    pub model: String,
    /// Language hint (ISO 639-1, e.g., "en").
    pub language: Option<String>,
    /// Response format ("json", "verbose_json", "text").
    pub response_format: String,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            api_url: "https://api.openai.com/v1/audio/transcriptions".to_string(),
            api_key: String::new(),
            model: "whisper-1".to_string(),
            language: Some("en".to_string()),
            response_format: "json".to_string(),
        }
    }
}

/// Result of a transcription request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    /// Transcribed text.
    pub text: String,
    /// Confidence score (0.0–1.0), if available.
    pub confidence: Option<f64>,
    /// Duration of the audio in seconds.
    pub duration_secs: Option<f64>,
    /// Language detected, if available.
    pub language: Option<String>,
}

/// The transcription provider trait.
#[async_trait::async_trait]
pub trait TranscriptionProvider: Send + Sync {
    /// Transcribe audio samples (PCM i16, 16kHz mono).
    async fn transcribe(
        &self,
        audio: &[i16],
        sample_rate: u32,
    ) -> Result<TranscriptionResult, TranscriptionError>;
}

/// Whisper API transcription provider.
pub struct WhisperProvider {
    config: TranscriptionConfig,
    client: reqwest::Client,
}

impl WhisperProvider {
    pub fn new(config: TranscriptionConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }

    /// Encode PCM i16 samples as WAV bytes for upload.
    fn encode_wav(audio: &[i16], sample_rate: u32) -> Vec<u8> {
        let data_len = audio.len() * 2;
        let file_len = 36 + data_len;
        let mut buf = Vec::with_capacity(44 + data_len);

        // RIFF header
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&(file_len as u32).to_le_bytes());
        buf.extend_from_slice(b"WAVE");

        // fmt chunk
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes()); // chunk size
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM format
        buf.extend_from_slice(&1u16.to_le_bytes()); // mono
        buf.extend_from_slice(&sample_rate.to_le_bytes());
        buf.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
        buf.extend_from_slice(&2u16.to_le_bytes()); // block align
        buf.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

        // data chunk
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&(data_len as u32).to_le_bytes());
        for &sample in audio {
            buf.extend_from_slice(&sample.to_le_bytes());
        }

        buf
    }
}

#[async_trait::async_trait]
impl TranscriptionProvider for WhisperProvider {
    async fn transcribe(
        &self,
        audio: &[i16],
        sample_rate: u32,
    ) -> Result<TranscriptionResult, TranscriptionError> {
        let wav_data = Self::encode_wav(audio, sample_rate);

        let part = reqwest::multipart::Part::bytes(wav_data)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| TranscriptionError::Other(e.to_string()))?;

        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", self.config.model.clone())
            .text("response_format", self.config.response_format.clone());

        if let Some(ref lang) = self.config.language {
            form = form.text("language", lang.clone());
        }

        let response = self
            .client
            .post(&self.config.api_url)
            .header("Authorization", format!("Bearer {}", self.config.api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| TranscriptionError::Network(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            return Err(TranscriptionError::ApiError {
                status: status.as_u16(),
                message: body,
            });
        }

        let result: serde_json::Value = response
            .json()
            .await
            .map_err(|e| TranscriptionError::Other(e.to_string()))?;

        Ok(TranscriptionResult {
            text: result["text"].as_str().unwrap_or("").to_string(),
            confidence: None,
            duration_secs: result["duration"].as_f64(),
            language: result["language"].as_str().map(String::from),
        })
    }
}

/// Transcription error types.
#[derive(Debug, thiserror::Error)]
pub enum TranscriptionError {
    #[error("Network error: {0}")]
    Network(String),
    #[error("API error (HTTP {status}): {message}")]
    ApiError { status: u16, message: String },
    #[error("Transcription error: {0}")]
    Other(String),
}
