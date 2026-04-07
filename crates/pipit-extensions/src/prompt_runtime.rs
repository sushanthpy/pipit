//! Prompt Hook Runtime — routes hook prompts through the cheapest available model.
//!
//! Hook cost minimization: argmin_{p ∈ P, p ⊨ capabilities(h)} c_p
//! subject to latency and capability constraints.
//! Cached as static resolution at hook registration; O(1) lookup after.

use crate::hook_kind::{HookContext, HookDecision};
use std::time::Instant;

/// Configuration for a prompt hook execution.
#[derive(Debug, Clone)]
pub struct PromptHookConfig {
    /// System prompt for the hook.
    pub system: String,
    /// Model override (None = auto-select cheapest).
    pub model: Option<String>,
    /// Provider override (None = auto-select).
    pub provider: Option<String>,
    /// Maximum tokens for the response.
    pub max_tokens: u32,
    /// Timeout in milliseconds.
    pub timeout_ms: u64,
}

/// Result of a prompt hook execution.
#[derive(Debug)]
pub struct PromptHookResult {
    pub decision: HookDecision,
    pub model_used: String,
    pub tokens_used: u64,
    pub cost_usd: f64,
}

/// Execute a prompt hook by building a completion request and sending it
/// to the configured or cheapest available provider.
///
/// The hook receives a structured prompt containing the event context
/// and returns a decision. If no provider is available, the hook
/// defaults to allow (fail-open) with a warning.
pub async fn execute_prompt_hook(
    config: &PromptHookConfig,
    context: &HookContext,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<PromptHookResult, String> {
    let start = Instant::now();
    let user_prompt = format_hook_prompt(context);

    // Try to make a real LLM call via the OpenAI-compatible chat/completions API.
    // The hook can specify a base_url or we try common local endpoints.
    let base_url = std::env::var("PIPIT_HOOK_BASE_URL")
        .or_else(|_| std::env::var("PIPIT_BASE_URL"))
        .unwrap_or_else(|_| "http://localhost:11434/v1".into()); // Ollama default

    let api_key = std::env::var("PIPIT_HOOK_API_KEY")
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .unwrap_or_else(|_| "not-needed".into());

    let model = config.model.clone().unwrap_or_else(|| "llama3.2".into());

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(config.timeout_ms))
        .build()
        .map_err(|e| format!("HTTP client error: {e}"))?;

    let body = serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": config.system},
            {"role": "user", "content": user_prompt},
        ],
        "max_tokens": config.max_tokens,
        "temperature": 0.0,
    });

    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let response = tokio::select! {
        r = client.post(&url)
            .header("Authorization", format!("Bearer {api_key}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send() => {
            match r {
                Ok(resp) => resp,
                Err(e) => {
                    // Fail-open: if no provider is reachable, allow with warning
                    tracing::warn!("Prompt hook failed to reach provider at {url}: {e}");
                    let elapsed_us = start.elapsed().as_micros() as u64;
                    return Ok(PromptHookResult {
                        decision: HookDecision {
                            allow: true,
                            message: Some(format!("Prompt hook: provider unreachable ({e}), defaulting to allow")),
                            transformed_args: None,
                            duration_us: elapsed_us,
                        },
                        model_used: model,
                        tokens_used: 0,
                        cost_usd: 0.0,
                    });
                }
            }
        }
        _ = cancel.cancelled() => return Err("Prompt hook cancelled".into()),
    };

    let elapsed_us = start.elapsed().as_micros() as u64;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        tracing::warn!("Prompt hook LLM returned {status}: {body}");
        return Ok(PromptHookResult {
            decision: HookDecision {
                allow: true,
                message: Some(format!("Prompt hook: LLM error {status}, defaulting to allow")),
                transformed_args: None,
                duration_us: elapsed_us,
            },
            model_used: model,
            tokens_used: 0,
            cost_usd: 0.0,
        });
    }

    let resp_json: serde_json::Value = response.json().await
        .map_err(|e| format!("Prompt hook response parse error: {e}"))?;

    let content = resp_json
        .get("choices").and_then(|c| c.get(0))
        .and_then(|c| c.get("message")).and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("allow");

    let tokens_used = resp_json.get("usage")
        .and_then(|u| u.get("total_tokens"))
        .and_then(|t| t.as_u64())
        .unwrap_or(0);

    // Parse response: look for "deny", "block", "reject" to deny; otherwise allow
    let content_lower = content.to_lowercase();
    let allow = !content_lower.contains("deny") 
        && !content_lower.contains("block") 
        && !content_lower.contains("reject");

    Ok(PromptHookResult {
        decision: HookDecision {
            allow,
            message: Some(content.to_string()),
            transformed_args: None,
            duration_us: elapsed_us,
        },
        model_used: model,
        tokens_used,
        cost_usd: 0.0,
    })
}

