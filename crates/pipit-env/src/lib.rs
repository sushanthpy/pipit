//! Pipit Env — Environment Reconstruction Agent
//!
//! Bet 5: Environment fingerprinting + failure-to-environment correlation.

pub mod fingerprint;
pub mod correlator;

pub use fingerprint::{EnvironmentFingerprint, FingerprintProbe, collect_fingerprint};
pub use correlator::{diagnose_error, Diagnosis, ErrorPattern};
