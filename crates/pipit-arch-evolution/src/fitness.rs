//! Multi-Objective Fitness Engine — Task 10.2 (NSGA-II)
//!
//! Evaluates architecture genomes against user requirements.
//! Produces Pareto-optimal frontier.
//!
//! NSGA-II: non-dominated sort O(k·N²) + crowding distance.
//! Fitness evaluation per genome: O(V+E) analytical (no deployment needed).

use crate::genome::*;
use rand::Rng;
use serde::{Deserialize, Serialize};

/// User-specified optimization objectives.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitnessObjectives {
    /// Target: minimize end-to-end latency (ms).
    pub max_latency_ms: f64,
    /// Target: minimize monthly cost ($).
    pub max_cost_usd: f64,
    /// Target: maximize reliability (0-1).
    pub min_reliability: f64,
    /// Weight for operational complexity penalty.
    pub complexity_weight: f64,
}

impl Default for FitnessObjectives {
    fn default() -> Self {
        Self {
            max_latency_ms: 100.0,
            max_cost_usd: 500.0,
            min_reliability: 0.999,
            complexity_weight: 0.1,
        }
    }
}

/// Fitness scores for a single genome (higher is better for each objective).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FitnessScores {
    /// Negative latency (lower latency = higher fitness).
    pub latency_fitness: f64,
    /// Negative cost (lower cost = higher fitness).
    pub cost_fitness: f64,
    /// Reliability (higher = better).
    pub reliability_fitness: f64,
    /// Pareto rank (1 = non-dominated front).
    pub pareto_rank: usize,
    /// Crowding distance (diversity measure).
    pub crowding_distance: f64,
}

/// A genome on the Pareto frontier with its scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParetoFront {
    pub solutions: Vec<(ArchGenome, FitnessScores)>,
}

pub struct FitnessEngine {
    pub objectives: FitnessObjectives,
}

impl FitnessEngine {
    pub fn new(objectives: FitnessObjectives) -> Self {
        Self { objectives }
    }

    /// Evaluate a single genome's fitness. O(V + E).
    pub fn evaluate(&self, genome: &ArchGenome) -> FitnessScores {
        // Latency: longest path in the service graph (critical path).
        let latency = self.compute_critical_path_latency(genome);
        let latency_fitness = 1.0 - (latency / self.objectives.max_latency_ms).min(1.0);

        // Cost: sum of service costs.
        let cost: f64 = genome.services.iter().map(|s| s.cost_estimate).sum();
        let cost_fitness = 1.0 - (cost / self.objectives.max_cost_usd).min(1.0);

        // Reliability: product of channel reliabilities × service reliabilities.
        let svc_reliability: f64 = genome.services.iter()
            .map(|s| s.reliability).product();
        let ch_reliability: f64 = if genome.channels.is_empty() { 1.0 }
            else { genome.channels.iter().map(|c| c.reliability).product() };
        let reliability = svc_reliability * ch_reliability;
        let reliability_fitness = reliability;

        FitnessScores {
            latency_fitness,
            cost_fitness,
            reliability_fitness,
            pareto_rank: 0,
            crowding_distance: 0.0,
        }
    }

    /// Critical path latency: longest path from any gateway to any database.
    fn compute_critical_path_latency(&self, genome: &ArchGenome) -> f64 {
        if genome.services.is_empty() { return 0.0; }

        let n = genome.services.len();
        // Build adjacency list with edge latencies
        let mut adj: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
        for ch in &genome.channels {
            if ch.from < n && ch.to < n {
                let edge_latency = match ch.channel_type {
                    ChannelType::SyncRpc => 2.0,
                    ChannelType::AsyncMessage => 10.0,
                    ChannelType::EventStream => 5.0,
                    ChannelType::SharedDb => 3.0,
                };
                adj[ch.from].push((ch.to, edge_latency));
            }
        }

        // Find longest path from any node (simple DFS, works for DAGs)
        let mut max_latency = 0.0_f64;
        for i in 0..n {
            let path_latency = dfs_longest_path(&adj, &genome.services, i, &mut vec![false; n]);
            max_latency = max_latency.max(path_latency);
        }

        max_latency
    }

