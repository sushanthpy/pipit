//! Selection & LLM-Directed Mutation — Task EVO-3
//!
//! Tournament selection (t=3): O(t) per selection.
//! LLM-directed mutation: prompt targets weakest fitness dimension.
//! Adaptive temperature: T(g) = T_max·(1-g/G)^0.5 (square-root decay).
//! Convergence: Var(f) < ε. Expected generations: O(N·log(1/ε)/pressure).
//! Fail-fast: abort after 5 stagnant generations (≤250 LLM calls).

use crate::fitness::{FitnessVector, FitnessEvaluator, ParetoFront};
use rand::Rng;
use serde::{Deserialize, Serialize};

/// Configuration for the evolutionary loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionConfig {
    pub population_size: usize,
    pub max_generations: usize,
    pub tournament_size: usize,
    pub t_max: f64,
    pub t_min: f64,
    pub convergence_epsilon: f64,
    pub stagnation_limit: usize,
    pub max_llm_calls: usize,
}

impl Default for EvolutionConfig {
    fn default() -> Self {
        Self {
            population_size: 5,
            max_generations: 30,
            tournament_size: 3,
            t_max: 0.9,
            t_min: 0.3,
            convergence_epsilon: 0.01,
            stagnation_limit: 5,
            max_llm_calls: 1500,
        }
    }
}

/// An individual in the population.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Individual {
    pub id: usize,
    pub generation: usize,
    pub code: String,
    pub fitness: FitnessVector,
    pub parent_id: Option<usize>,
    pub mutation_type: String,
}

/// Report for one generation of evolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerationReport {
    pub generation: usize,
    pub population_size: usize,
    pub best_fitness: f64,
    pub mean_fitness: f64,
    pub fitness_variance: f64,
    pub pareto_front_size: usize,
    pub temperature: f64,
    pub llm_calls_used: usize,
    pub converged: bool,
    pub stagnant: bool,
}

/// The evolutionary optimization engine.
pub struct EvolutionEngine {
    pub config: EvolutionConfig,
    pub population: Vec<Individual>,
    pub generation: usize,
    pub total_llm_calls: usize,
    pub best_pareto_front: Option<ParetoFront>,
    stagnation_counter: usize,
    prev_best: f64,
}

impl EvolutionEngine {
    pub fn new(config: EvolutionConfig) -> Self {
        Self {
            config,
            population: Vec::new(),
            generation: 0,
            total_llm_calls: 0,
            best_pareto_front: None,
            stagnation_counter: 0,
            prev_best: f64::NEG_INFINITY,
        }
    }

    /// Initialize population from seed code variants.
    pub fn initialize(&mut self, variants: Vec<(String, FitnessVector)>) {
        self.population = variants.into_iter().enumerate().map(|(i, (code, fitness))| {
            Individual {
                id: i,
                generation: 0,
                code,
                fitness,
                parent_id: None,
                mutation_type: "seed".into(),
            }
        }).collect();
    }

    /// Tournament selection: randomly sample t individuals, return the best.
    pub fn tournament_select(&self, rng: &mut impl Rng) -> &Individual {
        let mut best: Option<&Individual> = None;
        let t = self.config.tournament_size.min(self.population.len());

        for _ in 0..t {
            let idx = rng.gen_range(0..self.population.len());
            let candidate = &self.population[idx];
            if best.is_none() || candidate.fitness.weighted_score() > best.unwrap().fitness.weighted_score() {
                best = Some(candidate);
            }
        }

        best.unwrap()
    }

    /// Adaptive temperature: T(g) = T_max·(1-g/G)^0.5 (square-root decay).
    pub fn current_temperature(&self) -> f64 {
        let progress = self.generation as f64 / self.config.max_generations as f64;
        let t = self.config.t_max * (1.0 - progress).sqrt();
        t.max(self.config.t_min)
    }

