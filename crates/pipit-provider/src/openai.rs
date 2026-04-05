use crate::{
    ContentEvent, CompletionRequest, LlmProvider, ModelCapabilities, PreferredFormat,
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

/// OpenAI-compatible provider (works with OpenAI, OpenRouter, DeepSeek, etc.)
pub struct OpenAiProvider {
    client: Client,
    provider_id: String,
    model: String,
    api_key: String,
    base_url: String,
    capabilities: ModelCapabilities,
}

impl OpenAiProvider {
    pub fn new(
        model: String,
        api_key: String,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        Self::with_id("openai".to_string(), model, api_key, base_url)
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
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com".to_string()),
            capabilities,
        })
    }

    fn capabilities_for_model(model: &str) -> ModelCapabilities {
        ModelCapabilities {
            context_window: 128_000,
            max_output_tokens: if model.contains("gpt-4") { 16_384 } else { 8192 },
            supports_tool_use: true,
            supports_streaming: true,
            supports_thinking: false,
            // Most modern models support vision — enable by default for OpenAI-compatible
            supports_images: true,
            supports_prefill: false,
            preferred_edit_format: Some(PreferredFormat::SearchReplace),
        }
    }

    fn build_request_body(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut messages = Vec::new();

        // Collect all system content into a single system message at position 0.
        // Many models (e.g. Qwen) require exactly one system message at the start.
        let mut system_parts: Vec<String> = Vec::new();
        if !request.system.is_empty() {
            system_parts.push(request.system.clone());
        }
        for msg in &request.messages {
            if matches!(&msg.role, crate::Role::System) {
                let text = msg.text_content();
                if !text.is_empty() {
                    system_parts.push(text);
                }
            }
        }
        if !system_parts.is_empty() {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system_parts.join("\n\n"),
            }));
        }

        for msg in &request.messages {
            match &msg.role {
                crate::Role::System => {
                    // Already merged into the single system message above
                    continue;
                }
                crate::Role::User => {
                    // Check if this message contains images
                    let has_images = msg.content.iter().any(|b| matches!(b, crate::ContentBlock::Image { .. }));

                    if has_images {
                        // Multi-part content with text + image_url blocks
                        let mut parts = Vec::new();
                        for block in &msg.content {
                            match block {
                                crate::ContentBlock::Text(t) => {
                                    if !t.is_empty() {
                                        parts.push(serde_json::json!({
                                            "type": "text",
                                            "text": t,
                                        }));
                                    }
                                }
                                crate::ContentBlock::Image { media_type, data } => {
                                    use base64::Engine;
                                    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
                                    parts.push(serde_json::json!({
                                        "type": "image_url",
                                        "image_url": {
                                            "url": format!("data:{};base64,{}", media_type, encoded),
                                        }
                                    }));
                                }
                                _ => {}
                            }
                        }
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": parts,
                        }));
                    } else {
                        messages.push(serde_json::json!({
                            "role": "user",
                            "content": msg.text_content(),
                        }));
                    }
                }
                crate::Role::Assistant => {
                    let mut content_parts = Vec::new();
                    let mut tool_calls = Vec::new();

                    for block in &msg.content {
                        match block {
                            crate::ContentBlock::Text(t) => {
                                content_parts.push(t.clone());
                            }
                            crate::ContentBlock::ToolCall {
                                call_id,
                                name,
                                args,
                            } => {
                                tool_calls.push(serde_json::json!({
                                    "id": call_id,
                                    "type": "function",
                                    "function": {
                                        "name": name,
                                        "arguments": args.to_string(),
                                    }
                                }));
                            }
                            _ => {}
                        }
                    }

                    let mut msg_json = serde_json::json!({
                        "role": "assistant",
                    });
                    if !content_parts.is_empty() {
                        msg_json["content"] =
                            serde_json::json!(content_parts.join(""));
                    }
                    if !tool_calls.is_empty() {
                        msg_json["tool_calls"] = serde_json::json!(tool_calls);
                    }
                    messages.push(msg_json);
                }
                crate::Role::ToolResult { call_id } => {
                    let content = msg.text_content();
                    let tool_content = msg.content.iter().find_map(|b| match b {
                        crate::ContentBlock::ToolResult { content, .. } => Some(content.clone()),
                        _ => None,
                    }).unwrap_or(content);

                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": tool_content,
                    }));
                }
            }
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "stream": true,
            "messages": messages,
        });

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(max) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tools);
        }

        body
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
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

        let response = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());

            // Detect context overflow from 400 responses (vLLM, Ollama, etc.)
            let is_context_overflow = {
                let lower = body_text.to_ascii_lowercase();
                lower.contains("maximum context length")
                    || lower.contains("context length exceeded")
                    || lower.contains("context_length_exceeded")
                    || (lower.contains("prompt") && lower.contains("too long"))
                    || (lower.contains("maximum") && lower.contains("token"))
                    || lower.contains("too many tokens")
            };

            return match status.as_u16() {
                401 => Err(ProviderError::AuthFailed {
                    message: body_text,
                }),
                413 => Err(ProviderError::RequestTooLarge {
                    message: body_text,
                }),
                429 => Err(ProviderError::RateLimited {
                    retry_after_ms: None,
                }),
                400 if is_context_overflow => Err(ProviderError::ContextOverflow {
                    used: 0,
                    limit: 0,
                }),
                _ => Err(ProviderError::Other(format!(
                    "HTTP {}: {}",
                    status, body_text
                ))),
            };
        }

        let byte_stream = response.bytes_stream();
        let event_stream = OpenAiEventStream {
            byte_stream: Box::pin(byte_stream),
            parser: OpenAiStreamParser::new(),
            buffer: BytesMut::new(),
            pending_events: VecDeque::new(),
            cancel,
            finished: false,
        };

        Ok(Box::pin(event_stream))
    }

    // Fix #5: Better token estimation
    async fn count_tokens(&self, messages: &[crate::Message]) -> Result<TokenCount, ProviderError> {
        let (mut bytes, mut words) = (0usize, 0usize);
        for m in messages {
            for b in &m.content {
                match b {
                    crate::ContentBlock::Text(t) => { bytes += t.len(); words += t.split_whitespace().count(); }
                    crate::ContentBlock::ToolCall { args, .. } => { bytes += args.to_string().len(); }
                    crate::ContentBlock::ToolResult { content, .. } => { bytes += content.len(); words += content.split_whitespace().count(); }
                    _ => {}
                }
            }
        }
        Ok(TokenCount { tokens: ((bytes as u64) / 4).max(((words as f64) * 1.3) as u64) })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }
}

