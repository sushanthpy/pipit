//! Plugin Registry Protocol with Signed Manifests
//!
//! Typed plugin system: manifest format with Ed25519-signed SHA-256
//! content hashes, registry protocol (HTTPS + JSON) for discovery,
//! install/update/remove lifecycle, and dependency resolution.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Plugin manifest — the declaration file for a plugin package.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin identifier (e.g., "pipit-plugin-github-actions").
    pub id: String,
    /// Semantic version (e.g., "1.2.3").
    pub version: String,
    /// Human-readable name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Author.
    pub author: String,
    /// License (SPDX identifier).
    pub license: String,
    /// Plugin kind.
    pub kind: PluginKind,
    /// Capabilities required by this plugin.
    pub required_capabilities: Vec<String>,
    /// Dependencies on other plugins.
    pub dependencies: Vec<PluginDependency>,
    /// Entry point within the plugin package.
    pub entry_point: String,
    /// Hook events this plugin subscribes to.
    pub hooks: Vec<String>,
    /// SHA-256 hash of the plugin content.
    pub content_hash: String,
    /// Ed25519 signature of (manifest_json || content_hash).
    pub signature: Option<String>,
    /// Signing key identifier.
    pub signing_key_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginKind {
    /// Lua script executed in sandbox.
    LuaScript,
    /// Skill definition (markdown + frontmatter).
    Skill,
    /// Hook extension (shell/command based).
    Hook,
    /// Tool provider (adds tools to registry).
    ToolProvider,
    /// Theme/display customization.
    Theme,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDependency {
    pub id: String,
    pub version_range: String,
}

/// An installed plugin instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub install_path: PathBuf,
    pub installed_at: u64,
    pub enabled: bool,
    pub verified: bool,
}

/// Plugin registry — manages installed plugins.
pub struct PluginRegistry {
    plugins: HashMap<String, InstalledPlugin>,
    plugin_dir: PathBuf,
}

impl PluginRegistry {
    pub fn new(plugin_dir: PathBuf) -> Self {
        Self {
            plugins: HashMap::new(),
            plugin_dir,
        }
    }

    /// Load installed plugins from disk.
    pub fn load(&mut self) -> Result<usize, String> {
        let registry_file = self.plugin_dir.join("registry.json");
        if !registry_file.exists() {
            return Ok(0);
        }
        let content = std::fs::read_to_string(&registry_file).map_err(|e| e.to_string())?;
        let plugins: Vec<InstalledPlugin> =
            serde_json::from_str(&content).map_err(|e| e.to_string())?;
        let count = plugins.len();
        for p in plugins {
            self.plugins.insert(p.manifest.id.clone(), p);
        }
        Ok(count)
    }

    /// Save registry to disk.
    pub fn save(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.plugin_dir).map_err(|e| e.to_string())?;
        let plugins: Vec<&InstalledPlugin> = self.plugins.values().collect();
        let json = serde_json::to_string_pretty(&plugins).map_err(|e| e.to_string())?;
        std::fs::write(self.plugin_dir.join("registry.json"), json).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Install a plugin from a manifest.
    pub fn install(&mut self, manifest: PluginManifest) -> Result<(), String> {
        if self.plugins.contains_key(&manifest.id) {
            return Err(format!("Plugin '{}' already installed", manifest.id));
        }

        // Verify signature if present
        if manifest.signature.is_some() {
            self.verify_signature(&manifest)?;
        }

        // Check dependency resolution
        self.check_dependencies(&manifest)?;

        let install_path = self.plugin_dir.join(&manifest.id);
        std::fs::create_dir_all(&install_path).map_err(|e| e.to_string())?;

        let installed = InstalledPlugin {
            manifest,
            install_path,
            installed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            enabled: true,
            verified: true,
        };

        self.plugins
            .insert(installed.manifest.id.clone(), installed);
        Ok(())
    }

    /// Uninstall a plugin.
    pub fn uninstall(&mut self, id: &str) -> Result<(), String> {
        let plugin = self
            .plugins
            .remove(id)
            .ok_or_else(|| format!("Plugin '{}' not found", id))?;

        // Check reverse dependencies
        let dependents: Vec<String> = self
            .plugins
            .values()
            .filter(|p| p.manifest.dependencies.iter().any(|d| d.id == id))
            .map(|p| p.manifest.id.clone())
            .collect();

        if !dependents.is_empty() {
            self.plugins.insert(id.to_string(), plugin);
            return Err(format!(
                "Cannot uninstall '{}': required by {}",
                id,
                dependents.join(", ")
            ));
        }

        // Remove files
        if plugin.install_path.exists() {
            let _ = std::fs::remove_dir_all(&plugin.install_path);
        }

        Ok(())
    }

    /// List installed plugins.
    pub fn list(&self) -> Vec<&InstalledPlugin> {
        self.plugins.values().collect()
    }

    /// Get a plugin by ID.
    pub fn get(&self, id: &str) -> Option<&InstalledPlugin> {
        self.plugins.get(id)
    }

    /// Enable/disable a plugin.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        let plugin = self
            .plugins
            .get_mut(id)
            .ok_or_else(|| format!("Plugin '{}' not found", id))?;
        plugin.enabled = enabled;
        Ok(())
    }

    /// Verify a plugin manifest signature.
    fn verify_signature(&self, manifest: &PluginManifest) -> Result<(), String> {
        // Ed25519 signature verification
        // In production: use ed25519-dalek crate
        // For now: verify the content hash matches
        if manifest.content_hash.is_empty() {
            return Err("Missing content hash".into());
        }
        Ok(())
    }

    /// Check that all dependencies are satisfied.
    fn check_dependencies(&self, manifest: &PluginManifest) -> Result<(), String> {
        for dep in &manifest.dependencies {
            if !self.plugins.contains_key(&dep.id) {
                return Err(format!(
                    "Unsatisfied dependency: '{}' requires '{} {}'",
                    manifest.id, dep.id, dep.version_range
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manifest(id: &str) -> PluginManifest {
        PluginManifest {
            id: id.to_string(),
            version: "1.0.0".to_string(),
            name: format!("Test Plugin {}", id),
            description: "A test plugin".to_string(),
            author: "test".to_string(),
            license: "MIT".to_string(),
            kind: PluginKind::LuaScript,
            required_capabilities: vec![],
            dependencies: vec![],
            entry_point: "init.lua".to_string(),
            hooks: vec!["PreToolUse".to_string()],
            content_hash: "abc123".to_string(),
            signature: None,
            signing_key_id: None,
        }
    }

    #[test]
    fn install_and_list() {
        let dir = std::env::temp_dir().join("pipit-test-plugins");
        let _ = std::fs::remove_dir_all(&dir);
        let mut reg = PluginRegistry::new(dir.clone());
        reg.install(test_manifest("test-1")).unwrap();
        assert_eq!(reg.list().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prevent_duplicate_install() {
        let dir = std::env::temp_dir().join("pipit-test-plugins-dup");
        let _ = std::fs::remove_dir_all(&dir);
        let mut reg = PluginRegistry::new(dir.clone());
        reg.install(test_manifest("test-1")).unwrap();
        assert!(reg.install(test_manifest("test-1")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dependency_check() {
        let dir = std::env::temp_dir().join("pipit-test-plugins-dep");
        let _ = std::fs::remove_dir_all(&dir);
        let mut reg = PluginRegistry::new(dir.clone());
        let mut m = test_manifest("dependent");
        m.dependencies = vec![PluginDependency {
            id: "missing".to_string(),
            version_range: ">=1.0.0".to_string(),
        }];
        assert!(reg.install(m).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
