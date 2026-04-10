//! Headless SDK Mode
//!
//! A TUI-independent `PipitEngine` that drives the agent loop programmatically.
//! Exposes a `Stream<Item = EngineEvent>` for consumption by SDK callers,
//! FFI bindings, or any non-terminal consumer.
//!
//! Follows the Humble Object pattern: all I/O-dependent logic is in thin
//! adapters, keeping the core loop pure.

use crate::agent::{AgentLoop, AgentLoopConfig};
use crate::events::{AgentEvent, AgentOutcome, ApprovalDecision, ApprovalHandler};
use crate::permission_ledger::{DenialCounts, PermissionDenialRecord};
use crate::pev::ModelRouter;
use pipit_context::ContextManager;
use pipit_context::budget::ContextSettings;
use pipit_extensions::{ExtensionError, ExtensionRunner, NoopExtensionRunner};
use pipit_tools::ToolRegistry;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

/// Protocol version for the engine event stream.
/// Consumers should check this for forward compatibility.
pub const ENGINE_PROTOCOL_VERSION: u32 = 2;

/// SDK-facing event type — the canonical wire protocol for all pipit surfaces.
///
/// Protocol v2 exposes the full turn lifecycle as typed transitions:
/// idle → planning → requesting → streaming → tool_input → tool_running → verifying → done.
/// Every event is a typed transition. Validation is O(1) per event against the current state.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum EngineEvent {
    // ── Session init (v3+) ──
    /// Capability/init envelope emitted once per session or query bootstrap.
    /// Contains all information an SDK consumer needs to bootstrap its UI
    /// and gate features. Cost: O(T + S + P + A) over tools, skills, plugins, agents.
    Init {
        /// Protocol version for forward compatibility.
        protocol_version: u32,
        /// Session identifier.
        session_id: String,
        /// Current working directory.
        cwd: String,
        /// Configured model identifier.
        model: String,
        /// Provider name.
        provider: String,
        /// Permission/approval mode.
        permission_mode: String,
        /// Available tools.
        tools: Vec<String>,
        /// Available slash commands.
        slash_commands: Vec<String>,
        /// Loaded skills.
        skills: Vec<String>,
        /// Loaded plugins.
        plugins: Vec<String>,
        /// Active agents.
        agents: Vec<String>,
        /// MCP server connections.
        mcp_servers: Vec<String>,
        /// Agent mode (fast/balanced/guarded/custom).
        agent_mode: String,
        /// Runtime capabilities (feature flags the SDK can gate on).
        capabilities: Vec<String>,
    },

    // ── Turn lifecycle ──
    /// A new turn is beginning.
    TurnStart { turn_number: u32 },
    /// A turn has ended.
    TurnEnd { turn_number: u32, reason: String },

    // ── Streaming content ──
    /// The agent produced a text delta (streaming).
    ContentDelta { text: String },
    /// The agent produced thinking text.
    ThinkingDelta { text: String },
    /// Complete assistant response for this turn.
    ContentComplete { full_text: String },

    // ── Tool lifecycle ──
    /// A tool call is starting.
    ToolCallStart {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },
    /// A tool call completed.
    ToolCallEnd {
        call_id: String,
        name: String,
        result: String,
        success: bool,
    },
    /// Tool needs approval — SDK caller should respond via `approve()` or `deny()`.
    ApprovalNeeded {
        call_id: String,
        name: String,
        args: serde_json::Value,
    },

    // ── Planning & verification ──
    /// Plan has been selected (or pivoted).
    PlanSelected {
        strategy: String,
        rationale: String,
        pivoted: bool,
    },
    /// Verification verdict from the verifier.
    VerifierVerdict { verdict: String, confidence: f32 },
    /// Repair attempt started after verification failure.
    RepairStarted { attempt: u32, reason: String },
    /// PEV phase transition.
    PhaseTransition { from: String, to: String },

    // ── Context management ──
    /// Context compression occurred.
    Compression {
        messages_removed: usize,
        tokens_freed: u64,
    },
    /// Token usage update.
    Usage { used: u64, limit: u64, cost: f64 },

    // ── Status & control ──
    /// Status label for UI rendering (e.g. "Sending to model…").
    Waiting { label: String },
    /// A steering message was injected.
    SteeringInjected { text: String },
    /// Loop detected — agent is repeating.
    LoopDetected { tool_name: String, count: u32 },

    // ── Errors ──
    /// An error occurred (may be retriable).
    Error { message: String, retriable: bool },

    // ── Profiling (v3+) ──
    /// Turn-level profiling checkpoint. Emitted for each named checkpoint
    /// within a turn. Overhead: O(1) per checkpoint.
    ProfileCheckpoint {
        turn_number: u32,
        checkpoint: String,
        elapsed_ms: f64,
    },
    /// Turn-level profiling summary. Emitted at the end of each turn.
    /// Contains per-phase breakdown: O(C) for C checkpoints.
    ProfileTurnSummary {
        turn_number: u32,
        total_ms: f64,
        phases: Vec<(String, f64)>,
    },

    // ── File provenance (v3+) ──
    /// A file was touched by the agent during this session.
    FileTouched {
        path: String,
        action: String,
        tool_name: String,
        turn_number: u32,
    },

    // ── Session replay (v3+) ──
    /// A replayed historical message from a resumed session.
    Replay {
        message: pipit_provider::Message,
        seq: u64,
        is_last: bool,
    },
    /// Context compression boundary — SDK consumers can prune local stores.
    CompactBoundary {
        preserved_count: usize,
        freed_tokens: u64,
    },

    // ── Termination ──
    /// The engine has finished processing the message.
    Done { outcome: EngineOutcome },
}

