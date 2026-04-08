use crate::{
    CompletionRequest, ContentEvent, LlmProvider, ModelCapabilities, PreferredFormat,
    ProviderError, StopReason, TokenCount, UsageMetadata,
};
use async_trait::async_trait;
use bytes::BytesMut;
use futures::stream::Stream;
use pin_project_lite::pin_project;
use reqwest::Client;
use std::collections::VecDeque;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

/// Google Gemini provider — uses the Gemini REST streaming API.
pub struct GoogleProvider {
    client: Client,
    model: String,
    api_key: String,
    base_url: String,
    capabilities: ModelCapabilities,
}

impl GoogleProvider {
    pub fn new(
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
            model,
            api_key,
            base_url: base_url
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com".to_string()),
            capabilities,
        })
    }

    fn capabilities_for_model(model: &str) -> ModelCapabilities {
        let is_flash = model.contains("flash");
        ModelCapabilities {
            context_window: 1_000_000,
            max_output_tokens: if is_flash { 8192 } else { 65536 },
            supports_tool_use: true,
            supports_streaming: true,
            supports_thinking: model.contains("thinking") || model.contains("2.5"),
            supports_images: true,
            supports_prefill: false,
            preferred_edit_format: Some(PreferredFormat::SearchReplace),
        }
    }

    fn build_request_body(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut contents = Vec::new();

        for msg in &request.messages {
            match &msg.role {
                crate::Role::System => continue, // handled via systemInstruction
                crate::Role::User => {
                    let parts = self.convert_parts(&msg.content);
                    contents.push(serde_json::json!({"role": "user", "parts": parts}));
                }
                crate::Role::Assistant => {
                    let parts = self.convert_parts(&msg.content);
                    contents.push(serde_json::json!({"role": "model", "parts": parts}));
                }
                crate::Role::ToolResult { call_id } => {
                    let result_content = msg
                        .content
                        .iter()
                        .find_map(|b| match b {
                            crate::ContentBlock::ToolResult { content, .. } => {
                                Some(content.clone())
                            }
                            _ => None,
                        })
                        .unwrap_or_default();

                    // Parse as JSON if possible, otherwise wrap in a response object
                    let response_val = serde_json::from_str::<serde_json::Value>(&result_content)
                        .unwrap_or_else(|_| serde_json::json!({"result": result_content}));

                    contents.push(serde_json::json!({
                        "role": "user",
                        "parts": [{
                            "functionResponse": {
                                "name": call_id,
                                "response": response_val,
                            }
                        }]
                    }));
                }
            }
        }

        let mut body = serde_json::json!({
            "contents": contents,
        });

        if !request.system.is_empty() {
            body["systemInstruction"] = serde_json::json!({
                "parts": [{"text": request.system}]
            });
        }

        // Generation config
        let mut gen_config = serde_json::json!({});
        if let Some(temp) = request.temperature {
            gen_config["temperature"] = serde_json::json!(temp);
        }
        if let Some(max) = request.max_tokens {
            gen_config["maxOutputTokens"] = serde_json::json!(max);
        }
        body["generationConfig"] = gen_config;

        // Tools (function declarations)
        if !request.tools.is_empty() {
            let functions: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    })
                })
                .collect();
            body["tools"] = serde_json::json!([{"functionDeclarations": functions}]);
        }

        body
    }

    fn convert_parts(&self, blocks: &[crate::ContentBlock]) -> Vec<serde_json::Value> {
        let mut parts = Vec::new();
        for block in blocks {
            match block {
                crate::ContentBlock::Text(text) => {
                    if !text.is_empty() {
                        parts.push(serde_json::json!({"text": text}));
                    }
                }
                crate::ContentBlock::ToolCall {
                    call_id: _,
                    name,
                    args,
                } => {
                    parts.push(serde_json::json!({
                        "functionCall": {
                            "name": name,
                            "args": args,
                        }
                    }));
                }
                crate::ContentBlock::Image { media_type, data } => {
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD.encode(data);
                    parts.push(serde_json::json!({
                        "inlineData": {
                            "mimeType": media_type,
                            "data": encoded,
                        }
                    }));
                }
                _ => {}
            }
        }
        if parts.is_empty() {
            parts.push(serde_json::json!({"text": ""}));
        }
        parts
    }
}

#[async_trait]
impl LlmProvider for GoogleProvider {
    fn id(&self) -> &str {
        "google"
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

        // Gemini streaming endpoint:
        // POST /v1beta/models/{model}:streamGenerateContent?alt=sse&key={key}
        let url = format!(
            "{}/v1beta/models/{}:streamGenerateContent?alt=sse&key={}",
            self.base_url, self.model, self.api_key
        );

        let response = self
            .client
            .post(&url)
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
            return match status.as_u16() {
                401 | 403 => Err(ProviderError::AuthFailed { message: body_text }),
                429 => Err(ProviderError::RateLimited {
                    retry_after_ms: None,
                }),
                _ => Err(ProviderError::Other(format!(
                    "HTTP {}: {}",
                    status, body_text
                ))),
            };
        }

        let event_stream = GeminiEventStream {
            byte_stream: Box::pin(response.bytes_stream()),
            buffer: BytesMut::new(),
            pending_events: VecDeque::new(),
            usage: UsageMetadata::default(),
            cancel,
            finished: false,
        };

