//! Cron-expression scheduler with durable next-fire persistence.
//!
//! Parses 5-field cron expressions, computes next fire time,
//! and submits tasks on schedule via the same TaskSink as channels.

use crate::config::{ChannelConfig, ScheduleConfig};
use crate::store::DaemonStore;

use anyhow::Result;
use chrono::{DateTime, Utc};
use pipit_channel::{MessageOrigin, NormalizedTask, TaskPriority, TaskSink};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing;

// ---------------------------------------------------------------------------
// Cron scheduler
// ---------------------------------------------------------------------------

pub struct CronScheduler {
    schedules: Vec<CronEntry>,
    store: Arc<DaemonStore>,
    sink: TaskSink,
    cancel: CancellationToken,
}

struct CronEntry {
    name: String,
    schedule: cron::Schedule,
    project: String,
    prompt: String,
    priority: TaskPriority,
    notify_origin: Option<MessageOrigin>,
}

impl CronScheduler {
    pub fn new(
        config: &HashMap<String, ScheduleConfig>,
        channels: &HashMap<String, ChannelConfig>,
        store: Arc<DaemonStore>,
        sink: TaskSink,
        cancel: CancellationToken,
    ) -> Self {
        let mut schedules = Vec::new();

        for (name, sched_config) in config {
            // Parse cron expression (add seconds field "0" as prefix for the cron crate)
            let cron_expr = format!("0 {}", sched_config.cron);
            match cron::Schedule::from_str(&cron_expr) {
                Ok(schedule) => {
                    let notify_origin = sched_config.notify_channel.as_ref().and_then(|ch_name| {
                        // Resolve notification channel to a default origin
                        // This would need the channel registry in a full implementation
                        None // Placeholder
                    });

                    schedules.push(CronEntry {
                        name: name.clone(),
                        schedule,
                        project: sched_config.project.clone(),
                        prompt: sched_config.prompt.clone(),
                        priority: sched_config.priority.unwrap_or(TaskPriority::Low),
                        notify_origin,
                    });

                    tracing::info!(
                        schedule = %name,
                        cron = %sched_config.cron,
                        project = %sched_config.project,
                        "cron schedule registered"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        schedule = %name,
                        cron = %sched_config.cron,
                        error = %e,
                        "invalid cron expression, skipping"
                    );
                }
            }
        }

        Self {
            schedules,
            store,
            sink,
            cancel,
        }
    }

    /// Spawn the cron scheduler as a background task.
    pub fn spawn(&self) -> tokio::task::JoinHandle<()> {
        // This is a simplified single-task approach. A production implementation
        // would use a min-heap of fire times for O(log N) next-schedule finding.

        let schedules: Vec<(
            String,
            cron::Schedule,
            String,
            String,
            TaskPriority,
            Option<MessageOrigin>,
        )> = self
            .schedules
            .iter()
            .map(|e| {
                (
                    e.name.clone(),
                    e.schedule.clone(),
                    e.project.clone(),
                    e.prompt.clone(),
                    e.priority,
                    e.notify_origin.clone(),
                )
            })
            .collect();

        let store = self.store.clone();
        let sink = self.sink.clone();
        let cancel = self.cancel.clone();

        tokio::spawn(async move {
            if schedules.is_empty() {
                tracing::info!("no cron schedules configured");
                cancel.cancelled().await;
                return;
            }

            loop {
                // Find earliest next fire across all schedules
                let now = Utc::now();
                let mut earliest: Option<(usize, DateTime<Utc>)> = None;

                for (i, (name, schedule, _, _, _, _)) in schedules.iter().enumerate() {
                    // Check if we have a persisted next_fire
                    let next = match store.get_cron_next_fire(name) {
                        Ok(Some(t)) if t > now => t,
                        _ => {
                            // Compute from schedule
                            match schedule.upcoming(Utc).next() {
                                Some(t) => {
                                    let _ = store.set_cron_next_fire(name, t);
                                    t
                                }
                                None => continue,
                            }
                        }
                    };

                    match &earliest {
                        None => earliest = Some((i, next)),
                        Some((_, current_earliest)) if next < *current_earliest => {
                            earliest = Some((i, next));
                        }
                        _ => {}
                    }
                }

                let (idx, fire_at) = match earliest {
                    Some(v) => v,
                    None => {
                        // No schedules have upcoming fires
                        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                        continue;
                    }
                };

                let wait_duration = (fire_at - now)
                    .to_std()
                    .unwrap_or(std::time::Duration::ZERO);

                tracing::debug!(
                    schedule = %schedules[idx].0,
                    fire_at = %fire_at,
                    wait_secs = wait_duration.as_secs(),
                    "sleeping until next cron fire"
                );

                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::info!("cron scheduler shutting down");
                        break;
                    }
                    _ = tokio::time::sleep(wait_duration) => {
                        let (name, schedule, project, prompt, priority, notify_origin) =
                            &schedules[idx];

                        tracing::info!(
                            schedule = %name,
                            project = %project,
                            "cron firing"
                        );

                        // Record last fire
                        let _ = store.set_cron_last_fire(name, Utc::now());

                        // Compute and persist next fire
                        if let Some(next) = schedule.upcoming(Utc).next() {
                            let _ = store.set_cron_next_fire(name, next);
                        }

                        // Submit task
                        let origin = MessageOrigin::Cron {
                            schedule_name: name.clone(),
                            notification_origin: notify_origin.clone().map(Box::new),
                        };

                        let task = NormalizedTask::new(
                            project.clone(),
                            prompt.clone(),
                            origin,
                        )
                        .with_priority(*priority);

                        if let Err(e) = sink.send(task).await {
                            tracing::error!(
                                schedule = %name,
                                error = %e,
                                "failed to submit cron task"
                            );
                        }
                    }
                }
            }
        })
    }

    /// Persist all cron state to the store (called during shutdown).
    pub fn persist_state(&self, store: &DaemonStore) -> Result<()> {
        for entry in &self.schedules {
            if let Some(next) = entry.schedule.upcoming(Utc).next() {
                store.set_cron_next_fire(&entry.name, next)?;
            }
        }
        Ok(())
    }
}
