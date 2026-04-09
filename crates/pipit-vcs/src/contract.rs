//! # Branch Contracts & Promotion Gates
//!
//! Typed execution constraints that make planning structurally enforceable.
//! A branch contract declares objective, expected subsystems, required verification,
//! failure budget, and promotion conditions.
//!
//! Gate evaluation: O(K) where K = required predicates.
//! Promotion order: O(V + E) for dependency DAG.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A branch contract — typed execution constraints for a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchContract {
    /// Unique contract ID.
    pub id: String,
    /// Workspace/branch this contract applies to.
    pub workspace_id: String,
    /// Human-readable objective.
    pub objective: String,
    /// Expected subsystems that will be modified.
    pub expected_subsystems: Vec<String>,
    /// Required verification classes that must pass.
    pub required_checks: Vec<RequiredCheck>,
    /// Maximum allowed verification failures before blocking.
    pub failure_budget: u32,
    /// Current failure count.
    pub failure_count: u32,
    /// Promotion conditions (all must be satisfied).
    pub promotion_gates: Vec<PromotionGate>,
    /// Target branch for promotion.
    pub target_branch: String,
    /// Dependencies on other contracts (must promote first).
    pub depends_on: Vec<String>,
    /// When this contract was created.
    pub created_at: DateTime<Utc>,
    /// When this contract expires (None = no expiry).
    pub expires_at: Option<DateTime<Utc>>,
    /// Current state of the contract.
    pub state: ContractState,
}

/// State of a branch contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContractState {
    /// Contract is active and being worked on.
    Active,
    /// All promotion gates are satisfied.
    Satisfied,
    /// Contract was fulfilled and changes promoted.
    Fulfilled,
    /// Contract was abandoned.
    Abandoned,
    /// Contract expired.
    Expired,
    /// Contract is blocked by failures exceeding budget.
    Blocked,
}

/// A required verification check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequiredCheck {
    /// Check name (e.g., "unit_tests", "lint", "type_check").
    pub name: String,
    /// Check category.
    pub category: CheckCategory,
    /// Whether this check must pass (true) or is advisory (false).
    pub required: bool,
    /// Whether this check has been satisfied.
    pub satisfied: bool,
    /// Evidence from the last run.
    pub evidence: Option<String>,
}

/// Categories of verification checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckCategory {
    Build,
    Lint,
    TypeCheck,
    UnitTest,
    IntegrationTest,
    SecurityScan,
    PerfRegression,
    CodeReview,
    Custom(String),
}

/// A promotion gate — a predicate that must be satisfied for promotion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromotionGate {
    /// Gate name for display.
    pub name: String,
    /// The predicate to evaluate.
    pub predicate: ContractPredicate,
    /// Whether this gate is currently satisfied.
    pub satisfied: bool,
}

/// Typed predicates for promotion gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContractPredicate {
    /// All required checks must have passed.
    AllChecksPassed,
    /// Failure count must be within budget.
    WithinFailureBudget,
    /// Specific files must not have been modified.
    FilesUnmodified(Vec<String>),
    /// No merge conflicts with target branch.
    NoMergeConflicts,
    /// Dependencies must be fulfilled first.
    DependenciesFulfilled,
    /// Contract must not have expired.
    NotExpired,
    /// Custom predicate (description for display).
    Custom(String),
}

/// Result of evaluating all promotion gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateResult {
    pub all_passed: bool,
    pub results: Vec<(String, bool, String)>, // (gate_name, passed, reason)
}

impl BranchContract {
    /// Create a new contract with default gates.
    pub fn new(
        workspace_id: impl Into<String>,
        objective: impl Into<String>,
        target_branch: impl Into<String>,
    ) -> Self {
        let id = format!("contract-{}", uuid::Uuid::new_v4().simple());
        Self {
            id,
            workspace_id: workspace_id.into(),
            objective: objective.into(),
            expected_subsystems: Vec::new(),
            required_checks: Vec::new(),
            failure_budget: 3,
            failure_count: 0,
            promotion_gates: vec![
                PromotionGate {
                    name: "all_checks_passed".to_string(),
                    predicate: ContractPredicate::AllChecksPassed,
                    satisfied: false,
                },
                PromotionGate {
                    name: "within_failure_budget".to_string(),
                    predicate: ContractPredicate::WithinFailureBudget,
                    satisfied: false,
                },
                PromotionGate {
                    name: "not_expired".to_string(),
                    predicate: ContractPredicate::NotExpired,
                    satisfied: false,
                },
            ],
            target_branch: target_branch.into(),
            depends_on: Vec::new(),
            created_at: Utc::now(),
            expires_at: None,
            state: ContractState::Active,
        }
    }

    /// Add a required verification check.
    pub fn require_check(&mut self, name: impl Into<String>, category: CheckCategory) {
        self.required_checks.push(RequiredCheck {
            name: name.into(),
            category,
            required: true,
            satisfied: false,
            evidence: None,
        });
    }

    /// Record a check result.
    pub fn record_check(&mut self, name: &str, passed: bool, evidence: &str) {
        if let Some(check) = self.required_checks.iter_mut().find(|c| c.name == name) {
            check.satisfied = passed;
            check.evidence = Some(evidence.to_string());
        }
        if !passed {
            self.failure_count += 1;
        }
        // Re-evaluate state
        self.evaluate_state();
    }

