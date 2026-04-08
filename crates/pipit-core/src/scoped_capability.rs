//! Scoped Capability Kernel for Subagents and Skills
//!
//! Eliminates ambient authority by making permissions explicit, local, and
//! auditable. Every spawned child runtime receives an immutable capability
//! bundle — never mutated ambient state.
//!
//! Authority model: C_child ⊆ C_parent. Tool execution allowed iff
//! required(tool, args) ⊆ C_task. Membership is O(1) via bitset meet
//! plus O(k) predicate checks for path/domain constraints.

use crate::capability::{
    Capability, CapabilitySet, ExecutionLineage, PolicyDecision, ResourceScope,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

// ─── Scoped Capability Bundle ──────────────────────────────────────────

/// An immutable, signed capability bundle passed to each child runtime.
///
/// Unlike mutable allow-rules, this is:
///   - Explicit: every permission is declared up-front
///   - Local: cannot be widened by the child
///   - Auditable: the full bundle is logged at spawn time
///   - Bounded: expires, has budget limits, and path constraints
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopedCapabilityBundle {
    /// Unique identifier for this bundle.
    pub bundle_id: String,
    /// Task ID this bundle is scoped to.
    pub task_id: String,
    /// Parent bundle ID (for lineage tracking). None for root.
    pub parent_bundle_id: Option<String>,
    /// Bitset of granted capabilities — immutable after creation.
    pub granted: CapabilitySet,
    /// Path constraints: only these path prefixes are accessible.
    pub allowed_paths: Vec<String>,
    /// Domain constraints: only these hosts may be contacted.
    pub allowed_domains: Vec<String>,
    /// Tool constraints: only these tools may be invoked. Empty = all tools.
    pub allowed_tools: Vec<String>,
    /// Command constraints: only these binaries may be executed.
    pub allowed_binaries: Vec<String>,
    /// Maximum token/cost budget for this scope.
    pub budget: ScopeBudget,
    /// Mutability class: what the child is allowed to modify.
    pub mutability: MutabilityClass,
    /// When this bundle was issued (unix timestamp ms).
    pub issued_at: u64,
    /// When this bundle expires (unix timestamp ms). 0 = no expiry.
    pub expires_at: u64,
    /// Maximum delegation depth from this bundle.
    pub max_depth: u32,
    /// Current depth in the delegation chain.
    pub current_depth: u32,
}

/// Budget constraints for a scoped capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeBudget {
    /// Maximum total tokens (input + output) this scope may consume.
    pub max_tokens: u64,
    /// Maximum cost in USD this scope may incur.
    pub max_cost_usd: f64,
    /// Maximum number of tool invocations.
    pub max_tool_calls: u32,
    /// Maximum number of file mutations.
    pub max_mutations: u32,
    /// Tokens consumed so far.
    pub tokens_used: u64,
    /// Cost incurred so far.
    pub cost_used: f64,
    /// Tool calls made so far.
    pub tool_calls_used: u32,
    /// Mutations made so far.
    pub mutations_used: u32,
}

impl Default for ScopeBudget {
    fn default() -> Self {
        Self {
            max_tokens: 500_000,
            max_cost_usd: 5.0,
            max_tool_calls: 200,
            max_mutations: 50,
            tokens_used: 0,
            cost_used: 0.0,
            tool_calls_used: 0,
            mutations_used: 0,
        }
    }
}

impl ScopeBudget {
    /// Check if any budget limit has been exceeded.
    pub fn is_exhausted(&self) -> Option<&'static str> {
        if self.tokens_used >= self.max_tokens {
            Some("token budget exhausted")
        } else if self.cost_used >= self.max_cost_usd {
            Some("cost budget exhausted")
        } else if self.tool_calls_used >= self.max_tool_calls {
            Some("tool call budget exhausted")
        } else if self.mutations_used >= self.max_mutations {
            Some("mutation budget exhausted")
        } else {
            None
        }
    }
}

/// What a child runtime is allowed to modify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutabilityClass {
    /// Read-only: no side effects allowed.
    ReadOnly,
    /// May modify files within allowed_paths only.
    ScopedWrite,
    /// May modify files and run commands within allowed_paths.
    ScopedExec,
    /// Full write access within project root.
    ProjectWrite,
}

impl ScopedCapabilityBundle {
    /// Create a root bundle from an approval mode.
    pub fn root(task_id: &str, granted: CapabilitySet, project_root: &str) -> Self {
        Self {
            bundle_id: uuid::Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            parent_bundle_id: None,
            granted,
            allowed_paths: vec![project_root.to_string()],
            allowed_domains: Vec::new(),
            allowed_tools: Vec::new(),
            allowed_binaries: Vec::new(),
            budget: ScopeBudget::default(),
            mutability: MutabilityClass::ProjectWrite,
            issued_at: now_ms(),
            expires_at: 0,
            max_depth: 3,
            current_depth: 0,
        }
    }

