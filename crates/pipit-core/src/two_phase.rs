//! Verification-Gated Two-Phase Commit (Architecture Task 6)
//!
//! Every mutating run is a two-phase commit:
//! Phase 1: speculative edits into a checkpointed workspace
//! Phase 2: verifier/governor evaluates proof, tests, lint, policy
//!
//! Acceptance condition: P(correct | evidence) ≥ θ AND risk ≤ ρ
//! This turns verification from advisory to transactional.

use crate::governor::RiskReport;
use crate::pev::Verdict;
use crate::proof::{ConfidenceReport, EvidenceArtifact, RealizedEdit};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ─── Two-Phase Commit State Machine ─────────────────────────────────────

/// States of the verification-gated commit protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommitPhase {
    /// No active transaction.
    Idle,
    /// Phase 1: speculative edits are being applied.
    Speculative,
    /// Phase 1 complete: awaiting verification.
    PendingVerification,
    /// Phase 2: verification is running.
    Verifying,
    /// Verification passed: ready to commit.
    ReadyToCommit,
    /// Committed: changes are permanent.
    Committed,
    /// Rolled back: changes were reverted.
    RolledBack { reason: String },
    /// Repair loop: attempting fixes before re-verification.
    Repairing { attempt: u32, max_attempts: u32 },
}

/// Commit policy thresholds.
#[derive(Debug, Clone)]
pub struct CommitPolicy {
    /// Minimum confidence for auto-commit (θ).
    pub confidence_threshold: f32,
    /// Maximum risk score for auto-commit (ρ).
    pub risk_threshold: f32,
    /// Maximum repair attempts before rollback.
    pub max_repairs: u32,
    /// Whether to require all verification checks to pass.
    pub require_all_checks: bool,
    /// Whether to auto-commit when thresholds are met.
    pub auto_commit: bool,
}

impl Default for CommitPolicy {
    fn default() -> Self {
        Self {
            confidence_threshold: 0.7,
            risk_threshold: 0.5,
            max_repairs: 2,
            require_all_checks: false,
            auto_commit: false,
        }
    }
}

/// The checkpoint state for rollback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionCheckpoint {
    /// Unique checkpoint identifier.
    pub id: String,
    /// Git commit hash (if in a git repo).
    pub git_ref: Option<String>,
    /// Files that were modified in this transaction.
    pub modified_files: Vec<String>,
    /// File snapshots for non-git rollback.
    pub file_snapshots: Vec<FileSnapshot>,
    /// Timestamp.
    pub created_at: u64,
}

/// File snapshot for rollback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSnapshot {
    pub path: String,
    pub content: String,
    pub existed: bool,
}

/// Result of the commit evaluation.
#[derive(Debug, Clone)]
pub enum CommitDecision {
    /// All checks pass: commit the changes.
    Commit { confidence: f32, risk: f32 },
    /// Checks failed but repairable: enter repair loop.
    Repair {
        findings: Vec<String>,
        attempt: u32,
    },
    /// Unrecoverable failure: rollback.
    Rollback { reason: String },
    /// Cannot determine: ask user.
    Ask {
        confidence: f32,
        risk: f32,
        findings: Vec<String>,
    },
}

// ─── Transaction Controller ─────────────────────────────────────────────

/// The two-phase commit controller.
pub struct TransactionController {
    phase: CommitPhase,
    policy: CommitPolicy,
    checkpoint: Option<TransactionCheckpoint>,
    /// Evidence collected during execution.
    evidence: Vec<EvidenceArtifact>,
    /// Edits applied during this transaction.
    edits: Vec<RealizedEdit>,
    /// Verification results.
    verification_checks: Vec<VerificationCheck>,
    /// Repair attempt counter.
    repair_count: u32,
}

/// A single verification check result.
#[derive(Debug, Clone)]
pub struct VerificationCheck {
    pub name: String,
    pub kind: VerificationCheckKind,
    pub passed: bool,
    pub output: String,
    pub critical: bool,
}

#[derive(Debug, Clone)]
pub enum VerificationCheckKind {
    UnitTest,
    Lint,
    TypeCheck,
    IntegrationTest,
    RuntimeProbe,
    PolicyConstraint,
    LlmVerifier,
}

impl TransactionController {
    pub fn new(policy: CommitPolicy) -> Self {
        Self {
            phase: CommitPhase::Idle,
            policy,
            checkpoint: None,
            evidence: Vec::new(),
            edits: Vec::new(),
            verification_checks: Vec::new(),
            repair_count: 0,
        }
    }

