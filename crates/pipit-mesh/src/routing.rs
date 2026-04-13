//! Policy-Aware Routing — tenant isolation and task routing policies.
//!
//! Three layers:
//! 1. TenantIsolation: namespace agents by tenant, enforce boundaries
//! 2. RoutingPolicy: rules for how tasks get assigned to agents
//! 3. PolicyRouter: combines discovery + policy + isolation into routing decisions
//!
//! Design principle: policy is declarative, not procedural. Define
//! constraints, and the router solves for the best assignment.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::registry::{AgentCapability, AgentDescriptor, AgentId, MeshRegistry};

// ── Tenant isolation ────────────────────────────────────────────────

/// Tenant identifier.
pub type TenantId = String;

/// Tenant namespace — isolates agents and data by tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tenant {
    pub id: TenantId,
    pub name: String,
    /// Agents belonging to this tenant.
    pub agent_ids: HashSet<AgentId>,
    /// Resource quotas.
    pub quotas: TenantQuotas,
    /// Current resource usage.
    pub usage: TenantUsage,
}

/// Resource limits for a tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantQuotas {
    /// Maximum concurrent agents.
    pub max_agents: u32,
    /// Maximum tasks per hour.
    pub max_tasks_per_hour: u32,
    /// Maximum total cost per day in USD.
    pub max_daily_cost_usd: f64,
    /// Maximum concurrent task executions.
    pub max_concurrent_tasks: u32,
}

impl Default for TenantQuotas {
    fn default() -> Self {
        Self {
            max_agents: 10,
            max_tasks_per_hour: 100,
            max_daily_cost_usd: 50.0,
            max_concurrent_tasks: 5,
        }
    }
}

/// Current resource consumption for a tenant.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TenantUsage {
    pub active_agents: u32,
    pub tasks_this_hour: u32,
    pub cost_today_usd: f64,
    pub concurrent_tasks: u32,
}

impl Tenant {
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            agent_ids: HashSet::new(),
            quotas: TenantQuotas::default(),
            usage: TenantUsage::default(),
        }
    }

    /// Check if this tenant can accept another task.
    pub fn can_accept_task(&self) -> Result<(), QuotaViolation> {
        if self.usage.tasks_this_hour >= self.quotas.max_tasks_per_hour {
            return Err(QuotaViolation::TaskRateExceeded {
                limit: self.quotas.max_tasks_per_hour,
                current: self.usage.tasks_this_hour,
            });
        }
        if self.usage.cost_today_usd >= self.quotas.max_daily_cost_usd {
            return Err(QuotaViolation::CostExceeded {
                limit: self.quotas.max_daily_cost_usd,
                current: self.usage.cost_today_usd,
            });
        }
        if self.usage.concurrent_tasks >= self.quotas.max_concurrent_tasks {
            return Err(QuotaViolation::ConcurrencyExceeded {
                limit: self.quotas.max_concurrent_tasks,
                current: self.usage.concurrent_tasks,
            });
        }
        Ok(())
    }

    /// Check if an agent belongs to this tenant.
    pub fn owns_agent(&self, agent_id: &str) -> bool {
        self.agent_ids.contains(agent_id)
    }
}

#[derive(Debug, Clone)]
pub enum QuotaViolation {
    TaskRateExceeded { limit: u32, current: u32 },
    CostExceeded { limit: f64, current: f64 },
    ConcurrencyExceeded { limit: u32, current: u32 },
    AgentLimitExceeded { limit: u32, current: u32 },
}

impl std::fmt::Display for QuotaViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QuotaViolation::TaskRateExceeded { limit, current } => {
                write!(f, "Task rate exceeded: {}/{} per hour", current, limit)
            }
            QuotaViolation::CostExceeded { limit, current } => {
                write!(f, "Daily cost exceeded: ${:.2}/${:.2}", current, limit)
            }
            QuotaViolation::ConcurrencyExceeded { limit, current } => {
                write!(f, "Concurrent task limit: {}/{}", current, limit)
            }
            QuotaViolation::AgentLimitExceeded { limit, current } => {
                write!(f, "Agent limit exceeded: {}/{}", current, limit)
            }
        }
    }
}

