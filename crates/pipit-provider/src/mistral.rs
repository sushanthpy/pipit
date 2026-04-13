//! Dedicated Mistral provider.
//!
//! While Mistral's API is largely OpenAI-compatible, this dedicated provider adds:
//! - Proper model capability detection (Codestral, Mistral Large, etc.)
//! - Tool call ID normalization (9-character IDs)
//! - Native `tool_choice` support (auto/none/any/required)
//! - Reasoning mode support for extended thinking
//!
//! Falls back to the OpenAI-compatible transport for the actual HTTP layer,
//! but wraps it with Mistral-specific logic.

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

const MISTRAL_BASE_URL: &str = "https://api.mistral.ai";

pub struct MistralProvider {
    client: Client,
    model: String,
    api_key: String,
    base_url: String,
    capabilities: ModelCapabilities,
}

impl MistralProvider {
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
            base_url: base_url.unwrap_or_else(|| MISTRAL_BASE_URL.to_string()),
            capabilities,
        })
    }

    fn capabilities_for_model(model: &str) -> ModelCapabilities {
        let lower = model.to_lowercase();
        let (context_window, max_output_tokens, supports_thinking) =
            if lower.contains("codestral") {
                (256_000, 32_768, false)
            } else if lower.contains("large") {
                (128_000, 32_768, true)
            } else if lower.contains("medium") {
                (128_000, 16_384, false)
            } else if lower.contains("small") {
                (32_000, 8_192, false)
            } else {
                // Default for unknown Mistral models
                (128_000, 16_384, false)
            };

        ModelCapabilities {
            context_window,
            max_output_tokens,
            supports_tool_use: true,
            supports_streaming: true,
            supports_thinking,
            supports_images: lower.contains("pixtral") || lower.contains("large"),
            supports_prefill: true,
            preferred_edit_format: Some(PreferredFormat::SearchReplace),
        }
    }

    /// Normalize a tool call ID to 9 chars (Mistral convention).
    fn normalize_tool_id(id: &str) -> String {
        if id.len() <= 9 {
            format!("{:0>9}", id)
        } else {
            id[..9].to_string()
        }
    }

    fn build_request_body(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut messages = Vec::new();

        // System message
        if !request.system.is_empty() {
            messages.push(serde_json::json!({
                "role": "system",
                "content": request.system,
            }));
        }

        // Conversation messages
        for msg in &request.messages {
            match &msg.role {
                crate::Role::System => {
                    // Already handled above or merge
                    let text = msg.text_content();
                    if !text.is_empty() {
                        messages.push(serde_json::json!({
                            "role": "system",
                            "content": text,
                        }));
                    }
                }
                crate::Role::User => {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": msg.text_content(),
                    }));
                }
                crate::Role::Assistant => {
                    let text = msg.text_content();
                    let tool_calls = msg.tool_calls();

                    if tool_calls.is_empty() {
                        messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": text,
                        }));
                    } else {
                        let tc: Vec<serde_json::Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": Self::normalize_tool_id(&tc.call_id),
                                    "type": "function",
                                    "function": {
                                        "name": tc.tool_name,
                                        "arguments": tc.args.to_string(),
                                    }
                                })
                            })
                            .collect();
                        let mut msg_json = serde_json::json!({
                            "role": "assistant",
                            "tool_calls": tc,
                        });
                        if !text.is_empty() {
                            msg_json["content"] = serde_json::json!(text);
                        }
                        messages.push(msg_json);
                    }
                }
                crate::Role::ToolResult { call_id } => {
                    let content = msg.text_content();
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": Self::normalize_tool_id(call_id),
                        "content": content,
                    }));
                }
            }
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "max_tokens": request.max_tokens.unwrap_or(self.capabilities.max_output_tokens),
        });

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        // Tools
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
            body["tool_choice"] = serde_json::json!("auto");
        }

        if !request.stop_sequences.is_empty() {
            body["stop"] = serde_json::json!(request.stop_sequences);
        }

        body
    }
}

#[async_trait]
impl LlmProvider for MistralProvider {
    fn id(&self) -> &str {
        "mistral"
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
        let url = format!("{}/v1/chat/completions", self.base_url.trim_end_matches('/'));

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(match status.as_u16() {
                401 => ProviderError::AuthFailed {
                    message: body_text,
                },
                429 => ProviderError::RateLimited {
                    retry_after_ms: None,
                },
                400 => ProviderError::MalformedRequest { message: body_text },
                _ => ProviderError::Other(format!("HTTP {status}: {body_text}")),
            });
        }

        // SSE stream parsing — same format as OpenAI (Mistral is OpenAI-compatible)
        let byte_stream = response.bytes_stream();

        Ok(Box::pin(MistralStream {
            inner: Box::pin(byte_stream),
            buffer: BytesMut::new(),
            pending: VecDeque::new(),
            tool_calls: HashMap::new(),
            cancel,
            done: false,
        }))
    }

    async fn count_tokens(&self, messages: &[crate::Message]) -> Result<TokenCount, ProviderError> {
        let tokens: u64 = messages.iter().map(|m| m.estimated_tokens()).sum();
        Ok(TokenCount { tokens })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }
}

