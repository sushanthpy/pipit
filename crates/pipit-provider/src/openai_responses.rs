//! OpenAI Responses API provider.
//!
//! The Responses API (`/v1/responses`) is OpenAI's newer inference endpoint
//! that supersedes Chat Completions for models like o-series and GPT-4o.
//!
//! Key differences from Chat Completions:
//! - Different endpoint: `/v1/responses` instead of `/v1/chat/completions`
//! - Graduated reasoning levels (none/low/medium/high)
//! - Tool call IDs use `callId|fc_itemId` format
//! - Built-in service tier selection
//! - Prompt caching with 24h retention

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

pub struct OpenAiResponsesProvider {
    client: Client,
    model: String,
    api_key: String,
    base_url: String,
    capabilities: ModelCapabilities,
    /// Reasoning effort level for o-series models.
    reasoning_effort: Option<String>,
}

impl OpenAiResponsesProvider {
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

        // Infer reasoning effort for o-series models
        let reasoning_effort = {
            let lower = model.to_lowercase();
            if lower.starts_with("o1") || lower.starts_with("o3") || lower.starts_with("o4") {
                Some("medium".to_string())
            } else {
                None
            }
        };

        Ok(Self {
            client,
            model,
            api_key,
            base_url: base_url.unwrap_or_else(|| "https://api.openai.com".to_string()),
            capabilities,
            reasoning_effort,
        })
    }

    fn capabilities_for_model(model: &str) -> ModelCapabilities {
        let lower = model.to_lowercase();
        let (context_window, max_output_tokens, supports_thinking) =
            if lower.starts_with("o1") || lower.starts_with("o3") || lower.starts_with("o4") {
                (200_000, 32_768, true)
            } else if lower.contains("gpt-4o") {
                (128_000, 16_384, false)
            } else {
                (128_000, 16_384, false)
            };

        ModelCapabilities {
            context_window,
            max_output_tokens,
            supports_tool_use: true,
            supports_streaming: true,
            supports_thinking,
            supports_images: true,
            supports_prefill: false,
            preferred_edit_format: Some(PreferredFormat::SearchReplace),
        }
    }

    fn build_request_body(&self, request: &CompletionRequest) -> serde_json::Value {
        // The Responses API uses a flat `input` field instead of `messages`
        let mut input = Vec::new();

        // System/instructions go as a top-level field
        let instructions = if !request.system.is_empty() {
            Some(request.system.clone())
        } else {
            None
        };

        // Convert messages to Responses API format
        for msg in &request.messages {
            match &msg.role {
                crate::Role::System => {
                    // Additional system messages merged into instructions
                }
                crate::Role::User => {
                    let text = msg.text_content();
                    let has_images = msg
                        .content
                        .iter()
                        .any(|b| matches!(b, crate::ContentBlock::Image { .. }));

                    if has_images {
                        let mut content = Vec::new();
                        for block in &msg.content {
                            match block {
                                crate::ContentBlock::Text(t) => {
                                    if !t.is_empty() {
                                        content.push(serde_json::json!({
                                            "type": "input_text",
                                            "text": t,
                                        }));
                                    }
                                }
                                crate::ContentBlock::Image { media_type, data } => {
                                    use base64::Engine;
                                    let encoded =
                                        base64::engine::general_purpose::STANDARD.encode(data);
                                    content.push(serde_json::json!({
                                        "type": "input_image",
                                        "image_url": format!("data:{};base64,{}", media_type, encoded),
                                    }));
                                }
                                _ => {}
                            }
                        }
                        input.push(serde_json::json!({
                            "role": "user",
                            "content": content,
                        }));
                    } else {
                        input.push(serde_json::json!({
                            "role": "user",
                            "content": text,
                        }));
                    }
                }
                crate::Role::Assistant => {
                    let text = msg.text_content();
                    let tool_calls = msg.tool_calls();
                    if !text.is_empty() {
                        input.push(serde_json::json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}],
                        }));
                    }
                    for tc in tool_calls {
                        // Responses API tool call format
                        input.push(serde_json::json!({
                            "type": "function_call",
                            "call_id": tc.call_id,
                            "name": tc.tool_name,
                            "arguments": tc.args.to_string(),
                        }));
                    }
                }
                crate::Role::ToolResult { call_id } => {
                    let content = msg.text_content();
                    input.push(serde_json::json!({
                        "type": "function_call_output",
                        "call_id": call_id,
                        "output": content,
                    }));
                }
            }
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "input": input,
            "stream": true,
            "max_output_tokens": request.max_tokens.unwrap_or(self.capabilities.max_output_tokens),
        });

        if let Some(ref instr) = instructions {
            body["instructions"] = serde_json::json!(instr);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        // Reasoning effort for o-series
        if let Some(ref effort) = self.reasoning_effort {
            body["reasoning"] = serde_json::json!({"effort": effort});
        }

        // Tools
        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                        "strict": true,
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tools);
        }

        body
    }
}