// ── Routing policies ────────────────────────────────────────────────

/// A routing rule that constrains task assignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingRule {
    pub name: String,
    pub condition: RoutingCondition,
    pub action: RoutingAction,
    /// Priority (higher = evaluated first).
    pub priority: u32,
}

/// Conditions under which a routing rule applies.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingCondition {
    /// Always applies.
    Always,
    /// Applies when task matches a tag pattern.
    TaskTag(String),
    /// Applies when the requesting agent has specific capabilities.
    RequesterHasCapability(String),
    /// Applies during specific hours (UTC).
    TimeWindow { start_hour: u32, end_hour: u32 },
    /// Applies when system load is above threshold.
    HighLoad { threshold: f64 },
}

/// What the router should do when a rule matches.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingAction {
    /// Route to a specific agent.
    RouteToAgent(AgentId),
    /// Route to agents with a specific tag.
    RouteToTag(String),
    /// Require human approval before routing.
    RequireApproval,
    /// Block the task entirely.
    Block(String),
    /// Add priority boost.
    PriorityBoost(i32),
    /// Limit to agents in a specific tenant.
    LimitToTenant(TenantId),
}

// ── Policy router ───────────────────────────────────────────────────

/// A task routing request.
#[derive(Debug, Clone)]
pub struct RoutingRequest {
    pub task_id: String,
    pub tenant_id: Option<TenantId>,
    pub capability: AgentCapability,
    pub tags: Vec<String>,
    pub priority: i32,
}

/// A routing decision.
#[derive(Debug, Clone)]
pub struct RoutingDecision {
    pub task_id: String,
    /// Selected agent (if any).
    pub agent: Option<AgentDescriptor>,
    /// Similarity score of the selected agent.
    pub score: f64,
    /// Priority (possibly boosted by rules).
    pub priority: i32,
    /// Whether approval is required.
    pub requires_approval: bool,
    /// Reason if blocked.
    pub blocked: Option<String>,
    /// Rules that fired.
    pub applied_rules: Vec<String>,
}

/// The policy router: combines registry discovery with policy rules and tenant isolation.
pub struct PolicyRouter {
    rules: Vec<RoutingRule>,
    tenants: HashMap<TenantId, Tenant>,
}

impl PolicyRouter {
    pub fn new() -> Self {
        Self {
            rules: Vec::new(),
            tenants: HashMap::new(),
        }
    }

    /// Add a routing rule.
    pub fn add_rule(&mut self, rule: RoutingRule) {
        self.rules.push(rule);
        self.rules.sort_by(|a, b| b.priority.cmp(&a.priority));
    }

    /// Register a tenant.
    pub fn add_tenant(&mut self, tenant: Tenant) {
        self.tenants.insert(tenant.id.clone(), tenant);
    }

    /// Get a tenant by ID.
    pub fn get_tenant(&self, id: &str) -> Option<&Tenant> {
        self.tenants.get(id)
    }

    /// Get a mutable tenant reference.
    pub fn get_tenant_mut(&mut self, id: &str) -> Option<&mut Tenant> {
        self.tenants.get_mut(id)
    }

