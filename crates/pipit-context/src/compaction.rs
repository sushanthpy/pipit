//! Tiered Context Compaction Pipeline
//!
//! A composable, multi-stage compaction pipeline where each stage is a
//! `CompactionPass` trait object, executed in sequence. The pipeline is a
//! monotone function chain on token count: each stage satisfies f_i(tokens) ≤ tokens.
//!
//! Stages (in order):
//! 1. ToolResultBudget — Per-message truncation of oversized tool outputs
//! 2. SnipCompact — Boundary-based truncation of completed conversation segments
//! 3. MicroCompact — Remove stale tool results by tool_use_id
//! 4. ContextCollapse — Read-time projection that commits staged collapses
//! 5. AutoCompact — Full LLM-based summarization as last resort

use crate::ContextError;
use pipit_provider::{CompletionRequest, ContentBlock, ContentEvent, LlmProvider, Message};
use tokio_util::sync::CancellationToken;
use crate::budget::estimate_text_tokens;

// ─── CompactionPass Trait ───────────────────────────────────────────────

/// A single stage in the compaction pipeline.
/// Each pass may modify messages in place, returning stats about work done.
#[async_trait::async_trait]
pub trait CompactionPass: Send + Sync {
    /// Human-readable name for telemetry.
    fn name(&self) -> &str;

    /// Execute this compaction pass on the message history.
    /// Returns stats and whether the pass modified anything.
    async fn compact(
        &mut self,
        messages: &mut Vec<Message>,
        budget_tokens: u64,
        cancel: CancellationToken,
    ) -> Result<PassResult, ContextError>;
}

/// Result of a single compaction pass.
#[derive(Debug, Clone, Default)]
pub struct PassResult {
    pub messages_removed: usize,
    pub tokens_freed: u64,
    pub modified: bool,
}

// ─── Pipeline ───────────────────────────────────────────────────────────

/// Circuit breaker state per stage.
#[derive(Debug, Clone, Default)]
struct CircuitBreaker {
    consecutive_failures: u32,
    turns_to_skip: u32,
}

impl CircuitBreaker {
    const MAX_SKIP: u32 = 32;

    fn should_skip(&self) -> bool {
        self.turns_to_skip > 0
    }

    fn tick(&mut self) {
        self.turns_to_skip = self.turns_to_skip.saturating_sub(1);
    }

    fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.turns_to_skip = 0;
    }

    fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        // Exponential backoff: skip for 2^n turns (bounded by MAX_SKIP)
        self.turns_to_skip = (1u32 << self.consecutive_failures.min(5)).min(Self::MAX_SKIP);
    }
}

/// Per-stage telemetry snapshot.
#[derive(Debug, Clone, Default)]
pub struct StageMetrics {
    pub name: String,
    pub tokens_freed: u64,
    pub messages_removed: usize,
    pub skipped: bool,
    pub failed: bool,
    pub duration_ms: u64,
}

/// Result of the full pipeline run.
#[derive(Debug, Clone, Default)]
pub struct PipelineResult {
    pub total_tokens_freed: u64,
    pub total_messages_removed: usize,
    pub stages: Vec<StageMetrics>,
}

/// The tiered compaction pipeline.
pub struct CompactionPipeline {
    passes: Vec<Box<dyn CompactionPass>>,
    breakers: Vec<CircuitBreaker>,
    pending_collapses: Vec<StagedCollapse>,
    /// Timestamp of the last periodic micro-compaction run.
    last_periodic_compact: std::time::Instant,
    /// Interval between periodic micro-compaction runs (seconds).
    periodic_interval_secs: u64,
    /// Number of turns since last periodic compact.
    turns_since_compact: u32,
    /// Turns between periodic compactions (trigger if exceeded).
    turns_between_compact: u32,
}

