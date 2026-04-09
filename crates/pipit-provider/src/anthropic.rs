use crate::{
    CompletionRequest, ContentEvent, LlmProvider, ModelCapabilities, PreferredFormat,
    ProviderError, StopReason, TokenCount, UsageMetadata,
};
use async_trait::async_trait;
use bytes::BytesMut;
use futures::stream::Stream;
use pin_project_lite::pin_project;
use reqwest::Client;
use std::collections::{HashMap, VecDeque};
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

pub struct AnthropicProvider {
    client: Client,
    provider_id: String,
    model: String,
    api_key: String,
    base_url: String,
    capabilities: ModelCapabilities,
}

impl AnthropicProvider {
    pub fn new(
        model: String,
        api_key: String,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        Self::with_id("anthropic".to_string(), model, api_key, base_url)
    }

    pub fn with_id(
        provider_id: String,
        model: String,
        api_key: String,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| ProviderError::Network(e.to_string()))?;
        let capabilities = Self::capabilities_for_model(&model);
        Ok(Self {
            client,
            provider_id,
            model,
            api_key,
            capabilities,
            base_url: base_url.unwrap_or_else(|| "https://api.anthropic.com".to_string()),
        })
    }

    fn capabilities_for_model(model: &str) -> ModelCapabilities {
        let max_output_tokens = if model.contains("opus") {
            32_000
        } else if model.contains("sonnet") {
            // Claude Sonnet 4/4.5 supports up to 64K output tokens
            16_384
        } else {
            // Haiku and unknown models
            8192
        };
        ModelCapabilities {
            context_window: 200_000,
            max_output_tokens,
            supports_tool_use: true,
            supports_streaming: true,
            supports_thinking: model.contains("opus") || model.contains("sonnet"),
            supports_images: true,
            supports_prefill: true,
            preferred_edit_format: Some(PreferredFormat::SearchReplace),
        }
    }

    fn build_request_body(&self, request: &CompletionRequest) -> serde_json::Value {
        let messages = self.merge_messages_for_alternation(&request.messages);

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": request.max_tokens.unwrap_or(self.capabilities.max_output_tokens),
            "stream": true,
            "messages": messages,
        });
        if !request.system.is_empty() {
            // Use structured system prompt with cache_control for Anthropic prompt caching.
            // This can reduce input costs by ~90% for repeated system prompts.
            body["system"] = serde_json::json!([{
                "type": "text",
                "text": request.system,
                "cache_control": {"type": "ephemeral"}
            }]);
        }
        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if !request.tools.is_empty() {
            let mut tools: Vec<serde_json::Value> = request.tools.iter().map(|t| serde_json::json!({
                "name": t.name, "description": t.description, "input_schema": t.input_schema,
            })).collect();
            // Mark the last tool with cache_control for prompt caching
            if let Some(last_tool) = tools.last_mut() {
                last_tool["cache_control"] = serde_json::json!({"type": "ephemeral"});
            }
            body["tools"] = serde_json::json!(tools);
        }
        body
    }

    /// Enforce Anthropic's user/assistant role alternation requirement.
    ///
    /// The API rejects consecutive messages with the same role. This method
    /// merges adjacent same-role messages into a single message by concatenating
    /// their content blocks. System messages are skipped (they go in the
    /// top-level `system` field). ToolResult roles are mapped to "user".
    fn merge_messages_for_alternation(
        &self,
        raw_messages: &[crate::Message],
    ) -> Vec<serde_json::Value> {
        let mut merged: Vec<serde_json::Value> = Vec::new();
        let mut prev_role: Option<&str> = None;

        for msg in raw_messages {
            let role = match &msg.role {
                crate::Role::User => "user",
                crate::Role::Assistant => "assistant",
                crate::Role::ToolResult { .. } => "user",
                crate::Role::System => continue, // system messages handled separately
            };

            let content = self.convert_content_blocks(&msg.content);

            if prev_role == Some(role) {
                // Merge into the last message's content array
                if let Some(last) = merged.last_mut() {
                    if let Some(existing) = last.get_mut("content").and_then(|c| c.as_array_mut()) {
                        if let Some(new_blocks) = content.as_array() {
                            existing.extend(new_blocks.iter().cloned());
                        }
                        continue;
                    }
                }
            }

            prev_role = Some(if role == "user" { "user" } else { "assistant" });
            merged.push(serde_json::json!({"role": role, "content": content}));
        }

        merged
    }

    fn convert_content_blocks(&self, blocks: &[crate::ContentBlock]) -> serde_json::Value {
        let mut result = Vec::new();
        for block in blocks {
            match block {
                crate::ContentBlock::Text(text) => {
                    result.push(serde_json::json!({"type": "text", "text": text}));
                }
                crate::ContentBlock::ToolCall {
                    call_id,
                    name,
                    args,
                } => {
                    result.push(serde_json::json!({"type":"tool_use","id":call_id,"name":name,"input":args}));
                }
                crate::ContentBlock::ToolResult {
                    call_id,
                    content,
                    is_error,
                } => {
                    result.push(serde_json::json!({"type":"tool_result","tool_use_id":call_id,"content":content,"is_error":is_error}));
                }
                crate::ContentBlock::Image { media_type, data } => {
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
                    result.push(serde_json::json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":encoded}}));
                }
                crate::ContentBlock::Thinking(text) => {
                    result.push(serde_json::json!({"type":"thinking","thinking":text}));
                }
                crate::ContentBlock::CacheBreakpoint => {}
            }
        }
        if result.is_empty() {
            serde_json::json!([{"type":"text","text":""}])
        } else {
            serde_json::json!(result)
        }
    }

    /// Heuristic token estimation: content-aware byte/word analysis.
    /// Used as fallback when the count_tokens API is unavailable.
    fn estimate_tokens_heuristic(messages: &[crate::Message]) -> Result<TokenCount, ProviderError> {
        let (mut bytes, mut words, mut punct) = (0usize, 0usize, 0usize);
        for m in messages {
            for b in &m.content {
                match b {
                    crate::ContentBlock::Text(t) => {
                        bytes += t.len();
                        words += t.split_whitespace().count();
                        punct += t.bytes().filter(|b| b.is_ascii_punctuation()).count();
                    }
                    crate::ContentBlock::ToolCall { args, .. } => {
                        let s = args.to_string();
                        bytes += s.len();
                        punct += s.bytes().filter(|b| b.is_ascii_punctuation()).count();
                    }
                    crate::ContentBlock::ToolResult { content, .. } => {
                        bytes += content.len();
                        words += content.split_whitespace().count();
                        punct += content.bytes().filter(|b| b.is_ascii_punctuation()).count();
                    }
                    crate::ContentBlock::Thinking(t) => {
                        bytes += t.len();
                        words += t.split_whitespace().count();
                    }
                    _ => {}
                }
            }
        }
        if bytes == 0 {
            return Ok(TokenCount { tokens: 0 });
        }
        // Anthropic's tokenizer averages ~3.5 chars/token for English prose,
        // ~3.0 for code (higher punctuation density), ~1.5 for CJK.
        let punct_ratio = punct as f64 / bytes as f64;
        let divisor = if punct_ratio > 0.15 { 3.0 } else { 3.5 };
        let byte_estimate = (bytes as f64 / divisor).ceil() as u64;
        let word_estimate = ((words as f64) * 1.3) as u64;
        Ok(TokenCount {
            tokens: byte_estimate.max(word_estimate),
        })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.provider_id
    }

    async fn complete(
        &self,
        request: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ContentEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        let body = self.build_request_body(&request);

        // Fix #7: Integrate retry policy
        let retry_policy = pipit_config::RetryPolicy::default();
        let response = crate::retry::with_retry(&retry_policy, || {
            let client = self.client.clone();
            let url = format!("{}/v1/messages", self.base_url);
            let api_key = self.api_key.clone();
            let body = body.clone();
            async move {
                let resp = client
                    .post(&url)
                    .header("x-api-key", &api_key)
                    .header("anthropic-version", "2023-06-01")
                    .header("content-type", "application/json")
                    .header("accept", "text/event-stream")
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| ProviderError::Network(e.to_string()))?;
                let status = resp.status();
                if !status.is_success() {
                    let body_text = resp.text().await.unwrap_or_default();
                    return match status.as_u16() {
                        401 => Err(ProviderError::AuthFailed { message: body_text }),
                        413 => Err(ProviderError::RequestTooLarge { message: body_text }),
                        429 => Err(ProviderError::RateLimited {
                            retry_after_ms: None,
                        }),
                        _ => Err(ProviderError::Other(format!(
                            "HTTP {}: {}",
                            status, body_text
                        ))),
                    };
                }
                Ok(resp)
            }
        })
        .await?;

        let event_stream = AnthropicEventStream {
            byte_stream: Box::pin(response.bytes_stream()),
            parser: AnthropicStreamParser::new(),
            buffer: BytesMut::new(),
            pending_events: VecDeque::new(),
            cancel,
            finished: false,
        };
        Ok(Box::pin(event_stream))
    }

    // Exact token counting via Anthropic's /v1/messages/count_tokens API.
    // Falls back to heuristic estimation if the API call fails (network error,
    // unsupported endpoint on compatible providers, etc.).
    async fn count_tokens(&self, messages: &[crate::Message]) -> Result<TokenCount, ProviderError> {
        // Build the same message format used for completions
        let merged = self.merge_messages_for_alternation(messages);

        // Extract system text from any System-role messages
        let system_text: String = messages
            .iter()
            .filter(|m| matches!(m.role, crate::Role::System))
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                crate::ContentBlock::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": merged,
        });
        if !system_text.is_empty() {
            body["system"] = serde_json::json!(system_text);
        }

        let url = format!("{}/v1/messages/count_tokens", self.base_url);
        match self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                match resp.json::<serde_json::Value>().await {
                    Ok(json) => {
                        if let Some(tokens) = json.get("input_tokens").and_then(|v| v.as_u64()) {
                            return Ok(TokenCount { tokens });
                        }
                        tracing::debug!(
                            "count_tokens: unexpected response shape, falling back to heuristic"
                        );
                    }
                    Err(e) => {
                        tracing::debug!(
                            "count_tokens: failed to parse response: {e}, falling back to heuristic"
                        );
                    }
                }
            }
            Ok(resp) => {
                tracing::debug!(
                    "count_tokens: API returned {}, falling back to heuristic",
                    resp.status()
                );
            }
            Err(e) => {
                tracing::debug!("count_tokens: network error: {e}, falling back to heuristic");
            }
        }

        // Heuristic fallback: content-aware estimation
        Self::estimate_tokens_heuristic(messages)
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }
}

