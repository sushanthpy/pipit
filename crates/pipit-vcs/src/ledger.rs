//! # Repository Ledger
//!
//! Append-only event log for all VCS mutations. Provides linearizable workflow
//! history for replay, reconciliation, debugging, and multi-agent coordination.
//!
//! - Append: O(1) per event
//! - State reconstruction: O(N) in event count, O(S + Δ) with periodic snapshots
//! - Crash recovery via write-ahead append semantics

use crate::snapshot::SnapshotGraph;
use crate::workflow::{WorkflowPhase, WorkflowTransition};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// A single event in the repository ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Monotonically increasing sequence number.
    pub seq: u64,
    /// When this event occurred.
    pub timestamp: DateTime<Utc>,
    /// The event payload.
    pub event: LedgerEvent,
    /// Actor that triggered this event (session ID, agent ID, etc.)
    pub actor: String,
}

/// All possible repository events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LedgerEvent {
    /// Workspace created.
    WorkspaceCreated {
        workspace_id: String,
        branch: String,
        base_commit: String,
        objective: Option<String>,
    },
    /// Workflow phase transition.
    PhaseTransition {
        workspace_id: String,
        from: WorkflowPhase,
        to: WorkflowPhase,
        op: String,
    },
    /// Snapshot created.
    SnapshotCreated {
        workspace_id: String,
        snapshot_id: String,
        files_count: usize,
    },
    /// Verification run completed.
    VerificationCompleted {
        workspace_id: String,
        check_name: String,
        passed: bool,
        duration_ms: u64,
    },
    /// Contract created.
    ContractCreated {
        workspace_id: String,
        contract_id: String,
        objective: String,
    },
    /// Contract gate evaluated.
    GateEvaluated {
        workspace_id: String,
        gate_name: String,
        passed: bool,
    },
    /// Promotion executed (merge).
    PromotionExecuted {
        workspace_id: String,
        target_branch: String,
        merge_commit: Option<String>,
    },
    /// Workspace reconciled.
    WorkspaceReconciled {
        workspace_id: String,
        action: String,
        snapshot_id: Option<String>,
    },
    /// Conflict detected between workspaces.
    ConflictDetected {
        workspace_a: String,
        workspace_b: String,
        conflicting_files: Vec<String>,
    },
    /// Firewall blocked an operation.
    FirewallBlocked {
        workspace_id: String,
        operation: String,
        threat: String,
    },
    /// Generic annotation/note.
    Note {
        workspace_id: Option<String>,
        message: String,
    },
}

/// The append-only repository ledger.
pub struct RepositoryLedger {
    /// Path to the ledger file (JSONL format).
    path: PathBuf,
    /// Next sequence number.
    next_seq: u64,
    /// Default actor for events.
    actor: String,
}

impl RepositoryLedger {
    /// Create a new ledger at the given path.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            next_seq: 1,
            actor: "pipit".to_string(),
        }
    }

    /// Set the actor identity for subsequent events.
    pub fn set_actor(&mut self, actor: impl Into<String>) {
        self.actor = actor.into();
    }

    /// Append a workflow transition as a ledger event. O(1).
    pub fn append(&mut self, transition: &WorkflowTransition) -> Result<(), std::io::Error> {
        let entry = LedgerEntry {
            seq: self.next_seq,
            timestamp: transition.timestamp,
            event: LedgerEvent::PhaseTransition {
                workspace_id: transition.workspace_id.clone(),
                from: transition.from.clone(),
                to: transition.to.clone(),
                op: format!("{:?}", transition.op),
            },
            actor: self.actor.clone(),
        };
        self.write_entry(&entry)?;
        self.next_seq += 1;
        Ok(())
    }

    /// Append a raw event. O(1).
    pub fn append_event(&mut self, event: LedgerEvent) -> Result<(), std::io::Error> {
        let entry = LedgerEntry {
            seq: self.next_seq,
            timestamp: Utc::now(),
            event,
            actor: self.actor.clone(),
        };
        self.write_entry(&entry)?;
        self.next_seq += 1;
        Ok(())
    }

    /// Replay the ledger to reconstruct state. O(N).
    pub fn replay(
        &mut self,
        workspaces: &mut HashMap<String, WorkflowPhase>,
        _snapshots: &mut SnapshotGraph,
    ) -> Result<u64, std::io::Error> {
        if !self.path.exists() {
            return Ok(0);
        }

        let file = std::fs::File::open(&self.path)?;
        let reader = std::io::BufReader::new(file);
        let mut count = 0u64;

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let entry: LedgerEntry = match serde_json::from_str(&line) {
                Ok(e) => e,
                Err(_) => continue, // Skip malformed entries
            };

            match &entry.event {
                LedgerEvent::WorkspaceCreated { workspace_id, .. } => {
                    workspaces.insert(workspace_id.clone(), WorkflowPhase::Editing);
                }
                LedgerEvent::PhaseTransition {
                    workspace_id, to, ..
                } => {
                    workspaces.insert(workspace_id.clone(), to.clone());
                }
                _ => {}
            }

            self.next_seq = entry.seq + 1;
            count += 1;
        }

        Ok(count)
    }

    /// Read all entries from the ledger. O(N).
    pub fn read_all(&self) -> Result<Vec<LedgerEntry>, std::io::Error> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        let file = std::fs::File::open(&self.path)?;
        let reader = std::io::BufReader::new(file);
        let mut entries = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<LedgerEntry>(&line) {
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    /// Get entries for a specific workspace.
    pub fn workspace_history(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<LedgerEntry>, std::io::Error> {
        let all = self.read_all()?;
        Ok(all
            .into_iter()
            .filter(|e| match &e.event {
                LedgerEvent::WorkspaceCreated {
                    workspace_id: id, ..
                }
                | LedgerEvent::PhaseTransition {
                    workspace_id: id, ..
                }
                | LedgerEvent::SnapshotCreated {
                    workspace_id: id, ..
                }
                | LedgerEvent::VerificationCompleted {
                    workspace_id: id, ..
                }
                | LedgerEvent::ContractCreated {
                    workspace_id: id, ..
                }
                | LedgerEvent::GateEvaluated {
                    workspace_id: id, ..
                }
                | LedgerEvent::PromotionExecuted {
                    workspace_id: id, ..
                }
                | LedgerEvent::WorkspaceReconciled {
                    workspace_id: id, ..
                }
                | LedgerEvent::FirewallBlocked {
                    workspace_id: id, ..
                } => id == workspace_id,
                LedgerEvent::Note {
                    workspace_id: Some(id),
                    ..
                } => id == workspace_id,
                LedgerEvent::ConflictDetected {
                    workspace_a,
                    workspace_b,
                    ..
                } => workspace_a == workspace_id || workspace_b == workspace_id,
                _ => false,
            })
            .collect())
    }

    /// Total number of events in the ledger.
    pub fn len(&self) -> u64 {
        self.next_seq.saturating_sub(1)
    }

    /// Whether the ledger is empty.
    pub fn is_empty(&self) -> bool {
        self.next_seq <= 1
    }

    /// Write a single entry to the ledger file.
    fn write_entry(&self, entry: &LedgerEntry) -> Result<(), std::io::Error> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        let json = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        writeln!(file, "{}", json)?;
        file.flush()?;
        Ok(())
    }
}