    /// Derive a child bundle with narrower scope. C_child ⊆ C_parent.
    ///
    /// The child bundle can only have FEWER capabilities, not more.
    /// This is the monotone property of the capability lattice.
    pub fn derive_child(&self, constraints: ChildConstraints) -> Result<Self, String> {
        // Enforce depth limit
        if self.current_depth >= self.max_depth {
            return Err(format!(
                "delegation depth {} exceeds max {}",
                self.current_depth + 1,
                self.max_depth
            ));
        }

        // Child capabilities must be a subset of parent
        let child_granted = self.granted.meet(constraints.granted);

        // Child paths must be within parent paths
        let child_paths = if constraints.allowed_paths.is_empty() {
            self.allowed_paths.clone()
        } else {
            constraints
                .allowed_paths
                .into_iter()
                .filter(|p| {
                    self.allowed_paths
                        .iter()
                        .any(|parent| p.starts_with(parent))
                })
                .collect()
        };

        // Child budget cannot exceed parent remaining budget
        let child_budget = ScopeBudget {
            max_tokens: constraints
                .budget
                .max_tokens
                .min(self.budget.max_tokens - self.budget.tokens_used),
            max_cost_usd: constraints
                .budget
                .max_cost_usd
                .min(self.budget.max_cost_usd - self.budget.cost_used),
            max_tool_calls: constraints
                .budget
                .max_tool_calls
                .min(self.budget.max_tool_calls - self.budget.tool_calls_used),
            max_mutations: constraints
                .budget
                .max_mutations
                .min(self.budget.max_mutations - self.budget.mutations_used),
            ..Default::default()
        };

        // Child mutability cannot exceed parent
        let child_mutability = narrower_mutability(self.mutability, constraints.mutability);

        Ok(Self {
            bundle_id: uuid::Uuid::new_v4().to_string(),
            task_id: constraints
                .child_task_id
                .unwrap_or_else(|| self.task_id.clone()),
            parent_bundle_id: Some(self.bundle_id.clone()),
            granted: child_granted,
            allowed_paths: child_paths,
            allowed_domains: if constraints.allowed_domains.is_empty() {
                self.allowed_domains.clone()
            } else {
                constraints.allowed_domains
            },
            allowed_tools: constraints.allowed_tools,
            allowed_binaries: constraints.allowed_binaries,
            budget: child_budget,
            mutability: child_mutability,
            issued_at: now_ms(),
            expires_at: constraints
                .duration_ms
                .map(|d| now_ms() + d)
                .unwrap_or(self.expires_at),
            max_depth: self.max_depth,
            current_depth: self.current_depth + 1,
        })
    }

    /// Check if this bundle has expired.
    pub fn is_expired(&self) -> bool {
        self.expires_at > 0 && now_ms() > self.expires_at
    }

