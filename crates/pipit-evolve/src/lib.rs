//! Pipit Evolve — Evolutionary Code Optimization
//!
//! Research Bet 1: Population-based parallel variant execution,
//! multi-objective fitness evaluation, and LLM-directed evolution.

pub mod evolution;
pub mod fitness;
pub mod population;

pub use evolution::{EvolutionConfig, EvolutionEngine, GenerationReport};
pub use fitness::{FitnessEvaluator, FitnessVector, ParetoFront as EvoParetoFront};
pub use population::{PopulationRunner, Variant, VariantResult};
