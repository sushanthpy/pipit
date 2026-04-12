use crate::ContextError;
use pipit_provider::{CompletionRequest, ContentBlock, ContentEvent, LlmProvider, Message};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Atomically write data to a file: write(tmp) → fsync → rename(tmp, target).
fn atomic_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Estimate tokens from raw text using content-aware heuristics.
/// Uses provider-calibrated ratios and content-type detection for
/// accuracy within ~5% of actual tokenizer output.
pub fn estimate_text_tokens(text: &str) -> u64 {
    estimate_text_tokens_for_provider(text, "anthropic")
}

/// Provider-specific token estimation with content-type awareness.
/// Calibrated ratios:
///   - Code (high punctuation): ~3.2 chars/token
///   - CJK text: ~1.5 chars/token
///   - English prose: ~3.5-4.0 chars/token depending on provider
pub fn estimate_text_tokens_for_provider(text: &str, provider: &str) -> u64 {
    if text.is_empty() {
        return 0;
    }
    let len = text.len();

    // Base chars-per-token ratio by provider
    let base_ratio = match provider {
        "anthropic" => 3.5,
        "openai" => 3.8,
        "google" | "gemini" => 3.4,
        _ => 4.0,
    };

    // Content-type detection
    let punct = text.bytes().filter(|b| b.is_ascii_punctuation()).count();
    let punct_ratio = punct as f64 / len as f64;

    // CJK detection (Unicode ranges for CJK Unified Ideographs)
    let cjk_chars = text
        .chars()
        .filter(|c| {
            let cp = *c as u32;
            (0x4E00..=0x9FFF).contains(&cp)      // CJK Unified
            || (0x3400..=0x4DBF).contains(&cp) // CJK Extension A
            || (0x3040..=0x309F).contains(&cp) // Hiragana
            || (0x30A0..=0x30FF).contains(&cp) // Katakana
            || (0xAC00..=0xD7AF).contains(&cp) // Hangul
        })
        .count();
    let cjk_ratio = cjk_chars as f64 / text.chars().count().max(1) as f64;

    let adjusted = if cjk_ratio > 0.2 {
        // CJK-heavy text: ~1.5 chars per token
        base_ratio * 0.4
    } else if punct_ratio > 0.15 {
        // Code: higher punctuation density means more tokens per char
        base_ratio * 0.85
    } else {
        base_ratio
    };

    (len as f64 / adjusted).ceil() as u64
}

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
    /// Optional WAL for pre-API-call transcript persistence.
    transcript_wal: Option<crate::transcript::TranscriptWal>,
    /// Actual input token count from the last API response.
    /// More accurate than char-based estimates; drives usage-based compaction trigger.
    last_api_input_tokens: Option<u64>,
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
            max_output_tokens: 16_384,
            tool_result_max_chars: 131_072,
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
        let system_tokens = estimate_text_tokens(&system_prompt);
        // output_reserve must match max_output_tokens so the compression
        // trigger correctly accounts for the full output budget.
        // Previously output_reserve was a separate smaller value (4096)
        // which meant compression didn't trigger early enough when
        // max_output_tokens was raised to 16K-32K.
        let effective_output_reserve = (settings.max_output_tokens as u64)
            .max(settings.output_reserve);
        let budget = TokenBudget::compute(
            model_limit,
            system_tokens,
            0,
            effective_output_reserve,
            settings.tool_result_reserve,
        );

        Self {
            messages: Vec::new(),
            system_prompt,
            budget,
            settings,
            total_cost: 0.0,
            session_dir: None,
            transcript_wal: None,
            last_api_input_tokens: None,
        }
    }

    pub fn set_session_dir(&mut self, dir: PathBuf) {
        self.session_dir = Some(dir);
    }

    /// Enable transcript WAL for pre-API-call persistence.
    pub fn enable_transcript(&mut self, wal: crate::transcript::TranscriptWal) {
        self.transcript_wal = Some(wal);
    }

    /// Get a reference to the transcript WAL (if enabled).
    pub fn transcript_wal(&self) -> Option<&crate::transcript::TranscriptWal> {
        self.transcript_wal.as_ref()
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
        // Flush to WAL BEFORE adding to context — ensures recovery on crash.
        // Skip WAL for ephemeral control-plane messages (they're transient).
        if !message.metadata.ephemeral {
            if let Some(ref mut wal) = self.transcript_wal {
                if let Err(e) = wal.append_message(&message) {
                    tracing::warn!("WAL append failed (continuing): {}", e);
                }
            }
        }
        self.messages.push(message);
    }

    /// Push an ephemeral control-plane message. Included in the next API
    /// request but automatically drained afterward so it does not
    /// contaminate future requests. Use for loop-recovery nudges, budget
    /// warnings, verification feedback, and auto-continue prompts.
    pub fn push_control_plane(&mut self, text: &str) {
        self.messages.push(Message::control_plane(text));
    }

    /// Remove all ephemeral control-plane messages from the context.
    /// Called after each API response so runtime self-talk does not
    /// accumulate in the conversational history.
    pub fn drain_ephemeral(&mut self) {
        self.messages.retain(|m| !m.metadata.ephemeral);
    }

    /// Push a tool result message with noise reduction and micro-compaction.
    ///
    /// Pipeline (runs in order):
    ///   1. strip_ansi — remove terminal escape codes
    ///   2. strip_noise_lines — remove progress bars, download logs, etc.
    ///   3. collapse_blanks — normalize whitespace runs
    ///   4. head_tail_split — smart truncation with error rescue
    ///   5. micro_compact — final head/tail split if still over threshold
    pub fn push_tool_result(&mut self, call_id: &str, content: &str, is_error: bool) {
        const HEAD_TAIL_MAX_LINES: usize = 2000;
        const MICRO_COMPACT_THRESHOLD: usize = 65_536; // 64KB
        const KEEP_HEAD_LINES: usize = 500;
        const KEEP_TAIL_LINES: usize = 500;

        let max_chars = self.settings.tool_result_max_chars;

        // ── Noise reduction pipeline (stages 1-4) ──
        let cleaned = crate::tool_noise::clean_tool_output(content, HEAD_TAIL_MAX_LINES, is_error);

        // ── Stage 5: micro-compaction for very large results ──
        let truncated = if cleaned.len() > MICRO_COMPACT_THRESHOLD {
            let lines: Vec<&str> = cleaned.lines().collect();
            let total = lines.len();
            let head = KEEP_HEAD_LINES.min(total);
            let tail = KEEP_TAIL_LINES.min(total.saturating_sub(head));
            if total > head + tail {
                format!(
                    "{}\n\n[...{} of {} lines omitted for context efficiency...]\n\n{}",
                    lines[..head].join("\n"),
                    total - head - tail,
                    total,
                    lines[total - tail..].join("\n"),
                )
            } else {
                cleaned
            }
        } else if max_chars > 0 && cleaned.len() > max_chars {
            cleaned[..max_chars].to_string()
        } else {
            cleaned
        };
        self.messages
            .push(Message::tool_result(call_id, &truncated, is_error));
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Remove the last assistant message from the conversation history.
    /// Used when retrying after a StopReason::Error — the failed response
    /// should not persist in context since the model will produce a replacement.
    pub fn pop_last_assistant(&mut self) {
        if let Some(pos) = self
            .messages
            .iter()
            .rposition(|m| m.role == pipit_provider::Role::Assistant)
        {
            self.messages.remove(pos);
        }
    }

    /// Strip the last assistant message AND all subsequent tool-result messages.
    ///
    /// Used for malformed-request recovery: when the provider rejects the
    /// conversation because a tool-call or its results contain content the
    /// API can't parse. Removes the entire corrupted turn so the next
    /// completion request starts from a clean state.
    ///
    /// Returns the number of messages removed.
    pub fn strip_last_assistant_turn(&mut self) -> usize {
        let Some(pos) = self
            .messages
            .iter()
            .rposition(|m| m.role == pipit_provider::Role::Assistant)
        else {
            return 0;
        };

        // Remove the assistant message and all tool-result messages after it
        let before_len = self.messages.len();
        let mut i = self.messages.len();
        while i > pos {
            i -= 1;
            let is_tool_result = matches!(
                self.messages[i].role,
                pipit_provider::Role::ToolResult { .. }
            );
            let is_assistant = self.messages[i].role == pipit_provider::Role::Assistant;
            if is_tool_result || (is_assistant && i == pos) {
                self.messages.remove(i);
            }
        }

        before_len - self.messages.len()
    }

    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Estimate total token usage of current history.
    pub fn estimate_token_usage(&self) -> u64 {
        self.messages.iter().map(|m| m.estimated_tokens()).sum()
    }

    /// Check if compression is needed.
    ///
    /// Two triggers:
    /// 1. Estimation-based: history tokens / available budget > compression_threshold
    /// 2. Usage-based: if the last API response reported actual input_tokens close
    ///    to the context window, compress even if the estimate disagrees.
    ///    This catches estimation drift (our char-based heuristic can be 10-20% off).
    pub fn needs_compression(&self) -> bool {
        let usage = self.estimate_token_usage();
        let ratio = usage as f64 / self.budget.available_for_history as f64;
        if ratio > self.settings.compression_threshold {
            return true;
        }
        // Usage-based trigger: if we have actual API token counts, check them.
        if let Some(actual) = self.last_api_input_tokens {
            let reserve = self.settings.max_output_tokens as u64;
            let remaining = self.budget.model_limit.saturating_sub(actual);
            // If actual remaining is less than the output reserve, we must compact.
            if remaining < reserve {
                return true;
            }
        }
        false
    }

    /// Record the actual input token count from the latest API response.
    /// Called after every successful LLM completion.  This drives the
    /// usage-based compaction trigger which is more accurate than the
    /// char-based estimate.
    pub fn record_api_usage(&mut self, input_tokens: u64) {
        self.last_api_input_tokens = Some(input_tokens);
    }

    /// Pre-flight check: estimate total request size and return how much over budget we are.
    /// Returns (estimated_input_tokens, model_limit, over_by) where over_by > 0 means overflow.
    pub fn preflight_check(&self, tools_count: usize, repo_map: Option<&str>) -> (u64, u64, i64) {
        let system_tokens = estimate_text_tokens(&self.system_prompt);
        let repo_map_tokens = repo_map
            .map(|m| estimate_text_tokens(m))
            .unwrap_or(self.budget.repo_map);
        let history_tokens = self.estimate_token_usage();
        let tools_tokens = (tools_count as u64) * 50;
        let output_reserve = self.settings.max_output_tokens as u64;

        let total_input =
            system_tokens + repo_map_tokens + history_tokens + tools_tokens + output_reserve;
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
                        let old_tokens = estimate_text_tokens(content);
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
                        let new_tokens = estimate_text_tokens(content);
                        freed += old_tokens.saturating_sub(new_tokens);
                    }
                }
            }
        }
        freed
    }

    /// Evict stale tool results older than `age_threshold` messages.
    /// Replace with a short placeholder. Returns estimated tokens freed.
    pub fn evict_stale_tool_results(&mut self, age_threshold: usize) -> u64 {
        let msg_count = self.messages.len();
        if msg_count <= age_threshold {
            return 0;
        }
        let cutoff = msg_count - age_threshold;
        let mut freed = 0u64;
        for msg in &mut self.messages[..cutoff] {
            for block in &mut msg.content {
                if let pipit_provider::ContentBlock::ToolResult { content, .. } = block {
                    if content.len() > 200 {
                        let old_tokens = estimate_text_tokens(content);
                        *content = "[Tool result cleared — re-read file if needed]".to_string();
                        let new_tokens = estimate_text_tokens(content);
                        freed += old_tokens.saturating_sub(new_tokens);
                    }
                }
            }
        }
        freed
    }

    /// Truncate individual large tool results to first N lines + summary.
    /// Returns estimated tokens freed.
    pub fn truncate_large_results(&mut self, max_result_chars: usize) -> u64 {
        let mut freed = 0u64;
        for msg in &mut self.messages {
            for block in &mut msg.content {
                if let pipit_provider::ContentBlock::ToolResult { content, .. } = block {
                    if content.len() > max_result_chars {
                        let old_tokens = estimate_text_tokens(content);
                        let lines: Vec<&str> = content.lines().collect();
                        let keep = lines.len().min(30);
                        *content = format!(
                            "{}\n[...{} more lines truncated...]",
                            lines[..keep].join("\n"),
                            lines.len() - keep
                        );
                        let new_tokens = estimate_text_tokens(content);
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

        // Structured compression prompt that preserves critical context
        // to prevent post-compaction repetition (the turn-25 cascade).
        let summary_request = CompletionRequest {
            system: "Summarize this conversation as structured context for continuation. \
                     You MUST include ALL of the following sections:\n\n\
                     FILES_MODIFIED: Every file path that was created, edited, or deleted. Include the action taken.\n\
                     FAILED_TOOL_CALLS: Any tool calls that failed or returned errors. Include the tool name, file path, and error reason.\n\
                     USER_DECISIONS: Preferences, constraints, or decisions the user explicitly stated.\n\
                     CURRENT_TASK: What the user is currently working on and what remains to be done.\n\
                     APPROACHES_TRIED: Strategies or approaches that were attempted (successful or not).\n\
                     KEY_CONTEXT: Important errors encountered, test results, build outputs, and patterns used.\n\n\
                     Rules:\n\
                     - Be concise but COMPLETE for file paths and failed operations.\n\
                     - Omit raw tool result content and intermediate thinking.\n\
                     - Never omit a file path that was modified.\n\
                     - Never omit a failed tool call — the model needs this to avoid repeating mistakes."
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

        let summary_message = Message::system(format!("[Conversation summary]\n{}", summary_text));

        let new_tokens = summary_message.estimated_tokens();

        self.messages = std::iter::once(summary_message).chain(to_keep).collect();

        Ok(CompressionStats {
            messages_removed: old_count,
            tokens_freed: old_tokens.saturating_sub(new_tokens),
        })
    }

    /// Compress using the tiered CompactionPipeline (6-stage with circuit breakers).
    /// Replaces the monolithic `compress()` with per-stage metrics and session memory sink.
    pub async fn compress_pipeline(
        &mut self,
        provider: Arc<dyn LlmProvider>,
        session_id: &str,
        memory_store: Option<&dyn crate::session_memory_compact::MemoryStore>,
        cancel: CancellationToken,
    ) -> Result<CompressionStats, ContextError> {
        use crate::compaction::CompactionPipeline;

        if self.messages.len() <= self.settings.preserve_recent_messages {
            return Ok(CompressionStats::default());
        }

        let budget_tokens = self.budget.available_for_history;
        let old_count = self.messages.len();
        let old_tokens: u64 = self.messages.iter().map(|m| m.estimated_tokens()).sum();

        let mut pipeline = CompactionPipeline::new(
            self.settings.preserve_recent_messages,
            self.settings.tool_result_max_chars,
            Some(provider),
        );

        let result = pipeline
            .run(&mut self.messages, budget_tokens, cancel)
            .await;

        // Log per-stage metrics
        for stage in &result.stages {
            if stage.skipped {
                tracing::debug!(stage = %stage.name, "Compaction stage skipped (circuit breaker)");
            } else if stage.failed {
                tracing::warn!(stage = %stage.name, "Compaction stage failed");
            } else if stage.tokens_freed > 0 {
                tracing::info!(
                    stage = %stage.name,
                    tokens_freed = stage.tokens_freed,
                    messages_removed = stage.messages_removed,
                    duration_ms = stage.duration_ms,
                    "Compaction stage completed"
                );
            }
        }

        // Sink auto-compact summaries into session memory if a store is provided
        if let Some(store) = memory_store {
            if result.total_tokens_freed > 0 {
                let summary_text = self
                    .messages
                    .first()
                    .and_then(|m| m.content.first())
                    .and_then(|b| match b {
                        pipit_provider::ContentBlock::Text(t)
                            if t.contains("[Auto-compact summary]") =>
                        {
                            Some(t.as_str())
                        }
                        _ => None,
                    });
                if let Some(summary) = summary_text {
                    let entry = crate::session_memory_compact::summary_to_memory(
                        session_id,
                        summary,
                        0,
                        old_count as u32,
                        old_tokens,
                    );
                    if let Err(e) = store.store(entry) {
                        tracing::warn!(
                            "Failed to store compaction summary in session memory: {}",
                            e
                        );
                    }
                }
            }
        }

        Ok(CompressionStats {
            messages_removed: result.total_messages_removed,
            tokens_freed: result.total_tokens_freed,
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

        // Send a FIXED max_tokens independent of context size.
        //
        // Previously this did: min(configured_max, context_window - input_estimate)
        // which shrank the output budget as conversation grew. By turn 100+, the
        // model might only get 2-4K output tokens — making it impossible to write
        // large files via write_file.
        //
        // Fixed-output approach: send the configured max_output_tokens every time.
        // Rely on compaction (triggered by needs_compression()) to keep context
        // within the window, not by squeezing the output budget.  The LLM API
        // will itself return a context-overflow error if we truly exceed the
        // window, which the resilience layer handles.
        let max_output = self.settings.max_output_tokens;

        CompletionRequest {
            system,
            messages: self.messages.clone(),
            tools: tools.to_vec(),
            temperature: Some(0.0),
            max_tokens: Some(max_output),
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

    /// Persist session to disk using atomic write (write → fsync → rename).
    pub fn persist_session(&self) -> Result<(), ContextError> {
        let dir = match &self.session_dir {
            Some(d) => d,
            None => return Ok(()),
        };

        std::fs::create_dir_all(dir)?;
        let session_file = dir.join("session.json");
        let json = serde_json::to_string_pretty(&self.messages)
            .map_err(|e| ContextError::Serialization(e.to_string()))?;
        atomic_write(&session_file, json.as_bytes())?;

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
            let summary_message =
                Message::system(format!("[Transport fallback summary]\n{}", summary));
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
                .map(|call| {
                    format!(
                        "{} {}",
                        call.tool_name,
                        truncate_text(&call.args.to_string(), 100)
                    )
                })
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
        assert!(
            manager.messages()[0]
                .text_content()
                .contains("Transport fallback summary")
        );
        assert!(manager.messages().len() <= 3);
    }
}
