//! Discord Bot channel adapter via Gateway WebSocket + REST API.
//!
//! Uses Discord's Gateway (wss://gateway.discord.gg) for receiving messages
//! and the REST API for sending replies. Thread-per-task support via
//! `ThreadedChannel`. Reaction-based controls via `ReactiveChannel`.

use crate::config::DiscordConfig;

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use pipit_channel::*;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing;

const DISCORD_API_BASE: &str = "https://discord.com/api/v10";
const DISCORD_GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
const DISCORD_MAX_MESSAGE_LEN: usize = 2000;

// ---------------------------------------------------------------------------
// Discord channel
// ---------------------------------------------------------------------------

pub struct DiscordChannel {
    config: DiscordConfig,
    client: Client,
    cancel: CancellationToken,
    /// Bot's own user ID (discovered on READY).
    bot_user_id: AtomicU64,
    /// Set of configured project names for prefix parsing.
    project_names: HashSet<String>,
    /// Default project name when no prefix is given.
    default_project: String,
    /// Last known gateway sequence number for resume/heartbeat.
    sequence: Arc<Mutex<Option<u64>>>,
    /// Session ID for RESUME (from READY event).
    session_id: Mutex<Option<String>>,
    /// Resume gateway URL (from READY event).
    resume_gateway_url: Mutex<Option<String>>,
    /// Heartbeat ACK tracking — true if we're waiting for an ACK.
    awaiting_ack: Arc<std::sync::atomic::AtomicBool>,
}

impl DiscordChannel {
    pub fn new(
        config: DiscordConfig,
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
            bot_user_id: AtomicU64::new(0),
            project_names,
            default_project,
            sequence: Arc::new(Mutex::new(None)),
            session_id: Mutex::new(None),
            resume_gateway_url: Mutex::new(None),
            awaiting_ack: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    // ── REST helpers ──

    /// Make an authenticated Discord REST API call.
    async fn api_get<R: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<R> {
        let url = format!("{}{}", DISCORD_API_BASE, path);
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .send()
            .await
            .map_err(|e| anyhow!("discord api error: {e}"))?;

        let status = resp.status();
        let body = resp.text().await?;

        if !status.is_success() {
            return Err(anyhow!(
                "discord api GET {} returned {}: {}",
                path,
                status,
                body
            ));
        }

        serde_json::from_str(&body).map_err(|e| anyhow!("discord parse error: {e}"))
    }

    async fn api_post<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        payload: &T,
    ) -> Result<R> {
        let url = format!("{}{}", DISCORD_API_BASE, path);
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .json(payload)
            .send()
            .await
            .map_err(|e| anyhow!("discord api error: {e}"))?;

        let status = resp.status();
        let body = resp.text().await?;

        if !status.is_success() {
            return Err(anyhow!(
                "discord api POST {} returned {}: {}",
                path,
                status,
                body
            ));
        }

        serde_json::from_str(&body).map_err(|e| anyhow!("discord parse error: {e}"))
    }

