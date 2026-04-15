//! # Verifier Ensemble with Platt Calibration (C3)
//!
//! Multiple verification strategies vote on correctness, and Platt scaling
//! calibrates raw scores into well-calibrated probabilities.
//!
//! ## Design
//!
//! Each verifier produces a raw score in [0,1]. Platt scaling transforms:
//! `P(correct) = 1 / (1 + exp(A*s + B))` where A,B are learned from feedback.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A verification verdict from a single verifier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub verifier: String,
    pub raw_score: f64,
    pub explanation: String,
}

/// Platt calibration parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlattParams {
    /// Sigmoid slope parameter (learned).
    pub a: f64,
    /// Sigmoid intercept parameter (learned).
    pub b: f64,
}

impl Default for PlattParams {
    fn default() -> Self {
        // Default: maps [0,1] scores through a sigmoid centered at 0.5
        // calibrate(0.9) ≈ 0.92, calibrate(0.1) ≈ 0.08
        Self { a: 6.0, b: -3.0 }
    }
}

impl PlattParams {
    /// Transform raw score → calibrated probability.
    pub fn calibrate(&self, raw_score: f64) -> f64 {
        1.0 / (1.0 + (-(self.a * raw_score + self.b)).exp())
    }

    /// Fit Platt parameters from labeled data (score, label) pairs.
    /// Uses iterative reweighted least squares (simplified Newton's method).
    pub fn fit(data: &[(f64, bool)]) -> Self {
        if data.is_empty() {
            return Self::default();
        }

        let mut a: f64 = 0.0;
        let mut b: f64 = (data.len() as f64).ln();

        let n_pos = data.iter().filter(|(_, y)| *y).count() as f64;
        let n_neg = data.len() as f64 - n_pos;
        if n_pos == 0.0 || n_neg == 0.0 {
            return Self::default();
        }

        // Target probabilities (Platt's target encoding)
        let t_pos = (n_pos + 1.0) / (n_pos + 2.0);
        let t_neg = 1.0 / (n_neg + 2.0);

        // Newton's method for 100 iterations (typically converges in ~10)
        for _ in 0..100 {
            let mut d1a = 0.0_f64;
            let mut d1b = 0.0_f64;
            let mut d2a = 0.0_f64;
            let mut d2ab = 0.0_f64;
            let mut d2b = 0.0_f64;

            for &(score, label) in data {
                let t = if label { t_pos } else { t_neg };
                let fval = a * score + b;
                let p = 1.0 / (1.0 + (-fval).exp());
                let d = p - t;
                let w = p * (1.0 - p);

                d1a += score * d;
                d1b += d;
                d2a += score * score * w;
                d2ab += score * w;
                d2b += w;
            }

            let det = d2a * d2b - d2ab * d2ab;
            if det.abs() < 1e-12 {
                break;
            }

            let da = -(d2b * d1a - d2ab * d1b) / det;
            let db = -(d2a * d1b - d2ab * d1a) / det;

            a += da;
            b += db;

            if da.abs() < 1e-10 && db.abs() < 1e-10 {
                break;
            }
        }

        Self { a, b }
    }
}

/// Ensemble of verifiers with calibrated confidence.
pub struct VerifierEnsemble {
    verifiers: Vec<Box<dyn Fn(&str) -> Verdict + Send + Sync>>,
    calibration: HashMap<String, PlattParams>,
    /// Weight per verifier (default 1.0).
    weights: HashMap<String, f64>,
    /// Minimum calibrated confidence to pass.
    threshold: f64,
}

/// Ensemble result.
#[derive(Debug, Clone)]
pub struct EnsembleResult {
    pub verdicts: Vec<Verdict>,
    pub calibrated_scores: Vec<(String, f64)>,
    pub aggregate_confidence: f64,
    pub passed: bool,
}

impl VerifierEnsemble {
    pub fn new(threshold: f64) -> Self {
        Self {
            verifiers: Vec::new(),
            calibration: HashMap::new(),
            weights: HashMap::new(),
            threshold,
        }
    }

    /// Register a verifier with an optional name and weight.
    pub fn add_verifier<F>(&mut self, name: &str, weight: f64, f: F)
    where
        F: Fn(&str) -> Verdict + Send + Sync + 'static,
    {
        self.weights.insert(name.to_string(), weight);
        self.verifiers.push(Box::new(f));
    }

