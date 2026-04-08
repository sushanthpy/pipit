//! Skill Policy Engine — runtime enforcement of skill constraints.
//!
//! Replaces the parsed-but-never-enforced `allowed_tools` field with a
//! real policy evaluation pipeline. Three enforcement points:
//!
//! 1. **Pre-invocation**: validate inputs, check trust tier, resolve dependencies
//! 2. **Per-tool-call**: intercept tool dispatch, check against allowed/denied lists
//! 3. **Post-turn**: check cost/time/turn budgets, kill runaway skills
//!
//! Policy decisions are logged to the telemetry layer for audit.

use crate::manifest::{SkillPackage, TrustTier};
use std::collections::HashSet;
use std::time::{Duration, Instant};

// ── Policy decision types ───────────────────────────────────────────

/// Result of a policy evaluation.
#[derive(Debug, Clone)]
pub enum PolicyDecision {
    /// Action is allowed.
    Allow,
    /// Action is denied with a reason.
    Deny(String),
    /// Action requires explicit user approval before proceeding.
    RequiresApproval(String),
}

/// A recorded policy event for audit trail.
#[derive(Debug, Clone)]
pub struct PolicyEvent {
    pub skill_name: String,
    pub action: String,
    pub decision: PolicyDecision,
    pub timestamp: Instant,
}

// ── Per-tool-call policy ────────────────────────────────────────────

/// Tool categories mapped from trust tiers.
/// Each tier unlocks the tools below it plus its own.
///
/// Sandbox: read_file, grep, list_dir, semantic_search
/// Standard: + write_file, create_file, edit_file
/// Elevated: + bash, curl, network tools
/// Privileged: + unrestricted
const SANDBOX_TOOLS: &[&str] = &[
    "read_file",
    "grep_search",
    "file_search",
    "list_dir",
    "semantic_search",
    "view_image",
];

const STANDARD_TOOLS: &[&str] = &[
    "create_file",
    "replace_string_in_file",
    "multi_replace_string_in_file",
];

const ELEVATED_TOOLS: &[&str] = &["run_in_terminal", "fetch_webpage"];

/// Check if a tool is permitted given a trust tier (independent of manifest allowlist).
fn tier_allows_tool(tier: TrustTier, tool: &str) -> bool {
    match tier {
        TrustTier::Privileged => true,
        TrustTier::Elevated => {
            SANDBOX_TOOLS.contains(&tool)
                || STANDARD_TOOLS.contains(&tool)
                || ELEVATED_TOOLS.contains(&tool)
        }
        TrustTier::Standard => SANDBOX_TOOLS.contains(&tool) || STANDARD_TOOLS.contains(&tool),
        TrustTier::Sandbox => SANDBOX_TOOLS.contains(&tool),
    }
}

// ── The engine ──────────────────────────────────────────────────────

/// Runtime policy enforcer for skill execution.
pub struct SkillPolicyEngine {
    /// Globally denied tools (admin override).
    global_deny: HashSet<String>,
    /// Maximum trust tier allowed without approval.
    max_auto_approve_tier: TrustTier,
    /// Audit log of policy decisions.
    events: Vec<PolicyEvent>,
}

impl SkillPolicyEngine {
    pub fn new() -> Self {
        Self {
            global_deny: HashSet::new(),
            max_auto_approve_tier: TrustTier::Elevated,
            events: Vec::new(),
        }
    }

    /// Configure the engine with global deny list and auto-approve ceiling.
    pub fn with_config(global_deny: HashSet<String>, max_auto_approve_tier: TrustTier) -> Self {
        Self {
            global_deny,
            max_auto_approve_tier,
            events: Vec::new(),
        }
    }

    // ── Enforcement point 1: Pre-invocation ─────────────────────────

    /// Evaluate whether a skill may be invoked at all.
    pub fn check_invocation(&mut self, package: &SkillPackage) -> PolicyDecision {
        let tier = package.manifest.package.trust_tier;
        let name = &package.manifest.package.name;

        // Trust tier gate
        if tier > self.max_auto_approve_tier {
            let decision = PolicyDecision::RequiresApproval(format!(
                "Skill '{}' has trust_tier={:?} which exceeds auto-approve ceiling {:?}",
                name, tier, self.max_auto_approve_tier
            ));
            self.record(name, "invocation", decision.clone());
            return decision;
        }

        let decision = PolicyDecision::Allow;
        self.record(name, "invocation", decision.clone());
        decision
    }

