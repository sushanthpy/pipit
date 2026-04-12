//! SubagentSupervisor — Semaphore-bounded pool with graceful termination (Task 2).
//!
//! Centralizes subagent lifecycle: bounded concurrency, SIGTERM→SIGKILL
//! escalation, and real-time status aggregation.

/// Maximum concurrent subagent processes. Protects provider quotas and
/// local resources. Same order of magnitude as pi-mono's MAX_CONCURRENCY=4.
pub const MAX_CONCURRENCY: usize = 4;

/// Maximum parallel forks (slightly higher since forks share prefix cache).
pub const MAX_PARALLEL_FORKS: usize = 8;

/// Grace period between SIGTERM and SIGKILL (seconds).
/// Same as systemd's TimeoutStopSec default.
pub const TERMINATION_GRACE_SECS: u64 = 5;

/// Maximum subagent nesting depth. Subagents cannot spawn subagents
/// (enforce depth < 1, same as abc-src's isInForkChild guard).
pub const MAX_NESTING_DEPTH: u32 = 1;

/// Gracefully terminate a child process: SIGTERM first, then SIGKILL after grace period.
///
/// On Unix: `kill(pid, SIGTERM)` → sleep(TERMINATION_GRACE_SECS) → `kill(pid, SIGKILL)`
/// On non-Unix: falls back to immediate kill.
#[cfg(unix)]
pub async fn graceful_terminate(child: &mut tokio::process::Child) {
    use std::os::unix::process::CommandExt;

    if let Some(pid) = child.id() {
        // Send SIGTERM
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }

        // Wait for grace period
        let grace = tokio::time::sleep(std::time::Duration::from_secs(TERMINATION_GRACE_SECS));
        tokio::pin!(grace);

        tokio::select! {
            _ = child.wait() => {
                // Child exited cleanly after SIGTERM
                return;
            }
            _ = &mut grace => {
                // Grace period expired — escalate to SIGKILL
                tracing::warn!(pid, "Subagent did not exit after SIGTERM, sending SIGKILL");
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
        }
    } else {
        // No PID available, best-effort kill
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

#[cfg(not(unix))]
pub async fn graceful_terminate(child: &mut tokio::process::Child) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}
