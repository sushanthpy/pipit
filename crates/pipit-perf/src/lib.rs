//! Pipit Perf — Neural Profile-Guided Optimization
//!
//! Bet 4: Profile ingestion, optimization hypothesis generation,
//! and automated benchmark-driven rewrite loop.

pub mod profile;
pub mod hypothesis;
pub mod rewrite_loop;

pub use profile::{ProfileReport, HotFunction, parse_folded_stacks};
pub use hypothesis::{OptimizationHypothesis, BottleneckKind, generate_hypotheses};
pub use rewrite_loop::{RewriteLoop, RewriteResult};