impl CompactionPipeline {
    /// Create a new pipeline with the default 5-stage configuration.
    pub fn new(
        preserve_recent: usize,
        tool_result_budget_chars: usize,
        provider: Option<std::sync::Arc<dyn LlmProvider>>,
    ) -> Self {
        let mut passes: Vec<Box<dyn CompactionPass>> = vec![
            Box::new(ToolResultBudgetPass {
                max_chars: tool_result_budget_chars,
                exempt_tools: vec!["grep".to_string(), "glob".to_string()],
            }),
            Box::new(SnipCompactPass {
                preserve_recent,
            }),
            Box::new(MicroCompactPass {
                stale_turn_threshold: 6,
            }),
            Box::new(ContextCollapsePass {
                staged_collapses: Vec::new(),
            }),
        ];

        if let Some(prov) = provider {
            passes.push(Box::new(AutoCompactPass {
                provider: prov,
                preserve_recent,
                consecutive_failures: 0,
            }));
        }

        let breaker_count = passes.len();
        Self {
            passes,
            breakers: vec![CircuitBreaker::default(); breaker_count],
            pending_collapses: Vec::new(),
            last_periodic_compact: std::time::Instant::now(),
            periodic_interval_secs: 120, // 2 minutes
            turns_since_compact: 0,
            turns_between_compact: 8,
        }
    }

    /// Run all stages in sequence. Short-circuits if budget is met.
    pub async fn run(
        &mut self,
        messages: &mut Vec<Message>,
        budget_tokens: u64,
        cancel: CancellationToken,
    ) -> PipelineResult {
        let mut result = PipelineResult::default();

        // Drain pending collapses into the ContextCollapsePass
        let pending = std::mem::take(&mut self.pending_collapses);
        if !pending.is_empty() {
            for pass in &mut self.passes {
                if pass.name() == "context_collapse" {
                    // Safe: we know the concrete type by name
                    // Thread collapses through the pass interface
                    break;
                }
            }
            // Directly inject into the collapse pass (index 3)
            if self.passes.len() > 3 && self.passes[3].name() == "context_collapse" {
                // Re-create collapse pass with staged data
                self.passes[3] = Box::new(ContextCollapsePass {
                    staged_collapses: pending,
                });
            }
        }

        // Tick all circuit breakers
        for breaker in &mut self.breakers {
            breaker.tick();
        }

        for (i, pass) in self.passes.iter_mut().enumerate() {
            if cancel.is_cancelled() {
                break;
            }

            let mut stage_metrics = StageMetrics {
                name: pass.name().to_string(),
                ..Default::default()
            };

            // Check circuit breaker
            if self.breakers[i].should_skip() {
                stage_metrics.skipped = true;
                result.stages.push(stage_metrics);
                continue;
            }

            // Check if we're already under budget
            let current_tokens: u64 = messages.iter().map(|m| m.estimated_tokens()).sum();
            if current_tokens <= budget_tokens {
                stage_metrics.skipped = true;
                result.stages.push(stage_metrics);
                continue;
            }

            let start = std::time::Instant::now();
            match pass.compact(messages, budget_tokens, cancel.clone()).await {
                Ok(pass_result) => {
                    stage_metrics.tokens_freed = pass_result.tokens_freed;
                    stage_metrics.messages_removed = pass_result.messages_removed;
                    stage_metrics.duration_ms = start.elapsed().as_millis() as u64;
                    result.total_tokens_freed += pass_result.tokens_freed;
                    result.total_messages_removed += pass_result.messages_removed;
                    self.breakers[i].record_success();
                }
                Err(e) => {
                    tracing::warn!("Compaction pass '{}' failed: {}", pass.name(), e);
                    stage_metrics.failed = true;
                    stage_metrics.duration_ms = start.elapsed().as_millis() as u64;
                    self.breakers[i].record_failure();
                }
            }

            result.stages.push(stage_metrics);
        }

        result
    }

    /// Stage a collapse: record a summary to be committed on the next pipeline run.
    pub fn stage_collapse(&mut self, summary: String, message_range: std::ops::Range<usize>) {
        self.pending_collapses.push(StagedCollapse {
            summary,
            message_range,
        });
    }

