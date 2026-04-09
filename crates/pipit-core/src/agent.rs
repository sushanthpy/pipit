use crate::capability::{
    CapabilityRequest, CapabilitySet, ExecutionContext, ExecutionLineage, PolicyDecision,
    PolicyKernel, ResourceScope,
};
use crate::events::{
    AgentEvent, AgentOutcome, ApprovalDecision, ApprovalHandler, ToolCallOutcome, TurnEndReason,
};
use crate::governor::{Governor, RiskReport};
use crate::ledger::{SessionEvent, SessionLedger};
use crate::loop_detector::LoopDetector;
use crate::pev::{ModelRouter, PevConfig};
use crate::planner::{CandidatePlan, PlanStrategy, Planner, VerifyStrategy};
use crate::proof::{
    ChangeClaim, ConfidenceReport, EvidenceArtifact, Objective, PlanPivot, PolicyStage,
    ProofPacket, RealizedEdit, VerificationKind,
};
use crate::session_kernel::{SessionKernel, SessionKernelConfig};
use crate::telemetry_facade::{SpanStatus, SpanValue, TelemetryFacade};
use crate::tool_semantics::{SemanticClass, builtin_semantics, classify_semantically};
use crate::turn_kernel::{TurnInput, TurnKernel, TurnOutput, TurnPhase};
use crate::verifier::Verifier;
use futures::FutureExt;
use futures::StreamExt;
use pipit_config::{ApprovalMode, PricingConfig};
use pipit_context::ContextManager;
use pipit_extensions::ExtensionRunner;
use pipit_provider::{
    AssistantResponse, CompletionRequest, ContentEvent, LlmProvider, Message, StopReason,
};
use pipit_tools::{ToolContext, ToolRegistry};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

/// Configuration for the agent loop.
#[derive(Debug, Clone)]
pub struct AgentLoopConfig {
    pub max_turns: u32,
    pub max_reflections: u32,
    pub loop_detection_window: usize,
    pub loop_detection_threshold: usize,
    pub tool_timeout_secs: u64,
    pub enable_steering: bool,
    pub approval_mode: ApprovalMode,
    pub pricing: PricingConfig,
    /// If set, run this command after any file mutation to verify edits.
    pub test_command: Option<String>,
    /// If set, run this lint command after any file mutation.
    pub lint_command: Option<String>,
    /// PEV (Plan/Execute/Verify) orchestration config.
    /// When Some, the agent uses role-routed model inference.
    pub pev: Option<PevConfig>,
    /// Hard cost ceiling in USD. Before every API call, the estimated cost
    /// is checked against this budget. `None` = unlimited.
    pub max_budget_usd: Option<f64>,
    /// Dry-run mode: read-only tools execute normally, mutating tools
    /// return a preview instead of executing.
    pub dry_run: bool,
    /// Boot context injected into the first user message preamble.
    /// Contains the initial project structure listing for orientation.
    /// Kept out of the system prompt for cache stability.
    pub boot_context: Option<String>,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            max_turns: 100,
            max_reflections: 3,
            loop_detection_window: 10,
            loop_detection_threshold: 3,
            tool_timeout_secs: 120,
            enable_steering: true,
            approval_mode: ApprovalMode::AutoEdit,
            pricing: PricingConfig::default(),
            test_command: None,
            lint_command: None,
            pev: None,
            max_budget_usd: None,
            dry_run: false,
            boot_context: None,
        }
    }
}

/// Evolving proof state — updated incrementally on every mutation.
/// Proof is the runtime state of justified action, not just a terminal artifact.
#[derive(Debug, Clone)]
pub struct ProofState {
    pub objective: Objective,
    pub claim: ChangeClaim,
    pub evidence: Vec<EvidenceArtifact>,
    pub realized_edits: Vec<RealizedEdit>,
    pub risk: RiskReport,
    pub plan_pivots: Vec<PlanPivot>,
    pub selected_plan: CandidatePlan,
    pub candidate_plans: Vec<CandidatePlan>,
}

impl ProofState {
    fn new(
        objective: Objective,
        selected_plan: CandidatePlan,
        candidate_plans: Vec<CandidatePlan>,
    ) -> Self {
        let claim = ChangeClaim::from_objective(objective.clone());
        Self {
            objective,
            claim,
            evidence: Vec::new(),
            realized_edits: Vec::new(),
            risk: RiskReport::default(),
            plan_pivots: Vec::new(),
            selected_plan,
            candidate_plans,
        }
    }

    /// Record a tool execution into the proof state (O(1) append).
    fn record_tool_evidence(&mut self, artifact: EvidenceArtifact) {
        self.evidence.push(artifact);
    }

    /// Record a realized edit (O(1) append).
    fn record_edit(&mut self, edit: RealizedEdit) {
        self.realized_edits.push(edit);
    }

    /// Update risk if new risk is higher.
    fn update_risk(&mut self, new_risk: RiskReport) {
        if new_risk.score > self.risk.score {
            self.risk = new_risk;
        }
    }

    /// Record a plan pivot.
    fn record_pivot(&mut self, pivot: PlanPivot) {
        self.plan_pivots.push(pivot);
    }

    /// Refresh confidence from accumulated evidence.
    fn refresh_confidence(&mut self, verifier: &dyn VerifyStrategy) {
        self.claim.confidence = verifier.summarize_confidence(&self.evidence, &self.realized_edits);
    }

    /// Finalize into a ProofPacket (terminal conversion).
    fn finalize(
        &mut self,
        governor: &Governor,
        verifier: &dyn VerifyStrategy,
        project_root: &std::path::Path,
    ) -> ProofPacket {
        finalize_proof(
            governor,
            verifier,
            self.objective.clone(),
            &mut self.claim,
            self.selected_plan.clone(),
            self.candidate_plans.clone(),
            self.plan_pivots.clone(),
            &self.evidence,
            &self.realized_edits,
            self.risk.clone(),
            project_root,
        )
    }
}

/// The agent loop — the central ~400 lines of the project.
/// Coordinates LLM calls, tool execution, context management.
pub struct AgentLoop {
    models: ModelRouter,
    tools: ToolRegistry,
    context: ContextManager,
    extensions: Arc<dyn ExtensionRunner>,
    approval_handler: Arc<dyn ApprovalHandler>,
    event_tx: broadcast::Sender<AgentEvent>,
    steering_rx: Option<mpsc::Receiver<String>>,
    config: AgentLoopConfig,
    loop_detector: LoopDetector,
    tool_context: ToolContext,
    repo_map: Option<String>,
    planning_state: Option<PlanningState>,
    /// How many consecutive turns the loop detector has fired.
    consecutive_loop_hits: u32,
    /// Centralized permission kernel — single authority for tool authorization.
    policy_kernel: PolicyKernel,
    /// Optional session ledger for durable event sourcing.
    session_ledger: Option<std::sync::Mutex<SessionLedger>>,
    /// Session kernel — single authority for all session state mutations.
    /// When present, ALL mutations flow through the kernel (not ad-hoc).
    session_kernel: Option<std::sync::Mutex<SessionKernel>>,
    /// Turn kernel — pure Mealy machine for turn phase transitions.
    turn_kernel: TurnKernel,
    /// Evolving proof state — updated incrementally on every mutation.
    proof_state: Option<ProofState>,
    /// Telemetry facade — every agent action produces a span.
    telemetry: Arc<TelemetryFacade>,
    /// Closed-loop telemetry controller — feeds profiler signals back into decisions.
    telemetry_controller: crate::query_profiler::TelemetryController,
    /// Command registry for slash command dispatch.
    command_registry: crate::command_registry::CommandRegistry,
    /// Session ID for memory store integration.
    session_id: String,
    /// Optional session memory store — sinks compaction summaries for recall.
    memory_store: Option<Box<dyn pipit_context::MemoryStore>>,
    /// Derived session state for projection injection.
    session_state: Option<crate::ledger::SessionState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningState {
    pub selected_plan: CandidatePlan,
    pub candidate_plans: Vec<CandidatePlan>,
    pub plan_pivots: Vec<PlanPivot>,
}

/// Summary produced by graceful_shutdown().
#[derive(Debug, Clone)]
pub struct ShutdownSummary {
    pub turns: u64,
    pub total_cost: f64,
    pub tokens_used: u64,
    pub tool_calls: u64,
    pub spans_exported: u64,
    pub ledger_flushed: bool,
}

impl AgentLoop {
    pub fn new(
        models: ModelRouter,
        tools: ToolRegistry,
        context: ContextManager,
        extensions: Arc<dyn ExtensionRunner>,
        approval_handler: Arc<dyn ApprovalHandler>,
        config: AgentLoopConfig,
        project_root: PathBuf,
    ) -> (Self, broadcast::Receiver<AgentEvent>, mpsc::Sender<String>) {
        let (event_tx, event_rx) = broadcast::channel(1024);
        let (steering_tx, steering_rx) = mpsc::channel(32);

        let loop_detector = LoopDetector::new(
            config.loop_detection_window,
            config.loop_detection_threshold,
        );

        let tool_context = ToolContext::new(project_root.clone(), config.approval_mode);

        let policy_kernel = PolicyKernel::from_approval_mode(config.approval_mode, project_root);

        let session_id = uuid::Uuid::new_v4().to_string();

        // Populate session lineage on the tool context so subagent
        // transcripts can trace back to the originating session.
        let mut tool_context = tool_context;
        tool_context.session_id = Some(session_id.clone());
        let model_name = models
            .for_role(crate::pev::ModelRole::Executor)
            .model_id
            .clone();
        let provider_name = models
            .for_role(crate::pev::ModelRole::Executor)
            .provider
            .id()
            .to_string();
        let telemetry = Arc::new(TelemetryFacade::new(
            &session_id,
            &model_name,
            &provider_name,
        ));
        let command_registry = crate::command_registry::builtin_registry();
        let max_turns = config.max_turns;

        let agent = Self {
            models,
            tools,
            context,
            extensions,
            approval_handler,
            event_tx,
            steering_rx: Some(steering_rx),
            config,
            loop_detector,
            tool_context,
            repo_map: None,
            planning_state: None,
            consecutive_loop_hits: 0,
            policy_kernel,
            session_ledger: None,
            session_kernel: None,
            turn_kernel: TurnKernel::new(max_turns),
            proof_state: None,
            telemetry,
            telemetry_controller: crate::query_profiler::TelemetryController::new(),
            command_registry,
            session_id,
            memory_store: None,
            session_state: None,
        };

        (agent, event_rx, steering_tx)
    }

    /// Enable durable session logging. All state-changing events will be
    /// appended to the ledger for crash recovery, audit, and replay.
    pub fn enable_session_ledger(&mut self, ledger: SessionLedger) {
        self.session_ledger = Some(std::sync::Mutex::new(ledger));
    }

    /// Enable the session kernel — single authority for all session state.
    /// When enabled, all mutations flow through the kernel, guaranteeing
    /// deterministic replay, hash-chained integrity, and snapshot recovery.
    pub fn enable_session_kernel(&mut self, kernel: SessionKernel) {
        self.session_kernel = Some(std::sync::Mutex::new(kernel));
    }

    /// Enable session memory store for compaction summary persistence.
    pub fn enable_memory_store(&mut self, store: Box<dyn pipit_context::MemoryStore>) {
        self.memory_store = Some(store);
    }

    /// Record a session event to the ledger (if enabled). Non-blocking.
    fn record(&self, event: SessionEvent) {
        // Route through kernel if available (single authority)
        if let Some(ref mtx) = self.session_kernel {
            if let Ok(mut kernel) = mtx.lock() {
                match &event {
                    SessionEvent::UserMessageAccepted { content } => {
                        let _ = kernel.accept_user_message(content);
                    }
                    SessionEvent::AssistantResponseStarted { turn } => {
                        let _ = kernel.begin_response(*turn);
                    }
                    SessionEvent::AssistantResponseCompleted {
                        text,
                        thinking,
                        tokens_used,
                    } => {
                        // Build a message for the WAL so crash recovery can reconstruct
                        let msg = Message::assistant(text);
                        let _ = kernel.complete_response(text, thinking, *tokens_used, &msg);
                    }
                    SessionEvent::ToolCallProposed {
                        call_id,
                        tool_name,
                        args,
                    } => {
                        let _ = kernel.propose_tool_call(call_id, tool_name, args);
                    }
                    SessionEvent::ToolApproved { call_id } => {
                        let _ = kernel.approve_tool(call_id);
                    }
                    SessionEvent::ToolStarted { call_id } => {
                        let _ = kernel.start_tool(call_id);
                    }
                    SessionEvent::ToolCompleted {
                        call_id,
                        success,
                        mutated,
                        result_summary,
                        result_blob_hash,
                    } => {
                        let _ = kernel.complete_tool(
                            call_id,
                            *success,
                            *mutated,
                            result_summary,
                            result_blob_hash.clone(),
                        );
                    }
                    SessionEvent::ContextCompressed {
                        messages_removed,
                        tokens_freed,
                        strategy,
                    } => {
                        let _ =
                            kernel.record_compression(*messages_removed, *tokens_freed, strategy);
                    }
                    SessionEvent::PlanSelected {
                        strategy,
                        rationale,
                    } => {
                        let _ = kernel.select_plan(strategy, rationale);
                    }
                    SessionEvent::PlanPivoted {
                        from_strategy,
                        to_strategy,
                        trigger,
                    } => {
                        let _ = kernel.pivot_plan(from_strategy, to_strategy, trigger);
                    }
                    SessionEvent::TurnCompleted { turn } => {
                        let _ = kernel.gate_turn_committed(*turn);
                    }
                    _ => {
                        // Events not handled by kernel fall through to legacy ledger
                    }
                }
                return;
            }
        }

        // Fall back to legacy ledger if no kernel
        if let Some(ref mtx) = self.session_ledger {
            if let Ok(mut ledger) = mtx.lock() {
                if let Err(e) = ledger.append(event) {
                    tracing::warn!("Ledger append failed: {}", e);
                }
            }
        }
    }

    pub fn set_repo_map(&mut self, map: String) {
        self.repo_map = Some(map);
    }

