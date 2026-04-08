//! Team memory synchronization worker.
//!
//! Bidirectional team memory sync using file watching (OS-native via `notify`)
//! for detecting local changes, `reqwest` for pushing to the daemon, and
//! periodic pull with last-writer-wins merge. Mandatory secret scanning
//! before any push.
//!
//! # Architecture
//!
//! ```text
//! ┌────────────────────┐     ┌──────────────┐
//! │  Local filesystem  │────→│  TeamSyncWkr │
//! │  .pipit/team/      │←────│  (background) │
//! └────────────────────┘     └──────┬───────┘
//!                                   │ push/pull
//!                            ┌──────▼───────┐
//!                            │ pipit-daemon  │
//!                            │ /api/teams/   │
//!                            └──────────────┘
//! ```
//!
//! File watching uses OS-native mechanisms:
//! - Linux: inotify (O(1) per event)
//! - macOS: FSEvents (O(1) per event)
//! - Windows: ReadDirectoryChangesW (O(1) per event)

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::secret_scanner;

/// Configuration for team sync.
#[derive(Debug, Clone)]
pub struct TeamSyncConfig {
    /// URL of the pipit-daemon (e.g. "http://localhost:3100").
    pub daemon_url: Option<String>,
    /// Team identifier.
    pub team_id: Option<String>,
    /// Interval between periodic pulls (default: 60s).
    pub sync_interval: Duration,
    /// Whether sync is enabled.
    pub enabled: bool,
}

impl Default for TeamSyncConfig {
    fn default() -> Self {
        Self {
            daemon_url: None,
            team_id: None,
            sync_interval: Duration::from_secs(60),
            enabled: false,
        }
    }
}

/// Background worker for bidirectional team memory sync.
///
/// - Watches `.pipit/team/` for local file changes
/// - On local change: secret-scan → push to daemon
/// - On timer tick: pull from daemon → merge into local
/// - On cancellation: graceful shutdown
pub struct TeamSyncWorker {
    project_root: PathBuf,
    config: TeamSyncConfig,
    session_id: String,
    last_pull: Option<Instant>,
}

