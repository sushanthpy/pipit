//! Pipit Architecture Evolution — Evolutionary Architecture Discovery
//!
//! Task 10.1: Architecture Genome representation.
//! Task 10.2: NSGA-II multi-objective fitness evaluation.
//! Task 10.3: Architecture-to-code scaffold generation.

pub mod genome;
pub mod fitness;
pub mod scaffold;

pub use genome::{ArchGenome, ServiceType, ChannelType, Mutation};
pub use fitness::{FitnessEngine, FitnessObjectives, ParetoFront};
pub use scaffold::generate_scaffold;