    /// Begin a speculative transaction.
    pub fn begin(&mut self, project_root: &Path) -> Result<(), String> {
        if self.phase != CommitPhase::Idle {
            return Err(format!(
                "Cannot begin transaction in phase {:?}",
                self.phase
            ));
        }

        // Create checkpoint
        let checkpoint_id = format!("txn-{}", current_timestamp());
        let git_ref = get_git_head(project_root);

        self.checkpoint = Some(TransactionCheckpoint {
            id: checkpoint_id,
            git_ref,
            modified_files: Vec::new(),
            file_snapshots: Vec::new(),
            created_at: current_timestamp(),
        });

        self.phase = CommitPhase::Speculative;
        self.evidence.clear();
        self.edits.clear();
        self.verification_checks.clear();
        self.repair_count = 0;

        Ok(())
    }

    /// Record a file snapshot before mutation (during speculative phase).
    pub fn snapshot_before_edit(&mut self, path: &Path) -> Result<(), String> {
        if self.phase != CommitPhase::Speculative
            && !matches!(self.phase, CommitPhase::Repairing { .. })
        {
            return Err("Can only snapshot during speculative/repair phase".to_string());
        }

        let content = if path.exists() {
            std::fs::read_to_string(path).unwrap_or_default()
        } else {
            String::new()
        };

        if let Some(ref mut checkpoint) = self.checkpoint {
            let path_str = path.display().to_string();
            if !checkpoint.modified_files.contains(&path_str) {
                checkpoint.modified_files.push(path_str.clone());
                checkpoint.file_snapshots.push(FileSnapshot {
                    path: path_str,
                    content,
                    existed: path.exists(),
                });
            }
        }

        Ok(())
    }

    /// Record a completed edit.
    pub fn record_edit(&mut self, edit: RealizedEdit) {
        self.edits.push(edit);
    }

    /// Record evidence.
    pub fn record_evidence(&mut self, evidence: EvidenceArtifact) {
        self.evidence.push(evidence);
    }

    /// Mark speculative phase as complete, ready for verification.
    pub fn mark_pending_verification(&mut self) {
        if self.phase == CommitPhase::Speculative
            || matches!(self.phase, CommitPhase::Repairing { .. })
        {
            self.phase = CommitPhase::PendingVerification;
        }
    }

    /// Run verification and make the commit decision.
    pub fn evaluate(
        &mut self,
        confidence: &ConfidenceReport,
        risk: &RiskReport,
        verdict: Option<&Verdict>,
    ) -> CommitDecision {
        self.phase = CommitPhase::Verifying;

        let conf_score = confidence.overall();
        let risk_score = risk.score;

        // Check verification results
        let critical_failures: Vec<String> = self
            .verification_checks
            .iter()
            .filter(|c| c.critical && !c.passed)
            .map(|c| format!("{}: {}", c.name, c.output))
            .collect();

        let all_checks_pass = self
            .verification_checks
            .iter()
            .all(|c| c.passed || !c.critical);

        // Check LLM verifier verdict
        let verifier_pass = match verdict {
            Some(Verdict::Pass) => true,
            Some(Verdict::Repairable) => false,
            Some(Verdict::Fail) => {
                self.phase = CommitPhase::RolledBack {
                    reason: "Verifier verdict: Fail".to_string(),
                };
                return CommitDecision::Rollback {
                    reason: "Verifier determined fundamental failure".to_string(),
                };
            }
            Some(Verdict::Inconclusive) => false,
            None => true, // No verifier = assume pass
        };

        // Evaluate against policy thresholds
        if conf_score >= self.policy.confidence_threshold
            && risk_score <= self.policy.risk_threshold
            && (all_checks_pass || !self.policy.require_all_checks)
            && verifier_pass
        {
            self.phase = CommitPhase::ReadyToCommit;
            if self.policy.auto_commit {
                self.phase = CommitPhase::Committed;
            }
            return CommitDecision::Commit {
                confidence: conf_score,
                risk: risk_score,
            };
        }

        // Check if repairable
        if !critical_failures.is_empty() || !verifier_pass {
            if self.repair_count < self.policy.max_repairs {
                self.repair_count += 1;
                self.phase = CommitPhase::Repairing {
                    attempt: self.repair_count,
                    max_attempts: self.policy.max_repairs,
                };
                return CommitDecision::Repair {
                    findings: critical_failures,
                    attempt: self.repair_count,
                };
            }
        }

        // Below threshold but not clearly broken — ask
        if conf_score < self.policy.confidence_threshold
            || risk_score > self.policy.risk_threshold
        {
            return CommitDecision::Ask {
                confidence: conf_score,
                risk: risk_score,
                findings: critical_failures,
            };
        }

        CommitDecision::Rollback {
            reason: format!(
                "Verification failed after {} repair attempts",
                self.repair_count
            ),
        }
    }

