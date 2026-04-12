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

/// OpenAI-compatible provider (works with OpenAI, OpenRouter, DeepSeek, etc.)
pub struct OpenAiProvider {
    client: Client,
    provider_id: String,
    model: String,
    api_key: String,
    base_url: String,
    /// Path appended to base_url for chat completions.
    /// Defaults to `/v1/chat/completions`. Azure overrides this.
    chat_path: String,
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
            chat_path: "/v1/chat/completions".to_string(),
            capabilities,
        })
    }

    /// Set a custom chat completions path (used by Azure OpenAI).
    pub fn set_chat_path(&mut self, path: String) {
        self.chat_path = path;
    }

    fn capabilities_for_model(model: &str) -> ModelCapabilities {
        let lower = model.to_lowercase();

        // Infer max_output_tokens from model family.
        // Modern open-weight models (Qwen-3, Llama-4, DeepSeek-V3, Mistral-Large)
        // support at least 32K output tokens.  GPT-4o supports 16K.  Unknown
        // models get a generous 16K default — much better than 8K which truncates
        // any non-trivial write_file call.
        let max_output_tokens = if lower.contains("qwen") {
            // Qwen 2.5/3/3.5 all support 32K+ output
            32_768
        } else if lower.contains("llama") || lower.contains("meta-llama") {
            32_768
        } else if lower.contains("deepseek") {
            32_768
        } else if lower.contains("mistral") || lower.contains("codestral") {
            32_768
        } else if lower.contains("gpt-4o") || lower.contains("gpt-4-turbo") {
            16_384
        } else if lower.contains("gpt-4") {
            16_384
        } else if lower.contains("o1") || lower.contains("o3") || lower.contains("o4") {
            32_768
        } else {
            // Unknown model — default to 16K which is safe for most modern models
            // and dramatically better than 8K for write-heavy tasks.
            16_384
        };

        // o-series models (o1, o3, o4) support extended thinking / reasoning
        let supports_thinking = lower.contains("o1")
            || lower.contains("o3")
            || lower.contains("o4");

        ModelCapabilities {
            context_window: 128_000,
            max_output_tokens,
            supports_tool_use: true,
            supports_streaming: true,
            supports_thinking,
            // Most modern models support vision — enable by default for OpenAI-compatible
            supports_images: true,
            supports_prefill: false,
            preferred_edit_format: Some(PreferredFormat::SearchReplace),
        }
    }

    fn apply_provider_headers(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.provider_id.as_str() {
            "github_copilot" => builder
                .header("User-Agent", "GitHubCopilotChat/0.35.0")
                .header("Editor-Version", "vscode/1.107.0")
                .header("Editor-Plugin-Version", "copilot-chat/0.35.0")
                .header("Copilot-Integration-Id", "vscode-chat"),
            "azure_openai" => builder.header("api-key", &self.api_key),
            _ => builder,
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
            let system_content = system_parts.join("\n\n");
            tracing::info!(
                system_prompt_chars = system_content.len(),
                system_prompt_approx_tokens = system_content.len() / 4,
                "System prompt size"
            );
            messages.push(serde_json::json!({
                "role": "system",
                "content": system_content,
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
                    let has_images = msg
                        .content
                        .iter()
                        .any(|b| matches!(b, crate::ContentBlock::Image { .. }));

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
                                    let encoded =
                                        base64::engine::general_purpose::STANDARD.encode(data);
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
                    // Always set content. OpenAI/Azure rejects null content
                    // on assistant messages. When the response has only thinking
                    // (no visible text), content_parts is empty — use "".
                    msg_json["content"] = serde_json::json!(content_parts.join(""));
                    if !tool_calls.is_empty() {
                        msg_json["tool_calls"] = serde_json::json!(tool_calls);
                    }
                    messages.push(msg_json);
                }
                crate::Role::ToolResult { call_id } => {
                    let content = msg.text_content();
                    let tool_content = msg
                        .content
                        .iter()
                        .find_map(|b| match b {
                            crate::ContentBlock::ToolResult { content, .. } => {
                                Some(content.clone())
                            }
                            _ => None,
                        })
                        .unwrap_or(content);

                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": tool_content,
                    }));
                }
            }
        }

        // Safety net: some providers (e.g. Qwen) require at least one
        // user-role message. If compaction removed all of them, inject a
        // minimal one so the request doesn't 400.
        let has_user = messages
            .iter()
            .any(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"));
        if !has_user {
            messages.push(serde_json::json!({
                "role": "user",
                "content": "Continue.",
            }));
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
            // Newer models (GPT-4o+, GPT-5+, Azure) use max_completion_tokens.
            // Older models use max_tokens. Use the newer field for Azure and
            // models that indicate they're new enough.
            if self.provider_id == "azure_openai"
                || self.model.starts_with("gpt-5")
                || self.model.starts_with("gpt-4o")
                || self.model.starts_with("o1")
                || self.model.starts_with("o3")
                || self.model.starts_with("o4")
            {
                body["max_completion_tokens"] = serde_json::json!(max);
            } else {
                body["max_tokens"] = serde_json::json!(max);
            }
        }

        if !request.tools.is_empty() {
            tracing::info!(
                tool_count = request.tools.len(),
                tool_names = %request.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", "),
                "Sending tools to LLM"
            );
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

        // ── Request-level profiling ──
        // When PIPIT_DUMP_REQUESTS is set, write the full request JSON to that
        // directory.  This gives ground-truth token accounting without guessing.
        if let Ok(dump_dir) = std::env::var("PIPIT_DUMP_REQUESTS") {
            use std::io::Write;
            static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = format!("{}/req_{:04}.json", dump_dir, n);
            if let Ok(mut f) = std::fs::File::create(&path) {
                let _ = f.write_all(body.to_string().as_bytes());
            }
            // Also write a summary with component sizes
            let messages = body["messages"].as_array();
            let tools = body["tools"].as_array();
            let mut summary = String::new();
            summary.push_str(&format!("=== Request {} ===\n", n));
            summary.push_str(&format!("Total body bytes: {}\n", body.to_string().len()));
            if let Some(msgs) = messages {
                for (i, msg) in msgs.iter().enumerate() {
                    let role = msg["role"].as_str().unwrap_or("?");
                    let content_len = msg["content"].to_string().len();
                    let tc_len = msg.get("tool_calls").map(|t| t.to_string().len()).unwrap_or(0);
                    summary.push_str(&format!(
                        "  msg[{}] role={:<12} content={:>6} bytes  tool_calls={:>5} bytes\n",
                        i, role, content_len, tc_len
                    ));
                }
            }
            if let Some(tl) = tools {
                summary.push_str(&format!("Tools: {} definitions, {} bytes total\n",
                    tl.len(), serde_json::to_string(tl).unwrap_or_default().len()));
                for t in tl {
                    let name = t["function"]["name"].as_str().unwrap_or("?");
                    let sz = t.to_string().len();
                    summary.push_str(&format!("  tool {:<20} {} bytes\n", name, sz));
                }
            }
            let summary_path = format!("{}/req_{:04}_summary.txt", dump_dir, n);
            if let Ok(mut f) = std::fs::File::create(&summary_path) {
                let _ = f.write_all(summary.as_bytes());
            }
        }

        let request_builder = self
            .client
            .post(format!("{}{}", self.base_url, self.chat_path))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body);
        let response = self
            .apply_provider_headers(request_builder)
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            // Parse Retry-After header before consuming body.
            // Azure OpenAI sends this on 429: "Retry-After: 6" (seconds)
            let retry_after_ms = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<f64>().ok())
                .map(|secs| (secs * 1000.0) as u64);

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

            // Detect malformed request body (vLLM/Ollama reject corrupted tool JSON)
            let is_malformed = !is_context_overflow && status.as_u16() == 400 && {
                let lower = body_text.to_ascii_lowercase();
                lower.contains("can only get item")
                    || lower.contains("invalid tool")
                    || lower.contains("invalid function")
                    || lower.contains("invalid json")
                    || lower.contains("could not parse")
                    || lower.contains("malformed")
                    || lower.contains("unexpected token")
                    || lower.contains("is not valid json")
                    || lower.contains("invalid value")
                    || lower.contains("does not match")
            };

            return match status.as_u16() {
                401 => Err(ProviderError::AuthFailed { message: body_text }),
                413 => Err(ProviderError::RequestTooLarge { message: body_text }),
                429 => Err(ProviderError::RateLimited {
                    retry_after_ms,
                }),
                400 if is_context_overflow => {
                    Err(ProviderError::ContextOverflow { used: 0, limit: 0 })
                }
                400 if is_malformed => {
                    Err(ProviderError::MalformedRequest { message: body_text })
                }
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

    // Content-aware heuristic token estimation.
    // OpenAI does not expose a public count_tokens API endpoint;
    // we use calibrated heuristics (~3.8 chars/token for English prose,
    // ~3.0 for code, ~1.5 for CJK).
    async fn count_tokens(&self, messages: &[crate::Message]) -> Result<TokenCount, ProviderError> {
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
        let punct_ratio = punct as f64 / bytes as f64;
        let divisor = if punct_ratio > 0.15 { 3.0 } else { 3.8 };
        let byte_estimate = (bytes as f64 / divisor).ceil() as u64;
        let word_estimate = ((words as f64) * 1.3) as u64;
        Ok(TokenCount {
            tokens: byte_estimate.max(word_estimate),
        })
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
                // Skip ghost tool calls with empty name (vLLM streaming artifact)
                if name.is_empty() {
                    tracing::debug!("Skipping ghost tool call with empty name");
                    continue;
                }
                let parsed_args = crate::parse_tool_args(&name, &args);
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

                // Reasoning/thinking content (vLLM Qwen sends delta.reasoning)
                if let Some(reasoning) = delta.get("reasoning").and_then(|r| r.as_str()) {
                    if !reasoning.is_empty() {
                        events.push(ContentEvent::ThinkingDelta {
                            text: reasoning.to_string(),
                        });
                    }
                }

                // Tool calls (streaming)
                if let Some(tool_calls) = delta.get("tool_calls").and_then(|tc| tc.as_array()) {
                    for tc in tool_calls {
                        let idx = tc["index"].as_u64().unwrap_or(0) as u32;
                        let entry = self
                            .tool_calls
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
                        // Skip ghost tool calls with empty name (vLLM streaming artifact)
                        if name.is_empty() {
                            tracing::debug!("Skipping ghost tool call with empty name");
                            continue;
                        }
                        let parsed = crate::parse_tool_args(&name, &args);
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
            if matches!(&event, ContentEvent::Finished { .. }) {
                *this.finished = true;
            }
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
                            for e in events {
                                this.pending_events.push_back(e);
                            }
                        }
                    }
                }
                if let Some(event) = this.pending_events.pop_front() {
                    if matches!(&event, ContentEvent::Finished { .. }) {
                        *this.finished = true;
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

// ═══════════════════════════════════════════════════════════════════════
// Streaming conformance tests for OpenAI SSE parser
// ═══════════════════════════════════════════════════════════════════════
//
// These tests verify that the parser correctly transduces raw OpenAI
// SSE chunks into the canonical ContentEvent sequence. The contract:
//
//   1. Every stream MUST end with exactly one Finished event.
//   2. ContentDelta events MUST NOT contain empty text.
//   3. ToolCallComplete args MUST be valid JSON (or Value::Null on parse failure).
//   4. Usage MUST be propagated into the Finished event when present.
//   5. Streaming tool call fragments MUST be buffered and emitted as
//      a single ToolCallComplete per tool invocation.
//   6. ToolCallDelta is NEVER emitted (buffered internally).
//
#[cfg(test)]
mod tests {
    use super::*;

    // ── Helpers ──

    fn text_chunk(content: &str) -> String {
        serde_json::json!({
            "choices": [{
                "delta": {"content": content},
                "finish_reason": null
            }]
        })
        .to_string()
    }

    fn finish_chunk(reason: &str) -> String {
        serde_json::json!({
            "choices": [{
                "delta": {},
                "finish_reason": reason
            }]
        })
        .to_string()
    }

    fn tool_call_start_chunk(index: u32, id: &str, name: &str) -> String {
        serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "id": id,
                        "function": {"name": name, "arguments": ""}
                    }]
                },
                "finish_reason": null
            }]
        })
        .to_string()
    }

    fn tool_call_args_chunk(index: u32, args_fragment: &str) -> String {
        serde_json::json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "function": {"arguments": args_fragment}
                    }]
                },
                "finish_reason": null
            }]
        })
        .to_string()
    }

    fn usage_chunk(input: u64, output: u64) -> String {
        serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": input,
                "completion_tokens": output
            }
        })
        .to_string()
    }

    // ── Contract: Simple text response ──

    #[test]
    fn text_response_emits_content_deltas_then_finished() {
        let mut parser = OpenAiStreamParser::new();

        let e1 = parser.process_chunk(&text_chunk("Hello"));
        assert_eq!(e1.len(), 1);
        assert!(matches!(&e1[0], ContentEvent::ContentDelta { text } if text == "Hello"));

        let e2 = parser.process_chunk(&text_chunk(" world"));
        assert_eq!(e2.len(), 1);
        assert!(matches!(&e2[0], ContentEvent::ContentDelta { text } if text == " world"));

        let e3 = parser.process_chunk(&finish_chunk("stop"));
        assert_eq!(e3.len(), 1);
        assert!(matches!(&e3[0], ContentEvent::Finished { stop_reason: StopReason::Stop, .. }));
    }

    // ── Contract: Empty content is suppressed ──

    #[test]
    fn empty_content_delta_is_suppressed() {
        let mut parser = OpenAiStreamParser::new();
        let events = parser.process_chunk(&text_chunk(""));
        assert!(events.is_empty(), "Empty text should not emit ContentDelta");
    }

    // ── Contract: Tool call streaming accumulation ──

    #[test]
    fn tool_call_fragments_are_buffered_into_single_complete() {
        let mut parser = OpenAiStreamParser::new();

        // Start tool call
        let e1 = parser.process_chunk(&tool_call_start_chunk(0, "call_abc", "read_file"));
        assert!(e1.is_empty(), "Tool call start should not emit events (buffered)");

        // Stream argument fragments
        let e2 = parser.process_chunk(&tool_call_args_chunk(0, r#"{"pa"#));
        assert!(e2.is_empty());
        let e3 = parser.process_chunk(&tool_call_args_chunk(0, r#"th":"#));
        assert!(e3.is_empty());
        let e4 = parser.process_chunk(&tool_call_args_chunk(0, r#""foo.rs"}"#));
        assert!(e4.is_empty());

        // Finish — tool call emitted before Finished
        let e5 = parser.process_chunk(&finish_chunk("tool_calls"));
        assert_eq!(e5.len(), 2, "Should emit ToolCallComplete + Finished");
        assert!(matches!(&e5[0], ContentEvent::ToolCallComplete {
            call_id, tool_name, args
        } if call_id == "call_abc" && tool_name == "read_file"
            && args.get("path").and_then(|v| v.as_str()) == Some("foo.rs")));
        assert!(matches!(&e5[1], ContentEvent::Finished {
            stop_reason: StopReason::ToolUse, ..
        }));
    }

    // ── Contract: Multiple parallel tool calls ──

    #[test]
    fn multiple_parallel_tool_calls_are_tracked_independently() {
        let mut parser = OpenAiStreamParser::new();

        parser.process_chunk(&tool_call_start_chunk(0, "call_1", "read_file"));
        parser.process_chunk(&tool_call_start_chunk(1, "call_2", "grep"));
        parser.process_chunk(&tool_call_args_chunk(0, r#"{"path":"a.rs"}"#));
        parser.process_chunk(&tool_call_args_chunk(1, r#"{"pattern":"foo"}"#));

        let events = parser.process_chunk(&finish_chunk("tool_calls"));
        // 2 ToolCallComplete + 1 Finished
        assert_eq!(events.len(), 3);

        let tool_events: Vec<_> = events.iter().filter(|e| matches!(e, ContentEvent::ToolCallComplete { .. })).collect();
        assert_eq!(tool_events.len(), 2);
        assert!(matches!(&events.last().unwrap(), ContentEvent::Finished { .. }));
    }

    // ── Contract: [DONE] marker terminates stream ──

    #[test]
    fn done_marker_emits_pending_tools_and_finished() {
        let mut parser = OpenAiStreamParser::new();
        parser.process_chunk(&tool_call_start_chunk(0, "call_x", "bash"));
        parser.process_chunk(&tool_call_args_chunk(0, r#"{"command":"ls"}"#));

        let events = parser.process_chunk("[DONE]");
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], ContentEvent::ToolCallComplete { .. }));
        assert!(matches!(&events[1], ContentEvent::Finished {
            stop_reason: StopReason::EndTurn, ..
        }));
    }

    // ── Contract: Usage propagation ──

    #[test]
    fn usage_is_captured_and_propagated_to_finished() {
        let mut parser = OpenAiStreamParser::new();
        parser.process_chunk(&text_chunk("Hi"));
        parser.process_chunk(&usage_chunk(100, 50));

        let events = parser.process_chunk(&finish_chunk("stop"));
        let finished = events.iter().find(|e| matches!(e, ContentEvent::Finished { .. }));
        assert!(finished.is_some());
        if let ContentEvent::Finished { usage, .. } = finished.unwrap() {
            assert_eq!(usage.input_tokens, 100);
            assert_eq!(usage.output_tokens, 50);
        }
    }

    // ── Contract: Stop reason mapping ──

    #[test]
    fn stop_reasons_are_mapped_correctly() {
        let cases = [
            ("stop", StopReason::Stop),
            ("tool_calls", StopReason::ToolUse),
            ("length", StopReason::MaxTokens),
            ("content_filter", StopReason::EndTurn), // unknown → EndTurn
        ];
        for (reason, expected) in cases {
            let mut parser = OpenAiStreamParser::new();
            let events = parser.process_chunk(&finish_chunk(reason));
            assert!(matches!(&events[0], ContentEvent::Finished { stop_reason, .. } if *stop_reason == expected),
                "reason '{}' should map to {:?}", reason, expected);
        }
    }

    // ── Contract: ToolCallDelta is never emitted ──

    #[test]
    fn no_tool_call_delta_is_ever_emitted() {
        let mut parser = OpenAiStreamParser::new();
        parser.process_chunk(&tool_call_start_chunk(0, "c1", "write_file"));
        let e1 = parser.process_chunk(&tool_call_args_chunk(0, r#"{"p"#));
        let e2 = parser.process_chunk(&tool_call_args_chunk(0, r#":"v"}"#));
        let e3 = parser.process_chunk(&finish_chunk("tool_calls"));

        for events in [e1, e2, e3] {
            for event in &events {
                assert!(!matches!(event, ContentEvent::ToolCallDelta { .. }),
                    "ToolCallDelta must never be emitted");
            }
        }
    }

    // ── Contract: Invalid JSON is silently skipped ──

    #[test]
    fn invalid_json_chunk_returns_empty() {
        let mut parser = OpenAiStreamParser::new();
        let events = parser.process_chunk("not valid json {{");
        assert!(events.is_empty());
    }

    // ── Contract: Malformed tool call args parse to Null ──

    #[test]
    fn malformed_tool_args_parse_to_empty_object() {
        let mut parser = OpenAiStreamParser::new();
        parser.process_chunk(&tool_call_start_chunk(0, "c1", "bash"));
        parser.process_chunk(&tool_call_args_chunk(0, "not json at all"));

        let events = parser.process_chunk(&finish_chunk("tool_calls"));
        if let ContentEvent::ToolCallComplete { args, .. } = &events[0] {
            assert!(args.is_object(), "Unparseable args should become empty object {{}}");
            assert_eq!(args.as_object().unwrap().len(), 0, "Should be empty object");
        } else {
            panic!("Expected ToolCallComplete");
        }
    }

    #[test]
    fn double_brace_args_are_repaired() {
        let mut parser = OpenAiStreamParser::new();
        parser.process_chunk(&tool_call_start_chunk(0, "c1", "read_file"));
        parser.process_chunk(&tool_call_args_chunk(0, r#"{"path": "foo.rs"}}"#));

        let events = parser.process_chunk(&finish_chunk("tool_calls"));
        if let ContentEvent::ToolCallComplete { args, .. } = &events[0] {
            assert!(args.is_object(), "Repaired args should be an object");
            assert_eq!(args["path"], "foo.rs", "Repaired args should have path");
        } else {
            panic!("Expected ToolCallComplete");
        }
    }

    #[test]
    fn ghost_tool_calls_with_empty_name_are_filtered() {
        let mut parser = OpenAiStreamParser::new();
        // Simulate a ghost entry: index 1 with no name and no args
        parser.tool_calls.insert(1, ("id1".to_string(), "".to_string(), "".to_string()));
        // And a real entry
        parser.tool_calls.insert(0, ("id0".to_string(), "bash".to_string(), r#"{"command":"ls"}"#.to_string()));

        let events = parser.process_chunk(&finish_chunk("tool_calls"));
        let tool_events: Vec<_> = events.iter().filter(|e| matches!(e, ContentEvent::ToolCallComplete { .. })).collect();
        assert_eq!(tool_events.len(), 1, "Ghost tool call should be filtered out");
        if let ContentEvent::ToolCallComplete { tool_name, .. } = &tool_events[0] {
            assert_eq!(tool_name, "bash");
        }
    }

    // ── Capability: o-series thinking support ──

    #[test]
    fn o_series_models_have_thinking_support() {
        for model in ["o1-preview", "o3-mini", "o4-mini"] {
            let caps = OpenAiProvider::capabilities_for_model(model);
            assert!(caps.supports_thinking, "{} should support thinking", model);
        }
    }

    #[test]
    fn standard_models_do_not_support_thinking() {
        for model in ["gpt-4o", "gpt-4-turbo", "Qwen/Qwen3.5-35B", "deepseek-v3"] {
            let caps = OpenAiProvider::capabilities_for_model(model);
            assert!(!caps.supports_thinking, "{} should not support thinking", model);
        }
    }
}