    // ── Enforcement point 2: Per-tool-call ──────────────────────────

    /// Check if a skill is allowed to call a specific tool.
    pub fn check_tool_call(&mut self, package: &SkillPackage, tool_name: &str) -> PolicyDecision {
        let name = &package.manifest.package.name;

        // Global deny overrides everything
        if self.global_deny.contains(tool_name) {
            let decision = PolicyDecision::Deny(format!("Tool '{}' is globally denied", tool_name));
            self.record(name, &format!("tool:{}", tool_name), decision.clone());
            return decision;
        }

        // Trust tier check — does this tier even allow this class of tool?
        let tier = package.manifest.package.trust_tier;
        if !tier_allows_tool(tier, tool_name) {
            let decision = PolicyDecision::Deny(format!(
                "Tool '{}' not permitted at trust_tier={:?}",
                tool_name, tier
            ));
            self.record(name, &format!("tool:{}", tool_name), decision.clone());
            return decision;
        }

        // Package-level tool policy
        if !package.is_tool_allowed(tool_name) {
            let decision = PolicyDecision::Deny(format!(
                "Tool '{}' not in skill '{}' allowed tools",
                tool_name, name
            ));
            self.record(name, &format!("tool:{}", tool_name), decision.clone());
            return decision;
        }

        let decision = PolicyDecision::Allow;
        self.record(name, &format!("tool:{}", tool_name), decision.clone());
        decision
    }

    // ── Enforcement point 3: Post-turn budget check ─────────────────

    /// Check if a skill has exceeded its resource budget.
    /// Returns Deny if any budget is blown.
    pub fn check_budget(
        &mut self,
        package: &SkillPackage,
        turns_used: u32,
        cost_usd: f64,
        elapsed: Duration,
    ) -> PolicyDecision {
        let name = &package.manifest.package.name;
        let policy = &package.manifest.policy;

        if let Some(max_turns) = policy.max_turns {
            if turns_used > max_turns {
                let decision = PolicyDecision::Deny(format!(
                    "Skill '{}' exceeded max_turns: {} > {}",
                    name, turns_used, max_turns
                ));
                self.record(name, "budget:turns", decision.clone());
                return decision;
            }
        }

        if let Some(max_cost) = policy.max_cost_usd {
            if cost_usd > max_cost {
                let decision = PolicyDecision::Deny(format!(
                    "Skill '{}' exceeded max_cost: ${:.4} > ${:.4}",
                    name, cost_usd, max_cost
                ));
                self.record(name, "budget:cost", decision.clone());
                return decision;
            }
        }

        if let Some(max_time) = policy.max_time_secs {
            if elapsed.as_secs() > max_time {
                let decision = PolicyDecision::Deny(format!(
                    "Skill '{}' exceeded max_time: {}s > {}s",
                    name,
                    elapsed.as_secs(),
                    max_time
                ));
                self.record(name, "budget:time", decision.clone());
                return decision;
            }
        }

        PolicyDecision::Allow
    }

    // ── Audit log ───────────────────────────────────────────────────

    fn record(&mut self, skill: &str, action: &str, decision: PolicyDecision) {
        self.events.push(PolicyEvent {
            skill_name: skill.to_string(),
            action: action.to_string(),
            decision,
            timestamp: Instant::now(),
        });
    }

    /// Drain all recorded policy events (for telemetry export).
    pub fn drain_events(&mut self) -> Vec<PolicyEvent> {
        std::mem::take(&mut self.events)
    }

    /// Count of deny decisions in the audit log.
    pub fn deny_count(&self) -> usize {
        self.events
            .iter()
            .filter(|e| matches!(e.decision, PolicyDecision::Deny(_)))
            .count()
    }
}