    /// Evolve a population for N generations using NSGA-II.
    pub fn evolve(&self, population_size: usize, generations: usize) -> ParetoFront {
        let mut rng = rand::thread_rng();

        // Initialize population from monolith with random mutations
        let mut population: Vec<ArchGenome> = (0..population_size)
            .map(|i| {
                let mut g = ArchGenome::monolith(&format!("svc-{}", i));
                for _ in 0..rng.gen_range(1..5) { g.mutate(&mut rng); }
                g
            })
            .collect();

        for generation in 0..generations {
            // Evaluate fitness
            let mut scored: Vec<(ArchGenome, FitnessScores)> = population.iter()
                .map(|g| (g.clone(), self.evaluate(g)))
                .collect();

            // Non-dominated sorting
            self.non_dominated_sort(&mut scored);

            // Crowding distance within each rank
            self.compute_crowding_distance(&mut scored);

            // Selection: keep top N by (rank ASC, crowding DESC)
            scored.sort_by(|a, b| {
                a.1.pareto_rank.cmp(&b.1.pareto_rank)
                    .then(b.1.crowding_distance.partial_cmp(&a.1.crowding_distance)
                        .unwrap_or(std::cmp::Ordering::Equal))
            });
            scored.truncate(population_size);

            // Generate offspring via crossover + mutation
            let mut offspring = Vec::new();
            for i in (0..scored.len()).step_by(2) {
                if i + 1 < scored.len() {
                    let mut child = scored[i].0.crossover(&scored[i + 1].0, &mut rng);
                    if rng.gen_range(0.0..1.0) < 0.3 { child.mutate(&mut rng); }
                    offspring.push(child);
                }
            }

            population = scored.into_iter().map(|(g, _)| g).collect();
            population.extend(offspring);
            population.truncate(population_size);
        }

        // Final evaluation and Pareto front extraction
        let mut final_scored: Vec<(ArchGenome, FitnessScores)> = population.iter()
            .map(|g| (g.clone(), self.evaluate(g)))
            .collect();
        self.non_dominated_sort(&mut final_scored);

        ParetoFront {
            solutions: final_scored.into_iter()
                .filter(|(_, s)| s.pareto_rank == 1)
                .collect(),
        }
    }

    /// NSGA-II non-dominated sorting.
    /// Optimized: only compare i < j pairs (each pair checked once, both directions).
    /// Reduces constant factor by ~2x vs naive O(k·N²).
    fn non_dominated_sort(&self, scored: &mut [(ArchGenome, FitnessScores)]) {
        let n = scored.len();
        let mut ranks = vec![0usize; n];
        let mut domination_count = vec![0usize; n];
        let mut dominated_by: Vec<Vec<usize>> = vec![Vec::new(); n];

        // Compare each pair only once (upper triangle)
        for i in 0..n {
            for j in (i + 1)..n {
                if self.dominates(&scored[i].1, &scored[j].1) {
                    dominated_by[i].push(j);
                    domination_count[j] += 1;
                } else if self.dominates(&scored[j].1, &scored[i].1) {
                    dominated_by[j].push(i);
                    domination_count[i] += 1;
                }
            }
        }

        let mut current_rank = 1;
        let mut front: Vec<usize> = (0..n).filter(|&i| domination_count[i] == 0).collect();

        while !front.is_empty() {
            let mut next_front = Vec::new();
            for &i in &front {
                ranks[i] = current_rank;
                for &j in &dominated_by[i] {
                    domination_count[j] = domination_count[j].saturating_sub(1);
                    if domination_count[j] == 0 {
                        next_front.push(j);
                    }
                }
            }
            current_rank += 1;
            front = next_front;
        }

        for (i, (_, scores)) in scored.iter_mut().enumerate() {
            scores.pareto_rank = ranks[i].max(1);
        }
    }

