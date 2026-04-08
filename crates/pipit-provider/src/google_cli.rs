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

pub struct GoogleCliProvider {
    client: Client,
    provider_id: String,
    model: String,
    access_token: String,
    project_id: String,
    base_url: String,
    capabilities: ModelCapabilities,
}

impl GoogleCliProvider {
    pub fn new(
        provider_id: String,
        model: String,
        api_key: String,
        base_url: Option<String>,
    ) -> Result<Self, ProviderError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        let (access_token, project_id) = Self::parse_credentials(&api_key)?;
        let capabilities = Self::capabilities_for_model(&model);

        Ok(Self {
            client,
            provider_id,
            model,
            access_token,
            project_id,
            base_url: base_url.unwrap_or_else(|| "https://cloudcode-pa.googleapis.com".to_string()),
            capabilities,
        })
    }

    fn parse_credentials(api_key: &str) -> Result<(String, String), ProviderError> {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(api_key) {
            let token = value
                .get("token")
                .or_else(|| value.get("access_token"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let project = value
                .get("projectId")
                .or_else(|| value.get("project_id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !token.is_empty() && !project.is_empty() {
                return Ok((token, project));
            }
        }

        if let Some((token, project)) = api_key.split_once("::") {
            if !token.is_empty() && !project.is_empty() {
                return Ok((token.to_string(), project.to_string()));
            }
        }

        let project = std::env::var("GOOGLE_GEMINI_CLI_PROJECT")
            .ok()
            .or_else(|| std::env::var("GOOGLE_CLOUD_PROJECT").ok())
            .or_else(|| std::env::var("GCLOUD_PROJECT").ok())
            .unwrap_or_default();

        if !api_key.is_empty() && !project.is_empty() {
            return Ok((api_key.to_string(), project));
        }

        Err(ProviderError::AuthFailed {
            message: "google_gemini_cli/google_antigravity require token plus project id. Use GOOGLE_GEMINI_CLI_PROJECT or provide `{ \"token\": \"...\", \"projectId\": \"...\" }` as the credential".into(),
        })
    }

    fn capabilities_for_model(model: &str) -> ModelCapabilities {
        let is_flash = model.contains("flash");
        ModelCapabilities {
            context_window: 1_000_000,
            max_output_tokens: if is_flash { 8192 } else { 65536 },
            supports_tool_use: true,
            supports_streaming: true,
            supports_thinking: model.contains("thinking")
                || model.contains("2.5")
                || model.contains("3"),
            supports_images: true,
            supports_prefill: false,
            preferred_edit_format: Some(PreferredFormat::SearchReplace),
        }
    }

    fn is_antigravity(&self) -> bool {
        self.provider_id == "google_antigravity"
    }

    fn build_request_body(&self, request: &CompletionRequest) -> serde_json::Value {
        let mut contents = Vec::new();

        for msg in &request.messages {
            match &msg.role {
                crate::Role::System => continue,
                crate::Role::User => {
                    contents.push(serde_json::json!({
                        "role": "user",
                        "parts": self.convert_parts(&msg.content),
                    }));
                }
                crate::Role::Assistant => {
                    contents.push(serde_json::json!({
                        "role": "model",
                        "parts": self.convert_parts(&msg.content),
                    }));
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
            "project": self.project_id,
            "model": self.model,
            "request": {
                "contents": contents,
            },
            "requestType": "chat",
        });

        if !request.system.is_empty() {
            body["request"]["systemInstruction"] = serde_json::json!({
                "parts": [{"text": request.system}]
            });
        }

        let mut gen_config = serde_json::json!({});
        if let Some(temp) = request.temperature {
            gen_config["temperature"] = serde_json::json!(temp);
        }
        if let Some(max) = request.max_tokens {
            gen_config["maxOutputTokens"] = serde_json::json!(max);
        }
        body["request"]["generationConfig"] = gen_config;

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
            body["request"]["tools"] = serde_json::json!([{"functionDeclarations": functions}]);
            body["request"]["toolConfig"] = serde_json::json!({
                "functionCallingConfig": {"mode": "AUTO"}
            });
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
                    call_id,
                    name,
                    args,
                } => {
                    parts.push(serde_json::json!({
                        "functionCall": {
                            "id": call_id,
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
impl LlmProvider for GoogleCliProvider {
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
        let url = format!("{}/v1internal:streamGenerateContent?alt=sse", self.base_url);

        let mut req = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .header("Accept", "text/event-stream");

        if self.is_antigravity() {
            req = req.header("User-Agent", "antigravity/1.18.4 darwin/arm64");
        } else {
            req = req
                .header("User-Agent", "google-cloud-sdk vscode_cloudshelleditor/0.1")
                .header("X-Goog-Api-Client", "gl-node/22.17.0")
                .header(
                    "Client-Metadata",
                    "{\"ideType\":\"IDE_UNSPECIFIED\",\"platform\":\"PLATFORM_UNSPECIFIED\",\"pluginType\":\"GEMINI\"}",
                );
        }

        let response = req
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

        Ok(Box::pin(GoogleCliEventStream {
            byte_stream: Box::pin(response.bytes_stream()),
            buffer: BytesMut::new(),
            pending_events: VecDeque::new(),
            usage: UsageMetadata::default(),
            cancel,
            finished: false,
        }))
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
            tokens: ((bytes as u64) / 4).max(((words as f64) * 13.0 / 10.0) as u64),
        })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }
}

fn parse_google_cli_chunk(data: &str) -> (Vec<ContentEvent>, Option<UsageMetadata>) {
    let json: serde_json::Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return (vec![], None),
    };

    let mut events = Vec::new();
    let mut usage_out = None;

    if let Some(response) = json.get("response") {
        if let Some(candidates) = response.get("candidates").and_then(|c| c.as_array()) {
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
                                let args =
                                    fc.get("args").cloned().unwrap_or(serde_json::Value::Null);
                                let call_id = fc
                                    .get("id")
                                    .and_then(|id| id.as_str())
                                    .unwrap_or(&name)
                                    .to_string();
                                events.push(ContentEvent::ToolCallComplete {
                                    call_id,
                                    tool_name: name,
                                    args,
                                });
                            }
                        }
                    }
                }

                if let Some(reason) = candidate.get("finishReason").and_then(|r| r.as_str()) {
                    let stop = match reason {
                        "STOP" => StopReason::EndTurn,
                        "MAX_TOKENS" => StopReason::MaxTokens,
                        _ => StopReason::EndTurn,
                    };
                    events.push(ContentEvent::Finished {
                        stop_reason: stop,
                        usage: UsageMetadata::default(),
                    });
                }
            }
        }

        if let Some(meta) = response.get("usageMetadata") {
            usage_out = Some(UsageMetadata {
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
            });
        }
    }

    (events, usage_out)
}

pin_project! {
    struct GoogleCliEventStream {
        #[pin]
        byte_stream: Pin<Box<dyn Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send>>,
        buffer: BytesMut,
        pending_events: VecDeque<ContentEvent>,
        usage: UsageMetadata,
        cancel: CancellationToken,
        finished: bool,
    }
}

impl Stream for GoogleCliEventStream {
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
            let buf_bytes = &this.buffer[..];
            if let Some(pos) = buf_bytes.windows(2).position(|w| w == b"\n\n") {
                let block_bytes = this.buffer.split_to(pos);
                let _ = this.buffer.split_to(2);
                let block = String::from_utf8_lossy(&block_bytes);

                for line in block.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        let data = data.trim();
                        if !data.is_empty() {
                            let (events, usage_update) = parse_google_cli_chunk(data);
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
                std::task::Poll::Ready(Some(Ok(bytes))) => this.buffer.extend_from_slice(&bytes),
                std::task::Poll::Ready(Some(Err(e))) => {
                    *this.finished = true;
                    return std::task::Poll::Ready(Some(Err(ProviderError::Network(
                        e.to_string(),
                    ))));
                }
                std::task::Poll::Ready(None) => {
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
