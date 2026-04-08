//! First-Class Skill Execution Kernel (Skill Task 5)
//!
//! Turns skills from marketplace objects into runtime-enforced execution units.
//! Each skill is compiled into a validated execution plan with:
//! - Trigger condition
//! - Prompt template
//! - Allowed capabilities
//! - Execution mode (inline vs forked)
//! - Input/output schema validation

use crate::capability::CapabilitySet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// A compiled, validated skill ready for runtime execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledSkill {
    /// Unique skill identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Source path of the skill definition.
    pub source: PathBuf,
    /// Trigger condition: when should this skill activate?
    pub trigger: SkillTrigger,
    /// Prompt template with parameter placeholders.
    pub template: String,
    /// Input schema (JSON Schema) for parameter validation.
    pub input_schema: Option<serde_json::Value>,
    /// Output schema (JSON Schema) for result validation.
    pub output_schema: Option<serde_json::Value>,
    /// Sandbox contract: what this skill is allowed to do.
    pub sandbox: SkillSandbox,
    /// Execution mode.
    pub execution_mode: ExecutionMode,
    /// Priority for conflict resolution when multiple skills match.
    pub priority: i32,
    /// Trust tier (affects what capabilities are granted).
    pub trust_tier: SkillTrustTier,
}

/// When a skill should activate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SkillTrigger {
    /// Always available (globally loaded into system prompt).
    Always,
    /// Activate when specific file patterns are touched.
    PathPattern { patterns: Vec<String> },
    /// Activate for specific programming languages.
    Language { languages: Vec<String> },
    /// Activate when specific dependencies are present.
    Dependency { packages: Vec<String> },
    /// Activate on explicit user invocation (/skill-name).
    Explicit { command: String },
    /// Composite: all sub-triggers must match.
    All { triggers: Vec<SkillTrigger> },
    /// Composite: any sub-trigger matches.
    Any { triggers: Vec<SkillTrigger> },
}

/// Sandbox contract: what a skill is allowed to do.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillSandbox {
    /// Maximum capability set this skill can request.
    pub max_capabilities: u32,
    /// Explicitly allowed tool names.
    pub allowed_tools: Vec<String>,
    /// Explicitly denied tool names.
    pub denied_tools: Vec<String>,
    /// Allowed MCP servers.
    pub allowed_mcp_servers: Vec<String>,
    /// Token budget.
    pub token_budget: SkillTokenBudget,
    /// Wall-clock timeout.
    pub timeout: Duration,
    /// Whether the skill may fork a subagent.
    pub may_delegate: bool,
}

impl Default for SkillSandbox {
    fn default() -> Self {
        Self {
            max_capabilities: CapabilitySet::READ_ONLY.bits(),
            allowed_tools: vec![],
            denied_tools: vec![],
            allowed_mcp_servers: vec![],
            token_budget: SkillTokenBudget::default(),
            timeout: Duration::from_secs(120),
            may_delegate: false,
        }
    }
}

/// Token budget for a skill execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillTokenBudget {
    /// Maximum input tokens.
    pub max_input: u64,
    /// Maximum output tokens.
    pub max_output: u64,
}

impl Default for SkillTokenBudget {
    fn default() -> Self {
        Self {
            max_input: 50_000,
            max_output: 8_000,
        }
    }
}

/// How a skill executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionMode {
    /// Skill instructions are injected inline into the current agent's context.
    Inline,
    /// Skill runs as a forked subagent with its own context.
    Forked,
    /// Runtime decides based on complexity.
    Auto,
}

/// Trust tier for supply-chain safety.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SkillTrustTier {
    /// Built-in skills shipped with pipit.
    Builtin,
    /// Project-local skills (from .pipit/skills/).
    ProjectLocal,
    /// User-installed skills (from ~/.config/pipit/skills/).
    UserInstalled,
    /// Community marketplace skills (require signature verification).
    Community,
    /// Unknown/unsigned skills.
    Untrusted,
}

/// Result of validating a skill invocation.
#[derive(Debug, Clone)]
pub enum SkillValidation {
    /// Valid: all parameters check out, sandbox is compatible.
    Valid,
    /// Invalid input: parameters don't match schema.
    InvalidInput { errors: Vec<String> },
    /// Capability violation: skill requests more than sandbox allows.
    CapabilityViolation {
        requested: CapabilitySet,
        allowed: CapabilitySet,
    },
    /// Budget violation: not enough tokens remaining.
    BudgetExceeded { requested: u64, remaining: u64 },
    /// Trust violation: skill trust tier insufficient.
    TrustViolation {
        skill_tier: SkillTrustTier,
        minimum_required: SkillTrustTier,
    },
}

/// The skill execution kernel — validates and dispatches skill invocations.
pub struct SkillKernel {
    /// All compiled skills, indexed by ID.
    skills: HashMap<String, CompiledSkill>,
    /// Minimum trust tier for execution.
    minimum_trust: SkillTrustTier,
}

impl SkillKernel {
    pub fn new(minimum_trust: SkillTrustTier) -> Self {
        Self {
            skills: HashMap::new(),
            minimum_trust,
        }
    }

