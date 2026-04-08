use anyhow::{Context, Result};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const REPO: &str = "sushanthpy/pipit";
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60; // 24 hours

/// Cached version check result stored in ~/.pipit/version-check
#[derive(Debug)]
struct VersionCache {
    checked_at: u64,
    latest_tag: String,
}

fn cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".pipit").join("version-check"))
}

fn read_cache() -> Option<VersionCache> {
    let path = cache_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    let mut lines = content.lines();
    let checked_at: u64 = lines.next()?.parse().ok()?;
    let latest_tag = lines.next()?.to_string();
    Some(VersionCache {
        checked_at,
        latest_tag,
    })
}

fn write_cache(tag: &str) {
    if let Some(path) = cache_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = std::fs::write(&path, format!("{}\n{}\n", now, tag));
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Fetch the latest release tag from GitHub API.
async fn fetch_latest_tag() -> Result<String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", REPO);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    let resp = client
        .get(&url)
        .header("User-Agent", "pipit-update-check")
        .header("Accept", "application/vnd.github.v3+json")
        .send()
        .await
        .context("Failed to reach GitHub")?;

    let body: serde_json::Value = resp.json().await.context("Invalid response from GitHub")?;
    body["tag_name"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("No tag_name in GitHub response"))
}

/// Parse a version tag like "v0.1.0" into (major, minor, patch).
fn parse_version(tag: &str) -> Option<(u32, u32, u32)> {
    let v = tag.strip_prefix('v').unwrap_or(tag);
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    Some((
        parts[0].parse().ok()?,
        parts[1].parse().ok()?,
        parts[2].parse().ok()?,
    ))
}

/// Returns true if `latest` is newer than `current`.
fn is_newer(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(cur), Some(lat)) => lat > cur,
        _ => false,
    }
}

/// Spawn a non-blocking background version check.
/// Returns a message to display if a new version is available, or None.
/// Respects a 24-hour cooldown to avoid spamming GitHub.
pub async fn check_for_update_background() -> Option<String> {
    let current = env!("CARGO_PKG_VERSION");

    // Check cache first
    if let Some(cache) = read_cache() {
        if now_secs() - cache.checked_at < CHECK_INTERVAL_SECS {
            // Use cached result
            if is_newer(current, &cache.latest_tag) {
                return Some(format_update_message(current, &cache.latest_tag));
            }
            return None;
        }
    }

    // Cache expired or missing — fetch from GitHub
    match fetch_latest_tag().await {
        Ok(latest) => {
            write_cache(&latest);
            if is_newer(current, &latest) {
                Some(format_update_message(current, &latest))
            } else {
                None
            }
        }
        Err(_) => {
            // Silently ignore network errors for background check
            None
        }
    }
}

fn format_update_message(current: &str, latest: &str) -> String {
    format!(
        "Update available: v{} → {} — run: curl -fsSL https://raw.githubusercontent.com/{}/main/install.sh | sh",
        current, latest, REPO
    )
}

/// Self-update: download and replace the running binary with the latest release.
pub async fn self_update() -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    eprintln!("Current version: v{}", current);
    eprintln!("Checking for updates...");

    let latest = fetch_latest_tag().await?;
    write_cache(&latest);

    if !is_newer(current, &latest) {
        eprintln!("Already up to date (v{}).", current);
        return Ok(());
    }

    eprintln!("New version available: v{} → {}", current, latest);

    let target = detect_target()?;
    let url = format!(
        "https://github.com/{}/releases/download/{}/pipit-{}-{}.tar.gz",
        REPO, latest, latest, target
    );

    eprintln!("Downloading {}...", url);

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    let resp = client
        .get(&url)
        .header("User-Agent", "pipit-self-update")
        .send()
        .await
        .context("Failed to download release")?;

    if !resp.status().is_success() {
        anyhow::bail!(
            "Download failed: HTTP {} — check that release {} exists for {}",
            resp.status(),
            latest,
            target
        );
    }

    let bytes = resp.bytes().await.context("Failed to read download")?;

    // Extract to a temp directory
    let tmpdir = tempfile::tempdir().context("Failed to create temp dir")?;
    let archive_path = tmpdir.path().join("pipit.tar.gz");
    std::fs::write(&archive_path, &bytes).context("Failed to write archive")?;

    // Use tar to extract
    let output = std::process::Command::new("tar")
        .args([
            "xzf",
            archive_path.to_str().unwrap(),
            "-C",
            tmpdir.path().to_str().unwrap(),
        ])
        .output()
        .context("Failed to run tar")?;

    if !output.status.success() {
        anyhow::bail!(
            "tar extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Find the pipit binary in the extracted files
    let new_binary = find_binary_in_dir(tmpdir.path())?;

    // Get current binary path
    let current_binary = std::env::current_exe().context("Cannot determine current binary path")?;
    let current_binary = current_binary.canonicalize().unwrap_or(current_binary);

    eprintln!("Replacing {}...", current_binary.display());

    // Atomic-ish replace: rename old, move new, delete old
    let backup = current_binary.with_extension("old");
    if backup.exists() {
        let _ = std::fs::remove_file(&backup);
    }

    // On Unix, we can replace even while running (inode stays valid)
    std::fs::rename(&current_binary, &backup)
        .context("Failed to back up current binary (try with sudo?)")?;

    match std::fs::copy(&new_binary, &current_binary) {
        Ok(_) => {
            // Set executable permissions
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &current_binary,
                    std::fs::Permissions::from_mode(0o755),
                );
            }
            let _ = std::fs::remove_file(&backup);
            eprintln!("\x1b[32m✓\x1b[0m Updated to {}", latest);
            Ok(())
        }
        Err(e) => {
            // Rollback
            let _ = std::fs::rename(&backup, &current_binary);
            Err(e).context("Failed to install new binary (rolled back)")
        }
    }
}

fn detect_target() -> Result<String> {
    let os = if cfg!(target_os = "linux") {
        "unknown-linux-gnu"
    } else if cfg!(target_os = "macos") {
        "apple-darwin"
    } else {
        anyhow::bail!("Unsupported OS for self-update");
    };

    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        anyhow::bail!("Unsupported architecture for self-update");
    };

    Ok(format!("{}-{}", arch, os))
}

fn find_binary_in_dir(dir: &std::path::Path) -> Result<PathBuf> {
    for entry in walkdir(dir)? {
        if let Some(name) = entry.file_name().and_then(|n| n.to_str()) {
            if name == "pipit" && entry.is_file() {
                return Ok(entry);
            }
        }
    }
    anyhow::bail!("pipit binary not found in release archive")
}

/// Simple recursive directory walk (avoids adding walkdir crate).
fn walkdir(dir: &std::path::Path) -> Result<Vec<PathBuf>> {
    let mut results = Vec::new();
    for entry in std::fs::read_dir(dir).context("Failed to read directory")? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            results.extend(walkdir(&path)?);
        } else {
            results.push(path);
        }
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("v0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("invalid"), None);
    }

    #[test]
    fn test_is_newer() {
        assert!(is_newer("0.1.0", "v0.2.0"));
        assert!(is_newer("0.1.0", "v0.1.1"));
        assert!(is_newer("0.1.0", "v1.0.0"));
        assert!(!is_newer("0.1.0", "v0.1.0"));
        assert!(!is_newer("0.2.0", "v0.1.0"));
    }
}
