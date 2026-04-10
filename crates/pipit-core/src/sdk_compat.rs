//! SDK Compatibility Layer with Protocol Versioning
//!
//! Maps internal `EngineEvent` to version-specific wire formats.
//! Provides a 2-version backward compatibility window. New fields
//! (denial tracking, budget state, replay) are available to v3+
//! consumers while v2 consumers continue to work unchanged.
//!
//! Cost: O(1) per event — one match on `SdkVersion` with conditional
//! field inclusion. ContentDelta events short-circuit with zero-copy.

use crate::sdk::{BudgetSummary, EngineEvent, EngineOutcome};
use serde_json::Value;

/// Supported SDK protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SdkVersion {
    /// Original protocol — basic events.
    V2,
    /// Extended with replay, denial tracking, budget state.
    V3,
}

impl SdkVersion {
    /// Parse from a version number.
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            2 => Some(Self::V2),
            3 => Some(Self::V3),
            _ => None,
        }
    }

    pub fn as_u32(&self) -> u32 {
        match self {
            Self::V2 => 2,
            Self::V3 => 3,
        }
    }
}

/// Range of supported protocol versions.
pub const SUPPORTED_VERSIONS: std::ops::RangeInclusive<u32> = 2..=3;

/// Current (latest) protocol version.
pub const CURRENT_VERSION: SdkVersion = SdkVersion::V3;

/// Check if a client's requested version is supported.
pub fn is_version_supported(version: u32) -> bool {
    SUPPORTED_VERSIONS.contains(&version)
}

