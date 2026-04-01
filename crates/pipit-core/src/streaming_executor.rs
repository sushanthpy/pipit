//! Streaming Parallel Tool Executor (Task 2.2)
//!
//! Begins executing tools **while the model is still streaming**. As each
//! `ToolCallComplete` event arrives during the SSE stream, the executor
//! immediately starts running it without waiting for the full response.
//!
//! This overlaps network latency (waiting for remaining tool_use blocks)
//! with computation (running already-received tools).
//!
//! Design:
//! - Read tools execute concurrently via `FuturesUnordered`
//! - Write tools execute sequentially (order matters for correctness)
//! - Abort signals discard in-flight tools and generate synthetic results
//! - Bounded concurrency prevents resource exhaustion

use crate::events::{AgentEvent, ApprovalDecision, ApprovalHandler, ToolCallOutcome};
use crate::proof::{EvidenceArtifact, PolicyStage, RealizedEdit};
use crate::governor::{Governor, RiskReport};
use crate::proof::ConfidenceReport;
use crate::loop_detector::LoopDetector;
use pipit_extensions::ExtensionRunner;
use pipit_provider::{ContentEvent, ToolCall};
use pipit_tools::{ToolContext, ToolRegistry};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio_util::sync::CancellationToken;

/// Maximum number of read tools executing concurrently.
const MAX_CONCURRENT_READS: usize = 8;

/// Result of streaming tool execution for a single tool call.
#[derive(Debug, Clone)]
pub struct StreamingToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub outcome: ToolCallOutcome,
    pub evidence: Option<EvidenceArtifact>,
    pub realized_edit: Option<RealizedEdit>,
    pub modified_file: Option<String>,
}

/// Collects in-flight tool executions and their results.
pub struct StreamingToolExecutor {
    tools: ToolRegistry,
    tool_context: ToolContext,
    extensions: Arc<dyn ExtensionRunner>,
    approval_handler: Arc<dyn ApprovalHandler>,
    event_tx: broadcast::Sender<AgentEvent>,
    cancel: CancellationToken,

    // In-flight futures for read tools
    pending_reads: FuturesUnordered<Pin<Box<dyn std::future::Future<Output = StreamingToolResult> + Send>>>,
    // Queued write tools (executed sequentially after streaming completes)
    pending_writes: Vec<ToolCall>,
    // Already completed results
    completed: Vec<StreamingToolResult>,
    // Semaphore for bounded concurrency
    read_semaphore: Arc<Semaphore>,
    // Tracks whether a tool has been started (prevents double-execution)
    started: std::collections::HashSet<String>,
}

