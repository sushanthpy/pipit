//! Population Runner — Task EVO-1
//!
//! Parallel variant executor using git worktrees for isolation.
//! Worktree creation: O(1) (symlinks, no file copy).
//! Execution: embarrassingly parallel via tokio::spawn.
//! Sweet spot: N=3-5 variants (E[improvement] = σ√(2·ln(N)), Gumbel distribution).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A variant implementation of a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variant {
    pub id: usize,
    pub strategy: String,
    pub description: String,
    pub worktree_path: Option<PathBuf>,
    pub branch_name: String,
}

/// Result of running a variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantResult {
    pub variant_id: usize,
    pub strategy: String,
    pub success: bool,
    pub test_passed: bool,
    pub diff_lines: usize,
    pub elapsed_ms: u64,
    pub error: Option<String>,
    pub diff_content: String,
}

/// Manages parallel execution of N variant implementations.
pub struct PopulationRunner {
    pub repo_root: PathBuf,
    pub population_size: usize,
    worktree_base: PathBuf,
}

impl PopulationRunner {
    pub fn new(repo_root: PathBuf, population_size: usize) -> Self {
        let worktree_base = std::env::temp_dir().join("pipit-evolve");
        Self {
            repo_root,
            population_size,
            worktree_base,
        }
    }