    /// Route a task: apply policies, check tenant quotas, discover best agent.
    pub fn route(&self, request: &RoutingRequest, registry: &MeshRegistry) -> RoutingDecision {
        let mut decision = RoutingDecision {
            task_id: request.task_id.clone(),
            agent: None,
            score: 0.0,
            priority: request.priority,
            requires_approval: false,
            blocked: None,
            applied_rules: Vec::new(),
        };

        // Phase 1: Evaluate routing rules
        let mut forced_agent: Option<AgentId> = None;
        let mut forced_tag: Option<String> = None;
        let mut forced_tenant: Option<TenantId> = None;

        for rule in &self.rules {
            if self.condition_matches(&rule.condition, request) {
                decision.applied_rules.push(rule.name.clone());

                match &rule.action {
                    RoutingAction::Block(reason) => {
                        decision.blocked = Some(reason.clone());
                        return decision;
                    }
                    RoutingAction::RequireApproval => {
                        decision.requires_approval = true;
                    }
                    RoutingAction::PriorityBoost(boost) => {
                        decision.priority += boost;
                    }
                    RoutingAction::RouteToAgent(id) => {
                        forced_agent = Some(id.clone());
                    }
                    RoutingAction::RouteToTag(tag) => {
                        forced_tag = Some(tag.clone());
                    }
                    RoutingAction::LimitToTenant(tid) => {
                        forced_tenant = Some(tid.clone());
                    }
                }
            }
        }

        // Phase 2: Tenant quota check
        let effective_tenant = forced_tenant.as_deref().or(request.tenant_id.as_deref());

        if let Some(tenant_id) = effective_tenant {
            if let Some(tenant) = self.tenants.get(tenant_id) {
                if let Err(violation) = tenant.can_accept_task() {
                    decision.blocked = Some(format!("Tenant quota: {}", violation));
                    return decision;
                }
            }
        }

        // Phase 3: Discover agents
        let mut capability = request.capability.clone();
        if let Some(tag) = forced_tag {
            capability.required_tags.insert(tag);
        }

        let candidates = registry.discover(&capability);

        // Phase 4: Filter by tenant if applicable
        let filtered: Vec<(AgentDescriptor, f64)> = if let Some(tid) = effective_tenant {
            if let Some(tenant) = self.tenants.get(tid) {
                candidates
                    .into_iter()
                    .filter(|(agent, _)| tenant.owns_agent(&agent.id))
                    .collect()
            } else {
                candidates
            }
        } else {
            candidates
        };

        // Phase 5: Select best agent
        if let Some(forced) = forced_agent {
            // Forced routing — find the specific agent
            if let Some((agent, score)) = filtered.iter().find(|(a, _)| a.id == forced) {
                decision.agent = Some(agent.clone());
                decision.score = *score;
            }
        } else if let Some((agent, score)) = filtered.first() {
            decision.agent = Some(agent.clone());
            decision.score = *score;
        }

        decision
    }

    /// Check if a routing condition matches the current request.
    fn condition_matches(&self, condition: &RoutingCondition, request: &RoutingRequest) -> bool {
        match condition {
            RoutingCondition::Always => true,
            RoutingCondition::TaskTag(tag) => request.tags.contains(tag),
            RoutingCondition::RequesterHasCapability(cap) => {
                request.capability.required_tags.contains(cap)
            }
            RoutingCondition::TimeWindow {
                start_hour,
                end_hour,
            } => {
                let hour = chrono::Utc::now()
                    .format("%H")
                    .to_string()
                    .parse::<u32>()
                    .unwrap_or(0);
                if start_hour <= end_hour {
                    hour >= *start_hour && hour < *end_hour
                } else {
                    // Wraps midnight
                    hour >= *start_hour || hour < *end_hour
                }
            }
            RoutingCondition::HighLoad { .. } => {
                // Would need system load metrics — false by default
                false
            }
        }
    }
}

impl Default for PolicyRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn make_registry_with_agents() -> MeshRegistry {
        let registry = MeshRegistry::new();

        registry.register(AgentDescriptor {
            id: "rust-agent".into(),
            name: "Rust Expert".into(),
            tools: BTreeSet::from(["bash".into(), "cargo".into()]),
            languages: BTreeSet::from(["rust".into()]),
            projects: BTreeSet::new(),
            tags: BTreeSet::from(["security".into()]),
            endpoint: "local".into(),
            last_seen: chrono::Utc::now(),
        });

        registry.register(AgentDescriptor {
            id: "python-agent".into(),
            name: "Python Expert".into(),
            tools: BTreeSet::from(["bash".into(), "pytest".into()]),
            languages: BTreeSet::from(["python".into()]),
            projects: BTreeSet::new(),
            tags: BTreeSet::from(["testing".into()]),
            endpoint: "local".into(),
            last_seen: chrono::Utc::now(),
        });

