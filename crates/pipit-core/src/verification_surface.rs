//! Selective Verification Feedback Surface
//!
//! Surfaces verification state after semantically meaningful events,
//! not on every micro-edit. Triggers: completed edit batch, end of turn,
//! explicit /verify, pre-commit, or post-tool mutation.
//!
//! Cost-gated policy: trigger when C_v < p * R_e, where C_v is verification
//! cost, p is estimated defect probability, and R_e is expected rework cost.

use serde::{Deserialize, Serialize};

/// When verification should be triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationTrigger {
    /// After a batch of file edits completes.
    PostEditBatch,
    /// At the end of each turn (if mutations occurred).
    TurnEnd,
    /// User explicitly requested (/verify).
    Explicit,
    /// Before committing changes (pre-commit hook).
    PreCommit,
    /// After a tool that mutated external state (bash, shell).
    PostMutation,
    /// After plan pivot (strategy changed).
    PostPivot,
}

/// A verification event for surface rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationFeedback {
    /// What triggered this verification.
    pub trigger: VerificationTrigger,
    /// Overall verdict.
    pub verdict: VerificationVerdict,
    /// Confidence score (0.0–1.0).
    pub confidence: f32,
    /// Number of checks passed.
    pub checks_passed: u32,
    /// Number of checks failed.
    pub checks_failed: u32,
    /// Actionable findings.
    pub findings: Vec<VerificationFinding>,
    /// Files that were verified.
    pub verified_files: Vec<String>,
    /// Whether repair was attempted.
    pub repair_attempted: bool,
}

/// Verification verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerificationVerdict {
    /// All checks pass.
    Pass,
    /// Some checks failed but not critical.
    Warning,
    /// Critical failures detected.
    Fail,
    /// Verification was skipped (cost-gated).
    Skipped,
}

/// A single verification finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationFinding {
    pub severity: FindingSeverity,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<u32>,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FindingSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

/// Policy controller for verification triggering.
pub struct VerificationPolicy {
    /// Base verification cost (estimated tokens).
    pub verification_cost: f64,
    /// Files modified since last verification.
    pub pending_files: Vec<String>,
    /// Number of mutations since last verification.
    pub mutations_since_verify: u32,
    /// Estimated defect probability per mutation.
    pub defect_probability: f64,
    /// Rework cost per file (estimated tokens).
    pub rework_cost_per_file: f64,
    /// Whether the user has auto-verify enabled.
    pub auto_verify: bool,
    /// Maximum mutations before forcing verification.
    pub max_mutations_before_verify: u32,
}

impl Default for VerificationPolicy {
    fn default() -> Self {
        Self {
            verification_cost: 500.0,
            pending_files: Vec::new(),
            mutations_since_verify: 0,
            defect_probability: 0.15,
            rework_cost_per_file: 300.0,
            auto_verify: true,
            max_mutations_before_verify: 5,
        }
    }
}

impl VerificationPolicy {
    /// Record a file mutation.
    pub fn record_mutation(&mut self, path: &str) {
        if !self.pending_files.contains(&path.to_string()) {
            self.pending_files.push(path.to_string());
        }
        self.mutations_since_verify += 1;
    }

    /// Should verification trigger for the given event?
    /// Uses cost-gated policy: C_v < p * R_e.
    pub fn should_verify(&self, trigger: VerificationTrigger) -> bool {
        // Explicit always triggers
        if matches!(
            trigger,
            VerificationTrigger::Explicit | VerificationTrigger::PreCommit
        ) {
            return true;
        }

        if !self.auto_verify {
            return false;
        }

        // No pending mutations → skip
        if self.pending_files.is_empty() {
            return false;
        }

        // Force after max mutations
        if self.mutations_since_verify >= self.max_mutations_before_verify {
            return true;
        }

        // Cost-gated: verify when C_v < p * R_e
        let p = self.defect_probability;
        let r_e = self.rework_cost_per_file * self.pending_files.len() as f64;
        let expected_penalty = p * r_e;

        expected_penalty > self.verification_cost
    }

