//! Pipit Env — Environment Reconstruction Agent
//!
//! Bet 5: Environment fingerprinting + failure-to-environment correlation.

pub mod correlator;
pub mod fingerprint;

pub use correlator::{Diagnosis, ErrorPattern, diagnose_error};
pub use fingerprint::{EnvironmentFingerprint, FingerprintProbe, collect_fingerprint};