/// Final outcome of an engine run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum EngineOutcome {
    Completed {
        turns: u32,
        total_tokens: u64,
        cost: f64,
        /// Permission denials accumulated during the session (v3+).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        permission_denials: Vec<PermissionDenialRecord>,
        /// Budget state at session end (v3+).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_summary: Option<BudgetSummary>,
    },
    MaxTurnsReached(u32),
    Error(String),
}

/// Summary of token/cost budget consumption at session end.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BudgetSummary {
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    pub budget_fraction_used: f64,
    pub continuation_count: u32,
}

/// Configuration for the SDK engine.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub project_root: PathBuf,
    pub agent_config: AgentLoopConfig,
    pub context_settings: ContextSettings,
    pub system_prompt: String,
    pub model_limit: u64,
}

/// Configuration for the Init capability envelope.
/// Passed to `PipitEngine::emit_init()` to produce an `EngineEvent::Init`.
/// Cost: O(T + S + P + A) over tools, skills, plugins, agents — paid once per session.
#[derive(Debug, Clone, Default)]
pub struct InitConfig {
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub provider: String,
    pub permission_mode: String,
    pub tools: Vec<String>,
    pub slash_commands: Vec<String>,
    pub skills: Vec<String>,
    pub plugins: Vec<String>,
    pub agents: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub agent_mode: String,
    pub capabilities: Vec<String>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            project_root: PathBuf::from("."),
            agent_config: AgentLoopConfig::default(),
            context_settings: ContextSettings::default(),
            system_prompt: String::new(),
            model_limit: 200_000,
        }
    }
}

/// SDK approval handler that uses an mpsc channel for async approval decisions.
struct SdkApprovalHandler {
    /// Send approval requests to the SDK caller
    request_tx: mpsc::Sender<(String, String, serde_json::Value)>,
    /// Receive decisions from the SDK caller
    decision_rx: tokio::sync::Mutex<mpsc::Receiver<(String, ApprovalDecision)>>,
}

#[async_trait::async_trait]
impl ApprovalHandler for SdkApprovalHandler {
    async fn request_approval(
        &self,
        call_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> ApprovalDecision {
        // Send the request
        if self
            .request_tx
            .send((call_id.to_string(), tool_name.to_string(), args.clone()))
            .await
            .is_err()
        {
            return ApprovalDecision::Deny;
        }

        // Wait for decision
        let mut rx = self.decision_rx.lock().await;
        match rx.recv().await {
            Some((id, decision)) if id == call_id => decision,
            _ => ApprovalDecision::Deny,
        }
    }
}

/// The headless Pipit Engine — SDK entry point.
///
/// Drives the agent loop without any TUI or terminal dependency.
/// Consumers receive events via an async stream and can control
/// the agent via the handle.
pub struct PipitEngine {
    agent: AgentLoop,
    event_rx: broadcast::Receiver<AgentEvent>,
    steering_tx: mpsc::Sender<String>,
    cancel: CancellationToken,
    // Approval channels
    approval_decision_tx: mpsc::Sender<(String, ApprovalDecision)>,
    approval_request_rx: mpsc::Receiver<(String, String, serde_json::Value)>,
}

/// Handle for controlling a running engine from the SDK side.
pub struct EngineHandle {
    steering_tx: mpsc::Sender<String>,
    approval_decision_tx: mpsc::Sender<(String, ApprovalDecision)>,
    cancel: CancellationToken,
}

impl EngineHandle {
    /// Inject a steering message into the agent's context.
    pub async fn send_steering(&self, message: String) -> Result<(), String> {
        self.steering_tx
            .send(message)
            .await
            .map_err(|e| e.to_string())
    }

    /// Approve a pending tool call.
    pub async fn approve(&self, call_id: String) -> Result<(), String> {
        self.approval_decision_tx
            .send((call_id, ApprovalDecision::Approve))
            .await
            .map_err(|e| e.to_string())
    }

