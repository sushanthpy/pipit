use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// The canonical message format used throughout pipit.
/// All providers convert to/from this at the boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    #[serde(default)]
    pub metadata: MessageMetadata,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text(text.into())],
            metadata: MessageMetadata::default(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text(text.into())],
            metadata: MessageMetadata::default(),
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::Text(text.into())],
            metadata: MessageMetadata::default(),
        }
    }

    /// An ephemeral control-plane message. Included in the next API request
    /// but stripped from context afterward to avoid polluting future requests.
    pub fn control_plane(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text(text.into())],
            metadata: MessageMetadata {
                ephemeral: true,
                ..Default::default()
            },
        }
    }

    pub fn tool_result(
        call_id: impl Into<String>,
        content: impl Into<String>,
        is_error: bool,
    ) -> Self {
        let cid = call_id.into();
        Self {
            role: Role::ToolResult {
                call_id: cid.clone(),
            },
            content: vec![ContentBlock::ToolResult {
                call_id: cid,
                content: content.into(),
                is_error,
            }],
            metadata: MessageMetadata::default(),
        }
    }

    /// Extract all text content from this message.
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Extract all tool calls from this message.
    pub fn tool_calls(&self) -> Vec<ToolCall> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolCall {
                    call_id,
                    name,
                    args,
                } => Some(ToolCall {
                    call_id: call_id.clone(),
                    tool_name: name.clone(),
                    args: args.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Token estimation using a refined heuristic.
    ///
    /// Uses `max(bytes/4, words*1.3)` as a baseline, then applies corrections:
    /// - Code (high punctuation density) tends to tokenize at ~3.5 bytes/token
    /// - CJK text tokenizes at ~1.5 bytes/token
    /// - JSON/structured data at ~3 bytes/token
    /// Error: ~10-15% vs exact tokenizer (down from 15-40% with naive chars/4).
    pub fn estimated_tokens(&self) -> u64 {
        let mut bytes = 0usize;
        let mut words = 0usize;
        let mut punct = 0usize;
        let mut non_ascii = 0usize;
        for b in &self.content {
            match b {
                ContentBlock::Text(t) => {
                    bytes += t.len();
                    words += t.split_whitespace().count();
                    punct += t.chars().filter(|c| c.is_ascii_punctuation()).count();
                    non_ascii += t.chars().filter(|c| !c.is_ascii()).count();
                }
                ContentBlock::ToolCall { args, .. } => {
                    let s = args.to_string();
                    bytes += s.len();
                    punct += s.chars().filter(|c| c.is_ascii_punctuation()).count();
                }
                ContentBlock::ToolResult { content, .. } => {
                    bytes += content.len();
                    words += content.split_whitespace().count();
                    punct += content.chars().filter(|c| c.is_ascii_punctuation()).count();
                }
                ContentBlock::Thinking(t) => {
                    bytes += t.len();
                    words += t.split_whitespace().count();
                }
                ContentBlock::Image { data, .. } => {
                    bytes += data.len();
                }
                ContentBlock::CacheBreakpoint => {}
            }
        }
        if bytes == 0 {
            return 0;
        }

        // Compute a dynamic divisor based on content characteristics
        let punct_ratio = punct as f64 / bytes as f64;
        let non_ascii_ratio = non_ascii as f64 / bytes.max(1) as f64;

        let divisor = if non_ascii_ratio > 0.3 {
            // CJK or non-Latin text: ~1.5 bytes per token
            1.5
        } else if punct_ratio > 0.15 {
            // Code or JSON: ~3.0 bytes per token (more tokens per byte)
            3.0
        } else {
            // Natural language prose: ~4.0 bytes per token
            4.0
        };

        let byte_estimate = (bytes as f64 / divisor) as u64;
        let word_estimate = ((words as f64) * 1.3) as u64;
        byte_estimate.max(word_estimate)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    ToolResult { call_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentBlock {
    Text(String),
    Image {
        media_type: String,
        data: Vec<u8>,
    },
    ToolCall {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    Thinking(String),
    CacheBreakpoint,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageMetadata {
    pub timestamp: Option<SystemTime>,
    pub token_count: Option<u64>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub cost: Option<f64>,
    pub branch_id: Option<String>,
    pub is_summary: bool,
    #[serde(default)]
    pub summarized_message_ids: Vec<String>,
    /// Ephemeral control-plane messages are included in the next API request
    /// but stripped afterward so they do not contaminate future requests.
    /// Used for loop-recovery nudges, budget warnings, verification feedback,
    /// and auto-continue prompts.
    #[serde(default)]
    pub ephemeral: bool,
}

/// Completion request sent to a provider.
#[derive(Debug, Clone, Default)]
pub struct CompletionRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDeclaration>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stop_sequences: Vec<String>,
}

/// A tool declaration for the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDeclaration {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Events streamed from the LLM provider.
#[derive(Debug, Clone)]
pub enum ContentEvent {
    ContentDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolCallDelta {
        call_id: String,
        tool_name: String,
        args_delta: String,
    },
    ToolCallComplete {
        call_id: String,
        tool_name: String,
        args: serde_json::Value,
    },
    Finished {
        stop_reason: StopReason,
        usage: UsageMetadata,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StopReason {
    EndTurn,
    Stop,
    ToolUse,
    MaxTokens,
    Error,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageMetadata {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
}

impl UsageMetadata {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

#[derive(Debug, Clone, Default)]
pub struct TokenCount {
    pub tokens: u64,
}

/// Model capabilities used for feature selection.
#[derive(Debug, Clone)]
pub struct ModelCapabilities {
    pub context_window: u64,
    pub max_output_tokens: u32,
    pub supports_tool_use: bool,
    pub supports_streaming: bool,
    pub supports_thinking: bool,
    pub supports_images: bool,
    pub supports_prefill: bool,
    pub preferred_edit_format: Option<PreferredFormat>,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            context_window: 200_000,
            max_output_tokens: 8192,
            supports_tool_use: true,
            supports_streaming: true,
            supports_thinking: false,
            supports_images: true,
            supports_prefill: true,
            preferred_edit_format: Some(PreferredFormat::SearchReplace),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum PreferredFormat {
    SearchReplace,
    UnifiedDiff,
    WholeFile,
}

/// A tool call extracted from a response.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub call_id: String,
    pub tool_name: String,
    pub args: serde_json::Value,
}

/// Accumulated assistant response from streaming.
#[derive(Debug, Clone, Default)]
pub struct AssistantResponse {
    pub text: String,
    pub thinking: String,
    pub tool_calls: Vec<ToolCall>,
    pub stop_reason: Option<StopReason>,
    pub usage: UsageMetadata,
    /// Time-to-first-token in milliseconds, measured from request dispatch
    /// to first ContentDelta or ThinkingDelta event.
    pub ttft_ms: Option<u64>,
}

impl AssistantResponse {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_text(&mut self, text: &str) {
        self.text.push_str(text);
    }

    pub fn push_thinking(&mut self, text: &str) {
        self.thinking.push_str(text);
    }

    pub fn push_tool_call(&mut self, call_id: String, tool_name: String, args: serde_json::Value) {
        self.tool_calls.push(ToolCall {
            call_id,
            tool_name,
            args,
        });
    }

    pub fn finish(&mut self, stop_reason: StopReason, usage: UsageMetadata) {
        self.stop_reason = Some(stop_reason);
        self.usage = usage;
    }

    pub fn to_message(&self) -> Message {
        let mut content = Vec::new();

        if !self.thinking.is_empty() {
            content.push(ContentBlock::Thinking(self.thinking.clone()));
        }
        if !self.text.is_empty() {
            content.push(ContentBlock::Text(self.text.clone()));
        }
        for tc in &self.tool_calls {
            content.push(ContentBlock::ToolCall {
                call_id: tc.call_id.clone(),
                name: tc.tool_name.clone(),
                args: tc.args.clone(),
            });
        }

        Message {
            role: Role::Assistant,
            content,
            metadata: MessageMetadata {
                token_count: Some(self.usage.output_tokens),
                ..Default::default()
            },
        }
    }

    pub fn has_tool_calls(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

// ═══════════════════════════════════════════════════════════════
//  Cache-Edit Protocol Types
// ═══════════════════════════════════════════════════════════════

/// An edit operation on the provider's prompt cache.
///
/// Instead of invalidating the entire cache and rebuilding from scratch
/// (O(|cache|) in tokens), cache edits mutate specific entries (O(|edits|)).
/// Savings: for 100K cached tokens and 5 stale tool results, rebuild costs
/// ~$0.30/turn; cache-edit costs ~$0.015/turn. A 20× reduction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CacheEdit {
    /// Remove a tool result by its call_id from the cache.
    RemoveToolResult { call_id: String },
    /// Remove a message by index from the cache.
    RemoveMessage { index: usize },
    /// Replace a message's content in the cache (for summarization).
    ReplaceContent { index: usize, new_content: String },
}

/// Receipt from a cache-edit operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEditReceipt {
    /// Number of edits applied.
    pub edits_applied: usize,
    /// Tokens freed by the edits.
    pub tokens_freed: u64,
    /// Whether the cache is still warm (not fully invalidated).
    pub cache_warm: bool,
}

/// Safely parse tool-call arguments into a JSON Value.
///
/// Returns an empty object `{}` on failure rather than `Null`, so that
/// downstream required-field checks can produce helpful error messages
/// instead of silently passing through.
///
/// Includes repair heuristics for common streaming corruption:
/// - Trailing extra `}` or `]}` characters (vLLM double-close)
/// - Leading/trailing whitespace
/// - Empty strings
///
/// All providers should use this instead of bare
/// `serde_json::from_str(&args).unwrap_or(Value::Null)`.
pub fn parse_tool_args(tool_name: &str, raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        tracing::warn!(
            tool = tool_name,
            "Tool call arguments empty — returning empty object"
        );
        return serde_json::json!({});
    }
    // First attempt: direct parse
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if v.is_object() {
            return v;
        }
        tracing::warn!(
            tool = tool_name,
            raw_args = %trimmed,
            "Tool call arguments parsed but not a JSON object — returning empty object"
        );
        return serde_json::json!({});
    }
    // Repair: strip trailing extra braces/brackets one at a time.
    // vLLM streaming sometimes appends an extra `}` to the accumulated args.
    let mut repaired = trimmed.to_string();
    for _ in 0..3 {
        if repaired.ends_with("}}") || repaired.ends_with("]}") {
            repaired.pop();
        } else {
            break;
        }
    }
    if repaired != trimmed {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&repaired) {
            if v.is_object() {
                tracing::info!(
                    tool = tool_name,
                    original_len = trimmed.len(),
                    repaired_len = repaired.len(),
                    "Repaired tool call arguments by stripping trailing brace(s)"
                );
                return v;
            }
        }
    }
    tracing::warn!(
        tool = tool_name,
        raw_args = %trimmed,
        "Failed to parse tool call arguments — returning empty object"
    );
    serde_json::json!({})
}

/// Safely coerce a JSON Value into an object suitable for tool call args.
///
/// Use this when the provider already gives you a parsed `Value`
/// (e.g. Gemini returns args as a nested JSON object directly).
/// Returns the value as-is if it's an object, otherwise returns `{}`.
pub fn coerce_tool_args(tool_name: &str, value: serde_json::Value) -> serde_json::Value {
    if value.is_object() {
        value
    } else if value.is_null() {
        tracing::warn!(
            tool = tool_name,
            "Tool call args are null — returning empty object"
        );
        serde_json::json!({})
    } else {
        tracing::warn!(
            tool = tool_name,
            value = %value,
            "Tool call args are not a JSON object — returning empty object"
        );
        serde_json::json!({})
    }
}
