//! Pipit Verify — Formal Verification (Neuro-Symbolic)
//!
//! Research Bet 2: Constraint specification language, LLM spec generation,
//! Z3 integration, and proof certificate storage.

pub mod csl;
pub mod spec_gen;
pub mod solver;
pub mod proof_store;

pub use csl::{CslSpec, CslConstraint, CslType, CslVariable};
pub use solver::{SolverResult, SmtTranslator};
pub use proof_store::{ProofCertificate, CertificateStore};
