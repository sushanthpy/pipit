//! Task #11: Rule feedback via reactive file watching.
//!
//! Rule file changes are detected via `notify` and propagated as events.
//! Consumers (RuleRegistry, budget, plan gate) see updates immediately.

use crate::rule::RuleId;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Events emitted when rule files change.
#[derive(Debug, Clone)]
pub enum RuleFileEvent {
    /// A rule file was created or modified.
    Changed { path: PathBuf },
    /// A rule file was removed.
    Removed { path: PathBuf },
}

/// Debounced rule file watcher.
///
/// Watches rule directories for changes and emits debounced events.
/// Debounce window: 300ms to coalesce editor save bursts.
pub struct RuleWatcher {
    _watcher: RecommendedWatcher,
    rx: mpsc::Receiver<RuleFileEvent>,
}

impl RuleWatcher {
    /// Create a watcher on the given rule directories.
    /// Returns `None` if no directories exist or watcher creation fails.
    pub fn new(rule_dirs: &[PathBuf]) -> Option<Self> {
        let (tx, rx) = mpsc::channel();

        // Internal channel for raw notify events.
        let (raw_tx, raw_rx) = mpsc::channel::<notify::Result<Event>>();

        let mut watcher = RecommendedWatcher::new(
            move |res| {
                let _ = raw_tx.send(res);
            },
            notify::Config::default().with_poll_interval(Duration::from_secs(2)),
        )
        .ok()?;

        let mut watched_any = false;
        for dir in rule_dirs {
            if dir.exists() {
                if watcher.watch(dir, RecursiveMode::Recursive).is_ok() {
                    watched_any = true;
                }
            }
        }

        if !watched_any {
            return None;
        }

        // Spawn debounce thread.
        let debounce_tx = tx;
        std::thread::spawn(move || {
            let debounce_window = Duration::from_millis(300);
            let mut pending: Vec<(PathBuf, bool)> = Vec::new(); // (path, is_remove)
            let mut last_event = Instant::now();

            loop {
                match raw_rx.recv_timeout(debounce_window) {
                    Ok(Ok(event)) => {
                        for path in &event.paths {
                            if is_rule_file(path) {
                                let is_remove = matches!(
                                    event.kind,
                                    EventKind::Remove(_)
                                );
                                pending.push((path.clone(), is_remove));
                                last_event = Instant::now();
                            }
                        }
                    }
                    Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Timeout) => {
                        if !pending.is_empty() && last_event.elapsed() >= debounce_window {
                            // Flush debounced events.
                            for (path, is_remove) in pending.drain(..) {
                                let event = if is_remove {
                                    RuleFileEvent::Removed { path }
                                } else {
                                    RuleFileEvent::Changed { path }
                                };
                                if debounce_tx.send(event).is_err() {
                                    return; // Receiver dropped.
                                }
                            }
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
        });

        Some(Self {
            _watcher: watcher,
            rx,
        })
    }

    /// Drain all pending events (non-blocking).
    pub fn drain_events(&self) -> Vec<RuleFileEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.rx.try_recv() {
            events.push(event);
        }
        events
    }
}

fn is_rule_file(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext == "md" || ext == "yaml" || ext == "yml")
}