    /// Generate a mutation prompt targeting the weakest fitness dimension.
    pub fn mutation_prompt(&self, parent: &Individual) -> String {
        let dims = [
            ("correctness (test pass rate)", parent.fitness.correctness),
            ("performance (execution speed)", parent.fitness.performance),
            ("complexity (code simplicity)", parent.fitness.complexity),
            ("diff size (change minimality)", parent.fitness.diff_size),
            ("quality (code readability)", parent.fitness.quality),
        ];

        // Find weakest dimension
        let (weakest_name, weakest_value) = dims.iter()
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap();

        let temp = self.current_temperature();
        let intensity = if temp > 0.7 { "major restructuring" }
            else if temp > 0.4 { "moderate optimization" }
            else { "minor refinement" };

        format!(
            "Rewrite this code focusing on improving {}.\n\
             Current scores: correctness={:.2}, performance={:.2}, complexity={:.2}, diff_size={:.2}, quality={:.2}\n\
             Weakest dimension: {} = {:.2}\n\
             Mutation intensity: {} (temperature={:.2})\n\n\
             Code to improve:\n```\n{}\n```\n\n\
             Provide the improved version. Maintain correctness above all.",
            weakest_name,
            parent.fitness.correctness, parent.fitness.performance,
            parent.fitness.complexity, parent.fitness.diff_size, parent.fitness.quality,
            weakest_name, weakest_value,
            intensity, temp,
            parent.code
        )
    }

    /// Run one generation: select parents, create offspring, evaluate, replace.
    pub fn step_generation(&mut self, offspring: Vec<(String, FitnessVector)>) -> GenerationReport {
        self.generation += 1;
        let gen = self.generation;

        // Add offspring to population
        let base_id = self.population.len();
        for (i, (code, fitness)) in offspring.into_iter().enumerate() {
            self.population.push(Individual {
                id: base_id + i,
                generation: gen,
                code,
                fitness,
                parent_id: None,
                mutation_type: "llm_mutation".into(),
            });
        }

        // Compute Pareto front
        let scored: Vec<(usize, FitnessVector)> = self.population.iter()
            .map(|ind| (ind.id, ind.fitness.clone()))
            .collect();
        let front = FitnessEvaluator::pareto_front(&scored);

        // Elitism: preserve the best individual before truncation
        let elite = self.population.iter()
            .max_by(|a, b| a.fitness.weighted_score().partial_cmp(&b.fitness.weighted_score())
                .unwrap_or(std::cmp::Ordering::Equal))
            .cloned();

        // Select top N by weighted score
        self.population.sort_by(|a, b|
            b.fitness.weighted_score().partial_cmp(&a.fitness.weighted_score())
                .unwrap_or(std::cmp::Ordering::Equal));
        self.population.truncate(self.config.population_size);

        // Ensure elite survives (replace worst if elite was dropped)
        if let Some(elite) = elite {
            if !self.population.iter().any(|ind| ind.id == elite.id) {
                if let Some(last) = self.population.last_mut() {
                    *last = elite;
                }
            }
        }

        // Compute statistics
        let scores: Vec<f64> = self.population.iter().map(|i| i.fitness.weighted_score()).collect();
        let mean = scores.iter().sum::<f64>() / scores.len() as f64;
        let variance = scores.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / scores.len() as f64;
        let best = scores.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

        // Check convergence
        let converged = variance < self.config.convergence_epsilon;

        // Check stagnation
        let improved = best > self.prev_best + 0.001;
        if improved {
            self.stagnation_counter = 0;
            self.prev_best = best;
        } else {
            self.stagnation_counter += 1;
        }
        let stagnant = self.stagnation_counter >= self.config.stagnation_limit;

        self.best_pareto_front = Some(front.clone());

        GenerationReport {
            generation: gen,
            population_size: self.population.len(),
            best_fitness: best,
            mean_fitness: mean,
            fitness_variance: variance,
            pareto_front_size: front.points.len(),
            temperature: self.current_temperature(),
            llm_calls_used: self.total_llm_calls,
            converged,
            stagnant,
        }
    }

    /// Should we stop evolving?
    pub fn should_stop(&self) -> bool {
        self.generation >= self.config.max_generations
            || self.total_llm_calls >= self.config.max_llm_calls
            || self.stagnation_counter >= self.config.stagnation_limit
    }

