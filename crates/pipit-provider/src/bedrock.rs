//! Amazon Bedrock provider using the ConverseStream API.
//!
//! Authentication: AWS credential chain (env vars, profile, IAM roles).
//! - AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY / AWS_SESSION_TOKEN
//! - AWS_PROFILE → ~/.aws/credentials
//! - ECS task role, EC2 instance role, IRSA
//! - AWS_REGION or AWS_DEFAULT_REGION (defaults to us-east-1)
//!
//! Uses AWS Signature V4 signing on raw HTTP (no SDK dependency).
//!
//! Format: Amazon Bedrock ConverseStream (not OpenAI-compatible).

use crate::{
    CompletionRequest, ContentEvent, LlmProvider, ModelCapabilities, PreferredFormat,
    ProviderError, StopReason, TokenCount, UsageMetadata,
};
use async_trait::async_trait;
use bytes::BytesMut;
use futures::stream::Stream;
use pin_project_lite::pin_project;
use reqwest::Client;
use serde_json::json;
use std::collections::VecDeque;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

/// AWS credentials resolved from the environment.
#[derive(Debug, Clone)]
struct AwsCredentials {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
    region: String,
}

impl AwsCredentials {
    /// Resolve credentials from environment variables.
    fn from_env() -> Result<Self, ProviderError> {
        let access_key_id = std::env::var("AWS_ACCESS_KEY_ID").map_err(|_| {
            ProviderError::AuthFailed {
                message: "AWS_ACCESS_KEY_ID not set. Configure AWS credentials for Bedrock."
                    .into(),
            }
        })?;
        let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY").map_err(|_| {
            ProviderError::AuthFailed {
                message: "AWS_SECRET_ACCESS_KEY not set".into(),
            }
        })?;
        let session_token = std::env::var("AWS_SESSION_TOKEN").ok();
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_string());

        Ok(Self {
            access_key_id,
            secret_access_key,
            session_token,
            region,
        })
    }

    /// Sign a request with AWS Signature V4.
    fn sign_request(
        &self,
        method: &str,
        url: &str,
        headers: &mut Vec<(String, String)>,
        body: &[u8],
        service: &str,
    ) -> Result<(), ProviderError> {
        use sha2::Digest;

        let now = chrono::Utc::now();
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        // Parse URL
        let parsed: url::Url = url
            .parse()
            .map_err(|e| ProviderError::Other(format!("invalid URL: {e}")))?;
        let host = parsed
            .host_str()
            .ok_or_else(|| ProviderError::Other("no host in URL".into()))?;
        let path = parsed.path();

        // Payload hash
        let payload_hash = hex::encode(sha2::Sha256::digest(body));

        // Add required headers
        headers.push(("host".to_string(), host.to_string()));
        headers.push(("x-amz-date".to_string(), amz_date.clone()));
        headers.push((
            "x-amz-content-sha256".to_string(),
            payload_hash.clone(),
        ));
        if let Some(ref token) = self.session_token {
            headers.push(("x-amz-security-token".to_string(), token.clone()));
        }

        // Canonical headers (sorted)
        let mut canonical_headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        canonical_headers.sort_by_key(|(k, _)| k.to_lowercase());

        let canonical_headers_str: String = canonical_headers
            .iter()
            .map(|(k, v)| format!("{}:{}\n", k.to_lowercase(), v.trim()))
            .collect();
        let signed_headers: String = canonical_headers
            .iter()
            .map(|(k, _)| k.to_lowercase())
            .collect::<Vec<_>>()
            .join(";");

        // Canonical request
        let canonical_request = format!(
            "{}\n{}\n{}\n{}\n{}\n{}",
            method,
            path,
            parsed.query().unwrap_or(""),
            canonical_headers_str,
            signed_headers,
            payload_hash
        );

        let canonical_hash = hex::encode(sha2::Sha256::digest(canonical_request.as_bytes()));

        // String to sign
        let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, self.region, service);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date, credential_scope, canonical_hash
        );

        // Signing key
        fn hmac_sha256(key: &[u8], msg: &[u8]) -> Vec<u8> {
            use hmac::{Hmac, Mac};
            type HmacSha256 = Hmac<sha2::Sha256>;
            let mut mac =
                HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
            mac.update(msg);
            mac.finalize().into_bytes().to_vec()
        }

        let k_date = hmac_sha256(
            format!("AWS4{}", self.secret_access_key).as_bytes(),
            date_stamp.as_bytes(),
        );
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, service.as_bytes());
        let k_signing = hmac_sha256(&k_service, b"aws4_request");
        let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        // Authorization header
        let auth_header = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key_id, credential_scope, signed_headers, signature
        );
        headers.push(("authorization".to_string(), auth_header));

        Ok(())
    }
}

pub struct BedrockProvider {
    client: Client,
    model: String,
    credentials: AwsCredentials,
    capabilities: ModelCapabilities,
}

