//! `pipit plugin` — plugin management with local and remote registry support.
//!
//! Supports:
//! - `pipit plugin install <source>` — install from local path or remote registry
//! - `pipit plugin uninstall <name>` — remove a plugin
//! - `pipit plugin list` — list installed plugins
//! - `pipit plugin search <query>` — search the registry index

use crate::PluginAction;
use anyhow::Result;
use std::path::PathBuf;

/// Default registry URL. Points to a GitHub-hosted JSON index.
const REGISTRY_INDEX_URL: &str =
    "https://raw.githubusercontent.com/pipit-project/registry/main/index.json";

/// Handle a plugin subcommand.
pub async fn handle_plugin_action(action: &PluginAction) -> Result<()> {
    let cwd = std::env::current_dir()?;

    match action {
        PluginAction::Install { source } => install_plugin(source, &cwd).await,
        PluginAction::Uninstall { name } => uninstall_plugin(name, &cwd),
        PluginAction::List => list_plugins(&cwd),
        PluginAction::Search { query } => search_registry(query).await,
    }
}

/// Install a plugin from either a local path or remote registry.
async fn install_plugin(source: &str, project_root: &std::path::Path) -> Result<()> {
    let source_path = PathBuf::from(source);

    if source_path.exists() && source_path.is_dir() {
        // Local path install
        eprintln!("\x1b[36mInstalling from local path: {}\x1b[0m", source);
        let mut registry = pipit_mcp::PluginRegistry::load(project_root);
        registry
            .install_from_path(&source_path)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        eprintln!("\x1b[32m✓ Plugin installed from {}\x1b[0m", source);
        return Ok(());
    }

    // Try remote registry
    eprintln!("\x1b[36mSearching registry for '{}'\x1b[0m", source);

    let index = fetch_registry_index().await?;
    let entry = index
        .iter()
        .find(|e| e.name == *source)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Plugin '{}' not found in registry. Use `pipit plugin search <query>` to browse.",
                source
            )
        })?;

    eprintln!(
        "  Found: {} v{} — {}",
        entry.name, entry.version, entry.description
    );

    // Download to temp directory
    let temp_dir = tempfile::tempdir()?;
    let archive_path = temp_dir.path().join("plugin.tar.gz");

    eprintln!("  Downloading from {}...", entry.download_url);
    let response = reqwest::get(&entry.download_url).await?;
    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Download failed: HTTP {}",
            response.status()
        ));
    }
    let bytes = response.bytes().await?;

    // Verify checksum if provided
    if let Some(ref expected_sha) = entry.sha256 {
        use std::io::Write;
        let mut hasher = sha2_hash(&bytes);
        if hasher != *expected_sha {
            return Err(anyhow::anyhow!(
                "Checksum mismatch! Expected {} got {}. The download may be corrupted or tampered with.",
                expected_sha, hasher
            ));
        }
        eprintln!("  \x1b[32m✓\x1b[0m Checksum verified");
    }

    std::fs::write(&archive_path, &bytes)?;

    // Extract archive
    let extract_dir = temp_dir.path().join("extracted");
    std::fs::create_dir_all(&extract_dir)?;
    let status = std::process::Command::new("tar")
        .args(["xzf", archive_path.to_str().unwrap_or("")])
        .arg("-C")
        .arg(extract_dir.to_str().unwrap_or(""))
        .status()?;
    if !status.success() {
        return Err(anyhow::anyhow!("Failed to extract plugin archive"));
    }

    // Find the plugin directory (may be nested one level)
    let plugin_dir = find_plugin_dir(&extract_dir)?;

    // Install from extracted path
    let mut registry = pipit_mcp::PluginRegistry::load(project_root);
    registry
        .install_from_path(&plugin_dir)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    eprintln!(
        "\x1b[32m✓ Installed {} v{}\x1b[0m",
        entry.name, entry.version
    );
    Ok(())
}

fn uninstall_plugin(name: &str, project_root: &std::path::Path) -> Result<()> {
    let mut registry = pipit_mcp::PluginRegistry::load(project_root);
    registry
        .uninstall(name)
        .map_err(|e| anyhow::anyhow!("{}", e))?;
    eprintln!("\x1b[32m✓ Uninstalled '{}'\x1b[0m", name);
    Ok(())
}