impl TeamSyncWorker {
    pub fn new(project_root: &Path, config: TeamSyncConfig, session_id: String) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            config,
            session_id,
            last_pull: None,
        }
    }

    /// Spawn the sync worker as a background tokio task.
    /// Returns a handle that can be used to cancel.
    pub fn spawn(self, cancel: tokio_util::sync::CancellationToken) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(e) = self.run(cancel).await {
                error!("TeamSyncWorker exited with error: {}", e);
            }
        })
    }

    /// Main run loop.
    async fn run(mut self, cancel: tokio_util::sync::CancellationToken) -> Result<(), String> {
        let team_dir = self.project_root.join(".pipit").join("team");
        if !team_dir.exists() {
            std::fs::create_dir_all(&team_dir)
                .map_err(|e| format!("Failed to create team dir: {}", e))?;
        }

        info!("TeamSyncWorker started for {}", team_dir.display());

        // Set up file watcher
        let (fs_tx, mut fs_rx) = mpsc::channel::<PathBuf>(32);
        let _watcher = self.setup_watcher(&team_dir, fs_tx)?;

        // Periodic pull interval
        let mut pull_interval = tokio::time::interval(self.config.sync_interval);
        pull_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        // Do initial pull
        if self.config.daemon_url.is_some() && self.config.team_id.is_some() {
            if let Err(e) = self.pull().await {
                warn!("Initial team sync pull failed: {}", e);
            }
        }

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!("TeamSyncWorker shutting down");
                    break;
                }

                Some(changed_path) = fs_rx.recv() => {
                    debug!("Team file changed: {}", changed_path.display());
                    // Read the changed file and attempt push
                    if let Err(e) = self.handle_local_change(&changed_path).await {
                        warn!("Failed to handle team file change: {}", e);
                    }
                }

                _ = pull_interval.tick() => {
                    if self.config.daemon_url.is_some() && self.config.team_id.is_some() {
                        if let Err(e) = self.pull().await {
                            debug!("Periodic team sync pull failed: {}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Set up the filesystem watcher.
    fn setup_watcher(
        &self,
        watch_dir: &Path,
        tx: mpsc::Sender<PathBuf>,
    ) -> Result<notify::RecommendedWatcher, String> {
        use notify::{Config, RecursiveMode, Watcher};

        let mut watcher = notify::RecommendedWatcher::new(
            move |res: Result<notify::Event, notify::Error>| {
                if let Ok(event) = res {
                    use notify::EventKind;
                    match event.kind {
                        EventKind::Create(_) | EventKind::Modify(_) => {
                            for path in event.paths {
                                // Only watch .toml and .md files
                                if let Some(ext) = path.extension() {
                                    if ext == "toml" || ext == "md" {
                                        let _ = tx.try_send(path);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            },
            Config::default().with_poll_interval(Duration::from_secs(2)),
        )
        .map_err(|e| format!("Failed to create file watcher: {}", e))?;

        watcher
            .watch(watch_dir, RecursiveMode::NonRecursive)
            .map_err(|e| format!("Failed to watch {}: {}", watch_dir.display(), e))?;

        info!("Watching {} for team changes", watch_dir.display());
        Ok(watcher)
    }

    /// Handle a local file change: secret-scan then push.
    async fn handle_local_change(&self, path: &Path) -> Result<(), String> {
        // Read the file content
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;

        // Secret scanning — NEVER push content containing secrets
        if secret_scanner::contains_secrets(&content) {
            warn!(
                "SECRET DETECTED in team file {}. NOT pushing to daemon. \
                 Remove secrets before syncing.",
                path.display()
            );
            return Err("Content contains secrets — push blocked".to_string());
        }

        // Push to daemon (best-effort)
        if let (Some(daemon_url), Some(team_id)) = (&self.config.daemon_url, &self.config.team_id) {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");

            let payload = serde_json::json!({
                "content": content,
                "file_name": file_name,
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "author": self.session_id,
            });

            let url = format!("{}/api/teams/{}/memory", daemon_url, team_id);
            match reqwest::Client::new()
                .post(&url)
                .json(&payload)
                .timeout(Duration::from_secs(10))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    info!("Pushed team file {} to daemon", file_name);
                }
                Ok(resp) => {
                    warn!("Daemon returned {} for team push", resp.status());
                }
                Err(e) => {
                    debug!("Daemon unreachable for team push: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Pull team memory from daemon and merge into local files.
    async fn pull(&mut self) -> Result<(), String> {
        let daemon_url = self
            .config
            .daemon_url
            .as_deref()
            .ok_or("No daemon URL configured")?;
        let team_id = self
            .config
            .team_id
            .as_deref()
            .ok_or("No team ID configured")?;

        let since = self
            .last_pull
            .map(|t| t.elapsed().as_secs().to_string())
            .unwrap_or_else(|| "0".to_string());

        let url = format!(
            "{}/api/teams/{}/memory?since={}",
            daemon_url, team_id, since
        );

        let resp = reqwest::Client::new()
            .get(&url)
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| format!("Pull request failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("Daemon returned {}", resp.status()));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("Failed to parse pull response: {}", e))?;

        // Process entries from daemon
        if let Some(entries) = body.get("entries").and_then(|v| v.as_array()) {
            let team_dir = self.project_root.join(".pipit").join("team");

            for entry in entries {
                let author = entry.get("author").and_then(|v| v.as_str()).unwrap_or("");
                // Skip our own entries
                if author == self.session_id {
                    continue;
                }

                let file_name = entry
                    .get("file_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("shared.toml");
                let content = entry.get("content").and_then(|v| v.as_str()).unwrap_or("");

                if content.is_empty() {
                    continue;
                }

                // Secret scan incoming content too
                if secret_scanner::contains_secrets(content) {
                    warn!(
                        "SECRET DETECTED in pulled team entry from {}. Skipping.",
                        author
                    );
                    continue;
                }

                let local_path = team_dir.join(file_name);

                if file_name.ends_with(".toml") {
                    // For TOML: merge by key (last-writer-wins per key)
                    self.merge_toml(&local_path, content)?;
                } else {
                    // For markdown: append new content
                    let existing = std::fs::read_to_string(&local_path).unwrap_or_default();
                    if !existing.contains(content.trim()) {
                        let merged = format!("{}\n\n{}", existing.trim(), content.trim());
                        std::fs::write(&local_path, merged).map_err(|e| {
                            format!("Failed to write {}: {}", local_path.display(), e)
                        })?;
                    }
                }

                info!("Merged team entry from {} into {}", author, file_name);
            }
        }

        self.last_pull = Some(Instant::now());
        Ok(())
    }

    /// Merge TOML content using last-writer-wins per key.
    fn merge_toml(&self, local_path: &Path, remote_content: &str) -> Result<(), String> {
        let local_content = std::fs::read_to_string(local_path).unwrap_or_default();

        let mut local: toml::Value = local_content
            .parse()
            .unwrap_or(toml::Value::Table(toml::map::Map::new()));
        let remote: toml::Value = remote_content
            .parse()
            .map_err(|e| format!("Failed to parse remote TOML: {}", e))?;

        // Merge: remote keys override local (last-writer-wins)
        if let (toml::Value::Table(local_table), toml::Value::Table(remote_table)) =
            (&mut local, &remote)
        {
            for (section_key, remote_section) in remote_table {
                if let (
                    Some(toml::Value::Table(local_section)),
                    toml::Value::Table(remote_entries),
                ) = (local_table.get_mut(section_key), remote_section)
                {
                    for (key, value) in remote_entries {
                        local_section.insert(key.clone(), value.clone());
                    }
                } else {
                    local_table.insert(section_key.clone(), remote_section.clone());
                }
            }
        }

        let merged_content = toml::to_string_pretty(&local)
            .map_err(|e| format!("Failed to serialize merged TOML: {}", e))?;
        std::fs::write(local_path, merged_content)
            .map_err(|e| format!("Failed to write {}: {}", local_path.display(), e))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_team_sync_config_default() {
        let config = TeamSyncConfig::default();
        assert!(!config.enabled);
        assert!(config.daemon_url.is_none());
        assert_eq!(config.sync_interval, Duration::from_secs(60));
    }

    #[test]
    fn test_merge_toml_last_writer_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared.toml");

        // Write initial local content
        std::fs::write(&path, "[shared]\nfoo = \"local\"\nbar = \"local_only\"\n").unwrap();

        let worker = TeamSyncWorker::new(
            dir.path(),
            TeamSyncConfig::default(),
            "test-session".to_string(),
        );

        // Remote has foo with different value and a new key
        let remote = "[shared]\nfoo = \"remote\"\nbaz = \"remote_new\"\n";
        worker.merge_toml(&path, remote).unwrap();

        let result = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = result.parse().unwrap();
        let shared = parsed.get("shared").unwrap().as_table().unwrap();

        assert_eq!(shared.get("foo").unwrap().as_str().unwrap(), "remote"); // LWW
        assert_eq!(shared.get("bar").unwrap().as_str().unwrap(), "local_only"); // preserved
        assert_eq!(shared.get("baz").unwrap().as_str().unwrap(), "remote_new"); // new
    }

    #[test]
    fn test_secret_scan_blocks_push() {
        // Verify that content with secrets is detected
        assert!(secret_scanner::contains_secrets(
            "my key is sk-ant-api03-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMN1234567890"
        ));
        assert!(!secret_scanner::contains_secrets(
            "normal team convention text"
        ));
    }
}
