//! # Semantic Git Firewall
//!
//! Deep validation of Git operations against trust boundaries.
//! Generalizes repository-level privilege escalation guards into a semantic
//! validator over state transitions.
//!
//! Complexity: O(T + P) where T = tokenized command structure, P = path targets.
//! Protected-path lookup: O(1) average via HashSet.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Classification of threats detected by the firewall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreatClass {
    /// Attempt to modify git hooks (pre-commit, post-checkout, etc.)
    HookPlanting,
    /// Attempt to modify .git/ internals (objects, refs, HEAD, config)
    GitInternalsMutation,
    /// Dangerous git config injection (core.fsmonitor, core.hooksPath, etc.)
    ConfigInjection,
    /// Repository ambiguity attack (bare repo indicators, gitdir manipulation)
    RepoAmbiguity,
    /// Archive extraction that could overwrite .git/ contents (TOCTOU)
    ArchiveExtraction,
    /// Force push or destructive ref manipulation
    DestructiveRefMutation,
    /// Submodule URL injection (arbitrary code execution via submodule update)
    SubmoduleInjection,
    /// Worktree escape (operating outside sanctioned worktree boundaries)
    WorktreeEscape,
    /// Branch protection violation (direct push to protected branches)
    BranchProtectionViolation,
    /// Large binary commit (potential repo bloat attack)
    RepoBloat,
}

/// Firewall decision for a git operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirewallDecision {
    pub allowed: bool,
    pub threats: Vec<ThreatClass>,
    pub explanation: String,
}

impl FirewallDecision {
    pub fn allow() -> Self {
        Self {
            allowed: true,
            threats: Vec::new(),
            explanation: String::new(),
        }
    }

    pub fn deny(threat: ThreatClass, explanation: impl Into<String>) -> Self {
        Self {
            allowed: false,
            threats: vec![threat],
            explanation: explanation.into(),
        }
    }
}

/// The semantic git firewall. Validates operations against trust boundaries.
pub struct GitFirewall {
    /// Protected branches that cannot be directly modified.
    protected_branches: HashSet<String>,
    /// Protected path patterns within the repository.
    protected_paths: Vec<ProtectedPathRule>,
    /// Dangerous git config keys that require escalation.
    dangerous_config_keys: HashSet<String>,
    /// Dangerous git subcommands.
    dangerous_subcommands: HashSet<String>,
}

/// A rule protecting a specific path pattern.
#[derive(Debug, Clone)]
struct ProtectedPathRule {
    pattern: String,
    threat: ThreatClass,
    description: String,
}

impl GitFirewall {
    /// Create a firewall with default security rules.
    pub fn new() -> Self {
        let mut fw = Self {
            protected_branches: HashSet::new(),
            protected_paths: Vec::new(),
            dangerous_config_keys: HashSet::new(),
            dangerous_subcommands: HashSet::new(),
        };
        fw.install_default_rules();
        fw
    }

    /// Install the default set of security rules.
    fn install_default_rules(&mut self) {
        // Protected branches
        for branch in &["main", "master", "release", "production", "prod"] {
            self.protected_branches.insert(branch.to_string());
        }

        // Protected paths — git internals
        let git_paths = [
            (".git/hooks/", ThreatClass::HookPlanting, "git hook directory"),
            (".git/config", ThreatClass::GitInternalsMutation, "git config"),
            (".git/objects/", ThreatClass::GitInternalsMutation, "git objects"),
            (".git/refs/", ThreatClass::GitInternalsMutation, "git refs"),
            (".git/HEAD", ThreatClass::GitInternalsMutation, "git HEAD"),
            (".git/index", ThreatClass::GitInternalsMutation, "git index"),
            (".git/packed-refs", ThreatClass::GitInternalsMutation, "packed refs"),
            (".gitmodules", ThreatClass::SubmoduleInjection, "submodule config"),
            (".gitattributes", ThreatClass::ConfigInjection, "git attributes"),
        ];
        for (pattern, threat, desc) in git_paths {
            self.protected_paths.push(ProtectedPathRule {
                pattern: pattern.to_string(),
                threat,
                description: desc.to_string(),
            });
        }

        // Dangerous config keys (arbitrary code execution vectors)
        for key in &[
            "core.fsmonitor",
            "core.hooksPath",
            "core.sshCommand",
            "core.gitProxy",
            "core.pager",
            "diff.external",
            "merge.tool",
            "credential.helper",
            "filter.clean",
            "filter.smudge",
            "receive.denyCurrentBranch",
            "protocol.allow",
        ] {
            self.dangerous_config_keys.insert(key.to_string());
        }

        // Dangerous subcommands
        for cmd in &[
            "push --force",
            "push -f",
            "reset --hard",
            "clean -fd",
            "clean -fdx",
            "reflog expire",
            "gc --prune=now",
            "filter-branch",
            "rebase --root",
        ] {
            self.dangerous_subcommands.insert(cmd.to_string());
        }
    }