    /// Check if a dominates b: all objectives ≥ and at least one strictly >.
    fn dominates(&self, a: &FitnessScores, b: &FitnessScores) -> bool {
        let a_vals = [a.latency_fitness, a.cost_fitness, a.reliability_fitness];
        let b_vals = [b.latency_fitness, b.cost_fitness, b.reliability_fitness];

        let all_ge = a_vals.iter().zip(&b_vals).all(|(ai, bi)| ai >= bi);
        let any_gt = a_vals.iter().zip(&b_vals).any(|(ai, bi)| ai > bi);
        all_ge && any_gt
    }

    fn compute_crowding_distance(&self, scored: &mut [(ArchGenome, FitnessScores)]) {
        let n = scored.len();
        if n <= 2 {
            for (_, s) in scored.iter_mut() { s.crowding_distance = f64::INFINITY; }
            return;
        }

        // For each objective, sort and assign distance
        for obj in 0..3 {
            let mut indices: Vec<usize> = (0..n).collect();
            indices.sort_by(|&a, &b| {
                let va = Self::objective_value(&scored[a].1, obj);
                let vb = Self::objective_value(&scored[b].1, obj);
                va.partial_cmp(&vb).unwrap_or(std::cmp::Ordering::Equal)
            });

            scored[indices[0]].1.crowding_distance = f64::INFINITY;
            scored[indices[n - 1]].1.crowding_distance = f64::INFINITY;

            let range = Self::objective_value(&scored[indices[n - 1]].1, obj)
                - Self::objective_value(&scored[indices[0]].1, obj);
            if range < f64::EPSILON { continue; }

            for i in 1..n - 1 {
                let dist = (Self::objective_value(&scored[indices[i + 1]].1, obj)
                    - Self::objective_value(&scored[indices[i - 1]].1, obj)) / range;
                scored[indices[i]].1.crowding_distance += dist;
            }
        }
    }

    fn objective_value(scores: &FitnessScores, idx: usize) -> f64 {
        match idx {
            0 => scores.latency_fitness,
            1 => scores.cost_fitness,
            2 => scores.reliability_fitness,
            _ => 0.0,
        }
    }
}

fn dfs_longest_path(adj: &[Vec<(usize, f64)>], services: &[Service], node: usize, visited: &mut Vec<bool>) -> f64 {
    if visited[node] { return 0.0; }
    visited[node] = true;

    let node_latency = services[node].latency_estimate;
    let mut max_child: f64 = 0.0;

    for &(next, edge_latency) in &adj[node] {
        let child = edge_latency + dfs_longest_path(adj, services, next, visited);
        max_child = max_child.max(child);
    }

    visited[node] = false;
    node_latency + max_child
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_evaluate_monolith() {
        let engine = FitnessEngine::new(FitnessObjectives::default());
        let genome = ArchGenome::monolith("app");
        let scores = engine.evaluate(&genome);

        assert!(scores.latency_fitness > 0.0, "Should have positive latency fitness");
        assert!(scores.cost_fitness > 0.0, "Should have positive cost fitness");
        assert!(scores.reliability_fitness > 0.0, "Should have positive reliability");
    }

    #[test]
    fn test_dominance() {
        let engine = FitnessEngine::new(FitnessObjectives::default());
        let a = FitnessScores { latency_fitness: 0.9, cost_fitness: 0.8, reliability_fitness: 0.7, pareto_rank: 0, crowding_distance: 0.0 };
        let b = FitnessScores { latency_fitness: 0.8, cost_fitness: 0.7, reliability_fitness: 0.6, pareto_rank: 0, crowding_distance: 0.0 };
        assert!(engine.dominates(&a, &b), "a should dominate b");
        assert!(!engine.dominates(&b, &a), "b should not dominate a");
    }

    #[test]
    fn test_evolution_produces_pareto_front() {
        let engine = FitnessEngine::new(FitnessObjectives::default());
        let front = engine.evolve(20, 10);
        assert!(!front.solutions.is_empty(), "Should find Pareto-optimal solutions");
        assert!(front.solutions.len() <= 20, "Front can't exceed population");

        // All solutions should be rank 1
        for (_, scores) in &front.solutions {
            assert_eq!(scores.pareto_rank, 1, "All Pareto front members should be rank 1");
        }
    }
}