struct OpenAiStreamParser {
    tool_calls: HashMap<u32, (String, String, String)>, // index -> (id, name, args)
    usage: UsageMetadata,
}

impl OpenAiStreamParser {
    fn new() -> Self {
        Self {
            tool_calls: HashMap::new(),
            usage: UsageMetadata::default(),
        }
    }

    fn process_chunk(&mut self, data: &str) -> Vec<ContentEvent> {
        if data.trim() == "[DONE]" {
            // Emit any pending tool calls, then finish
            let mut events = Vec::new();
            for (_idx, (id, name, args)) in self.tool_calls.drain() {
                let parsed_args =
                    serde_json::from_str(&args).unwrap_or(serde_json::Value::Null);
                events.push(ContentEvent::ToolCallComplete {
                    call_id: id,
                    tool_name: name,
                    args: parsed_args,
                });
            }
            events.push(ContentEvent::Finished {
                stop_reason: StopReason::EndTurn,
                usage: self.usage.clone(),
            });
            return events;
        }

        let json: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        let mut events = Vec::new();

        if let Some(choices) = json.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                let delta = &choice["delta"];
                let finish_reason = choice["finish_reason"].as_str();

                // Text content
                if let Some(content) = delta.get("content").and_then(|c| c.as_str()) {
                    if !content.is_empty() {
                        events.push(ContentEvent::ContentDelta {
                            text: content.to_string(),
                        });
                    }
                }

                // Tool calls (streaming)
                if let Some(tool_calls) = delta.get("tool_calls").and_then(|tc| tc.as_array()) {
                    for tc in tool_calls {
                        let idx = tc["index"].as_u64().unwrap_or(0) as u32;
                        let entry =
                            self.tool_calls
                                .entry(idx)
                                .or_insert_with(|| (String::new(), String::new(), String::new()));

                        if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                            entry.0 = id.to_string();
                        }
                        if let Some(func) = tc.get("function") {
                            if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                                entry.1 = name.to_string();
                            }
                            if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                                entry.2.push_str(args);
                            }
                        }
                    }
                }

                // Finish reason
                if let Some(reason) = finish_reason {
                    let stop = match reason {
                        "stop" => StopReason::Stop,
                        "tool_calls" => StopReason::ToolUse,
                        "length" => StopReason::MaxTokens,
                        _ => StopReason::EndTurn,
                    };

                    // Emit pending tool calls
                    for (_idx, (id, name, args)) in self.tool_calls.drain() {
                        let parsed =
                            serde_json::from_str(&args).unwrap_or(serde_json::Value::Null);
                        events.push(ContentEvent::ToolCallComplete {
                            call_id: id,
                            tool_name: name,
                            args: parsed,
                        });
                    }

                    events.push(ContentEvent::Finished {
                        stop_reason: stop,
                        usage: self.usage.clone(),
                    });
                }
            }
        }

        // Usage info
        if let Some(usage) = json.get("usage") {
            self.usage.input_tokens = usage
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            self.usage.output_tokens = usage
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
        }

        events
    }
}

