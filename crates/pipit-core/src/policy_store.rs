//! Durable Policy Events — Persistent Permission State Machine
//!
//! Elevates approval/permission decisions into durable session facts.
//! When a user grants a scoped approval or denies a tool, that decision
//! is written into the session ledger as a first-class event and reapplied
//! on resume or remote reattachment.
//!
//! Each permission request follows a persistent FSM:
//! ```text
//! Requested → Approved | Denied → Applied | Expired
//! ```
//! State transitions are O(1). Scoped grants are constraint predicates
//! over tool name and argument schema; validation is O(m) in constraint count.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// States of a permission request FSM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionState {
    /// Awaiting user decision.
    Requested,
    /// User approved (may be scoped).
    Approved {
        /// If true, this approval applies to future calls of the same tool.
        permanent: bool,
    },
    /// User denied.
    Denied {
        /// Optional user feedback explaining the denial.
        feedback: Option<String>,
    },
    /// The approval/denial has been applied to a tool call.
    Applied,
    /// The approval expired (e.g., session ended, timeout).
    Expired,
}

/// A scoped grant: a standing approval for a tool under certain constraints.
///
/// Example: "Always allow `read_file` for paths under `src/`"
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedGrant {
    /// Tool name this grant applies to.
    pub tool_name: String,
    /// Constraints on arguments (JSON pointer → allowed value/pattern).
    pub constraints: Vec<GrantConstraint>,
    /// When this grant was issued.
    pub granted_at_ms: u64,
    /// Session-scoped or permanent.
    pub scope: GrantScope,
    /// Turn in which the grant was issued.
    pub turn: u32,
}

/// A constraint on tool arguments for a scoped grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantConstraint {
    /// JSON pointer to the argument field (e.g., "/path").
    pub field: String,
    /// Constraint type.
    pub kind: ConstraintKind,
}

/// Types of constraints on scoped grants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConstraintKind {
    /// Exact value match.
    Equals(String),
    /// Prefix match (e.g., path starts with "src/").
    StartsWith(String),
    /// Any value is accepted.
    Any,
}

/// Scope of a grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrantScope {
    /// Valid only for the current session.
    Session,
    /// Survives across sessions (persisted to config).
    Permanent,
}

/// Tracks the lifecycle of a single permission request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// Unique ID for this request.
    pub request_id: String,
    /// Tool call ID this request is for.
    pub call_id: String,
    /// Tool name.
    pub tool_name: String,
    /// Tool arguments.
    pub args: serde_json::Value,
    /// Current FSM state.
    pub state: PermissionState,
    /// When the request was created.
    pub created_at_ms: u64,
    /// When the state last changed.
    pub last_transition_ms: u64,
}

impl PermissionRequest {
    /// Transition to Approved state. O(1).
    pub fn approve(&mut self, permanent: bool) {
        self.state = PermissionState::Approved { permanent };
        self.last_transition_ms = current_millis();
    }

    /// Transition to Denied state. O(1).
    pub fn deny(&mut self, feedback: Option<String>) {
        self.state = PermissionState::Denied { feedback };
        self.last_transition_ms = current_millis();
    }

    /// Transition to Applied state. O(1).
    pub fn applied(&mut self) {
        self.state = PermissionState::Applied;
        self.last_transition_ms = current_millis();
    }

    /// Transition to Expired state. O(1).
    pub fn expire(&mut self) {
        self.state = PermissionState::Expired;
        self.last_transition_ms = current_millis();
    }

    /// Whether this request is still pending.
    pub fn is_pending(&self) -> bool {
        matches!(self.state, PermissionState::Requested)
    }
}

/// Durable policy store — manages permission requests and scoped grants.
///
/// Survives resume by replaying ToolApproved/ToolDenied events from the ledger.
pub struct PolicyStore {
    /// Active permission requests (keyed by call_id).
    requests: HashMap<String, PermissionRequest>,
    /// Standing scoped grants (keyed by tool_name → grants).
    grants: HashMap<String, Vec<ScopedGrant>>,
    /// Counter for generating request IDs.
    next_id: u64,
}

impl PolicyStore {
    pub fn new() -> Self {
        Self {
            requests: HashMap::new(),
            grants: HashMap::new(),
            next_id: 1,
        }
    }

