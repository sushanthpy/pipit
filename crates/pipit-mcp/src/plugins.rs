//! Plugin Marketplace — community extension registry (ECO-4).
//!
//! Supports:
//! - Skills (custom slash commands with SKILL.md instructions)
//! - Hooks (Lua scripts for lifecycle events)
//! - Agents (specialized sub-agent definitions)
//! - MCP server configs (pre-configured connections)
//!
//! Install: `pipit plugin install <name>`
//! Publish: `pipit plugin publish <path>`

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A plugin manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub kind: PluginKind,
    pub tags: Vec<String>,
    pub homepage: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    Skill,
    Hook,
    Agent,
    McpServer,
    Bundle, // Multiple types in one package
}

/// A plugin in the local registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub install_path: PathBuf,
    pub installed_at: String,
}

/// Local plugin registry — tracks installed plugins.
pub struct PluginRegistry {
    plugins: Vec<InstalledPlugin>,
    registry_dir: PathBuf,
}

impl PluginRegistry {
    /// Load the local plugin registry from `.pipit/plugins/`.
    pub fn load(project_root: &Path) -> Self {
        let registry_dir = project_root.join(".pipit").join("plugins");
        let plugins = if registry_dir.exists() {
            let manifest_path = registry_dir.join("registry.json");
            if manifest_path.exists() {
                std::fs::read_to_string(&manifest_path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        Self {
            plugins,
            registry_dir,
        }
    }

    /// List all installed plugins.
    pub fn list(&self) -> &[InstalledPlugin] {
        &self.plugins
    }

    /// Install a plugin from a local path.
    pub fn install_from_path(&mut self, source: &Path) -> Result<(), String> {
        let manifest_path = source.join("plugin.json");
        if !manifest_path.exists() {
            return Err(format!("No plugin.json found in {}", source.display()));
        }
        let content =
            std::fs::read_to_string(&manifest_path).map_err(|e| format!("Read error: {}", e))?;
        let manifest: PluginManifest =
            serde_json::from_str(&content).map_err(|e| format!("Invalid plugin.json: {}", e))?;

        let install_dir = self.registry_dir.join(&manifest.name);
        let _ = std::fs::create_dir_all(&install_dir);

        // Copy plugin files
        copy_dir_recursive(source, &install_dir).map_err(|e| format!("Copy failed: {}", e))?;

        let installed = InstalledPlugin {
            manifest,
            install_path: install_dir,
            installed_at: chrono::Utc::now().to_rfc3339(),
        };

        // Remove old version if exists
        self.plugins
            .retain(|p| p.manifest.name != installed.manifest.name);
        self.plugins.push(installed);
        self.save();

        Ok(())
    }

    /// Uninstall a plugin.
    pub fn uninstall(&mut self, name: &str) -> Result<(), String> {
        let idx = self
            .plugins
            .iter()
            .position(|p| p.manifest.name == name)
            .ok_or_else(|| format!("Plugin '{}' not installed", name))?;
        let plugin = self.plugins.remove(idx);
        if plugin.install_path.exists() {
            let _ = std::fs::remove_dir_all(&plugin.install_path);
        }
        self.save();
        Ok(())
    }

    fn save(&self) {
        let _ = std::fs::create_dir_all(&self.registry_dir);
        let path = self.registry_dir.join("registry.json");
        let json = serde_json::to_string_pretty(&self.plugins).unwrap_or_default();
        let _ = std::fs::write(path, json);
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