    /// Get the telemetry facade (for commands like /cost).
    pub fn telemetry(&self) -> &Arc<TelemetryFacade> {
        &self.telemetry
    }

    /// Get the command registry.
    pub fn command_registry(&self) -> &crate::command_registry::CommandRegistry {
        &self.command_registry
    }

    /// Borrow the context manager (for serialization / inspection).
    pub fn context(&self) -> &ContextManager {
        &self.context
    }

    /// Mutable access to the context manager (for resume/hydration).
    pub fn context_mut(&mut self) -> &mut ContextManager {
        &mut self.context
    }

    /// Mutable access to the tool context (for resume cwd restoration).
    pub fn tool_context_mut(&mut self) -> &mut ToolContext {
        &mut self.tool_context
    }

    /// Whether any tool calls have been executed in the current context.
    /// Used to distinguish first-turn Q&A from multi-turn coding tasks.
    fn has_had_tool_calls(&self) -> bool {
        self.context.messages().iter().any(|msg| {
            msg.content
                .iter()
                .any(|block| matches!(block, pipit_provider::ContentBlock::ToolCall { .. }))
        })
    }

    /// Convenience: get the executor provider (the default for the main loop).
    fn provider(&self) -> &Arc<dyn LlmProvider> {
        &self
            .models
            .for_role(crate::pev::ModelRole::Executor)
            .provider
    }

    /// Hot-swap the model at runtime (from /model command).
    /// Creates a new provider with the given model string, keeping the same provider kind and API key.
    pub fn set_model(
        &mut self,
        provider_kind: pipit_config::ProviderKind,
        model: &str,
        api_key: &str,
        base_url: Option<&str>,
    ) -> Result<(), String> {
        let new_provider = pipit_provider::create_provider(provider_kind, model, api_key, base_url)
            .map_err(|e| format!("Failed to create provider for model '{}': {}", model, e))?;
        self.models = crate::ModelRouter::single(Arc::from(new_provider), model.to_string());
        Ok(())
    }

    /// Update the approval mode at runtime (from /permissions command).
    pub fn set_approval_mode(&mut self, mode: ApprovalMode) {
        self.tool_context.approval_mode = mode;
        self.config.approval_mode = mode;
        // Rebuild the policy kernel so the granted capability set matches the new mode.
        self.policy_kernel =
            PolicyKernel::from_approval_mode(mode, self.tool_context.project_root.clone());
    }

    /// Get mutable access to the policy kernel for injecting project-level
    /// constraints (daemon: protected paths, network block, write limits).
    /// All authorization goes through this single oracle.
    pub fn policy_kernel_mut(&mut self) -> &mut PolicyKernel {
        &mut self.policy_kernel
    }

    /// Inject an image into the conversation context as a user message.
    pub fn inject_image(&mut self, media_type: &str, data: Vec<u8>) {
        use pipit_provider::ContentBlock;
        let msg = Message {
            role: pipit_provider::Role::User,
            content: vec![ContentBlock::Image {
                media_type: media_type.to_string(),
                data,
            }],
            metadata: Default::default(),
        };
        self.context.push_message(msg);
    }

    /// Process TurnKernel outputs and emit canonical events.
    /// This is the single-source event derivation point: UI, telemetry,
    /// logs, and replay all stem from TurnKernel phase changes emitted here.
    fn process_turn_outputs(&self, outputs: &[TurnOutput]) {
        for output in outputs {
            if let TurnOutput::PhaseChange(phase) = output {
                let snapshot = self.turn_kernel.snapshot();
                let milestone = snapshot.milestones.last();
                self.emit(AgentEvent::TurnPhaseEntered {
                    turn: snapshot.turn_number,
                    phase: format!("{:?}", phase),
                    detail: milestone.and_then(|m| m.detail.clone()),
                    timestamp_ms: milestone.map(|m| m.timestamp_ms).unwrap_or(0),
                });
            }
        }
    }