        Ok(Box::pin(event_stream))
    }

    async fn count_tokens(&self, messages: &[crate::Message]) -> Result<TokenCount, ProviderError> {
        let (mut bytes, mut words) = (0usize, 0usize);
        for m in messages {
            for b in &m.content {
                match b {
                    crate::ContentBlock::Text(t) => {
                        bytes += t.len();
                        words += t.split_whitespace().count();
                    }
                    crate::ContentBlock::ToolCall { args, .. } => {
                        bytes += args.to_string().len();
                    }
                    crate::ContentBlock::ToolResult { content, .. } => {
                        bytes += content.len();
                        words += content.split_whitespace().count();
                    }
                    _ => {}
                }
            }
        }
        Ok(TokenCount {
            tokens: ((bytes as u64) / 4).max(((words as f64) * 1.3) as u64),
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }
}

/// Parse a Gemini SSE streaming chunk.
fn parse_gemini_chunk(data: &str) -> (Vec<ContentEvent>, Option<UsageMetadata>) {
    let json: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return (vec![], None),
    };

    let mut events = Vec::new();
    let mut usage_out = None;

    if let Some(candidates) = json.get("candidates").and_then(|c| c.as_array()) {
        for candidate in candidates {
            if let Some(content) = candidate.get("content") {
                if let Some(parts) = content.get("parts").and_then(|p| p.as_array()) {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                events.push(ContentEvent::ContentDelta {
                                    text: text.to_string(),
                                });
                            }
                        }
                        if let Some(fc) = part.get("functionCall") {
                            let name = fc
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string();
                            let args = fc.get("args").cloned().unwrap_or(serde_json::Value::Null);
                            events.push(ContentEvent::ToolCallComplete {
                                call_id: name.clone(),
                                tool_name: name,
                                args,
                            });
                        }
                        // Gemini thinking
                        if let Some(thought) = part.get("thought").and_then(|t| t.as_str()) {
                            if !thought.is_empty() {
                                events.push(ContentEvent::ThinkingDelta {
                                    text: thought.to_string(),
                                });
                            }
                        }
                    }
                }
            }

            // Finish reason
            if let Some(reason) = candidate.get("finishReason").and_then(|r| r.as_str()) {
                let stop = match reason {
                    "STOP" => StopReason::EndTurn,
                    "MAX_TOKENS" => StopReason::MaxTokens,
                    "SAFETY" => StopReason::Stop,
                    _ => StopReason::EndTurn,
                };
                // Defer Finished until we've parsed usage
                // Mark it for later
                // Usage is a placeholder here; the GeminiEventStream::poll_next
                // replaces it with accumulated usage before emitting to consumers.
                events.push(ContentEvent::Finished {
                    stop_reason: stop,
                    usage: UsageMetadata::default(),
                });
            }
        }
    }

    // Usage metadata
    if let Some(meta) = json.get("usageMetadata") {
        let u = UsageMetadata {
            input_tokens: meta
                .get("promptTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            output_tokens: meta
                .get("candidatesTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0),
            cache_read_tokens: meta.get("cachedContentTokenCount").and_then(|v| v.as_u64()),
            cache_creation_tokens: None,
        };
        usage_out = Some(u);
    }

    (events, usage_out)
}

pin_project! {
    struct GeminiEventStream {
        #[pin]
        byte_stream: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
        buffer: BytesMut,
        pending_events: VecDeque<ContentEvent>,
        usage: UsageMetadata,
        cancel: CancellationToken,
        finished: bool,
    }
}

impl Stream for GeminiEventStream {
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

        // Drain pending events
        if let Some(event) = this.pending_events.pop_front() {
            if let ContentEvent::Finished { stop_reason, .. } = &event {
                *this.finished = true;
                return std::task::Poll::Ready(Some(Ok(ContentEvent::Finished {
                    stop_reason: *stop_reason,
                    usage: this.usage.clone(),
                })));
            }
            return std::task::Poll::Ready(Some(Ok(event)));
        }

        loop {
            // Parse SSE data lines
            let buf_bytes = &this.buffer[..];
            if let Some(pos) = buf_bytes.windows(2).position(|w| w == b"\n\n") {
                let block_bytes = this.buffer.split_to(pos);
                let _ = this.buffer.split_to(2);
                let block = String::from_utf8_lossy(&block_bytes);

                for line in block.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if !data.is_empty() {
                            let (events, usage_update) = parse_gemini_chunk(data);
                            if let Some(u) = usage_update {
                                *this.usage = u;
                            }
                            for e in events {
                                this.pending_events.push_back(e);
                            }
                        }
                    }
                }
                if let Some(event) = this.pending_events.pop_front() {
                    if let ContentEvent::Finished { stop_reason, .. } = &event {
                        *this.finished = true;
                        return std::task::Poll::Ready(Some(Ok(ContentEvent::Finished {
                            stop_reason: *stop_reason,
                            usage: this.usage.clone(),
                        })));
                    }
                    return std::task::Poll::Ready(Some(Ok(event)));
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
                    // Stream ended — if we haven't gotten a Finished event, emit one
                    if !*this.finished {
                        *this.finished = true;
                        return std::task::Poll::Ready(Some(Ok(ContentEvent::Finished {
                            stop_reason: StopReason::EndTurn,
                            usage: this.usage.clone(),
                        })));
                    }
                    return std::task::Poll::Ready(None);
                }
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}