    /// Register a compiled skill.
    pub fn register(&mut self, skill: CompiledSkill) {
        self.skills.insert(skill.id.clone(), skill);
    }

    /// Validate a skill invocation before execution.
    pub fn validate(
        &self,
        skill_id: &str,
        params: &serde_json::Value,
        available_budget: u64,
    ) -> SkillValidation {
        let skill = match self.skills.get(skill_id) {
            Some(s) => s,
            None => {
                return SkillValidation::InvalidInput {
                    errors: vec![format!("Skill '{}' not found", skill_id)],
                };
            }
        };

        // Trust check
        if skill.trust_tier > self.minimum_trust {
            return SkillValidation::TrustViolation {
                skill_tier: skill.trust_tier,
                minimum_required: self.minimum_trust,
            };
        }

        // Budget check
        if skill.sandbox.token_budget.max_input > available_budget {
            return SkillValidation::BudgetExceeded {
                requested: skill.sandbox.token_budget.max_input,
                remaining: available_budget,
            };
        }

        // Input schema validation (basic)
        if let Some(ref schema) = skill.input_schema {
            if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
                for field in required {
                    if let Some(name) = field.as_str() {
                        if params.get(name).is_none() {
                            return SkillValidation::InvalidInput {
                                errors: vec![format!("Missing required field: {}", name)],
                            };
                        }
                    }
                }
            }
        }

        SkillValidation::Valid
    }

    /// Find skills that should activate for the given context.
    pub fn find_matching_skills(
        &self,
        touched_files: &[&str],
        languages: &[&str],
    ) -> Vec<&CompiledSkill> {
        let mut matches: Vec<&CompiledSkill> = self
            .skills
            .values()
            .filter(|s| self.trigger_matches(&s.trigger, touched_files, languages))
            .collect();

        // Sort by priority (highest first)
        matches.sort_by(|a, b| b.priority.cmp(&a.priority));
        matches
    }

    /// Get a skill by ID.
    pub fn get(&self, id: &str) -> Option<&CompiledSkill> {
        self.skills.get(id)
    }

    /// Number of registered skills.
    pub fn count(&self) -> usize {
        self.skills.len()
    }

    fn trigger_matches(&self, trigger: &SkillTrigger, files: &[&str], languages: &[&str]) -> bool {
        match trigger {
            SkillTrigger::Always => true,
            SkillTrigger::PathPattern { patterns } => files
                .iter()
                .any(|f| patterns.iter().any(|p| simple_glob_match(p, f))),
            SkillTrigger::Language { languages: langs } => {
                languages.iter().any(|l| langs.iter().any(|sl| sl == l))
            }
            SkillTrigger::Dependency { .. } => false, // Needs project analysis
            SkillTrigger::Explicit { .. } => false,   // Only on explicit invocation
            SkillTrigger::All { triggers } => triggers
                .iter()
                .all(|t| self.trigger_matches(t, files, languages)),
            SkillTrigger::Any { triggers } => triggers
                .iter()
                .any(|t| self.trigger_matches(t, files, languages)),
        }
    }
}

impl Default for SkillKernel {
    fn default() -> Self {
        Self::new(SkillTrustTier::UserInstalled)
    }
}

/// Simple glob matching.
fn simple_glob_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(ext) = pattern.strip_prefix("*.") {
        return text.ends_with(&format!(".{}", ext));
    }
    if pattern.contains("**") {
        let parts: Vec<&str> = pattern.split("**").collect();
        if parts.len() == 2 {
            return text.starts_with(parts[0].trim_end_matches('/'))
                && text.ends_with(parts[1].trim_start_matches('/'));
        }
    }
    text.contains(pattern.trim_matches('*'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_matching() {
        let kernel = SkillKernel::default();

        assert!(kernel.trigger_matches(&SkillTrigger::Always, &[], &[]));

        let path_trigger = SkillTrigger::PathPattern {
            patterns: vec!["*.rs".to_string()],
        };
        assert!(kernel.trigger_matches(&path_trigger, &["src/lib.rs"], &[]));
        assert!(!kernel.trigger_matches(&path_trigger, &["src/lib.py"], &[]));

        let lang_trigger = SkillTrigger::Language {
            languages: vec!["rust".to_string()],
        };
        assert!(kernel.trigger_matches(&lang_trigger, &[], &["rust"]));
        assert!(!kernel.trigger_matches(&lang_trigger, &[], &["python"]));
    }

    #[test]
    fn validation_checks_trust() {
        let mut kernel = SkillKernel::new(SkillTrustTier::ProjectLocal);
        kernel.register(CompiledSkill {
            id: "untrusted-skill".to_string(),
            name: "Untrusted".to_string(),
            description: "test".to_string(),
            source: PathBuf::from("/tmp"),
            trigger: SkillTrigger::Always,
            template: String::new(),
            input_schema: None,
            output_schema: None,
            sandbox: SkillSandbox::default(),
            execution_mode: ExecutionMode::Inline,
            priority: 0,
            trust_tier: SkillTrustTier::Community, // Higher than minimum
        });

        let result = kernel.validate("untrusted-skill", &serde_json::json!({}), 100000);
        assert!(matches!(result, SkillValidation::TrustViolation { .. }));
    }
}
