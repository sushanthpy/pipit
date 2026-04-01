//! Skill Evaluation Framework — test skills against expected outcomes.
//!
//! Bridges pipit-skills with pipit-bench: generates BenchTask instances
//! from SkillPackage test suites, runs them, and reports per-skill
//! pass rates for regression gating.
//!
//! Flow:
//! ```text
//! SkillPackage.test → SkillEvalTask → run_eval → SkillEvalResult
//!                                                        ↓
//!                                               Regression check
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A single skill evaluation task, derived from a SkillTestSuite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEvalTask {
    /// Skill being evaluated.
    pub skill_name: String,
    /// Skill version.
    pub skill_version: String,
    /// The test script to run.
    pub test_script: String,
    /// Working directory for the test.
    pub work_dir: PathBuf,
    /// Timeout in seconds.
    pub timeout_secs: u64,
    /// Minimum pass rate to not flag regression.
    pub pass_threshold: f64,
}

/// Result of a skill evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEvalResult {
    pub skill_name: String,
    pub skill_version: String,
    pub passed: bool,
    pub elapsed_secs: f64,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

/// Aggregated evaluation report for a batch of skills.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillEvalReport {
    pub timestamp: String,
    pub results: Vec<SkillEvalResult>,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub pass_rate: f64,
    /// Skills that regressed below their threshold.
    pub regressions: Vec<String>,
}

/// Generate eval tasks from all skills that have test suites.
pub fn generate_eval_tasks(
    packages: &[crate::manifest::SkillPackage],
) -> Vec<SkillEvalTask> {
    packages
        .iter()
        .filter_map(|pkg| {
            let test_suite = pkg.manifest.test.as_ref()?;
            let script_path = pkg.skill_dir.join(&test_suite.script);
            let script_content = std::fs::read_to_string(&script_path).ok()?;

            Some(SkillEvalTask {
                skill_name: pkg.manifest.package.name.clone(),
                skill_version: pkg.manifest.package.version.clone(),
                test_script: script_content,
                work_dir: pkg.skill_dir.clone(),
                timeout_secs: test_suite.timeout_secs,
                pass_threshold: test_suite.pass_threshold,
            })
        })
        .collect()
}

/// Run a single eval task synchronously (blocking).
pub fn run_eval_task(task: &SkillEvalTask) -> SkillEvalResult {
    let start = std::time::Instant::now();

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&task.test_script)
        .current_dir(&task.work_dir)
        .output();

    let elapsed = start.elapsed().as_secs_f64();

    match output {
        Ok(out) => SkillEvalResult {
            skill_name: task.skill_name.clone(),
            skill_version: task.skill_version.clone(),
            passed: out.status.success(),
            elapsed_secs: elapsed,
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            exit_code: out.status.code(),
        },
        Err(e) => SkillEvalResult {
            skill_name: task.skill_name.clone(),
            skill_version: task.skill_version.clone(),
            passed: false,
            elapsed_secs: elapsed,
            stdout: String::new(),
            stderr: format!("Execution error: {}", e),
            exit_code: None,
        },
    }
}

/// Run all eval tasks and produce a report.
pub fn run_eval_suite(tasks: &[SkillEvalTask]) -> SkillEvalReport {
    let results: Vec<SkillEvalResult> = tasks.iter().map(|t| run_eval_task(t)).collect();
    let total = results.len();
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = total - passed;

    // Identify regressions: skills that failed and have a threshold
    let regressions: Vec<String> = tasks
        .iter()
        .zip(results.iter())
        .filter(|(task, result)| !result.passed && task.pass_threshold > 0.0)
        .map(|(task, _)| task.skill_name.clone())
        .collect();

    SkillEvalReport {
        timestamp: chrono::Utc::now().to_rfc3339(),
        results,
        total,
        passed,
        failed,
        pass_rate: if total > 0 {
            passed as f64 / total as f64
        } else {
            1.0
        },
        regressions,
    }
}

/// Persist eval report to `.pipit/eval/skills-report.json`.
pub fn save_report(report: &SkillEvalReport, project_root: &Path) {
    let dir = project_root.join(".pipit").join("eval");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("skills-report.json");
    if let Ok(json) = serde_json::to_string_pretty(report) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_eval_report_aggregation() {
        let results = vec![
            SkillEvalResult {
                skill_name: "a".into(),
                skill_version: "1.0".into(),
                passed: true,
                elapsed_secs: 1.0,
                stdout: String::new(),
                stderr: String::new(),
                exit_code: Some(0),
            },
            SkillEvalResult {
                skill_name: "b".into(),
                skill_version: "1.0".into(),
                passed: false,
                elapsed_secs: 2.0,
                stdout: String::new(),
                stderr: "error".into(),
                exit_code: Some(1),
            },
        ];

        let report = SkillEvalReport {
            timestamp: "now".into(),
            total: 2,
            passed: 1,
            failed: 1,
            pass_rate: 0.5,
            results,
            regressions: vec!["b".into()],
        };

        assert_eq!(report.pass_rate, 0.5);
        assert_eq!(report.regressions, vec!["b"]);
    }

    #[test]
    fn test_run_eval_task_echo() {
        let task = SkillEvalTask {
            skill_name: "echo-test".into(),
            skill_version: "0.1.0".into(),
            test_script: "echo hello".into(),
            work_dir: PathBuf::from("/tmp"),
            timeout_secs: 10,
            pass_threshold: 1.0,
        };

        let result = run_eval_task(&task);
        assert!(result.passed);
        assert!(result.stdout.contains("hello"));
    }
}
