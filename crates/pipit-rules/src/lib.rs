//! # pipit-rules
//!
//! Typed, capability-scoped, proof-carrying rule system for pipit.
//!
//! A rule is a typed claim about permitted or required behavior, attached to a
//! capability subset, activated by a scope predicate, tracked by lineage, and
//! justified by a proof tier.

pub mod rule;
pub mod registry;
pub mod activation;
pub mod budget;
pub mod cache_key;
pub mod compile;
pub mod conflict;
pub mod evolution;
pub mod inheritance;
pub mod loader;
pub mod signing;
pub mod snapshot;
pub mod trust;
pub mod watcher;

pub use rule::{Rule, RuleId, RuleKind, RuleTrustTier};
pub use registry::RuleRegistry;
pub use activation::RuleActivationIndex;
pub use loader::RuleLoader;

/// Errors produced by the rules subsystem.
#[derive(Debug, thiserror::Error)]
pub enum RuleError {
    #[error("rule parse error in {path}: {detail}")]
    Parse { path: String, detail: String },

    #[error("rule conflict between {rule_a} and {rule_b}: {detail}")]
    Conflict {
        rule_a: String,
        rule_b: String,
        detail: String,
    },

    #[error("rule signature invalid for {path}: {detail}")]
    SignatureInvalid { path: String, detail: String },

    #[error("rule budget exceeded: {detail}")]
    BudgetExceeded { detail: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