    /// Reset after verification completes.
    pub fn verification_completed(&mut self) {
        self.pending_files.clear();
        self.mutations_since_verify = 0;
    }

    /// Adjust defect probability based on verification history.
    /// If recent verifications found defects, increase p; otherwise decay.
    pub fn update_probability(&mut self, had_defects: bool) {
        if had_defects {
            self.defect_probability = (self.defect_probability * 1.5).min(0.8);
        } else {
            self.defect_probability = (self.defect_probability * 0.8).max(0.05);
        }
    }
}

/// Format verification feedback for terminal display.
pub fn format_verification_feedback(feedback: &VerificationFeedback) -> String {
    let icon = match feedback.verdict {
        VerificationVerdict::Pass => "✓",
        VerificationVerdict::Warning => "⚠",
        VerificationVerdict::Fail => "✗",
        VerificationVerdict::Skipped => "○",
    };

    let mut output = format!(
        "{} Verification: {} ({:.0}% confidence, {}/{} checks)\n",
        icon,
        match feedback.verdict {
            VerificationVerdict::Pass => "PASS",
            VerificationVerdict::Warning => "WARNING",
            VerificationVerdict::Fail => "FAIL",
            VerificationVerdict::Skipped => "SKIPPED",
        },
        feedback.confidence * 100.0,
        feedback.checks_passed,
        feedback.checks_passed + feedback.checks_failed,
    );

    for finding in &feedback.findings {
        let sev = match finding.severity {
            FindingSeverity::Info => "ℹ",
            FindingSeverity::Warning => "⚠",
            FindingSeverity::Error => "✗",
            FindingSeverity::Critical => "‼",
        };
        output.push_str(&format!("  {} {}", sev, finding.message));
        if let Some(ref file) = finding.file {
            output.push_str(&format!(" ({})", file));
            if let Some(line) = finding.line {
                output.push_str(&format!(":{}", line));
            }
        }
        output.push('\n');
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_mutations_skip_verification() {
        let policy = VerificationPolicy::default();
        assert!(!policy.should_verify(VerificationTrigger::TurnEnd));
    }

    #[test]
    fn explicit_always_verifies() {
        let policy = VerificationPolicy::default();
        assert!(policy.should_verify(VerificationTrigger::Explicit));
        assert!(policy.should_verify(VerificationTrigger::PreCommit));
    }

    #[test]
    fn cost_gated_trigger() {
        let mut policy = VerificationPolicy::default();
        // Add mutations to make expected penalty > verification cost
        for i in 0..4 {
            policy.record_mutation(&format!("file{}.rs", i));
        }
        // p=0.15, R_e = 300*4 = 1200, expected = 0.15*1200 = 180 < 500 → no
        assert!(!policy.should_verify(VerificationTrigger::PostEditBatch));

        // Add more files to tip the balance
        for i in 4..15 {
            policy.record_mutation(&format!("file{}.rs", i));
        }
        // p=0.15, R_e = 300*15 = 4500, expected = 0.15*4500 = 675 > 500 → yes
        assert!(policy.should_verify(VerificationTrigger::PostEditBatch));
    }

    #[test]
    fn forced_after_max_mutations() {
        let mut policy = VerificationPolicy {
            max_mutations_before_verify: 3,
            ..Default::default()
        };
        policy.record_mutation("a.rs");
        policy.record_mutation("b.rs");
        assert!(!policy.should_verify(VerificationTrigger::TurnEnd));
        policy.record_mutation("c.rs");
        assert!(policy.should_verify(VerificationTrigger::TurnEnd));
    }

    #[test]
    fn probability_adaptation() {
        let mut policy = VerificationPolicy::default();
        let initial = policy.defect_probability;
        policy.update_probability(true);
        assert!(policy.defect_probability > initial);
        policy.update_probability(false);
        policy.update_probability(false);
        policy.update_probability(false);
        assert!(policy.defect_probability < initial);
    }
}
