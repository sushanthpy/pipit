use crate::ContextError;
use pipit_provider::{CompletionRequest, ContentEvent, LlmProvider, Message};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// Token budget model.
#[derive(Debug, Clone)]
pub struct TokenBudget {
    pub model_limit: u64,
    pub system_prompt: u64,
    pub repo_map: u64,
    pub output_reserve: u64,
    pub tool_result_reserve: u64,
    pub available_for_history: u64,
}

impl TokenBudget {
    pub fn compute(
        model_limit: u64,
        system_prompt_tokens: u64,
        repo_map_tokens: u64,
        output_reserve: u64,
        tool_result_reserve: u64,
    ) -> Self {
        let available = model_limit
            .saturating_sub(system_prompt_tokens)
            .saturating_sub(repo_map_tokens)
            .saturating_sub(output_reserve)
            .saturating_sub(tool_result_reserve);

        Self {
            model_limit,
            system_prompt: system_prompt_tokens,
            repo_map: repo_map_tokens,
            output_reserve,
            tool_result_reserve,
            available_for_history: available,
        }
    }
}

/// Manages conversation context, compression, and token budgeting.
pub struct ContextManager {
    messages: Vec<Message>,
    system_prompt: String,
    budget: TokenBudget,
    settings: ContextSettings,
    total_cost: f64,
    session_dir: Option<PathBuf>,
}

/// Reserve tokens for model output generation.
const DEFAULT_OUTPUT_RESERVE: u64 = 4096;
/// Reserve tokens for tool results injected into the conversation.
const DEFAULT_TOOL_RESULT_RESERVE: u64 = 8192;
/// Trigger context compression when token usage exceeds this fraction of the budget.
/// 0.85 = compress when 85% of available history tokens are consumed.
const DEFAULT_COMPRESSION_THRESHOLD: f64 = 0.85;
/// Number of most-recent messages to preserve (never compress) during summarization.
const DEFAULT_PRESERVE_RECENT_MESSAGES: usize = 4;

#[derive(Debug, Clone)]
pub struct ContextSettings {
    pub output_reserve: u64,
    pub tool_result_reserve: u64,
    pub compression_threshold: f64,
    pub preserve_recent_messages: usize,
    /// Maximum output tokens to request from the model.
    pub max_output_tokens: u32,
    /// Per-tool-result truncation limit (chars). 0 = no truncation.
    pub tool_result_max_chars: usize,
}

