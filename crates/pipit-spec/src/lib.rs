//! Pipit Spec — Formal Specification DSL + Ghost Code Generator
//!
//! Task 5.1: Specification language for invariants, pre/post-conditions.
//! Task 5.2: Deterministic ghost code generation (LLM-free).

pub mod spec_lang;
pub mod consistency;
pub mod ghost_gen;

pub use spec_lang::{Spec, SpecConstraint, SpecType, SpecRule};
pub use consistency::{check_consistency, ConsistencyResult};
pub use ghost_gen::{generate_ghost_code, GhostCodeOptions};
