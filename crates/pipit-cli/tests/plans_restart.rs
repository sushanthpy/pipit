use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

use tempfile::tempdir;

#[test]
fn plans_command_loads_persisted_snapshot_with_metadata_after_restart() {
    let temp = tempdir().unwrap();
    let pipit_dir = temp.path().join(".pipit");
    let plans_dir = pipit_dir.join("plans");
    let proofs_dir = pipit_dir.join("proofs");
    fs::create_dir_all(&plans_dir).unwrap();
    fs::create_dir_all(&proofs_dir).unwrap();

    let proof_path = proofs_dir.join("proof-123.json");
    fs::write(&proof_path, "{}\n").unwrap();

    let snapshot = serde_json::json!({
        "planning_state": {
            "selected_plan": {
                "strategy": "CharacterizationFirst",
                "rationale": "Persisted snapshot selected characterization after failed checks.",
                "expected_value": 0.99,
                "estimated_cost": 0.4,
                "verification_plan": [
                    { "description": "Run documented examples first." }
                ]
            },
            "candidate_plans": [
                {
                    "strategy": "MinimalPatch",
                    "rationale": "Too risky after repeated failures.",
                    "expected_value": 0.44,
                    "estimated_cost": 0.25,
                    "verification_plan": [
                        { "description": "Run a narrow test." }
                    ]
                },
                {
                    "strategy": "CharacterizationFirst",
                    "rationale": "Persisted snapshot selected characterization after failed checks.",
                    "expected_value": 0.99,
                    "estimated_cost": 0.4,
                    "verification_plan": [
                        { "description": "Run documented examples first." }
                    ]
                }
            ],
            "plan_pivots": [
                {
                    "turn_number": 3,
                    "from": {
                        "strategy": "MinimalPatch",
                        "rationale": "Initial lightweight strategy.",
                        "expected_value": 0.82,
                        "estimated_cost": 0.25,
                        "verification_plan": [
                            { "description": "Run a narrow test." }
                        ]
                    },
                    "to": {
                        "strategy": "CharacterizationFirst",
                        "rationale": "Persisted snapshot selected characterization after failed checks.",
                        "expected_value": 0.99,
                        "estimated_cost": 0.4,
                        "verification_plan": [
                            { "description": "Run documented examples first." }
                        ]
                    },
                    "trigger": "Verification evidence forced a pivot."
                }
            ]
        },
        "proof_summary": {
            "objective": "verify persisted plans metadata survives restart",
            "confidence": 0.61,
            "risk_score": 0.12,
            "proof_file": proof_path.display().to_string()
        }
    });
    fs::write(
        plans_dir.join("latest.json"),
        serde_json::to_vec_pretty(&snapshot).unwrap(),
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_pipit"))
        .current_dir(temp.path())
        .arg("--root")
        .arg(temp.path())
        .arg("--provider")
        .arg("openai")
        .arg("--model")
        .arg("test-model")
        .arg("--api-key")
        .arg("test-key")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"/plans\n/quit\n").unwrap();
    drop(stdin);

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Ranked plans"));
    assert!(stderr.contains("source: persisted snapshot"));
    assert!(stderr.contains("latest proof: confidence 0.61 | risk 0.1200"));
    assert!(stderr.contains("objective: verify persisted plans metadata survives restart"));
    assert!(stderr.contains(&format!("proof file: {}", proof_path.display())));
    assert!(stderr.contains("Pivot history"));
}