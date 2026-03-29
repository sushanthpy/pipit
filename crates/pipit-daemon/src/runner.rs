//! PID-guarded daemon lifecycle with 3-phase graceful shutdown.
//!
//! Phase 1: Stop accepting (cancel channels + queue)
//! Phase 2: Drain in-flight (wait for running agents with timeout)
//! Phase 3: Checkpoint + close store (serialize contexts, flush WAL)

use crate::channels;
use crate::config::DaemonConfig;
use crate::cron::CronScheduler;
use crate::pool::AgentPool;
use crate::queue::TaskQueue;
use crate::reporter::Reporter;
use crate::server;
use crate::store::DaemonStore;

use anyhow::{anyhow, Result};
use pipit_channel::{task_channel, ChannelRegistry};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing;

// ---------------------------------------------------------------------------
// PID file management
// ---------------------------------------------------------------------------

pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Acquire the PID file. Fails if another instance is running.
    /// Removes stale PID files from crashed processes.
    pub fn acquire(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if self.path.exists() {
            let content = std::fs::read_to_string(&self.path)?;
            if let Ok(pid) = content.trim().parse::<u32>() {
                // Check if process is still alive via kill -0
                if is_process_alive(pid) {
                    return Err(anyhow!(
                        "daemon already running (PID {}). Remove {} if stale.",
                        pid,
                        self.path.display()
                    ));
                }
                // Stale PID file — remove it
                tracing::warn!(pid, "removing stale PID file");
                std::fs::remove_file(&self.path)?;
            }
        }

        let pid = std::process::id();
        std::fs::write(&self.path, pid.to_string())?;
        tracing::info!(pid, path = %self.path.display(), "PID file acquired");
        Ok(())
    }

    /// Release the PID file.
    pub fn release(&self) -> Result<()> {
        if self.path.exists() {
            std::fs::remove_file(&self.path)?;
            tracing::info!(path = %self.path.display(), "PID file released");
        }
        Ok(())
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn is_process_alive(pid: u32) -> bool {
    // POSIX: kill(pid, 0) checks if process exists without sending a signal
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

// ---------------------------------------------------------------------------
// Systemd integration
// ---------------------------------------------------------------------------

struct SystemdNotifier;

impl SystemdNotifier {
    fn notify_ready() {
        if let Ok(sock) = std::env::var("NOTIFY_SOCKET") {
            let _ = Self::sd_notify(&sock, "READY=1");
            tracing::info!("systemd: READY=1");
        }
    }

    fn notify_stopping() {
        if let Ok(sock) = std::env::var("NOTIFY_SOCKET") {
            let _ = Self::sd_notify(&sock, "STOPPING=1");
        }
    }

    fn notify_status(status: &str) {
        if let Ok(sock) = std::env::var("NOTIFY_SOCKET") {
            let _ = Self::sd_notify(&sock, &format!("STATUS={status}"));
        }
    }

    fn sd_notify(socket_path: &str, message: &str) -> Result<()> {
        use std::os::unix::net::UnixDatagram;
        let socket = UnixDatagram::unbound()?;
        socket.send_to(message.as_bytes(), socket_path)?;
        Ok(())
    }

    /// Spawn watchdog heartbeat task if WATCHDOG_USEC is set.
    fn spawn_watchdog(cancel: CancellationToken) -> Option<tokio::task::JoinHandle<()>> {
        let watchdog_usec: u64 = std::env::var("WATCHDOG_USEC")
            .ok()
            .and_then(|v| v.parse().ok())?;

        let interval = std::time::Duration::from_micros(watchdog_usec / 2);
        let sock = std::env::var("NOTIFY_SOCKET").ok()?;

        Some(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        let _ = Self::sd_notify(&sock, "WATCHDOG=1");
                    }
                }
            }
        }))
    }
}

// ---------------------------------------------------------------------------
// DaemonRunner
// ---------------------------------------------------------------------------

pub struct DaemonRunner {
    config: DaemonConfig,
}

impl DaemonRunner {
    pub fn new(config: DaemonConfig) -> Self {
        Self { config }
    }

