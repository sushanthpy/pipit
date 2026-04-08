//! Speculative Execution — parallel strategy racing (INTEL-3).
//!
//! For complex tasks, fork into N git worktrees and run different strategies
//! in parallel using different models. First strategy to pass verification wins.
//!
//! Inspired by CPU speculative execution: E[T_spec] = E[min(T₁..Tₖ)] ≤ E[T₁]

use crate::worktree::{WorktreeHandle, WorktreeManager};
use std::path::Path;

/// A speculative execution strategy.
#[derive(Debug, Clone)]
pub struct Strategy {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub prompt_prefix: String,
}

/// Result of a speculative execution.
#[derive(Debug)]
pub struct SpeculativeResult {
    pub strategy: Strategy,
    pub worktree_path: std::path::PathBuf,
    pub passed_verification: bool,
    pub elapsed_secs: f64,
    pub cost_usd: f64,
    pub turns: u32,
}

/// Default strategies for common task types.
pub fn default_strategies() -> Vec<Strategy> {
    vec![
        Strategy {
            name: "MinimalPatch".to_string(),
            description: "Smallest possible fix, surgical change".to_string(),
            model: None, // Use current model (typically fast/cheap)
            prompt_prefix: "Apply the MINIMAL, surgical fix. Change as few lines as possible. \
                            Do not refactor or improve code beyond what's needed."
                .to_string(),
        },
        Strategy {
            name: "RootCause".to_string(),
            description: "Deep analysis and root cause fix".to_string(),
            model: None,
            prompt_prefix:
                "Analyze the ROOT CAUSE deeply. Read all relevant files, understand the \
                            architecture, then fix the underlying issue — not just the symptom."
                    .to_string(),
        },
        Strategy {
            name: "TestFirst".to_string(),
            description: "Write tests first, then fix to pass".to_string(),
            model: None,
            prompt_prefix: "Use TDD: first write a failing test that reproduces the issue, \
                            then write the minimal code to make it pass."
                .to_string(),
        },
    ]
}

/// Create isolated worktrees for each strategy.
pub fn prepare_worktrees(
    project_root: &Path,
    strategies: &[Strategy],
) -> Result<Vec<(Strategy, WorktreeHandle)>, String> {
    let manager =
        WorktreeManager::new(project_root).map_err(|e| format!("Worktree setup failed: {}", e))?;

    let mut worktrees = Vec::new();
    for strategy in strategies {
        let handle = manager
            .create(Some(&strategy.name))
            .map_err(|e| format!("Failed to create worktree for '{}': {}", strategy.name, e))?;
        worktrees.push((strategy.clone(), handle));
    }
    Ok(worktrees)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_strategies() {
        let strategies = default_strategies();
        assert_eq!(strategies.len(), 3);
        assert_eq!(strategies[0].name, "MinimalPatch");
    }
}
