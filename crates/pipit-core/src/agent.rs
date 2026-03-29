use crate::events::{AgentEvent, AgentOutcome, ApprovalDecision, ApprovalHandler, ToolCallOutcome, TurnEndReason};
use crate::governor::{Governor, RiskReport};
use crate::loop_detector::LoopDetector;
use crate::pev::{ModelRouter, PevConfig};
use crate::planner::{CandidatePlan, Planner, PlanStrategy, VerifyStrategy};
use crate::proof::{
    ChangeClaim, ConfidenceReport, EvidenceArtifact, Objective, PlanPivot, ProofPacket,
    PolicyStage, RealizedEdit, VerificationKind,
};
use crate::verifier::Verifier;
use pipit_context::ContextManager;
use pipit_extensions::ExtensionRunner;
use pipit_config::{ApprovalMode, PricingConfig};
use pipit_provider::{
    AssistantResponse, CompletionRequest, ContentEvent, LlmProvider, Message, StopReason,
};
use pipit_tools::{ToolContext, ToolRegistry};
use futures::StreamExt;
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
        }
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningState {
    pub selected_plan: CandidatePlan,
    pub candidate_plans: Vec<CandidatePlan>,
    pub plan_pivots: Vec<PlanPivot>,
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

        let tool_context = ToolContext::new(project_root, config.approval_mode);

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
        };

        (agent, event_rx, steering_tx)
    }

    pub fn set_repo_map(&mut self, map: String) {
        self.repo_map = Some(map);
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

        // Only emit plan selected event if this isn't a trivial Q&A
        if !is_qa {
            self.emit_plan_selected(&selected_plan, &candidate_plans, false);
        }

        // Add user message to context
        self.context.push_message(Message::user(&processed));
        self.emit(AgentEvent::TurnStart { turn_number: 0 });

        let mut turn = 0u32;

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

            // Check turn limit
            turn += 1;
            if turn > self.config.max_turns {
                let usage = self.context.token_usage();
                self.emit(AgentEvent::TokenUsageUpdate {
                    used: usage.total,
                    limit: usage.limit,
                    cost: usage.cost,
                });
                return AgentOutcome::MaxTurnsReached(turn);
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
                    plan_pivots.push(PlanPivot {
                        turn_number: turn,
                        from: previous_plan.clone(),
                        to: selected_plan.clone(),
                        trigger: format!(
                            "Plan changed after verification evidence update: {}",
                            selected_plan.rationale
                        ),
                    });
                    self.emit_plan_selected(&selected_plan, &candidate_plans, true);
                }
                claim.align_with_plan(&selected_plan);
                self.update_planning_state(&selected_plan, &candidate_plans, &plan_pivots);
            }
            // Stream the LLM response
            self.emit(AgentEvent::Waiting { label: "Sending to model\u{2026}".to_string() });
            let response = match self
                .stream_response_with_recovery(
                    &claim,
                    &selected_plan,
                    &tools,
                    cancel.clone(),
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    self.emit(AgentEvent::ProviderError {
                        error: e.to_string(),
                        will_retry: false,
                    });
                    return AgentOutcome::Error(e.to_string());
                }
            };

            // Fix #12: Track cost from response usage
            if response.stop_reason.is_some() {
                let cost = compute_cost(self.provider().id(), &response.usage, &self.config.pricing);
                self.context.add_cost(cost);
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
                    let proof = finalize_proof(
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
                    );
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
                    evidence.extend(artifacts);
                    realized_edits.extend(edits);
                    if tool_risk.score > risk.score {
                        risk = tool_risk;
                    }

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
                        let proof = finalize_proof(
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
                        );
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
        let mut stream = self.provider().complete(request, cancel.clone()).await?;
        let mut response = AssistantResponse::new();

        while let Some(event) = stream.next().await {
            if cancel.is_cancelled() {
                return Err(pipit_provider::ProviderError::Cancelled);
            }

            match event? {
                ContentEvent::ContentDelta { text } => {
                    response.push_text(&text);
                    self.emit(AgentEvent::ContentDelta { text: text.clone() });
                    if let Err(e) = self.extensions.on_content_delta(&text).await {
                        tracing::warn!("ContentDelta extension hook failed: {}", e);
                    }
                }
                ContentEvent::ThinkingDelta { text } => {
                    response.push_thinking(&text);
                    self.emit(AgentEvent::ThinkingDelta { text });
                }
                ContentEvent::ToolCallComplete {
                    call_id,
                    tool_name,
                    args,
                } => {
                    response.push_tool_call(call_id.clone(), tool_name.clone(), args.clone());
                    self.emit(AgentEvent::ToolCallStart {
                        call_id,
                        name: tool_name,
                        args,
                    });
                }
                ContentEvent::Finished {
                    stop_reason,
                    usage,
                } => {
                    response.finish(stop_reason, usage);
                }
                _ => {}
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
            // Step 1: shrink old tool results
            let freed = self.context.shrink_tool_results(500);
            if freed > 0 {
                self.emit(AgentEvent::Waiting {
                    label: format!("Freed ~{} tokens from old tool results", freed),
                });
            }
            // Step 2: check again, if still over, drop repo map + compress
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
                Ok(response) => return Ok(response),
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

            if tool.requires_approval(self.tool_context.approval_mode) {
                self.emit(AgentEvent::ToolApprovalNeeded {
                    call_id: call.call_id.clone(),
                    name: call.tool_name.clone(),
                    args: call.args.clone(),
                });

                // Block until the user responds
                let decision = self
                    .approval_handler
                    .request_approval(&call.call_id, &call.tool_name, &call.args)
                    .await;

                match decision {
                    ApprovalDecision::Approve => {
                        // Fall through to execute the tool below
                    }
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
                            reason: "User denied approval".to_string(),
                        });
                        continue;
                    }
                }
            }

            if tool.is_mutating() {
                approved_writes.push(call.clone());
            } else {
                approved_reads.push(call.clone());
            }
        }

        // Run reads concurrently
        let read_futures: Vec<_> = approved_reads
            .into_iter()
            .map(|call| {
                let call = call.clone();
                let tools = self.tools.clone();
                let ctx = self.tool_context.clone();
                let cancel = cancel.clone();
                let extensions = self.extensions.clone();
                async move {
                    // Fix #4: Call extension hooks before/after tool execution
                    let modified_args = match extensions.on_before_tool(&call.tool_name, &call.args).await {
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
                    let outcome = execute_single_tool(&tools, &modified_call, &ctx, cancel).await;
                    let outcome = apply_after_tool_hook(&*extensions, &call.tool_name, outcome).await;
                    (call.call_id.clone(), outcome)
                }
            })
            .collect();

        let read_results = futures::future::join_all(read_futures).await;
        for (call_id, outcome) in &read_results {
            let name = calls.iter().find(|c| c.call_id == *call_id)
                .map(|c| c.tool_name.as_str()).unwrap_or("unknown");
            self.emit(AgentEvent::ToolCallEnd {
                call_id: call_id.clone(), name: name.to_string(), result: outcome.clone(),
            });
            if let Some(call) = calls.iter().find(|c| c.call_id == *call_id) {
                evidence.push(evidence_from_tool(call, outcome));
            }
        }
        results.extend(read_results);

        // Run writes sequentially
        for call in approved_writes {
            if cancel.is_cancelled() { break; }

            // Fix #4: Extension hooks for writes too
            let modified_args = match self.extensions.on_before_tool(&call.tool_name, &call.args).await {
                Ok(Some(new_args)) => new_args,
                Ok(None) => call.args.clone(),
                Err(err) => {
                    let outcome = ToolCallOutcome::Error {
                        message: err.to_string(),
                    };
                    evidence.push(evidence_from_tool(&call, &outcome));
                    self.emit(AgentEvent::ToolCallEnd {
                        call_id: call.call_id.clone(), name: call.tool_name.clone(), result: outcome.clone(),
                    });
                    results.push((call.call_id.clone(), outcome));
                    continue;
                }
            };
            let mut modified_call = call.clone();
            modified_call.args = modified_args;

            let outcome = execute_single_tool(&self.tools, &modified_call, &self.tool_context, cancel.clone()).await;
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
            evidence.push(evidence_from_tool(&call, &outcome));

            self.emit(AgentEvent::ToolCallEnd {
                call_id: call.call_id.clone(), name: call.tool_name.clone(), result: outcome.clone(),
            });
            results.push((call.call_id.clone(), outcome));
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

fn summarize_tool_outcome(tool_name: &str, outcome: &ToolCallOutcome) -> String {
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

fn evidence_from_tool(
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

    match call.tool_name.as_str() {
        "read_file" => EvidenceArtifact::FileRead {
            path: call.args.get("path").and_then(|value| value.as_str()).map(str::to_string),
            summary: summarize_tool_outcome(&call.tool_name, outcome),
        },
        "bash" => EvidenceArtifact::CommandResult {
            kind: classify_verification_command(
                call.args
                    .get("command")
                    .and_then(|value| value.as_str())
                    .unwrap_or(""),
            ),
            command: call
                .args
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string(),
            output: match outcome {
                ToolCallOutcome::Success { content, .. } => truncate(content, 240),
                ToolCallOutcome::PolicyBlocked { message, .. } => truncate(message, 240),
                ToolCallOutcome::Error { message } => truncate(message, 240),
            },
            success: matches!(outcome, ToolCallOutcome::Success { .. }),
        },
        "edit_file" | "write_file" => EvidenceArtifact::EditApplied {
            path: call.args.get("path").and_then(|value| value.as_str()).map(str::to_string),
            summary: summarize_tool_outcome(&call.tool_name, outcome),
        },
        _ => EvidenceArtifact::ToolExecution {
            tool_name: call.tool_name.clone(),
            summary: summarize_tool_outcome(&call.tool_name, outcome),
            success: matches!(outcome, ToolCallOutcome::Success { .. }),
        },
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

async fn apply_after_tool_hook(
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

async fn execute_single_tool(
    tools: &ToolRegistry,
    call: &pipit_provider::ToolCall,
    ctx: &ToolContext,
    cancel: CancellationToken,
) -> ToolCallOutcome {
    // Fix #13: Validate tool args against schema before execution
    let tool = match tools.get(&call.tool_name) {
        Some(t) => t,
        None => return ToolCallOutcome::Error { message: format!("Tool not found: {}", call.tool_name) },
    };

    // Basic required-field validation from schema
    if let Some(required) = tool.schema().get("required").and_then(|r| r.as_array()) {
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

    match tool.execute(call.args.clone(), ctx, cancel).await {
        Ok(result) => ToolCallOutcome::Success { content: result.content, mutated: result.mutated },
        Err(e) => ToolCallOutcome::Error { message: e.to_string() },
    }
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