    /// Run periodic micro-compaction based on time and turn count.
    ///
    /// Call this every turn. It will only actually compact if either:
    /// 1. More than `periodic_interval_secs` have elapsed since last compact, or
    /// 2. More than `turns_between_compact` turns have passed.
    ///
    /// Unlike `run()`, this does NOT short-circuit on budget — it always runs
    /// micro-compact to keep stale data from accumulating.
    pub async fn periodic_compact(
        &mut self,
        messages: &mut Vec<Message>,
        cancel: CancellationToken,
    ) -> PassResult {
        self.turns_since_compact += 1;

        let elapsed = self.last_periodic_compact.elapsed().as_secs();
        let time_trigger = elapsed >= self.periodic_interval_secs;
        let turn_trigger = self.turns_since_compact >= self.turns_between_compact;

        if !time_trigger && !turn_trigger {
            return PassResult::default();
        }

        // Run only the MicroCompactPass (index 2)
        let result = if self.passes.len() > 2 && self.passes[2].name() == "micro_compact" {
            match self.passes[2].compact(messages, u64::MAX, cancel).await {
                Ok(r) => r,
                Err(_) => PassResult::default(),
            }
        } else {
            PassResult::default()
        };

        if result.modified {
            self.last_periodic_compact = std::time::Instant::now();
            self.turns_since_compact = 0;
            tracing::info!(
                tokens_freed = result.tokens_freed,
                trigger = if time_trigger { "time" } else { "turns" },
                "Periodic micro-compaction completed"
            );
        }

        result
    }
}

// ─── Stage 1: Tool Result Budget ────────────────────────────────────────

/// Per-message truncation of oversized tool outputs.
/// Tools in `exempt_tools` are never truncated (e.g., grep needs full results).
pub struct ToolResultBudgetPass {
    pub max_chars: usize,
    pub exempt_tools: Vec<String>,
}

#[async_trait::async_trait]
impl CompactionPass for ToolResultBudgetPass {
    fn name(&self) -> &str {
        "tool_result_budget"
    }

    async fn compact(
        &mut self,
        messages: &mut Vec<Message>,
        _budget_tokens: u64,
        _cancel: CancellationToken,
    ) -> Result<PassResult, ContextError> {
        let mut freed = 0u64;
        let mut modified = false;

        for msg in messages.iter_mut() {
            // Check if this message contains a tool call for an exempt tool
            let tool_name = msg.content.iter().find_map(|block| {
                if let ContentBlock::ToolCall { name, .. } = block {
                    Some(name.clone())
                } else {
                    None
                }
            });

            if let Some(ref name) = tool_name {
                if self.exempt_tools.iter().any(|e| e == name) {
                    continue;
                }
            }

            for block in &mut msg.content {
                if let ContentBlock::ToolResult { content, .. } = block {
                    if content.len() > self.max_chars {
                        let old_tokens = estimate_text_tokens(content);
                        let lines: Vec<&str> = content.lines().collect();
                        let total = lines.len();
                        let head = 30.min(total);
                        let tail = 30.min(total.saturating_sub(head));
                        if total > head + tail {
                            *content = format!(
                                "{}\n\n[...{} of {} lines truncated by tool_result_budget...]\n\n{}",
                                lines[..head].join("\n"),
                                total - head - tail,
                                total,
                                lines[total - tail..].join("\n"),
                            );
                        } else {
                            *content = content.chars().take(self.max_chars).collect();
                        }
                        let new_tokens = estimate_text_tokens(content);
                        freed += old_tokens.saturating_sub(new_tokens);
                        modified = true;
                    }
                }
            }
        }

        Ok(PassResult {
            messages_removed: 0,
            tokens_freed: freed,
            modified,
        })
    }
}

// ─── Stage 2: Snip Compact ─────────────────────────────────────────────

/// Boundary-based truncation that removes entire completed conversation segments.
/// A "segment" is a user-message → assistant-response → tool-results cycle.
/// Completed segments (not in the last `preserve_recent` messages) are snipped.
pub struct SnipCompactPass {
    pub preserve_recent: usize,
}

#[async_trait::async_trait]
impl CompactionPass for SnipCompactPass {
    fn name(&self) -> &str {
        "snip_compact"
    }

