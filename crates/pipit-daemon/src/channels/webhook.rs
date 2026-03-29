//! Webhook channel adapter — outbound POST delivery of task updates.
//!
//! Implements at-least-once delivery with exponential backoff retry.
//! HMAC-SHA256 signature for authenticity verification.

use crate::config::WebhookConfig;

use anyhow::Result;
use async_trait::async_trait;
use pipit_channel::*;
use reqwest::Client;
use sha2::Digest;
use std::any::Any;
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;
use tracing;

const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF_MS: u64 = 1000;

// ---------------------------------------------------------------------------
// Webhook channel
// ---------------------------------------------------------------------------

pub struct WebhookChannel {
    config: WebhookConfig,
    client: Client,
    cancel: CancellationToken,
    project_names: HashSet<String>,
    default_project: String,
}

impl WebhookChannel {
    pub fn new(
        config: WebhookConfig,
        project_names: HashSet<String>,
        cancel: CancellationToken,
    ) -> Self {
        let default_project = config
            .default_project
            .clone()
            .unwrap_or_else(|| {
                project_names
                    .iter()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| "default".to_string())
            });

        Self {
            config,
            client: Client::new(),
            cancel,
            project_names,
            default_project,
        }
    }

    /// Compute HMAC-SHA256 signature for a payload.
    fn compute_signature(&self, payload: &[u8]) -> String {
        let key = self.config.secret.as_bytes();
        let block_size = 64usize;

        // Normalize key — hash if longer than block size
        let mut key_block = vec![0u8; block_size];
        if key.len() > block_size {
            let hash = sha2::Sha256::digest(key);
            key_block[..hash.len()].copy_from_slice(&hash);
        } else {
            key_block[..key.len()].copy_from_slice(key);
        }

        // Inner hash: SHA256(K ^ ipad || message)
        let mut ipad = vec![0x36u8; block_size];
        for (i, b) in key_block.iter().enumerate() {
            ipad[i] ^= b;
        }
        let mut inner = sha2::Sha256::new();
        inner.update(&ipad);
        inner.update(payload);
        let inner_hash = inner.finalize();

        // Outer hash: SHA256(K ^ opad || inner_hash)
        let mut opad = vec![0x5cu8; block_size];
        for (i, b) in key_block.iter().enumerate() {
            opad[i] ^= b;
        }
        let mut outer = sha2::Sha256::new();
        outer.update(&opad);
        outer.update(&inner_hash);
        let result = outer.finalize();

        // Hex encode
        let hex: String = result.iter().map(|b| format!("{:02x}", b)).collect();
        format!("sha256={}", hex)
    }

    /// Deliver a payload to a callback URL with retry.
    async fn deliver(&self, url: &str, payload: &[u8]) -> Result<(), ChannelError> {
        let signature = self.compute_signature(payload);

        let mut attempt = 0u32;
        let mut backoff = INITIAL_BACKOFF_MS;

        loop {
            let resp = self
                .client
                .post(url)
                .header("Content-Type", "application/json")
                .header("X-Pipit-Signature", &signature)
                .header("X-Pipit-Event", "task_update")
                .body(payload.to_vec())
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() => return Ok(()),
                Ok(r) => {
                    attempt += 1;
                    if attempt >= MAX_RETRIES {
                        return Err(ChannelError::Other(format!(
                            "webhook delivery failed after {} retries: HTTP {}",
                            MAX_RETRIES,
                            r.status()
                        )));
                    }
                    tracing::warn!(
                        attempt,
                        status = %r.status(),
                        url,
                        "webhook delivery failed, retrying"
                    );
                }
                Err(e) => {
                    attempt += 1;
                    if attempt >= MAX_RETRIES {
                        return Err(ChannelError::Network(format!(
                            "webhook delivery failed after {} retries: {}",
                            MAX_RETRIES, e
                        )));
                    }
                    tracing::warn!(
                        attempt,
                        error = %e,
                        url,
                        "webhook delivery error, retrying"
                    );
                }
            }

            tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
            backoff *= 2;
        }
    }
}

#[async_trait]
impl Channel for WebhookChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Webhook
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Webhook".to_string(),
            supports_streaming: false,
            supports_threads: false,
            supports_reactions: false,
            max_message_length: None,
        }
    }

    async fn start(&self, _sink: TaskSink) -> Result<(), ChannelError> {
        // Webhook is outbound-only — no inbound message ingestion
        tracing::info!("webhook channel started (outbound delivery only)");
        Ok(())
    }

    async fn send_update(&self, update: TaskUpdate) -> Result<(), ChannelError> {
        if let MessageOrigin::Webhook {
            callback_url: Some(ref url),
            ..
        } = &update.origin
        {
            let payload = serde_json::to_vec(&update)
                .map_err(|e| ChannelError::Other(format!("serialize error: {e}")))?;

            self.deliver(url, &payload).await?;

            tracing::debug!(
                task_id = %update.task_id,
                url,
                "webhook delivered"
            );
        }
        // If no callback_url, silently skip (API tasks without webhook)

        Ok(())
    }

    async fn stop(&self) -> Result<(), ChannelError> {
        tracing::info!("webhook channel stopped");
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
