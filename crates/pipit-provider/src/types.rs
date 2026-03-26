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

    pub fn tool_result(call_id: impl Into<String>, content: impl Into<String>, is_error: bool) -> Self {
        let cid = call_id.into();
        Self {
            role: Role::ToolResult { call_id: cid.clone() },
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

    /// Fix #5: Better token estimation — max(bytes/4, words*1.3)
    pub fn estimated_tokens(&self) -> u64 {
        let mut bytes = 0usize;
        let mut words = 0usize;
        for b in &self.content {
            match b {
                ContentBlock::Text(t) => { bytes += t.len(); words += t.split_whitespace().count(); }
                ContentBlock::ToolCall { args, .. } => { bytes += args.to_string().len(); }
                ContentBlock::ToolResult { content, .. } => { bytes += content.len(); words += content.split_whitespace().count(); }
                ContentBlock::Thinking(t) => { bytes += t.len(); words += t.split_whitespace().count(); }
                ContentBlock::Image { data, .. } => { bytes += data.len(); }
                ContentBlock::CacheBreakpoint => {}
            }
        }
        ((bytes as u64) / 4).max(((words as f64) * 1.3) as u64)
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
}

/// Completion request sent to a provider.
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDeclaration>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stop_sequences: Vec<String>,
}

impl Default for CompletionRequest {
    fn default() -> Self {
        Self {
            system: String::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            stop_sequences: Vec::new(),
        }
    }
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
    ContentDelta { text: String },
    ThinkingDelta { text: String },
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

    pub fn push_tool_call(
        &mut self,
        call_id: String,
        tool_name: String,
        args: serde_json::Value,
    ) {
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
