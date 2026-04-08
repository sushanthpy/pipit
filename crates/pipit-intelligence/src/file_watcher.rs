//! Ambient file watcher — monitors project for external changes (INTEL-2).
//!
//! Uses the `notify` crate (already a workspace dependency) to watch for:
//! - File modifications from external editors (VS Code, vim, etc.)
//! - Git operations (pull, checkout, merge)
//! - Package manager changes (npm install, cargo add)
//!
//! On detected changes:
//! 1. Incrementally update the RepoMap symbol graph
//! 2. Re-run taint analysis on modified files
//! 3. Notify TUI of stale context
//! 4. Suggest proactive actions

use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Events emitted by the file watcher for the TUI/agent to consume.
#[derive(Debug, Clone)]
pub enum WatchEvent {
    /// Files were modified externally.
    FilesChanged { paths: Vec<PathBuf> },
    /// New files were created.
    FilesCreated { paths: Vec<PathBuf> },
    /// Files were deleted.
    FilesDeleted { paths: Vec<PathBuf> },
    /// Proactive suggestion based on detected pattern.
    Suggestion { message: String },
}

/// Configuration for the file watcher.
#[derive(Debug, Clone)]
pub struct WatcherConfig {
    /// Debounce window in milliseconds (batch rapid saves).
    pub debounce_ms: u64,
    /// Watch for dependency manifest changes.
    pub watch_deps: bool,
    /// Watch for test file changes.
    pub watch_tests: bool,
    /// Watch for security-sensitive changes.
    pub watch_security: bool,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 200,
            watch_deps: true,
            watch_tests: true,
            watch_security: true,
        }
    }
}

/// Accumulates file events with debouncing.
struct EventAccumulator {
    changed: HashSet<PathBuf>,
    created: HashSet<PathBuf>,
    deleted: HashSet<PathBuf>,
    last_event: Instant,
}

impl EventAccumulator {
    fn new() -> Self {
        Self {
            changed: HashSet::new(),
            created: HashSet::new(),
            deleted: HashSet::new(),
            last_event: Instant::now(),
        }
    }

    fn push(&mut self, event: &Event) {
        self.last_event = Instant::now();
        for path in &event.paths {
            match event.kind {
                EventKind::Create(_) => {
                    self.created.insert(path.clone());
                }
                EventKind::Modify(_) => {
                    self.changed.insert(path.clone());
                }
                EventKind::Remove(_) => {
                    self.deleted.insert(path.clone());
                }
                _ => {}
            }
        }
    }

    fn drain(&mut self) -> Vec<WatchEvent> {
        let mut events = Vec::new();
        if !self.changed.is_empty() {
            events.push(WatchEvent::FilesChanged {
                paths: self.changed.drain().collect(),
            });
        }
        if !self.created.is_empty() {
            events.push(WatchEvent::FilesCreated {
                paths: self.created.drain().collect(),
            });
        }
        if !self.deleted.is_empty() {
            events.push(WatchEvent::FilesDeleted {
                paths: self.deleted.drain().collect(),
            });
        }
        events
    }

    fn is_ready(&self, debounce: Duration) -> bool {
        !self.changed.is_empty()
            || !self.created.is_empty()
            || !self.deleted.is_empty() && self.last_event.elapsed() > debounce
    }
}

/// Start the ambient file watcher.
/// Returns a receiver for watch events.
pub fn start_watcher(
    project_root: &Path,
    config: WatcherConfig,
) -> Result<(tokio::sync::mpsc::Receiver<WatchEvent>, WatcherHandle), String> {
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    let accumulator = Arc::new(Mutex::new(EventAccumulator::new()));

    let acc = accumulator.clone();
    let debounce = Duration::from_millis(config.debounce_ms);
    let root = project_root.to_path_buf();

    let mut watcher = RecommendedWatcher::new(
        move |event: Result<Event, notify::Error>| {
            if let Ok(event) = event {
                let mut acc = acc.lock().unwrap();
                acc.push(&event);
            }
        },
        Config::default(),
    )
    .map_err(|e| format!("Watcher init failed: {}", e))?;

    watcher
        .watch(project_root, RecursiveMode::Recursive)
        .map_err(|e| format!("Watch failed: {}", e))?;

    // Spawn debounce + suggestion generator
    let acc = accumulator.clone();
    let config_clone = config.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        loop {
            interval.tick().await;
            let events = {
                let mut acc = acc.lock().unwrap();
                if acc.is_ready(debounce) {
                    let mut events = acc.drain();
                    // Generate proactive suggestions
                    events.extend(generate_suggestions(&events, &config_clone));
                    events
                } else {
                    Vec::new()
                }
            };
            for event in events {
                if tx.send(event).await.is_err() {
                    return; // Channel closed
                }
            }
        }
    });

    Ok((rx, WatcherHandle { _watcher: watcher }))
}

/// Handle that keeps the watcher alive. Drop to stop watching.
pub struct WatcherHandle {
    _watcher: RecommendedWatcher,
}

/// Generate proactive suggestions based on detected changes.
fn generate_suggestions(events: &[WatchEvent], config: &WatcherConfig) -> Vec<WatchEvent> {
    let mut suggestions = Vec::new();
    for event in events {
        match event {
            WatchEvent::FilesChanged { paths } => {
                // Test file changed without implementation → suggest running tests
                if config.watch_tests {
                    let test_files: Vec<_> = paths
                        .iter()
                        .filter(|p| {
                            let name = p.file_name().unwrap_or_default().to_string_lossy();
                            name.contains("test")
                                || name.contains("spec")
                                || name.starts_with("test_")
                        })
                        .collect();
                    if !test_files.is_empty() {
                        suggestions.push(WatchEvent::Suggestion {
                            message: format!(
                                "{} test file(s) changed externally — run tests to check for regressions?",
                                test_files.len()
                            ),
                        });
                    }
                }
                // Dependency manifest changed → suggest dep check
                if config.watch_deps {
                    let dep_files: Vec<_> = paths
                        .iter()
                        .filter(|p| {
                            let name = p.file_name().unwrap_or_default().to_string_lossy();
                            matches!(
                                name.as_ref(),
                                "Cargo.toml"
                                    | "package.json"
                                    | "pyproject.toml"
                                    | "go.mod"
                                    | "Cargo.lock"
                                    | "package-lock.json"
                                    | "yarn.lock"
                            )
                        })
                        .collect();
                    if !dep_files.is_empty() {
                        suggestions.push(WatchEvent::Suggestion {
                            message: "Dependency manifest changed — run /deps to check for vulnerabilities?".to_string(),
                        });
                    }
                }
            }
            WatchEvent::FilesCreated { paths } => {
                // New .env.example → suggest checking env vars
                let env_files: Vec<_> = paths
                    .iter()
                    .filter(|p| {
                        let name = p.file_name().unwrap_or_default().to_string_lossy();
                        name.contains(".env")
                    })
                    .collect();
                if !env_files.is_empty() {
                    suggestions.push(WatchEvent::Suggestion {
                        message: "New environment file detected — check if all variables are set?"
                            .to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    suggestions
}