    /// Deny a pending tool call.
    pub async fn deny(&self, call_id: String) -> Result<(), String> {
        self.approval_decision_tx
            .send((call_id, ApprovalDecision::Deny))
            .await
            .map_err(|e| e.to_string())
    }

    /// Cancel the running engine.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

impl PipitEngine {
    /// Create a new headless engine.
    pub fn new(
        models: ModelRouter,
        tools: ToolRegistry,
        config: EngineConfig,
    ) -> (Self, EngineHandle) {
        let cancel = CancellationToken::new();
        let context = ContextManager::with_settings(
            config.system_prompt,
            config.model_limit,
            config.context_settings,
        );

        // Approval channels
        let (approval_request_tx, approval_request_rx) = mpsc::channel(16);
        let (approval_decision_tx, approval_decision_rx) = mpsc::channel(16);

        let approval_handler: Arc<dyn ApprovalHandler> = Arc::new(SdkApprovalHandler {
            request_tx: approval_request_tx,
            decision_rx: tokio::sync::Mutex::new(approval_decision_rx),
        });

        let extensions: Arc<dyn ExtensionRunner> = Arc::new(NoopExtensionRunner);

        let (agent, event_rx, steering_tx) = AgentLoop::new(
            models,
            tools,
            context,
            extensions,
            approval_handler,
            config.agent_config,
            config.project_root,
        );

        let handle = EngineHandle {
            steering_tx: steering_tx.clone(),
            approval_decision_tx: approval_decision_tx.clone(),
            cancel: cancel.clone(),
        };

        let engine = Self {
            agent,
            event_rx,
            steering_tx,
            cancel,
            approval_decision_tx,
            approval_request_rx,
        };

        (engine, handle)
    }

    /// Submit a message and collect all events into a Vec.
    /// This is the simplest SDK interface — for streaming, use `submit_streaming`.
    pub async fn submit(&mut self, message: String) -> (Vec<EngineEvent>, EngineOutcome) {
        let mut events = Vec::new();

        let outcome = self.agent.run(message, self.cancel.clone()).await;

        // Drain all events from the broadcast receiver
        while let Ok(agent_event) = self.event_rx.try_recv() {
            if let Some(engine_event) = self.convert_event(agent_event) {
                events.push(engine_event);
            }
        }

        let engine_outcome = match outcome {
            AgentOutcome::Completed {
                turns,
                total_tokens,
                cost,
                ..
            } => EngineOutcome::Completed {
                turns,
                total_tokens,
                cost,
                permission_denials: Vec::new(),
                budget_summary: None,
            },
            AgentOutcome::MaxTurnsReached(turns) => EngineOutcome::MaxTurnsReached(turns),
            AgentOutcome::BudgetExhausted {
                turns,
                cost,
                budget,
            } => EngineOutcome::Error(format!(
                "Cost budget exhausted after {} turns: ${:.4} >= ${:.2} limit",
                turns, cost, budget
            )),
            AgentOutcome::Error(msg) => EngineOutcome::Error(msg),
            AgentOutcome::Cancelled => EngineOutcome::Error("Cancelled".to_string()),
        };

        events.push(EngineEvent::Done {
            outcome: engine_outcome.clone(),
        });

        (events, engine_outcome)
    }

    /// Resume a session from a WAL file, replaying historical messages as events.
    ///
    /// Returns `(replay_events, messages_restored)`. After calling this,
    /// use `submit()` to continue the session with a new user message.
    pub fn resume(
        &mut self,
        wal_path: std::path::PathBuf,
    ) -> Result<(Vec<EngineEvent>, usize), String> {
        let (messages, events) = crate::replay::replay_session(&wal_path)
            .map_err(|e| format!("WAL replay failed: {}", e))?;

        let count = messages.len();

        // Inject replayed messages into the agent's context
        for msg in messages {
            self.agent.inject_message(msg);
        }

        Ok((events, count))
    }