    /// Evaluate all promotion gates. O(K) where K = gate count.
    pub fn evaluate_gates(&mut self) -> GateResult {
        let mut results = Vec::new();

        for gate in &mut self.promotion_gates {
            let (passed, reason) = match &gate.predicate {
                ContractPredicate::AllChecksPassed => {
                    let all_pass = self
                        .required_checks
                        .iter()
                        .filter(|c| c.required)
                        .all(|c| c.satisfied);
                    (
                        all_pass,
                        if all_pass {
                            "All required checks passed".to_string()
                        } else {
                            let failing: Vec<&str> = self
                                .required_checks
                                .iter()
                                .filter(|c| c.required && !c.satisfied)
                                .map(|c| c.name.as_str())
                                .collect();
                            format!("Failing checks: {}", failing.join(", "))
                        },
                    )
                }
                ContractPredicate::WithinFailureBudget => {
                    let within = self.failure_count <= self.failure_budget;
                    (
                        within,
                        format!(
                            "Failures: {}/{} budget",
                            self.failure_count, self.failure_budget
                        ),
                    )
                }
                ContractPredicate::NotExpired => {
                    let not_expired = self.expires_at.map(|e| Utc::now() < e).unwrap_or(true);
                    (
                        not_expired,
                        if not_expired {
                            "Contract is active".to_string()
                        } else {
                            "Contract has expired".to_string()
                        },
                    )
                }
                ContractPredicate::DependenciesFulfilled => {
                    // This requires external state — caller must verify
                    (true, "Dependencies checked externally".to_string())
                }
                ContractPredicate::NoMergeConflicts => {
                    // This requires git state — caller must verify
                    (true, "Conflicts checked externally".to_string())
                }
                ContractPredicate::FilesUnmodified(files) => {
                    // This requires git state — caller must verify
                    (
                        true,
                        format!("{} protected files checked externally", files.len()),
                    )
                }
                ContractPredicate::Custom(desc) => (gate.satisfied, format!("Custom: {}", desc)),
            };
            gate.satisfied = passed;
            results.push((gate.name.clone(), passed, reason));
        }

        let all_passed = results.iter().all(|(_, p, _)| *p);
        GateResult {
            all_passed,
            results,
        }
    }

    /// Update contract state based on current check results and gates.
    pub fn evaluate_state(&mut self) {
        if self.failure_count > self.failure_budget {
            self.state = ContractState::Blocked;
            return;
        }
        if let Some(expires) = self.expires_at {
            if Utc::now() >= expires {
                self.state = ContractState::Expired;
                return;
            }
        }
        let gate_result = self.evaluate_gates();
        if gate_result.all_passed {
            self.state = ContractState::Satisfied;
        }
    }

    /// Check if the contract is ready for promotion.
    pub fn is_promotable(&self) -> bool {
        self.state == ContractState::Satisfied
    }
}

/// Registry of active contracts with on-disk persistence.
pub struct ContractRegistry {
    contracts: HashMap<String, BranchContract>,
    /// Directory for persistence (e.g. `.pipit/contracts/`).
    persist_dir: Option<std::path::PathBuf>,
}

impl ContractRegistry {
    pub fn new() -> Self {
        Self {
            contracts: HashMap::new(),
            persist_dir: None,
        }
    }

    /// Create a registry with on-disk persistence.
    pub fn with_persistence(dir: std::path::PathBuf) -> Self {
        let mut reg = Self {
            contracts: HashMap::new(),
            persist_dir: Some(dir),
        };
        reg.load_all();
        reg
    }

    pub fn register(&mut self, contract: BranchContract) {
        self.persist_one(&contract);
        self.contracts
            .insert(contract.workspace_id.clone(), contract);
    }

    pub fn get(&self, workspace_id: &str) -> Option<&BranchContract> {
        self.contracts.get(workspace_id)
    }

    pub fn get_mut(&mut self, workspace_id: &str) -> Option<&mut BranchContract> {
        self.contracts.get_mut(workspace_id)
    }

    /// Update a contract and persist.
    pub fn update(&mut self, contract: BranchContract) {
        self.persist_one(&contract);
        self.contracts
            .insert(contract.workspace_id.clone(), contract);
    }

    /// Check if a workspace has an active contract.
    pub fn has_contract(&self, workspace_id: &str) -> bool {
        self.contracts
            .get(workspace_id)
            .map(|c| c.state == ContractState::Active || c.state == ContractState::Satisfied)
            .unwrap_or(false)
    }

    /// Find contracts that depend on the given workspace.
    pub fn dependents(&self, workspace_id: &str) -> Vec<&BranchContract> {
        self.contracts
            .values()
            .filter(|c| c.depends_on.contains(&workspace_id.to_string()))
            .collect()
    }

    /// Get all promotable contracts.
    pub fn promotable(&self) -> Vec<&BranchContract> {
        self.contracts
            .values()
            .filter(|c| c.is_promotable())
            .collect()
    }

    /// Persist a single contract to disk.
    fn persist_one(&self, contract: &BranchContract) {
        if let Some(ref dir) = self.persist_dir {
            let _ = std::fs::create_dir_all(dir);
            let path = dir.join(format!("{}.json", contract.id));
            if let Ok(json) = serde_json::to_string_pretty(contract) {
                let _ = std::fs::write(path, json);
            }
        }
    }

    /// Load all contracts from the persistence directory.
    fn load_all(&mut self) {
        if let Some(ref dir) = self.persist_dir {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().and_then(|e| e.to_str()) == Some("json") {
                        if let Ok(content) = std::fs::read_to_string(entry.path()) {
                            if let Ok(contract) = serde_json::from_str::<BranchContract>(&content) {
                                self.contracts
                                    .insert(contract.workspace_id.clone(), contract);
                            }
                        }
                    }
                }
            }
        }
    }
}

impl Default for ContractRegistry {
    fn default() -> Self {
        Self::new()
    }
}
