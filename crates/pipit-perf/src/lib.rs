//! Pipit Perf — Neural Profile-Guided Optimization
//!
//! Bet 4: Profile ingestion, optimization hypothesis generation,
//! and automated benchmark-driven rewrite loop.

pub mod hypothesis;
pub mod profile;
pub mod rewrite_loop;

pub use hypothesis::{BottleneckKind, OptimizationHypothesis, generate_hypotheses};
pub use profile::{HotFunction, ProfileReport, parse_folded_stacks};
pub use rewrite_loop::{RewriteLoop, RewriteResult};