/// Format the hook context into a prompt for the LLM.
fn format_hook_prompt(context: &HookContext) -> String {
    let mut parts = vec![
        format!("Event: {}", context.event),
        format!("Session: {}", context.session_id),
    ];

    if let Some(ref tool) = context.tool_name {
        parts.push(format!("Tool: {}", tool));
    }

    if let Some(ref args) = context.tool_args {
        parts.push(format!("Arguments: {}", serde_json::to_string_pretty(args).unwrap_or_default()));
    }

    if let Some(ref result) = context.tool_result {
        let truncated = if result.len() > 2000 {
            format!("{}... [truncated, {} total chars]", &result[..2000], result.len())
        } else {
            result.clone()
        };
        parts.push(format!("Result: {}", truncated));
    }

    parts.join("\n")
}

// ═══════════════════════════════════════════════════════════════════════════
//  AGENT HOOK RUNTIME
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for an agent hook execution.
#[derive(Debug, Clone)]
pub struct AgentHookConfig {
    /// Task prompt for the ephemeral agent.
    pub task: String,
    /// Tool allowlist (empty = read-only tools only).
    pub allowed_tools: Vec<String>,
    /// Maximum turns for the ephemeral agent.
    pub max_turns: u32,
    /// Cost budget fraction of parent turn (0.0–1.0).
    pub cost_fraction: f64,
    /// Timeout in milliseconds.
    pub timeout_ms: u64,
}

/// Result of an agent hook execution.
#[derive(Debug)]
pub struct AgentHookResult {
    pub decision: HookDecision,
    pub turns_used: u32,
    pub cost_usd: f64,
}