struct AnthropicStreamParser {
    tool_arg_buffers: HashMap<String, (String, String)>,
    current_block_type: Option<String>,
    current_block_id: Option<String>,
    usage: UsageMetadata,
}

impl AnthropicStreamParser {
    fn new() -> Self {
        Self {
            tool_arg_buffers: HashMap::new(),
            current_block_type: None,
            current_block_id: None,
            usage: UsageMetadata::default(),
        }
    }

    // Fix #2: Returns Vec — all events from one SSE frame
    fn process_event(&mut self, event_type: &str, data: &str) -> Vec<ContentEvent> {
        let json: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        match event_type {
            "message_start" => {
                if let Some(u) = json.get("message").and_then(|m| m.get("usage")) {
                    self.usage.input_tokens =
                        u.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                }
                vec![]
            }
            "content_block_start" => {
                let block = &json["content_block"];
                match block["type"].as_str().unwrap_or("") {
                    "tool_use" => {
                        let id = block["id"].as_str().unwrap_or("").to_string();
                        let name = block["name"].as_str().unwrap_or("").to_string();
                        self.current_block_type = Some("tool_use".to_string());
                        self.current_block_id = Some(id.clone());
                        self.tool_arg_buffers.insert(id, (name, String::new()));
                    }
                    "thinking" => {
                        self.current_block_type = Some("thinking".to_string());
                    }
                    _ => {
                        self.current_block_type = Some("text".to_string());
                    }
                }
                vec![]
            }
            "content_block_delta" => {
                let delta = &json["delta"];
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => vec![ContentEvent::ContentDelta {
                        text: delta["text"].as_str().unwrap_or("").to_string(),
                    }],
                    "thinking_delta" => vec![ContentEvent::ThinkingDelta {
                        text: delta["thinking"].as_str().unwrap_or("").to_string(),
                    }],
                    "input_json_delta" => {
                        if let Some(id) = &self.current_block_id {
                            if let Some((_, buf)) = self.tool_arg_buffers.get_mut(id) {
                                buf.push_str(delta["partial_json"].as_str().unwrap_or(""));
                            }
                        }
                        vec![]
                    }
                    _ => vec![],
                }
            }
            "content_block_stop" => {
                let r = if self.current_block_type.as_deref() == Some("tool_use") {
                    if let Some(id) = self.current_block_id.take() {
                        if let Some((name, args_json)) = self.tool_arg_buffers.remove(&id) {
                            vec![ContentEvent::ToolCallComplete {
                                call_id: id,
                                tool_name: name,
                                args: serde_json::from_str(&args_json)
                                    .unwrap_or(serde_json::Value::Null),
                            }]
                        } else {
                            vec![]
                        }
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                };
                self.current_block_type = None;
                r
            }
            "message_delta" => {
                let delta = &json["delta"];
                if let Some(u) = json.get("usage") {
                    self.usage.output_tokens =
                        u.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                }
                vec![ContentEvent::Finished {
                    stop_reason: match delta["stop_reason"].as_str().unwrap_or("end_turn") {
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" => StopReason::MaxTokens,
                        "stop_sequence" => StopReason::Stop,
                        _ => StopReason::EndTurn,
                    },
                    usage: self.usage.clone(),
                }]
            }
            "error" => {
                tracing::error!(
                    "Anthropic: {}",
                    json["error"]["message"].as_str().unwrap_or("?")
                );
                vec![]
            }
            _ => vec![],
        }
    }
}

