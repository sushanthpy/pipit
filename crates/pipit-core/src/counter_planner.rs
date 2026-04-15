//! # Adversarial Counter-Planner (C4)
//!
//! Red-team adversarial strategy generator that stress-tests proposed plans
//! by generating worst-case scenarios and failure modes.
//!
//! ## Approach
//!
//! Given a plan (sequence of tool calls), the counter-planner generates:
//! 1. **Failure injections**: What if tool X fails at step N?
//! 2. **Adversarial inputs**: Edge cases in tool arguments that could break things
//! 3. **Resource attacks**: What if budget/time runs out mid-plan?
//! 4. **Concurrency hazards**: TOCTOU races in file operations

use serde::{Deserialize, Serialize};

/// A planned step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub tool: String,
    pub args_summary: String,
    pub expected_outcome: String,
}

/// An adversarial challenge to a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Challenge {
    pub category: ChallengeCategory,
    pub target_step: usize,
    pub description: String,
    pub severity: Severity,
    pub mitigation: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChallengeCategory {
    ToolFailure,
    AdversarialInput,
    ResourceExhaustion,
    ConcurrencyHazard,
    PermissionEscalation,
    DataLoss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

/// Results from adversarial analysis.
#[derive(Debug, Clone)]
pub struct AdversarialReport {
    pub challenges: Vec<Challenge>,
    pub overall_risk: Severity,
    pub plan_is_safe: bool,
}

impl AdversarialReport {
    /// Count challenges by severity.
    pub fn count_by_severity(&self, severity: Severity) -> usize {
        self.challenges.iter().filter(|c| c.severity == severity).count()
    }

    /// Get critical challenges.
    pub fn critical_challenges(&self) -> Vec<&Challenge> {
        self.challenges
            .iter()
            .filter(|c| c.severity == Severity::Critical)
            .collect()
    }
}

/// The adversarial counter-planner.
pub struct CounterPlanner {
    /// Maximum challenges to generate per plan.
    max_challenges: usize,
    /// Known dangerous tool patterns.
    dangerous_patterns: Vec<DangerousPattern>,
}

#[derive(Debug, Clone)]
struct DangerousPattern {
    tool: String,
    pattern: String,
    category: ChallengeCategory,
    severity: Severity,
    description: String,
}

impl Default for CounterPlanner {
    fn default() -> Self {
        Self::new(50)
    }
}

impl CounterPlanner {
    pub fn new(max_challenges: usize) -> Self {
        let dangerous_patterns = vec![
            DangerousPattern {
                tool: "bash".into(),
                pattern: "rm ".into(),
                category: ChallengeCategory::DataLoss,
                severity: Severity::Critical,
                description: "Destructive file removal in shell".into(),
            },
            DangerousPattern {
                tool: "bash".into(),
                pattern: "sudo".into(),
                category: ChallengeCategory::PermissionEscalation,
                severity: Severity::Critical,
                description: "Privilege escalation via sudo".into(),
            },
            DangerousPattern {
                tool: "bash".into(),
                pattern: "curl".into(),
                category: ChallengeCategory::AdversarialInput,
                severity: Severity::Medium,
                description: "External data fetched without validation".into(),
            },
            DangerousPattern {
                tool: "edit_file".into(),
                pattern: "credentials".into(),
                category: ChallengeCategory::DataLoss,
                severity: Severity::High,
                description: "Editing credential files".into(),
            },
            DangerousPattern {
                tool: "bash".into(),
                pattern: "git push".into(),
                category: ChallengeCategory::DataLoss,
                severity: Severity::High,
                description: "Pushing to remote without review".into(),
            },
        ];

        Self {
            max_challenges,
            dangerous_patterns,
        }
    }

    /// Analyze a plan and generate adversarial challenges.
    pub fn analyze(&self, plan: &[PlanStep]) -> AdversarialReport {
        let mut challenges = Vec::new();

        for (i, step) in plan.iter().enumerate() {
            // Check dangerous patterns
            for pattern in &self.dangerous_patterns {
                if step.tool == pattern.tool && step.args_summary.contains(&pattern.pattern) {
                    challenges.push(Challenge {
                        category: pattern.category,
                        target_step: i,
                        description: pattern.description.clone(),
                        severity: pattern.severity,
                        mitigation: Some(format!(
                            "Add confirmation gate before step {}",
                            i
                        )),
                    });
                }
            }

            // Tool failure injection for every step
            challenges.push(Challenge {
                category: ChallengeCategory::ToolFailure,
                target_step: i,
                description: format!(
                    "What if '{}' fails at step {}?",
                    step.tool, i
                ),
                severity: if i == plan.len() - 1 {
                    Severity::Low
                } else {
                    Severity::Medium
                },
                mitigation: Some("Add error handling/retry logic".into()),
            });

            // TOCTOU for file operations
            if step.tool == "edit_file" || step.tool == "write_file" {
                let reads_same = plan[..i].iter().any(|prev| {
                    prev.tool == "read_file" && prev.args_summary == step.args_summary
                });
                if reads_same {
                    challenges.push(Challenge {
                        category: ChallengeCategory::ConcurrencyHazard,
                        target_step: i,
                        description: format!(
                            "TOCTOU: file may have changed between read and write at step {}",
                            i
                        ),
                        severity: Severity::Medium,
                        mitigation: Some("Use atomic write with content hash check".into()),
                    });
                }
            }

            if challenges.len() >= self.max_challenges {
                break;
            }
        }

        let overall_risk = challenges
            .iter()
            .map(|c| c.severity)
            .max()
            .unwrap_or(Severity::Low);

        let plan_is_safe = !challenges
            .iter()
            .any(|c| c.severity == Severity::Critical);

        AdversarialReport {
            challenges,
            overall_risk,
            plan_is_safe,
        }
    }

    /// Add a custom dangerous pattern.
    pub fn add_pattern(
        &mut self,
        tool: &str,
        pattern: &str,
        category: ChallengeCategory,
        severity: Severity,
        desc: &str,
    ) {
        self.dangerous_patterns.push(DangerousPattern {
            tool: tool.into(),
            pattern: pattern.into(),
            category,
            severity,
            description: desc.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rm_as_critical() {
        let planner = CounterPlanner::default();
        let plan = vec![PlanStep {
            tool: "bash".into(),
            args_summary: "rm -rf /tmp/build".into(),
            expected_outcome: "clean build dir".into(),
        }];
        let report = planner.analyze(&plan);
        assert!(!report.plan_is_safe);
        assert!(report.count_by_severity(Severity::Critical) >= 1);
    }

    #[test]
    fn detects_sudo_as_critical() {
        let planner = CounterPlanner::default();
        let plan = vec![PlanStep {
            tool: "bash".into(),
            args_summary: "sudo apt install foo".into(),
            expected_outcome: "install package".into(),
        }];
        let report = planner.analyze(&plan);
        assert!(!report.plan_is_safe);
    }

    #[test]
    fn safe_plan_passes() {
        let planner = CounterPlanner::default();
        let plan = vec![
            PlanStep {
                tool: "read_file".into(),
                args_summary: "src/main.rs".into(),
                expected_outcome: "read source".into(),
            },
            PlanStep {
                tool: "edit_file".into(),
                args_summary: "src/lib.rs".into(),
                expected_outcome: "add function".into(),
            },
        ];
        let report = planner.analyze(&plan);
        assert!(report.plan_is_safe);
    }

    #[test]
    fn detects_toctou() {
        let planner = CounterPlanner::default();
        let plan = vec![
            PlanStep {
                tool: "read_file".into(),
                args_summary: "config.toml".into(),
                expected_outcome: "read config".into(),
            },
            PlanStep {
                tool: "edit_file".into(),
                args_summary: "config.toml".into(),
                expected_outcome: "update config".into(),
            },
        ];
        let report = planner.analyze(&plan);
        let toctou = report
            .challenges
            .iter()
            .any(|c| c.category == ChallengeCategory::ConcurrencyHazard);
        assert!(toctou);
    }

    #[test]
    fn every_step_gets_failure_challenge() {
        let planner = CounterPlanner::default();
        let plan = vec![
            PlanStep {
                tool: "read_file".into(),
                args_summary: "a.rs".into(),
                expected_outcome: "read".into(),
            },
            PlanStep {
                tool: "read_file".into(),
                args_summary: "b.rs".into(),
                expected_outcome: "read".into(),
            },
        ];
        let report = planner.analyze(&plan);
        let failures = report
            .challenges
            .iter()
            .filter(|c| c.category == ChallengeCategory::ToolFailure)
            .count();
        assert_eq!(failures, 2);
    }

    #[test]
    fn custom_pattern() {
        let mut planner = CounterPlanner::new(10);
        planner.add_pattern(
            "bash",
            "DROP TABLE",
            ChallengeCategory::DataLoss,
            Severity::Critical,
            "SQL injection risk",
        );
        let plan = vec![PlanStep {
            tool: "bash".into(),
            args_summary: "psql -c 'DROP TABLE users'".into(),
            expected_outcome: "delete table".into(),
        }];
        let report = planner.analyze(&plan);
        assert!(!report.plan_is_safe);
    }
}