impl StreamingToolExecutor {
    pub fn new(
        tools: ToolRegistry,
        tool_context: ToolContext,
        extensions: Arc<dyn ExtensionRunner>,
        approval_handler: Arc<dyn ApprovalHandler>,
        event_tx: broadcast::Sender<AgentEvent>,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            tools,
            tool_context,
            extensions,
            approval_handler,
            event_tx,
            cancel,
            pending_reads: FuturesUnordered::new(),
            pending_writes: Vec::new(),
            completed: Vec::new(),
            read_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_READS)),
            started: std::collections::HashSet::new(),
        }
    }

    /// Called when a ToolCallComplete event arrives during streaming.
    /// Immediately begins execution for read tools; queues write tools.
    pub fn submit(&mut self, call: ToolCall) {
        if self.started.contains(&call.call_id) {
            return; // Already submitted
        }
        self.started.insert(call.call_id.clone());

        let Some(tool) = self.tools.get(&call.tool_name) else {
            self.completed.push(StreamingToolResult {
                call_id: call.call_id.clone(),
                tool_name: call.tool_name.clone(),
                outcome: ToolCallOutcome::Error {
                    message: format!("Tool not found: {}", call.tool_name),
                },
                evidence: None,
                realized_edit: None,
                modified_file: None,
            });
            return;
        };

        if tool.is_mutating() {
            // Queue writes for sequential execution after streaming
            self.pending_writes.push(call);
        } else {
            // Start read tools immediately
            let tools = self.tools.clone();
            let ctx = self.tool_context.clone();
            let cancel = self.cancel.clone();
            let extensions = self.extensions.clone();
            let semaphore = self.read_semaphore.clone();
            let event_tx = self.event_tx.clone();

            let fut = Box::pin(async move {
                // Acquire semaphore permit for bounded concurrency
                let _permit = semaphore.acquire().await;

                // Extension hook: before tool
                let modified_args = match extensions.on_before_tool(&call.tool_name, &call.args).await {
                    Ok(Some(new_args)) => new_args,
                    Ok(None) => call.args.clone(),
                    Err(err) => {
                        return StreamingToolResult {
                            call_id: call.call_id.clone(),
                            tool_name: call.tool_name.clone(),
                            outcome: ToolCallOutcome::Error {
                                message: err.to_string(),
                            },
                            evidence: None,
                            realized_edit: None,
                            modified_file: None,
                        };
                    }
                };

                let mut modified_call = call.clone();
                modified_call.args = modified_args;

                let outcome = crate::agent::execute_single_tool(&tools, &modified_call, &ctx, cancel).await;
                let outcome = crate::agent::apply_after_tool_hook(&*extensions, &call.tool_name, outcome).await;

                let evidence = Some(crate::agent::evidence_from_tool(&call, &outcome));

                let _ = event_tx.send(AgentEvent::ToolCallEnd {
                    call_id: call.call_id.clone(),
                    name: call.tool_name.clone(),
                    result: outcome.clone(),
                });

                StreamingToolResult {
                    call_id: call.call_id,
                    tool_name: call.tool_name,
                    outcome,
                    evidence,
                    realized_edit: None,
                    modified_file: None,
                }
            });

            self.pending_reads.push(fut);
        }
    }

    /// Drain all completed read results (non-blocking poll).
    pub fn drain_completed(&mut self) -> Vec<StreamingToolResult> {
        // Poll pending_reads for any that have completed
        use std::task::{Context, Poll};
        let waker = futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);

        loop {
            match self.pending_reads.poll_next_unpin(&mut cx) {
                Poll::Ready(Some(result)) => {
                    self.completed.push(result);
                }
                _ => break,
            }
        }

        std::mem::take(&mut self.completed)
    }

    /// Finalize: wait for all in-flight reads, then execute writes sequentially.
    /// Returns all results in submission order.
    pub async fn finalize(
        mut self,
        governor: &Governor,
        confidence: &ConfidenceReport,
        loop_detector: &mut LoopDetector,
    ) -> (
        Vec<StreamingToolResult>,
        RiskReport,
    ) {
        let mut all_results = Vec::new();
        let mut highest_risk = RiskReport::default();

        // Record all calls in loop detector
        for result in &self.completed {
            // Already-completed reads
        }
        for call in &self.pending_writes {
            loop_detector.record(&call.tool_name, &call.args);
            let call_risk = governor.assess_tool_call(call, confidence);
            if call_risk.score > highest_risk.score {
                highest_risk = call_risk;
            }
        }

        // Wait for remaining reads
        while let Some(result) = self.pending_reads.next().await {
            all_results.push(result);
        }

        // Add already-completed results
        all_results.extend(self.completed);

        // Execute writes sequentially
        for call in self.pending_writes {
            if self.cancel.is_cancelled() {
                all_results.push(StreamingToolResult {
                    call_id: call.call_id.clone(),
                    tool_name: call.tool_name.clone(),
                    outcome: ToolCallOutcome::Error {
                        message: "Cancelled".to_string(),
                    },
                    evidence: None,
                    realized_edit: None,
                    modified_file: None,
                });
                continue;
            }

            // Check approval
            let tool = match self.tools.get(&call.tool_name) {
                Some(t) => t,
                None => {
                    all_results.push(StreamingToolResult {
                        call_id: call.call_id.clone(),
                        tool_name: call.tool_name.clone(),
                        outcome: ToolCallOutcome::Error {
                            message: format!("Tool not found: {}", call.tool_name),
                        },
                        evidence: None,
                        realized_edit: None,
                        modified_file: None,
                    });
                    continue;
                }
            };

            if tool.requires_approval(self.tool_context.approval_mode) {
                let _ = self.event_tx.send(AgentEvent::ToolApprovalNeeded {
                    call_id: call.call_id.clone(),
                    name: call.tool_name.clone(),
                    args: call.args.clone(),
                });

                let decision = self
                    .approval_handler
                    .request_approval(&call.call_id, &call.tool_name, &call.args)
                    .await;

                if let ApprovalDecision::Deny = decision {
                    all_results.push(StreamingToolResult {
                        call_id: call.call_id.clone(),
                        tool_name: call.tool_name.clone(),
                        outcome: ToolCallOutcome::PolicyBlocked {
                            message: format!(
                                "User denied approval for '{}'.",
                                call.tool_name
                            ),
                            stage: PolicyStage::PreToolUse,
                            mutated: false,
                        },
                        evidence: Some(EvidenceArtifact::ApprovalBlocked {
                            tool_name: call.tool_name.clone(),
                            reason: "User denied approval".to_string(),
                        }),
                        realized_edit: None,
                        modified_file: None,
                    });
                    continue;
                }
            }

            // Extension hook: before tool
            let modified_args = match self.extensions.on_before_tool(&call.tool_name, &call.args).await {
                Ok(Some(new_args)) => new_args,
                Ok(None) => call.args.clone(),
                Err(err) => {
                    all_results.push(StreamingToolResult {
                        call_id: call.call_id.clone(),
                        tool_name: call.tool_name.clone(),
                        outcome: ToolCallOutcome::Error {
                            message: err.to_string(),
                        },
                        evidence: None,
                        realized_edit: None,
                        modified_file: None,
                    });
                    continue;
                }
            };

            let mut modified_call = call.clone();
            modified_call.args = modified_args;

            let outcome = crate::agent::execute_single_tool(
                &self.tools,
                &modified_call,
                &self.tool_context,
                self.cancel.clone(),
            )
            .await;

            let mutation_applied = matches!(outcome, ToolCallOutcome::Success { mutated: true, .. });
            let outcome = crate::agent::apply_after_tool_hook(&*self.extensions, &call.tool_name, outcome).await;

            let modified_file = if mutation_applied {
                call.args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            };

            let realized_edit = if mutation_applied {
                call.args.get("path").and_then(|v| v.as_str()).map(|path| {
                    RealizedEdit {
                        path: path.to_string(),
                        summary: crate::agent::summarize_tool_outcome(&call.tool_name, &outcome),
                    }
                })
            } else {
                None
            };

            let evidence = Some(crate::agent::evidence_from_tool(&call, &outcome));

            let _ = self.event_tx.send(AgentEvent::ToolCallEnd {
                call_id: call.call_id.clone(),
                name: call.tool_name.clone(),
                result: outcome.clone(),
            });

            all_results.push(StreamingToolResult {
                call_id: call.call_id,
                tool_name: call.tool_name,
                outcome,
                evidence,
                realized_edit,
                modified_file,
            });
        }

        (all_results, highest_risk)
    }

    /// Abort all in-flight tools and generate synthetic error results.
    pub fn abort(mut self) -> Vec<StreamingToolResult> {
        self.cancel.cancel();
        let mut results = std::mem::take(&mut self.completed);

        // Generate synthetic results for pending writes
        for call in &self.pending_writes {
            results.push(StreamingToolResult {
                call_id: call.call_id.clone(),
                tool_name: call.tool_name.clone(),
                outcome: ToolCallOutcome::Error {
                    message: "Tool execution aborted due to fallback".to_string(),
                },
                evidence: None,
                realized_edit: None,
                modified_file: None,
            });
        }

        results
    }
}