    /// Update calibration for a verifier from labeled data.
    pub fn calibrate_verifier(&mut self, name: &str, data: &[(f64, bool)]) {
        let params = PlattParams::fit(data);
        self.calibration.insert(name.to_string(), params);
    }

    /// Run the ensemble on an artifact (code diff, output, etc.).
    pub fn verify(&self, artifact: &str) -> EnsembleResult {
        let verdicts: Vec<Verdict> = self.verifiers.iter().map(|v| v(artifact)).collect();

        let calibrated_scores: Vec<(String, f64)> = verdicts
            .iter()
            .map(|v| {
                let params = self
                    .calibration
                    .get(&v.verifier)
                    .cloned()
                    .unwrap_or_default();
                let cal = params.calibrate(v.raw_score);
                (v.verifier.clone(), cal)
            })
            .collect();

        // Weighted average of calibrated scores.
        let total_weight: f64 = calibrated_scores
            .iter()
            .map(|(name, _)| self.weights.get(name).copied().unwrap_or(1.0))
            .sum();

        let aggregate: f64 = if total_weight > 0.0 {
            calibrated_scores
                .iter()
                .map(|(name, score)| {
                    let w = self.weights.get(name).copied().unwrap_or(1.0);
                    w * score
                })
                .sum::<f64>()
                / total_weight
        } else {
            0.0
        };

        EnsembleResult {
            verdicts,
            calibrated_scores,
            aggregate_confidence: aggregate,
            passed: aggregate >= self.threshold,
        }
    }

    /// Get the confidence threshold.
    pub fn threshold(&self) -> f64 {
        self.threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platt_default_calibrates_near_identity() {
        let params = PlattParams::default();
        let cal = params.calibrate(0.8);
        // With a=6, b=-3: 1/(1+exp(-(6*0.8-3))) = 1/(1+exp(-1.8)) ≈ 0.86
        assert!(cal > 0.5 && cal < 1.0);
    }

    #[test]
    fn platt_fit_on_separable_data() {
        let data: Vec<(f64, bool)> = vec![
            (0.9, true),
            (0.85, true),
            (0.8, true),
            (0.7, true),
            (0.3, false),
            (0.2, false),
            (0.15, false),
            (0.1, false),
        ];
        let params = PlattParams::fit(&data);
        // High scores should calibrate to high probability
        let high = params.calibrate(0.9);
        let low = params.calibrate(0.1);
        assert!(high > low, "high={high:.3}, low={low:.3}");
    }

    #[test]
    fn platt_fit_empty_returns_default() {
        let params = PlattParams::fit(&[]);
        assert_eq!(params.a, 6.0);
        assert_eq!(params.b, -3.0);
    }

    #[test]
    fn ensemble_passes_on_high_scores() {
        let mut ensemble = VerifierEnsemble::new(0.5);
        ensemble.add_verifier("syntax", 1.0, |_artifact| Verdict {
            verifier: "syntax".into(),
            raw_score: 0.95,
            explanation: "syntax ok".into(),
        });
        ensemble.add_verifier("types", 1.0, |_artifact| Verdict {
            verifier: "types".into(),
            raw_score: 0.9,
            explanation: "types ok".into(),
        });

        let result = ensemble.verify("fn main() {}");
        assert!(result.passed);
        assert_eq!(result.verdicts.len(), 2);
    }

    #[test]
    fn ensemble_fails_on_low_scores() {
        let mut ensemble = VerifierEnsemble::new(0.7);
        ensemble.add_verifier("lint", 1.0, |_artifact| Verdict {
            verifier: "lint".into(),
            raw_score: 0.1,
            explanation: "many warnings".into(),
        });

        let result = ensemble.verify("bad code");
        assert!(!result.passed);
    }

    #[test]
    fn weighted_ensemble() {
        let mut ensemble = VerifierEnsemble::new(0.5);
        ensemble.add_verifier("critical", 3.0, |_| Verdict {
            verifier: "critical".into(),
            raw_score: 0.9,
            explanation: "good".into(),
        });
        ensemble.add_verifier("minor", 1.0, |_| Verdict {
            verifier: "minor".into(),
            raw_score: 0.1,
            explanation: "bad".into(),
        });

        let result = ensemble.verify("code");
        // Weighted: (3*cal(0.9) + 1*cal(0.1)) / 4
        // With default Platt: cal(0.9)≈0.71, cal(0.1)≈0.52
        // aggregate≈(3*0.71 + 0.52)/4 ≈ 0.66
        assert!(result.aggregate_confidence > 0.5);
    }
}