    pub async fn run(self) -> Result<()> {
        // Phase 0: Acquire PID file
        let pid_file = PidFile::new(self.config.daemon.pid_path.clone());
        pid_file.acquire()?;

        // Open store
        let store = Arc::new(DaemonStore::open(&self.config.daemon.store_path)?);
        tracing::info!(
            path = %self.config.daemon.store_path.display(),
            keys = store.key_count(),
            "store opened"
        );

        // Master cancellation token
        let cancel = CancellationToken::new();

        // Task submission channel
        let (task_tx, task_rx) = task_channel(self.config.daemon.max_queue_depth);

        // Build agent pool
        let pool = Arc::new(AgentPool::new(store.clone(), &self.config)?);

        // Build task queue
        let queue = Arc::new(TaskQueue::new(
            store.clone(),
            pool.clone(),
            self.config.daemon.max_concurrent,
            self.config.daemon.max_queue_depth,
        ));

        // Register channels
        let channel_registry = Arc::new(ChannelRegistry::new());
        channels::register_channels(
            &self.config,
            &channel_registry,
            task_tx.clone(),
            cancel.clone(),
        )
        .await?;

        // Build reporter (with channel registry for delivery)
        let reporter = Arc::new(Reporter::new(store.clone(), channel_registry.clone()));

        // Start cron scheduler
        let cron = CronScheduler::new(
            &self.config.schedules,
            &self.config.channels,
            store.clone(),
            task_tx.clone(),
            cancel.clone(),
        );
        let cron_handle = cron.spawn();

        // Start HTTP API server
        let server_handle = server::spawn_server(
            &self.config.server,
            &self.config.auth,
            queue.clone(),
            pool.clone(),
            store.clone(),
            reporter.clone(),
            cancel.clone(),
        )
        .await?;

        // Start queue processor
        let queue_cancel = cancel.clone();
        let queue_ref = queue.clone();
        let reporter_ref = reporter.clone();
        let queue_handle = tokio::spawn(async move {
            queue_ref
                .process_loop(task_rx, reporter_ref, queue_cancel)
                .await;
        });

        // Systemd notifications
        SystemdNotifier::notify_ready();
        SystemdNotifier::notify_status("running");
        let watchdog_handle = SystemdNotifier::spawn_watchdog(cancel.clone());

        tracing::info!(
            bind = %self.config.server.bind,
            port = self.config.server.port,
            projects = self.config.projects.len(),
            "daemon ready"
        );

        // Wait for shutdown signal
        tokio::select! {
            _ = signal::ctrl_c() => {
                tracing::info!("received SIGINT, initiating shutdown");
            }
            _ = cancel.cancelled() => {
                tracing::info!("cancellation token triggered, initiating shutdown");
            }
        }

        // ===================================================================
        // 3-phase graceful shutdown
        // ===================================================================

        SystemdNotifier::notify_stopping();

        // Phase 1: Stop accepting
        tracing::info!("shutdown phase 1: stop accepting");
        cancel.cancel(); // Stops all channel loops, cron, HTTP listener

        // Phase 2: Drain in-flight
        tracing::info!(
            timeout_secs = self.config.daemon.drain_timeout_secs,
            "shutdown phase 2: drain in-flight"
        );
        let drain_timeout =
            std::time::Duration::from_secs(self.config.daemon.drain_timeout_secs);

        tokio::select! {
            _ = tokio::time::sleep(drain_timeout) => {
                tracing::warn!("drain timeout expired, cancelling running tasks");
                pool.cancel_all();
            }
            _ = pool.wait_idle() => {
                tracing::info!("all tasks drained");
            }
        }

        // Phase 3: Checkpoint and close
        tracing::info!("shutdown phase 3: checkpoint and close");
        if let Err(e) = pool.checkpoint_all(&store) {
            tracing::error!(error = %e, "failed to checkpoint agent contexts");
        }

        // Persist cron state
        if let Err(e) = cron.persist_state(&store) {
            tracing::error!(error = %e, "failed to persist cron state");
        }

        // Flush store
        store.checkpoint()?;
        store.sync()?;
        tracing::info!("store flushed and synced");

        // Clean up
        if let Some(handle) = watchdog_handle {
            handle.abort();
        }
        drop(pid_file); // releases PID file

        tracing::info!("daemon shutdown complete");
        Ok(())
    }
}
