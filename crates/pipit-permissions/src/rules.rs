
//! TOML Permission Rules — User-defined permission policies.
//!
//! Rules are loaded from `.pipit/permissions.toml` files (project and global).
//! Evaluation: first matching rule wins (priority-ordered).
//!
//! ```toml
//! [[rules]]
//! name = "allow-test-commands"
//! tool = "bash"
//! command_pattern = "npm test*"
//! decision = "allow"
//! modes = ["default", "plan", "auto"]
//!
//! [[rules]]
//! name = "deny-production-writes"
//! tool = "*"
//! path_pattern = "production/**"
//! decision = "deny"
//! ```

use crate::{Decision, PermissionMode, PermissionResult, ToolCallDescriptor};
use globset::{Glob, GlobMatcher};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Where a rule was loaded from (for audit and shadow detection).
#[derive(Debug, Clone)]
pub struct RuleSource {
    pub file: PathBuf,
    pub line: usize,
    pub index: usize,
}

/// A single permission rule from TOML.
#[derive(Debug, Clone)]
pub struct PermissionRule {
    pub name: String,
    pub tool_matcher: GlobMatcher,
    pub command_matcher: Option<GlobMatcher>,
    pub path_matcher: Option<GlobMatcher>,
    pub decision: Decision,
    pub modes: Vec<PermissionMode>,
    pub description: Option<String>,
    pub source: RuleSource,
}

/// Raw deserialization target for TOML rules.
#[derive(Debug, Deserialize)]
struct RawRuleFile {
    #[serde(default)]
    rules: Vec<RawRule>,
}

#[derive(Debug, Deserialize)]
struct RawRule {
    name: String,
    tool: String,
    #[serde(default)]
    command_pattern: Option<String>,
    #[serde(default)]
    path_pattern: Option<String>,
    decision: String,
    #[serde(default)]
    modes: Vec<String>,
    #[serde(default)]
    description: Option<String>,
}

/// The complete set of permission rules, ordered by priority.
#[derive(Debug, Clone)]
pub struct PermissionRuleSet {
    rules: Vec<PermissionRule>,
}

impl PermissionRuleSet {
    pub fn empty() -> Self {
        Self { rules: Vec::new() }
    }

    /// Load rules from one or more TOML files.
    pub fn load(paths: &[PathBuf]) -> Self {
        let mut all_rules = Vec::new();

        for path in paths {
            if !path.exists() {
                continue;
            }

            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to read permission rules from {}: {}", path.display(), e);
                    continue;
                }
            };

            let raw: RawRuleFile = match toml::from_str(&content) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("Failed to parse permission rules from {}: {}", path.display(), e);
                    continue;
                }
            };

            for (index, raw_rule) in raw.rules.into_iter().enumerate() {
                match parse_rule(raw_rule, path, index) {
                    Ok(rule) => all_rules.push(rule),
                    Err(e) => {
                        tracing::warn!("Invalid permission rule in {}: {}", path.display(), e);
                    }
                }
            }
        }

        tracing::info!(count = all_rules.len(), "Loaded permission rules");
        Self { rules: all_rules }
    }

    /// Evaluate a tool call against rules. Returns the first matching rule's decision.
    /// Returns None if no rule matches (fall through to classifiers).
    pub fn evaluate(
        &self,
        descriptor: &ToolCallDescriptor,
        mode: PermissionMode,
    ) -> Option<PermissionResult> {
        for rule in &self.rules {
            // Check mode applicability
            if !rule.modes.is_empty() && !rule.modes.contains(&mode) {
                continue;
            }

            // Check tool name match
            if !rule.tool_matcher.is_match(&descriptor.tool_name) {
                continue;
            }

            // Check command pattern (if specified)
            if let Some(ref cmd_matcher) = rule.command_matcher {
                if let Some(ref cmd) = descriptor.command {
                    if !cmd_matcher.is_match(cmd) {
                        continue;
                    }
                } else {
                    continue; // Rule requires command but tool has none
                }
            }

            // Check path pattern (if specified)
            if let Some(ref path_matcher) = rule.path_matcher {
                let any_path_matches = descriptor.paths.iter().any(|p| {
                    path_matcher.is_match(p.display().to_string().as_str())
                });
                if !any_path_matches && !descriptor.paths.is_empty() {
                    continue;
                }
                if descriptor.paths.is_empty() {
                    continue; // Rule requires path but tool has none
                }
            }

            // Rule matches
            return Some(PermissionResult {
                decision: rule.decision,
                mode,
                classifier_verdicts: HashMap::new(),
                matched_rule: Some(rule.name.clone()),
                explanation: format!(
                    "Rule '{}' matched: {:?}{}",
                    rule.name,
                    rule.decision,
                    rule.description
                        .as_ref()
                        .map(|d| format!(" — {d}"))
                        .unwrap_or_default()
                ),
            });
        }

        None // No rule matched
    }

    /// Get all rules (for shadow detection).
    pub fn rules(&self) -> &[PermissionRule] {
        &self.rules
    }
}

