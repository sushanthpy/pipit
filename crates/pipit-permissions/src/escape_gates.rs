//! # Escape Gates — Bare-Repo Planting & Settings-Write Closure
//!
//! Unconditionally prevents sandboxed commands from altering paths that are
//! auto-loaded with elevated authority on the next turn:
//!
//! - `.pipit/**` (config, hooks, skills, agents)
//! - `.git/hooks/*`, `.git/config`, `.git/objects/`, `.git/refs/` (bare-repo planting)
//!
//! Also provides post-command scrubbing of files that were planted during
//! command execution (files that didn't exist pre-command but exist post-command
//! in the dangerous set).
//!
//! ## Threat Model
//!
//! An attacker model (or prompt-injected LLM) plants files such that git's
//! `is_git_directory()` treats the cwd as a bare repo. If `core.fsmonitor` is
//! set in the planted `config`, every subsequent git command executes the
//! fsmonitor payload. This crate prevents the planting.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// The set of paths that are unconditionally denied for writes.
/// These paths are auto-loaded by pipit or git with elevated authority.
#[derive(Debug, Clone)]
pub struct DangerousWriteSet {
    /// Paths relative to project root that cannot be written.
    deny_write: HashSet<PathBuf>,
    /// Pre-existing paths sampled at session start.
    /// Files in `deny_write` that existed before the session are left alone
    /// (they were there before the agent, not planted by it).
    pre_existing: HashSet<PathBuf>,
}

/// Result of checking a write against the escape gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteCheck {
    /// Write is allowed.
    Allowed,
    /// Write is denied with the given reason.
    Denied(String),
}

impl DangerousWriteSet {
    /// Build the dangerous write set for a project.
    ///
    /// Samples pre-existing files in the dangerous set at construction time.
    /// Cost: O(|dangerous_paths|) stat calls.
    pub fn new(project_root: &Path) -> Self {
        let deny_patterns = Self::dangerous_paths();
        let mut deny_write = HashSet::new();
        let mut pre_existing = HashSet::new();

        for pattern in &deny_patterns {
            let abs_path = project_root.join(pattern);
            deny_write.insert(PathBuf::from(pattern));
            if abs_path.exists() {
                pre_existing.insert(PathBuf::from(pattern));
            }
        }

        Self {
            deny_write,
            pre_existing,
        }
    }

    /// The canonical set of auto-loaded dangerous paths.
    fn dangerous_paths() -> Vec<PathBuf> {
        vec![
            // Pipit auto-loaded config and code
            ".pipit/config.toml".into(),
            ".pipit/sandbox.toml".into(),
            ".pipit/hooks".into(),
            ".pipit/skills".into(),
            ".pipit/agents".into(),
            ".pipit/settings.toml".into(),
            ".pipit/settings.local.toml".into(),
            // Bare-repo planting targets
            "HEAD".into(),
            "objects".into(),
            "refs".into(),
            "hooks".into(),
            "config".into(), // git bare-repo config
            // Git hooks directory
            ".git/hooks".into(),
            ".git/config".into(),
            ".git/objects".into(),
            ".git/refs".into(),
            // Git submodule attack surface
            ".gitmodules".into(),
        ]
    }

    /// Check if a write to the given path (relative to project root) is allowed.
    ///
    /// Cost: O(1) HashSet lookup.
    pub fn check_write(&self, relative_path: &Path) -> WriteCheck {
        // Check exact match
        if self.deny_write.contains(relative_path) {
            return WriteCheck::Denied(format!(
                "Write to '{}' is unconditionally denied — auto-loaded config path",
                relative_path.display()
            ));
        }

        // Check if path is under a denied directory
        for denied in &self.deny_write {
            if relative_path.starts_with(denied) {
                return WriteCheck::Denied(format!(
                    "Write to '{}' is denied — under protected path '{}'",
                    relative_path.display(),
                    denied.display()
                ));
            }
        }

        WriteCheck::Allowed
    }

    /// Post-command scrub: remove files that were planted during command execution.
    ///
    /// Returns the list of files that were scrubbed.
    ///
    /// A "planted file" is one that:
    ///   1. Is in the dangerous write set
    ///   2. Did NOT exist pre-session (not in `pre_existing`)
    ///   3. NOW exists post-command
    ///
    /// Cost: O(|dangerous_paths|) stat calls.
    pub fn scrub_planted(&self, project_root: &Path) -> Vec<PathBuf> {
        let mut scrubbed = Vec::new();

        for denied in &self.deny_write {
            if self.pre_existing.contains(denied) {
                // Was there before the session — leave it alone
                continue;
            }

            let abs_path = project_root.join(denied);
            if abs_path.exists() {
                // Planted during this session — remove it
                tracing::warn!(
                    path = %abs_path.display(),
                    "Scrubbing planted file in dangerous write set"
                );
                if abs_path.is_dir() {
                    let _ = std::fs::remove_dir_all(&abs_path);
                } else {
                    let _ = std::fs::remove_file(&abs_path);
                }
                scrubbed.push(denied.clone());
            }
        }

        scrubbed
    }

