//! Multi-Objective Fitness Evaluator — Task EVO-2
//!
//! 5 dimensions: correctness, performance, complexity, diff_size, quality.
//! Pareto dominance: A dominates B iff ∀i: fᵢ(A) ≥ fᵢ(B) ∧ ∃i: fᵢ(A) > fᵢ(B).
//! Pareto front: O(N²·k) via pairwise comparison (N≤10, k=5: 250 comparisons).

use serde::{Deserialize, Serialize};

/// Multi-dimensional fitness vector for a code change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitnessVector {
    /// Test pass rate [0, 1]. Binary per-test, averaged.
    pub correctness: f64,
    /// Relative performance (1.0 = baseline, >1.0 = faster).
    pub performance: f64,
    /// Inverse cyclomatic complexity change (1.0 = unchanged, >1.0 = simpler).
    pub complexity: f64,
    /// Inverse diff size (1.0 = no change, smaller = better).
    pub diff_size: f64,
    /// LLM quality judgment [0, 1]. Goodhart-vulnerable axis.
    pub quality: f64,
}

impl FitnessVector {
    pub fn dimensions(&self) -> [f64; 5] {
        [
            self.correctness,
            self.performance,
            self.complexity,
            self.diff_size,
            self.quality,
        ]
    }

    /// Scalar summary (weighted sum). Used for quick ranking, not Pareto analysis.
    pub fn weighted_score(&self) -> f64 {
        0.40 * self.correctness
            + 0.20 * self.performance
            + 0.15 * self.complexity
            + 0.15 * self.diff_size
            + 0.10 * self.quality
    }
}

/// A point on the Pareto front.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParetoPoint {
    pub variant_id: usize,
    pub fitness: FitnessVector,
    pub pareto_rank: usize,
    pub crowding_distance: f64,
}

/// The Pareto-optimal set of variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParetoFront {
    pub points: Vec<ParetoPoint>,
}

pub struct FitnessEvaluator;

impl FitnessEvaluator {
    /// Check if A Pareto-dominates B.
    pub fn dominates(a: &FitnessVector, b: &FitnessVector) -> bool {
        let a_dims = a.dimensions();
        let b_dims = b.dimensions();
        let all_ge = a_dims.iter().zip(&b_dims).all(|(ai, bi)| ai >= bi);
        let any_gt = a_dims.iter().zip(&b_dims).any(|(ai, bi)| ai > bi);
        all_ge && any_gt
    }

    /// Compute the Pareto front from a set of fitness vectors.
    /// O(N²·k) via pairwise comparison.
    pub fn pareto_front(variants: &[(usize, FitnessVector)]) -> ParetoFront {
        let n = variants.len();
        let mut ranks = vec![1usize; n];
        let mut domination_count = vec![0usize; n];

        // Non-dominated sorting
        for i in 0..n {
            for j in 0..n {
                if i == j {
                    continue;
                }
                if Self::dominates(&variants[j].1, &variants[i].1) {
                    domination_count[i] += 1;
                }
            }
        }

        // Rank assignment (rank 1 = non-dominated)
        let mut current_rank = 1;
        let mut remaining: Vec<usize> = (0..n).collect();

        while !remaining.is_empty() {
            let front: Vec<usize> = remaining
                .iter()
                .filter(|&&i| domination_count[i] == 0)
                .copied()
                .collect();

            if front.is_empty() {
                break;
            }

            for &i in &front {
                ranks[i] = current_rank;
            }

            // Remove front members and update domination counts
            for &i in &front {
                for &j in &remaining {
                    if Self::dominates(&variants[i].1, &variants[j].1) {
                        domination_count[j] = domination_count[j].saturating_sub(1);
                    }
                }
            }

            remaining.retain(|i| !front.contains(i));
            current_rank += 1;
        }

        // Build Pareto front (rank 1 points)
        let points: Vec<ParetoPoint> = variants
            .iter()
            .enumerate()
            .map(|(idx, (vid, fitness))| ParetoPoint {
                variant_id: *vid,
                fitness: fitness.clone(),
                pareto_rank: ranks[idx],
                crowding_distance: 0.0,
            })
            .filter(|p| p.pareto_rank == 1)
            .collect();

        ParetoFront { points }
    }