fn list_plugins(project_root: &std::path::Path) -> Result<()> {
    let registry = pipit_mcp::PluginRegistry::load(project_root);
    let plugins = registry.list();

    if plugins.is_empty() {
        eprintln!("\x1b[2mNo plugins installed.\x1b[0m");
        eprintln!("\x1b[2mInstall with: pipit plugin install <name>\x1b[0m");
        return Ok(());
    }

    eprintln!("\x1b[1mInstalled plugins:\x1b[0m\n");
    for p in plugins {
        eprintln!(
            "  \x1b[36m{}\x1b[0m v{} ({:?})",
            p.manifest.name, p.manifest.version, p.manifest.kind
        );
        eprintln!("    {}", p.manifest.description);
        eprintln!("    \x1b[2mInstalled: {}\x1b[0m", p.installed_at);
    }
    Ok(())
}

async fn search_registry(query: &str) -> Result<()> {
    eprintln!("\x1b[36mSearching registry for '{}'\x1b[0m\n", query);

    let index = fetch_registry_index().await?;
    let query_lower = query.to_lowercase();
    let matches: Vec<_> = index
        .iter()
        .filter(|e| {
            e.name.to_lowercase().contains(&query_lower)
                || e.description.to_lowercase().contains(&query_lower)
                || e.tags.iter().any(|t| t.to_lowercase().contains(&query_lower))
        })
        .collect();

    if matches.is_empty() {
        eprintln!("No plugins found matching '{}'.", query);
        return Ok(());
    }

    eprintln!("\x1b[1m{} plugin(s) found:\x1b[0m\n", matches.len());
    for entry in matches {
        eprintln!(
            "  \x1b[36m{}\x1b[0m v{} ({:?})",
            entry.name, entry.version, entry.kind
        );
        eprintln!("    {}", entry.description);
        if !entry.tags.is_empty() {
            eprintln!("    \x1b[2mTags: {}\x1b[0m", entry.tags.join(", "));
        }
    }
    eprintln!("\n\x1b[2mInstall with: pipit plugin install <name>\x1b[0m");
    Ok(())
}

/// Registry index entry (fetched from remote JSON).
#[derive(Debug, serde::Deserialize)]
struct RegistryEntry {
    name: String,
    version: String,
    description: String,
    kind: pipit_mcp::PluginKind,
    #[serde(default)]
    tags: Vec<String>,
    download_url: String,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    author: String,
}

/// Fetch the plugin registry index from the remote URL.
async fn fetch_registry_index() -> Result<Vec<RegistryEntry>> {
    let url = std::env::var("PIPIT_REGISTRY_URL").unwrap_or_else(|_| REGISTRY_INDEX_URL.to_string());

    let response = reqwest::get(&url).await.map_err(|e| {
        anyhow::anyhow!(
            "Failed to fetch registry index from {}: {}. \
             Check your network connection or set PIPIT_REGISTRY_URL to an alternative.",
            url,
            e
        )
    })?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Registry returned HTTP {}. The registry may be unavailable.",
            response.status()
        ));
    }

    let entries: Vec<RegistryEntry> = response.json().await.map_err(|e| {
        anyhow::anyhow!("Failed to parse registry index: {}", e)
    })?;

    Ok(entries)
}

/// Simple SHA-256 hash using std — we avoid adding a heavy crypto dependency
/// by using a basic implementation. For production, switch to `sha2` crate.
fn sha2_hash(data: &[u8]) -> String {
    // Use the system's shasum command for simplicity
    use std::io::Write;
    let mut child = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap_or_else(|_| {
            // Fallback: use openssl
            std::process::Command::new("openssl")
                .args(["dgst", "-sha256", "-hex"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .expect("Neither shasum nor openssl available for checksum verification")
        });
    if let Some(ref mut stdin) = child.stdin {
        let _ = stdin.write_all(data);
    }
    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(_) => return String::new(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    // shasum output: "hash  -\n" — extract just the hash
    stdout.split_whitespace().next().unwrap_or("").to_string()
}

/// Find the plugin directory inside an extracted archive.
/// Handles both flat (plugin.json at root) and nested (single subdir) layouts.
fn find_plugin_dir(extract_dir: &std::path::Path) -> Result<PathBuf> {
    // Check if plugin.json is directly in extract_dir
    if extract_dir.join("plugin.json").exists() {
        return Ok(extract_dir.to_path_buf());
    }
    // Check one level deep
    for entry in std::fs::read_dir(extract_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && path.join("plugin.json").exists() {
            return Ok(path);
        }
    }
    Err(anyhow::anyhow!(
        "No plugin.json found in the extracted archive"
    ))
}