    /// Check if a command string attempts to write to any dangerous path.
    ///
    /// This is a fast heuristic check on the command text. It does NOT
    /// replace the post-command scrub, which catches writes that bypass
    /// command-text analysis.
    pub fn check_command(&self, command: &str) -> WriteCheck {
        let lower = command.to_lowercase();

        for denied in &self.deny_write {
            let denied_str = denied.to_string_lossy();
            // Check various write patterns
            let write_patterns = [
                format!("> {}", denied_str),
                format!(">> {}", denied_str),
                format!("tee {}", denied_str),
                format!("cp {} ", denied_str), // wrong direction, but check anyway
                format!("mv {} ", denied_str),
                format!("mkdir {}", denied_str),
                format!("mkdir -p {}", denied_str),
                format!("touch {}", denied_str),
                format!("echo {} > {}", "", denied_str), // echo ... > path
                format!("install {} {}", "", denied_str),
            ];

            for pattern in &write_patterns {
                if lower.contains(pattern.as_str()) {
                    return WriteCheck::Denied(format!(
                        "Command writes to protected path '{}' — auto-loaded config path",
                        denied_str
                    ));
                }
            }
        }

        WriteCheck::Allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn check_write_blocks_pipit_config() {
        let dir = tempdir().unwrap();
        let dws = DangerousWriteSet::new(dir.path());

        assert_eq!(
            dws.check_write(Path::new(".pipit/config.toml")),
            WriteCheck::Denied("Write to '.pipit/config.toml' is unconditionally denied — auto-loaded config path".into())
        );
    }

    #[test]
    fn check_write_blocks_git_hooks() {
        let dir = tempdir().unwrap();
        let dws = DangerousWriteSet::new(dir.path());

        assert_eq!(
            dws.check_write(Path::new(".git/hooks")),
            WriteCheck::Denied("Write to '.git/hooks' is unconditionally denied — auto-loaded config path".into())
        );
    }

    #[test]
    fn check_write_blocks_subdirs() {
        let dir = tempdir().unwrap();
        let dws = DangerousWriteSet::new(dir.path());

        match dws.check_write(Path::new(".pipit/hooks/evil.rhai")) {
            WriteCheck::Denied(_) => {} // expected
            WriteCheck::Allowed => panic!("should deny writes under .pipit/hooks/"),
        }
    }

    #[test]
    fn check_write_allows_normal_files() {
        let dir = tempdir().unwrap();
        let dws = DangerousWriteSet::new(dir.path());

        assert_eq!(dws.check_write(Path::new("src/main.rs")), WriteCheck::Allowed);
        assert_eq!(dws.check_write(Path::new("README.md")), WriteCheck::Allowed);
    }

    #[test]
    fn scrub_planted_files() {
        let dir = tempdir().unwrap();
        // Don't create any pre-existing files
        let dws = DangerousWriteSet::new(dir.path());

        // Simulate planting: create HEAD file (bare-repo indicator)
        std::fs::write(dir.path().join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let scrubbed = dws.scrub_planted(dir.path());
        assert!(scrubbed.contains(&PathBuf::from("HEAD")));
        assert!(!dir.path().join("HEAD").exists());
    }

    #[test]
    fn scrub_preserves_pre_existing() {
        let dir = tempdir().unwrap();
        // Create a pre-existing .gitmodules
        std::fs::write(dir.path().join(".gitmodules"), "[submodule]").unwrap();

        let dws = DangerousWriteSet::new(dir.path());
        let scrubbed = dws.scrub_planted(dir.path());

        // Should NOT scrub pre-existing files
        assert!(!scrubbed.contains(&PathBuf::from(".gitmodules")));
        assert!(dir.path().join(".gitmodules").exists());
    }

    #[test]
    fn check_command_blocks_writes() {
        let dir = tempdir().unwrap();
        let dws = DangerousWriteSet::new(dir.path());

        match dws.check_command("echo evil > .pipit/config.toml") {
            WriteCheck::Denied(_) => {} // expected
            WriteCheck::Allowed => panic!("should deny echo > .pipit/config.toml"),
        }
    }
}