    async fn api_put(&self, path: &str) -> Result<()> {
        let url = format!("{}{}", DISCORD_API_BASE, path);
        let resp = self
            .client
            .put(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .send()
            .await
            .map_err(|e| anyhow!("discord api error: {e}"))?;

        if !resp.status().is_success() {
            let body = resp.text().await?;
            return Err(anyhow!("discord api PUT {} failed: {}", path, body));
        }
        Ok(())
    }

    async fn api_delete(&self, path: &str) -> Result<()> {
        let url = format!("{}{}", DISCORD_API_BASE, path);
        let resp = self
            .client
            .delete(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .send()
            .await
            .map_err(|e| anyhow!("discord api error: {e}"))?;

        if !resp.status().is_success() {
            let body = resp.text().await?;
            return Err(anyhow!("discord api DELETE {} failed: {}", path, body));
        }
        Ok(())
    }

    // ── Message sending ──

    /// Send a message to a channel, returning the created message.
    async fn send_message(&self, channel_id: u64, content: &str) -> Result<DiscordMessage> {
        // Truncate to Discord's 2000-char limit
        let content = if content.len() > DISCORD_MAX_MESSAGE_LEN {
            format!("{}…", &content[..DISCORD_MAX_MESSAGE_LEN - 1])
        } else {
            content.to_string()
        };

        let payload = serde_json::json!({ "content": content });
        self.api_post(&format!("/channels/{}/messages", channel_id), &payload)
            .await
    }

    /// Edit an existing message.
    async fn edit_message(&self, channel_id: u64, message_id: u64, content: &str) -> Result<()> {
        let content = if content.len() > DISCORD_MAX_MESSAGE_LEN {
            format!("{}…", &content[..DISCORD_MAX_MESSAGE_LEN - 1])
        } else {
            content.to_string()
        };

        let url = format!(
            "{}/channels/{}/messages/{}",
            DISCORD_API_BASE, channel_id, message_id
        );
        let resp = self
            .client
            .patch(&url)
            .header("Authorization", format!("Bot {}", self.config.bot_token))
            .json(&serde_json::json!({ "content": content }))
            .send()
            .await
            .map_err(|e| anyhow!("discord edit error: {e}"))?;

        if !resp.status().is_success() {
            let body = resp.text().await?;
            return Err(anyhow!("discord edit failed: {}", body));
        }
        Ok(())
    }

    // ── Gateway WebSocket ──

    /// Connect to Discord Gateway and process events.
    /// Implements spec-compliant lifecycle: proactive heartbeat, ACK tracking,
    /// session resume, op 7/9 handling.
    async fn gateway_loop(&self, sink: TaskSink) {
        use futures::{SinkExt, StreamExt};
        use tokio_tungstenite::connect_async;

        let mut backoff_secs = 1u64;

        loop {
            if self.cancel.is_cancelled() {
                break;
            }

            // Determine connection URL (resume or fresh)
            let connect_url = {
                let resume_url = self.resume_gateway_url.lock().await;
                let session_id = self.session_id.lock().await;
                if resume_url.is_some() && session_id.is_some() {
                    resume_url.clone().unwrap()
                } else {
                    DISCORD_GATEWAY_URL.to_string()
                }
            };

            tracing::info!(url = %connect_url, "connecting to discord gateway");

            let ws_stream = match connect_async(&connect_url).await {
                Ok((stream, _)) => {
                    backoff_secs = 1; // Reset backoff on successful connect
                    stream
                }
                Err(e) => {
                    tracing::error!(error = %e, "discord gateway connect failed");
                    tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(60);
                    continue;
                }
            };

            let (ws_write, mut ws_read) = ws_stream.split();

            // Fan-in channel: heartbeat timer + event handler → single writer
            let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<String>(64);

            // Writer task: drains cmd_rx and sends to WebSocket
            let cancel_writer = self.cancel.clone();
            let writer_handle = tokio::spawn(async move {
                let mut ws_write = ws_write;
                loop {
                    tokio::select! {
                        _ = cancel_writer.cancelled() => break,
                        msg = cmd_rx.recv() => {
                            match msg {
                                Some(text) => {
                                    if let Err(e) = ws_write.send(
                                        tokio_tungstenite::tungstenite::Message::Text(text.into())
                                    ).await {
                                        tracing::error!(error = %e, "ws write failed");
                                        break;
                                    }
                                }
                                None => break, // channel closed
                            }
                        }
                    }
                }
                ws_write
            });

            // Reset ACK state
            self.awaiting_ack.store(false, Ordering::Relaxed);

            // Track whether we need to reconnect vs terminate
            let mut should_resume = true;

            // Process gateway events
            while let Some(msg_result) = ws_read.next().await {
                if self.cancel.is_cancelled() {
                    break;
                }

                let msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!(error = %e, "discord gateway read error");
                        break;
                    }
                };

                if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                    match self.handle_gateway_event_v2(&text, &sink, &cmd_tx).await {
                        Ok(GatewayAction::Continue) => {}
                        Ok(GatewayAction::Reconnect) => {
                            should_resume = true;
                            break;
                        }
                        Ok(GatewayAction::ReidentifyAfterDelay) => {
                            // Clear session to force fresh IDENTIFY
                            *self.session_id.lock().await = None;
                            *self.resume_gateway_url.lock().await = None;
                            should_resume = false;
                            break;
                        }
                        Ok(GatewayAction::ZombieDetected) => {
                            tracing::warn!("zombie connection detected, reconnecting");
                            should_resume = true;
                            break;
                        }
                        Err(e) => {
                            tracing::error!(error = %e, "discord gateway event error");
                        }
                    }
                }
            }

            // Clean up writer
            drop(cmd_tx);
            let _ = writer_handle.await;

            if self.cancel.is_cancelled() {
                break;
            }

            if !should_resume {
                // Fresh identify — jittered 1-5s delay per spec
                let jitter = 1
                    + (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .subsec_millis()
                        % 4000) as u64
                        / 1000;
                tracing::info!(delay = jitter, "re-identifying after invalid session");
                tokio::time::sleep(std::time::Duration::from_secs(jitter)).await;
            } else {
                tracing::warn!(
                    backoff = backoff_secs,
                    "discord gateway disconnected, reconnecting"
                );
                tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)).await;
                backoff_secs = (backoff_secs * 2).min(60);
            }
        }
    }

    /// Handle a gateway event and return an action.
    async fn handle_gateway_event_v2(
        &self,
        text: &str,
        sink: &TaskSink,
        cmd_tx: &tokio::sync::mpsc::Sender<String>,
    ) -> Result<GatewayAction> {
        let event: GatewayEvent =
            serde_json::from_str(text).map_err(|e| anyhow!("gateway parse: {e}"))?;

        // Update sequence
        if let Some(seq) = event.s {
            *self.sequence.lock().await = Some(seq);
        }

        match event.op {
            // Opcode 10: Hello — start heartbeat and identify/resume
            10 => {
                let interval_ms = event
                    .d
                    .as_ref()
                    .and_then(|d| d.get("heartbeat_interval"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(41250);

                tracing::debug!(interval_ms, "discord heartbeat interval");

                // Spawn proactive heartbeat timer
                let cancel_hb = self.cancel.clone();
                let seq_ref = Arc::clone(&self.sequence);
                let awaiting_ref = Arc::clone(&self.awaiting_ack);
                let hb_tx = cmd_tx.clone();
                tokio::spawn(async move {
                    // First heartbeat: jittered delay (0..interval_ms)
                    let jitter = (std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .subsec_millis() as u64
                        * interval_ms)
                        / 1000;
                    let first_delay = jitter % interval_ms;
                    tokio::time::sleep(std::time::Duration::from_millis(first_delay)).await;

                    loop {
                        if cancel_hb.is_cancelled() {
                            break;
                        }

                        // Check if we're still awaiting ACK from last heartbeat
                        if awaiting_ref.load(Ordering::Relaxed) {
                            // Zombie detected — signal reconnect by closing the channel
                            tracing::warn!("heartbeat ACK not received — zombie connection");
                            break;
                        }

                        let seq_val = *seq_ref.lock().await;
                        let hb = serde_json::json!({ "op": 1, "d": seq_val });
                        awaiting_ref.store(true, Ordering::Relaxed);

                        if hb_tx.send(hb.to_string()).await.is_err() {
                            break; // Writer closed
                        }

                        tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
                    }
                });

                // Check if we should RESUME or IDENTIFY
                let session_id = self.session_id.lock().await.clone();
                let seq = *self.sequence.lock().await;

                if let (Some(sid), Some(s)) = (session_id, seq) {
                    // RESUME (op 6)
                    let resume = serde_json::json!({
                        "op": 6,
                        "d": {
                            "token": self.config.bot_token,
                            "session_id": sid,
                            "seq": s
                        }
                    });
                    cmd_tx
                        .send(resume.to_string())
                        .await
                        .map_err(|e| anyhow!("resume send failed: {e}"))?;
                    tracing::info!("sent RESUME");
                } else {
                    // IDENTIFY (op 2)
                    let identify = serde_json::json!({
                        "op": 2,
                        "d": {
                            "token": self.config.bot_token,
                            "intents": 512 | 32768,
                            "properties": {
                                "os": std::env::consts::OS,
                                "browser": "pipit-daemon",
                                "device": "pipit-daemon"
                            }
                        }
                    });
                    cmd_tx
                        .send(identify.to_string())
                        .await
                        .map_err(|e| anyhow!("identify send failed: {e}"))?;
                    tracing::info!("sent IDENTIFY");
                }
            }

            // Opcode 1: Heartbeat request from server — respond immediately
            1 => {
                let seq_val = *self.sequence.lock().await;
                let hb = serde_json::json!({ "op": 1, "d": seq_val });
                cmd_tx
                    .send(hb.to_string())
                    .await
                    .map_err(|e| anyhow!("heartbeat send failed: {e}"))?;
            }

            // Opcode 11: Heartbeat ACK — clear awaiting flag
            11 => {
                self.awaiting_ack.store(false, Ordering::Relaxed);
            }

            // Opcode 7: Reconnect — server requests we reconnect
            7 => {
                tracing::info!("discord requested reconnect (op 7)");
                return Ok(GatewayAction::Reconnect);
            }

            // Opcode 9: Invalid Session — re-identify (d: false) or resume (d: true)
            9 => {
                let resumable = event.d.as_ref().and_then(|v| v.as_bool()).unwrap_or(false);

                if resumable {
                    tracing::info!("invalid session (resumable), reconnecting");
                    return Ok(GatewayAction::Reconnect);
                } else {
                    tracing::warn!("invalid session (not resumable), re-identifying");
                    return Ok(GatewayAction::ReidentifyAfterDelay);
                }
            }

            // Opcode 0: Dispatch (events)
            0 => {
                if let Some(ref event_name) = event.t {
                    match event_name.as_str() {
                        "READY" => {
                            if let Some(ref d) = event.d {
                                // Extract bot user ID
                                if let Some(user) = d.get("user") {
                                    if let Some(id) = user.get("id").and_then(|v| v.as_str()) {
                                        if let Ok(uid) = id.parse::<u64>() {
                                            self.bot_user_id.store(uid, Ordering::Relaxed);
                                            let username = user
                                                .get("username")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("unknown");
                                            tracing::info!(
                                                bot_id = uid,
                                                username,
                                                "discord bot ready"
                                            );
                                        }
                                    }
                                }

                                // Cache session_id and resume_gateway_url
                                if let Some(sid) = d.get("session_id").and_then(|v| v.as_str()) {
                                    *self.session_id.lock().await = Some(sid.to_string());
                                }
                                if let Some(url) =
                                    d.get("resume_gateway_url").and_then(|v| v.as_str())
                                {
                                    let resume_url = format!("{}/?v=10&encoding=json", url);
                                    *self.resume_gateway_url.lock().await = Some(resume_url);
                                }
                            }
                        }

                        "RESUMED" => {
                            tracing::info!("discord session resumed");
                        }

                        "MESSAGE_CREATE" => {
                            if let Some(ref d) = event.d {
                                self.handle_message_create(d, sink).await;
                            }
                        }

                        _ => {
                            tracing::trace!(event = event_name, "unhandled discord event");
                        }
                    }
                }
            }

            _ => {
                tracing::trace!(op = event.op, "unhandled discord gateway op");
            }
        }

        Ok(GatewayAction::Continue)
    }

    /// Process MESSAGE_CREATE dispatch event.
    async fn handle_message_create(&self, d: &serde_json::Value, sink: &TaskSink) {
        // Ignore bot's own messages
        let author = match d.get("author") {
            Some(a) => a,
            None => return,
        };
        if author.get("bot").and_then(|v| v.as_bool()).unwrap_or(false) {
            return;
        }
        let author_id: u64 = author
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // Guild check
        let guild_id: Option<u64> = d
            .get("guild_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok());

        if !self.config.allowed_guilds.is_empty() {
            if let Some(gid) = guild_id {
                if !self.config.allowed_guilds.contains(&gid) {
                    return;
                }
            }
        }

        let channel_id: u64 = d
            .get("channel_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let message_id: u64 = d
            .get("id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        let content = d.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if content.is_empty() {
            return;
        }

        let sender_name = author
            .get("username")
            .and_then(|v| v.as_str())
            .map(String::from);

        // Check if bot is mentioned (for guild channels)
        let bot_id = self.bot_user_id.load(Ordering::Relaxed);
        let mentions_bot = d
            .get("mentions")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter().any(|m| {
                    m.get("id")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<u64>().ok())
                        .map(|id| id == bot_id)
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);

        let is_dm = guild_id.is_none();

        // Only process DMs or messages that mention the bot
        if !is_dm && !mentions_bot {
            return;
        }

        // Strip bot mention from content
        let cleaned = if mentions_bot {
            let mention_pattern = format!("<@{}>", bot_id);
            let alt_pattern = format!("<@!{}>", bot_id);
            content
                .replace(&mention_pattern, "")
                .replace(&alt_pattern, "")
                .trim()
                .to_string()
        } else {
            content.to_string()
        };

        if cleaned.is_empty() {
            return;
        }

        // Bot commands
        if cleaned.starts_with('!') {
            self.handle_command(&cleaned, channel_id).await;
            return;
        }

        // Parse project prefix
        let (project, prompt) = self.parse_project_prefix(&cleaned);

        let origin = MessageOrigin::Discord {
            guild_id,
            channel_id,
            message_id: Some(message_id),
            thread_id: None,
        };

        let mut task = NormalizedTask::new(project, prompt, origin);
        if let Some(name) = sender_name {
            task = task.with_sender(name);
        }

        match sink.send(task).await {
            Ok(_) => {
                tracing::info!(channel_id, "task submitted from discord");
                // React with 👀 to acknowledge
                let _ = self
                    .api_put(&format!(
                        "/channels/{}/messages/{}/reactions/%F0%9F%91%80/@me",
                        channel_id, message_id
                    ))
                    .await;
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to submit discord task");
                let _ = self
                    .send_message(channel_id, "⚠ Queue is full. Try again later.")
                    .await;
            }
        }
    }

    /// Parse `@project_name rest of prompt` pattern.
    fn parse_project_prefix(&self, text: &str) -> (String, String) {
        if text.starts_with('@') {
            let parts: Vec<&str> = text.splitn(2, ' ').collect();
            if parts.len() == 2 {
                let candidate = &parts[0][1..];
                if self.project_names.contains(candidate) {
                    return (candidate.to_string(), parts[1].to_string());
                }
            }
        }
        (self.default_project.clone(), text.to_string())
    }

    /// Handle bot commands (prefixed with `!`).
    async fn handle_command(&self, text: &str, channel_id: u64) {
        let cmd = text.split_whitespace().next().unwrap_or("");
        match cmd {
            "!help" => {
                let help = "🤖 **Pipit Daemon**\n\n\
                    Mention me or DM me to run a coding task.\n\n\
                    Use `@project prompt` to target a specific project.\n\n\
                    **Commands:**\n\
                    `!status` — Queue and project status\n\
                    `!projects` — List configured projects\n\
                    `!cancel` — Cancel running task\n\
                    `!last` — Show last task result";
                let _ = self.send_message(channel_id, help).await;
            }
            "!status" => {
                let _ = self
                    .send_message(channel_id, "Status: use the HTTP API for detailed status.")
                    .await;
            }
            "!projects" => {
                let list: Vec<String> = self.project_names.iter().cloned().collect();
                let msg = format!("Configured projects: {}", list.join(", "));
                let _ = self.send_message(channel_id, &msg).await;
            }
            _ => {
                let _ = self
                    .send_message(channel_id, "Unknown command. Try `!help`")
                    .await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Channel trait implementations
// ---------------------------------------------------------------------------

#[async_trait]
impl Channel for DiscordChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Discord
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Discord".to_string(),
            supports_streaming: false,
            supports_threads: true,
            supports_reactions: true,
            max_message_length: Some(DISCORD_MAX_MESSAGE_LEN),
        }
    }

    async fn start(&self, sink: TaskSink) -> Result<(), ChannelError> {
        tracing::info!("starting discord channel");

        // Verify bot token
        let _: serde_json::Value = self
            .api_get("/users/@me")
            .await
            .map_err(|e| ChannelError::AuthFailed(e.to_string()))?;

        tracing::info!("discord bot authenticated");

        // NOTE: In production the gateway_loop should be spawned on an Arc<Self>.
        // For the scaffold, we log readiness — the DaemonRunner will spawn it.
        tracing::info!("discord channel started (gateway placeholder)");

        Ok(())
    }

    async fn send_update(&self, update: TaskUpdate) -> Result<(), ChannelError> {
        if let MessageOrigin::Discord {
            channel_id,
            thread_id,
            ..
        } = &update.origin
        {
            // Prefer thread channel if available
            let target = thread_id.unwrap_or(*channel_id);

            let text = match &update.kind {
                TaskUpdateKind::Started { project, model } => {
                    format!("▸ Working on **{}** ({})", project, model)
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
                TaskUpdateKind::ToolStarted { name, .. } => format!("○ `{}`", name),
                TaskUpdateKind::ToolCompleted { name, success, .. } => {
                    let icon = if *success { "●" } else { "✗" };
                    format!("{} `{}`", icon, name)
                }
            };

            self.send_message(target, &text)
                .await
                .map_err(|e| ChannelError::Other(e.to_string()))?;
        }

        Ok(())
    }

    async fn stop(&self) -> Result<(), ChannelError> {
        tracing::info!("stopping discord channel");
        Ok(())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[async_trait]
impl ThreadedChannel for DiscordChannel {
    async fn create_thread(
        &self,
        origin: &MessageOrigin,
        title: &str,
    ) -> Result<MessageOrigin, ChannelError> {
        if let MessageOrigin::Discord {
            guild_id,
            channel_id,
            message_id,
            ..
        } = origin
        {
            // Create a thread from the original message
            let path = if let Some(msg_id) = message_id {
                format!("/channels/{}/messages/{}/threads", channel_id, msg_id)
            } else {
                format!("/channels/{}/threads", channel_id)
            };

            let params = serde_json::json!({
                "name": &title[..title.len().min(100)],
                "auto_archive_duration": 1440 // 24 hours
            });

            let thread: serde_json::Value = self
                .api_post(&path, &params)
                .await
                .map_err(|e| ChannelError::Other(e.to_string()))?;

            let thread_id: u64 = thread
                .get("id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .ok_or_else(|| ChannelError::Other("no thread id in response".to_string()))?;

            Ok(MessageOrigin::Discord {
                guild_id: *guild_id,
                channel_id: *channel_id,
                message_id: None,
                thread_id: Some(thread_id),
            })
        } else {
            Err(ChannelError::Other("not a discord origin".to_string()))
        }
    }
}

#[async_trait]
impl ReactiveChannel for DiscordChannel {
    async fn add_reaction(&self, origin: &MessageOrigin, emoji: &str) -> Result<(), ChannelError> {
        if let MessageOrigin::Discord {
            channel_id,
            message_id: Some(msg_id),
            ..
        } = origin
        {
            // URL-encode the emoji
            let encoded = urlencoding::encode(emoji);
            self.api_put(&format!(
                "/channels/{}/messages/{}/reactions/{}/@me",
                channel_id, msg_id, encoded
            ))
            .await
            .map_err(|e| ChannelError::Other(e.to_string()))?;
        }
        Ok(())
    }

    async fn remove_reaction(
        &self,
        origin: &MessageOrigin,
        emoji: &str,
    ) -> Result<(), ChannelError> {
        if let MessageOrigin::Discord {
            channel_id,
            message_id: Some(msg_id),
            ..
        } = origin
        {
            let encoded = urlencoding::encode(emoji);
            self.api_delete(&format!(
                "/channels/{}/messages/{}/reactions/{}/@me",
                channel_id, msg_id, encoded
            ))
            .await
            .map_err(|e| ChannelError::Other(e.to_string()))?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Gateway action and types
// ---------------------------------------------------------------------------

/// Action returned by handle_gateway_event_v2 to control the reconnect loop.
enum GatewayAction {
    Continue,
    Reconnect,
    ReidentifyAfterDelay,
    ZombieDetected,
}

#[derive(Debug, Deserialize)]
struct GatewayEvent {
    op: u8,
    d: Option<serde_json::Value>,
    s: Option<u64>,
    t: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DiscordMessage {
    id: String,
    channel_id: String,
    content: Option<String>,
}
