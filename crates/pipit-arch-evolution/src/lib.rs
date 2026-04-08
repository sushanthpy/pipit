//! Pipit Architecture Evolution — Evolutionary Architecture Discovery
//!
//! Task 10.1: Architecture Genome representation.
//! Task 10.2: NSGA-II multi-objective fitness evaluation.
//! Task 10.3: Architecture-to-code scaffold generation.

pub mod fitness;
pub mod genome;
pub mod scaffold;

pub use fitness::{FitnessEngine, FitnessObjectives, ParetoFront};
pub use genome::{ArchGenome, ChannelType, Mutation, ServiceType};
pub use scaffold::generate_scaffold;
