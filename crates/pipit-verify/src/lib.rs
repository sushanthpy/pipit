//! Pipit Verify — Formal Verification (Neuro-Symbolic)
//!
//! Research Bet 2: Constraint specification language, LLM spec generation,
//! Z3 integration, and proof certificate storage.

pub mod csl;
pub mod proof_store;
pub mod solver;
pub mod spec_gen;

pub use csl::{CslConstraint, CslSpec, CslType, CslVariable};
pub use proof_store::{CertificateStore, ProofCertificate};
pub use solver::{SmtTranslator, SolverResult};