    /// Run the agent loop for a single user message.
    pub async fn run(&mut self, user_message: String, cancel: CancellationToken) -> AgentOutcome {
        // Preprocess through extensions
        let processed = match self.extensions.on_input(&user_message).await {
            Ok(Some(modified)) => modified,
            Ok(None) => user_message,
            Err(e) => {
                return AgentOutcome::Error(format!("Extension error: {}", e));
            }
        };

        let objective = Objective::from_prompt(&processed);
        // Record user message to ledger
        self.record(SessionEvent::UserMessageAccepted {
            content: processed.clone(),
        });
        let mut claim = ChangeClaim::from_objective(objective.clone());
        let planner: Box<dyn PlanStrategy> = Box::new(Planner);
        let verifier: Box<dyn VerifyStrategy> = Box::new(Verifier);
        let governor = Governor;
        let mut evidence = Vec::new();
        let mut realized_edits = Vec::new();
        let mut risk = RiskReport::default();
        let mut plan_pivots = Vec::new();

        // Short-circuit planning only for pure Q&A tasks.
        // Questions like "what is this code" don't need strategy selection.
        // But for all coding tasks (including fast mode), the heuristic planner
        // must run — it selects CharacterizationFirst for test-related tasks,
        // MinimalPatch for simple fixes, etc. This is critical for edit quality.
        let is_qa = crate::planner::is_question_task(&processed);

        let mut candidate_plans: Vec<CandidatePlan>;
        let mut selected_plan: CandidatePlan;

        if is_qa {
            // Pure Q&A: trivial plan, no noise
            selected_plan = CandidatePlan {
                strategy: crate::planner::StrategyKind::MinimalPatch,
                rationale: "Direct response.".to_string(),
                expected_value: 1.0,
                estimated_cost: 0.05,
                verification_plan: Vec::new(),
                plan_source: crate::planner::PlanSource::Heuristic,
            };
            candidate_plans = vec![selected_plan.clone()];
        } else {
            // Normal planning flow (heuristic planner for all modes)
            candidate_plans = planner.candidate_plans(&objective, &claim.confidence, &evidence);
            selected_plan = planner.select_plan(&objective, &claim.confidence, &evidence);
        }

        claim.align_with_plan(&selected_plan);
        self.update_planning_state(&selected_plan, &candidate_plans, &plan_pivots);

        // Initialize evolving proof state — updated incrementally, not just at end
        self.proof_state = Some(ProofState::new(
            objective.clone(),
            selected_plan.clone(),
            candidate_plans.clone(),
        ));

        // Only emit plan selected event if this isn't a trivial Q&A
        if !is_qa {
            self.emit_plan_selected(&selected_plan, &candidate_plans, false);
            self.record(SessionEvent::PlanSelected {
                strategy: format!("{:?}", selected_plan.strategy),
                rationale: selected_plan.rationale.clone(),
            });
        }

        // Add user message to context.
        // On the first turn, prepend boot context (project structure) to the user
        // message so the model gets orientation without polluting the system prompt.
        let message_with_context = if self.context.messages().is_empty() {
            if let Some(ref boot) = self.config.boot_context {
                format!("{}\n\n{}", boot, processed)
            } else {
                processed.clone()
            }
        } else {
            processed.clone()
        };
        self.context.push_message(Message::user(&message_with_context));
        // ── Canonical FSM: Idle → Accepted ──
        let outputs = self
            .turn_kernel
            .transition(TurnInput::UserMessage(processed.clone()));
        self.process_turn_outputs(&outputs);
        // ── Canonical FSM: Accepted → ContextFrozen ──
        // Control-plane snapshot: freeze tool registry, policy view, context budget
        // for this turn. Actual snapshot is taken here; per-LLM-call refresh happens
        // inside the loop for MCP reconnections and budget updates.
        let outputs = self.turn_kernel.transition(TurnInput::ContextFrozen);
        self.process_turn_outputs(&outputs);
        self.emit(AgentEvent::TurnStart { turn_number: 0 });

        let mut turn = 0u32;
        /// Track the last turn that produced a file mutation (forward progress).
        let mut last_mutation_turn: u32 = 0;
        /// Consecutive StopReason::Error retries (abort after 3).
        let mut consecutive_error_retries: u32 = 0;
        /// Per-turn MaxTokens continuation counter (abort after 3 continuations per turn).
        let mut max_tokens_continuations: u32 = 0;
        /// Auto-continue counter for EndTurn without tool calls.
        /// Prevents infinite re-prompting when the model keeps ending without tools.
        let mut end_turn_continuations: u32 = 0;

        // ── Adaptive turn budget (replaces dumb counter for extension decisions) ──
        let mut adaptive_budget =
            crate::adaptive_budget::AdaptiveTurnBudget::new(self.config.max_turns);
        /// Track unique files modified across the session.
        let mut unique_files: std::collections::HashSet<String> = std::collections::HashSet::new();

        // ═══════════════════════════════════════════════════════
        // THE LOOP: LLM → tool calls → LLM → tool calls → done
        // ═══════════════════════════════════════════════════════
        loop {
            // Check for steering messages
            if self.config.enable_steering {
                self.drain_steering_messages().await;
            }

            // Check context budget, compress if needed
            if self.context.needs_compression() {
                // Fire PreCompact hook before compressing
                if let Err(e) = self.extensions.on_pre_compact().await {
                    tracing::warn!("PreCompact extension hook failed: {}", e);
                }
                self.emit(AgentEvent::CompressionStart);
                let compress_provider = self.provider().clone();
                let session_id = self.session_id.clone();
                match self
                    .context
                    .compress_pipeline(
                        compress_provider,
                        &session_id,
                        self.memory_store.as_deref(),
                        cancel.clone(),
                    )
                    .await
                {
                    Ok(stats) => {
                        self.emit(AgentEvent::CompressionEnd {
                            messages_removed: stats.messages_removed,
                            tokens_freed: stats.tokens_freed,
                        });
                        // Fire PostCompact hook after successful compression
                        if let Err(e) = self
                            .extensions
                            .on_post_compact(stats.messages_removed, stats.tokens_freed)
                            .await
                        {
                            tracing::warn!("PostCompact extension hook failed: {}", e);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Pipeline compression failed, falling back to monolithic: {}",
                            e
                        );
                        // Fallback to monolithic compressor
                        match self
                            .context
                            .compress(&*self.provider().clone(), cancel.clone())
                            .await
                        {
                            Ok(stats) => {
                                self.emit(AgentEvent::CompressionEnd {
                                    messages_removed: stats.messages_removed,
                                    tokens_freed: stats.tokens_freed,
                                });
                            }
                            Err(e2) => {
                                tracing::warn!("Fallback compression also failed: {}", e2);
                            }
                        }
                    }
                }
            }

            // ── Adaptive turn budget ──
            // Instead of a hard ceiling, the budget extends dynamically based
            // on forward progress (file mutations), idle detection, and
            // completion signals in the model's response.
            turn += 1;

            // Use the adaptive budget for smart decisions
            match adaptive_budget.evaluate(turn) {
                crate::adaptive_budget::TurnBudgetDecision::Continue => {
                    // Under budget — proceed normally
                }
                crate::adaptive_budget::TurnBudgetDecision::WindDown { turns_remaining } => {
                    self.context.push_control_plane(
                        &crate::adaptive_budget::AdaptiveTurnBudget::wind_down_message(
                            turns_remaining,
                        ),
                    );
                    self.emit(AgentEvent::Waiting {
                        label: format!("{} turns remaining", turns_remaining),
                    });
                }
                crate::adaptive_budget::TurnBudgetDecision::Extend {
                    extra_turns,
                    ref reason,
                } => {
                    let is_final =
                        adaptive_budget.extensions_granted >= adaptive_budget.max_auto_extensions;
                    let msg = if is_final {
                        crate::adaptive_budget::AdaptiveTurnBudget::final_extension_message(
                            extra_turns,
                        )
                    } else {
                        crate::adaptive_budget::AdaptiveTurnBudget::extension_message(
                            extra_turns,
                            reason,
                        )
                    };
                    self.context.push_control_plane(&msg);
                    self.emit(AgentEvent::Waiting {
                        label: format!("Budget extended +{} turns ({})", extra_turns, reason),
                    });
                    tracing::info!(
                        turn,
                        extra_turns,
                        reason = reason.as_str(),
                        "Adaptive budget: extension granted",
                    );
                    // Notify UI of new budget so status bar updates
                    self.emit(AgentEvent::BudgetExtended {
                        new_approved: adaptive_budget.approved_budget,
                    });
                }
                crate::adaptive_budget::TurnBudgetDecision::Stop { reason } => {
                    let usage = self.context.token_usage();
                    self.emit(AgentEvent::TokenUsageUpdate {
                        used: usage.total,
                        limit: usage.limit,
                        cost: usage.cost,
                    });
                    self.emit(AgentEvent::Waiting {
                        label: format!("Stopped: {}", reason),
                    });
                    return AgentOutcome::MaxTurnsReached(turn);
                }
            }

            self.emit(AgentEvent::TurnStart { turn_number: turn });

            // Build completion request — show ALL tools to the model.
            // Approval gating happens at execution time, not discovery time.
            let tools = self.tools.declarations();
            if !evidence.is_empty() {
                claim.confidence = verifier.summarize_confidence(&evidence, &realized_edits);
                let previous_plan = selected_plan.clone();
                candidate_plans = planner.candidate_plans(&objective, &claim.confidence, &evidence);
                selected_plan = planner.select_plan(&objective, &claim.confidence, &evidence);
                if selected_plan != previous_plan {
                    let pivot = PlanPivot {
                        turn_number: turn,
                        from: previous_plan.clone(),
                        to: selected_plan.clone(),
                        trigger: format!(
                            "Plan changed after verification evidence update: {}",
                            selected_plan.rationale
                        ),
                    };
                    plan_pivots.push(pivot.clone());
                    // Record pivot in evolving proof state
                    if let Some(ref mut proof) = self.proof_state {
                        proof.record_pivot(pivot);
                        proof.selected_plan = selected_plan.clone();
                        proof.candidate_plans = candidate_plans.clone();
                    }
                    self.emit_plan_selected(&selected_plan, &candidate_plans, true);
                }
                claim.align_with_plan(&selected_plan);
                self.update_planning_state(&selected_plan, &candidate_plans, &plan_pivots);
            }

            // ── Cost budget enforcement ──
            // Before every API call, check if the accumulated cost is nearing the budget.
            if let Some(max_budget) = self.config.max_budget_usd {
                let current_cost = self.context.token_usage().cost;
                if current_cost >= max_budget {
                    self.emit(AgentEvent::Waiting {
                        label: format!(
                            "Cost budget exhausted: ${:.4} >= ${:.2} limit",
                            current_cost, max_budget
                        ),
                    });
                    return AgentOutcome::BudgetExhausted {
                        turns: turn,
                        cost: current_cost,
                        budget: max_budget,
                    };
                }
                // Warn when approaching budget (>90%)
                if current_cost > max_budget * 0.9 {
                    self.emit(AgentEvent::Waiting {
                        label: format!(
                            "Warning: ${:.4} of ${:.2} budget used ({:.0}%)",
                            current_cost,
                            max_budget,
                            current_cost / max_budget * 100.0
                        ),
                    });
                }
            }

            // Stream the LLM response
            let model_id = self
                .models
                .for_role(crate::pev::ModelRole::Executor)
                .model_id
                .clone();

            // Update session state projection before each LLM call
            {
                let usage = self.context.token_usage();
                let state = self
                    .session_state
                    .get_or_insert_with(crate::ledger::SessionState::new);
                state.current_turn = turn;
                state.total_tokens = usage.total;
                state.total_cost = usage.cost;
                state.model = Some(model_id.clone());
            }

            self.emit(AgentEvent::Waiting {
                label: format!("Sending to model ({})\u{2026}", model_id),
            });

            // ── Canonical FSM: → Requesting ──
            let tk = self.turn_kernel.transition(TurnInput::RequestSent);
            self.process_turn_outputs(&tk);

            let mut llm_span = self
                .telemetry
                .start_span("llm.complete")
                .attr(
                    "model.name",
                    SpanValue::String(
                        self.models
                            .for_role(crate::pev::ModelRole::Executor)
                            .model_id
                            .clone(),
                    ),
                )
                .attr("turn", SpanValue::Int(turn as i64));
            let response = match self
                .stream_response_with_recovery(&claim, &selected_plan, &tools, cancel.clone())
                .await
            {
                Ok(r) => {
                    llm_span.finish(SpanStatus::Ok);
                    self.telemetry.record_span(llm_span);
                    r
                }
                Err(e) => {
                    llm_span.finish(SpanStatus::Error);
                    self.telemetry.record_span(llm_span);
                    cancel.cancel(); // Signal all in-flight tools to abort
                    self.emit(AgentEvent::ProviderError {
                        error: e.to_string(),
                        will_retry: false,
                    });
                    return AgentOutcome::Error(e.to_string());
                }
            };

            // ── Canonical FSM: Requesting → ResponseStarted ──
            let tk = self.turn_kernel.transition(TurnInput::StreamStarted);
            self.process_turn_outputs(&tk);

            // Track cost via telemetry facade (Kahan summation for precision)
            if response.stop_reason.is_some() {
                let cost =
                    compute_cost(self.provider().id(), &response.usage, &self.config.pricing);
                self.context.add_cost(cost);
                self.telemetry.session_counters.add_cost(cost);
                self.telemetry
                    .session_counters
                    .add_tokens(response.usage.input_tokens, response.usage.output_tokens);

                // Feed actual API token usage into the context manager for
                // usage-based compaction trigger.  More accurate than char-based
                // estimates — catches estimation drift before context overflow.
                self.context
                    .record_api_usage(response.usage.input_tokens);
            }
            self.telemetry.session_counters.increment_turns();

            // ── Closed-loop telemetry feedback ──
            // Feed turn observations into the controller and check for control signals.
            {
                let ttft_ms = response.ttft_ms;
                let turn_latency_ms = response
                    .ttft_ms
                    .map(|t| {
                        // Use TTFT as lower bound; actual turn latency is from request start
                        // to response completion. We approximate with usage-derived estimate:
                        // output_tokens / ~50 tok/s ≈ generation time in ms, added to TTFT.
                        t + (response.usage.output_tokens as u64 * 20) // ~50 tok/s ≈ 20ms/tok
                    })
                    .unwrap_or(0);
                let tool_calls_this_turn = response.tool_calls.len() as u32;
                self.telemetry_controller.observe_turn(
                    ttft_ms,
                    turn_latency_ms,
                    tool_calls_this_turn,
                );

                let signals = self.telemetry_controller.control_signals();
                if signals.trigger_compaction && !self.context.needs_compression() {
                    tracing::info!(
                        ttft_ema = self.telemetry_controller.ttft_ema_ms(),
                        "Telemetry controller: proactive compaction triggered"
                    );
                    // Proactive eviction of stale results when TTFT is climbing
                    let freed = self.context.evict_stale_tool_results(6);
                    if freed > 0 {
                        self.emit(AgentEvent::Waiting {
                            label: format!("Proactive eviction: freed ~{} tokens", freed),
                        });
                    }
                }
            }

            // Emit content complete
            if !response.text.is_empty() {
                self.emit(AgentEvent::ContentComplete {
                    full_text: response.text.clone(),
                });
            }

            // Record response text for semantic loop detection.
            // The thinking text (or response text if no thinking) is tracked
            // so we can detect when the model repeats the same reasoning.
            let thinking_for_loop = if !response.thinking.is_empty() {
                &response.thinking
            } else {
                &response.text
            };
            self.loop_detector.record_thinking(thinking_for_loop);

            // Drain ephemeral control-plane messages BEFORE adding the new
            // assistant response. These messages were included in the request
            // that just completed; keeping them would contaminate future
            // requests with stale runtime self-talk.
            self.context.drain_ephemeral();

            // Add assistant response to context
            self.context.push_message(response.to_message());

            // Record response completion in the session kernel (closes the
            // begin_response → complete_response lifecycle boundary).
            self.record(SessionEvent::AssistantResponseCompleted {
                text: response.text.clone(),
                thinking: response.thinking.clone(),
                tokens_used: response.usage.output_tokens,
            });

            // Handle stop reason
            match response.stop_reason.unwrap_or(StopReason::EndTurn) {
                StopReason::EndTurn | StopReason::Stop => {
                    // Reset error retry counter on successful response
                    consecutive_error_retries = 0;
                    max_tokens_continuations = 0;
                    // ── Canonical FSM: ResponseStarted → ResponseCompleted ──
                    let tk_outputs = self.turn_kernel.transition(TurnInput::ResponseComplete);
                    self.process_turn_outputs(&tk_outputs);

                    let model_says_done =
                        crate::adaptive_budget::detect_completion_signal(&response.text);

                    // Record turn signals (no tools, response only)
                    adaptive_budget.record_turn(crate::adaptive_budget::TurnSignals {
                        model_signaled_done: model_says_done,
                        ..Default::default()
                    });

                    // ── Auto-continue logic ──
                    // Best-practice agent loops keep running as long as there is
                    // work to do.  Previously we exited immediately on EndTurn,
                    // causing half-finished tasks.
                    //
                    // Now: if the model did NOT explicitly signal completion, AND
                    // it has been actively working recently, we inject a continuation
                    // prompt and keep the loop running.  A counter caps re-prompts
                    // so we don't loop forever on a model that simply refuses to
                    // call tools.

                    // ── Stall-loop detection for EndTurn responses ──
                    // If the model keeps emitting near-identical text-only responses
                    // (e.g. "Now I have enough information, let me write it" × 5)
                    // while interleaving read-only tool calls, the semantic loop
                    // detector catches it here and prevents auto-continue.
                    let is_semantic_stall = self.loop_detector.check_semantic_loop().is_some();
                    if is_semantic_stall {
                        tracing::warn!(
                            turn,
                            end_turn_continuations,
                            "Repetitive EndTurn text detected — breaking stall loop",
                        );
                    }

                    let had_recent_activity = adaptive_budget.had_recent_tool_activity(3);
                    let should_auto_continue = !model_says_done
                        && !is_semantic_stall
                        && had_recent_activity
                        && end_turn_continuations < 3;

                    // On the first turn of a non-QA task, always give the model
                    // one retry if it returned text without tools.  Weaker models
                    // often plan/explain first, then start calling tools on the
                    // next prompt.  Without this, pipit marks it "Completed"
                    // after a single planning response.
                    let is_first_turn_coding = turn <= 1 && !is_qa && end_turn_continuations == 0;

                    if should_auto_continue || is_first_turn_coding {
                        end_turn_continuations += 1;
                        tracing::info!(
                            turn,
                            end_turn_continuations,
                            "EndTurn without completion signal — auto-continuing",
                        );
                        self.emit(AgentEvent::TurnEnd {
                            turn_number: turn,
                            reason: TurnEndReason::Complete,
                        });

                        // ── FSM: commit + reset the current turn before continuing ──
                        // Without this, the kernel stays in ResponseCompleted and the
                        // next turn's inputs (UserAccepted, etc.) hit the catch-all
                        // "invalid transition" branch, generating warn! spam.
                        let tk_outputs = self.turn_kernel.transition(TurnInput::TurnCommitted);
                        self.process_turn_outputs(&tk_outputs);
                        let tk_outputs = self.turn_kernel.transition(TurnInput::Reset);
                        self.process_turn_outputs(&tk_outputs);

                        self.context.push_control_plane(
                            "[SYSTEM] Your response ended without calling any tools. \
                             The task may not be fully complete. Review your progress:\n\
                             - Are all requested changes implemented?\n\
                             - Have you run verification (tests, lint) if applicable?\n\
                             - Is there anything remaining to finish the task?\n\n\
                             If more work is needed, continue using tools. \
                             If the task is truly complete, provide your final summary.",
                        );
                        continue;
                    }

                    // Model signaled done or no recent activity — accept completion.
                    end_turn_continuations = 0;

                    // No tool calls, done
                    self.emit(AgentEvent::TurnEnd {
                        turn_number: turn,
                        reason: TurnEndReason::Complete,
                    });
                    if let Err(e) = self.extensions.on_turn_end(&[]).await {
                        tracing::warn!("TurnEnd extension hook failed: {}", e);
                    }
                    // Fire Stop hook after each agent response
                    if let Err(e) = self.extensions.on_stop().await {
                        tracing::warn!("Stop extension hook failed: {}", e);
                    }

                    let usage = self.context.token_usage();
                    // Use evolving proof state for finalization (not ad-hoc assembly)
                    let proof = if let Some(ref mut ps) = self.proof_state {
                        ps.refresh_confidence(&*verifier);
                        ps.finalize(&governor, &*verifier, &self.tool_context.project_root)
                    } else {
                        finalize_proof(
                            &governor,
                            &*verifier,
                            objective.clone(),
                            &mut claim,
                            selected_plan.clone(),
                            candidate_plans.clone(),
                            plan_pivots.clone(),
                            &evidence,
                            &realized_edits,
                            risk.clone(),
                            &self.tool_context.project_root,
                        )
                    };
                    self.emit(AgentEvent::TokenUsageUpdate {
                        used: usage.total,
                        limit: usage.limit,
                        cost: usage.cost,
                    });

                    // ── Canonical FSM: ResponseCompleted → Committed ──
                    // Kernel-gated commit: turn result is only externally visible
                    // after the kernel records the terminal milestone.
                    self.record(SessionEvent::TurnCompleted { turn });
                    let tk_outputs = self.turn_kernel.transition(TurnInput::TurnCommitted);
                    self.process_turn_outputs(&tk_outputs);

                    return AgentOutcome::Completed {
                        turns: turn,
                        total_tokens: usage.total,
                        cost: usage.cost,
                        proof,
                    };
                }
                StopReason::ToolUse => {
                    // Reset error/continuation counters on successful tool use.
                    // NOTE: end_turn_continuations is NOT reset here — it is
                    // only reset below after we confirm a mutation happened.
                    // Read-only tool calls (file reads, searches) must not
                    // clear the stall counter, otherwise the model can loop
                    // forever alternating reads and text-only EndTurns.
                    consecutive_error_retries = 0;
                    max_tokens_continuations = 0;
                    // ── Canonical FSM: ResponseStarted → ToolProposed ──
                    let tool_calls = response.tool_calls.clone();
                    let tk_outputs = self.turn_kernel.transition(TurnInput::ToolCallsReceived {
                        call_count: tool_calls.len(),
                    });
                    self.process_turn_outputs(&tk_outputs);

                    // Execute tool calls with turn-level timeout
                    let turn_timeout = std::time::Duration::from_secs(
                        self.config.tool_timeout_secs.max(30) * (tool_calls.len() as u64).max(1),
                    );
                    let tool_future = self.execute_tools(
                        &tool_calls,
                        cancel.clone(),
                        &governor,
                        &claim.confidence,
                        turn,
                    );
                    let (results, modified_files, artifacts, edits, tool_risk) = tokio::select! {
                        result = tool_future => result,
                        _ = tokio::time::sleep(turn_timeout) => {
                            self.emit(AgentEvent::ProviderError {
                                error: format!("Tool execution timed out after {}s", turn_timeout.as_secs()),
                                will_retry: false,
                            });
                            // Push timeout error as tool results so the agent knows what happened.
                            // Only push for tools that haven't already completed — the
                            // execute_tools future may have finished some calls before the
                            // turn-level timeout fired.
                            for call in &tool_calls {
                                self.context.push_tool_result(
                                    &call.call_id,
                                    &format!("TIMEOUT: Tool execution exceeded {}s. The command may still be running. Try a different approach or break the task into smaller steps.", turn_timeout.as_secs()),
                                    true,
                                );
                            }
                            self.emit(AgentEvent::TurnEnd {
                                turn_number: turn,
                                reason: TurnEndReason::Error,
                            });
                            // Record timeout signals so adaptive budget sees tool activity
                            adaptive_budget.record_turn(crate::adaptive_budget::TurnSignals {
                                tool_calls: tool_calls.len() as u32,
                                had_error: true,
                                ..Default::default()
                            });
                            // Don't increment turn — the loop header does it
                            continue;
                        }
                    };
                    let new_artifact_count = artifacts.len();
                    let new_edit_count = edits.len();
                    evidence.extend(artifacts);
                    realized_edits.extend(edits);
                    if tool_risk.score > risk.score {
                        risk = tool_risk.clone();
                    }

                    // Update evolving proof state incrementally (not just at finalization)
                    if let Some(ref mut proof) = self.proof_state {
                        for artifact in &evidence[evidence.len() - new_artifact_count..] {
                            proof.record_tool_evidence(artifact.clone());
                        }
                        for edit in &realized_edits[realized_edits.len() - new_edit_count..] {
                            proof.record_edit(edit.clone());
                        }
                        proof.update_risk(tool_risk);
                    }

                    // ── Canonical FSM: ToolStarted → ToolCompleted ──
                    let tk_outputs = self.turn_kernel.transition(TurnInput::AllToolsCompleted {
                        modified_files: modified_files.clone(),
                    });
                    self.process_turn_outputs(&tk_outputs);

                    // Track failures/successes for loop detection
                    let mut had_mutation_success = false;
                    for (call_id, outcome) in &results {
                        if let Some(call) = tool_calls.iter().find(|c| c.call_id == *call_id) {
                            match outcome {
                                ToolCallOutcome::Error { .. } => {
                                    self.loop_detector.mark_last_failed(&call.tool_name);
                                }
                                ToolCallOutcome::PolicyBlocked { .. } => {
                                    self.loop_detector.mark_last_failed(&call.tool_name);
                                }
                                ToolCallOutcome::Success { mutated, .. } => {
                                    if *mutated {
                                        had_mutation_success = true;
                                    }
                                }
                            }
                        }
                    }

                    // Reset loop detector when forward progress is made
                    if had_mutation_success {
                        self.loop_detector.reset();
                        self.consecutive_loop_hits = 0;
                        end_turn_continuations = 0;
                        last_mutation_turn = turn;
                    }

                    // ── Record turn signals for adaptive budget ──
                    for f in &modified_files {
                        unique_files.insert(f.clone());
                        // Update session state projection with modified files
                        if let Some(ref mut state) = self.session_state {
                            if !state.modified_files.contains(f) {
                                state.modified_files.push(f.clone());
                            }
                        }
                    }
                    adaptive_budget.record_turn(crate::adaptive_budget::TurnSignals {
                        files_mutated: modified_files.len() as u32,
                        tool_calls: tool_calls.len() as u32,
                        had_error: results
                            .iter()
                            .any(|(_, r)| matches!(r, ToolCallOutcome::Error { .. })),
                        total_files_mutated: realized_edits.len() as u32,
                        unique_files_modified: unique_files.len() as u32,
                        idle_turns: if had_mutation_success {
                            0
                        } else {
                            turn.saturating_sub(last_mutation_turn)
                        },
                        // Don't trust completion signals when the model is actively using tools.
                        // The model says "done" while calling tools = unreliable signal.
                        model_signaled_done: false,
                        verification_passed: false, // updated after verification
                    });

                    // Check for loops
                    if let Some((name, count)) = self.loop_detector.is_looping() {
                        self.consecutive_loop_hits += 1;
                        self.emit(AgentEvent::LoopDetected {
                            tool_name: name.clone(),
                            count,
                        });

                        // Escalation: after 5 consecutive loop-detected turns, abort
                        if self.consecutive_loop_hits >= 5 {
                            self.emit(AgentEvent::TurnEnd {
                                turn_number: turn,
                                reason: TurnEndReason::Error,
                            });
                            return AgentOutcome::Error(format!(
                                "Agent stuck in a loop: {} was repeated {} times across {} consecutive turns. \
                                 Aborting to avoid wasting tokens. Try rephrasing your request or breaking it into smaller steps.",
                                name, count, self.consecutive_loop_hits
                            ));
                        }

                        self.context
                            .push_control_plane(&build_loop_recovery_message(
                                &name,
                                count,
                                &evidence,
                                self.consecutive_loop_hits,
                            ));
                    } else {
                        // Reset counter when no loop is detected
                        self.consecutive_loop_hits = 0;
                    }

                    // ── Semantic loop detection ──
                    // Even if tool args differ, check if the model's reasoning text
                    // is repetitive (e.g. "Let me try a different approach" × 4).
                    if let Some(similar_count) = self.loop_detector.check_semantic_loop() {
                        self.consecutive_loop_hits += 1;
                        self.emit(AgentEvent::LoopDetected {
                            tool_name: "semantic_loop".to_string(),
                            count: similar_count,
                        });

                        if self.consecutive_loop_hits >= 3 {
                            self.emit(AgentEvent::TurnEnd {
                                turn_number: turn,
                                reason: TurnEndReason::Error,
                            });
                            return AgentOutcome::Error(
                                "Agent stuck in a semantic loop: reasoning text is >70% similar across \
                                 recent turns despite varying tool arguments. The model is repeating \
                                 the same approach without progress. Try rephrasing your request or \
                                 breaking it into smaller steps.".to_string(),
                            );
                        }

                        self.context.push_control_plane(
                            "[SYSTEM] Your reasoning is very similar to your previous turns. \
                             You appear to be stuck in a loop. STOP repeating the same approach. \
                             Try a fundamentally different strategy:\n\
                             - Use a different tool entirely\n\
                             - Read the file first to understand the current state\n\
                             - Ask the user for clarification\n\
                             - If the task cannot be completed, explain why and stop",
                        );
                    }

                    // Push tool results to context in the ORIGINAL call order.
                    // The scheduler may reorder calls (reads before writes, batched),
                    // but the LLM expects results in the same order it emitted tool_use blocks.
                    let results_map: std::collections::HashMap<String, ToolCallOutcome> =
                        results.into_iter().collect();
                    for call in &tool_calls {
                        let (content, is_error) =
                            if let Some(result) = results_map.get(&call.call_id) {
                                match result {
                                    ToolCallOutcome::Success { content, .. } => {
                                        (content.clone(), false)
                                    }
                                    ToolCallOutcome::PolicyBlocked { message, .. } => {
                                        (message.clone(), true)
                                    }
                                    ToolCallOutcome::Error { message } => (message.clone(), true),
                                }
                            } else {
                                // Tool was dispatched but no result came back — shouldn't happen,
                                // but defensively report rather than silently skip.
                                (
                                    format!(
                                        "Internal error: no result received for tool '{}'",
                                        call.tool_name
                                    ),
                                    true,
                                )
                            };
                        self.context
                            .push_tool_result(&call.call_id, &content, is_error);
                    }

                    self.emit(AgentEvent::TurnEnd {
                        turn_number: turn,
                        reason: TurnEndReason::ToolsExecuted,
                    });
                    if let Err(e) = self.extensions.on_turn_end(&modified_files).await {
                        tracing::warn!("TurnEnd extension hook failed: {}", e);
                    }

                    // Post-edit verification: auto-run lint/test if files were mutated.
                    //
                    // Skip verification when:
                    // 1. No lint/test commands are configured (greenfield projects)
                    // 2. Only config/metadata files were changed (no code to verify)
                    // 3. Fewer than 3 code files exist in the session (still bootstrapping)
                    //
                    // This avoids the throughput penalty of running verification on every
                    // write turn during project scaffolding, where lint/test either don't
                    // exist yet or would produce noise on incomplete code.
                    let has_verification_commands =
                        self.config.lint_command.is_some() || self.config.test_command.is_some();
                    let only_config_files = modified_files.iter().all(|f| {
                        let fl = f.to_lowercase();
                        fl.ends_with(".json")
                            || fl.ends_with(".toml")
                            || fl.ends_with(".yaml")
                            || fl.ends_with(".yml")
                            || fl.ends_with(".env")
                            || fl.ends_with(".env.local")
                            || fl.ends_with(".env.example")
                            || fl.ends_with(".gitignore")
                            || fl.ends_with(".md")
                            || fl.ends_with(".lock")
                            || fl.ends_with(".config.js")
                            || fl.ends_with(".config.ts")
                            || fl.ends_with("tsconfig.json")
                            || fl.ends_with("package.json")
                    });
                    let should_verify = has_verification_commands
                        && !modified_files.is_empty()
                        && !only_config_files;

                    if should_verify {
                        self.emit(AgentEvent::Waiting {
                            label: "Running verification\u{2026}".to_string(),
                        });
                        let verification_results = self
                            .run_post_edit_verification(&modified_files, cancel.clone())
                            .await;
                        let all_passed = verification_results.iter().all(|(_, _, s)| *s);
                        for (cmd, output, success) in &verification_results {
                            evidence.push(EvidenceArtifact::CommandResult {
                                kind: classify_verification_command(cmd),
                                command: cmd.clone(),
                                output: truncate(output, 500),
                                success: *success,
                            });
                        }
                        // Feed verification result back to adaptive budget so it
                        // counts "verification passed + idle" as a stop signal.
                        if let Some(last_signals) = adaptive_budget.turn_history.last_mut() {
                            last_signals.verification_passed = all_passed;
                        }
                        // If verification failed, inject the failure into context
                        // so the agent sees it and can fix the issue
                        for (cmd, output, success) in &verification_results {
                            if !success {
                                self.context.push_control_plane(&format!(
                                    "[Auto-verification failed]\n$ {}\n{}",
                                    cmd,
                                    truncate(output, 1000)
                                ));
                            }
                        }
                    }

                    // Continue the loop — commit + reset the FSM so the next
                    // turn starts from Idle phase (prevents "invalid transition"
                    // warnings when the next turn sends UserMessage etc.)
                    {
                        let tk_outputs = self.turn_kernel.transition(TurnInput::TurnCommitted);
                        self.process_turn_outputs(&tk_outputs);
                        let tk_outputs = self.turn_kernel.transition(TurnInput::Reset);
                        self.process_turn_outputs(&tk_outputs);
                    }
                    self.emit(AgentEvent::Waiting {
                        label: "Preparing next turn\u{2026}".to_string(),
                    });
                }
                StopReason::MaxTokens => {
                    // Per-turn continuation budget: prevent infinite MaxTokens loops
                    // where the model keeps generating the same truncated response.
                    max_tokens_continuations += 1;
                    if max_tokens_continuations > 3 {
                        self.emit(AgentEvent::TurnEnd {
                            turn_number: turn,
                            reason: TurnEndReason::Complete,
                        });
                        let usage = self.context.token_usage();
                        let proof = if let Some(ref mut ps) = self.proof_state {
                            ps.refresh_confidence(&*verifier);
                            ps.finalize(&governor, &*verifier, &self.tool_context.project_root)
                        } else {
                            finalize_proof(
                                &governor,
                                &*verifier,
                                objective.clone(),
                                &mut claim,
                                selected_plan.clone(),
                                candidate_plans.clone(),
                                plan_pivots.clone(),
                                &evidence,
                                &realized_edits,
                                risk.clone(),
                                &self.tool_context.project_root,
                            )
                        };
                        return AgentOutcome::Completed {
                            turns: turn,
                            total_tokens: usage.total,
                            cost: usage.cost,
                            proof,
                        };
                    }

                    if self.provider().capabilities().supports_prefill && !response.text.is_empty()
                    {
                        // Append partial text as assistant prefill and loop again
                        self.context.push_control_plane(
                            "Continue from where you left off. Your previous response was truncated."
                        );
                        // Continue the loop to get more output
                    } else {
                        // No prefill support: compact context to free space, then retry.
                        // Reducing context on MaxTokens is better than giving up.
                        self.emit(AgentEvent::Waiting {
                            label: "Output truncated — compacting context and retrying…"
                                .to_string(),
                        });
                        // Remove the truncated response — the model will regenerate
                        self.context.pop_last_assistant();
                        // Try to free space
                        let freed = self.context.evict_stale_tool_results(6);
                        if freed > 0 {
                            self.emit(AgentEvent::Waiting {
                                label: format!("Freed ~{} tokens from stale tool results", freed),
                            });
                        }
                        let freed2 = self.context.truncate_large_results(4000);
                        if freed2 > 0 {
                            self.emit(AgentEvent::Waiting {
                                label: format!("Freed ~{} tokens from large results", freed2),
                            });
                        }
                        // Inject a nudge so the model knows to be more concise
                        self.context.push_control_plane(
                            "[SYSTEM] Your previous response was truncated due to output length. \
                             Please be more concise. Focus on the most important action and \
                             use tool calls instead of explaining what you would do.",
                        );
                        continue;
                    }
                }
                StopReason::Error => {
                    // Retry transient LLM errors with exponential backoff.
                    // The model returned an error stop_reason but the connection
                    // itself succeeded — this is often a transient backend issue.
                    consecutive_error_retries += 1;
                    if consecutive_error_retries > 3 {
                        return AgentOutcome::Error(
                            "LLM returned error stop reason 3 times consecutively. Aborting."
                                .to_string(),
                        );
                    }
                    let delay_ms = 500 * 2u64.pow(consecutive_error_retries - 1);
                    self.emit(AgentEvent::ProviderError {
                        error: format!(
                            "LLM returned error stop reason — retrying in {}ms (attempt {}/3)",
                            delay_ms, consecutive_error_retries
                        ),
                        will_retry: true,
                    });
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    // Remove the failed assistant message from context so the
                    // model doesn't see its own error response as history.
                    self.context.pop_last_assistant();
                    // Don't increment turn — this is a retry, not a new turn.
                    continue;
                }
            }
        }
    }