fn parse_rule(
    raw: RawRule,
    source_file: &std::path::Path,
    index: usize,
) -> Result<PermissionRule, String> {
    let tool_glob = Glob::new(&raw.tool)
        .map_err(|e| format!("Invalid tool pattern '{}': {}", raw.tool, e))?;

    let command_matcher = raw
        .command_pattern
        .as_ref()
        .map(|p| Glob::new(p).map(|g| g.compile_matcher()))
        .transpose()
        .map_err(|e| format!("Invalid command pattern: {e}"))?;

    let path_matcher = raw
        .path_pattern
        .as_ref()
        .map(|p| Glob::new(p).map(|g| g.compile_matcher()))
        .transpose()
        .map_err(|e| format!("Invalid path pattern: {e}"))?;

    let decision = match raw.decision.to_lowercase().as_str() {
        "allow" => Decision::Allow,
        "ask" => Decision::Ask,
        "deny" => Decision::Deny,
        "escalate" => Decision::Escalate,
        other => return Err(format!("Unknown decision: {other}")),
    };

    let modes: Vec<PermissionMode> = raw
        .modes
        .iter()
        .filter_map(|m| m.parse().ok())
        .collect();

    Ok(PermissionRule {
        name: raw.name,
        tool_matcher: tool_glob.compile_matcher(),
        command_matcher,
        path_matcher,
        decision,
        modes,
        description: raw.description,
        source: RuleSource {
            file: source_file.to_path_buf(),
            line: 0, // TOML doesn't give us line numbers easily
            index,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn load_and_match_rules() {
        let toml = r#"
[[rules]]
name = "allow-tests"
tool = "bash"
command_pattern = "npm test*"
decision = "allow"

[[rules]]
name = "deny-production"
tool = "*"
path_pattern = "production/**"
decision = "deny"
"#;
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(toml.as_bytes()).unwrap();

        let rules = PermissionRuleSet::load(&[f.path().to_path_buf()]);
        assert_eq!(rules.rules().len(), 2);

        // Test match: bash npm test
        let desc = crate::ToolCallDescriptor {
            tool_name: "bash".to_string(),
            args: serde_json::json!({}),
            paths: vec![],
            command: Some("npm test --coverage".to_string()),
            is_mutating: true,
            project_root: PathBuf::from("/tmp"),
        };
        let result = rules.evaluate(&desc, PermissionMode::Default);
        assert!(result.is_some());
        assert_eq!(result.unwrap().decision, Decision::Allow);

        // Test match: write to production/
        let desc2 = crate::ToolCallDescriptor {
            tool_name: "write_file".to_string(),
            args: serde_json::json!({}),
            paths: vec![PathBuf::from("production/config.yml")],
            command: None,
            is_mutating: true,
            project_root: PathBuf::from("/tmp"),
        };
        let result2 = rules.evaluate(&desc2, PermissionMode::Default);
        assert!(result2.is_some());
        assert_eq!(result2.unwrap().decision, Decision::Deny);
    }
}

