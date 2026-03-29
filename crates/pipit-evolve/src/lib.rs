//! Pipit Evolve — Evolutionary Code Optimization
//!
//! Research Bet 1: Population-based parallel variant execution,
//! multi-objective fitness evaluation, and LLM-directed evolution.

pub mod population;
pub mod fitness;
pub mod evolution;

pub use population::{PopulationRunner, Variant, VariantResult};
pub use fitness::{FitnessVector, FitnessEvaluator, ParetoFront as EvoParetoFront};
pub use evolution::{EvolutionEngine, EvolutionConfig, GenerationReport};
