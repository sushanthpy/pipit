//! Rule loader — replaces `workflow.rs::load_rules` string-concat.
//!
//! Parses rule files from disk, extracts YAML frontmatter, produces
//! typed `Rule` structs with content-addressed IDs.

use crate::rule::{
    GrantDeclaration, Rule, RuleFrontmatter, RuleId, RuleKind, RuleTrustTier,
};
use crate::registry::RuleRegistry;
use crate::RuleError;
use pipit_core::capability::{Capability, CapabilitySet};
use pipit_core::proof::ImplementationTier;
use pipit_core::skill_activation::ActivationScope;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Rule loader — discovers and parses rule files into a RuleRegistry.
pub struct RuleLoader {
    rule_dirs: Vec<PathBuf>,
}

impl RuleLoader {
    pub fn new(rule_dirs: Vec<PathBuf>) -> Self {
        Self { rule_dirs }
    }

    /// Load all rules from configured directories into a registry.
    pub fn load(&self) -> Result<RuleRegistry, RuleError> {
        let mut registry = RuleRegistry::new();

        for dir in &self.rule_dirs {
            if !dir.exists() {
                continue;
            }
            let trust_tier = infer_trust_tier(dir);
            let scope = infer_scope(dir);
            self.scan_directory(dir, dir, trust_tier, scope, &mut registry)?;
        }

        Ok(registry)
    }

    fn scan_directory(
        &self,
        dir: &Path,
        root: &Path,
        trust_tier: RuleTrustTier,
        scope: ActivationScope,
        registry: &mut RuleRegistry,
    ) -> Result<(), RuleError> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return Ok(()),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Subdirectory increases depth.
                let depth = path
                    .strip_prefix(root)
                    .map(|p| p.components().count() as u32)
                    .unwrap_or(0);
                let sub_scope = ActivationScope::SubDirectory { depth };
                self.scan_directory(&path, root, trust_tier, sub_scope, registry)?;
            } else if is_rule_file(&path) {
                match self.parse_rule_file(&path, root, trust_tier, scope) {
                    Ok(rule) => {
                        registry.register(rule);
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse rule {}: {}", path.display(), e);
                    }
                }
            }
        }

        Ok(())
    }

    fn parse_rule_file(
        &self,
        path: &Path,
        root: &Path,
        trust_tier: RuleTrustTier,
        scope: ActivationScope,
    ) -> Result<Rule, RuleError> {
        let content = std::fs::read_to_string(path)?;
        let (frontmatter, body) = extract_frontmatter(&content);

        // Canonical path: relative to root, no extension, forward slashes.
        let rel = path.strip_prefix(root).unwrap_or(path);
        let canonical_path = rel
            .with_extension("")
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");

        let name = canonical_path.clone();

        // Parse frontmatter if present.
        let fm: RuleFrontmatter = if let Some(fm_str) = &frontmatter {
            serde_yaml_ng::from_str(fm_str).unwrap_or_default()
        } else {
            RuleFrontmatter::default()
        };

        let kind = parse_kind(fm.kind.as_deref());
        let tier = parse_tier(fm.tier.as_deref());
        let trust = fm
            .trust
            .as_deref()
            .map(parse_trust_tier)
            .unwrap_or(trust_tier);
        let capabilities = parse_capabilities(&fm.capabilities);

        // Content hash for cache keying.
        let content_hash = {
            let mut hasher = Sha256::new();
            hasher.update(body.as_bytes());
            let h = hasher.finalize();
            h.iter().map(|b| format!("{b:02x}")).collect::<String>()
        };

        let id = RuleId::compute(&canonical_path, &body);

        Ok(Rule {
            id,
            name,
            source_path: path.to_path_buf(),
            canonical_path,
            description: fm.description,
            kind,
            tier,
            trust_tier: trust,
            scope,
            required_capabilities: capabilities,
            body,
            content_hash,
            path_patterns: fm.paths,
            language_patterns: fm.languages,
            forbidden_paths: fm.forbidden_paths,
            required_sequence: fm.required_sequence,
            grants: fm.grants,
        })
    }

    /// Render rules as a prompt section (backward-compatible with load_rules).
    /// Uses the new type system but produces markdown output.
    pub fn render_prompt_section(registry: &RuleRegistry) -> String {
        let mut out = String::new();
        for rule in registry.active_rules() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            let kind_tag = match rule.kind {
                RuleKind::Mandate => "[MANDATE] ",
                RuleKind::Invariant => "[INVARIANT] ",
                RuleKind::Procedure => "[PROCEDURE] ",
                RuleKind::Preference => "",
            };
            out.push_str(&format!("### {}Rule: {}\n", kind_tag, rule.name));
            out.push_str(&rule.body);
        }
        out
    }
}

/// Extract YAML frontmatter delimited by `---` fences.
fn extract_frontmatter(content: &str) -> (Option<String>, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (None, content.to_string());
    }

    let after_first = &trimmed[3..];
    if let Some(end_idx) = after_first.find("\n---") {
        let fm = after_first[..end_idx].trim().to_string();
        let body = after_first[end_idx + 4..].trim_start().to_string();
        (Some(fm), body)
    } else {
        (None, content.to_string())
    }
}

