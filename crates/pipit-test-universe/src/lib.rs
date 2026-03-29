//! Pipit Test Universe — Synthetic Users + Environment Simulation
//!
//! Task 7.1: MDP-based user behavior model with archetypes.
//! Task 7.2: Environment simulator with fault injection.

pub mod user_model;
pub mod environment;

pub use user_model::{UserArchetype, UserSession, UserUniverse};
pub use environment::{EnvironmentSimulator, FaultConfig, ServiceMock};
