use crate::events::{AgentEvent, AgentOutcome, ApprovalDecision, ApprovalHandler, ToolCallOutcome, TurnEndReason};
use crate::capability::{PolicyKernel, CapabilityRequest, CapabilitySet, PolicyDecision, ResourceScope, ExecutionLineage};
use crate::governor::{Governor, RiskReport};
use crate::ledger::{SessionLedger, SessionEvent};
use crate::loop_detector::LoopDetector;
use crate::session_kernel::{SessionKernel, SessionKernelConfig};
use crate::turn_kernel::{TurnKernel, TurnInput, TurnOutput, TurnPhase};
use crate::telemetry_facade::{TelemetryFacade, SpanStatus, SpanValue};
use crate::pev::{ModelRouter, PevConfig};
use crate::planner::{CandidatePlan, Planner, PlanStrategy, VerifyStrategy};
use crate::proof::{
    ChangeClaim, ConfidenceReport, EvidenceArtifact, Objective, PlanPivot, ProofPacket,
    PolicyStage, RealizedEdit, VerificationKind,
};
use crate::tool_semantics::{builtin_semantics, classify_semantically, SemanticClass};
use crate::verifier::Verifier;
use pipit_context::ContextManager;
use pipit_extensions::ExtensionRunner;
use pipit_config::{ApprovalMode, PricingConfig};
use pipit_provider::{
    AssistantResponse, CompletionRequest, ContentEvent, LlmProvider, Message, StopReason,
};
use pipit_tools::{ToolContext, ToolRegistry};
use futures::StreamExt;
use futures::FutureExt;
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
    fn new(objective: Objective, selected_plan: CandidatePlan, candidate_plans: Vec<CandidatePlan>) -> Self {
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

        let loop_detector =
            LoopDetector::new(config.loop_detection_window, config.loop_detection_threshold);

        let tool_context = ToolContext::new(project_root.clone(), config.approval_mode);

        let policy_kernel = PolicyKernel::from_approval_mode(config.approval_mode, project_root);

        let session_id = uuid::Uuid::new_v4().to_string();
        let model_name = models.for_role(crate::pev::ModelRole::Executor).model_id.clone();
        let provider_name = models.for_role(crate::pev::ModelRole::Executor).provider.id().to_string();
        let telemetry = Arc::new(TelemetryFacade::new(&session_id, &model_name, &provider_name));
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
                    SessionEvent::ToolCallProposed { call_id, tool_name, args } => {
                        let _ = kernel.propose_tool_call(call_id, tool_name, args);
                    }
                    SessionEvent::ToolApproved { call_id } => {
                        let _ = kernel.approve_tool(call_id);
                    }
                    SessionEvent::ToolStarted { call_id } => {
                        let _ = kernel.start_tool(call_id);
                    }
                    SessionEvent::ToolCompleted { call_id, success, mutated, result_summary, result_blob_hash } => {
                        let _ = kernel.complete_tool(call_id, *success, *mutated, result_summary, result_blob_hash.clone());
                    }
                    SessionEvent::ContextCompressed { messages_removed, tokens_freed, strategy } => {
                        let _ = kernel.record_compression(*messages_removed, *tokens_freed, strategy);
                    }
                    SessionEvent::PlanSelected { strategy, rationale } => {
                        let _ = kernel.select_plan(strategy, rationale);
                    }
                    SessionEvent::PlanPivoted { from_strategy, to_strategy, trigger } => {
                        let _ = kernel.pivot_plan(from_strategy, to_strategy, trigger);
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

    /// Whether any tool calls have been executed in the current context.
    /// Used to distinguish first-turn Q&A from multi-turn coding tasks.
    fn has_had_tool_calls(&self) -> bool {
        self.context.messages().iter().any(|msg| {
            msg.content.iter().any(|block| {
                matches!(block, pipit_provider::ContentBlock::ToolCall { .. })
            })
        })
    }

    /// Convenience: get the executor provider (the default for the main loop).
    fn provider(&self) -> &Arc<dyn LlmProvider> {
        &self.models.for_role(crate::pev::ModelRole::Executor).provider
    }

    /// Hot-swap the model at runtime (from /model command).
    /// Creates a new provider with the given model string, keeping the same provider kind and API key.
    pub fn set_model(&mut self, provider_kind: pipit_config::ProviderKind, model: &str, api_key: &str, base_url: Option<&str>) -> Result<(), String> {
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
        self.policy_kernel = PolicyKernel::from_approval_mode(
            mode,
            self.tool_context.project_root.clone(),
        );
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

    /// Run the agent loop for a single user message.
    pub async fn run(
        &mut self,
        user_message: String,
        cancel: CancellationToken,
    ) -> AgentOutcome {
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
            candidate_plans =
                planner.candidate_plans(&objective, &claim.confidence, &evidence);
            selected_plan =
                planner.select_plan(&objective, &claim.confidence, &evidence);
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

        // Add user message to context
        self.context.push_message(Message::user(&processed));
        // Drive turn kernel: user message → planning → requesting
        let _outputs = self.turn_kernel.transition(TurnInput::UserMessage(processed.clone()));
        self.emit(AgentEvent::TurnStart { turn_number: 0 });

        let mut turn = 0u32;
        /// How many bonus turns to grant when the agent has recent forward progress.
        const GRACE_TURNS: u32 = 3;
        /// How many turns before the limit to inject a wind-down warning.
        const WINDDOWN_WARNING_TURNS: u32 = 3;
        /// Track the last turn that produced a file mutation (forward progress).
        let mut last_mutation_turn: u32 = 0;
        /// Whether we're in the grace period (past max_turns but still active).
        let mut in_grace_period = false;
        /// Whether the wind-down warning has already been injected.
        let mut winddown_warned = false;

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
                match self.context.compress(&*compress_provider, cancel.clone()).await {
                    Ok(stats) => {
                        self.emit(AgentEvent::CompressionEnd {
                            messages_removed: stats.messages_removed,
                            tokens_freed: stats.tokens_freed,
                        });
                    }
                    Err(e) => {
                        tracing::warn!("Compression failed: {}", e);
                    }
                }
            }

            // ── Smart turn limit with grace period ──
            // Instead of hard-stopping at max_turns, we:
            // 1. Warn the model WINDDOWN_WARNING_TURNS before the limit
            // 2. At the limit: if recent forward progress, grant GRACE_TURNS to wrap up
            // 3. Hard-stop at max_turns + GRACE_TURNS regardless
            turn += 1;
            let hard_limit = self.config.max_turns + GRACE_TURNS;

            if turn > hard_limit {
                // Hard cap — no more extensions
                let usage = self.context.token_usage();
                self.emit(AgentEvent::TokenUsageUpdate {
                    used: usage.total,
                    limit: usage.limit,
                    cost: usage.cost,
                });
                return AgentOutcome::MaxTurnsReached(turn);
            }

            if turn > self.config.max_turns && !in_grace_period {
                // Check if the agent had recent forward progress (mutation in last 3 turns)
                let recent_progress = last_mutation_turn > 0
                    && (turn - last_mutation_turn) <= WINDDOWN_WARNING_TURNS;

                if recent_progress {
                    // Grant grace period — agent is actively making edits
                    in_grace_period = true;
                    self.context.push_message(Message::user(
                        "[SYSTEM] You have reached the turn limit, but you have been making progress. \
                         You have been granted a few extra turns to wrap up. \
                         IMPORTANT: Finish your current task NOW. Do not start new work. \
                         Complete any in-progress edits, run a final verification if needed, \
                         and then stop."
                    ));
                    self.emit(AgentEvent::Waiting {
                        label: format!("Grace period — {} bonus turns to finish", GRACE_TURNS),
                    });
                    tracing::info!(
                        turn, last_mutation_turn,
                        "Smart turn limit: granting grace period ({} bonus turns)",
                        GRACE_TURNS
                    );
                } else {
                    // No recent progress — hard stop
                    let usage = self.context.token_usage();
                    self.emit(AgentEvent::TokenUsageUpdate {
                        used: usage.total,
                        limit: usage.limit,
                        cost: usage.cost,
                    });
                    return AgentOutcome::MaxTurnsReached(turn);
                }
            }

            // Wind-down warning: inject a gentle nudge a few turns before the limit
            if !winddown_warned
                && !in_grace_period
                && self.config.max_turns > WINDDOWN_WARNING_TURNS
                && turn == self.config.max_turns - WINDDOWN_WARNING_TURNS + 1
            {
                winddown_warned = true;
                self.context.push_message(Message::user(&format!(
                    "[SYSTEM] You have {} turns remaining before the limit. \
                     Start wrapping up: finish current edits, verify your changes, \
                     and prepare to conclude.",
                    self.config.max_turns - turn
                )));
            }

            self.emit(AgentEvent::TurnStart { turn_number: turn });

            // Build completion request — show ALL tools to the model.
            // Approval gating happens at execution time, not discovery time.
            let tools = self.tools.declarations();
            if !evidence.is_empty() {
                claim.confidence = verifier.summarize_confidence(&evidence, &realized_edits);
                let previous_plan = selected_plan.clone();
                candidate_plans =
                    planner.candidate_plans(&objective, &claim.confidence, &evidence);
                selected_plan =
                    planner.select_plan(&objective, &claim.confidence, &evidence);
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
                            current_cost, max_budget, current_cost / max_budget * 100.0
                        ),
                    });
                }
            }

            // Stream the LLM response
            self.emit(AgentEvent::Waiting { label: "Sending to model\u{2026}".to_string() });
            let mut llm_span = self.telemetry.start_span("llm.complete")
                .attr("model.name", SpanValue::String(
                    self.models.for_role(crate::pev::ModelRole::Executor).model_id.clone()
                ))
                .attr("turn", SpanValue::Int(turn as i64));
            let response = match self
                .stream_response_with_recovery(
                    &claim,
                    &selected_plan,
                    &tools,
                    cancel.clone(),
                )
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

            // Track cost via telemetry facade (Kahan summation for precision)
            if response.stop_reason.is_some() {
                let cost = compute_cost(self.provider().id(), &response.usage, &self.config.pricing);
                self.context.add_cost(cost);
                self.telemetry.session_counters.add_cost(cost);
                self.telemetry.session_counters.add_tokens(
                    response.usage.input_tokens,
                    response.usage.output_tokens,
                );
            }
            self.telemetry.session_counters.increment_turns();

            // ── Closed-loop telemetry feedback ──
            // Feed turn observations into the controller and check for control signals.
            {
                let ttft_ms = None; // TODO: wire from QueryProfiler checkpoints
                let tool_calls_this_turn = response.tool_calls.len() as u32;
                self.telemetry_controller.observe_turn(ttft_ms, 0, tool_calls_this_turn);

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

            // Add assistant response to context
            self.context.push_message(response.to_message());

            // Handle stop reason
            match response.stop_reason.unwrap_or(StopReason::EndTurn) {
                StopReason::EndTurn | StopReason::Stop => {
                    // Drive turn kernel: response complete (no tools)
                    let _tk_outputs = self.turn_kernel.transition(TurnInput::ResponseComplete);

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

                    return AgentOutcome::Completed {
                        turns: turn,
                        total_tokens: usage.total,
                        cost: usage.cost,
                        proof,
                    };
                }
                StopReason::ToolUse => {
                    // Execute tool calls with turn-level timeout
                    let tool_calls = response.tool_calls.clone();
                    let turn_timeout = std::time::Duration::from_secs(
                        self.config.tool_timeout_secs.max(30) * (tool_calls.len() as u64).max(1)
                    );
                    let tool_future = self.execute_tools(
                        &tool_calls, cancel.clone(), &governor, &claim.confidence
                    );
                    let (results, modified_files, artifacts, edits, tool_risk) = tokio::select! {
                        result = tool_future => result,
                        _ = tokio::time::sleep(turn_timeout) => {
                            self.emit(AgentEvent::ProviderError {
                                error: format!("Tool execution timed out after {}s", turn_timeout.as_secs()),
                                will_retry: false,
                            });
                            // Push timeout error as tool results so the agent knows what happened
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
                            turn += 1;
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

                    // Drive turn kernel: tool calls completed
                    let _tk_outputs = self.turn_kernel.transition(TurnInput::AllToolsCompleted {
                        modified_files: modified_files.clone(),
                    });

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
                        last_mutation_turn = turn;
                    }

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

                        self.context.push_message(Message::user(&build_loop_recovery_message(
                            &name,
                            count,
                            &evidence,
                            self.consecutive_loop_hits,
                        )));
                    } else {
                        // Reset counter when no loop is detected
                        self.consecutive_loop_hits = 0;
                    }

                    // Push tool results to context (with truncation)
                    for (call_id, result) in results {
                        let (content, is_error) = match &result {
                            ToolCallOutcome::Success { content, .. } => {
                                (content.clone(), false)
                            }
                            ToolCallOutcome::PolicyBlocked { message, .. } => {
                                (message.clone(), true)
                            }
                            ToolCallOutcome::Error { message } => {
                                (message.clone(), true)
                            }
                        };
                        self.context.push_tool_result(&call_id, &content, is_error);
                    }

                    self.emit(AgentEvent::TurnEnd {
                        turn_number: turn,
                        reason: TurnEndReason::ToolsExecuted,
                    });
                    if let Err(e) = self.extensions.on_turn_end(&modified_files).await {
                        tracing::warn!("TurnEnd extension hook failed: {}", e);
                    }

                    // Post-edit verification: auto-run lint/test if files were mutated
                    if !modified_files.is_empty() {
                        self.emit(AgentEvent::Waiting { label: "Running verification\u{2026}".to_string() });
                        let verification_results =
                            self.run_post_edit_verification(&modified_files, cancel.clone()).await;
                        for (cmd, output, success) in &verification_results {
                            evidence.push(EvidenceArtifact::CommandResult {
                                kind: classify_verification_command(cmd),
                                command: cmd.clone(),
                                output: truncate(output, 500),
                                success: *success,
                            });
                        }
                        // If verification failed, inject the failure into context
                        // so the agent sees it and can fix the issue
                        for (cmd, output, success) in &verification_results {
                            if !success {
                                self.context.push_message(Message::user(&format!(
                                    "[Auto-verification failed]\n$ {}\n{}",
                                    cmd, truncate(output, 1000)
                                )));
                            }
                        }
                    }

                    // Continue the loop
                    self.emit(AgentEvent::Waiting { label: "Preparing next turn\u{2026}".to_string() });
                }
                StopReason::MaxTokens => {
                    // Fix #14: Continue generation via assistant prefill if supported
                    if self.provider().capabilities().supports_prefill && !response.text.is_empty() {
                        // Append partial text as assistant prefill and loop again
                        self.context.push_message(Message::user(
                            "Continue from where you left off. Your previous response was truncated."
                        ));
                        // Continue the loop to get more output
                    } else {
                        self.emit(AgentEvent::TurnEnd {
                            turn_number: turn,
                            reason: TurnEndReason::Complete,
                        });
                        if let Err(e) = self.extensions.on_turn_end(&[]).await {
                            tracing::warn!("TurnEnd extension hook failed: {}", e);
                        }
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
                            turns: turn, total_tokens: usage.total, cost: usage.cost, proof,
                        };
                    }
                }
                StopReason::Error => {
                    return AgentOutcome::Error("LLM returned error stop reason".to_string());
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
        let mut stream = self.provider().complete(request.clone(), cancel.clone()).await?;
        let mut response = AssistantResponse::new();
        let mut has_tool_calls = false;

        while let Some(event) = stream.next().await {
            if cancel.is_cancelled() {
                return Err(pipit_provider::ProviderError::Cancelled);
            }

            match event {
                Ok(ContentEvent::ContentDelta { text }) => {
                    response.push_text(&text);
                    self.emit(AgentEvent::ContentDelta { text: text.clone() });
                    if let Err(e) = self.extensions.on_content_delta(&text).await {
                        tracing::warn!("ContentDelta extension hook failed: {}", e);
                    }
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
                    response.push_tool_call(call_id.clone(), tool_name.clone(), args.clone());
                    self.emit(AgentEvent::ToolCallStart {
                        call_id,
                        name: tool_name,
                        args,
                    });
                }
                Ok(ContentEvent::Finished {
                    stop_reason,
                    usage,
                }) => {
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
                            label: "Network interrupted — reconnecting with partial response…".to_string(),
                        });

                        // Build continuation request: inject partial text as assistant message
                        // so the model continues from where it left off.
                        let mut continuation = request.clone();
                        continuation.messages.push(Message::assistant(&response.text));
                        continuation.messages.push(Message::user(
                            "[SYSTEM] Your previous response was interrupted by a network error. \
                             The partial response above has been preserved. Please continue from \
                             where you left off. Do not repeat what you already said."
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
                                            self.emit(AgentEvent::ContentDelta { text: text.clone() });
                                        }
                                        Ok(ContentEvent::ThinkingDelta { text }) => {
                                            response.push_thinking(&text);
                                            self.emit(AgentEvent::ThinkingDelta { text });
                                        }
                                        Ok(ContentEvent::ToolCallComplete { call_id, tool_name, args }) => {
                                            has_tool_calls = true;
                                            response.push_tool_call(call_id.clone(), tool_name.clone(), args.clone());
                                            self.emit(AgentEvent::ToolCallStart { call_id, name: tool_name, args });
                                        }
                                        Ok(ContentEvent::Finished { stop_reason, usage }) => {
                                            response.finish(stop_reason, usage);
                                        }
                                        Ok(_) => {}
                                        Err(e2) => {
                                            tracing::warn!("Reconnection stream also failed: {}", e2);
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
        let (input_est, model_limit, over) = self.context.preflight_check(
            tools.len(),
            self.repo_map.as_deref(),
        );
        if over > 0 {
            tracing::warn!(
                "Pre-flight: estimated {} tokens vs {} limit (over by {}). Reducing.",
                input_est, model_limit, over
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
            let (_est2, _lim2, over2) = self.context.preflight_check(tools.len(), self.repo_map.as_deref());
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
            let request = self.build_completion_request(claim, selected_plan, tools).await;

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
                            err,
                            reduction.summary
                        ),
                        will_retry: true,
                    });
                }
                Err(err) if err.is_transient() && attempts < 3 && self.telemetry.session_counters.can_retry() => {
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

        if let Ok(Some(modified_system)) = self.extensions.on_before_request(&request.system).await {
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

        let lineage = ExecutionLineage::default();

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

            let decision = self.policy_kernel.evaluate(
                &call.tool_name,
                &cap_request,
                &lineage,
            );

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
                        path: call.args.get("path").and_then(|v| v.as_str()).map(str::to_string),
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
                                        path: call.args.get("path").and_then(|v| v.as_str()).map(str::to_string),
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
            if cancel.is_cancelled() { break; }

            let batch_calls: Vec<&pipit_provider::ToolCall> = batch.indices.iter()
                .map(|&i| &all_approved[i])
                .collect();

            if batch_calls.len() == 1 || !batch.all_read_only {
                // Sequential: execute one at a time (writes, or mixed)
                for call in batch_calls {
                    if cancel.is_cancelled() { break; }

                    let modified_args = match self.extensions.on_before_tool(&call.tool_name, &call.args).await {
                        Ok(Some(new_args)) => new_args,
                        Ok(None) => call.args.clone(),
                        Err(err) => {
                            let outcome = ToolCallOutcome::Error { message: err.to_string() };
                            evidence.push(evidence_from_tool(call, &outcome));
                            self.emit(AgentEvent::ToolCallEnd {
                                call_id: call.call_id.clone(), name: call.tool_name.clone(), result: outcome.clone(),
                            });
                            results.push((call.call_id.clone(), outcome));
                            continue;
                        }
                    };
                    let mut modified_call = call.clone();
                    modified_call.args = modified_args;

                    let mut tool_span = self.telemetry.start_span("tool.execute")
                        .attr("tool.name", SpanValue::String(call.tool_name.clone()));
                    let outcome = execute_single_tool(&self.tools, &modified_call, &self.tool_context, cancel.clone()).await;
                    let success = matches!(outcome, ToolCallOutcome::Success { .. });
                    tool_span.finish(if success { SpanStatus::Ok } else { SpanStatus::Error });
                    self.telemetry.record_span(tool_span);
                    self.telemetry.session_counters.increment_tool_calls();
                    let mutation_applied = matches!(outcome, ToolCallOutcome::Success { mutated: true, .. });
                    let outcome = apply_after_tool_hook(&*self.extensions, &call.tool_name, outcome).await;
                    if mutation_applied {
                        if let Some(path) = call.args.get("path").and_then(|value| value.as_str()) {
                            modified_files.push(path.to_string());
                            realized_edits.push(RealizedEdit {
                                path: path.to_string(),
                                summary: summarize_tool_outcome(&call.tool_name, &outcome),
                            });
                        }
                    }
                    evidence.push(evidence_from_tool(call, &outcome));
                    self.emit(AgentEvent::ToolCallEnd {
                        call_id: call.call_id.clone(), name: call.tool_name.clone(), result: outcome.clone(),
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
                        async move {
                            let modified_args = match extensions.on_before_tool(&call.tool_name, &call.args).await {
                                Ok(Some(new_args)) => new_args,
                                Ok(None) => call.args.clone(),
                                Err(err) => {
                                    return (call.call_id.clone(), ToolCallOutcome::Error { message: err.to_string() });
                                }
                            };
                            let mut modified_call = call.clone();
                            modified_call.args = modified_args;
                            let outcome = execute_single_tool(&tools, &modified_call, &ctx, cancel).await;
                            let outcome = apply_after_tool_hook(&*extensions, &call.tool_name, outcome).await;
                            (call.call_id.clone(), outcome)
                        }
                    })
                    .collect();

                let batch_results = futures::future::join_all(concurrent_futures).await;
                for (call_id, outcome) in &batch_results {
                    let name = calls.iter().find(|c| c.call_id == *call_id)
                        .map(|c| c.tool_name.as_str()).unwrap_or("unknown");
                    self.emit(AgentEvent::ToolCallEnd {
                        call_id: call_id.clone(), name: name.to_string(), result: outcome.clone(),
                    });
                    if let Some(call) = calls.iter().find(|c| c.call_id == *call_id) {
                        evidence.push(evidence_from_tool(call, outcome));
                    }
                }
                results.extend(batch_results);
            }
        }

        (results, modified_files, evidence, realized_edits, highest_risk)
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
            AgentEvent::ToolCallStart { call_id, name, args } => {
                self.record(SessionEvent::ToolCallProposed {
                    call_id: call_id.clone(),
                    tool_name: name.clone(),
                    args: args.clone(),
                });
            }
            AgentEvent::ToolCallEnd { call_id, name, result, .. } => {
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
            AgentEvent::CompressionEnd { messages_removed, tokens_freed } => {
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
                self.record(SessionEvent::UserMessageAccepted { content: text.clone() });
            }
            AgentEvent::PlanSelected { strategy, rationale, pivoted, .. } => {
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
        let turns = self.telemetry.session_counters.turns.load(std::sync::atomic::Ordering::Relaxed) as u32;
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
        let json = serde_json::to_string_pretty(self.context.messages())
            .map_err(|e| e.to_string())?;
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
        self.context
            .compress(&*provider, cancel)
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
                    results.push((
                        cmd.to_string(),
                        combined,
                        output.status.success(),
                    ));
                }
                Err(e) => {
                    results.push((
                        cmd.to_string(),
                        format!("Failed to run: {}", e),
                        false,
                    ));
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
            format!(
                "{} {}: {}",
                tool_name,
                stage_label,
                truncate(message, 160)
            )
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
    if let ToolCallOutcome::PolicyBlocked {
        stage,
        mutated,
        ..
    } = outcome
    {
        return EvidenceArtifact::PolicyViolation {
            tool_name: call.tool_name.clone(),
            stage: stage.clone(),
            summary: summarize_tool_outcome(&call.tool_name, outcome),
            mutation_applied: *mutated,
            path: call.args.get("path").and_then(|value| value.as_str()).map(str::to_string),
        };
    }

    let semantic_class = classify_semantically(&call.tool_name, &call.args);

    match semantic_class {
        SemanticClass::Read { paths } => {
            EvidenceArtifact::FileRead {
                path: paths.into_iter().next(),
                summary: summarize_tool_outcome(&call.tool_name, outcome),
            }
        }
        SemanticClass::Search { .. } | SemanticClass::Pure => {
            EvidenceArtifact::FileRead {
                path: call.args.get("path").and_then(|v| v.as_str()).map(str::to_string),
                summary: summarize_tool_outcome(&call.tool_name, outcome),
            }
        }
        SemanticClass::Edit { paths } => {
            EvidenceArtifact::EditApplied {
                path: paths.into_iter().next(),
                summary: summarize_tool_outcome(&call.tool_name, outcome),
            }
        }
        SemanticClass::Exec { command } => {
            EvidenceArtifact::CommandResult {
                kind: classify_verification_command(&command),
                command,
                output: match outcome {
                    ToolCallOutcome::Success { content, .. } => truncate(content, 240),
                    ToolCallOutcome::PolicyBlocked { message, .. } => truncate(message, 240),
                    ToolCallOutcome::Error { message } => truncate(message, 240),
                },
                success: matches!(outcome, ToolCallOutcome::Success { .. }),
            }
        }
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
    let modified_files: Vec<String> = realized_edits.iter().map(|edit| edit.path.clone()).collect();
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
        format!("{}...", &input[..max_len])
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
        ) => Some((command.clone(), output.clone())),
        _ => None,
    })
}

pub(crate) async fn apply_after_tool_hook(
    extensions: &dyn ExtensionRunner,
    tool_name: &str,
    outcome: ToolCallOutcome,
) -> ToolCallOutcome {
    match outcome {
        ToolCallOutcome::Success { content, mutated } => match extensions.on_after_tool(tool_name, &content).await {
            Ok(Some(note)) => ToolCallOutcome::Success {
                content: format!("{}\n\n[Hook]\n{}", content, note),
                mutated,
            },
            Ok(None) => ToolCallOutcome::Success { content, mutated },
            Err(err) => ToolCallOutcome::PolicyBlocked {
                message: err.to_string(),
                stage: PolicyStage::PostToolUse,
                mutated,
            },
        },
        other => other,
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
                message: "Hook blocked tool execution: post-hook rejected formatted output".to_string(),
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
                message: "Hook blocked tool execution: formatter policy rejected change".to_string(),
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
        assert!(is_request_too_large_error(&pipit_provider::ProviderError::Other(
            "HTTP 413 Payload Too Large: length limit exceeded".to_string(),
        )));
        assert!(!is_request_too_large_error(&pipit_provider::ProviderError::Other(
            "HTTP 500 internal error".to_string(),
        )));
    }
}

pub(crate) async fn execute_single_tool(
    tools: &ToolRegistry,
    call: &pipit_provider::ToolCall,
    ctx: &ToolContext,
    cancel: CancellationToken,
) -> ToolCallOutcome {
    let tool = match tools.get(&call.tool_name) {
        Some(t) => t,
        None => return ToolCallOutcome::Error { message: format!("Tool not found: {}", call.tool_name) },
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
                        message: format!("Missing required argument '{}' for tool '{}'", field_name, call.tool_name),
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
                                prop_name, call.tool_name, expected_type,
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

    // Wrap tool execution in catch_unwind to isolate panics.
    // A panicking tool produces an error result instead of crashing the session.
    let tool_name = call.tool_name.clone();
    let args = call.args.clone();
    let execute_future = tool.execute(args, ctx, cancel);

    match std::panic::AssertUnwindSafe(execute_future).catch_unwind().await {
        Ok(Ok(result)) => ToolCallOutcome::Success { content: result.content, mutated: result.mutated },
        Ok(Err(e)) => ToolCallOutcome::Error { message: e.to_string() },
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
                message: format!("Tool '{}' panicked: {}. The tool crashed but the session continues.", tool_name, panic_msg),
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
                    return Err(format!(
                        "Path '{}' contains traversal sequences",
                        path_str
                    ));
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
