//! Faux (fake/mock) LLM provider for testing.
//!
//! Returns scripted responses without hitting any API. Useful for unit tests,
//! integration tests, and benchmarks.
//!
//! # Usage
//! ```ignore
//! let provider = FauxProvider::text("Hello, world!");
//! let provider = FauxProvider::tool_call("read_file", json!({"path": "foo.rs"}));
//! let provider = FauxProvider::sequence(vec![
//!     FauxResponse::text("first turn"),
//!     FauxResponse::tool_call("write_file", json!({"path": "x.rs", "content": "fn main(){}"})),
//! ]);
//! ```

use crate::{
    CompletionRequest, ContentEvent, LlmProvider, ModelCapabilities, PreferredFormat,
    ProviderError, StopReason, TokenCount, UsageMetadata,
};
use async_trait::async_trait;
use futures::stream::{self, Stream};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// A scripted response for the faux provider.
#[derive(Debug, Clone)]
pub enum FauxResponse {
    /// Return text content.
    Text(String),
    /// Return a thinking block followed by text.
    Thinking {
        thinking: String,
        text: String,
    },
    /// Return a tool call.
    ToolCall {
        name: String,
        args: serde_json::Value,
    },
    /// Return multiple tool calls in a single turn.
    MultiToolCall(Vec<(String, serde_json::Value)>),
    /// Simulate an error (stores message string, not ProviderError).
    Error(String),
}

impl FauxResponse {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    pub fn thinking(thinking: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Thinking {
            thinking: thinking.into(),
            text: text.into(),
        }
    }

    pub fn tool_call(name: impl Into<String>, args: serde_json::Value) -> Self {
        Self::ToolCall {
            name: name.into(),
            args,
        }
    }
}

/// Faux LLM provider for deterministic testing.
pub struct FauxProvider {
    responses: Vec<FauxResponse>,
    cursor: AtomicUsize,
    capabilities: ModelCapabilities,
}

impl FauxProvider {
    /// Create a provider that always returns the given text.
    pub fn text(text: impl Into<String>) -> Self {
        Self::sequence(vec![FauxResponse::Text(text.into())])
    }

    /// Create a provider that returns a single tool call.
    pub fn tool_call(name: impl Into<String>, args: serde_json::Value) -> Self {
        Self::sequence(vec![FauxResponse::ToolCall {
            name: name.into(),
            args,
        }])
    }

    /// Create a provider that cycles through a sequence of responses.
    pub fn sequence(responses: Vec<FauxResponse>) -> Self {
        Self {
            responses,
            cursor: AtomicUsize::new(0),
            capabilities: ModelCapabilities {
                context_window: 200_000,
                max_output_tokens: 16_384,
                supports_tool_use: true,
                supports_streaming: true,
                supports_thinking: true,
                supports_images: false,
                supports_prefill: false,
                preferred_edit_format: Some(PreferredFormat::SearchReplace),
            },
        }
    }

    /// Create a provider that always errors.
    pub fn error(err: ProviderError) -> Self {
        Self::sequence(vec![FauxResponse::Error(format!("{err}"))])
    }

