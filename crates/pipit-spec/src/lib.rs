//! Pipit Spec — Formal Specification DSL + Ghost Code Generator
//!
//! Task 5.1: Specification language for invariants, pre/post-conditions.
//! Task 5.2: Deterministic ghost code generation (LLM-free).

pub mod consistency;
pub mod ghost_gen;
pub mod spec_lang;

pub use consistency::{ConsistencyResult, check_consistency};
pub use ghost_gen::{GhostCodeOptions, generate_ghost_code};
pub use spec_lang::{Spec, SpecConstraint, SpecRule, SpecType};