    /// Check a git command string for threats.
    /// Complexity: O(T + P) where T = tokens, P = path patterns.
    pub fn check_command(&self, command: &str) -> FirewallDecision {
        let tokens: Vec<&str> = command.split_whitespace().collect();
        if tokens.is_empty() {
            return FirewallDecision::allow();
        }

        // Must start with "git"
        let is_git = tokens[0] == "git" || tokens[0].ends_with("/git");
        if !is_git {
            return FirewallDecision::allow(); // Not a git command
        }

        let mut threats = Vec::new();

        // Check for dangerous subcommands
        let subcmd = tokens[1..].join(" ");
        for dangerous in &self.dangerous_subcommands {
            if subcmd.starts_with(dangerous) {
                threats.push(ThreatClass::DestructiveRefMutation);
                break;
            }
        }

        // Check for config injection
        if tokens.get(1) == Some(&"config") {
            for key in &self.dangerous_config_keys {
                if tokens.iter().any(|t| t == key) {
                    threats.push(ThreatClass::ConfigInjection);
                    break;
                }
            }
        }

        // Check for --config-env= inline config (TOCTOU vector)
        for token in &tokens {
            if token.starts_with("--config-env=") || token.starts_with("-c ") {
                threats.push(ThreatClass::ConfigInjection);
                break;
            }
            if *token == "-c" {
                // Check if next token is a dangerous key
                if let Some(next) = tokens.iter().position(|t| *t == "-c").and_then(|i| tokens.get(i + 1)) {
                    let key = next.split('=').next().unwrap_or("");
                    if self.dangerous_config_keys.contains(key) {
                        threats.push(ThreatClass::ConfigInjection);
                    }
                }
                break;
            }
        }

        // Check for submodule URL injection
        if tokens.get(1) == Some(&"submodule") {
            if tokens.iter().any(|t| t.contains("://") || t.starts_with("git@")) {
                threats.push(ThreatClass::SubmoduleInjection);
            }
        }

        // Check for push to protected branches
        if tokens.get(1) == Some(&"push") {
            for branch in &self.protected_branches {
                if tokens.iter().any(|t| t == branch || t.ends_with(&format!(":{}", branch))) {
                    threats.push(ThreatClass::BranchProtectionViolation);
                    break;
                }
            }
        }

        if threats.is_empty() {
            FirewallDecision::allow()
        } else {
            FirewallDecision {
                allowed: false,
                threats: threats.clone(),
                explanation: format!(
                    "Command '{}' triggers security rules: {:?}",
                    command, threats
                ),
            }
        }
    }

    /// Check a file path against protected patterns.
    /// Returns the threat class if the path is protected.
    pub fn check_path(&self, path: &str) -> Option<ThreatClass> {
        for rule in &self.protected_paths {
            if path.starts_with(&rule.pattern)
                || path == rule.pattern.trim_end_matches('/')
            {
                return Some(rule.threat.clone());
            }
        }
        None
    }

    /// Check if a branch name is valid for workspace creation.
    pub fn check_workspace_name(&self, name: &str) -> Option<ThreatClass> {
        // Reject names that could escape or confuse git
        if name.contains("..") || name.contains('/') && name.starts_with('.') {
            return Some(ThreatClass::RepoAmbiguity);
        }
        if name.contains('\0') || name.contains('~') || name.contains('^') {
            return Some(ThreatClass::RepoAmbiguity);
        }
        None
    }

    /// Check if mutating a branch is allowed.
    pub fn check_branch_mutation(&self, branch: &str) -> Option<ThreatClass> {
        if self.protected_branches.contains(branch) {
            Some(ThreatClass::BranchProtectionViolation)
        } else {
            None
        }
    }

    /// Check a set of file mutations for archive extraction attacks.
    pub fn check_file_mutations(&self, paths: &[String]) -> FirewallDecision {
        let mut threats = Vec::new();

        for path in paths {
            if let Some(threat) = self.check_path(path) {
                threats.push(threat);
            }
        }

        if threats.is_empty() {
            FirewallDecision::allow()
        } else {
            FirewallDecision {
                allowed: false,
                threats: threats.clone(),
                explanation: format!(
                    "{} protected path(s) would be modified: {:?}",
                    threats.len(),
                    threats,
                ),
            }
        }
    }

    /// Add a custom protected branch.
    pub fn protect_branch(&mut self, branch: impl Into<String>) {
        self.protected_branches.insert(branch.into());
    }

    /// Add a custom protected path rule.
    pub fn protect_path(&mut self, pattern: impl Into<String>, threat: ThreatClass, description: impl Into<String>) {
        self.protected_paths.push(ProtectedPathRule {
            pattern: pattern.into(),
            threat,
            description: description.into(),
        });
    }
}

impl Default for GitFirewall {
    fn default() -> Self {
        Self::new()
    }
}