        registry
    }

    #[test]
    fn test_basic_routing() {
        let registry = make_registry_with_agents();
        let router = PolicyRouter::new();

        let request = RoutingRequest {
            task_id: "task-1".into(),
            tenant_id: None,
            capability: AgentCapability {
                required_tools: BTreeSet::from(["pytest".into()]),
                required_languages: BTreeSet::from(["python".into()]),
                required_tags: BTreeSet::new(),
            },
            tags: vec![],
            priority: 0,
        };

        let decision = router.route(&request, &registry);
        assert!(decision.agent.is_some());
        assert_eq!(decision.agent.unwrap().id, "python-agent");
        assert!(decision.blocked.is_none());
    }

    #[test]
    fn test_tenant_isolation() {
        let registry = make_registry_with_agents();
        let mut router = PolicyRouter::new();

        let mut tenant = Tenant::new("acme", "Acme Corp");
        tenant.agent_ids.insert("rust-agent".into());
        router.add_tenant(tenant);

        let request = RoutingRequest {
            task_id: "task-2".into(),
            tenant_id: Some("acme".into()),
            capability: AgentCapability {
                required_tools: BTreeSet::from(["bash".into()]),
                required_languages: BTreeSet::new(),
                required_tags: BTreeSet::new(),
            },
            tags: vec![],
            priority: 0,
        };

        let decision = router.route(&request, &registry);
        // Should only find rust-agent (owned by acme), not python-agent
        assert!(decision.agent.is_some());
        assert_eq!(decision.agent.unwrap().id, "rust-agent");
    }

    #[test]
    fn test_quota_enforcement() {
        let registry = make_registry_with_agents();
        let mut router = PolicyRouter::new();

        let mut tenant = Tenant::new("small", "Small Co");
        tenant.quotas.max_tasks_per_hour = 5;
        tenant.usage.tasks_this_hour = 5; // At limit
        tenant.agent_ids.insert("rust-agent".into());
        router.add_tenant(tenant);

        let request = RoutingRequest {
            task_id: "task-3".into(),
            tenant_id: Some("small".into()),
            capability: AgentCapability {
                required_tools: BTreeSet::new(),
                required_languages: BTreeSet::new(),
                required_tags: BTreeSet::new(),
            },
            tags: vec![],
            priority: 0,
        };

        let decision = router.route(&request, &registry);
        assert!(decision.blocked.is_some());
        assert!(decision.blocked.unwrap().contains("quota"));
    }

    #[test]
    fn test_routing_rule_block() {
        let registry = make_registry_with_agents();
        let mut router = PolicyRouter::new();

        router.add_rule(RoutingRule {
            name: "block-dangerous".into(),
            condition: RoutingCondition::TaskTag("dangerous".into()),
            action: RoutingAction::Block("Dangerous tasks not allowed".into()),
            priority: 100,
        });

        let request = RoutingRequest {
            task_id: "task-4".into(),
            tenant_id: None,
            capability: AgentCapability {
                required_tools: BTreeSet::new(),
                required_languages: BTreeSet::new(),
                required_tags: BTreeSet::new(),
            },
            tags: vec!["dangerous".into()],
            priority: 0,
        };

        let decision = router.route(&request, &registry);
        assert!(decision.blocked.is_some());
    }

    #[test]
    fn test_priority_boost() {
        let registry = make_registry_with_agents();
        let mut router = PolicyRouter::new();

        router.add_rule(RoutingRule {
            name: "boost-security".into(),
            condition: RoutingCondition::TaskTag("security".into()),
            action: RoutingAction::PriorityBoost(10),
            priority: 50,
        });

        let request = RoutingRequest {
            task_id: "task-5".into(),
            tenant_id: None,
            capability: AgentCapability {
                required_tools: BTreeSet::new(),
                required_languages: BTreeSet::new(),
                required_tags: BTreeSet::new(),
            },
            tags: vec!["security".into()],
            priority: 5,
        };

        let decision = router.route(&request, &registry);
        assert_eq!(decision.priority, 15); // 5 + 10
    }
}