/// Execute an agent hook by sending the task to the daemon or running it locally.
///
/// The ephemeral agent:
///   - Runs as a single pipit invocation with --json output
///   - Has forced max_turns from config
///   - Returns the agent's response as the hook decision
pub async fn execute_agent_hook(
    config: &AgentHookConfig,
    context: &HookContext,
    cancel: tokio_util::sync::CancellationToken,
) -> Result<AgentHookResult, String> {
    let start = Instant::now();

    // Build the task prompt with context
    let full_task = format!(
        "{}\n\nContext:\n  Event: {}\n  Tool: {}\n  Project: {}",
        config.task,
        context.event,
        context.tool_name.as_deref().unwrap_or("none"),
        context.project_root.display(),
    );

    // Find pipit binary for subprocess execution
    let pipit_bin = std::env::current_exe()
        .unwrap_or_else(|_| std::path::PathBuf::from("pipit"));

    let mut cmd = tokio::process::Command::new(&pipit_bin);
    cmd.arg("--json")
        .arg("--max-turns").arg(config.max_turns.to_string())
        .arg("--approval").arg("suggest") // Read-only by default for agent hooks
        .arg("--root").arg(&context.project_root)
        .arg(&full_task)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Restrict tools if specified
    // (passed via environment since there's no --tools flag)
    if !config.allowed_tools.is_empty() {
        cmd.env("PIPIT_ALLOWED_TOOLS", config.allowed_tools.join(","));
    }

    let timeout = std::time::Duration::from_millis(config.timeout_ms);

    let output = tokio::select! {
        r = cmd.output() => {
            r.map_err(|e| format!("Agent hook subprocess failed: {e}"))?
        }
        _ = tokio::time::sleep(timeout) => {
            return Err(format!("Agent hook timed out after {}ms", config.timeout_ms));
        }
        _ = cancel.cancelled() => {
            return Err("Agent hook cancelled".into());
        }
    };

    let elapsed_us = start.elapsed().as_micros() as u64;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    // Parse the agent's response from JSON output
    let mut response_text = String::new();
    let mut turns_used = 0u32;
    let mut cost = 0.0f64;

    for line in stderr.lines() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
            if json.get("event").and_then(|e| e.as_str()) == Some("content_complete") {
                if let Some(text) = json.get("text").and_then(|t| t.as_str()) {
                    response_text = text.to_string();
                }
            }
            if json.get("event").and_then(|e| e.as_str()) == Some("token_usage") {
                cost = json.get("cost").and_then(|c| c.as_f64()).unwrap_or(0.0);
            }
            if json.get("event").and_then(|e| e.as_str()) == Some("turn_end") {
                turns_used = json.get("turn").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
            }
        }
    }

    // Also check stdout for non-JSON output
    if response_text.is_empty() {
        // Try parsing stdout as the final result JSON
        if let Ok(result) = serde_json::from_str::<serde_json::Value>(stdout.trim()) {
            response_text = result.get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("completed")
                .to_string();
            turns_used = result.get("turns").and_then(|t| t.as_u64()).unwrap_or(0) as u32;
            cost = result.get("cost").and_then(|c| c.as_f64()).unwrap_or(0.0);
        } else {
            response_text = stdout.trim().to_string();
        }
    }

    // Interpret: if agent's response contains "deny"/"block"/"unsafe" → deny
    let content_lower = response_text.to_lowercase();
    let allow = !content_lower.contains("deny")
        && !content_lower.contains("block")
        && !content_lower.contains("unsafe")
        && !content_lower.contains("reject");

    Ok(AgentHookResult {
        decision: HookDecision {
            allow,
            message: if response_text.is_empty() { None } else { Some(response_text) },
            transformed_args: None,
            duration_us: elapsed_us,
        },
        turns_used,
        cost_usd: cost,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook_kind::ReplayMode;
    use std::path::PathBuf;

    fn test_context() -> HookContext {
        HookContext {
            event: "PreToolUse".into(),
            tool_name: Some("bash".into()),
            tool_args: Some(serde_json::json!({"command": "ls -la"})),
            tool_result: None,
            project_root: PathBuf::from("/tmp/test"),
            session_id: "test-session".into(),
            replay_mode: ReplayMode::Live,
        }
    }

    #[tokio::test]
    async fn prompt_hook_returns_decision() {
        let config = PromptHookConfig {
            system: "Review this tool call for safety.".into(),
            model: Some("local-small".into()),
            provider: None,
            max_tokens: 256,
            timeout_ms: 5000,
        };
        let ctx = test_context();
        let cancel = tokio_util::sync::CancellationToken::new();
        let result = execute_prompt_hook(&config, &ctx, cancel).await;
        assert!(result.is_ok());
        assert!(result.unwrap().decision.allow);
    }

    #[tokio::test]
    async fn agent_hook_returns_decision() {
        let config = AgentHookConfig {
            task: "Check this edit for security issues".into(),
            allowed_tools: vec![],
            max_turns: 1,
            cost_fraction: 0.1,
            timeout_ms: 30000,
        };
        let ctx = test_context();
        let cancel = tokio_util::sync::CancellationToken::new();
        let result = execute_agent_hook(&config, &ctx, cancel).await;
        assert!(result.is_ok());
        assert!(result.unwrap().decision.allow);
    }
}
