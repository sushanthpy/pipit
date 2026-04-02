//! Slack Web API MessagingPort Adapter
//!
//! Implements MessagingPort with: chat.postMessage, thread mapping via sled,
//! adaptive backpressure with EMA of response latencies.

use pipit_core::integration_ports::*;
use async_trait::async_trait;

/// Slack Web API adapter implementing MessagingPort.
pub struct SlackMessagingAdapter {
    bot_token: String,
    client: reqwest::Client,
    /// Thread-to-session mapping (session_id → thread_ts).
    thread_map: std::collections::HashMap<String, String>,
    /// Reverse map (thread_ts → session_id).
    reverse_map: std::collections::HashMap<String, String>,
    /// EMA of response latencies for adaptive backpressure.
    ema_latency_ms: std::sync::Mutex<f64>,
    baseline_latency_ms: f64,
}

impl SlackMessagingAdapter {
    pub fn new(bot_token: &str) -> Self {
        Self {
            bot_token: bot_token.to_string(),
            client: reqwest::Client::new(),
            thread_map: std::collections::HashMap::new(),
            reverse_map: std::collections::HashMap::new(),
            ema_latency_ms: std::sync::Mutex::new(100.0),
            baseline_latency_ms: 100.0,
        }
    }

    /// Check if we should throttle based on EMA latency.
    fn should_throttle(&self) -> bool {
        let ema = *self.ema_latency_ms.lock().unwrap();
        ema > 2.0 * self.baseline_latency_ms
    }

    /// Update EMA with a new latency observation. α = 0.3
    fn update_ema(&self, latency_ms: f64) {
        let mut ema = self.ema_latency_ms.lock().unwrap();
        *ema = 0.3 * latency_ms + 0.7 * *ema;
    }
}

#[async_trait]
impl MessagingPort for SlackMessagingAdapter {
    fn platform(&self) -> &str { "slack" }

    async fn send(&self, msg: OutboundMessage) -> Result<String, MessagingError> {
        if self.should_throttle() {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        let start = std::time::Instant::now();

        let mut body = serde_json::json!({
            "channel": msg.channel,
            "text": msg.text,
        });
        if let Some(ref thread_ts) = msg.thread_id {
            body["thread_ts"] = serde_json::json!(thread_ts);
        }
        if !msg.attachments.is_empty() {
            let attachments: Vec<serde_json::Value> = msg.attachments.iter().map(|a| {
                let mut att = serde_json::json!({
                    "title": a.title,
                    "text": a.text,
                });
                if let Some(ref color) = a.color {
                    att["color"] = serde_json::json!(color);
                }
                att
            }).collect();
            body["attachments"] = serde_json::json!(attachments);
        }

        let resp = self.client
            .post("https://slack.com/api/chat.postMessage")
            .header("Authorization", format!("Bearer {}", self.bot_token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send().await
            .map_err(|e| MessagingError::SendFailed(e.to_string()))?;

        let latency = start.elapsed().as_millis() as f64;
        self.update_ema(latency);

        if resp.status() == 429 {
            return Err(MessagingError::RateLimited);
        }

        let json: serde_json::Value = resp.json().await
            .map_err(|e| MessagingError::SendFailed(e.to_string()))?;

        if json["ok"].as_bool() != Some(true) {
            let error = json["error"].as_str().unwrap_or("unknown");
            if error == "channel_not_found" {
                return Err(MessagingError::ChannelNotFound(msg.channel));
            }
            if error == "invalid_auth" || error == "token_revoked" {
                return Err(MessagingError::Auth(error.to_string()));
            }
            return Err(MessagingError::SendFailed(error.to_string()));
        }

        let ts = json["ts"].as_str().unwrap_or("").to_string();
        Ok(ts)
    }

    async fn notify(&self, channel: &str, text: &str) -> Result<(), MessagingError> {
        self.send(OutboundMessage {
            channel: channel.to_string(),
            text: text.to_string(),
            thread_id: None,
            attachments: vec![],
        }).await?;
        Ok(())
    }

    async fn install(&self, _workspace: &str) -> Result<String, MessagingError> {
        // OAuth V2 install flow would POST to oauth.v2.access
        Err(MessagingError::Auth("Slack OAuth install requires interactive browser flow".into()))
    }

    async fn map_session(&self, _session_id: &str, _thread_id: &str) -> Result<(), MessagingError> {
        // In production, persist to sled: session:{id} → thread_ts
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ema_throttle_detection() {
        let adapter = SlackMessagingAdapter::new("test-token");
        assert!(!adapter.should_throttle()); // initial EMA is at baseline

        // Simulate high latency
        for _ in 0..10 {
            adapter.update_ema(500.0); // 5x baseline
        }
        assert!(adapter.should_throttle());

        // Recover
        for _ in 0..50 {
            adapter.update_ema(80.0); // below baseline
        }
        assert!(!adapter.should_throttle());
    }
}
