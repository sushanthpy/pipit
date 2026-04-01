//! Tool Use Summary Generation (Task 4.2)
//!
//! Background summarization of tool results using a lightweight model.
//! Runs concurrently during the next API call, hiding latency behind
//! the dominant cost (API response time).

use pipit_provider::{CompletionRequest, ContentEvent, LlmProvider};
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// Maximum output tokens for a tool summary.
const SUMMARY_MAX_TOKENS: u32 = 200;

/// A pending tool summary being generated in the background.
pub struct PendingSummary {
    receiver: oneshot::Receiver<Option<String>>,
}

impl PendingSummary {
    /// Await the summary. Returns None if the summary task was dropped or failed.
    pub async fn await_summary(self) -> Option<String> {
        self.receiver.await.ok().flatten()
    }

    /// Try to get the summary without blocking. Returns None if not ready.
    pub fn try_get(&mut self) -> Option<String> {
        match self.receiver.try_recv() {
            Ok(summary) => summary,
            Err(_) => None,
        }
    }
}

/// The tool summary generator.
pub struct ToolSummaryGenerator {
    /// The lightweight model to use for summarization.
    provider: Arc<dyn LlmProvider>,
}

impl ToolSummaryGenerator {
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self { provider }
    }

    /// Start generating a summary for a tool result in the background.
    /// Returns a PendingSummary that can be awaited later.
    pub fn summarize_in_background(
        &self,
        tool_name: &str,
        tool_result: &str,
        cancel: CancellationToken,
    ) -> PendingSummary {
        let (tx, rx) = oneshot::channel();
        let provider = self.provider.clone();
        let tool_name = tool_name.to_string();
        let tool_result = truncate_for_summary(tool_result, 4000);

        tokio::spawn(async move {
            let request = CompletionRequest {
                system: "Summarize this tool result in one concise line. \
                         Include the key outcome or finding. \
                         Do not include the tool name or metadata."
                    .to_string(),
                messages: vec![pipit_provider::Message::user(&format!(
                    "Tool: {}\nResult:\n{}",
                    tool_name, tool_result
                ))],
                tools: vec![],
                max_tokens: Some(SUMMARY_MAX_TOKENS),
                temperature: Some(0.0),
                stop_sequences: vec![],
            };

            let result = match provider.complete(request, cancel).await {
                Ok(mut stream) => {
                    use futures::StreamExt;
                    let mut text = String::new();
                    while let Some(event) = stream.next().await {
                        if let Ok(ContentEvent::ContentDelta { text: delta }) = event {
                            text.push_str(&delta);
                        }
                    }
                    if text.is_empty() {
                        None
                    } else {
                        Some(text.trim().to_string())
                    }
                }
                Err(_) => None, // Best-effort: don't fail if summary generation fails
            };

            let _ = tx.send(result);
        });

        PendingSummary { receiver: rx }
    }

    /// Generate a summary synchronously (for cases where background isn't suitable).
    pub async fn summarize(
        &self,
        tool_name: &str,
        tool_result: &str,
        cancel: CancellationToken,
    ) -> Option<String> {
        self.summarize_in_background(tool_name, tool_result, cancel)
            .await_summary()
            .await
    }
}

/// Truncate content for summarization input.
fn truncate_for_summary(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }
    let half = max_chars / 2;
    format!(
        "{}\n[...truncated...]\n{}",
        &content[..half],
        &content[content.len() - half..]
    )
}
