use anyhow::Result;
use pipit_core::{PlanningState, ProofPacket};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningSnapshot {
    pub planning_state: PlanningState,
    pub proof_summary: Option<PlanningProofSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningProofSummary {
    pub objective: String,
    pub confidence: f32,
    pub risk_score: f32,
    pub proof_file: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub enum PlanningStateSource {
    Live,
    Disk,
}

pub struct LoadedPlanningState {
    pub state: PlanningState,
    pub source: PlanningStateSource,
    pub proof_summary: Option<PlanningProofSummary>,
}

pub fn persist_proof_packet(project_root: &Path, proof: &ProofPacket) -> Result<PathBuf> {
    let proofs_dir = project_root.join(".pipit").join("proofs");
    std::fs::create_dir_all(&proofs_dir)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let file_path = proofs_dir.join(format!("proof-{}.json", timestamp));
    let json = serde_json::to_string_pretty(proof)?;
    std::fs::write(&file_path, json)?;
    Ok(file_path)
}

pub fn planning_proof_summary(
    proof: &ProofPacket,
    proof_path: Option<&PathBuf>,
) -> Option<PlanningProofSummary> {
    Some(PlanningProofSummary {
        objective: proof.objective.statement.clone(),
        confidence: proof.confidence.overall(),
        risk_score: proof.risk.score,
        proof_file: proof_path.map(|path| path.display().to_string()),
    })
}

pub fn persist_planning_snapshot(
    project_root: &Path,
    planning_state: &PlanningState,
    proof_summary: Option<PlanningProofSummary>,
) -> Result<()> {
    let plans_dir = project_root.join(".pipit").join("plans");
    std::fs::create_dir_all(&plans_dir)?;
    let file_path = plans_dir.join("latest.json");
    let snapshot = PlanningSnapshot {
        planning_state: planning_state.clone(),
        proof_summary,
    };
    let json = serde_json::to_string_pretty(&snapshot)?;
    std::fs::write(file_path, json)?;
    Ok(())
}

pub fn load_planning_snapshot(project_root: &Path) -> Result<Option<LoadedPlanningState>> {
    let file_path = project_root.join(".pipit").join("plans").join("latest.json");
    if !file_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(file_path)?;
    if let Ok(snapshot) = serde_json::from_str::<PlanningSnapshot>(&raw) {
        return Ok(Some(LoadedPlanningState {
            state: snapshot.planning_state,
            source: PlanningStateSource::Disk,
            proof_summary: snapshot.proof_summary,
        }));
    }

    let planning_state = serde_json::from_str::<PlanningState>(&raw)?;
    Ok(Some(LoadedPlanningState {
        state: planning_state,
        source: PlanningStateSource::Disk,
        proof_summary: None,
    }))
}

pub fn print_proof_summary(proof: &ProofPacket) {
    eprintln!("\n\x1b[2mProof packet\x1b[0m");
    eprintln!("  Objective: {}", proof.objective.statement);
    eprintln!(
        "  Selected plan: {:?} ({})",
        proof.selected_plan.strategy,
        proof.selected_plan.rationale
    );
    if !proof.candidate_plans.is_empty() {
        eprintln!("  Top candidate plans:");
        for (index, plan) in proof.candidate_plans.iter().take(3).enumerate() {
            let score = plan.expected_value - plan.estimated_cost;
            eprintln!(
                "    {}. {:?} | score {:.2} | expected {:.2} | cost {:.2}",
                index + 1,
                plan.strategy,
                score,
                plan.expected_value,
                plan.estimated_cost
            );
            eprintln!("       {}", plan.rationale);
        }
    }
    eprintln!(
        "  Confidence: {:.2} | Risk score: {:.4}",
        proof.confidence.overall(),
        proof.risk.score
    );
    eprintln!("  Evidence artifacts: {}", proof.evidence.len());
    if !proof.plan_pivots.is_empty() {
        eprintln!("  Plan pivots:");
        for pivot in &proof.plan_pivots {
            eprintln!(
                "    - turn {}: {:?} -> {:?} ({})",
                pivot.turn_number,
                pivot.from.strategy,
                pivot.to.strategy,
                pivot.trigger
            );
        }
    }
    if let Some(checkpoint_id) = &proof.rollback_checkpoint.checkpoint_id {
        eprintln!("  Rollback checkpoint: {}", checkpoint_id);
    }
    if !proof.realized_edits.is_empty() {
        eprintln!("  Realized edits:");
        for edit in &proof.realized_edits {
            eprintln!("    - {}: {}", edit.path, edit.summary);
        }
    }
    if !proof.unresolved_assumptions.is_empty() {
        eprintln!("  Unresolved assumptions:");
        for assumption in &proof.unresolved_assumptions {
            eprintln!("    - {}", assumption.description);
        }
    }
    if !proof.tiers.is_empty() {
        let tier_summary: Vec<String> = proof
            .tiers
            .iter()
            .map(|(k, v)| format!("{}: {}", k, v))
            .collect();
        eprintln!("  Tiers: {}", tier_summary.join(" | "));
    }
}

pub fn print_plans(loaded: Option<LoadedPlanningState>) {
    let Some(LoadedPlanningState {
        state,
        source,
        proof_summary,
    }) = loaded else {
        eprintln!("\x1b[2mNo planning state yet. Run a task first.\x1b[0m");
        return;
    };

    eprintln!("\x1b[2mRanked plans\x1b[0m");
    let source = match source {
        PlanningStateSource::Live => "live session",
        PlanningStateSource::Disk => "persisted snapshot",
    };
    eprintln!("  source: {}", source);
    if let Some(summary) = proof_summary {
        eprintln!(
            "  latest proof: confidence {:.2} | risk {:.4}",
            summary.confidence,
            summary.risk_score
        );
        eprintln!("  objective: {}", summary.objective);
        if let Some(path) = summary.proof_file {
            eprintln!("  proof file: {}", path);
        }
    }
    for (index, plan) in state.candidate_plans.iter().enumerate() {
        let score = plan.expected_value - plan.estimated_cost;
        let marker = if plan == &state.selected_plan { "*" } else { " " };
        eprintln!(
            "{} {}. {:?} | score {:.2} | expected {:.2} | cost {:.2}",
            marker,
            index + 1,
            plan.strategy,
            score,
            plan.expected_value,
            plan.estimated_cost
        );
        eprintln!("    {}", plan.rationale);
    }

    if !state.plan_pivots.is_empty() {
        eprintln!("\n\x1b[2mPivot history\x1b[0m");
        for pivot in &state.plan_pivots {
            eprintln!(
                "  turn {}: {:?} -> {:?} | {}",
                pivot.turn_number,
                pivot.from.strategy,
                pivot.to.strategy,
                pivot.trigger
            );
        }
    }
}
