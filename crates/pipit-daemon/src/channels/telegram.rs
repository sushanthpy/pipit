//! Telegram Bot API channel adapter.
//!
//! Long-poll via getUpdates. Edit-in-place streaming via editMessageText.
//! User whitelist authentication. Project prefix parsing (`@project prompt`).

use crate::config::TelegramConfig;

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

const TELEGRAM_API_BASE: &str = "https://api.telegram.org/bot";
const POLL_TIMEOUT_SECS: u64 = 30;

// ---------------------------------------------------------------------------
// Telegram channel
// ---------------------------------------------------------------------------

pub struct TelegramChannel {
    config: TelegramConfig,
    client: Client,
    cancel: CancellationToken,
    /// Discovered chat ID for proactive messages (cron results).
    discovered_chat_id: Mutex<Option<i64>>,
    /// Set of configured project names for prefix parsing.
    project_names: HashSet<String>,
    /// Default project name when no prefix is given.
    default_project: String,
}

impl TelegramChannel {
    pub fn new(
        config: TelegramConfig,
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
            discovered_chat_id: Mutex::new(None),
            project_names,
            default_project,
        }
    }

    /// Telegram Bot API URL helper.
    fn api_url(&self, method: &str) -> String {
        format!("{}{}/{}", TELEGRAM_API_BASE, self.config.bot_token, method)
    }

    /// Call a Telegram API method with rate-limit retry.
    async fn api_call<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: &T,
    ) -> Result<R> {
        let url = self.api_url(method);
        let mut attempt = 0u32;
        let max_retries = 3u32;

        loop {
            let resp = self
                .client
                .post(&url)
                .json(params)
                .send()
                .await
                .map_err(|e| anyhow!("telegram api error: {e}"))?;

            let status = resp.status();

            // Handle 429 Too Many Requests (flood control)
            if status.as_u16() == 429 {
                let body = resp.text().await?;
                let retry_after = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("parameters")?.get("retry_after")?.as_u64())
                    .unwrap_or(5);

                attempt += 1;
                if attempt > max_retries {
                    return Err(anyhow!(
                        "telegram rate limited on {} after {} retries (retry_after={}s)",
                        method,
                        max_retries,
                        retry_after
                    ));
                }

                tracing::warn!(
                    method,
                    attempt,
                    retry_after,
                    "telegram rate limited, backing off"
                );
                tokio::time::sleep(std::time::Duration::from_secs(retry_after)).await;
                continue;
            }

            let body = resp.text().await?;

            if !status.is_success() {
                // Non-429 error: exponential backoff with jitter for transient errors
                if status.is_server_error() && attempt < max_retries {
                    attempt += 1;
                    let backoff = (1u64 << attempt).min(30);
                    tracing::warn!(
                        method,
                        status = %status,
                        attempt,
                        "telegram server error, retrying in {}s", backoff
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                    continue;
                }
                return Err(anyhow!(
                    "telegram api {} returned {}: {}",
                    method,
                    status,
                    body
                ));
            }

            let api_resp: TelegramApiResponse<R> =
                serde_json::from_str(&body).map_err(|e| anyhow!("telegram parse error: {e}"))?;

            if !api_resp.ok {
                return Err(anyhow!(
                    "telegram api error: {}",
                    api_resp.description.unwrap_or_default()
                ));
            }

            return api_resp
                .result
                .ok_or_else(|| anyhow!("telegram api returned no result"));
        }
    }

    /// getUpdates long-poll loop.
    async fn poll_loop(&self, sink: TaskSink) {
        let mut offset: i64 = 0;

        loop {
            if self.cancel.is_cancelled() {
                break;
            }

            let params = serde_json::json!({
                "offset": offset,
                "timeout": POLL_TIMEOUT_SECS,
                "allowed_updates": ["message"]
            });

            match self
                .api_call::<_, Vec<TelegramUpdate>>("getUpdates", &params)
                .await
            {
                Ok(updates) => {
                    for update in updates {
                        offset = update.update_id + 1;

                        if let Some(message) = update.message {
                            // Auto-discover chat_id
                            {
                                let mut discovered = self.discovered_chat_id.lock().await;
                                if discovered.is_none() {
                                    *discovered = Some(message.chat.id);
                                    tracing::info!(
                                        chat_id = message.chat.id,
                                        "discovered default chat"
                                    );
                                }
                            }

                            // Auth check
                            if !self.is_allowed_user(message.from.as_ref().map(|u| u.id)) {
                                tracing::warn!(
                                    user_id = ?message.from.as_ref().map(|u| u.id),
                                    "unauthorized telegram user, ignoring"
                                );
                                continue;
                            }

                            if let Some(text) = message.text {
                                self.handle_message(
                                    text,
                                    message.chat.id,
                                    message.message_id,
                                    message.from.as_ref(),
                                    &sink,
                                )
                                .await;
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "telegram poll error");
                    // Back off on error
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    /// Check if a user is allowed to submit tasks.
    fn is_allowed_user(&self, user_id: Option<i64>) -> bool {
        if self.config.allowed_users.is_empty() {
            return true; // Allow-all mode for personal bots
        }
        user_id
            .map(|id| self.config.allowed_users.contains(&id))
            .unwrap_or(false)
    }

    /// Parse and handle an incoming text message.
    async fn handle_message(
        &self,
        text: String,
        chat_id: i64,
        message_id: i64,
        from: Option<&TelegramUser>,
        sink: &TaskSink,
    ) {
        let sender_name = from.map(|u| {
            if let Some(ref username) = u.username {
                format!("@{}", username)
            } else {
                u.first_name.clone()
            }
        });

        // Bot commands
        if text.starts_with('/') {
            self.handle_command(&text, chat_id).await;
            return;
        }

        // Parse project prefix: "@myproject fix the bug" → project=myproject, prompt="fix the bug"
        let (project, prompt) = self.parse_project_prefix(&text);

        let origin = MessageOrigin::Telegram {
            chat_id,
            message_id: Some(message_id),
            progress_message_id: None,
            user_id: from.map(|u| u.id),
        };

        let mut task = NormalizedTask::new(project, prompt, origin);
        if let Some(name) = sender_name {
            task = task.with_sender(name);
        }

        match sink.send(task).await {
            Ok(_) => {
                tracing::info!(chat_id, "task submitted from telegram");
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to submit telegram task");
                let _ = self
                    .send_text(chat_id, "⚠ Queue is full. Try again later.")
                    .await;
            }
        }
    }

    /// Parse `@project_name rest of prompt` pattern.
    fn parse_project_prefix<'a>(&self, text: &'a str) -> (String, String) {
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

    /// Handle bot commands.
    async fn handle_command(&self, text: &str, chat_id: i64) {
        let cmd = text.split_whitespace().next().unwrap_or("");
        match cmd {
            "/start" | "/help" => {
                let help = "🤖 *Pipit Daemon*\n\n\
                    Send a message to run a coding task\\.\n\n\
                    Use `@project prompt` to target a specific project\\.\n\n\
                    *Commands:*\n\
                    /status \\- Queue and project status\n\
                    /projects \\- List configured projects\n\
                    /cancel \\- Cancel running task\n\
                    /last \\- Show last task result";
                let _ = self.send_markdown(chat_id, help).await;
            }
            "/status" => {
                let _ = self
                    .send_text(
                        chat_id,
                        "Status endpoint: use the HTTP API for detailed status.",
                    )
                    .await;
            }
            "/projects" => {
                let list: Vec<String> = self.project_names.iter().cloned().collect();
                let msg = format!("Configured projects: {}", list.join(", "));
                let _ = self.send_text(chat_id, &msg).await;
            }
            _ => {
                let _ = self.send_text(chat_id, "Unknown command. Try /help").await;
            }
        }
    }

    /// Send a plain text message.
    async fn send_text(&self, chat_id: i64, text: &str) -> Result<TelegramMessage> {
        let params = serde_json::json!({
            "chat_id": chat_id,
            "text": text
        });
        self.api_call("sendMessage", &params).await
    }

    /// Send a Markdown-formatted message.
    async fn send_markdown(&self, chat_id: i64, text: &str) -> Result<TelegramMessage> {
        let params = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "MarkdownV2"
        });
        self.api_call("sendMessage", &params).await
    }

    /// Edit an existing message.
    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str) -> Result<()> {
        let params = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
            "parse_mode": "MarkdownV2"
        });
        let _: serde_json::Value = self.api_call("editMessageText", &params).await?;
        Ok(())
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Telegram
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Telegram".to_string(),
            supports_streaming: true,
            supports_threads: false,
            supports_reactions: true,
            max_message_length: Some(4096),
        }
    }

    async fn start(&self, sink: TaskSink) -> Result<(), ChannelError> {
        tracing::info!("starting telegram channel");

        // Probe with getMe to verify token
        let params = serde_json::json!({});
        let me: serde_json::Value = self
            .api_call("getMe", &params)
            .await
            .map_err(|e| ChannelError::AuthFailed(e.to_string()))?;

        tracing::info!(
            bot = %me.get("username").and_then(|v| v.as_str()).unwrap_or("unknown"),
            "telegram bot authenticated"
        );

        // Start poll loop in background
        // NOTE: In production, TelegramChannel should be wrapped in Arc
        // and the poll_loop spawned with a clone. For the scaffold,
        // we just log that the channel started.
        tracing::info!("telegram channel started (poll loop placeholder)");

        Ok(())
    }

    async fn send_update(&self, update: TaskUpdate) -> Result<(), ChannelError> {
        if let MessageOrigin::Telegram {
            chat_id,
            progress_message_id,
            ..
        } = &update.origin
        {
            let text = match &update.kind {
                TaskUpdateKind::Started { project, model } => {
                    format!("▸ Working on {} ({})", project, model)
                }
                TaskUpdateKind::Progress { text, .. } => text.clone(),
                TaskUpdateKind::Completed {
                    summary,
                    turns,
                    cost,
                    ..
                } => {
                    format!("✓ Done ({} turns, ${:.4})\n{}", turns, cost, summary)
                }
                TaskUpdateKind::Error { message } => format!("✗ Error: {}", message),
                TaskUpdateKind::Cancelled => "Task cancelled".to_string(),
                TaskUpdateKind::ToolStarted { name, .. } => format!("○ {}", name),
                TaskUpdateKind::ToolCompleted { name, success, .. } => {
                    let icon = if *success { "●" } else { "✗" };
                    format!("{} {}", icon, name)
                }
            };

            // Edit existing message or send new one
            if let Some(msg_id) = progress_message_id {
                let _ = self.edit_message(*chat_id, *msg_id, &text).await;
            } else {
                let _ = self.send_text(*chat_id, &text).await;
            }
        }

        Ok(())
    }

    async fn stop(&self) -> Result<(), ChannelError> {
        tracing::info!("stopping telegram channel");
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn default_origin(&self) -> Option<MessageOrigin> {
        // Use discovered chat_id for proactive messages
        let chat_id = self.discovered_chat_id.try_lock().ok()?.as_ref().copied()?;
        Some(MessageOrigin::Telegram {
            chat_id,
            message_id: None,
            progress_message_id: None,
            user_id: None,
        })
    }
}