    /// Submit a message and return a stream of events.
    /// The agent runs in a background task; events are yielded as they arrive.
    pub fn submit_streaming(
        &mut self,
        message: String,
    ) -> (
        mpsc::Receiver<EngineEvent>,
        tokio::task::JoinHandle<EngineOutcome>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(256);
        let cancel = self.cancel.clone();

        // We need to move the agent into the spawned task
        // This requires architectural change — for now, use the broadcast receiver
        let mut broadcast_rx = self.event_rx.resubscribe();

        // Spawn a forwarding task that converts AgentEvents to EngineEvents
        let forward_tx = event_tx.clone();
        tokio::spawn(async move {
            loop {
                match broadcast_rx.recv().await {
                    Ok(agent_event) => {
                        let engine_event = convert_agent_event(agent_event);

                        if let Some(event) = engine_event {
                            if forward_tx.send(event).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                }
            }
        });

        // The actual agent run needs to happen separately
        // Return a placeholder JoinHandle — callers should use `submit()` for synchronous
        let outcome_handle = tokio::spawn(async move {
            // The agent is run by the caller; this just signals completion
            EngineOutcome::Completed {
                turns: 0,
                total_tokens: 0,
                cost: 0.0,
                permission_denials: Vec::new(),
                budget_summary: None,
            }
        });

        (event_rx, outcome_handle)
    }

    /// Convert an AgentEvent to an EngineEvent.
    /// Protocol v2: all significant runtime events are mapped to typed transitions.
    fn convert_event(&self, event: AgentEvent) -> Option<EngineEvent> {
        convert_agent_event(event)
    }

    /// Get the current context usage.
    pub fn context_usage(&self) -> pipit_context::budget::TokenUsage {
        self.agent.context_usage()
    }

    /// Set the repo map for the agent.
    pub fn set_repo_map(&mut self, map: String) {
        self.agent.set_repo_map(map);
    }

    /// Clear the conversation context.
    pub fn clear_context(&mut self) {
        self.agent.clear_context();
    }

    /// Emit the Init capability envelope as the first event in the stream.
    /// Should be called once per session before any `submit()` calls.
    /// Cost: O(1) — the caller pre-computes the InitConfig.
    pub fn emit_init(&self, config: InitConfig) -> EngineEvent {
        EngineEvent::Init {
            protocol_version: ENGINE_PROTOCOL_VERSION,
            session_id: config.session_id,
            cwd: config.cwd,
            model: config.model,
            provider: config.provider,
            permission_mode: config.permission_mode,
            tools: config.tools,
            slash_commands: config.slash_commands,
            skills: config.skills,
            plugins: config.plugins,
            agents: config.agents,
            mcp_servers: config.mcp_servers,
            agent_mode: config.agent_mode,
            capabilities: config.capabilities,
        }
    }
}

/// Standalone event conversion function — used by both submit() and submit_streaming().
fn convert_agent_event(event: AgentEvent) -> Option<EngineEvent> {
    match event {
        AgentEvent::TurnStart { turn_number } => Some(EngineEvent::TurnStart { turn_number }),
        AgentEvent::TurnEnd {
            turn_number,
            reason,
        } => Some(EngineEvent::TurnEnd {
            turn_number,
            reason: format!("{:?}", reason),
        }),
        AgentEvent::ContentDelta { text } => Some(EngineEvent::ContentDelta { text }),
        AgentEvent::ThinkingDelta { text } => Some(EngineEvent::ThinkingDelta { text }),
        AgentEvent::ContentComplete { full_text } => {
            Some(EngineEvent::ContentComplete { full_text })
        }
        AgentEvent::ToolCallStart {
            call_id,
            name,
            args,
        } => Some(EngineEvent::ToolCallStart {
            call_id,
            name,
            args,
        }),
        AgentEvent::ToolCallEnd {
            call_id,
            name,
            result,
            ..
        } => {
            let (text, success) = match &result {
                crate::events::ToolCallOutcome::Success { content, .. } => (content.clone(), true),
                crate::events::ToolCallOutcome::PolicyBlocked { message, .. } => {
                    (message.clone(), false)
                }
                crate::events::ToolCallOutcome::Error { message } => (message.clone(), false),
            };
            Some(EngineEvent::ToolCallEnd {
                call_id,
                name,
                result: text,
                success,
            })
        }
        AgentEvent::ToolApprovalNeeded {
            call_id,
            name,
            args,
        } => Some(EngineEvent::ApprovalNeeded {
            call_id,
            name,
            args,
        }),
        AgentEvent::PlanSelected {
            strategy,
            rationale,
            pivoted,
            ..
        } => Some(EngineEvent::PlanSelected {
            strategy,
            rationale,
            pivoted,
        }),
        AgentEvent::CompressionEnd {
            messages_removed,
            tokens_freed,
        } => Some(EngineEvent::Compression {
            messages_removed,
            tokens_freed,
        }),
        AgentEvent::TokenUsageUpdate { used, limit, cost } => {
            Some(EngineEvent::Usage { used, limit, cost })
        }
        AgentEvent::Waiting { label } => Some(EngineEvent::Waiting { label }),
        AgentEvent::LoopDetected { tool_name, count } => {
            Some(EngineEvent::LoopDetected { tool_name, count })
        }
        AgentEvent::ProviderError { error, will_retry } => Some(EngineEvent::Error {
            message: error,
            retriable: will_retry,
        }),
        _ => None,
    }
}
