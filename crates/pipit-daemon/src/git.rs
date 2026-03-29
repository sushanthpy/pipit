//! Pre-mutation branch creation with proof-linked commit messages.
//!
//! Before any mutating tool call, creates `pipit/{task_id_short}` branch.
//! Protected path enforcement via compiled glob patterns.

use anyhow::{anyhow, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::Path;
use std::process::Command;
use tracing;

// ---------------------------------------------------------------------------
// Git safety operations
// ---------------------------------------------------------------------------

pub struct GitSafety;

impl GitSafety {
    /// Create a task-specific branch: `pipit/{task_id_short}`.
    /// Returns the branch name on success.
    pub fn create_task_branch(
        project_root: &Path,
        branch_prefix: &str,
        task_id: &str,
    ) -> Result<String> {
        // Verify git repo
        if !project_root.join(".git").exists() {
            return Err(anyhow!(
                "not a git repository: {}",
                project_root.display()
            ));
        }

        // Use first 8 chars of task_id for branch name
        let short_id = &task_id[..task_id.len().min(8)];
        let branch_name = format!("{}{}", branch_prefix, short_id);

        let output = Command::new("git")
            .args(["checkout", "-b", &branch_name])
            .current_dir(project_root)
            .output()
            .map_err(|e| anyhow!("git checkout -b failed: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow!("git checkout -b '{}' failed: {}", branch_name, stderr));
        }

        tracing::info!(
            branch = %branch_name,
            root = %project_root.display(),
            "created task branch"
        );

        Ok(branch_name)
    }

    /// Auto-commit all changes with a pipit-attributed commit message.
    pub fn auto_commit(project_root: &Path, summary: &str) -> Result<()> {
        // Stage all changes
        let add_output = Command::new("git")
            .args(["add", "-A"])
            .current_dir(project_root)
            .output()
            .map_err(|e| anyhow!("git add failed: {}", e))?;

        if !add_output.status.success() {
            return Err(anyhow!(
                "git add failed: {}",
                String::from_utf8_lossy(&add_output.stderr)
            ));
        }

        // Check if there are staged changes
        let diff_output = Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .current_dir(project_root)
            .output()?;

        if diff_output.status.success() {
            // No changes to commit
            tracing::info!("no changes to commit");
            return Ok(());
        }

        // Commit with pipit attribution
        let commit_msg = format!("[pipit] {}", summary);
        let output = Command::new("git")
            .args(["commit", "-m", &commit_msg])
            .env("GIT_COMMITTER_NAME", "Pipit")
            .env("GIT_COMMITTER_EMAIL", "noreply@pipit.dev")
            .current_dir(project_root)
            .output()
            .map_err(|e| anyhow!("git commit failed: {}", e))?;

        if !output.status.success() {
            return Err(anyhow!(
                "git commit failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        tracing::info!(
            root = %project_root.display(),
            "auto-committed changes"
        );

        Ok(())
    }

    /// Get current branch name.
    pub fn current_branch(project_root: &Path) -> Result<String> {
        let output = Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(project_root)
            .output()
            .map_err(|e| anyhow!("git rev-parse failed: {}", e))?;

        if !output.status.success() {
            return Err(anyhow!(
                "git rev-parse failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Switch back to the original branch after task completion.
    pub fn checkout_branch(project_root: &Path, branch: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["checkout", branch])
            .current_dir(project_root)
            .output()
            .map_err(|e| anyhow!("git checkout failed: {}", e))?;

        if !output.status.success() {
            return Err(anyhow!(
                "git checkout '{}' failed: {}",
                branch,
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Protected path enforcement
// ---------------------------------------------------------------------------

/// Compiled glob set for protected path matching.
/// Each pattern is compiled once at config load time.
pub struct ProtectedPaths {
    globset: GlobSet,
    patterns: Vec<String>,
}

impl ProtectedPaths {
    /// Compile glob patterns from config.
    pub fn compile(patterns: &[String]) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            builder.add(
                Glob::new(pattern)
                    .map_err(|e| anyhow!("invalid protected path pattern '{}': {}", pattern, e))?,
            );
        }
        let globset = builder
            .build()
            .map_err(|e| anyhow!("failed to build protected path globset: {}", e))?;

        Ok(Self {
            globset,
            patterns: patterns.to_vec(),
        })
    }

    /// Check if a path is protected. Returns the matching pattern if blocked.
    pub fn check(&self, path: &str) -> Option<&str> {
        if self.globset.is_match(path) {
            // Find which pattern matched (for error messages)
            for pattern in &self.patterns {
                if let Ok(glob) = Glob::new(pattern) {
                    if glob.compile_matcher().is_match(path) {
                        return Some(pattern);
                    }
                }
            }
            Some("(unknown pattern)")
        } else {
            None
        }
    }

    /// Returns true if no patterns are configured.
    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protected_paths() {
        let paths = ProtectedPaths::compile(&[
            ".env".to_string(),
            "*.pem".to_string(),
            "docker-compose.prod.yml".to_string(),
            "secrets/**".to_string(),
        ])
        .unwrap();

        assert!(paths.check(".env").is_some());
        assert!(paths.check("server.pem").is_some());
        assert!(paths.check("docker-compose.prod.yml").is_some());
        assert!(paths.check("secrets/api-key.txt").is_some());

        assert!(paths.check("src/main.rs").is_none());
        assert!(paths.check("docker-compose.yml").is_none());
        assert!(paths.check(".env.example").is_none());
    }

    #[test]
    fn test_empty_protected_paths() {
        let paths = ProtectedPaths::compile(&[]).unwrap();
        assert!(paths.is_empty());
        assert!(paths.check("anything.txt").is_none());
    }
}