impl Default for ContextSettings {
    fn default() -> Self {
        Self {
            output_reserve: DEFAULT_OUTPUT_RESERVE,
            tool_result_reserve: DEFAULT_TOOL_RESULT_RESERVE,
            compression_threshold: DEFAULT_COMPRESSION_THRESHOLD,
            preserve_recent_messages: DEFAULT_PRESERVE_RECENT_MESSAGES,
            max_output_tokens: 8192,
            tool_result_max_chars: 32_000,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub total: u64,
    pub limit: u64,
    pub cost: f64,
}

#[derive(Debug, Clone, Default)]
pub struct CompressionStats {
    pub messages_removed: usize,
    pub tokens_freed: u64,
}

impl ContextManager {
    pub fn new(system_prompt: String, model_limit: u64) -> Self {
        Self::with_settings(system_prompt, model_limit, ContextSettings::default())
    }

    pub fn with_settings(
        system_prompt: String,
        model_limit: u64,
        settings: ContextSettings,
    ) -> Self {
        let system_tokens = (system_prompt.len() as u64) / 4;
        let budget = TokenBudget::compute(
            model_limit,
            system_tokens,
            0,
            settings.output_reserve,
            settings.tool_result_reserve,
        );

        Self {
            messages: Vec::new(),
            system_prompt,
            budget,
            settings,
            total_cost: 0.0,
            session_dir: None,
        }
    }

    pub fn set_session_dir(&mut self, dir: PathBuf) {
        self.session_dir = Some(dir);
    }

    pub fn update_repo_map_tokens(&mut self, tokens: u64) {
        self.budget = TokenBudget::compute(
            self.budget.model_limit,
            self.budget.system_prompt,
            tokens,
            self.budget.output_reserve,
            self.budget.tool_result_reserve,
        );
    }

    pub fn push_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    /// Push a tool result message, truncating content if it exceeds the configured limit.
    pub fn push_tool_result(&mut self, call_id: &str, content: &str, is_error: bool) {
        let max_chars = self.settings.tool_result_max_chars;
        let truncated = if max_chars > 0 && content.len() > max_chars {
            let lines: Vec<&str> = content.lines().collect();
            let total = lines.len();
            let head = 40.min(total);
            let tail = 40.min(total.saturating_sub(head));
            if total > head + tail {
                format!(
                    "{}\n\n[...truncated {} of {} lines...]\n\n{}",
                    lines[..head].join("\n"),
                    total - head - tail,
                    total,
                    lines[total - tail..].join("\n"),
                )
            } else {
                content[..max_chars].to_string()
            }
        } else {
            content.to_string()
        };
        self.messages.push(Message::tool_result(call_id, &truncated, is_error));
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Estimate total token usage of current history.
    pub fn estimate_token_usage(&self) -> u64 {
        self.messages.iter().map(|m| m.estimated_tokens()).sum()
    }

    /// Check if compression is needed.
    pub fn needs_compression(&self) -> bool {
        let usage = self.estimate_token_usage();
        let ratio = usage as f64 / self.budget.available_for_history as f64;
        ratio > self.settings.compression_threshold
    }

    /// Pre-flight check: estimate total request size and return how much over budget we are.
    /// Returns (estimated_input_tokens, model_limit, over_by) where over_by > 0 means overflow.
    pub fn preflight_check(&self, tools_count: usize, repo_map: Option<&str>) -> (u64, u64, i64) {
        let system_tokens = (self.system_prompt.len() as u64) / 4;
        let repo_map_tokens = repo_map.map(|m| (m.len() as u64) / 4).unwrap_or(self.budget.repo_map);
        let history_tokens = self.estimate_token_usage();
        let tools_tokens = (tools_count as u64) * 50;
        let output_reserve = self.settings.max_output_tokens as u64;

        let total_input = system_tokens + repo_map_tokens + history_tokens + tools_tokens + output_reserve;
        let over = total_input as i64 - self.budget.model_limit as i64;
        (total_input, self.budget.model_limit, over)
    }

    /// Proactively truncate old tool results in history to free tokens.
    /// Returns number of tokens freed.
    pub fn shrink_tool_results(&mut self, target_chars: usize) -> u64 {
        let mut freed = 0u64;
        for msg in &mut self.messages {
            for block in &mut msg.content {
                if let pipit_provider::ContentBlock::ToolResult { content, .. } = block {
                    if content.len() > target_chars {
                        let old_tokens = (content.len() as u64) / 4;
                        let lines: Vec<&str> = content.lines().collect();
                        let total = lines.len();
                        let head = 20.min(total);
                        let tail = 20.min(total.saturating_sub(head));
                        if total > head + tail {
                            *content = format!(
                                "{}\n\n[...truncated {} of {} lines to free context...]\n\n{}",
                                lines[..head].join("\n"),
                                total - head - tail,
                                total,
                                lines[total - tail..].join("\n"),
                            );
                        } else {
                            *content = content[..target_chars].to_string();
                        }
                        let new_tokens = (content.len() as u64) / 4;
                        freed += old_tokens.saturating_sub(new_tokens);
                    }
                }
            }
        }
        freed
    }

    /// Compress old messages by summarizing them.
    pub async fn compress(
        &mut self,
        provider: &dyn LlmProvider,
        cancel: CancellationToken,
    ) -> Result<CompressionStats, ContextError> {
        if self.messages.len() <= self.settings.preserve_recent_messages {
            return Ok(CompressionStats::default());
        }

        let split_point = self.messages.len() - self.settings.preserve_recent_messages;
        let to_summarize = &self.messages[..split_point];
        let to_keep = self.messages[split_point..].to_vec();

        // Fix #18: Structured compression prompt for consistent summaries
        let summary_request = CompletionRequest {
            system: "Summarize this conversation as structured context. Output a concise summary with these sections:\n\
                     FILES_MODIFIED: List all file paths that were created or edited.\n\
                     DECISIONS: Key technical decisions made.\n\
                     CURRENT_TASK: What the user is currently working on.\n\
                     KEY_CONTEXT: Any other important context (errors encountered, patterns used, etc.).\n\
                     Be concise. Omit tool result details and intermediate thinking."
                .to_string(),
            messages: to_summarize.to_vec(),
            tools: vec![],
            max_tokens: Some(2048),
            temperature: Some(0.0),
            stop_sequences: vec![],
        };

        let mut stream = provider
            .complete(summary_request, cancel)
            .await
            .map_err(|e| ContextError::Other(format!("Compression failed: {}", e)))?;

        use futures::StreamExt;
        let mut summary_text = String::new();
        while let Some(event) = stream.next().await {
            if let Ok(ContentEvent::ContentDelta { text }) = event {
                summary_text.push_str(&text);
            }
        }

        let old_count = to_summarize.len();
        let old_tokens: u64 = to_summarize.iter().map(|m| m.estimated_tokens()).sum();

        let summary_message = Message::system(format!(
            "[Conversation summary]\n{}",
            summary_text
        ));

        let new_tokens = summary_message.estimated_tokens();

        self.messages = std::iter::once(summary_message)
            .chain(to_keep)
            .collect();

        Ok(CompressionStats {
            messages_removed: old_count,
            tokens_freed: old_tokens.saturating_sub(new_tokens),
        })
    }

    /// Build a CompletionRequest from current state.
    pub fn build_request(
        &self,
        tools: &[pipit_provider::ToolDeclaration],
        repo_map: Option<&str>,
    ) -> CompletionRequest {
        let mut system = self.system_prompt.clone();
        if let Some(map) = repo_map {
            system.push_str("\n\n");
            system.push_str(map);
        }

        // Calculate max_tokens: min(configured_max_output, remaining_budget)
        let input_estimate = self.budget.system_prompt
            + self.budget.repo_map
            + self.estimate_token_usage()
            + (tools.len() as u64) * 50; // ~50 tokens per tool schema
        let remaining = self.budget.model_limit.saturating_sub(input_estimate);
        let max_output = (self.settings.max_output_tokens as u64).min(remaining).max(256);

        CompletionRequest {
            system,
            messages: self.messages.clone(),
            tools: tools.to_vec(),
            temperature: Some(0.0),
            max_tokens: Some(max_output as u32),
            stop_sequences: vec![],
        }
    }

    /// Get current token usage stats.
    pub fn token_usage(&self) -> TokenUsage {
        TokenUsage {
            total: self.estimate_token_usage(),
            limit: self.budget.available_for_history,
            cost: self.total_cost,
        }
    }

    /// Update cost tracking.
    pub fn add_cost(&mut self, cost: f64) {
        self.total_cost += cost;
    }

    /// Persist session to disk.
    pub fn persist_session(&self) -> Result<(), ContextError> {
        let dir = match &self.session_dir {
            Some(d) => d,
            None => return Ok(()),
        };

        std::fs::create_dir_all(dir)?;
        let session_file = dir.join("session.json");
        let json = serde_json::to_string_pretty(&self.messages)
            .map_err(|e| ContextError::Serialization(e.to_string()))?;
        std::fs::write(&session_file, json)?;

        Ok(())
    }

    /// Load session from disk.
    pub fn load_session(path: &Path) -> Result<Vec<Message>, ContextError> {
        let content = std::fs::read_to_string(path)?;
        let messages: Vec<Message> = serde_json::from_str(&content)
            .map_err(|e| ContextError::Serialization(e.to_string()))?;
        Ok(messages)
    }

    /// Clear all messages.
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Restore messages from a previous session.
    pub fn restore_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    /// Deterministically shrink history when the provider rejects the request body size.
    pub fn force_shrink_for_transport(&mut self) -> CompressionStats {
        if self.messages.is_empty() {
            return CompressionStats::default();
        }

        let keep_recent = self.settings.preserve_recent_messages.clamp(1, 2);
        if self.messages.len() > keep_recent {
            let split_point = self.messages.len() - keep_recent;
            let to_summarize = &self.messages[..split_point];
            let to_keep = self.messages[split_point..].to_vec();
            let old_tokens: u64 = to_summarize.iter().map(|m| m.estimated_tokens()).sum();
            let summary = build_transport_summary(to_summarize);
            let summary_message = Message::system(format!(
                "[Transport fallback summary]\n{}",
                summary
            ));
            let new_tokens = summary_message.estimated_tokens();
            self.messages = std::iter::once(summary_message).chain(to_keep).collect();

            return CompressionStats {
                messages_removed: split_point,
                tokens_freed: old_tokens.saturating_sub(new_tokens),
            };
        }

        let old_tokens = self.estimate_token_usage();
        for message in &mut self.messages {
            truncate_message_for_transport(message, 480);
        }

        CompressionStats {
            messages_removed: 0,
            tokens_freed: old_tokens.saturating_sub(self.estimate_token_usage()),
        }
    }
}

fn build_transport_summary(messages: &[Message]) -> String {
    let mut lines = Vec::new();

    for message in messages.iter().rev().take(8).rev() {
        let role = match &message.role {
            pipit_provider::Role::System => "system",
            pipit_provider::Role::User => "user",
            pipit_provider::Role::Assistant => "assistant",
            pipit_provider::Role::ToolResult { .. } => "tool",
        };

        let text = message.text_content();
        if !text.trim().is_empty() {
            lines.push(format!("- {}: {}", role, truncate_text(&text, 220)));
            continue;
        }

        let tool_calls = message.tool_calls();
        if !tool_calls.is_empty() {
            let call_summaries = tool_calls
                .iter()
                .take(3)
                .map(|call| format!("{} {}", call.tool_name, truncate_text(&call.args.to_string(), 100)))
                .collect::<Vec<_>>()
                .join(" | ");
            lines.push(format!("- {}: {}", role, call_summaries));
        }
    }

    if lines.is_empty() {
        "Older conversation was compressed locally after the provider rejected the request body size.".to_string()
    } else {
        lines.join("\n")
    }
}

fn truncate_message_for_transport(message: &mut Message, max_chars: usize) {
    for block in &mut message.content {
        match block {
            pipit_provider::ContentBlock::Text(text)
            | pipit_provider::ContentBlock::Thinking(text)
            | pipit_provider::ContentBlock::ToolResult { content: text, .. } => {
                *text = truncate_text(text, max_chars);
            }
            pipit_provider::ContentBlock::ToolCall { args, .. } => {
                let compact = truncate_text(&args.to_string(), max_chars);
                *args = serde_json::Value::String(compact);
            }
            _ => {}
        }
    }
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_chars).collect();
        format!("{}...", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_shrink_for_transport_replaces_old_history_with_local_summary() {
        let mut manager = ContextManager::with_settings(
            "system".to_string(),
            10_000,
            ContextSettings {
                preserve_recent_messages: 4,
                ..ContextSettings::default()
            },
        );
        manager.push_message(Message::user("one"));
        manager.push_message(Message::assistant("two"));
        manager.push_message(Message::user("three"));
        manager.push_message(Message::assistant("four"));
        manager.push_message(Message::user("five"));

        let stats = manager.force_shrink_for_transport();

        assert!(stats.messages_removed > 0);
        assert!(manager.messages()[0].text_content().contains("Transport fallback summary"));
        assert!(manager.messages().len() <= 3);
    }
}