fn parse_kind(s: Option<&str>) -> RuleKind {
    match s.map(|s| s.to_lowercase()).as_deref() {
        Some("mandate") => RuleKind::Mandate,
        Some("procedure") => RuleKind::Procedure,
        Some("invariant") => RuleKind::Invariant,
        _ => RuleKind::Preference, // Default.
    }
}

fn parse_tier(s: Option<&str>) -> ImplementationTier {
    match s.map(|s| s.to_lowercase()).as_deref() {
        Some("validated") => ImplementationTier::Validated,
        Some("llmstructured" | "llm_structured") => ImplementationTier::LlmStructured,
        Some("typeonly" | "type_only") => ImplementationTier::TypeOnly,
        _ => ImplementationTier::Heuristic, // Default.
    }
}

fn parse_trust_tier(s: &str) -> RuleTrustTier {
    match s.to_lowercase().as_str() {
        "managed" => RuleTrustTier::Managed,
        "team" => RuleTrustTier::Team,
        "project" => RuleTrustTier::Project,
        _ => RuleTrustTier::Local,
    }
}

fn parse_capabilities(caps: &[String]) -> CapabilitySet {
    let mut set = CapabilitySet::EMPTY;
    for cap in caps {
        match cap.to_lowercase().as_str() {
            "fsread" | "fs_read" => set = set.grant(Capability::FsRead),
            "fswrite" | "fs_write" => set = set.grant(Capability::FsWrite),
            "fsreadexternal" | "fs_read_external" => {
                set = set.grant(Capability::FsReadExternal)
            }
            "fswriteexternal" | "fs_write_external" => {
                set = set.grant(Capability::FsWriteExternal)
            }
            "processexec" | "process_exec" => set = set.grant(Capability::ProcessExec),
            "processexecmutating" | "process_exec_mutating" => {
                set = set.grant(Capability::ProcessExecMutating)
            }
            "networkread" | "network_read" => set = set.grant(Capability::NetworkRead),
            "networkwrite" | "network_write" => set = set.grant(Capability::NetworkWrite),
            "mcpinvoke" | "mcp_invoke" => set = set.grant(Capability::McpInvoke),
            "delegate" => set = set.grant(Capability::Delegate),
            "verify" => set = set.grant(Capability::Verify),
            "configmodify" | "config_modify" => set = set.grant(Capability::ConfigModify),
            "envaccess" | "env_access" => set = set.grant(Capability::EnvAccess),
            _ => {
                tracing::warn!("Unknown capability in rule frontmatter: {}", cap);
            }
        }
    }
    set
}

fn infer_trust_tier(dir: &Path) -> RuleTrustTier {
    let dir_str = dir.to_string_lossy();
    if dir_str.contains(".pipit/rules") || dir_str.contains(".github/rules") {
        RuleTrustTier::Project
    } else if dir_str.contains(".config/pipit") {
        RuleTrustTier::Local
    } else {
        RuleTrustTier::Project
    }
}

fn infer_scope(dir: &Path) -> ActivationScope {
    let dir_str = dir.to_string_lossy();
    if dir_str.contains(".config/pipit") {
        ActivationScope::User
    } else {
        ActivationScope::Project
    }
}

fn is_rule_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext == "md" || ext == "yaml" || ext == "yml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_frontmatter() {
        let content = "---\nkind: mandate\n---\nRule body here.";
        let (fm, body) = extract_frontmatter(content);
        assert_eq!(fm.unwrap(), "kind: mandate");
        assert_eq!(body, "Rule body here.");
    }

    #[test]
    fn test_extract_no_frontmatter() {
        let content = "Just a rule body.";
        let (fm, body) = extract_frontmatter(content);
        assert!(fm.is_none());
        assert_eq!(body, "Just a rule body.");
    }

    #[test]
    fn test_parse_kind() {
        assert_eq!(parse_kind(Some("mandate")), RuleKind::Mandate);
        assert_eq!(parse_kind(Some("Procedure")), RuleKind::Procedure);
        assert_eq!(parse_kind(Some("INVARIANT")), RuleKind::Invariant);
        assert_eq!(parse_kind(None), RuleKind::Preference);
    }

    #[test]
    fn test_rule_id_deterministic() {
        let id1 = RuleId::compute("common/security", "Never write to /etc");
        let id2 = RuleId::compute("common/security", "Never write to /etc");
        assert_eq!(id1, id2);

        let id3 = RuleId::compute("common/security", "Never write to /etc!");
        assert_ne!(id1, id3); // Different content → different ID.
    }

    #[test]
    fn test_parse_capabilities() {
        let caps = vec!["FsWrite".to_string(), "verify".to_string()];
        let set = parse_capabilities(&caps);
        assert!(set.has(Capability::FsWrite));
        assert!(set.has(Capability::Verify));
        assert!(!set.has(Capability::NetworkRead));
    }
}