impl BedrockProvider {
    /// Create a new Bedrock provider.
    ///
    /// `api_key` is ignored (uses AWS credential chain).
    /// `base_url` can override the regional endpoint.
    pub fn new(
        model: String,
        _api_key: String,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        let mut credentials = AwsCredentials::from_env()?;

        // Allow base_url to override region endpoint
        if let Some(ref url) = base_url {
            // Extract region from custom endpoint if possible
            if let Some(region) = url
                .split('.')
                .nth(1)
                .filter(|r| r.starts_with("us-") || r.starts_with("eu-") || r.starts_with("ap-"))
            {
                credentials.region = region.to_string();
            }
        }

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let capabilities = Self::capabilities_for_model(&model);

        Ok(Self {
            client,
            model,
            credentials,
            capabilities,
        })
    }

    fn capabilities_for_model(model: &str) -> ModelCapabilities {
        let lower = model.to_lowercase();
        let (context_window, max_output_tokens, supports_thinking) = if lower.contains("claude") {
            if lower.contains("opus") {
                (200_000, 32_000, true)
            } else if lower.contains("sonnet") {
                (200_000, 16_384, true)
            } else {
                (200_000, 8_192, false)
            }
        } else if lower.contains("nova") {
            if lower.contains("pro") {
                (300_000, 16_384, false)
            } else if lower.contains("lite") {
                (300_000, 8_192, false)
            } else {
                (128_000, 8_192, false)
            }
        } else if lower.contains("llama") {
            (128_000, 16_384, false)
        } else if lower.contains("mistral") {
            (128_000, 16_384, false)
        } else {
            (128_000, 8_192, false)
        };

        ModelCapabilities {
            context_window,
            max_output_tokens,
            supports_tool_use: true,
            supports_streaming: true,
            supports_thinking,
            supports_images: lower.contains("claude") || lower.contains("nova"),
            supports_prefill: lower.contains("claude"),
            preferred_edit_format: Some(PreferredFormat::SearchReplace),
        }
    }

    fn build_converse_body(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut messages = Vec::new();

        for msg in &request.messages {
            match &msg.role {
                crate::Role::System => {} // Handled separately
                crate::Role::User => {
                    let mut content = Vec::new();
                    for block in &msg.content {
                        match block {
                            crate::ContentBlock::Text(t) => {
                                content.push(json!({"text": t}));
                            }
                            crate::ContentBlock::Image { media_type, data } => {
                                use base64::Engine;
                                let encoded =
                                    base64::engine::general_purpose::STANDARD.encode(data);
                                let format = media_type
                                    .strip_prefix("image/")
                                    .unwrap_or("jpeg");
                                content.push(json!({
                                    "image": {
                                        "format": format,
                                        "source": {
                                            "bytes": encoded,
                                        }
                                    }
                                }));
                            }
                            _ => {}
                        }
                    }
                    if !content.is_empty() {
                        messages.push(json!({"role": "user", "content": content}));
                    }
                }
                crate::Role::Assistant => {
                    let mut content = Vec::new();
                    let text = msg.text_content();
                    if !text.is_empty() {
                        content.push(json!({"text": text}));
                    }
                    for tc in msg.tool_calls() {
                        content.push(json!({
                            "toolUse": {
                                "toolUseId": tc.call_id,
                                "name": tc.tool_name,
                                "input": tc.args,
                            }
                        }));
                    }
                    if !content.is_empty() {
                        messages.push(json!({"role": "assistant", "content": content}));
                    }
                }
                crate::Role::ToolResult { call_id } => {
                    let result_text = msg.text_content();
                    let is_error = msg.content.iter().any(|b| {
                        matches!(b, crate::ContentBlock::ToolResult { is_error: true, .. })
                    });
                    messages.push(json!({
                        "role": "user",
                        "content": [{
                            "toolResult": {
                                "toolUseId": call_id,
                                "content": [{"text": result_text}],
                                "status": if is_error { "error" } else { "success" },
                            }
                        }]
                    }));
                }
            }
        }

        let mut body = json!({
            "messages": messages,
            "inferenceConfig": {
                "maxTokens": request.max_tokens.unwrap_or(self.capabilities.max_output_tokens),
            }
        });

        // System prompt
        if !request.system.is_empty() {
            body["system"] = json!([{"text": request.system}]);
        }

        if let Some(temp) = request.temperature {
            body["inferenceConfig"]["temperature"] = json!(temp);
        }

        if !request.stop_sequences.is_empty() {
            body["inferenceConfig"]["stopSequences"] = json!(request.stop_sequences);
        }

        // Tools
        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "toolSpec": {
                            "name": t.name,
                            "description": t.description,
                            "inputSchema": {
                                "json": t.input_schema,
                            }
                        }
                    })
                })
                .collect();
            body["toolConfig"] = json!({"tools": tools});
        }

        body
    }

    fn converse_url(&self) -> String {
        format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/converse-stream",
            self.credentials.region,
            urlencoding::encode(&self.model)
        )
    }
}

#[async_trait]
impl LlmProvider for BedrockProvider {
    fn id(&self) -> &str {
        "amazon_bedrock"
    }

    async fn complete(
        &self,
        request: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ContentEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        let body = self.build_converse_body(&request);
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| ProviderError::Other(format!("serialize error: {e}")))?;