    /// How many times `complete()` has been called.
    pub fn call_count(&self) -> usize {
        self.cursor.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl LlmProvider for FauxProvider {
    fn id(&self) -> &str {
        "faux"
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<ContentEvent, ProviderError>> + Send>>,
        ProviderError,
    > {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }

        let idx = self.cursor.fetch_add(1, Ordering::Relaxed);
        let response = if self.responses.is_empty() {
            &FauxResponse::Text("faux response".into())
        } else {
            &self.responses[idx % self.responses.len()]
        };

        let mut events: Vec<Result<ContentEvent, ProviderError>> = Vec::new();

        match response {
            FauxResponse::Text(text) => {
                events.push(Ok(ContentEvent::ContentDelta {
                    text: text.clone(),
                }));
                events.push(Ok(ContentEvent::Finished {
                    stop_reason: StopReason::EndTurn,
                    usage: UsageMetadata {
                        input_tokens: 10,
                        output_tokens: text.len() as u64 / 4,
                        ..Default::default()
                    },
                }));
            }
            FauxResponse::Thinking { thinking, text } => {
                events.push(Ok(ContentEvent::ThinkingDelta {
                    text: thinking.clone(),
                }));
                events.push(Ok(ContentEvent::ContentDelta {
                    text: text.clone(),
                }));
                events.push(Ok(ContentEvent::Finished {
                    stop_reason: StopReason::EndTurn,
                    usage: UsageMetadata {
                        input_tokens: 10,
                        output_tokens: (thinking.len() + text.len()) as u64 / 4,
                        ..Default::default()
                    },
                }));
            }
            FauxResponse::ToolCall { name, args } => {
                let call_id = format!("faux_call_{}", idx);
                events.push(Ok(ContentEvent::ToolCallComplete {
                    call_id,
                    tool_name: name.clone(),
                    args: args.clone(),
                }));
                events.push(Ok(ContentEvent::Finished {
                    stop_reason: StopReason::ToolUse,
                    usage: UsageMetadata {
                        input_tokens: 10,
                        output_tokens: 20,
                        ..Default::default()
                    },
                }));
            }
            FauxResponse::MultiToolCall(calls) => {
                for (i, (name, args)) in calls.iter().enumerate() {
                    let call_id = format!("faux_call_{}_{}", idx, i);
                    events.push(Ok(ContentEvent::ToolCallComplete {
                        call_id,
                        tool_name: name.clone(),
                        args: args.clone(),
                    }));
                }
                events.push(Ok(ContentEvent::Finished {
                    stop_reason: StopReason::ToolUse,
                    usage: UsageMetadata {
                        input_tokens: 10,
                        output_tokens: 20 * calls.len() as u64,
                        ..Default::default()
                    },
                }));
            }
            FauxResponse::Error(msg) => {
                return Err(ProviderError::Other(msg.clone()));
            }
        }

        Ok(Box::pin(stream::iter(events)))
    }

    async fn count_tokens(&self, messages: &[crate::Message]) -> Result<TokenCount, ProviderError> {
        let tokens: u64 = messages.iter().map(|m| m.estimated_tokens()).sum();
        Ok(TokenCount { tokens })
    }

    fn capabilities(&self) -> &ModelCapabilities {
        &self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn faux_text_response() {
        let provider = FauxProvider::text("hello world");
        let cancel = CancellationToken::new();
        let req = CompletionRequest::default();
        let mut stream = provider.complete(req, cancel).await.unwrap();

        let event = stream.next().await.unwrap().unwrap();
        assert!(matches!(event, ContentEvent::ContentDelta { text } if text == "hello world"));

        let event = stream.next().await.unwrap().unwrap();
        assert!(matches!(event, ContentEvent::Finished { stop_reason: StopReason::EndTurn, .. }));
    }

    #[tokio::test]
    async fn faux_tool_call_response() {
        let provider = FauxProvider::tool_call("read_file", serde_json::json!({"path": "foo.rs"}));
        let cancel = CancellationToken::new();
        let req = CompletionRequest::default();
        let mut stream = provider.complete(req, cancel).await.unwrap();

        let event = stream.next().await.unwrap().unwrap();
        match event {
            ContentEvent::ToolCallComplete {
                tool_name, args, ..
            } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(args["path"], "foo.rs");
            }
            other => panic!("expected ToolCallComplete, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn faux_sequence_cycles() {
        let provider = FauxProvider::sequence(vec![
            FauxResponse::text("first"),
            FauxResponse::text("second"),
        ]);
        let cancel = CancellationToken::new();

        // First call
        let mut s = provider
            .complete(CompletionRequest::default(), cancel.clone())
            .await
            .unwrap();
        let event = s.next().await.unwrap().unwrap();
        assert!(matches!(event, ContentEvent::ContentDelta { text } if text == "first"));

        // Second call
        let mut s = provider
            .complete(CompletionRequest::default(), cancel.clone())
            .await
            .unwrap();
        let event = s.next().await.unwrap().unwrap();
        assert!(matches!(event, ContentEvent::ContentDelta { text } if text == "second"));

        // Third call (wraps around to first)
        let mut s = provider
            .complete(CompletionRequest::default(), cancel.clone())
            .await
            .unwrap();
        let event = s.next().await.unwrap().unwrap();
        assert!(matches!(event, ContentEvent::ContentDelta { text } if text == "first"));

        assert_eq!(provider.call_count(), 3);
    }

    #[tokio::test]
    async fn faux_error_response() {
        let provider = FauxProvider::error(ProviderError::AuthFailed {
            message: "test".into(),
        });
        let cancel = CancellationToken::new();
        let result = provider.complete(CompletionRequest::default(), cancel).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn faux_cancelled() {
        let provider = FauxProvider::text("hello");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = provider.complete(CompletionRequest::default(), cancel).await;
        assert!(matches!(result, Err(ProviderError::Cancelled)));
    }

    #[tokio::test]
    async fn faux_thinking_response() {
        let provider = FauxProvider::sequence(vec![FauxResponse::thinking("let me think...", "the answer is 42")]);
        let cancel = CancellationToken::new();
        let mut stream = provider
            .complete(CompletionRequest::default(), cancel)
            .await
            .unwrap();

        let event = stream.next().await.unwrap().unwrap();
        assert!(matches!(event, ContentEvent::ThinkingDelta { text } if text == "let me think..."));

        let event = stream.next().await.unwrap().unwrap();
        assert!(matches!(event, ContentEvent::ContentDelta { text } if text == "the answer is 42"));
    }

    #[test]
    fn faux_capabilities() {
        let provider = FauxProvider::text("test");
        assert_eq!(provider.id(), "faux");
        assert!(provider.capabilities().supports_tool_use);
        assert!(provider.capabilities().supports_thinking);
    }
}