    async fn compact(
        &mut self,
        messages: &mut Vec<Message>,
        budget_tokens: u64,
        _cancel: CancellationToken,
    ) -> Result<PassResult, ContextError> {
        if messages.len() <= self.preserve_recent {
            return Ok(PassResult::default());
        }

        let current_tokens: u64 = messages.iter().map(|m| m.estimated_tokens()).sum();
        if current_tokens <= budget_tokens {
            return Ok(PassResult::default());
        }

        // Find segment boundaries in the compactable region
        let compactable_end = messages.len().saturating_sub(self.preserve_recent);
        let mut segments = Vec::new();
        let mut seg_start = 0;

        for i in 0..compactable_end {
            // A segment ends when the next message is a user message (new turn)
            let is_segment_end = i + 1 >= compactable_end
                || matches!(messages.get(i + 1).map(|m| &m.role), Some(pipit_provider::Role::User));

            if is_segment_end {
                let seg_tokens: u64 = messages[seg_start..=i]
                    .iter()
                    .map(|m| m.estimated_tokens())
                    .sum();
                segments.push((seg_start, i + 1, seg_tokens));
                seg_start = i + 1;
            }
        }

        // Remove oldest segments until under budget
        let mut tokens_freed = 0u64;
        let mut messages_removed = 0usize;
        let mut remove_up_to = 0;

        for (start, end, seg_tokens) in &segments {
            if current_tokens - tokens_freed <= budget_tokens {
                break;
            }
            tokens_freed += seg_tokens;
            messages_removed += end - start;
            remove_up_to = *end;
        }

        if remove_up_to > 0 {
            // Replace removed segments with a boundary marker
            let marker = Message::system(format!(
                "[Context snipped: {} earlier messages ({} tokens) removed to free space]",
                messages_removed, tokens_freed
            ));
            messages.drain(..remove_up_to);
            messages.insert(0, marker);

            Ok(PassResult {
                messages_removed,
                tokens_freed,
                modified: true,
            })
        } else {
            Ok(PassResult::default())
        }
    }
}

// ─── Stage 3: Micro Compact ────────────────────────────────────────────

/// Remove stale tool results by tool_use_id. Tool results from turns older
/// than `stale_turn_threshold` are replaced with a compact summary.
pub struct MicroCompactPass {
    pub stale_turn_threshold: usize,
}

#[async_trait::async_trait]
impl CompactionPass for MicroCompactPass {
    fn name(&self) -> &str {
        "micro_compact"
    }

    async fn compact(
        &mut self,
        messages: &mut Vec<Message>,
        _budget_tokens: u64,
        _cancel: CancellationToken,
    ) -> Result<PassResult, ContextError> {
        let total = messages.len();
        if total <= self.stale_turn_threshold {
            return Ok(PassResult::default());
        }

        let stale_boundary = total.saturating_sub(self.stale_turn_threshold);
        let mut freed = 0u64;
        let mut modified = false;

        // Collect tool_call_ids from the stale region to identify their results
        let mut stale_call_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for msg in &messages[..stale_boundary] {
            for block in &msg.content {
                if let ContentBlock::ToolCall { call_id, .. } = block {
                    stale_call_ids.insert(call_id.clone());
                }
            }
        }

        // Replace stale tool results with compact versions
        for msg in &mut messages[..stale_boundary] {
            for block in &mut msg.content {
                if let ContentBlock::ToolResult {
                    call_id, content, ..
                } = block
                {
                    if stale_call_ids.contains(call_id.as_str()) && content.len() > 200 {
                        let old_tokens = estimate_text_tokens(content);
                        // Keep first 2 lines as summary
                        let summary: String = content.lines().take(2).collect::<Vec<_>>().join("\n");
                        *content = format!("[stale result] {}", summary);
                        let new_tokens = estimate_text_tokens(content);
                        freed += old_tokens.saturating_sub(new_tokens);
                        modified = true;
                    }
                }
            }
        }

        Ok(PassResult {
            messages_removed: 0,
            tokens_freed: freed,
            modified,
        })
    }
}

// ─── Stage 4: Context Collapse ──────────────────────────────────────────

/// A staged collapse: summary replaces a range of messages.
#[derive(Debug, Clone)]
pub struct StagedCollapse {
    pub summary: String,
    pub message_range: std::ops::Range<usize>,
}

/// Read-time projection system that commits staged collapses.
/// Collapses live in a separate store, enabling rollback.
pub struct ContextCollapsePass {
    staged_collapses: Vec<StagedCollapse>,
}

#[async_trait::async_trait]
impl CompactionPass for ContextCollapsePass {
    fn name(&self) -> &str {
        "context_collapse"
    }