// Fix #3/#9: Safe pin projection + BytesMut buffer
pin_project! {
    struct OpenAiEventStream {
        #[pin]
        byte_stream: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
        parser: OpenAiStreamParser,
        buffer: BytesMut,
        pending_events: VecDeque<ContentEvent>,
        cancel: CancellationToken,
        finished: bool,
    }
}

impl Stream for OpenAiEventStream {
    type Item = Result<ContentEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        let mut this = self.project();
        if *this.finished { return std::task::Poll::Ready(None); }
        if this.cancel.is_cancelled() {
            *this.finished = true;
            return std::task::Poll::Ready(Some(Err(ProviderError::Cancelled)));
        }

        // Drain pending events
        if let Some(event) = this.pending_events.pop_front() {
            if matches!(&event, ContentEvent::Finished { .. }) { *this.finished = true; }
            return std::task::Poll::Ready(Some(Ok(event)));
        }

        loop {
            // Parse SSE data lines from buffer
            let buf_bytes = &this.buffer[..];
            if let Some(pos) = buf_bytes.windows(2).position(|w| w == b"\n\n") {
                let block_bytes = this.buffer.split_to(pos);
                let _ = this.buffer.split_to(2);
                let block = String::from_utf8_lossy(&block_bytes);

                for line in block.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if !data.is_empty() {
                            let events = this.parser.process_chunk(data);
                            for e in events { this.pending_events.push_back(e); }
                        }
                    }
                }
                if let Some(event) = this.pending_events.pop_front() {
                    if matches!(&event, ContentEvent::Finished { .. }) { *this.finished = true; }
                    return std::task::Poll::Ready(Some(Ok(event)));
                }
                continue;
            }

            match this.byte_stream.as_mut().poll_next(cx) {
                std::task::Poll::Ready(Some(Ok(bytes))) => { this.buffer.extend_from_slice(&bytes); }
                std::task::Poll::Ready(Some(Err(e))) => {
                    // Drain any buffered data before reporting error
                    if !this.buffer.is_empty() {
                        let remaining = std::mem::take(this.buffer);
                        let block = String::from_utf8_lossy(&remaining);
                        for line in block.lines() {
                            if let Some(data) = line.strip_prefix("data: ") {
                                let data = data.trim();
                                if !data.is_empty() {
                                    let events = this.parser.process_chunk(data);
                                    for ev in events {
                                        this.pending_events.push_back(ev);
                                    }
                                }
                            }
                        }
                    }
                    // If we extracted events from the buffer, return them before erroring
                    if let Some(event) = this.pending_events.pop_front() {
                        if matches!(&event, ContentEvent::Finished { .. }) {
                            *this.finished = true;
                        }
                        return std::task::Poll::Ready(Some(Ok(event)));
                    }
                    *this.finished = true;
                    return std::task::Poll::Ready(Some(Err(ProviderError::Network(e.to_string()))));
                }
                std::task::Poll::Ready(None) => { *this.finished = true; return std::task::Poll::Ready(None); }
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}