    /// Create a new permission request for a tool call.
    pub fn request_permission(
        &mut self,
        call_id: &str,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> &PermissionRequest {
        let request_id = format!("perm-{}", self.next_id);
        self.next_id += 1;

        let request = PermissionRequest {
            request_id,
            call_id: call_id.to_string(),
            tool_name: tool_name.to_string(),
            args: args.clone(),
            state: PermissionState::Requested,
            created_at_ms: current_millis(),
            last_transition_ms: current_millis(),
        };

        self.requests.insert(call_id.to_string(), request);
        &self.requests[call_id]
    }

    /// Record an approval decision.
    pub fn record_approval(&mut self, call_id: &str, permanent: bool) {
        let tool_name = self.requests.get(call_id).map(|r| r.tool_name.clone());

        if let Some(req) = self.requests.get_mut(call_id) {
            req.approve(permanent);
        }

        // If permanent, create a scoped grant
        if permanent {
            if let Some(tool_name) = tool_name {
                self.add_grant(ScopedGrant {
                    tool_name,
                    constraints: vec![GrantConstraint {
                        field: "*".to_string(),
                        kind: ConstraintKind::Any,
                    }],
                    granted_at_ms: current_millis(),
                    scope: GrantScope::Session,
                    turn: 0,
                });
            }
        }
    }

    /// Record a denial decision.
    pub fn record_denial(&mut self, call_id: &str, feedback: Option<String>) {
        if let Some(req) = self.requests.get_mut(call_id) {
            req.deny(feedback);
        }
    }

    /// Add a scoped grant.
    pub fn add_grant(&mut self, grant: ScopedGrant) {
        self.grants
            .entry(grant.tool_name.clone())
            .or_default()
            .push(grant);
    }

    /// Check if a tool call is covered by a standing scoped grant.
    /// Returns true if any grant matches. O(m) in number of grants for this tool.
    pub fn has_standing_grant(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> bool {
        let Some(grants) = self.grants.get(tool_name) else {
            return false;
        };

        grants.iter().any(|grant| {
            grant.constraints.iter().all(|c| match_constraint(c, args))
        })
    }

    /// Get pending permission requests.
    pub fn pending_requests(&self) -> Vec<&PermissionRequest> {
        self.requests.values().filter(|r| r.is_pending()).collect()
    }

    /// Get all scoped grants for a tool.
    pub fn grants_for_tool(&self, tool_name: &str) -> &[ScopedGrant] {
        self.grants.get(tool_name).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Expire all pending requests (e.g., on session end).
    pub fn expire_all_pending(&mut self) {
        for req in self.requests.values_mut() {
            if req.is_pending() {
                req.expire();
            }
        }
    }

    /// Replay a ledger event to rebuild policy state.
    pub fn replay_event(&mut self, event: &crate::ledger::SessionEvent) {
        match event {
            crate::ledger::SessionEvent::ToolCallProposed {
                call_id, tool_name, args,
            } => {
                self.request_permission(call_id, tool_name, args);
            }
            crate::ledger::SessionEvent::ToolApproved { call_id } => {
                self.record_approval(call_id, false);
            }
            crate::ledger::SessionEvent::ToolDenied { call_id, .. } => {
                self.record_denial(call_id, None);
            }
            _ => {}
        }
    }
}

impl Default for PolicyStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a constraint matches the given arguments.
fn match_constraint(constraint: &GrantConstraint, args: &serde_json::Value) -> bool {
    match &constraint.kind {
        ConstraintKind::Any => true,
        ConstraintKind::Equals(expected) => {
            if constraint.field == "*" {
                return true;
            }
            args.pointer(&constraint.field)
                .and_then(|v| v.as_str())
                .map(|v| v == expected)
                .unwrap_or(false)
        }
        ConstraintKind::StartsWith(prefix) => {
            args.pointer(&constraint.field)
                .and_then(|v| v.as_str())
                .map(|v| v.starts_with(prefix))
                .unwrap_or(false)
        }
    }
}

fn current_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_permission_lifecycle() {
        let mut store = PolicyStore::new();

        let args = serde_json::json!({"command": "rm -rf /tmp/test"});
        store.request_permission("call-1", "bash", &args);
        assert_eq!(store.pending_requests().len(), 1);

        store.record_approval("call-1", false);
        assert_eq!(store.pending_requests().len(), 0);

        let req = &store.requests["call-1"];
        assert!(matches!(req.state, PermissionState::Approved { permanent: false }));
    }

    #[test]
    fn test_scoped_grant_matching() {
        let mut store = PolicyStore::new();

        store.add_grant(ScopedGrant {
            tool_name: "read_file".to_string(),
            constraints: vec![GrantConstraint {
                field: "/path".to_string(),
                kind: ConstraintKind::StartsWith("src/".to_string()),
            }],
            granted_at_ms: 0,
            scope: GrantScope::Session,
            turn: 1,
        });

        // Under src/ — should match
        let args_ok = serde_json::json!({"path": "src/main.rs"});
        assert!(store.has_standing_grant("read_file", &args_ok));

        // Outside src/ — should NOT match
        let args_no = serde_json::json!({"path": "/etc/passwd"});
        assert!(!store.has_standing_grant("read_file", &args_no));

        // Wrong tool — should NOT match
        assert!(!store.has_standing_grant("bash", &args_ok));
    }

    #[test]
    fn test_permanent_approval_creates_grant() {
        let mut store = PolicyStore::new();

        let args = serde_json::json!({"path": "src/lib.rs"});
        store.request_permission("call-1", "read_file", &args);
        store.record_approval("call-1", true); // permanent

        // Should have a standing grant now
        assert!(store.has_standing_grant("read_file", &serde_json::json!({"any": "thing"})));
    }

    #[test]
    fn test_ledger_replay() {
        let mut store = PolicyStore::new();

        store.replay_event(&crate::ledger::SessionEvent::ToolCallProposed {
            call_id: "c1".into(),
            tool_name: "bash".into(),
            args: serde_json::json!({"command": "ls"}),
        });
        assert_eq!(store.pending_requests().len(), 1);

        store.replay_event(&crate::ledger::SessionEvent::ToolApproved {
            call_id: "c1".into(),
        });
        assert_eq!(store.pending_requests().len(), 0);
    }
}