    /// Stream an LLM response, forwarding events to subscribers.
    async fn stream_response(
        &self,
        request: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<AssistantResponse, pipit_provider::ProviderError> {
        let mut stream = self
            .provider()
            .complete(request.clone(), cancel.clone())
            .await?;
        let mut response = AssistantResponse::new();
        let mut has_tool_calls = false;
        let request_start = std::time::Instant::now();
        let mut first_token_recorded = false;

        // TTFT timeout: if no data arrives for 60 seconds, abort.
        // This prevents indefinite hangs when the model server accepts
        // the connection but never sends tokens (overloaded, crashed, etc.).
        let chunk_timeout = std::time::Duration::from_secs(60);
        let mut deadline = tokio::time::Instant::now() + chunk_timeout;

        loop {
            let event = tokio::select! {
                event = stream.next() => event,
                _ = tokio::time::sleep_until(deadline) => {
                    tracing::warn!(
                        "No data received for {}s — aborting stream",
                        chunk_timeout.as_secs()
                    );
                    // If we have partial content, salvage it
                    if !response.text.is_empty() || has_tool_calls {
                        response.finish(
                            pipit_provider::StopReason::EndTurn,
                            pipit_provider::UsageMetadata::default(),
                        );
                        return Ok(response);
                    }
                    return Err(pipit_provider::ProviderError::Network(
                        format!(
                            "No data received for {}s. Model may be overloaded or unresponsive.",
                            chunk_timeout.as_secs()
                        ),
                    ));
                }
            };

            let event = match event {
                Some(e) => e,
                None => break,
            };

            // Reset the deadline on every received chunk
            deadline = tokio::time::Instant::now() + chunk_timeout;

            if cancel.is_cancelled() {
                return Err(pipit_provider::ProviderError::Cancelled);
            }

            match event {
                Ok(ContentEvent::ContentDelta { text }) => {
                    if !first_token_recorded {
                        response.ttft_ms = Some(request_start.elapsed().as_millis() as u64);
                        first_token_recorded = true;
                    }
                    response.push_text(&text);
                    self.emit(AgentEvent::ContentDelta { text: text.clone() });
                    if let Err(e) = self.extensions.on_content_delta(&text).await {
                        tracing::warn!("ContentDelta extension hook failed: {}", e);
                    }
                }
                Ok(ContentEvent::ThinkingDelta { text }) => {
                    if !first_token_recorded {
                        response.ttft_ms = Some(request_start.elapsed().as_millis() as u64);
                        first_token_recorded = true;
                    }
                    response.push_thinking(&text);
                    self.emit(AgentEvent::ThinkingDelta { text });
                }
                Ok(ContentEvent::ToolCallComplete {
                    call_id,
                    tool_name,
                    args,
                }) => {
                    has_tool_calls = true;
                    response.push_tool_call(call_id.clone(), tool_name.clone(), args.clone());
                    self.emit(AgentEvent::ToolCallStart {
                        call_id,
                        name: tool_name,
                        args,
                    });
                }
                Ok(ContentEvent::Finished { stop_reason, usage }) => {
                    response.finish(stop_reason, usage);
                }
                Ok(_) => {}
                Err(e) => {
                    // If we have completed tool calls, salvage them instead of discarding
                    if has_tool_calls {
                        tracing::warn!(
                            "Stream error after {} tool calls — salvaging partial response: {}",
                            response.tool_calls.len(),
                            e
                        );
                        response.finish(
                            pipit_provider::StopReason::ToolUse,
                            pipit_provider::UsageMetadata::default(),
                        );
                        return Ok(response);
                    }

                    // Streaming reconnection: if we have partial text and the error
                    // is transient (network drop), attempt to continue from where we
                    // left off by re-sending with the partial response as assistant prefix.
                    if e.is_transient() && !response.text.is_empty() {
                        tracing::warn!(
                            "Stream interrupted with {} chars of partial text — attempting reconnection: {}",
                            response.text.len(),
                            e
                        );
                        self.emit(AgentEvent::Waiting {
                            label: "Network interrupted — reconnecting with partial response…"
                                .to_string(),
                        });

                        // Build continuation request: inject partial text as assistant message
                        // so the model continues from where it left off.
                        let mut continuation = request.clone();
                        continuation
                            .messages
                            .push(Message::assistant(&response.text));
                        continuation.messages.push(Message::user(
                            "[SYSTEM] Your previous response was interrupted by a network error. \
                             The partial response above has been preserved. Please continue from \
                             where you left off. Do not repeat what you already said.",
                        ));

                        // Brief backoff before reconnection
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                        match self.provider().complete(continuation, cancel.clone()).await {
                            Ok(mut reconnect_stream) => {
                                while let Some(event) = reconnect_stream.next().await {
                                    if cancel.is_cancelled() {
                                        return Err(pipit_provider::ProviderError::Cancelled);
                                    }
                                    match event {
                                        Ok(ContentEvent::ContentDelta { text }) => {
                                            response.push_text(&text);
                                            self.emit(AgentEvent::ContentDelta {
                                                text: text.clone(),
                                            });
                                        }
                                        Ok(ContentEvent::ThinkingDelta { text }) => {
                                            response.push_thinking(&text);
                                            self.emit(AgentEvent::ThinkingDelta { text });
                                        }
                                        Ok(ContentEvent::ToolCallComplete {
                                            call_id,
                                            tool_name,
                                            args,
                                        }) => {
                                            has_tool_calls = true;
                                            response.push_tool_call(
                                                call_id.clone(),
                                                tool_name.clone(),
                                                args.clone(),
                                            );
                                            self.emit(AgentEvent::ToolCallStart {
                                                call_id,
                                                name: tool_name,
                                                args,
                                            });
                                        }
                                        Ok(ContentEvent::Finished { stop_reason, usage }) => {
                                            response.finish(stop_reason, usage);
                                        }
                                        Ok(_) => {}
                                        Err(e2) => {
                                            tracing::warn!(
                                                "Reconnection stream also failed: {}",
                                                e2
                                            );
                                            return Err(e2);
                                        }
                                    }
                                }
                                return Ok(response);
                            }
                            Err(reconnect_err) => {
                                tracing::warn!("Reconnection failed: {}", reconnect_err);
                                return Err(reconnect_err);
                            }
                        }
                    }

                    return Err(e);
                }
            }
        }

        Ok(response)
    }

    async fn stream_response_with_recovery(
        &mut self,
        claim: &ChangeClaim,
        selected_plan: &CandidatePlan,
        tools: &[pipit_provider::ToolDeclaration],
        cancel: CancellationToken,
    ) -> Result<AssistantResponse, pipit_provider::ProviderError> {
        let mut attempts = 0usize;

        // Pre-flight: proactively shrink context if over budget
        let (input_est, model_limit, over) = self
            .context
            .preflight_check(tools.len(), self.repo_map.as_deref());
        if over > 0 {
            tracing::warn!(
                "Pre-flight: estimated {} tokens vs {} limit (over by {}). Reducing.",
                input_est,
                model_limit,
                over
            );
            // Stage 1: evict stale tool results (older than 10 messages)
            let freed1 = self.context.evict_stale_tool_results(10);
            if freed1 > 0 {
                self.emit(AgentEvent::Waiting {
                    label: format!("Evicted stale tool results, freed ~{} tokens", freed1),
                });
            }
            // Stage 2: truncate large individual results
            let freed2 = self.context.truncate_large_results(8000);
            if freed2 > 0 {
                self.emit(AgentEvent::Waiting {
                    label: format!("Truncated large results, freed ~{} tokens", freed2),
                });
            }
            // Stage 3: shrink remaining old tool results
            let freed3 = self.context.shrink_tool_results(500);
            if freed3 > 0 {
                self.emit(AgentEvent::Waiting {
                    label: format!("Freed ~{} tokens from old tool results", freed3),
                });
            }
            // Stage 4: check again, if still over, drop repo map + compress
            let (_est2, _lim2, over2) = self
                .context
                .preflight_check(tools.len(), self.repo_map.as_deref());
            if over2 > 0 {
                let reduction = self.reduce_request_size();
                if reduction.applied {
                    self.emit(AgentEvent::Waiting {
                        label: format!("Context reduced: {}", reduction.summary),
                    });
                }
            }
        }

        loop {
            let request = self
                .build_completion_request(claim, selected_plan, tools)
                .await;

            match self.stream_response(request, cancel.clone()).await {
                Ok(response) => {
                    self.telemetry.session_counters.record_success();
                    return Ok(response);
                }
                Err(err) if is_request_too_large_error(&err) && attempts < 3 => {
                    attempts += 1;
                    let reduction = self.reduce_request_size();
                    if !reduction.applied {
                        return Err(err);
                    }

                    self.emit(AgentEvent::ProviderError {
                        error: format!(
                            "{} Retrying with reduced context ({}).",
                            err, reduction.summary
                        ),
                        will_retry: true,
                    });
                }
                Err(err)
                    if err.is_transient()
                        && attempts < 3
                        && self.telemetry.session_counters.can_retry() =>
                {
                    attempts += 1;
                    self.telemetry.session_counters.record_retry();
                    let delay_ms = 500 * 2u64.pow(attempts as u32);
                    self.emit(AgentEvent::ProviderError {
                        error: format!(
                            "{} — retrying in {}ms (attempt {}/3)",
                            err, delay_ms, attempts
                        ),
                        will_retry: true,
                    });
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    async fn build_completion_request(
        &self,
        claim: &ChangeClaim,
        selected_plan: &CandidatePlan,
        tools: &[pipit_provider::ToolDeclaration],
    ) -> CompletionRequest {
        let mut request = self.context.build_request(tools, self.repo_map.as_deref());

        // Always inject planning context for non-trivial tasks.
        // The plan strategy (MinimalPatch, CharacterizationFirst, etc.) guides
        // the model's approach — omitting it causes regressions in edit quality.
        // Only suppress for first-turn Q&A that hasn't done any tool calls.
        let is_trivial_qa = !self.has_had_tool_calls()
            && selected_plan.strategy == crate::planner::StrategyKind::MinimalPatch
            && selected_plan.rationale == "Direct response.";

        if !is_trivial_qa {
            request.system.push_str("\n\n");
            request.system.push_str(&claim.render_for_prompt());
            request.system.push_str("\n## Selected Execution Plan\n");
            request.system.push_str(&format!(
                "Strategy: {:?}\nRationale: {}\n",
                selected_plan.strategy, selected_plan.rationale
            ));
        }

        // Inject session projection for multi-turn awareness.
        // Only inject when there's meaningful state to show (modified files).
        // Skip on early turns with no mutations — the empty projection adds
        // noise that can confuse smaller models into producing text instead
        // of tool calls.
        if let Some(ref state) = self.session_state {
            if !state.modified_files.is_empty() {
                let proj = crate::projections::project_workspace(state);
                let mut ctx_note = String::from("\n## Session State\n");
                ctx_note.push_str(&format!(
                    "Files modified this session: {}\n",
                    proj.modified_files.join(", ")
                ));
                if proj.compressions > 0 {
                    ctx_note.push_str(&format!(
                        "Tokens used: {} | Compressions: {}\n",
                        proj.total_tokens, proj.compressions
                    ));
                }
                request.system.push_str(&ctx_note);
            }
        }

        if let Ok(Some(modified_system)) = self.extensions.on_before_request(&request.system).await
        {
            request.system = modified_system;
        }
        request
    }

    fn reduce_request_size(&mut self) -> RequestReduction {
        if self.repo_map.is_some() {
            self.repo_map = None;
            self.context.update_repo_map_tokens(0);
            return RequestReduction {
                applied: true,
                summary: "dropped RepoMap from the prompt".to_string(),
            };
        }

        let stats = self.context.force_shrink_for_transport();
        if stats.messages_removed > 0 || stats.tokens_freed > 0 {
            self.emit(AgentEvent::CompressionStart);
            self.emit(AgentEvent::CompressionEnd {
                messages_removed: stats.messages_removed,
                tokens_freed: stats.tokens_freed,
            });
            return RequestReduction {
                applied: true,
                summary: format!(
                    "locally summarized history; removed {} messages and freed ~{} tokens",
                    stats.messages_removed, stats.tokens_freed
                ),
            };
        }

        RequestReduction {
            applied: false,
            summary: "no further local context reduction available".to_string(),
        }
    }

    /// Execute tool calls with concurrent read / sequential write.
    /// Authorization is handled by the centralized PolicyKernel.
    async fn execute_tools(
        &mut self,
        calls: &[pipit_provider::ToolCall],
        cancel: CancellationToken,
        governor: &Governor,
        confidence: &ConfidenceReport,
        turn: u32,
    ) -> (
        Vec<(String, ToolCallOutcome)>,
        Vec<String>,
        Vec<EvidenceArtifact>,
        Vec<RealizedEdit>,
        RiskReport,
    ) {
        let mut results = Vec::new();
        let mut modified_files = Vec::new();
        let mut approved_reads = Vec::new();
        let mut approved_writes = Vec::new();
        let mut evidence = Vec::new();
        let mut realized_edits = Vec::new();
        let mut highest_risk = RiskReport::default();

        // Fix #6: Record ALL tool calls in loop detector (not just writes)
        for call in calls {
            self.loop_detector.record(&call.tool_name, &call.args);
            let call_risk = governor.assess_tool_call(call, confidence);
            if call_risk.score > highest_risk.score {
                highest_risk = call_risk;
            }
        }

        // Build real execution lineage from session state so PolicyKernel
        // can make provenance-aware authorization decisions.
        let lineage = ExecutionLineage {
            task_chain: vec![self.session_id.clone(), format!("turn-{}", turn)],
            depth: 0, // root agent; subagents would increment
            parent_id: Some(self.session_id.clone()),
            context: ExecutionContext::Interactive,
        };

        for call in calls {
            let Some(tool) = self.tools.get(&call.tool_name) else {
                results.push((
                    call.call_id.clone(),
                    ToolCallOutcome::Error {
                        message: format!("Tool not found: {}", call.tool_name),
                    },
                ));
                continue;
            };

            // ── Semantic authorization via PolicyKernel (single oracle) ──
            let semantics = builtin_semantics(&call.tool_name);
            let mut resource_scopes = Vec::new();
            if let Some(path) = call.args.get("path").and_then(|v| v.as_str()) {
                resource_scopes.push(ResourceScope::Path(std::path::PathBuf::from(path)));
            }
            if let Some(cmd) = call.args.get("command").and_then(|v| v.as_str()) {
                resource_scopes.push(ResourceScope::Command(cmd.to_string()));
            }

            let cap_request = CapabilityRequest {
                required: semantics.required_capabilities,
                resource_scopes,
                justification: Some(format!("Tool '{}' invocation", call.tool_name)),
            };

            let decision = self
                .policy_kernel
                .evaluate(&call.tool_name, &cap_request, &lineage);

            match decision {
                PolicyDecision::Deny { reason } => {
                    results.push((
                        call.call_id.clone(),
                        ToolCallOutcome::PolicyBlocked {
                            message: format!(
                                "Policy denied '{}': {}. The tool requires capabilities that are not granted in the current approval mode.",
                                call.tool_name, reason
                            ),
                            stage: PolicyStage::PreToolUse,
                            mutated: false,
                        },
                    ));
                    evidence.push(EvidenceArtifact::PolicyViolation {
                        tool_name: call.tool_name.clone(),
                        stage: PolicyStage::PreToolUse,
                        summary: format!("Policy denied: {}", reason),
                        mutation_applied: false,
                        path: call
                            .args
                            .get("path")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                    });
                    continue;
                }
                PolicyDecision::Ask { reason } => {
                    self.emit(AgentEvent::ToolApprovalNeeded {
                        call_id: call.call_id.clone(),
                        name: call.tool_name.clone(),
                        args: call.args.clone(),
                    });

                    let approval = self
                        .approval_handler
                        .request_approval(&call.call_id, &call.tool_name, &call.args)
                        .await;

                    match approval {
                        ApprovalDecision::Approve => { /* fall through */ }
                        ApprovalDecision::Deny => {
                            results.push((
                                call.call_id.clone(),
                                ToolCallOutcome::PolicyBlocked {
                                    message: format!(
                                        "User denied approval for '{}'. Try a different approach or ask the user for guidance.",
                                        call.tool_name
                                    ),
                                    stage: PolicyStage::PreToolUse,
                                    mutated: false,
                                },
                            ));
                            evidence.push(EvidenceArtifact::ApprovalBlocked {
                                tool_name: call.tool_name.clone(),
                                reason: format!("User denied (policy reason: {})", reason),
                            });
                            continue;
                        }
                        ApprovalDecision::ScopedGrant(grant) => {
                            // Validate the scoped grant constraints against this call
                            match grant.validate_constraints(&call.tool_name, &call.args) {
                                Ok(()) => { /* constraints satisfied, fall through */ }
                                Err(violation) => {
                                    results.push((
                                        call.call_id.clone(),
                                        ToolCallOutcome::PolicyBlocked {
                                            message: format!(
                                                "Scoped grant constraint violated for '{}': {}",
                                                call.tool_name, violation
                                            ),
                                            stage: PolicyStage::PreToolUse,
                                            mutated: false,
                                        },
                                    ));
                                    evidence.push(EvidenceArtifact::PolicyViolation {
                                        tool_name: call.tool_name.clone(),
                                        stage: PolicyStage::PreToolUse,
                                        summary: format!("Scoped grant violated: {}", violation),
                                        mutation_applied: false,
                                        path: call
                                            .args
                                            .get("path")
                                            .and_then(|v| v.as_str())
                                            .map(str::to_string),
                                    });
                                    continue;
                                }
                            }
                        }
                    }
                }
                PolicyDecision::Allow | PolicyDecision::Sandbox { .. } => {
                    // Proceed directly
                }
            }

            // Classify as read or write based on semantic purity for scheduling
            if semantics.purity <= crate::tool_semantics::Purity::Idempotent {
                approved_reads.push(call.clone());
            } else {
                approved_writes.push(call.clone());
            }
        }

        // ── Schedule via conflict-aware scheduler (single authority for batching) ──
        // Combine all approved calls, then let the scheduler partition them.
        let mut all_approved: Vec<pipit_provider::ToolCall> = Vec::new();
        all_approved.extend(approved_reads);
        all_approved.extend(approved_writes);

        let batches = crate::scheduler::schedule(&all_approved);
        tracing::debug!(
            total_calls = all_approved.len(),
            batches = batches.len(),
            "Scheduler partitioned tool calls into {} batch(es)",
            batches.len()
        );

        // ── Execute batches: concurrent within batch, sequential across batches ──
        for batch in &batches {
            if cancel.is_cancelled() {
                break;
            }

            let batch_calls: Vec<&pipit_provider::ToolCall> =
                batch.indices.iter().map(|&i| &all_approved[i]).collect();

            if batch_calls.len() == 1 || !batch.all_read_only {
                // Sequential: execute one at a time (writes, or mixed)
                for call in batch_calls {
                    if cancel.is_cancelled() {
                        break;
                    }

                    let modified_args = match self
                        .extensions
                        .on_before_tool(&call.tool_name, &call.args)
                        .await
                    {
                        Ok(Some(new_args)) => new_args,
                        Ok(None) => call.args.clone(),
                        Err(err) => {
                            let outcome = ToolCallOutcome::Error {
                                message: err.to_string(),
                            };
                            evidence.push(evidence_from_tool(call, &outcome));
                            self.emit(AgentEvent::ToolCallEnd {
                                call_id: call.call_id.clone(),
                                name: call.tool_name.clone(),
                                result: outcome.clone(),
                            });
                            results.push((call.call_id.clone(), outcome));
                            continue;
                        }
                    };
                    let mut modified_call = call.clone();
                    modified_call.args = modified_args;

                    let mut tool_span = self
                        .telemetry
                        .start_span("tool.execute")
                        .attr("tool.name", SpanValue::String(call.tool_name.clone()));
                    let outcome = execute_single_tool(
                        &self.tools,
                        &modified_call,
                        &self.tool_context,
                        cancel.clone(),
                        self.config.dry_run,
                    )
                    .await;
                    let success = matches!(outcome, ToolCallOutcome::Success { .. });
                    tool_span.finish(if success {
                        SpanStatus::Ok
                    } else {
                        SpanStatus::Error
                    });
                    self.telemetry.record_span(tool_span);
                    self.telemetry.session_counters.increment_tool_calls();
                    let mutation_applied =
                        matches!(outcome, ToolCallOutcome::Success { mutated: true, .. });
                    let outcome =
                        apply_after_tool_hook(&*self.extensions, &call.tool_name, outcome).await;
                    if mutation_applied {
                        if let Some(path) = call.args.get("path").and_then(|value| value.as_str()) {
                            modified_files.push(path.to_string());
                            realized_edits.push(RealizedEdit {
                                path: path.to_string(),
                                summary: summarize_tool_outcome(&call.tool_name, &outcome),
                            });
                        }
                    }
                    // Propagate typed tool artifacts/edits into evidence
                    if let ToolCallOutcome::Success {
                        ref artifacts,
                        ref edits,
                        ..
                    } = outcome
                    {
                        for artifact in artifacts {
                            evidence.push(crate::proof::EvidenceArtifact::ToolExecution {
                                tool_name: call.tool_name.clone(),
                                summary: format!("{:?}", artifact),
                                success: true,
                            });
                        }
                        for edit in edits {
                            if !modified_files.contains(&edit.path.to_string_lossy().to_string()) {
                                modified_files.push(edit.path.to_string_lossy().to_string());
                            }
                            realized_edits.push(RealizedEdit {
                                path: edit.path.to_string_lossy().to_string(),
                                summary: format!("{} hunks", edit.hunks),
                            });
                        }
                    }
                    evidence.push(evidence_from_tool(call, &outcome));
                    self.emit(AgentEvent::ToolCallEnd {
                        call_id: call.call_id.clone(),
                        name: call.tool_name.clone(),
                        result: outcome.clone(),
                    });
                    results.push((call.call_id.clone(), outcome));
                }
            } else {
                // Concurrent: all calls in this batch are independent reads
                let concurrent_futures: Vec<_> = batch_calls
                    .into_iter()
                    .map(|call| {
                        let call = call.clone();
                        let tools = self.tools.clone();
                        let ctx = self.tool_context.clone();
                        let cancel = cancel.clone();
                        let extensions = self.extensions.clone();
                        let dry_run = self.config.dry_run;
                        async move {
                            let modified_args = match extensions
                                .on_before_tool(&call.tool_name, &call.args)
                                .await
                            {
                                Ok(Some(new_args)) => new_args,
                                Ok(None) => call.args.clone(),
                                Err(err) => {
                                    return (
                                        call.call_id.clone(),
                                        ToolCallOutcome::Error {
                                            message: err.to_string(),
                                        },
                                    );
                                }
                            };
                            let mut modified_call = call.clone();
                            modified_call.args = modified_args;
                            let outcome =
                                execute_single_tool(&tools, &modified_call, &ctx, cancel, dry_run)
                                    .await;
                            let outcome =
                                apply_after_tool_hook(&*extensions, &call.tool_name, outcome).await;
                            (call.call_id.clone(), outcome)
                        }
                    })
                    .collect();

                let batch_results = futures::future::join_all(concurrent_futures).await;
                for (call_id, outcome) in &batch_results {
                    let name = calls
                        .iter()
                        .find(|c| c.call_id == *call_id)
                        .map(|c| c.tool_name.as_str())
                        .unwrap_or("unknown");
                    self.emit(AgentEvent::ToolCallEnd {
                        call_id: call_id.clone(),
                        name: name.to_string(),
                        result: outcome.clone(),
                    });
                    if let Some(call) = calls.iter().find(|c| c.call_id == *call_id) {
                        evidence.push(evidence_from_tool(call, outcome));
                    }
                }
                results.extend(batch_results);
            }
        }

        (
            results,
            modified_files,
            evidence,
            realized_edits,
            highest_risk,
        )
    }

    /// Drain any pending steering messages.
    async fn drain_steering_messages(&mut self) {
        let mut messages = Vec::new();
        if let Some(ref mut rx) = self.steering_rx {
            while let Ok(msg) = rx.try_recv() {
                messages.push(msg);
            }
        }
        for msg in messages {
            self.emit(AgentEvent::SteeringMessageInjected { text: msg.clone() });
            self.context.push_message(Message::user(&msg));
        }
    }

    fn emit(&self, event: AgentEvent) {
        // Record significant events to the session ledger for crash recovery.
        match &event {
            AgentEvent::ToolCallStart {
                call_id,
                name,
                args,
            } => {
                self.record(SessionEvent::ToolCallProposed {
                    call_id: call_id.clone(),
                    tool_name: name.clone(),
                    args: args.clone(),
                });
            }
            AgentEvent::ToolCallEnd {
                call_id,
                name,
                result,
                ..
            } => {
                let (success, mutated) = match result {
                    ToolCallOutcome::Success { mutated, .. } => (true, *mutated),
                    _ => (false, false),
                };
                self.record(SessionEvent::ToolCompleted {
                    call_id: call_id.clone(),
                    success,
                    mutated,
                    result_summary: summarize_tool_outcome(name, result),
                    result_blob_hash: None,
                });
            }
            AgentEvent::CompressionEnd {
                messages_removed,
                tokens_freed,
            } => {
                self.record(SessionEvent::ContextCompressed {
                    messages_removed: *messages_removed,
                    tokens_freed: *tokens_freed,
                    strategy: "auto".to_string(),
                });
            }
            AgentEvent::TurnStart { turn_number } => {
                self.record(SessionEvent::AssistantResponseStarted { turn: *turn_number });
            }
            AgentEvent::SteeringMessageInjected { text } => {
                self.record(SessionEvent::UserMessageAccepted {
                    content: text.clone(),
                });
            }
            AgentEvent::PlanSelected {
                strategy,
                rationale,
                pivoted,
                ..
            } => {
                if *pivoted {
                    self.record(SessionEvent::PlanPivoted {
                        from_strategy: String::new(),
                        to_strategy: strategy.clone(),
                        trigger: rationale.clone(),
                    });
                }
            }
            _ => {}
        }
        let _ = self.event_tx.send(event);
    }

    fn emit_plan_selected(
        &self,
        plan: &CandidatePlan,
        candidate_plans: &[CandidatePlan],
        pivoted: bool,
    ) {
        self.emit(AgentEvent::PlanSelected {
            strategy: format!("{:?}", plan.strategy),
            rationale: plan.rationale.clone(),
            pivoted,
            candidate_plans: candidate_plans.to_vec(),
        });
    }

    fn update_planning_state(
        &mut self,
        selected_plan: &CandidatePlan,
        candidate_plans: &[CandidatePlan],
        plan_pivots: &[PlanPivot],
    ) {
        self.planning_state = Some(PlanningState {
            selected_plan: selected_plan.clone(),
            candidate_plans: candidate_plans.to_vec(),
            plan_pivots: plan_pivots.to_vec(),
        });
    }

    /// Get current context/token usage.
    pub fn context_usage(&self) -> pipit_context::budget::TokenUsage {
        self.context.token_usage()
    }

    /// Get the text content of the last assistant message in context.
    /// Used by `/commit` to extract LLM-generated commit messages.
    pub fn last_assistant_text(&self) -> Option<String> {
        self.context
            .messages()
            .iter()
            .rev()
            .find(|m| matches!(m.role, pipit_provider::Role::Assistant))
            .map(|m| m.text_content())
    }

    pub fn clear_context(&mut self) {
        self.context.clear();
    }

    /// Inject a message directly into context (used for session resume/replay).
    pub fn inject_message(&mut self, message: pipit_provider::Message) {
        self.context.push_message(message);
    }

    /// Graceful shutdown: flush all state, export telemetry, record session end.
    /// Call from signal handlers to ensure no data loss on Ctrl-C.
    pub fn graceful_shutdown(&self) -> ShutdownSummary {
        let usage = self.context.token_usage();
        let turns = self
            .telemetry
            .session_counters
            .turns
            .load(std::sync::atomic::Ordering::Relaxed) as u32;
        let cost = self.telemetry.session_counters.total_cost();

        // 1. End session via kernel (single authority) or legacy ledger
        if let Some(ref mtx) = self.session_kernel {
            if let Ok(mut kernel) = mtx.lock() {
                let _ = kernel.end_session(turns, usage.total, cost);
            }
        } else if let Some(ref mtx) = self.session_ledger {
            if let Ok(mut ledger) = mtx.lock() {
                let _ = ledger.append(SessionEvent::SessionEnded {
                    turns,
                    total_tokens: usage.total,
                    cost,
                });
            }
        }

        // 2. Export all buffered telemetry spans
        let spans_exported = self.telemetry.export().unwrap_or(0);

        // 3. Build summary
        let summary = self.telemetry.session_summary();
        ShutdownSummary {
            turns: summary.turns,
            total_cost: summary.total_cost,
            tokens_used: summary.tokens_input + summary.tokens_output,
            tool_calls: summary.tool_calls,
            spans_exported: spans_exported as u64,
            ledger_flushed: self.session_ledger.is_some(),
        }
    }

    /// Get a session summary snapshot (for /status, /cost, graceful shutdown).
    pub fn session_summary(&self) -> crate::telemetry_facade::SessionSummary {
        self.telemetry.session_summary()
    }

    /// Save the current conversation to a session directory.
    pub fn save_session(&self, session_dir: &std::path::Path) -> Result<(), String> {
        std::fs::create_dir_all(session_dir).map_err(|e| e.to_string())?;
        let session_file = session_dir.join("messages.json");
        let json =
            serde_json::to_string_pretty(self.context.messages()).map_err(|e| e.to_string())?;
        std::fs::write(&session_file, json).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Restore conversation from a session directory.
    pub fn load_session(&mut self, session_dir: &std::path::Path) -> Result<usize, String> {
        let session_file = session_dir.join("messages.json");
        let messages = pipit_context::ContextManager::load_session(&session_file)
            .map_err(|e| e.to_string())?;
        let count = messages.len();
        self.context.restore_messages(messages);
        Ok(count)
    }

    pub fn planning_state(&self) -> Option<PlanningState> {
        self.planning_state.clone()
    }

    pub async fn compact_context(
        &mut self,
        cancel: CancellationToken,
    ) -> Result<pipit_context::budget::CompressionStats, String> {
        let provider = self.provider().clone();
        let session_id = self.session_id.clone();
        self.context
            .compress_pipeline(provider, &session_id, self.memory_store.as_deref(), cancel)
            .await
            .map_err(|e| e.to_string())
    }

    /// Run configured verification commands (lint, test) after file mutations.
    /// Returns a list of (command, output, success) tuples.
    async fn run_post_edit_verification(
        &self,
        _modified_files: &[String],
        cancel: CancellationToken,
    ) -> Vec<(String, String, bool)> {
        let mut results = Vec::new();

        let commands: Vec<&str> = [
            self.config.lint_command.as_deref(),
            self.config.test_command.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();

        for cmd in commands {
            if cancel.is_cancelled() {
                break;
            }

            self.emit(AgentEvent::SteeringMessageInjected {
                text: format!("Running post-edit verification: {}", cmd),
            });

            let output = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&self.tool_context.project_root)
                .output()
                .await;

            match output {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    let combined = if stderr.is_empty() {
                        stdout
                    } else {
                        format!("{}\n{}", stdout, stderr)
                    };
                    results.push((cmd.to_string(), combined, output.status.success()));
                }
                Err(e) => {
                    results.push((cmd.to_string(), format!("Failed to run: {}", e), false));
                }
            }
        }

        results
    }
}

#[derive(Debug, Clone)]
struct RequestReduction {
    applied: bool,
    summary: String,
}

pub(crate) fn summarize_tool_outcome(tool_name: &str, outcome: &ToolCallOutcome) -> String {
    match outcome {
        ToolCallOutcome::Success { content, .. } => {
            format!("{} succeeded: {}", tool_name, truncate(content, 160))
        }
        ToolCallOutcome::PolicyBlocked {
            message,
            stage,
            mutated,
        } => {
            let stage_label = match stage {
                PolicyStage::PreToolUse => "pre-tool policy blocked",
                PolicyStage::PostToolUse if *mutated => "post-tool policy blocked after mutation",
                PolicyStage::PostToolUse => "post-tool policy blocked",
            };
            format!("{} {}: {}", tool_name, stage_label, truncate(message, 160))
        }
        ToolCallOutcome::Error { message } => {
            format!("{} failed: {}", tool_name, truncate(message, 160))
        }
    }
}

/// Classify a tool call into an evidence artifact based on its canonical
/// `SemanticClass` — the same algebraic type used by governor and scheduler.
/// Evidence is a function of semantics, not of spelling.
pub(crate) fn evidence_from_tool(
    call: &pipit_provider::ToolCall,
    outcome: &ToolCallOutcome,
) -> EvidenceArtifact {
    if let ToolCallOutcome::PolicyBlocked { stage, mutated, .. } = outcome {
        return EvidenceArtifact::PolicyViolation {
            tool_name: call.tool_name.clone(),
            stage: stage.clone(),
            summary: summarize_tool_outcome(&call.tool_name, outcome),
            mutation_applied: *mutated,
            path: call
                .args
                .get("path")
                .and_then(|value| value.as_str())
                .map(str::to_string),
        };
    }

    let semantic_class = classify_semantically(&call.tool_name, &call.args);

    match semantic_class {
        SemanticClass::Read { paths } => EvidenceArtifact::FileRead {
            path: paths.into_iter().next(),
            summary: summarize_tool_outcome(&call.tool_name, outcome),
        },
        SemanticClass::Search { .. } | SemanticClass::Pure => EvidenceArtifact::FileRead {
            path: call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            summary: summarize_tool_outcome(&call.tool_name, outcome),
        },
        SemanticClass::Edit { paths } => EvidenceArtifact::EditApplied {
            path: paths.into_iter().next(),
            summary: summarize_tool_outcome(&call.tool_name, outcome),
        },
        SemanticClass::Exec { command } => EvidenceArtifact::CommandResult {
            kind: classify_verification_command(&command),
            command,
            output: match outcome {
                ToolCallOutcome::Success { content, .. } => truncate(content, 240),
                ToolCallOutcome::PolicyBlocked { message, .. } => truncate(message, 240),
                ToolCallOutcome::Error { message } => truncate(message, 240),
            },
            success: matches!(outcome, ToolCallOutcome::Success { .. }),
        },
        SemanticClass::Delegate { .. } | SemanticClass::External { .. } => {
            EvidenceArtifact::ToolExecution {
                tool_name: call.tool_name.clone(),
                summary: summarize_tool_outcome(&call.tool_name, outcome),
                success: matches!(outcome, ToolCallOutcome::Success { .. }),
            }
        }
    }
}

fn finalize_proof(
    governor: &Governor,
    verifier: &dyn VerifyStrategy,
    objective: Objective,
    claim: &mut ChangeClaim,
    selected_plan: CandidatePlan,
    candidate_plans: Vec<CandidatePlan>,
    plan_pivots: Vec<PlanPivot>,
    evidence: &[EvidenceArtifact],
    realized_edits: &[RealizedEdit],
    risk: RiskReport,
    project_root: &std::path::Path,
) -> ProofPacket {
    use crate::planner::{PlanSource, VerificationSource};
    use crate::proof::ImplementationTier;
    use std::collections::HashMap;

    let confidence = verifier.summarize_confidence(evidence, realized_edits);
    claim.confidence = confidence.clone();
    let unresolved_assumptions = verifier.unresolved_assumptions(&claim.assumptions, evidence);
    let modified_files: Vec<String> = realized_edits
        .iter()
        .map(|edit| edit.path.clone())
        .collect();
    let rollback_checkpoint = governor.create_rollback_checkpoint(project_root, &modified_files);

    // Record implementation tiers for provenance
    let mut tiers = HashMap::new();
    tiers.insert(
        "planner".to_string(),
        match selected_plan.plan_source {
            PlanSource::Heuristic => ImplementationTier::Heuristic,
            PlanSource::LlmStructured => ImplementationTier::LlmStructured,
            PlanSource::UserSpecified => ImplementationTier::Heuristic,
        },
    );
    tiers.insert(
        "verifier".to_string(),
        match verifier.source() {
            VerificationSource::Heuristic => ImplementationTier::Heuristic,
            VerificationSource::LlmStructured => ImplementationTier::LlmStructured,
            VerificationSource::None => ImplementationTier::TypeOnly,
        },
    );
    tiers.insert("governor".to_string(), ImplementationTier::Heuristic);

    ProofPacket {
        objective,
        selected_plan,
        candidate_plans,
        plan_pivots,
        claim: claim.clone(),
        evidence: evidence.to_vec(),
        realized_edits: realized_edits.to_vec(),
        unresolved_assumptions,
        risk,
        confidence,
        rollback_checkpoint,
        tiers,
    }
}

fn classify_verification_command(command: &str) -> VerificationKind {
    let lower = command.to_ascii_lowercase();
    if lower.contains("pytest")
        || lower.contains("cargo test")
        || lower.contains("unittest")
        || lower.contains("npm test")
        || lower.contains("pnpm test")
        || lower.contains("go test")
    {
        VerificationKind::Test
    } else if lower.contains("cargo build")
        || lower.contains("cargo check")
        || lower.contains("npm run build")
        || lower.contains("tsc")
        || lower.contains("mvn package")
    {
        VerificationKind::Build
    } else if lower.contains("bench") || lower.contains("hyperfine") {
        VerificationKind::Benchmark
    } else if lower.contains("python") || lower.contains("node ") || lower.contains("./") {
        VerificationKind::RuntimeCheck
    } else {
        VerificationKind::Shell
    }
}

fn truncate(input: &str, max_len: usize) -> String {
    if input.len() <= max_len {
        input.to_string()
    } else {
        // Find the last char boundary at or before max_len to avoid
        // panicking on multi-byte UTF-8 characters (e.g. em dash '—').
        let end = input
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= max_len)
            .last()
            .unwrap_or(0);
        format!("{}...", &input[..end])
    }
}

fn is_request_too_large_error(error: &pipit_provider::ProviderError) -> bool {
    match error {
        pipit_provider::ProviderError::RequestTooLarge { .. } => true,
        pipit_provider::ProviderError::ContextOverflow { .. } => true,
        pipit_provider::ProviderError::Other(message) => {
            let lower = message.to_ascii_lowercase();
            // HTTP 413
            lower.contains("413")
                || lower.contains("payload too large")
                || (lower.contains("request body") && lower.contains("too large"))
                || lower.contains("length limit exceeded")
                // vLLM: "maximum context length is X tokens"
                || lower.contains("maximum context length")
                // vLLM: "prompt contains X characters"
                || (lower.contains("prompt") && lower.contains("too long"))
                // Generic: "context length exceeded"
                || lower.contains("context length exceeded")
                || lower.contains("context_length_exceeded")
                // Ollama / llama.cpp
                || (lower.contains("input") && lower.contains("too long"))
                || (lower.contains("token") && lower.contains("limit"))
                // OpenAI: "maximum.*token"
                || (lower.contains("maximum") && lower.contains("token"))
                // Generic "too many tokens"
                || lower.contains("too many tokens")
        }
        _ => false,
    }
}

fn build_loop_recovery_message(
    tool_name: &str,
    count: u32,
    evidence: &[EvidenceArtifact],
    consecutive_hits: u32,
) -> String {
    let mut message = String::new();

    // Escalating urgency based on how many consecutive turns the loop has persisted
    if consecutive_hits >= 4 {
        message.push_str(
            "CRITICAL: You are about to be terminated for looping. You MUST take a completely different action RIGHT NOW. ",
        );
    } else if consecutive_hits >= 3 {
        message.push_str(
            "WARNING: You have been stuck in a loop for multiple turns and will be stopped soon if you continue. ",
        );
    }

    message.push_str(&format!(
        "LOOP DETECTED: {} was called {} times with similar arguments. Do NOT call {} again with the same arguments. ",
        tool_name, count, tool_name
    ));

    // Context-specific guidance based on the tool that's looping
    match tool_name {
        "read_file" | "glob" | "list_directory" | "grep" => {
            message.push_str(
                "The files you are looking for do not exist. \
                 If you need to CREATE new files, use write_file (not read_file). \
                 If you need to CREATE a project from scratch, use write_file to create each file with the desired content. \
                 Stop trying to read non-existent files and start writing them instead.",
            );
        }
        "edit_file" => {
            message.push_str(
                "Your search text is not matching. \
                 Read the file first with read_file to see the actual content, \
                 then use the exact text from the file as your search string. \
                 If the file does not exist yet, use write_file instead of edit_file.",
            );
        }
        "bash" => {
            message.push_str(
                "The same command keeps failing. Analyze the error output carefully. \
                 Try a different approach: check if dependencies are installed, \
                 verify paths exist, or try an alternative command.",
            );
        }
        _ => {
            message.push_str(
                "Try a fundamentally different approach. \
                 If the current tool is failing, use a different tool or strategy.",
            );
        }
    }

    if let Some((command, output)) = latest_failed_verification(evidence) {
        message.push_str(" Most recent failing verification: ");
        message.push_str(&command);
        message.push_str(" -> ");
        message.push_str(&truncate(&output, 220));
        message.push('.');
    }

    message
}

fn latest_failed_verification(evidence: &[EvidenceArtifact]) -> Option<(String, String)> {
    evidence.iter().rev().find_map(|artifact| match artifact {
        EvidenceArtifact::CommandResult {
            kind,
            command,
            output,
            success: false,
        } if matches!(
            kind,
            VerificationKind::Test | VerificationKind::Build | VerificationKind::RuntimeCheck
        ) =>
        {
            Some((command.clone(), output.clone()))
        }
        _ => None,
    })
}

pub(crate) async fn apply_after_tool_hook(
    extensions: &dyn ExtensionRunner,
    tool_name: &str,
    outcome: ToolCallOutcome,
) -> ToolCallOutcome {
    match outcome {
        ToolCallOutcome::Success {
            content,
            mutated,
            artifacts,
            edits,
        } => match extensions.on_after_tool(tool_name, &content).await {
            Ok(Some(note)) => ToolCallOutcome::Success {
                content: format!("{}\n\n[Hook]\n{}", content, note),
                mutated,
                artifacts,
                edits,
            },
            Ok(None) => ToolCallOutcome::Success {
                content,
                mutated,
                artifacts,
                edits,
            },
            Err(err) => ToolCallOutcome::PolicyBlocked {
                message: err.to_string(),
                stage: PolicyStage::PostToolUse,
                mutated,
            },
        },
        other => other,
    }
}

/// Enrich a tool error message with what/why/fix structure.
/// Maps common stderr patterns to actionable guidance.
fn enrich_tool_error(tool_name: &str, raw_error: &str) -> String {
    let lower = raw_error.to_lowercase();

    // Pattern-match common failures and return (what, why, fix)
    if lower.contains("sed") && lower.contains("illegal option") {
        format!(
            "WHAT: sed command failed due to macOS/BSD incompatibility\n\
             WHY:  macOS uses BSD sed which requires different flags than GNU sed\n\
             FIX:  Use `sed -i '' 's/pattern/replacement/'` on macOS (note the empty quotes), \
             or use `perl -pi -e` which works cross-platform.\n\n\
             Raw error: {}",
            raw_error
        )
    } else if lower.contains("permission denied") {
        format!(
            "WHAT: Permission denied\n\
             WHY:  The file or directory lacks write permissions for the current user\n\
             FIX:  Check file permissions with `ls -la`. Use `chmod` to fix, or run with appropriate privileges.\n\n\
             Raw error: {}",
            raw_error
        )
    } else if lower.contains("no such file or directory") {
        format!(
            "WHAT: File or directory not found\n\
             WHY:  The specified path does not exist\n\
             FIX:  Verify the path with `ls` or `find`. The file may have been moved, deleted, \
             or the path may be relative to a different directory.\n\n\
             Raw error: {}",
            raw_error
        )
    } else if lower.contains("command not found") {
        format!(
            "WHAT: Command '{}' not found\n\
             WHY:  The required program is not installed or not in PATH\n\
             FIX:  Install the missing tool or use an alternative. Check with `which {}`.\n\n\
             Raw error: {}",
            tool_name, tool_name, raw_error
        )
    } else if lower.contains("connection refused") || lower.contains("econnrefused") {
        format!(
            "WHAT: Connection refused\n\
             WHY:  The target server is not running or not accepting connections\n\
             FIX:  Check if the service is running. Verify the host and port are correct.\n\n\
             Raw error: {}",
            raw_error
        )
    } else if lower.contains("timeout") || lower.contains("timed out") {
        format!(
            "WHAT: Operation timed out\n\
             WHY:  The command took too long to complete\n\
             FIX:  Try breaking the operation into smaller steps, or increase the timeout.\n\n\
             Raw error: {}",
            raw_error
        )
    } else if lower.contains("syntax error") || lower.contains("parse error") {
        format!(
            "WHAT: Syntax error in command or script\n\
             WHY:  The command contains invalid syntax\n\
             FIX:  Check the command syntax carefully. Shell quoting, escaping, and special characters are common issues.\n\n\
             Raw error: {}",
            raw_error
        )
    } else if lower.contains("disk full") || lower.contains("no space left") {
        format!(
            "WHAT: Disk full\n\
             WHY:  No disk space remaining on the device\n\
             FIX:  Free disk space with `df -h` to check usage and `du -sh *` to find large files.\n\n\
             Raw error: {}",
            raw_error
        )
    } else {
        raw_error.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::StrategyKind;

    #[test]
    fn failed_bash_tool_outcome_becomes_failed_command_evidence() {
        let call = pipit_provider::ToolCall {
            call_id: "call-1".to_string(),
            tool_name: "bash".to_string(),
            args: serde_json::json!({"command": "python3 -m unittest"}),
        };

        let evidence = evidence_from_tool(
            &call,
            &ToolCallOutcome::Error {
                message: "Execution failed: test failure\n\n[Exit code: 1]".to_string(),
            },
        );

        match evidence {
            EvidenceArtifact::CommandResult {
                kind,
                command,
                output,
                success,
            } => {
                assert!(matches!(kind, VerificationKind::Test));
                assert_eq!(command, "python3 -m unittest");
                assert!(!success);
                assert!(output.contains("Exit code: 1"));
            }
            other => panic!("expected command result evidence, got {:?}", other),
        }
    }

    #[test]
    fn planning_state_tracks_pivots() {
        let selected_plan = CandidatePlan {
            strategy: StrategyKind::CharacterizationFirst,
            rationale: "Pivoted".to_string(),
            expected_value: 0.9,
            estimated_cost: 0.4,
            verification_plan: vec![],
            plan_source: crate::planner::PlanSource::Heuristic,
        };
        let previous_plan = CandidatePlan {
            strategy: StrategyKind::MinimalPatch,
            rationale: "Initial".to_string(),
            expected_value: 0.8,
            estimated_cost: 0.2,
            verification_plan: vec![],
            plan_source: crate::planner::PlanSource::Heuristic,
        };
        let pivots = vec![PlanPivot {
            turn_number: 3,
            from: previous_plan.clone(),
            to: selected_plan.clone(),
            trigger: "repeated failures".to_string(),
        }];

        let state = PlanningState {
            selected_plan,
            candidate_plans: vec![previous_plan],
            plan_pivots: pivots.clone(),
        };

        assert_eq!(state.plan_pivots.len(), 1);
        assert_eq!(state.plan_pivots[0].turn_number, 3);
    }

    #[test]
    fn summarize_tool_outcome_marks_post_hook_failure_as_failure() {
        let summary = summarize_tool_outcome(
            "edit_file",
            &ToolCallOutcome::PolicyBlocked {
                message: "Hook blocked tool execution: post-hook rejected formatted output"
                    .to_string(),
                stage: PolicyStage::PostToolUse,
                mutated: true,
            },
        );

        assert!(summary.contains("policy blocked"));
        assert!(summary.contains("post-hook rejected"));
    }

    #[test]
    fn policy_blocked_after_mutation_becomes_policy_violation_evidence() {
        let call = pipit_provider::ToolCall {
            call_id: "call-2".to_string(),
            tool_name: "edit_file".to_string(),
            args: serde_json::json!({"path": "src/lib.rs"}),
        };

        let evidence = evidence_from_tool(
            &call,
            &ToolCallOutcome::PolicyBlocked {
                message: "Hook blocked tool execution: formatter policy rejected change"
                    .to_string(),
                stage: PolicyStage::PostToolUse,
                mutated: true,
            },
        );

        match evidence {
            EvidenceArtifact::PolicyViolation {
                tool_name,
                stage,
                mutation_applied,
                path,
                summary,
            } => {
                assert_eq!(tool_name, "edit_file");
                assert!(matches!(stage, PolicyStage::PostToolUse));
                assert!(mutation_applied);
                assert_eq!(path.as_deref(), Some("src/lib.rs"));
                assert!(summary.contains("after mutation"));
            }
            other => panic!("expected policy violation evidence, got {:?}", other),
        }
    }

    #[test]
    fn request_too_large_detection_matches_transport_errors() {
        assert!(is_request_too_large_error(
            &pipit_provider::ProviderError::RequestTooLarge {
                message: "HTTP 413 Payload Too Large".to_string(),
            }
        ));
        assert!(is_request_too_large_error(
            &pipit_provider::ProviderError::Other(
                "HTTP 413 Payload Too Large: length limit exceeded".to_string(),
            )
        ));
        assert!(!is_request_too_large_error(
            &pipit_provider::ProviderError::Other("HTTP 500 internal error".to_string(),)
        ));
    }
}

pub(crate) async fn execute_single_tool(
    tools: &ToolRegistry,
    call: &pipit_provider::ToolCall,
    ctx: &ToolContext,
    cancel: CancellationToken,
    dry_run: bool,
) -> ToolCallOutcome {
    let tool = match tools.get(&call.tool_name) {
        Some(t) => t,
        None => {
            return ToolCallOutcome::Error {
                message: format!("Tool not found: {}", call.tool_name),
            };
        }
    };

    let schema = tool.schema();

    // ── Layer 1: Schema structural validation ──
    // Validates required fields, types, and basic constraints.
    // Cost: O(|args| + |schema|)
    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        for field in required {
            if let Some(field_name) = field.as_str() {
                if call.args.get(field_name).is_none() || call.args[field_name].is_null() {
                    return ToolCallOutcome::Error {
                        message: format!(
                            "Missing required argument '{}' for tool '{}'",
                            field_name, call.tool_name
                        ),
                    };
                }
            }
        }
    }

    // Type validation: check each provided arg against its declared schema type
    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
        for (prop_name, prop_schema) in properties {
            if let Some(arg_value) = call.args.get(prop_name) {
                if arg_value.is_null() {
                    continue; // null is ok for optional fields
                }
                if let Some(expected_type) = prop_schema.get("type").and_then(|t| t.as_str()) {
                    let type_ok = match expected_type {
                        "string" => arg_value.is_string(),
                        "integer" | "number" => arg_value.is_number(),
                        "boolean" => arg_value.is_boolean(),
                        "array" => arg_value.is_array(),
                        "object" => arg_value.is_object(),
                        _ => true,
                    };
                    if !type_ok {
                        return ToolCallOutcome::Error {
                            message: format!(
                                "Type mismatch for '{}' in tool '{}': expected {}, got {}",
                                prop_name,
                                call.tool_name,
                                expected_type,
                                json_type_name(arg_value)
                            ),
                        };
                    }
                }

                // Enum validation
                if let Some(enum_values) = prop_schema.get("enum").and_then(|e| e.as_array()) {
                    if !enum_values.contains(arg_value) {
                        return ToolCallOutcome::Error {
                            message: format!(
                                "Invalid value for '{}' in tool '{}': must be one of {:?}",
                                prop_name, call.tool_name, enum_values
                            ),
                        };
                    }
                }
            }
        }
    }

    // ── Layer 2: Semantic domain validation ──
    // Cost: O(k) for bounded number of predicates + O(path_len) for path checks
    if let Err(msg) = validate_tool_semantics(&call.tool_name, &call.args, ctx) {
        return ToolCallOutcome::Error { message: msg };
    }

    // ── Dry-run interception ──
    // In dry-run mode, mutating tools return a preview instead of executing.
    if dry_run {
        let semantics = builtin_semantics(&call.tool_name);
        if semantics.needs_approval_by_purity() {
            let args_preview =
                serde_json::to_string_pretty(&call.args).unwrap_or_else(|_| call.args.to_string());
            return ToolCallOutcome::Success {
                content: format!(
                    "[DRY RUN] Would execute '{}' with args:\n{}",
                    call.tool_name, args_preview
                ),
                mutated: false,
                artifacts: Vec::new(),
                edits: Vec::new(),
            };
        }
    }

    // Wrap tool execution in catch_unwind to isolate panics.
    // A panicking tool produces an error result instead of crashing the session.
    let tool_name = call.tool_name.clone();
    let args = call.args.clone();
    let execute_future = tool.execute(args, ctx, cancel);

    match std::panic::AssertUnwindSafe(execute_future)
        .catch_unwind()
        .await
    {
        Ok(Ok(result)) => ToolCallOutcome::Success {
            content: result.content,
            mutated: result.mutated,
            artifacts: result.artifacts,
            edits: result.edits,
        },
        Ok(Err(e)) => {
            let raw = e.to_string();
            let enriched = enrich_tool_error(&tool_name, &raw);
            ToolCallOutcome::Error { message: enriched }
        }
        Err(panic_payload) => {
            let panic_msg = if let Some(s) = panic_payload.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = panic_payload.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "unknown panic".to_string()
            };
            tracing::error!(tool = %tool_name, "Tool panicked: {}", panic_msg);
            ToolCallOutcome::Error {
                message: format!(
                    "Tool '{}' panicked: {}. The tool crashed but the session continues.",
                    tool_name, panic_msg
                ),
            }
        }
    }
}

/// Return a human-readable JSON type name for error messages.
fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

/// Semantic validation layer: domain-specific invariant checks on tool args.
///
/// This layer proves *domain correctness*, complementing:
///   - Schema validator (structural correctness)
///   - PolicyKernel capability checker (authorization)
///
/// Cost: O(k) where k is the number of predicates for the given tool.
fn validate_tool_semantics(
    tool_name: &str,
    args: &serde_json::Value,
    ctx: &ToolContext,
) -> Result<(), String> {
    match tool_name {
        "edit_file" | "write_file" | "read_file" | "multi_edit_file" => {
            // Path must resolve within project root
            if let Some(path_str) = args.get("path").and_then(|v| v.as_str()) {
                let path = std::path::Path::new(path_str);
                let resolved = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    ctx.current_dir().join(path)
                };
                // Canonicalize to resolve .. and symlinks
                if let Ok(canonical) = resolved.canonicalize() {
                    if !canonical.starts_with(&ctx.project_root) {
                        return Err(format!(
                            "Path '{}' resolves outside project root '{}'",
                            path_str,
                            ctx.project_root.display()
                        ));
                    }
                }
                // Check for obvious traversal even if canonicalize fails (file may not exist yet)
                let path_str_normalized = path_str.replace('\\', "/");
                if path_str_normalized.contains("/../")
                    || path_str_normalized.starts_with("../")
                    || path_str_normalized.ends_with("/..")
                {
                    return Err(format!("Path '{}' contains traversal sequences", path_str));
                }
            }
        }
        "bash" => {
            // Timeout must be within policy bounds
            if let Some(timeout) = args.get("timeout").and_then(|v| v.as_u64()) {
                if timeout > 600 {
                    return Err(format!(
                        "Timeout {} exceeds maximum allowed (600s)",
                        timeout
                    ));
                }
            }
        }
        _ => {
            // Default: no additional semantic validation for unknown tools
        }
    }
    Ok(())
}

/// Fix #12: Compute cost from usage based on provider pricing
fn compute_cost(
    provider_id: &str,
    usage: &pipit_provider::UsageMetadata,
    pricing: &PricingConfig,
) -> f64 {
    let Some(provider_pricing) = pricing.pricing_for(provider_id) else {
        return 0.0;
    };

    let input_price = provider_pricing.input_per_million / 1_000_000.0;
    let output_price = provider_pricing.output_per_million / 1_000_000.0;
    let cache_read_price = provider_pricing.cache_read_per_million / 1_000_000.0;

    (usage.input_tokens as f64 * input_price)
        + (usage.output_tokens as f64 * output_price)
        + (usage.cache_read_tokens.unwrap_or(0) as f64 * cache_read_price)
}