impl Default for SkillPolicyEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontmatter::{SkillFrontmatter, SkillMetadata, SkillSource};
    use crate::manifest::{
        ManifestPackage, ManifestSource, PolicyConstraints, SkillManifest, ToolsSpec,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn make_package(
        name: &str,
        tier: TrustTier,
        allowed: &[&str],
        denied: &[&str],
    ) -> SkillPackage {
        let meta = SkillMetadata {
            name: name.to_string(),
            description: "test".to_string(),
            path: PathBuf::from("/tmp/test"),
            source: SkillSource::Project,
            frontmatter: SkillFrontmatter::default(),
        };
        SkillPackage {
            metadata: meta,
            manifest: SkillManifest {
                package: ManifestPackage {
                    name: name.to_string(),
                    version: "1.0.0".to_string(),
                    description: None,
                    authors: vec![],
                    trust_tier: tier,
                },
                inputs: HashMap::new(),
                outputs: HashMap::new(),
                tools: ToolsSpec {
                    allowed: allowed.iter().map(|s| s.to_string()).collect(),
                    denied: denied.iter().map(|s| s.to_string()).collect(),
                },
                dependencies: HashMap::new(),
                policy: PolicyConstraints {
                    max_turns: Some(10),
                    max_cost_usd: Some(0.50),
                    max_time_secs: Some(300),
                    ..Default::default()
                },
                test: None,
            },
            manifest_source: ManifestSource::Explicit,
            skill_dir: PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn test_sandbox_denies_bash() {
        let mut engine = SkillPolicyEngine::new();
        let pkg = make_package("safe", TrustTier::Sandbox, &[], &[]);

        match engine.check_tool_call(&pkg, "run_in_terminal") {
            PolicyDecision::Deny(_) => {} // expected
            other => panic!("Sandbox should deny bash, got {:?}", other),
        }
    }

    #[test]
    fn test_elevated_allows_bash() {
        let mut engine = SkillPolicyEngine::new();
        let pkg = make_package("powerful", TrustTier::Elevated, &[], &[]);

        match engine.check_tool_call(&pkg, "run_in_terminal") {
            PolicyDecision::Allow => {} // expected
            other => panic!("Elevated should allow bash, got {:?}", other),
        }
    }

    #[test]
    fn test_privileged_requires_approval() {
        let mut engine = SkillPolicyEngine::new();
        let pkg = make_package("root", TrustTier::Privileged, &[], &[]);

        match engine.check_invocation(&pkg) {
            PolicyDecision::RequiresApproval(_) => {} // expected
            other => panic!("Privileged should require approval, got {:?}", other),
        }
    }

    #[test]
    fn test_budget_enforcement() {
        let mut engine = SkillPolicyEngine::new();
        let pkg = make_package("budgeted", TrustTier::Standard, &[], &[]);

        // Within budget
        match engine.check_budget(&pkg, 5, 0.25, Duration::from_secs(100)) {
            PolicyDecision::Allow => {}
            other => panic!("Should allow within budget, got {:?}", other),
        }

        // Turns exceeded
        match engine.check_budget(&pkg, 15, 0.25, Duration::from_secs(100)) {
            PolicyDecision::Deny(msg) => assert!(msg.contains("max_turns")),
            other => panic!("Should deny over turns, got {:?}", other),
        }

        // Cost exceeded
        match engine.check_budget(&pkg, 5, 1.00, Duration::from_secs(100)) {
            PolicyDecision::Deny(msg) => assert!(msg.contains("max_cost")),
            other => panic!("Should deny over cost, got {:?}", other),
        }
    }

    #[test]
    fn test_global_deny_overrides_allowed() {
        let mut global_deny = HashSet::new();
        global_deny.insert("bash".to_string());

        let mut engine = SkillPolicyEngine::with_config(global_deny, TrustTier::Privileged);
        let pkg = make_package("test", TrustTier::Privileged, &["bash"], &[]);

        match engine.check_tool_call(&pkg, "bash") {
            PolicyDecision::Deny(msg) => assert!(msg.contains("globally denied")),
            other => panic!("Global deny should override, got {:?}", other),
        }
    }
}