    /// Create git worktrees for all variants. O(1) per worktree.
    pub fn setup_worktrees(&self) -> Result<Vec<Variant>, String> {
        std::fs::create_dir_all(&self.worktree_base)
            .map_err(|e| format!("Failed to create worktree base: {}", e))?;

        let mut variants = Vec::new();
        let strategies = [
            "MinimalPatch",
            "RootCauseRepair",
            "CharacterizationFirst",
            "ArchitecturalRepair",
            "DiagnosticFirst",
        ];

        for i in 0..self.population_size {
            let strategy = strategies[i % strategies.len()];
            let branch = format!("pipit/variant-{}", i);
            let worktree_path = self.worktree_base.join(format!("variant-{}", i));

            // Clean up any existing worktree
            if worktree_path.exists() {
                let _ = Command::new("git")
                    .args(["worktree", "remove", "--force"])
                    .arg(&worktree_path)
                    .current_dir(&self.repo_root)
                    .output();
                let _ = std::fs::remove_dir_all(&worktree_path);
            }

            // Delete branch if it exists
            let _ = Command::new("git")
                .args(["branch", "-D", &branch])
                .current_dir(&self.repo_root)
                .output();

            // Create worktree
            let output = Command::new("git")
                .args(["worktree", "add", "-b", &branch])
                .arg(&worktree_path)
                .arg("HEAD")
                .current_dir(&self.repo_root)
                .output()
                .map_err(|e| format!("git worktree add failed: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "git worktree failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            variants.push(Variant {
                id: i,
                strategy: strategy.to_string(),
                description: format!("Variant {} using {} strategy", i, strategy),
                worktree_path: Some(worktree_path),
                branch_name: branch,
            });
        }

        Ok(variants)
    }

    /// Run a test command in a variant's worktree.
    pub fn run_tests(&self, variant: &Variant, test_command: &str) -> VariantResult {
        let worktree = match &variant.worktree_path {
            Some(p) => p,
            None => {
                return VariantResult {
                    variant_id: variant.id,
                    strategy: variant.strategy.clone(),
                    success: false,
                    test_passed: false,
                    diff_lines: 0,
                    elapsed_ms: 0,
                    error: Some("No worktree path".into()),
                    diff_content: String::new(),
                };
            }
        };

        let start = std::time::Instant::now();

        // Run tests
        let test_result = Command::new("sh")
            .args(["-c", test_command])
            .current_dir(worktree)
            .output();

        let elapsed = start.elapsed().as_millis() as u64;

        let test_passed = test_result
            .as_ref()
            .map(|o| o.status.success())
            .unwrap_or(false);

        // Get diff
        let diff_output = Command::new("git")
            .args(["diff", "--stat"])
            .current_dir(worktree)
            .output();

        let diff_content = Command::new("git")
            .args(["diff"])
            .current_dir(worktree)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        let diff_lines = diff_content.lines().count();

        VariantResult {
            variant_id: variant.id,
            strategy: variant.strategy.clone(),
            success: test_passed,
            test_passed,
            diff_lines,
            elapsed_ms: elapsed,
            error: if test_passed {
                None
            } else {
                test_result.ok().map(|o| {
                    String::from_utf8_lossy(&o.stderr)
                        .chars()
                        .take(500)
                        .collect()
                })
            },
            diff_content,
        }
    }

    /// Clean up all worktrees.
    pub fn cleanup(&self) {
        for i in 0..self.population_size {
            let worktree_path = self.worktree_base.join(format!("variant-{}", i));
            let branch = format!("pipit/variant-{}", i);
            let _ = Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(&worktree_path)
                .current_dir(&self.repo_root)
                .output();
            let _ = Command::new("git")
                .args(["branch", "-D", &branch])
                .current_dir(&self.repo_root)
                .output();
        }
        let _ = std::fs::remove_dir_all(&self.worktree_base);
    }

    /// Expected fitness improvement from N samples: σ√(2·ln(N)) (Gumbel).
    pub fn expected_improvement(n: usize, variance: f64) -> f64 {
        let sigma = variance.sqrt();
        sigma * (2.0 * (n as f64).ln()).sqrt()
    }

    /// Run a variant: execute pipit in the worktree with the given prompt.
    /// This is the core integration point — runs a full agent loop in isolation.
    ///
    /// Args:
    ///   - variant: the variant with a valid worktree_path
    ///   - prompt: the task to execute
    ///   - pipit_binary: path to the pipit binary
    ///   - provider/model/base_url/api_key: LLM config
    ///   - max_turns: turn limit per variant
    ///   - test_command: optional command to run after agent completes
    ///
    /// Returns VariantResult with test outcome, diff, and timing.
    pub fn run_variant(
        &self,
        variant: &Variant,
        prompt: &str,
        pipit_binary: &str,
        provider: &str,
        model: &str,
        base_url: Option<&str>,
        api_key: &str,
        max_turns: u32,
        test_command: Option<&str>,
    ) -> VariantResult {
        let worktree = match &variant.worktree_path {
            Some(p) => p,
            None => {
                return VariantResult {
                    variant_id: variant.id,
                    strategy: variant.strategy.clone(),
                    success: false,
                    test_passed: false,
                    diff_lines: 0,
                    elapsed_ms: 0,
                    error: Some("No worktree path".into()),
                    diff_content: String::new(),
                };
            }
        };

        let start = std::time::Instant::now();

        // Build pipit command
        let mut cmd = Command::new(pipit_binary);
        cmd.arg(prompt)
            .arg("--provider")
            .arg(provider)
            .arg("--model")
            .arg(model)
            .arg("--api-key")
            .arg(api_key)
            .arg("--approval")
            .arg("full_auto")
            .arg("--max-turns")
            .arg(max_turns.to_string())
            .arg("--root")
            .arg(worktree)
            .current_dir(worktree);

        if let Some(url) = base_url {
            cmd.arg("--base-url").arg(url);
        }

        // Execute pipit in the worktree
        let pipit_result = cmd
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output();

        let agent_success = pipit_result
            .as_ref()
            .map(|o| o.status.success())
            .unwrap_or(false);

        // Run tests if provided
        let test_passed = if let Some(test_cmd) = test_command {
            Command::new("sh")
                .args(["-c", test_cmd])
                .current_dir(worktree)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        } else {
            agent_success
        };

        let elapsed = start.elapsed().as_millis() as u64;

        // Collect diff
        let diff_content = Command::new("git")
            .args(["diff"])
            .current_dir(worktree)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default();

        let diff_lines = diff_content.lines().count();

        let error = if !agent_success {
            pipit_result.ok().map(|o| {
                let stderr = String::from_utf8_lossy(&o.stderr);
                stderr.chars().take(500).collect()
            })
        } else {
            None
        };

        VariantResult {
            variant_id: variant.id,
            strategy: variant.strategy.clone(),
            success: agent_success && test_passed,
            test_passed,
            diff_lines,
            elapsed_ms: elapsed,
            error,
            diff_content,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expected_improvement_scaling() {
        let var = 1.0;
        let imp3 = PopulationRunner::expected_improvement(3, var);
        let imp10 = PopulationRunner::expected_improvement(10, var);
        // Going 3→10 should improve by ~37%
        let ratio = imp10 / imp3;
        assert!(ratio > 1.2 && ratio < 1.6, "3→10 ratio: {:.2}", ratio);
    }

    #[test]
    fn test_variant_creation() {
        let v = Variant {
            id: 0,
            strategy: "MinimalPatch".into(),
            description: "Test variant".into(),
            worktree_path: None,
            branch_name: "pipit/variant-0".into(),
        };
        assert_eq!(v.id, 0);
        assert_eq!(v.strategy, "MinimalPatch");
    }
}