    /// Get the best individual from the current population.
    pub fn best_individual(&self) -> Option<&Individual> {
        self.population.iter().max_by(|a, b|
            a.fitness.weighted_score().partial_cmp(&b.fitness.weighted_score())
                .unwrap_or(std::cmp::Ordering::Equal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temperature_schedule() {
        let mut engine = EvolutionEngine::new(EvolutionConfig {
            max_generations: 100,
            t_max: 0.9,
            t_min: 0.3,
            ..Default::default()
        });

        // Start: high temperature
        assert!((engine.current_temperature() - 0.9).abs() < 0.01);

        // Midpoint: √0.5 × 0.9 ≈ 0.636
        engine.generation = 50;
        let mid = engine.current_temperature();
        assert!(mid > 0.5 && mid < 0.75, "Mid temp: {}", mid);

        // End: approaches t_min
        engine.generation = 99;
        let end = engine.current_temperature();
        assert!(end <= 0.35, "End temp: {}", end);
    }

    #[test]
    fn test_evolution_step() {
        let mut engine = EvolutionEngine::new(EvolutionConfig {
            population_size: 3,
            stagnation_limit: 5,
            ..Default::default()
        });

        engine.initialize(vec![
            ("code_a".into(), FitnessVector { correctness: 1.0, performance: 0.5, complexity: 0.7, diff_size: 0.8, quality: 0.7 }),
            ("code_b".into(), FitnessVector { correctness: 0.8, performance: 0.9, complexity: 0.6, diff_size: 0.7, quality: 0.6 }),
            ("code_c".into(), FitnessVector { correctness: 0.6, performance: 0.4, complexity: 0.5, diff_size: 0.6, quality: 0.5 }),
        ]);

        // Add a better offspring
        let report = engine.step_generation(vec![
            ("code_d".into(), FitnessVector { correctness: 1.0, performance: 1.0, complexity: 0.8, diff_size: 0.9, quality: 0.8 }),
        ]);

        assert_eq!(report.generation, 1);
        assert_eq!(report.population_size, 3); // Truncated to pop size
        assert!(report.best_fitness > 0.5);
    }

    #[test]
    fn test_mutation_prompt_targets_weakest() {
        let engine = EvolutionEngine::new(EvolutionConfig::default());
        let individual = Individual {
            id: 0, generation: 0,
            code: "fn slow() { /* O(n²) */ }".into(),
            fitness: FitnessVector { correctness: 1.0, performance: 0.2, complexity: 0.8, diff_size: 0.9, quality: 0.7 },
            parent_id: None, mutation_type: "seed".into(),
        };

        let prompt = engine.mutation_prompt(&individual);
        assert!(prompt.contains("performance"), "Should target weakest dim: {}", &prompt[..200]);
    }

    #[test]
    fn test_stagnation_detection() {
        let mut engine = EvolutionEngine::new(EvolutionConfig {
            population_size: 2,
            stagnation_limit: 3,
            ..Default::default()
        });

        engine.initialize(vec![
            ("a".into(), FitnessVector { correctness: 0.8, performance: 0.5, complexity: 0.5, diff_size: 0.5, quality: 0.5 }),
            ("b".into(), FitnessVector { correctness: 0.7, performance: 0.4, complexity: 0.4, diff_size: 0.4, quality: 0.4 }),
        ]);

        // Run enough generations with non-improving offspring to trigger stagnation
        // Gen 1: sets prev_best (from seed). Gen 2-4: no improvement = stagnation_counter 1,2,3
        for _ in 0..5 {
            engine.step_generation(vec![
                ("same".into(), FitnessVector { correctness: 0.7, performance: 0.4, complexity: 0.4, diff_size: 0.4, quality: 0.4 }),
            ]);
            if engine.should_stop() { break; }
        }
        assert!(engine.should_stop(), "Should detect stagnation after flat generations (counter={})", engine.stagnation_counter);
    }
}
