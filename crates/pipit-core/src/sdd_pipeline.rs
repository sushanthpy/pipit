//! Spec-Driven Development Pipeline — Gap #3
//!
//! End-to-end: spec → decompose into tasks → assign to subagents → verify against spec.
//! Uses pipit-spec's CSL constraints + Z3 for formal verification.
//!
//! Pipeline:
//! 1. Parse spec from PIPIT.md / .pipit-spec files
//! 2. Decompose into ordered implementation tasks
//! 3. Execute each task (optionally via isolated subagent)
//! 4. Verify: run tests + check formal constraints
//! 5. Atomic commit per verified task

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A task decomposed from a specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecTask {
    pub id: usize,
    pub title: String,
    pub description: String,
    pub dependencies: Vec<usize>,
    pub files_to_create: Vec<String>,
    pub files_to_modify: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub status: TaskStatus,
    pub isolated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Verified,
}

/// A complete spec-driven development plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecPlan {
    pub spec_source: String,
    pub tasks: Vec<SpecTask>,
    pub verification_command: Option<String>,
    pub atomic_commits: bool,
}

/// Result of executing a spec plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecPlanResult {
    pub tasks_total: usize,
    pub tasks_completed: usize,
    pub tasks_verified: usize,
    pub tasks_failed: usize,
    pub task_results: Vec<SpecTaskResult>,
    pub all_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecTaskResult {
    pub task_id: usize,
    pub task_title: String,
    pub status: TaskStatus,
    pub output: String,
    pub verification_passed: bool,
    pub commit_sha: Option<String>,
}

impl SpecPlan {
    /// Create a plan from a list of tasks.
    pub fn new(spec_source: &str, tasks: Vec<SpecTask>) -> Self {
        Self {
            spec_source: spec_source.to_string(),
            tasks,
            verification_command: None,
            atomic_commits: true,
        }
    }

    /// Get tasks in dependency order (topological sort).
    pub fn execution_order(&self) -> Vec<&SpecTask> {
        let mut result = Vec::new();
        let mut completed: std::collections::HashSet<usize> = std::collections::HashSet::new();

        loop {
            let ready: Vec<&SpecTask> = self.tasks.iter()
                .filter(|t| t.status != TaskStatus::Completed && t.status != TaskStatus::Verified)
                .filter(|t| !completed.contains(&t.id))
                .filter(|t| t.dependencies.iter().all(|dep| completed.contains(dep)))
                .collect();

            if ready.is_empty() { break; }

            for task in &ready {
                completed.insert(task.id);
                result.push(*task);
            }
        }

        result
    }

    /// Get tasks that can run in parallel (no mutual dependencies).
    pub fn parallelizable_groups(&self) -> Vec<Vec<&SpecTask>> {
        let mut groups = Vec::new();
        let mut completed: std::collections::HashSet<usize> = std::collections::HashSet::new();

        loop {
            let ready: Vec<&SpecTask> = self.tasks.iter()
                .filter(|t| !completed.contains(&t.id))
                .filter(|t| t.dependencies.iter().all(|dep| completed.contains(dep)))
                .collect();

            if ready.is_empty() { break; }

            for task in &ready {
                completed.insert(task.id);
            }
            groups.push(ready);
        }

        groups
    }

    /// Generate the agent prompt for a specific task.
    pub fn task_prompt(&self, task: &SpecTask) -> String {
        let mut prompt = format!(
            "## Task {}: {}\n\n{}\n\n",
            task.id, task.title, task.description
        );

        if !task.files_to_create.is_empty() {
            prompt.push_str("### Files to create:\n");
            for f in &task.files_to_create {
                prompt.push_str(&format!("- {}\n", f));
            }
            prompt.push('\n');
        }

        if !task.files_to_modify.is_empty() {
            prompt.push_str("### Files to modify:\n");
            for f in &task.files_to_modify {
                prompt.push_str(&format!("- {}\n", f));
            }
            prompt.push('\n');
        }

        if !task.acceptance_criteria.is_empty() {
            prompt.push_str("### Acceptance criteria:\n");
            for c in &task.acceptance_criteria {
                prompt.push_str(&format!("- {}\n", c));
            }
            prompt.push('\n');
        }

        if let Some(ref verify_cmd) = self.verification_command {
            prompt.push_str(&format!(
                "### Verification:\nAfter completing changes, run: `{}`\nAll criteria must pass.\n",
                verify_cmd
            ));
        }

        prompt.push_str("\nMake minimal, focused changes for this task only. Run tests to verify.");
        prompt
    }
}

