//! Slack channel adapter via Socket Mode WebSocket.
//!
//! Uses Slack's Socket Mode (wss://wss-primary.slack.com) for receiving
//! events and the Web API for sending replies. Thread-per-task support
//! via `thread_ts`.

use crate::config::SlackConfig;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pipit_channel::*;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing;

const SLACK_API_BASE: &str = "https://slack.com/api";

// ---------------------------------------------------------------------------
// Slack channel
// ---------------------------------------------------------------------------

pub struct SlackChannel {
    config: SlackConfig,
    client: Client,
    cancel: CancellationToken,
    /// Bot's own user ID (discovered on auth.test).
    bot_user_id: Mutex<Option<String>>,
    /// Set of configured project names for prefix parsing.
    project_names: HashSet<String>,
    /// Default project name when no prefix is given.
    default_project: String,
}

impl SlackChannel {
    pub fn new(
        config: SlackConfig,
        project_names: HashSet<String>,
        cancel: CancellationToken,
    ) -> Self {
        let default_project = config.default_project.clone().unwrap_or_else(|| {
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
            bot_user_id: Mutex::new(None),
            project_names,
            default_project,
        }
    }

    /// Slack Web API call.
    async fn api_call(&self, method: &str, body: &serde_json::Value) -> Result<serde_json::Value> {
        let url = format!("{}/{}", SLACK_API_BASE, method);
        let resp = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.bot_token),
            )
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|e| anyhow!("slack api error: {e}"))?;

        let status = resp.status();

        if status.as_u16() == 429 {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(5);
            tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
            return Err(anyhow!("slack rate limited, retry after {}s", retry_after));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| anyhow!("slack parse error: {e}"))?;

        if json["ok"].as_bool() != Some(true) {
            let error = json["error"].as_str().unwrap_or("unknown");
            return Err(anyhow!("slack api error: {}", error));
        }

        Ok(json)
    }

    /// Discover bot user ID via auth.test.
    async fn discover_bot_id(&self) -> Result<String> {
        let resp = self
            .api_call("auth.test", &serde_json::json!({}))
            .await?;
        let user_id = resp["user_id"]
            .as_str()
            .ok_or_else(|| anyhow!("auth.test returned no user_id"))?
            .to_string();
        *self.bot_user_id.lock().await = Some(user_id.clone());
        tracing::info!(bot_user_id = %user_id, "slack bot identity discovered");
        Ok(user_id)
    }

    /// Send a message to a channel, optionally in a thread.
    async fn send_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<String> {
        let mut body = serde_json::json!({
            "channel": channel,
            "text": text,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::json!(ts);
        }

        let resp = self.api_call("chat.postMessage", &body).await?;
        let ts = resp["ts"].as_str().unwrap_or("").to_string();
        Ok(ts)
    }

    /// Edit an existing message.
    async fn update_message(
        &self,
        channel: &str,
        ts: &str,
        text: &str,
    ) -> Result<()> {
        let body = serde_json::json!({
            "channel": channel,
            "ts": ts,
            "text": text,
        });
        self.api_call("chat.update", &body).await?;
        Ok(())
    }

    /// Connect via Socket Mode and process events.
    async fn socket_mode_loop(&self, sink: TaskSink) {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;

        let mut backoff_secs = 1u64;

        loop {
            if self.cancel.is_cancelled() {
                break;
            }

            // Get WebSocket URL from apps.connections.open
            let ws_url = match self.get_socket_url().await {
                Ok(url) => {
                    backoff_secs = 1;
                    url
                }
                Err(e) => {
                    tracing::error!(error = %e, "failed to get slack socket mode URL");
                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                    continue;
                }
            };

            tracing::info!("connecting to slack socket mode");

            let ws_stream = match connect_async(&ws_url).await {
                Ok((stream, _)) => stream,
                Err(e) => {
                    tracing::error!(error = %e, "slack socket mode connect failed");
                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                    continue;
                }
            };

            let (mut ws_write, mut ws_read) = ws_stream.split();

            while let Some(msg_result) = ws_read.next().await {
                if self.cancel.is_cancelled() {
                    break;
                }

                let msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!(error = %e, "slack socket mode read error");
                        break;
                    }
                };

                if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(&*text) {
                        // Acknowledge the envelope
                        if let Some(envelope_id) = event["envelope_id"].as_str() {
                            let ack = serde_json::json!({"envelope_id": envelope_id});
                            if let Err(e) = ws_write
                                .send(tokio_tungstenite::tungstenite::Message::Text(
                                    ack.to_string().into(),
                                ))
                                .await
                            {
                                tracing::error!(error = %e, "slack ack send failed");
                                break;
                            }
                        }

                        // Process event payload
                        let event_type = event["type"].as_str().unwrap_or("");
                        if event_type == "events_api" {
                            if let Some(payload) = event.get("payload") {
                                self.handle_event(payload, &sink).await;
                            }
                        }
                    }
                }
            }

            if self.cancel.is_cancelled() {
                break;
            }

            tracing::warn!(backoff = backoff_secs, "slack socket mode disconnected, reconnecting");
            tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(60);
        }
    }

    /// Get a WebSocket URL from Socket Mode.
    async fn get_socket_url(&self) -> Result<String> {
        let app_token = self.config.app_token.as_deref().ok_or_else(|| {
            anyhow!("slack app_token required for Socket Mode (xapp-...)")
        })?;

        let resp = self
            .client
            .post(&format!("{}/apps.connections.open", SLACK_API_BASE))
            .header("Authorization", format!("Bearer {}", app_token))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
            .map_err(|e| anyhow!("slack connections.open error: {e}"))?;

        let json: serde_json::Value = resp.json().await?;

        if json["ok"].as_bool() != Some(true) {
            return Err(anyhow!(
                "slack connections.open failed: {}",
                json["error"].as_str().unwrap_or("unknown")
            ));
        }

        json["url"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow!("no url in connections.open response"))
    }

    /// Handle a Slack Events API event (message, app_mention).
    async fn handle_event(&self, payload: &serde_json::Value, sink: &TaskSink) {
        let event = match payload.get("event") {
            Some(e) => e,
            None => return,
        };

        let event_type = event["type"].as_str().unwrap_or("");

        // Only handle messages and app mentions
        if event_type != "message" && event_type != "app_mention" {
            return;
        }

        // Skip bot messages to avoid loops
        if event.get("bot_id").is_some() || event.get("subtype").is_some() {
            return;
        }

        // Auth check
        let user_id = event["user"].as_str().unwrap_or("");
        if !self.is_allowed_user(user_id) {
            tracing::warn!(user_id, "unauthorized slack user, ignoring");
            return;
        }

        let text = event["text"].as_str().unwrap_or("").to_string();
        let channel_id = event["channel"].as_str().unwrap_or("").to_string();
        let thread_ts = event
            .get("thread_ts")
            .or_else(|| event.get("ts"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let team_id = payload
            .get("team_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Strip bot mention prefix if present (e.g., "<@U12345> fix the bug")
        let bot_id = self.bot_user_id.lock().await;
        let cleaned_text = if let Some(ref bid) = *bot_id {
            let mention = format!("<@{}>", bid);
            text.strip_prefix(&mention)
                .unwrap_or(&text)
                .trim()
                .to_string()
        } else {
            text.clone()
        };
        drop(bot_id);

        if cleaned_text.is_empty() {
            return;
        }

        // Parse project prefix
        let (project, prompt) = self.parse_project_prefix(&cleaned_text);

        let origin = MessageOrigin::Slack {
            team_id,
            channel_id: channel_id.clone(),
            thread_ts,
        };

        let task = NormalizedTask::new(project, prompt, origin);

        match sink.send(task).await {
            Ok(_) => {
                tracing::info!(channel = %channel_id, "task submitted from slack");
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to submit slack task");
                let _ = self
                    .send_message(&channel_id, "⚠ Queue is full. Try again later.", None)
                    .await;
            }
        }
    }

    /// Check if a user is allowed to submit tasks.
    fn is_allowed_user(&self, user_id: &str) -> bool {
        if self.config.allowed_users.is_empty() {
            return true; // Allow-all mode
        }
        self.config.allowed_users.contains(&user_id.to_string())
    }

    /// Parse `@project_name rest of prompt` pattern.
    fn parse_project_prefix(&self, text: &str) -> (String, String) {
        // Handle project prefix patterns like "@myproject fix the bug"
        if text.starts_with('@') {
            let parts: Vec<&str> = text.splitn(2, ' ').collect();
            if parts.len() == 2 {
                let candidate = &parts[0][1..]; // strip @
                if self.project_names.contains(candidate) {
                    return (candidate.to_string(), parts[1].to_string());
                }
            }
        }
        (self.default_project.clone(), text.to_string())
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Slack
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Slack".to_string(),
            supports_streaming: true,
            supports_threads: true,
            supports_reactions: true,
            max_message_length: Some(40000), // Slack's limit
        }
    }

    async fn start(&self, sink: TaskSink) -> Result<(), ChannelError> {
        // Discover bot identity first
        self.discover_bot_id()
            .await
            .map_err(|e| ChannelError::NotConnected(e.to_string()))?;

        // Spawn socket mode loop
        let cancel = self.cancel.clone();
        // We cannot move self into the spawn, so spawn from the caller side.
        // The actual loop is driven by the caller in register_channels.
        tokio::spawn({
            // Build a lightweight handle for the socket loop
            let config = self.config.clone();
            let client = self.client.clone();
            let cancel = self.cancel.clone();
            let project_names = self.project_names.clone();
            let default_project = self.default_project.clone();
            let bot_user_id = self.bot_user_id.lock().await.clone();

            async move {
                let channel = SlackChannel {
                    config,
                    client,
                    cancel,
                    bot_user_id: Mutex::new(bot_user_id),
                    project_names,
                    default_project,
                };
                channel.socket_mode_loop(sink).await;
            }
        });

        Ok(())
    }

    async fn send_update(&self, update: TaskUpdate) -> Result<(), ChannelError> {
        if let MessageOrigin::Slack {
            ref channel_id,
            ref thread_ts,
            ..
        } = update.origin
        {
            let text = match &update.kind {
                TaskUpdateKind::Started { project, model } => {
                    format!("▶ Task started on *{}* (model: {})", project, model)
                }
                TaskUpdateKind::Progress { text, .. } => text.clone(),
                TaskUpdateKind::Completed {
                    summary,
                    turns,
                    cost,
                    files_modified,
                } => {
                    let files = if files_modified.is_empty() {
                        String::new()
                    } else {
                        format!("\nFiles: {}", files_modified.join(", "))
                    };
                    format!(
                        "✅ Completed ({} turns, ${:.4})\n{}{}",
                        turns, cost, summary, files
                    )
                }
                TaskUpdateKind::Error { message } => format!("❌ Error: {}", message),
                TaskUpdateKind::Cancelled => "🚫 Task cancelled".to_string(),
                TaskUpdateKind::ToolStarted { name, .. } => {
                    format!("🔧 Running: {}", name)
                }
                TaskUpdateKind::ToolCompleted {
                    name,
                    success,
                    duration_ms,
                } => {
                    let icon = if *success { "✓" } else { "✗" };
                    format!("{} {} ({}ms)", icon, name, duration_ms)
                }
            };
            self.send_message(channel_id, &text, thread_ts.as_deref())
                .await
                .map_err(|e| ChannelError::Network(e.to_string()))?;
        }
        Ok(())
    }

    async fn stop(&self) -> Result<(), ChannelError> {
        self.cancel.cancel();
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn default_origin(&self) -> Option<MessageOrigin> {
        if let Some(ref channel) = self.config.default_channel {
            Some(MessageOrigin::Slack {
                team_id: String::new(),
                channel_id: channel.clone(),
                thread_ts: None,
            })
        } else {
            None
        }
    }
}
