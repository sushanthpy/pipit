//! # Multi-Source Settings Hierarchy with Policy Precedence
//!
//! Deterministic, source-ranked settings merge algorithm:
//!
//! ```text
//! policy > flag > local > project > user
//! ```
//!
//! Allow rules follow source precedence. Deny rules union across all sources
//! (deny always wins, regardless of source). This enables enterprise deployment
//! where a managed drop-in dir (`/etc/pipit/managed/`) enforces policy that
//! user-level configs cannot override.
//!
//! Resolution is O(|S| · |rules|), single-pass.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Source of a setting, ordered by precedence (highest first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum SettingsSource {
    /// Organization/enterprise managed policy (highest precedence).
    /// Read from `/etc/pipit/managed/` or platform equivalent.
    Policy = 5,
    /// CLI flag overrides.
    Flag = 4,
    /// Local settings (`.pipit/settings.local.toml`, gitignored).
    Local = 3,
    /// Project settings (`.pipit/settings.toml`, shared in git).
    Project = 2,
    /// User settings (`~/.config/pipit/config.toml`).
    User = 1,
}

impl std::fmt::Display for SettingsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Policy => write!(f, "policy"),
            Self::Flag => write!(f, "flag"),
            Self::Local => write!(f, "local"),
            Self::Project => write!(f, "project"),
            Self::User => write!(f, "user"),
        }
    }
}

/// A resolved setting value with provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedValue<T: Clone> {
    pub value: T,
    pub source: SettingsSource,
    /// The file path that provided this value (if any).
    pub origin_file: Option<PathBuf>,
}

/// Permission rule with source tagging.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaggedRule {
    pub pattern: String,
    pub action: RuleAction,
    pub source: SettingsSource,
    /// The directory relative to which path patterns are resolved.
    pub base_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleAction {
    Allow,
    Deny,
    Ask,
}

/// A settings layer from a single source.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SettingsLayer {
    /// Sandbox configuration overrides.
    #[serde(default)]
    pub sandbox_enabled: Option<bool>,
    #[serde(default)]
    pub sandbox_mandatory: Option<bool>,
    /// Network domain allowlist additions.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
    /// Network domain denylist (union across all sources).
    #[serde(default)]
    pub denied_domains: Vec<String>,
    /// Binary allowlist additions.
    #[serde(default)]
    pub allowed_binaries: Vec<String>,
    /// Binary denylist (union across all sources).
    #[serde(default)]
    pub denied_binaries: Vec<String>,
    /// Permission rules (allow/deny/ask).
    #[serde(default)]
    pub rules: Vec<TaggedRule>,
    /// Whether to allow only managed domains (enterprise feature).
    #[serde(default)]
    pub allow_managed_domains_only: Option<bool>,
    /// Hard fail if sandbox runtime is unavailable.
    #[serde(default)]
    pub fail_if_unavailable: Option<bool>,
    /// Restrict sandbox to specific platforms.
    #[serde(default)]
    pub enabled_platforms: Vec<String>,
    /// Arbitrary key-value settings.
    #[serde(default)]
    pub settings: HashMap<String, toml::Value>,
}

/// The merged settings hierarchy.
#[derive(Debug, Clone)]
pub struct SettingsHierarchy {
    /// Layers ordered by source precedence (policy first).
    layers: Vec<(SettingsSource, SettingsLayer, PathBuf)>,
}

