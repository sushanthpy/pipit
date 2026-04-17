use crate::discovery::SkillRegistry;
use crate::frontmatter::SkillSource;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Memoized walk-up discovery for nested skill directories.
///
/// For each newly-touched file, walks from `dirname(file)` upward to `cwd`,
/// testing each level for a `.pipit/skills/` directory and skipping previously-checked paths.
/// Complexity: O(D) per unique file directory (D = depth to cwd), amortized O(1) for
/// subsequent files in the same subtree.
pub struct DynamicDiscovery {
    /// Directories already checked for `.pipit/skills/` (regardless of outcome).
    checked_dirs: HashSet<PathBuf>,
    /// Working directory — walk-up stops here.
    cwd: PathBuf,
}

impl DynamicDiscovery {
    pub fn new(cwd: PathBuf) -> Self {
        Self {
            checked_dirs: HashSet::new(),
            cwd,
        }
    }

    /// Discover skills reachable from the given file paths by walking up to `cwd`.
    /// Returns a registry containing only the newly-discovered skills (caller merges).
    /// Skips gitignored skill directories when inside a git repository.
    pub fn discover_for_paths(&mut self, file_paths: &[&Path]) -> SkillRegistry {
        let mut new_skill_dirs: Vec<(PathBuf, SkillSource)> = Vec::new();

        for fp in file_paths {
            let start = if fp.is_dir() {
                fp.to_path_buf()
            } else {
                match fp.parent() {
                    Some(p) => p.to_path_buf(),
                    None => continue,
                }
            };

            let mut dir = start;
            loop {
                // Don't walk past the cwd
                if !dir.starts_with(&self.cwd) {
                    break;
                }

                if self.checked_dirs.insert(dir.clone()) {
                    // First time seeing this dir — check for skill directories
                    let skill_dir = dir.join(".pipit").join("skills");
                    if skill_dir.is_dir() && !self.is_gitignored(&skill_dir) {
                        new_skill_dirs.push((skill_dir, SkillSource::Project));
                    }

                    // Also check .github/skills for compatibility
                    let gh_skill_dir = dir.join(".github").join("skills");
                    if gh_skill_dir.is_dir() && !self.is_gitignored(&gh_skill_dir) {
                        new_skill_dirs.push((gh_skill_dir, SkillSource::Project));
                    }
                }

                // Walk up
                match dir.parent() {
                    Some(parent) if parent != dir => dir = parent.to_path_buf(),
                    _ => break,
                }
            }
        }

        if new_skill_dirs.is_empty() {
            return SkillRegistry::new();
        }

        tracing::info!(
            "Dynamic discovery found {} new skill directories",
            new_skill_dirs.len()
        );

        SkillRegistry::discover_with_sources(&new_skill_dirs)
    }

    /// Check if a path is gitignored using `git check-ignore`.
    /// Fail-open: outside git repos (exit code 128) or on any error, treat as not ignored.
    fn is_gitignored(&self, path: &Path) -> bool {
        std::process::Command::new("git")
            .args(["check-ignore", "-q"])
            .arg(path)
            .current_dir(&self.cwd)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success()) // exit 0 = ignored
            .unwrap_or(false) // any error = not ignored (fail-open)
    }

    /// Number of directories already checked.
    pub fn checked_count(&self) -> usize {
        self.checked_dirs.len()
    }
}