#[async_trait]
impl LlmProvider for OpenAiResponsesProvider {
    fn id(&self) -> &str {
        "openai_responses"
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
        let url = format!("{}/v1/responses", self.base_url.trim_end_matches('/'));

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

        let byte_stream = response.bytes_stream();

        Ok(Box::pin(ResponsesStream {
            inner: Box::pin(byte_stream),
            buffer: BytesMut::new(),
            pending: VecDeque::new(),
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

// ── SSE Stream for Responses API ─────────────────────────────────────

pin_project! {
    struct ResponsesStream {
        #[pin]
        inner: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
        buffer: BytesMut,
        pending: VecDeque<ContentEvent>,
        cancel: CancellationToken,
        done: bool,
    }
}

impl Stream for ResponsesStream {
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

        if let Some(event) = this.pending.pop_front() {
            return Poll::Ready(Some(Ok(event)));
        }

        match this.inner.as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(chunk))) => {
                this.buffer.extend_from_slice(&chunk);

                while let Some(pos) = this.buffer.iter().position(|&b| b == b'\n') {
                    let line = this.buffer.split_to(pos + 1);
                    let line_str = String::from_utf8_lossy(&line).trim().to_string();

                    if line_str.starts_with("data: ") {
                        let data = &line_str[6..];
                        if data == "[DONE]" {
                            this.pending.push_back(ContentEvent::Finished {
                                stop_reason: StopReason::EndTurn,
                                usage: UsageMetadata::default(),
                            });
                            *this.done = true;
                            break;
                        }

                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                            let event_type = event["type"].as_str().unwrap_or("");

                            match event_type {
                                // Text output delta
                                "response.output_text.delta" => {
                                    if let Some(text) = event["delta"].as_str() {
                                        this.pending.push_back(ContentEvent::ContentDelta {
                                            text: text.to_string(),
                                        });
                                    }
                                }

                                // Reasoning/thinking delta
                                "response.reasoning_summary_text.delta" => {
                                    if let Some(text) = event["delta"].as_str() {
                                        this.pending.push_back(ContentEvent::ThinkingDelta {
                                            text: text.to_string(),
                                        });
                                    }
                                }

                                // Function call arguments delta
                                "response.function_call_arguments.delta" => {
                                    let call_id = event["call_id"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    let name = event["name"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    let delta = event["delta"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    this.pending.push_back(ContentEvent::ToolCallDelta {
                                        call_id,
                                        tool_name: name,
                                        args_delta: delta,
                                    });
                                }

                                // Function call complete
                                "response.function_call_arguments.done" => {
                                    let call_id = event["call_id"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    let name = event["name"]
                                        .as_str()
                                        .unwrap_or("")
                                        .to_string();
                                    let args_str = event["arguments"]
                                        .as_str()
                                        .unwrap_or("{}");
                                    let args = serde_json::from_str(args_str)
                                        .unwrap_or(serde_json::json!({}));
                                    this.pending.push_back(ContentEvent::ToolCallComplete {
                                        call_id,
                                        tool_name: name,
                                        args,
                                    });
                                }

                                // Response completed
                                "response.completed" => {
                                    let response = &event["response"];
                                    let status = response["status"]
                                        .as_str()
                                        .unwrap_or("completed");

                                    let stop_reason = match status {
                                        "completed" => {
                                            // Check if there were tool calls
                                            if response["output"]
                                                .as_array()
                                                .map(|arr| {
                                                    arr.iter().any(|o| {
                                                        o["type"].as_str()
                                                            == Some("function_call")
                                                    })
                                                })
                                                .unwrap_or(false)
                                            {
                                                StopReason::ToolUse
                                            } else {
                                                StopReason::EndTurn
                                            }
                                        }
                                        "incomplete" => StopReason::MaxTokens,
                                        "failed" => StopReason::Error,
                                        _ => StopReason::EndTurn,
                                    };

                                    let usage = if let Some(u) = response.get("usage") {
                                        UsageMetadata {
                                            input_tokens: u["input_tokens"]
                                                .as_u64()
                                                .unwrap_or(0),
                                            output_tokens: u["output_tokens"]
                                                .as_u64()
                                                .unwrap_or(0),
                                            cache_read_tokens: u["input_tokens_details"]
                                                ["cached_tokens"]
                                                .as_u64(),
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

                                _ => {} // Ignore other event types
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