impl SettingsHierarchy {
    /// Build the settings hierarchy by loading from all known sources.
    pub fn load(project_root: &Path) -> Self {
        let mut layers = Vec::new();

        // 1. User settings (~/.config/pipit/config.toml)
        if let Some(user_config) = dirs::config_dir() {
            let path = user_config.join("pipit").join("config.toml");
            if let Some(layer) = Self::load_layer(&path) {
                layers.push((SettingsSource::User, layer, path));
            }
        }

        // 2. Project settings (.pipit/settings.toml)
        let project_path = project_root.join(".pipit").join("settings.toml");
        if let Some(layer) = Self::load_layer(&project_path) {
            layers.push((SettingsSource::Project, layer, project_path));
        }

        // 3. Local settings (.pipit/settings.local.toml)
        let local_path = project_root.join(".pipit").join("settings.local.toml");
        if let Some(layer) = Self::load_layer(&local_path) {
            layers.push((SettingsSource::Local, layer, local_path));
        }

        // 4. Policy settings (/etc/pipit/managed/)
        let managed_dir = PathBuf::from("/etc/pipit/managed");
        if managed_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&managed_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().is_some_and(|e| e == "toml") {
                        if let Some(layer) = Self::load_layer(&path) {
                            layers.push((SettingsSource::Policy, layer, path));
                        }
                    }
                }
            }
        }

        // Sort by precedence (policy first = highest Ord value)
        layers.sort_by(|a, b| b.0.cmp(&a.0));

        Self { layers }
    }

    /// Load a single settings layer from a TOML file.
    fn load_layer(path: &Path) -> Option<SettingsLayer> {
        let content = std::fs::read_to_string(path).ok()?;
        toml::from_str(&content).ok()
    }

    /// Add a flag-source override (from CLI arguments).
    pub fn add_flag_override(&mut self, key: &str, value: toml::Value) {
        // Find or create the flag layer
        let flag_idx = self
            .layers
            .iter()
            .position(|(s, _, _)| *s == SettingsSource::Flag);

        if let Some(idx) = flag_idx {
            self.layers[idx].1.settings.insert(key.to_string(), value);
        } else {
            let mut layer = SettingsLayer::default();
            layer.settings.insert(key.to_string(), value);
            self.layers
                .push((SettingsSource::Flag, layer, PathBuf::from("<cli>")));
            self.layers.sort_by(|a, b| b.0.cmp(&a.0));
        }
    }

    /// Resolve a boolean setting with source precedence.
    /// Returns the value from the highest-precedence source that sets it.
    pub fn resolve_bool(&self, getter: impl Fn(&SettingsLayer) -> Option<bool>) -> Option<ResolvedValue<bool>> {
        for (source, layer, path) in &self.layers {
            if let Some(val) = getter(layer) {
                return Some(ResolvedValue {
                    value: val,
                    source: *source,
                    origin_file: Some(path.clone()),
                });
            }
        }
        None
    }

    /// Resolve sandbox_enabled with source precedence.
    pub fn sandbox_enabled(&self) -> Option<ResolvedValue<bool>> {
        self.resolve_bool(|l| l.sandbox_enabled)
    }

    /// Resolve sandbox_mandatory with source precedence.
    pub fn sandbox_mandatory(&self) -> Option<ResolvedValue<bool>> {
        self.resolve_bool(|l| l.sandbox_mandatory)
    }

    /// Resolve the merged domain allowlist.
    /// Deny rules union across all sources. Allow rules follow precedence.
    pub fn resolved_domains(&self) -> (Vec<String>, Vec<String>) {
        let mut allowed = Vec::new();
        let mut denied: HashSet<String> = HashSet::new();

        // Deny = union across all sources (deny always wins)
        for (_, layer, _) in &self.layers {
            for d in &layer.denied_domains {
                denied.insert(d.clone());
            }
        }

        // Allow from highest-precedence source that has allowed_domains
        // If policy sets allow_managed_domains_only, only policy's domains count
        let managed_only = self
            .layers
            .iter()
            .any(|(s, l, _)| *s == SettingsSource::Policy && l.allow_managed_domains_only == Some(true));

        if managed_only {
            // Only policy-level domains
            for (source, layer, _) in &self.layers {
                if *source == SettingsSource::Policy {
                    allowed.extend(layer.allowed_domains.clone());
                }
            }
        } else {
            // Merge from all sources (higher precedence first, dedup)
            let mut seen = HashSet::new();
            for (_, layer, _) in &self.layers {
                for d in &layer.allowed_domains {
                    if seen.insert(d.clone()) {
                        allowed.push(d.clone());
                    }
                }
            }
        }

        // Remove any allowed domains that are also denied
        allowed.retain(|d| !denied.contains(d));

        (allowed, denied.into_iter().collect())
    }

    /// Explain where each resolved setting came from.
    /// Useful for `pipit config explain <key>`.
    pub fn explain(&self) -> Vec<(String, String, SettingsSource, PathBuf)> {
        let mut explanations = Vec::new();

        for (source, layer, path) in &self.layers {
            if let Some(v) = layer.sandbox_enabled {
                explanations.push((
                    "sandbox_enabled".into(),
                    v.to_string(),
                    *source,
                    path.clone(),
                ));
            }
            if let Some(v) = layer.sandbox_mandatory {
                explanations.push((
                    "sandbox_mandatory".into(),
                    v.to_string(),
                    *source,
                    path.clone(),
                ));
            }
            if !layer.allowed_domains.is_empty() {
                explanations.push((
                    "allowed_domains".into(),
                    format!("{:?}", layer.allowed_domains),
                    *source,
                    path.clone(),
                ));
            }
            if !layer.denied_domains.is_empty() {
                explanations.push((
                    "denied_domains".into(),
                    format!("{:?}", layer.denied_domains),
                    *source,
                    path.clone(),
                ));
            }
            for (k, v) in &layer.settings {
                explanations.push((k.clone(), format!("{}", v), *source, path.clone()));
            }
        }

        explanations
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_always_wins() {
        let mut hierarchy = SettingsHierarchy { layers: vec![] };

        // User allows example.com
        let mut user = SettingsLayer::default();
        user.allowed_domains.push("example.com".into());
        hierarchy.layers.push((
            SettingsSource::User,
            user,
            PathBuf::from("~/.config/pipit/config.toml"),
        ));

        // Policy denies example.com
        let mut policy = SettingsLayer::default();
        policy.denied_domains.push("example.com".into());
        hierarchy.layers.push((
            SettingsSource::Policy,
            policy,
            PathBuf::from("/etc/pipit/managed/policy.toml"),
        ));

        hierarchy.layers.sort_by(|a, b| b.0.cmp(&a.0));

        let (allowed, denied) = hierarchy.resolved_domains();
        assert!(!allowed.contains(&"example.com".to_string()));
        assert!(denied.contains(&"example.com".to_string()));
    }

    #[test]
    fn policy_precedence() {
        let mut hierarchy = SettingsHierarchy { layers: vec![] };

        // User: sandbox not mandatory
        let mut user = SettingsLayer::default();
        user.sandbox_mandatory = Some(false);
        hierarchy.layers.push((
            SettingsSource::User,
            user,
            PathBuf::from("user.toml"),
        ));

        // Policy: sandbox mandatory
        let mut policy = SettingsLayer::default();
        policy.sandbox_mandatory = Some(true);
        hierarchy.layers.push((
            SettingsSource::Policy,
            policy,
            PathBuf::from("policy.toml"),
        ));

        hierarchy.layers.sort_by(|a, b| b.0.cmp(&a.0));

        let resolved = hierarchy.sandbox_mandatory().unwrap();
        assert!(resolved.value); // policy wins
        assert_eq!(resolved.source, SettingsSource::Policy);
    }

    #[test]
    fn managed_domains_only() {
        let mut hierarchy = SettingsHierarchy { layers: vec![] };

        // User adds evil.com
        let mut user = SettingsLayer::default();
        user.allowed_domains.push("evil.com".into());
        hierarchy.layers.push((SettingsSource::User, user, PathBuf::from("u.toml")));

        // Policy: only managed domains
        let mut policy = SettingsLayer::default();
        policy.allow_managed_domains_only = Some(true);
        policy.allowed_domains.push("github.com".into());
        hierarchy.layers.push((SettingsSource::Policy, policy, PathBuf::from("p.toml")));

        hierarchy.layers.sort_by(|a, b| b.0.cmp(&a.0));

        let (allowed, _denied) = hierarchy.resolved_domains();
        assert!(allowed.contains(&"github.com".to_string()));
        assert!(!allowed.contains(&"evil.com".to_string())); // user domain blocked
    }
}
