//! Pipit Daemon — 24/7 Remote Coding Agent
//!
//! Binary entry point. Orchestrates config loading, SochDB store,
//! channel registry, agent pool, task queue, reporter, and daemon lifecycle.

mod channels;
mod ci_fix;
mod config;
mod cron;
mod forge_github;
mod git;
mod health_monitor;
mod messaging_slack;
mod observability;
mod pipeline;
mod pool;
mod queue;
mod reporter;
mod runner;
mod server;
mod store;
mod teams_store;
mod triggers;
mod worker;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Load and validate config
    let config_path = config::resolve_config_path();
    let raw_toml = tokio::fs::read_to_string(&config_path)
        .await
        .map_err(|e| anyhow::anyhow!("failed to read config at {}: {}", config_path.display(), e))?;
    let daemon_config = config::DaemonConfig::from_toml_str(&raw_toml)?;
    daemon_config.validate()?;

    tracing::info!(
        projects = daemon_config.projects.len(),
        channels = daemon_config.channels.len(),
        schedules = daemon_config.schedules.len(),
        "configuration loaded from {}",
        config_path.display()
    );

    // Run the daemon
    runner::DaemonRunner::new(daemon_config).run().await
}