/// Parse a spec-driven plan from a PIPIT.md or spec file.
/// The spec format uses markdown headers for tasks:
///
/// ```markdown
/// # Spec: Payment Processing
///
/// ## Task 1: Create payment model
/// Create the payment data model with validation.
/// - Files: src/models/payment.py
/// - Depends: (none)
/// - Criteria: Model validates amount > 0, currency is valid
///
/// ## Task 2: Add payment endpoint
/// Add POST /payments endpoint.
/// - Files: src/routes/payments.py
/// - Depends: 1
/// - Criteria: Returns 201 on success, 400 on invalid input
/// ```
pub fn parse_spec_plan(source: &str) -> SpecPlan {
    let mut tasks = Vec::new();
    let mut current_task: Option<SpecTask> = None;
    let mut current_description = String::new();
    let mut task_id = 0;
    let mut verify_cmd = None;

    for line in source.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with("## Task") || trimmed.starts_with("## task") {
            // Save previous task
            if let Some(mut task) = current_task.take() {
                task.description = current_description.trim().to_string();
                tasks.push(task);
                current_description.clear();
            }

            task_id += 1;
            let title = trimmed.trim_start_matches('#').trim();
            let title = if let Some(idx) = title.find(':') {
                title[idx + 1..].trim().to_string()
            } else {
                title.to_string()
            };

            current_task = Some(SpecTask {
                id: task_id,
                title,
                description: String::new(),
                dependencies: Vec::new(),
                files_to_create: Vec::new(),
                files_to_modify: Vec::new(),
                acceptance_criteria: Vec::new(),
                status: TaskStatus::Pending,
                isolated: false,
            });
        } else if let Some(ref mut task) = current_task {
            if trimmed.starts_with("- Files:") || trimmed.starts_with("- files:") {
                let files: Vec<String> = trimmed.trim_start_matches("- Files:").trim_start_matches("- files:")
                    .split(',')
                    .map(|f| f.trim().to_string())
                    .filter(|f| !f.is_empty())
                    .collect();
                task.files_to_modify.extend(files);
            } else if trimmed.starts_with("- Depends:") || trimmed.starts_with("- depends:") {
                let deps: Vec<usize> = trimmed.trim_start_matches("- Depends:").trim_start_matches("- depends:")
                    .split(',')
                    .filter_map(|d| d.trim().parse().ok())
                    .collect();
                task.dependencies = deps;
            } else if trimmed.starts_with("- Criteria:") || trimmed.starts_with("- criteria:") {
                let criteria = trimmed.trim_start_matches("- Criteria:").trim_start_matches("- criteria:").trim();
                task.acceptance_criteria.push(criteria.to_string());
            } else if trimmed.starts_with("- Isolated") || trimmed.starts_with("- isolated") {
                task.isolated = true;
            } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                current_description.push_str(trimmed);
                current_description.push('\n');
            }
        }

        if trimmed.starts_with("Verify:") || trimmed.starts_with("verify:") {
            verify_cmd = Some(trimmed.trim_start_matches("Verify:").trim_start_matches("verify:").trim().to_string());
        }
    }

    // Save last task
    if let Some(mut task) = current_task.take() {
        task.description = current_description.trim().to_string();
        tasks.push(task);
    }

    let mut plan = SpecPlan::new(source, tasks);
    plan.verification_command = verify_cmd;
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_spec_plan() {
        let spec = r#"
# Spec: User Auth

## Task 1: Create user model
Create user data model with email validation.
- Files: src/models/user.py
- Criteria: Email must be valid format

## Task 2: Add register endpoint
Add POST /register endpoint.
- Files: src/routes/auth.py
- Depends: 1
- Criteria: Returns 201, hashes password

## Task 3: Add login endpoint
Add POST /login with JWT.
- Files: src/routes/auth.py
- Depends: 1, 2
- Criteria: Returns JWT token

Verify: pytest tests/
"#;
        let plan = parse_spec_plan(spec);
        assert_eq!(plan.tasks.len(), 3);
        assert_eq!(plan.tasks[0].title, "Create user model");
        assert_eq!(plan.tasks[1].dependencies, vec![1]);
        assert_eq!(plan.tasks[2].dependencies, vec![1, 2]);
        assert!(plan.verification_command.as_deref() == Some("pytest tests/"));
    }

    #[test]
    fn test_execution_order() {
        let plan = SpecPlan::new("test", vec![
            SpecTask { id: 1, title: "A".into(), description: "".into(), dependencies: vec![], files_to_create: vec![], files_to_modify: vec![], acceptance_criteria: vec![], status: TaskStatus::Pending, isolated: false },
            SpecTask { id: 2, title: "B".into(), description: "".into(), dependencies: vec![1], files_to_create: vec![], files_to_modify: vec![], acceptance_criteria: vec![], status: TaskStatus::Pending, isolated: false },
            SpecTask { id: 3, title: "C".into(), description: "".into(), dependencies: vec![1], files_to_create: vec![], files_to_modify: vec![], acceptance_criteria: vec![], status: TaskStatus::Pending, isolated: false },
            SpecTask { id: 4, title: "D".into(), description: "".into(), dependencies: vec![2, 3], files_to_create: vec![], files_to_modify: vec![], acceptance_criteria: vec![], status: TaskStatus::Pending, isolated: false },
        ]);
        let order = plan.execution_order();
        assert_eq!(order.len(), 4);
        assert_eq!(order[0].id, 1, "Task 1 has no deps, goes first");
        assert_eq!(order[3].id, 4, "Task 4 depends on 2+3, goes last");
    }

    #[test]
    fn test_parallelizable_groups() {
        let plan = SpecPlan::new("test", vec![
            SpecTask { id: 1, title: "A".into(), description: "".into(), dependencies: vec![], files_to_create: vec![], files_to_modify: vec![], acceptance_criteria: vec![], status: TaskStatus::Pending, isolated: false },
            SpecTask { id: 2, title: "B".into(), description: "".into(), dependencies: vec![1], files_to_create: vec![], files_to_modify: vec![], acceptance_criteria: vec![], status: TaskStatus::Pending, isolated: false },
            SpecTask { id: 3, title: "C".into(), description: "".into(), dependencies: vec![1], files_to_create: vec![], files_to_modify: vec![], acceptance_criteria: vec![], status: TaskStatus::Pending, isolated: false },
        ]);
        let groups = plan.parallelizable_groups();
        assert_eq!(groups.len(), 2, "Should be 2 groups: [A] then [B, C]");
        assert_eq!(groups[0].len(), 1, "First group: just A");
        assert_eq!(groups[1].len(), 2, "Second group: B and C in parallel");
    }

    #[test]
    fn test_task_prompt_generation() {
        let plan = SpecPlan {
            spec_source: "test".into(),
            tasks: vec![SpecTask {
                id: 1, title: "Add auth".into(),
                description: "Add JWT authentication".into(),
                dependencies: vec![],
                files_to_create: vec!["auth.py".into()],
                files_to_modify: vec!["app.py".into()],
                acceptance_criteria: vec!["Returns JWT token".into(), "Validates password".into()],
                status: TaskStatus::Pending,
                isolated: false,
            }],
            verification_command: Some("pytest".into()),
            atomic_commits: true,
        };

        let prompt = plan.task_prompt(&plan.tasks[0]);
        assert!(prompt.contains("Add auth"));
        assert!(prompt.contains("auth.py"));
        assert!(prompt.contains("Returns JWT token"));
        assert!(prompt.contains("pytest"));
    }
}