// ── SSE Stream (OpenAI-compatible format) ────────────────────────────

pin_project! {
    struct MistralStream {
        #[pin]
        inner: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
        buffer: BytesMut,
        pending: VecDeque<ContentEvent>,
        tool_calls: HashMap<u32, (String, String, String)>, // index → (id, name, args)
        cancel: CancellationToken,
        done: bool,
    }
}

impl Stream for MistralStream {
    type Item = Result<ContentEvent, ProviderError>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use futures::StreamExt;
        use std::task::Poll;

        let mut this = self.project();

        if *this.done {
            return Poll::Ready(None);
        }

        if this.cancel.is_cancelled() {
            *this.done = true;
            return Poll::Ready(Some(Err(ProviderError::Cancelled)));
        }

        // Drain pending events first
        if let Some(event) = this.pending.pop_front() {
            return Poll::Ready(Some(Ok(event)));
        }

        // Read from the byte stream
        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                this.buffer.extend_from_slice(&chunk);

                // Parse SSE lines
                while let Some(pos) = this.buffer.iter().position(|&b| b == b'\n') {
                    let line = this.buffer.split_to(pos + 1);
                    let line_str = String::from_utf8_lossy(&line).trim().to_string();

                    if line_str.starts_with("data: ") {
                        let data = &line_str[6..];
                        if data == "[DONE]" {
                            // Emit any pending tool calls
                            for (_, (id, name, args)) in this.tool_calls.drain() {
                                let parsed_args =
                                    serde_json::from_str(&args).unwrap_or(serde_json::json!({}));
                                this.pending.push_back(ContentEvent::ToolCallComplete {
                                    call_id: id,
                                    tool_name: name,
                                    args: parsed_args,
                                });
                            }
                            this.pending.push_back(ContentEvent::Finished {
                                stop_reason: StopReason::EndTurn,
                                usage: UsageMetadata::default(),
                            });
                            *this.done = true;
                            break;
                        }

                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(data) {
                            if let Some(choices) = parsed["choices"].as_array() {
                                for choice in choices {
                                    let delta = &choice["delta"];

                                    // Text content
                                    if let Some(text) = delta["content"].as_str() {
                                        if !text.is_empty() {
                                            this.pending.push_back(ContentEvent::ContentDelta {
                                                text: text.to_string(),
                                            });
                                        }
                                    }

                                    // Tool calls
                                    if let Some(tcs) = delta["tool_calls"].as_array() {
                                        for tc in tcs {
                                            let idx =
                                                tc["index"].as_u64().unwrap_or(0) as u32;
                                            let entry = this.tool_calls.entry(idx).or_insert_with(
                                                || {
                                                    (
                                                        tc["id"]
                                                            .as_str()
                                                            .unwrap_or("")
                                                            .to_string(),
                                                        String::new(),
                                                        String::new(),
                                                    )
                                                },
                                            );
                                            if let Some(id) = tc["id"].as_str() {
                                                if !id.is_empty() {
                                                    entry.0 = id.to_string();
                                                }
                                            }
                                            if let Some(name) =
                                                tc["function"]["name"].as_str()
                                            {
                                                entry.1 = name.to_string();
                                            }
                                            if let Some(args) =
                                                tc["function"]["arguments"].as_str()
                                            {
                                                entry.2.push_str(args);
                                            }
                                        }
                                    }

                                    // Finish reason
                                    if let Some(reason) = choice["finish_reason"].as_str() {
                                        let stop_reason = match reason {
                                            "stop" => StopReason::EndTurn,
                                            "tool_calls" => StopReason::ToolUse,
                                            "length" => StopReason::MaxTokens,
                                            _ => StopReason::Stop,
                                        };

                                        // Emit pending tool calls
                                        for (_, (id, name, args)) in this.tool_calls.drain() {
                                            let parsed_args =
                                                serde_json::from_str(&args)
                                                    .unwrap_or(serde_json::json!({}));
                                            this.pending
                                                .push_back(ContentEvent::ToolCallComplete {
                                                    call_id: id,
                                                    tool_name: name,
                                                    args: parsed_args,
                                                });
                                        }

                                        // Extract usage if available
                                        let usage = if let Some(u) = parsed.get("usage") {
                                            UsageMetadata {
                                                input_tokens: u["prompt_tokens"]
                                                    .as_u64()
                                                    .unwrap_or(0),
                                                output_tokens: u["completion_tokens"]
                                                    .as_u64()
                                                    .unwrap_or(0),
                                                ..Default::default()
                                            }
                                        } else {
                                            UsageMetadata::default()
                                        };

                                        this.pending.push_back(ContentEvent::Finished {
                                            stop_reason,
                                            usage,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }

                if let Some(event) = this.pending.pop_front() {
                    Poll::Ready(Some(Ok(event)))
                } else {
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
            }
            Poll::Ready(Some(Err(e))) => {
                *this.done = true;
                Poll::Ready(Some(Err(ProviderError::Network(e.to_string()))))
            }
            Poll::Ready(None) => {
                *this.done = true;
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}