// Fix #3: Safe pin projection via pin_project_lite — zero unsafe
// Fix #9: BytesMut ring buffer for O(N) total parsing
pin_project! {
    struct AnthropicEventStream {
        #[pin]
        byte_stream: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
        parser: AnthropicStreamParser,
        buffer: BytesMut,
        pending_events: VecDeque<ContentEvent>,  // Fix #2: multi-event queue
        cancel: CancellationToken,
        finished: bool,
    }
}

impl Stream for AnthropicEventStream {
    type Item = Result<ContentEvent, ProviderError>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut this = self.project();
        if *this.finished {
            return std::task::Poll::Ready(None);
        }
        if this.cancel.is_cancelled() {
            *this.finished = true;
            return std::task::Poll::Ready(Some(Err(ProviderError::Cancelled)));
        }

        // Drain pending events first (fix #2)
        if let Some(event) = this.pending_events.pop_front() {
            if matches!(&event, ContentEvent::Finished { .. }) {
                *this.finished = true;
            }
            return std::task::Poll::Ready(Some(Ok(event)));
        }

        loop {
            // Parse complete SSE frames from buffer (fix #9: O(1) split_to)
            let buf_bytes = &this.buffer[..];
            if let Some(pos) = find_double_newline(buf_bytes) {
                let block_bytes = this.buffer.split_to(pos);
                let _ = this.buffer.split_to(2); // consume \n\n
                let block = String::from_utf8_lossy(&block_bytes);
                let mut event_type = "";
                let mut data = "";
                for line in block.lines() {
                    if let Some(s) = line.strip_prefix("event: ") {
                        event_type = s.trim();
                    } else if let Some(s) = line.strip_prefix("data: ") {
                        data = s.trim();
                    }
                }
                if !event_type.is_empty() && !data.is_empty() {
                    let events = this.parser.process_event(event_type, data);
                    for e in events {
                        this.pending_events.push_back(e);
                    }
                    if let Some(event) = this.pending_events.pop_front() {
                        if matches!(&event, ContentEvent::Finished { .. }) {
                            *this.finished = true;
                        }
                        return std::task::Poll::Ready(Some(Ok(event)));
                    }
                }
                continue;
            }

            match this.byte_stream.as_mut().poll_next(cx) {
                std::task::Poll::Ready(Some(Ok(bytes))) => {
                    this.buffer.extend_from_slice(&bytes);
                }
                std::task::Poll::Ready(Some(Err(e))) => {
                    *this.finished = true;
                    return std::task::Poll::Ready(Some(Err(ProviderError::Network(
                        e.to_string(),
                    ))));
                }
                std::task::Poll::Ready(None) => {
                    *this.finished = true;
                    return std::task::Poll::Ready(None);
                }
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

fn find_double_newline(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}