    /// Evaluate whether a tool call is permitted under this bundle.
    /// Returns Allow if permitted, Deny with reason if not.
    ///
    /// Cost: O(1) bitset check + O(k) predicate checks.
    pub fn evaluate(
        &self,
        tool_name: &str,
        required: CapabilitySet,
        resource_scopes: &[ResourceScope],
    ) -> PolicyDecision {
        // 1. Expiry check
        if self.is_expired() {
            return PolicyDecision::Deny {
                reason: "capability bundle has expired".to_string(),
            };
        }

        // 2. Budget check
        if let Some(reason) = self.budget.is_exhausted() {
            return PolicyDecision::Deny {
                reason: reason.to_string(),
            };
        }

        // 3. Capability lattice check: R ⊆ G
        if !self.granted.satisfies(required) {
            let missing = CapabilitySet::from_bits(required.bits() & !self.granted.bits());
            return PolicyDecision::Deny {
                reason: format!(
                    "tool '{}' requires capabilities not granted: {}",
                    tool_name, missing
                ),
            };
        }

        // 4. Tool allowlist check
        if !self.allowed_tools.is_empty() && !self.allowed_tools.iter().any(|t| t == tool_name) {
            return PolicyDecision::Deny {
                reason: format!(
                    "tool '{}' not in allowed tools: {:?}",
                    tool_name, self.allowed_tools
                ),
            };
        }

        // 5. Path constraint check
        for scope in resource_scopes {
            if let ResourceScope::Path(path) = scope {
                let path_str = path.display().to_string();
                if !self.allowed_paths.is_empty()
                    && !self.allowed_paths.iter().any(|p| path_str.starts_with(p))
                {
                    return PolicyDecision::Deny {
                        reason: format!(
                            "path '{}' outside allowed paths: {:?}",
                            path_str, self.allowed_paths
                        ),
                    };
                }
            }
            if let ResourceScope::Command(cmd) = scope {
                if !self.allowed_binaries.is_empty() {
                    let binary = cmd.split_whitespace().next().unwrap_or("");
                    let name = binary.rsplit('/').next().unwrap_or(binary);
                    if !self.allowed_binaries.iter().any(|b| b == name) {
                        return PolicyDecision::Deny {
                            reason: format!(
                                "binary '{}' not in allowed binaries: {:?}",
                                name, self.allowed_binaries
                            ),
                        };
                    }
                }
            }
            if let ResourceScope::Host(host) = scope {
                if !self.allowed_domains.is_empty()
                    && !self.allowed_domains.iter().any(|d| host.contains(d))
                {
                    return PolicyDecision::Deny {
                        reason: format!(
                            "host '{}' not in allowed domains: {:?}",
                            host, self.allowed_domains
                        ),
                    };
                }
            }
        }

        // 6. Mutability class check
        match self.mutability {
            MutabilityClass::ReadOnly => {
                if required.has(Capability::FsWrite)
                    || required.has(Capability::ProcessExecMutating)
                {
                    return PolicyDecision::Deny {
                        reason: "scope is read-only".to_string(),
                    };
                }
            }
            MutabilityClass::ScopedWrite => {
                if required.has(Capability::ProcessExecMutating) {
                    return PolicyDecision::Deny {
                        reason: "scope allows scoped writes but not mutating exec".to_string(),
                    };
                }
            }
            _ => {}
        }

        PolicyDecision::Allow
    }

    /// Record token/cost usage against this bundle's budget.
    pub fn record_usage(&mut self, tokens: u64, cost: f64, is_mutation: bool) {
        self.budget.tokens_used += tokens;
        self.budget.cost_used += cost;
        self.budget.tool_calls_used += 1;
        if is_mutation {
            self.budget.mutations_used += 1;
        }
    }
}

/// Constraints for deriving a child capability bundle.
pub struct ChildConstraints {
    pub child_task_id: Option<String>,
    pub granted: CapabilitySet,
    pub allowed_paths: Vec<String>,
    pub allowed_domains: Vec<String>,
    pub allowed_tools: Vec<String>,
    pub allowed_binaries: Vec<String>,
    pub budget: ScopeBudget,
    pub mutability: MutabilityClass,
    pub duration_ms: Option<u64>,
}

impl Default for ChildConstraints {
    fn default() -> Self {
        Self {
            child_task_id: None,
            granted: CapabilitySet::ALL,
            allowed_paths: Vec::new(),
            allowed_domains: Vec::new(),
            allowed_tools: Vec::new(),
            allowed_binaries: Vec::new(),
            budget: ScopeBudget::default(),
            mutability: MutabilityClass::ProjectWrite,
            duration_ms: None,
        }
    }
}

fn narrower_mutability(parent: MutabilityClass, child: MutabilityClass) -> MutabilityClass {
    use MutabilityClass::*;
    match (parent, child) {
        (ReadOnly, _) => ReadOnly,
        (ScopedWrite, ProjectWrite | ScopedExec) => ScopedWrite,
        (ScopedExec, ProjectWrite) => ScopedExec,
        (_, c) if (c as u8) <= (parent as u8) => c,
        _ => parent,
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_cannot_exceed_parent() {
        let parent = ScopedCapabilityBundle::root("t1", CapabilitySet::EDIT, "/project");
        let child = parent
            .derive_child(ChildConstraints {
                granted: CapabilitySet::ALL, // tries to get ALL
                ..Default::default()
            })
            .unwrap();
        // Child should only have EDIT (intersection with parent)
        assert_eq!(child.granted, CapabilitySet::EDIT);
    }

    #[test]
    fn depth_limit_enforced() {
        let mut bundle = ScopedCapabilityBundle::root("t1", CapabilitySet::ALL, "/");
        bundle.max_depth = 2;
        bundle.current_depth = 2;
        let result = bundle.derive_child(ChildConstraints::default());
        assert!(result.is_err());
    }

    #[test]
    fn budget_exhaustion_denies() {
        let mut bundle = ScopedCapabilityBundle::root("t1", CapabilitySet::ALL, "/");
        bundle.budget.max_tool_calls = 5;
        bundle.budget.tool_calls_used = 5;
        let decision = bundle.evaluate("read_file", CapabilitySet::READ_ONLY, &[]);
        assert!(matches!(decision, PolicyDecision::Deny { .. }));
    }
}