    /// Evaluate a code change. Returns fitness from test results + metrics.
    pub fn evaluate(
        tests_passed: u32,
        tests_total: u32,
        baseline_time_ms: u64,
        variant_time_ms: u64,
        baseline_complexity: u32,
        variant_complexity: u32,
        diff_lines: usize,
        max_diff_lines: usize,
    ) -> FitnessVector {
        let correctness = if tests_total > 0 {
            tests_passed as f64 / tests_total as f64
        } else {
            0.5
        };

        let performance = if variant_time_ms > 0 {
            (baseline_time_ms as f64 / variant_time_ms as f64).min(5.0)
        } else {
            1.0
        };

        let complexity = if variant_complexity > 0 {
            (baseline_complexity as f64 / variant_complexity as f64).min(3.0)
        } else {
            1.0
        };

        let diff_size = if max_diff_lines > 0 {
            1.0 - (diff_lines as f64 / max_diff_lines as f64).min(1.0)
        } else {
            1.0
        };

        FitnessVector {
            correctness,
            performance,
            complexity,
            diff_size,
            quality: 0.5, // Default; LLM judge fills this in
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dominance() {
        let a = FitnessVector {
            correctness: 1.0,
            performance: 0.8,
            complexity: 0.7,
            diff_size: 0.9,
            quality: 0.8,
        };
        let b = FitnessVector {
            correctness: 0.9,
            performance: 0.7,
            complexity: 0.6,
            diff_size: 0.8,
            quality: 0.7,
        };
        assert!(FitnessEvaluator::dominates(&a, &b));
        assert!(!FitnessEvaluator::dominates(&b, &a));
    }

    #[test]
    fn test_non_dominated_pair() {
        // A is better on correctness, B is better on performance — neither dominates
        let a = FitnessVector {
            correctness: 1.0,
            performance: 0.5,
            complexity: 0.7,
            diff_size: 0.8,
            quality: 0.7,
        };
        let b = FitnessVector {
            correctness: 0.8,
            performance: 1.0,
            complexity: 0.7,
            diff_size: 0.8,
            quality: 0.7,
        };
        assert!(!FitnessEvaluator::dominates(&a, &b));
        assert!(!FitnessEvaluator::dominates(&b, &a));
    }

    #[test]
    fn test_pareto_front() {
        let variants = vec![
            (
                0,
                FitnessVector {
                    correctness: 1.0,
                    performance: 0.5,
                    complexity: 0.7,
                    diff_size: 0.8,
                    quality: 0.7,
                },
            ),
            (
                1,
                FitnessVector {
                    correctness: 0.8,
                    performance: 1.0,
                    complexity: 0.6,
                    diff_size: 0.9,
                    quality: 0.6,
                },
            ),
            (
                2,
                FitnessVector {
                    correctness: 0.5,
                    performance: 0.3,
                    complexity: 0.3,
                    diff_size: 0.4,
                    quality: 0.3,
                },
            ),
        ];
        let front = FitnessEvaluator::pareto_front(&variants);
        // Variants 0 and 1 are non-dominated; variant 2 is dominated by both
        assert_eq!(front.points.len(), 2, "Front should have 2 points");
        let ids: Vec<usize> = front.points.iter().map(|p| p.variant_id).collect();
        assert!(ids.contains(&0));
        assert!(ids.contains(&1));
    }

    #[test]
    fn test_evaluate_fitness() {
        let f = FitnessEvaluator::evaluate(9, 10, 1000, 800, 20, 15, 50, 200);
        assert!((f.correctness - 0.9).abs() < 0.01);
        assert!(f.performance > 1.0, "Faster variant should score >1.0");
        assert!(f.complexity > 1.0, "Simpler variant should score >1.0");
        assert!(f.diff_size > 0.5, "Small diff should score high");
    }
}