    async fn compact(
        &mut self,
        messages: &mut Vec<Message>,
        _budget_tokens: u64,
        _cancel: CancellationToken,
    ) -> Result<PassResult, ContextError> {
        if self.staged_collapses.is_empty() {
            return Ok(PassResult::default());
        }

        // Sort collapses by start index (descending) to apply from back to front
        // This avoids index invalidation when removing ranges
        self.staged_collapses
            .sort_by(|a, b| b.message_range.start.cmp(&a.message_range.start));

        let mut total_freed = 0u64;
        let mut total_removed = 0usize;

        for collapse in self.staged_collapses.drain(..) {
            let range = collapse.message_range.clone();
            if range.end > messages.len() || range.start >= range.end {
                continue;
            }

            let old_tokens: u64 = messages[range.clone()]
                .iter()
                .map(|m| m.estimated_tokens())
                .sum();
            let removed_count = range.end - range.start;

            let summary_msg = Message::system(format!(
                "[Collapsed {} messages]\n{}",
                removed_count, collapse.summary
            ));
            let new_tokens = summary_msg.estimated_tokens();

            messages.drain(range.clone());
            messages.insert(range.start.min(messages.len()), summary_msg);

            total_freed += old_tokens.saturating_sub(new_tokens);
            total_removed += removed_count;
        }

        Ok(PassResult {
            messages_removed: total_removed,
            tokens_freed: total_freed,
            modified: total_removed > 0,
        })
    }
}

// ─── Stage 5: Auto Compact ─────────────────────────────────────────────

/// Full LLM-based summarization as last resort.
/// Includes a circuit breaker via `consecutive_failures`.
pub struct AutoCompactPass {
    provider: std::sync::Arc<dyn LlmProvider>,
    preserve_recent: usize,
    consecutive_failures: u32,
}

#[async_trait::async_trait]
impl CompactionPass for AutoCompactPass {
    fn name(&self) -> &str {
        "auto_compact"
    }

    async fn compact(
        &mut self,
        messages: &mut Vec<Message>,
        _budget_tokens: u64,
        cancel: CancellationToken,
    ) -> Result<PassResult, ContextError> {
        if messages.len() <= self.preserve_recent {
            return Ok(PassResult::default());
        }

        let split_point = messages.len() - self.preserve_recent;
        let to_summarize = &messages[..split_point];
        let to_keep = messages[split_point..].to_vec();

        let summary_request = CompletionRequest {
            system: "Summarize this conversation as structured context. Output:\n\
                     FILES_MODIFIED: List all file paths created/edited.\n\
                     DECISIONS: Key technical decisions.\n\
                     CURRENT_TASK: What the user is working on.\n\
                     KEY_CONTEXT: Other important context (errors, patterns).\n\
                     Be concise. Omit tool result details."
                .to_string(),
            messages: to_summarize.to_vec(),
            tools: vec![],
            max_tokens: Some(2048),
            temperature: Some(0.0),
            stop_sequences: vec![],
        };

        let mut stream = self
            .provider
            .complete(summary_request, cancel)
            .await
            .map_err(|e| {
                self.consecutive_failures += 1;
                ContextError::Other(format!("Auto-compact LLM call failed: {}", e))
            })?;

        use futures::StreamExt;
        let mut summary_text = String::new();
        while let Some(event) = stream.next().await {
            if let Ok(ContentEvent::ContentDelta { text }) = event {
                summary_text.push_str(&text);
            }
        }

        if summary_text.is_empty() {
            self.consecutive_failures += 1;
            return Err(ContextError::Other(
                "Auto-compact produced empty summary".to_string(),
            ));
        }

        self.consecutive_failures = 0;
        let old_count = to_summarize.len();
        let old_tokens: u64 = to_summarize.iter().map(|m| m.estimated_tokens()).sum();

        let summary_message = Message::system(format!(
            "[Auto-compact summary]\n{}",
            summary_text
        ));
        let new_tokens = summary_message.estimated_tokens();

        *messages = std::iter::once(summary_message).chain(to_keep).collect();

        Ok(PassResult {
            messages_removed: old_count,
            tokens_freed: old_tokens.saturating_sub(new_tokens),
            modified: true,
        })
    }
}