    /// Commit the transaction (make changes permanent).
    pub fn commit(&mut self) -> Result<(), String> {
        if self.phase != CommitPhase::ReadyToCommit {
            return Err(format!("Cannot commit in phase {:?}", self.phase));
        }
        self.phase = CommitPhase::Committed;
        self.checkpoint = None; // Discard checkpoint
        Ok(())
    }

    /// Rollback the transaction (restore all files from checkpoint).
    pub fn rollback(&mut self) -> Result<Vec<String>, String> {
        let checkpoint = self
            .checkpoint
            .take()
            .ok_or("No checkpoint to rollback to")?;

        let mut restored = Vec::new();
        for snapshot in &checkpoint.file_snapshots {
            let path = Path::new(&snapshot.path);
            if snapshot.existed {
                std::fs::write(path, &snapshot.content).map_err(|e| {
                    format!("Failed to restore {}: {}", snapshot.path, e)
                })?;
            } else if path.exists() {
                std::fs::remove_file(path).map_err(|e| {
                    format!("Failed to remove {}: {}", snapshot.path, e)
                })?;
            }
            restored.push(snapshot.path.clone());
        }

        self.phase = CommitPhase::RolledBack {
            reason: "Explicit rollback".to_string(),
        };

        Ok(restored)
    }

    /// Add a verification check result.
    pub fn add_check(&mut self, check: VerificationCheck) {
        self.verification_checks.push(check);
    }

    /// Current phase.
    pub fn phase(&self) -> &CommitPhase {
        &self.phase
    }

    /// Whether a transaction is active.
    pub fn is_active(&self) -> bool {
        matches!(
            self.phase,
            CommitPhase::Speculative
                | CommitPhase::PendingVerification
                | CommitPhase::Verifying
                | CommitPhase::Repairing { .. }
        )
    }
}

fn get_git_head(project_root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(project_root)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_decision_flow() {
        let mut ctrl = TransactionController::new(CommitPolicy {
            confidence_threshold: 0.7,
            risk_threshold: 0.5,
            max_repairs: 2,
            require_all_checks: false,
            auto_commit: false,
        });

        // Begin transaction
        ctrl.phase = CommitPhase::Speculative;
        ctrl.mark_pending_verification();
        assert_eq!(ctrl.phase, CommitPhase::PendingVerification);

        // High confidence, low risk → commit
        let confidence = ConfidenceReport {
            root_cause: 0.9,
            semantic_understanding: 0.9,
            side_effect_risk: 0.9,
            verification_strength: 0.9,
            environment_certainty: 0.9,
        };
        let risk = RiskReport {
            score: 0.2,
            ..Default::default()
        };

        let decision = ctrl.evaluate(&confidence, &risk, Some(&Verdict::Pass));
        assert!(matches!(decision, CommitDecision::Commit { .. }));
    }

    #[test]
    fn low_confidence_triggers_ask() {
        let mut ctrl = TransactionController::new(CommitPolicy::default());
        ctrl.phase = CommitPhase::Speculative;
        ctrl.mark_pending_verification();

        let confidence = ConfidenceReport {
            root_cause: 0.3,
            semantic_understanding: 0.3,
            ..Default::default()
        };
        let risk = RiskReport {
            score: 0.1,
            ..Default::default()
        };

        let decision = ctrl.evaluate(&confidence, &risk, Some(&Verdict::Pass));
        assert!(matches!(decision, CommitDecision::Ask { .. }));
    }

    #[test]
    fn verifier_fail_triggers_rollback() {
        let mut ctrl = TransactionController::new(CommitPolicy::default());
        ctrl.phase = CommitPhase::Speculative;
        ctrl.mark_pending_verification();

        let confidence = ConfidenceReport::default();
        let risk = RiskReport::default();

        let decision = ctrl.evaluate(&confidence, &risk, Some(&Verdict::Fail));
        assert!(matches!(decision, CommitDecision::Rollback { .. }));
    }
}
