//! Self-Healing CI — auto-fix pipeline failures (INTEL-5).
//!
//! Webhook handler for CI/CD systems (GitHub Actions, GitLab CI, etc.).
//! When a pipeline fails:
//! 1. Fetch the failure log
//! 2. Identify the failing step (build, lint, test, deploy)
//! 3. Clone the branch into a worktree
//! 4. Run the agent with a fix prompt
//! 5. Push a fix commit
//! 6. Comment on the PR
//!
//! Endpoint: POST /api/ci-fix
//! GitHub Action webhook: on check_run.completed with conclusion == 'failure'

use serde::{Deserialize, Serialize};

/// Incoming CI failure webhook payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiFailurePayload {
    /// Repository URL (git clone target).
    pub repo_url: String,
    /// Branch to fix.
    pub branch: String,
    /// PR number (if applicable).
    pub pr_number: Option<u64>,
    /// The CI log output.
    pub failure_log: String,
    /// Which CI step failed.
    pub failed_step: Option<String>,
    /// Commit SHA that triggered the failure.
    pub commit_sha: String,
}

/// Result of a CI fix attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiFixResult {
    pub success: bool,
    pub fix_commit_sha: Option<String>,
    pub fix_description: String,
    pub files_changed: Vec<String>,
    pub error: Option<String>,
}

/// Classify the CI failure type from the log.
pub fn classify_failure(log: &str) -> FailureKind {
    let lower = log.to_lowercase();
    if lower.contains("cargo build")
        || lower.contains("tsc")
        || lower.contains("compile error")
        || lower.contains("build failed")
        || lower.contains("cannot find")
    {
        FailureKind::Build
    } else if lower.contains("cargo test")
        || lower.contains("jest")
        || lower.contains("pytest")
        || lower.contains("test failed")
        || lower.contains("assertion failed")
    {
        FailureKind::Test
    } else if lower.contains("clippy")
        || lower.contains("eslint")
        || lower.contains("pylint")
        || lower.contains("lint")
        || lower.contains("warning")
    {
        FailureKind::Lint
    } else if lower.contains("deploy") || lower.contains("docker") || lower.contains("kubectl") {
        FailureKind::Deploy
    } else if lower.contains("type") || lower.contains("pyright") || lower.contains("mypy") {
        FailureKind::TypeCheck
    } else {
        FailureKind::Unknown
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    Build,
    Test,
    Lint,
    TypeCheck,
    Deploy,
    Unknown,
}

impl FailureKind {
    /// Generate the agent prompt for fixing this type of failure.
    pub fn fix_prompt(&self, log: &str) -> String {
        let truncated = if log.len() > 4000 {
            &log[log.len() - 4000..]
        } else {
            log
        };
        match self {
            Self::Build => format!(
                "The CI build is failing. Fix the compilation errors.\n\n\
                 Build log (last 4000 chars):\n```\n{}\n```\n\n\
                 Steps:\n1. Read the error messages\n2. Fix each error\n3. Verify with a build command\n\
                 Make minimal, surgical fixes only.",
                truncated
            ),
            Self::Test => format!(
                "CI tests are failing. Fix the test failures.\n\n\
                 Test log (last 4000 chars):\n```\n{}\n```\n\n\
                 Steps:\n1. Identify which tests failed and why\n2. Fix the implementation (not the tests, unless they're wrong)\n\
                 3. Re-run failing tests to verify",
                truncated
            ),
            Self::Lint => format!(
                "CI linting is failing. Fix the lint errors.\n\n\
                 Lint log (last 4000 chars):\n```\n{}\n```\n\n\
                 Fix all lint violations. Do not disable rules.",
                truncated
            ),
            Self::TypeCheck => format!(
                "CI type checking is failing. Fix the type errors.\n\n\
                 Type check log (last 4000 chars):\n```\n{}\n```",
                truncated
            ),
            Self::Deploy => format!(
                "CI deployment is failing. Analyze the deployment error.\n\n\
                 Deploy log (last 4000 chars):\n```\n{}\n```\n\n\
                 Fix any configuration or code issues causing the deploy failure.",
                truncated
            ),
            Self::Unknown => format!(
                "CI pipeline is failing. Analyze and fix.\n\n\
                 Log (last 4000 chars):\n```\n{}\n```",
                truncated
            ),
        }
    }
}

/// Create a git worktree for the fix, apply fix, commit, and push.
pub fn prepare_fix_worktree(
    repo_root: &std::path::Path,
    branch: &str,
) -> Result<std::path::PathBuf, String> {
    let worktree_dir = repo_root
        .join(".pipit")
        .join("ci-fix")
        .join(branch.replace('/', "-"));
    let _ = std::fs::create_dir_all(worktree_dir.parent().unwrap_or(repo_root));

    let output = std::process::Command::new("git")
        .args([
            "worktree",
            "add",
            worktree_dir.to_str().unwrap_or("."),
            branch,
        ])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("git worktree add failed: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    Ok(worktree_dir)
}

/// Commit and push the fix.
pub fn commit_and_push_fix(
    worktree: &std::path::Path,
    description: &str,
) -> Result<String, String> {
    // Stage all changes
    let _ = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(worktree)
        .output();

    // Commit
    let msg = format!("fix(ci): {}", description);
    let output = std::process::Command::new("git")
        .args(["commit", "-m", &msg])
        .current_dir(worktree)
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    // Get commit SHA
    let sha_output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(worktree)
        .output()
        .map_err(|e| e.to_string())?;

    let sha = String::from_utf8_lossy(&sha_output.stdout)
        .trim()
        .to_string();

    // Push
    let push = std::process::Command::new("git")
        .args(["push"])
        .current_dir(worktree)
        .output()
        .map_err(|e| e.to_string())?;

    if !push.status.success() {
        return Err(format!(
            "Push failed: {}",
            String::from_utf8_lossy(&push.stderr)
        ));
    }

    Ok(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_build_failure() {
        assert_eq!(
            classify_failure("error[E0308]: mismatched types\ncargo build failed"),
            FailureKind::Build
        );
    }

    #[test]
    fn test_classify_test_failure() {
        assert_eq!(
            classify_failure("test result: FAILED. 3 passed; 1 failed\ncargo test"),
            FailureKind::Test
        );
    }

    #[test]
    fn test_classify_lint_failure() {
        assert_eq!(
            classify_failure("eslint found 5 errors\nlint step failed"),
            FailureKind::Lint
        );
    }
}