#[async_trait]
impl StreamingChannel for TelegramChannel {
    async fn send_streaming(
        &self,
        origin: &MessageOrigin,
        initial: &str,
    ) -> Result<StreamHandle, ChannelError> {
        if let MessageOrigin::Telegram { chat_id, .. } = origin {
            let result = self
                .send_text(*chat_id, initial)
                .await
                .map_err(|e| ChannelError::Other(e.to_string()))?;

            let chat_id = *chat_id;
            let message_id = result.message_id;
            let bot_token = self.config.bot_token.clone();

            Ok(StreamHandle::new(move |text| {
                let bot_token = bot_token.clone();
                async move {
                    let client = Client::new();
                    let url = format!("{}{}/editMessageText", TELEGRAM_API_BASE, bot_token);
                    let params = serde_json::json!({
                        "chat_id": chat_id,
                        "message_id": message_id,
                        "text": text
                    });
                    client
                        .post(&url)
                        .json(&params)
                        .send()
                        .await
                        .map_err(|e| ChannelError::Network(e.to_string()))?;
                    Ok(())
                }
            }))
        } else {
            Err(ChannelError::Other("not a telegram origin".to_string()))
        }
    }
}

// ---------------------------------------------------------------------------
// Telegram API types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TelegramApiResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize, Serialize)]
struct TelegramMessage {
    message_id: i64,
    chat: TelegramChat,
    from: Option<TelegramUser>,
    text: Option<String>,
    date: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    chat_type: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct TelegramUser {
    id: i64,
    is_bot: Option<bool>,
    first_name: String,
    username: Option<String>,
}

// ---------------------------------------------------------------------------
// Channel registration
// ---------------------------------------------------------------------------

use crate::config::{ChannelConfig, DaemonConfig};

/// Register all configured channels into the registry.
pub async fn register_channels(
    config: &DaemonConfig,
    registry: &ChannelRegistry,
    sink: TaskSink,
    cancel: CancellationToken,
) -> Result<()> {
    let project_names: HashSet<String> = config.projects.keys().cloned().collect();

    for (name, channel_config) in &config.channels {
        match channel_config {
            ChannelConfig::Telegram(tg_config) => {
                let channel = Arc::new(TelegramChannel::new(
                    tg_config.clone(),
                    project_names.clone(),
                    cancel.clone(),
                ));
                registry.register(channel.clone());
                channel
                    .start(sink.clone())
                    .await
                    .map_err(|e| anyhow!("failed to start telegram channel '{}': {}", name, e))?;
                tracing::info!(channel = %name, "telegram channel registered");
            }
            ChannelConfig::Discord(dc_config) => {
                let channel = Arc::new(super::discord::DiscordChannel::new(
                    dc_config.clone(),
                    project_names.clone(),
                    cancel.clone(),
                ));
                registry.register(channel.clone());
                channel
                    .start(sink.clone())
                    .await
                    .map_err(|e| anyhow!("failed to start discord channel '{}': {}", name, e))?;
                tracing::info!(channel = %name, "discord channel registered");
            }
            ChannelConfig::Webhook(wh_config) => {
                let channel = Arc::new(super::webhook::WebhookChannel::new(
                    wh_config.clone(),
                    project_names.clone(),
                    cancel.clone(),
                ));
                registry.register(channel.clone());
                channel
                    .start(sink.clone())
                    .await
                    .map_err(|e| anyhow!("failed to start webhook channel '{}': {}", name, e))?;
                tracing::info!(channel = %name, "webhook channel registered");
            }
        }
    }

    Ok(())
}