/// Map an `EngineEvent` to a version-appropriate JSON Value.
///
/// v2 consumers don't receive v3-only fields. This function is the single
/// serialization gateway — no direct `serde::Serialize` to the wire.
pub fn map_event(event: &EngineEvent, version: SdkVersion) -> Option<Value> {
    match event {
        // Short-circuit for high-frequency events (version-invariant)
        EngineEvent::ContentDelta { text } => Some(serde_json::json!({
            "type": "content_delta",
            "text": text,
        })),

        EngineEvent::ThinkingDelta { text } => Some(serde_json::json!({
            "type": "thinking_delta",
            "text": text,
        })),

        // v3-only events: filtered out for v2 consumers
        EngineEvent::Replay {
            message,
            seq,
            is_last,
        } => {
            if version < SdkVersion::V3 {
                return None; // v2 doesn't know about replay
            }
            Some(serde_json::json!({
                "type": "replay",
                "seq": seq,
                "is_last": is_last,
                "role": format!("{:?}", message.role),
            }))
        }

        EngineEvent::CompactBoundary {
            preserved_count,
            freed_tokens,
        } => {
            if version < SdkVersion::V3 {
                return None;
            }
            Some(serde_json::json!({
                "type": "compact_boundary",
                "preserved_count": preserved_count,
                "freed_tokens": freed_tokens,
            }))
        }

        // Done — conditionally include v3 fields
        EngineEvent::Done { outcome } => {
            let mut obj = serde_json::json!({ "type": "done" });
            match outcome {
                EngineOutcome::Completed {
                    turns,
                    total_tokens,
                    cost,
                    permission_denials,
                    budget_summary,
                } => {
                    obj["outcome"] = serde_json::json!({
                        "status": "completed",
                        "turns": turns,
                        "total_tokens": total_tokens,
                        "cost": cost,
                    });
                    if version >= SdkVersion::V3 {
                        if !permission_denials.is_empty() {
                            obj["outcome"]["permission_denials"] =
                                serde_json::to_value(permission_denials).unwrap_or_default();
                        }
                        if let Some(budget) = budget_summary {
                            obj["outcome"]["budget_summary"] =
                                serde_json::to_value(budget).unwrap_or_default();
                        }
                    }
                }
                EngineOutcome::MaxTurnsReached(turns) => {
                    obj["outcome"] = serde_json::json!({
                        "status": "max_turns_reached",
                        "turns": turns,
                    });
                }
                EngineOutcome::Error(msg) => {
                    obj["outcome"] = serde_json::json!({
                        "status": "error",
                        "message": msg,
                    });
                }
            }
            Some(obj)
        }

        // All other events: standard serialization (version-invariant)
        EngineEvent::TurnStart { turn_number } => Some(serde_json::json!({
            "type": "turn_start",
            "turn_number": turn_number,
        })),

        EngineEvent::TurnEnd {
            turn_number,
            reason,
        } => Some(serde_json::json!({
            "type": "turn_end",
            "turn_number": turn_number,
            "reason": reason,
        })),

        EngineEvent::ContentComplete { full_text } => Some(serde_json::json!({
            "type": "content_complete",
            "full_text": full_text,
        })),

        EngineEvent::ToolCallStart {
            call_id,
            name,
            args,
        } => Some(serde_json::json!({
            "type": "tool_call_start",
            "call_id": call_id,
            "name": name,
            "args": args,
        })),

        EngineEvent::ToolCallEnd {
            call_id,
            name,
            result,
            success,
        } => Some(serde_json::json!({
            "type": "tool_call_end",
            "call_id": call_id,
            "name": name,
            "result": result,
            "success": success,
        })),

        EngineEvent::ApprovalNeeded {
            call_id,
            name,
            args,
        } => Some(serde_json::json!({
            "type": "approval_needed",
            "call_id": call_id,
            "name": name,
            "args": args,
        })),

        EngineEvent::PlanSelected {
            strategy,
            rationale,
            pivoted,
        } => Some(serde_json::json!({
            "type": "plan_selected",
            "strategy": strategy,
            "rationale": rationale,
            "pivoted": pivoted,
        })),

        EngineEvent::VerifierVerdict {
            verdict,
            confidence,
        } => Some(serde_json::json!({
            "type": "verifier_verdict",
            "verdict": verdict,
            "confidence": confidence,
        })),

        EngineEvent::RepairStarted { attempt, reason } => Some(serde_json::json!({
            "type": "repair_started",
            "attempt": attempt,
            "reason": reason,
        })),

        EngineEvent::PhaseTransition { from, to } => Some(serde_json::json!({
            "type": "phase_transition",
            "from": from,
            "to": to,
        })),

        EngineEvent::Compression {
            messages_removed,
            tokens_freed,
        } => Some(serde_json::json!({
            "type": "compression",
            "messages_removed": messages_removed,
            "tokens_freed": tokens_freed,
        })),

        EngineEvent::Usage { used, limit, cost } => Some(serde_json::json!({
            "type": "usage",
            "used": used,
            "limit": limit,
            "cost": cost,
        })),

        EngineEvent::Waiting { label } => Some(serde_json::json!({
            "type": "waiting",
            "label": label,
        })),

        EngineEvent::SteeringInjected { text } => Some(serde_json::json!({
            "type": "steering_injected",
            "text": text,
        })),

        EngineEvent::LoopDetected { tool_name, count } => Some(serde_json::json!({
            "type": "loop_detected",
            "tool_name": tool_name,
            "count": count,
        })),

        EngineEvent::Error { message, retriable } => Some(serde_json::json!({
            "type": "error",
            "message": message,
            "retriable": retriable,
        })),

        // v3+ events — pass through as typed JSON for v3 consumers, skip for v2
        EngineEvent::Init { protocol_version, session_id, cwd, model, provider,
            permission_mode, tools, slash_commands, skills, plugins, agents,
            mcp_servers, agent_mode, capabilities } => {
            if version >= SdkVersion::V3 {
                Some(serde_json::json!({
                    "type": "init",
                    "protocol_version": protocol_version,
                    "session_id": session_id,
                    "cwd": cwd,
                    "model": model,
                    "provider": provider,
                    "permission_mode": permission_mode,
                    "tools": tools,
                    "slash_commands": slash_commands,
                    "skills": skills,
                    "plugins": plugins,
                    "agents": agents,
                    "mcp_servers": mcp_servers,
                    "agent_mode": agent_mode,
                    "capabilities": capabilities,
                }))
            } else {
                None
            }
        }

        EngineEvent::ProfileCheckpoint { turn_number, checkpoint, elapsed_ms } => {
            if version >= SdkVersion::V3 {
                Some(serde_json::json!({
                    "type": "profile_checkpoint",
                    "turn_number": turn_number,
                    "checkpoint": checkpoint,
                    "elapsed_ms": elapsed_ms,
                }))
            } else {
                None
            }
        }

        EngineEvent::ProfileTurnSummary { turn_number, total_ms, phases } => {
            if version >= SdkVersion::V3 {
                Some(serde_json::json!({
                    "type": "profile_turn_summary",
                    "turn_number": turn_number,
                    "total_ms": total_ms,
                    "phases": phases.iter().map(|(name, ms)| serde_json::json!({"name": name, "ms": ms})).collect::<Vec<_>>(),
                }))
            } else {
                None
            }
        }

        EngineEvent::FileTouched { path, action, tool_name, turn_number } => {
            if version >= SdkVersion::V3 {
                Some(serde_json::json!({
                    "type": "file_touched",
                    "path": path,
                    "action": action,
                    "tool_name": tool_name,
                    "turn_number": turn_number,
                }))
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v2_filters_replay() {
        let event = EngineEvent::Replay {
            message: pipit_provider::Message::user("test"),
            seq: 1,
            is_last: true,
        };
        assert!(map_event(&event, SdkVersion::V2).is_none());
        assert!(map_event(&event, SdkVersion::V3).is_some());
    }

    #[test]
    fn test_v2_filters_compact_boundary() {
        let event = EngineEvent::CompactBoundary {
            preserved_count: 5,
            freed_tokens: 1000,
        };
        assert!(map_event(&event, SdkVersion::V2).is_none());
        assert!(map_event(&event, SdkVersion::V3).is_some());
    }

    #[test]
    fn test_content_delta_passes_all_versions() {
        let event = EngineEvent::ContentDelta {
            text: "hello".into(),
        };
        assert!(map_event(&event, SdkVersion::V2).is_some());
        assert!(map_event(&event, SdkVersion::V3).is_some());
    }

    #[test]
    fn test_version_supported() {
        assert!(!is_version_supported(1));
        assert!(is_version_supported(2));
        assert!(is_version_supported(3));
        assert!(!is_version_supported(4));
    }

    #[test]
    fn test_done_v2_omits_denials() {
        let event = EngineEvent::Done {
            outcome: EngineOutcome::Completed {
                turns: 5,
                total_tokens: 10000,
                cost: 0.05,
                permission_denials: vec![],
                budget_summary: Some(BudgetSummary {
                    total_input_tokens: 5000,
                    total_output_tokens: 5000,
                    total_cost_usd: 0.05,
                    budget_fraction_used: 0.5,
                    continuation_count: 0,
                }),
            },
        };

        let v2_json = map_event(&event, SdkVersion::V2).unwrap();
        assert!(v2_json["outcome"].get("permission_denials").is_none());
        assert!(v2_json["outcome"].get("budget_summary").is_none());

        let v3_json = map_event(&event, SdkVersion::V3).unwrap();
        // budget_summary should be present in v3 (even if denials are empty)
        assert!(v3_json["outcome"].get("budget_summary").is_some());
    }
}