        let url = self.converse_url();

        // Sign the request
        let mut sig_headers = Vec::new();
        sig_headers.push(("content-type".to_string(), "application/json".to_string()));
        self.credentials.sign_request(
            "POST",
            &url,
            &mut sig_headers,
            &body_bytes,
            "bedrock",
        )?;

        // Build the reqwest request with signed headers
        let mut req = self
            .client
            .post(&url)
            .body(body_bytes);

        for (key, val) in &sig_headers {
            req = req.header(key.as_str(), val.as_str());
        }

        let response = req
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(match status.as_u16() {
                400 if body_text.contains("ValidationException") => {
                    ProviderError::MalformedRequest {
                        message: body_text,
                    }
                }
                403 => ProviderError::AuthFailed {
                    message: body_text,
                },
                429 => ProviderError::RateLimited {
                    retry_after_ms: None,
                },
                _ => ProviderError::Other(format!("HTTP {status}: {body_text}")),
            });
        }

        // Bedrock returns newline-delimited JSON events
        let byte_stream = response.bytes_stream();

        Ok(Box::pin(BedrockStream {
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

// ── Bedrock Event Stream ─────────────────────────────────────────────

pin_project! {
    struct BedrockStream {
        #[pin]
        inner: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
        buffer: BytesMut,
        pending: VecDeque<ContentEvent>,
        cancel: CancellationToken,
        done: bool,
    }
}

impl Stream for BedrockStream {
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

                // Parse newline-delimited JSON events
                while let Some(pos) = this.buffer.iter().position(|&b| b == b'\n') {
                    let line = this.buffer.split_to(pos + 1);
                    let line_str = String::from_utf8_lossy(&line).trim().to_string();

                    if line_str.is_empty() {
                        continue;
                    }

                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(&line_str) {
                        Self::parse_bedrock_event(event, this.pending);
                    }
                }

                // Also try to parse the buffer as a complete JSON object
                // (Bedrock sometimes sends events without newline separators)
                let buf_str = String::from_utf8_lossy(this.buffer).to_string();
                if !buf_str.trim().is_empty() {
                    if let Ok(event) = serde_json::from_str::<serde_json::Value>(buf_str.trim())
                    {
                        this.buffer.clear();
                        Self::parse_bedrock_event(event, this.pending);
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

impl BedrockStream {
    fn parse_bedrock_event(
        event: serde_json::Value,
        pending: &mut VecDeque<ContentEvent>,
    ) {
        // contentBlockDelta
        if let Some(delta) = event.get("contentBlockDelta") {
            if let Some(d) = delta.get("delta") {
                if let Some(text) = d["text"].as_str() {
                    pending.push_back(ContentEvent::ContentDelta {
                        text: text.to_string(),
                    });
                }
                if let Some(input) = d["toolUse"].as_object() {
                    if let Some(args) = input.get("input") {
                        let args_str = args.to_string();
                        pending.push_back(ContentEvent::ToolCallDelta {
                            call_id: String::new(),
                            tool_name: String::new(),
                            args_delta: args_str,
                        });
                    }
                }
            }
        }

        // contentBlockStart
        if let Some(start) = event.get("contentBlockStart") {
            if let Some(tool_use) = start.get("start").and_then(|s| s.get("toolUse")) {
                let call_id = tool_use["toolUseId"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let name = tool_use["name"].as_str().unwrap_or("").to_string();
                pending.push_back(ContentEvent::ToolCallDelta {
                    call_id,
                    tool_name: name,
                    args_delta: String::new(),
                });
            }
        }

        // contentBlockStop — tool call complete
        if let Some(stop) = event.get("contentBlockStop") {
            // The full tool call is assembled from deltas
            // This is just a marker
        }

        // messageStop
        if let Some(stop) = event.get("messageStop") {
            let reason = stop["stopReason"].as_str().unwrap_or("end_turn");
            let stop_reason = match reason {
                "end_turn" => StopReason::EndTurn,
                "tool_use" => StopReason::ToolUse,
                "max_tokens" => StopReason::MaxTokens,
                "stop_sequence" => StopReason::Stop,
                _ => StopReason::EndTurn,
            };
            pending.push_back(ContentEvent::Finished {
                stop_reason,
                usage: UsageMetadata::default(),
            });
        }

        // metadata (contains usage)
        if let Some(meta) = event.get("metadata") {
            if let Some(usage) = meta.get("usage") {
                // Update the last Finished event with usage info
                let usage_meta = UsageMetadata {
                    input_tokens: usage["inputTokens"].as_u64().unwrap_or(0),
                    output_tokens: usage["outputTokens"].as_u64().unwrap_or(0),
                    cache_read_tokens: usage["cacheReadInputTokenCount"].as_u64(),
                    cache_creation_tokens: usage["cacheWriteInputTokenCount"].as_u64(),
                };
                // If we have a pending Finished event, update it
                if let Some(last) = pending.back_mut() {
                    if let ContentEvent::Finished { usage, .. } = last {
                        *usage = usage_meta;
                    }
                }
            }
        }
    }
}
