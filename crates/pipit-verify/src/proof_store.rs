//! Proof Certificate Storage — Task FV-4
//!
//! Machine-checkable proof artifacts stored in .pipit/proofs/.
//! Checking O(|proof|) — linear. Invalidation via SHA-256 code hash.

use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A proof certificate that a code region satisfies a spec.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofCertificate {
    pub id: String,
    pub spec_name: String,
    pub code_file: String,
    pub code_hash: String,
    pub spec_hash: String,
    pub verified_at: chrono::DateTime<chrono::Utc>,
    pub solver: String,
    pub result: VerificationResult,
    pub proof_trace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VerificationResult {
    Verified,
    Falsified { counterexample: String },
    Timeout,
    Unknown { reason: String },
}

/// Storage backend for proof certificates.
pub struct CertificateStore {
    base_dir: PathBuf,
}

impl CertificateStore {
    pub fn new(project_root: &Path) -> Self {
        Self { base_dir: project_root.join(".pipit").join("proofs") }
    }

    /// Store a proof certificate.
    pub fn store(&self, cert: &ProofCertificate) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&self.base_dir).map_err(|e| e.to_string())?;
        let path = self.base_dir.join(format!("{}.proof.json", cert.id));
        let json = serde_json::to_string_pretty(cert).map_err(|e| e.to_string())?;
        std::fs::write(&path, json).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Load a proof certificate by ID.
    pub fn load(&self, id: &str) -> Result<ProofCertificate, String> {
        let path = self.base_dir.join(format!("{}.proof.json", id));
        let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        serde_json::from_str(&content).map_err(|e| e.to_string())
    }

    /// Check if a proof certificate is still valid (code hasn't changed).
    pub fn is_valid(&self, cert: &ProofCertificate, project_root: &Path) -> bool {
        let code_path = project_root.join(&cert.code_file);
        if let Ok(content) = std::fs::read_to_string(&code_path) {
            let current_hash = compute_hash(&content);
            current_hash == cert.code_hash
        } else {
            false
        }
    }

    /// List all certificates, with validity status.
    pub fn list(&self, project_root: &Path) -> Vec<(ProofCertificate, bool)> {
        let mut certs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.base_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().map(|e| e == "json").unwrap_or(false) {
                    if let Ok(content) = std::fs::read_to_string(entry.path()) {
                        if let Ok(cert) = serde_json::from_str::<ProofCertificate>(&content) {
                            let valid = self.is_valid(&cert, project_root);
                            certs.push((cert, valid));
                        }
                    }
                }
            }
        }
        certs
    }
}

/// Compute SHA-256 hash of content.
pub fn compute_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_deterministic() {
        let h1 = compute_hash("hello world");
        let h2 = compute_hash("hello world");
        let h3 = compute_hash("hello world!");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_certificate_serialization() {
        let cert = ProofCertificate {
            id: "test-001".into(),
            spec_name: "balance_check".into(),
            code_file: "src/transfer.rs".into(),
            code_hash: compute_hash("fn transfer() {}"),
            spec_hash: compute_hash("x >= 0"),
            verified_at: chrono::Utc::now(),
            solver: "z3".into(),
            result: VerificationResult::Verified,
            proof_trace: None,
        };
        let json = serde_json::to_string(&cert).unwrap();
        let parsed: ProofCertificate = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "test-001");
        assert!(matches!(parsed.result, VerificationResult::Verified));
    }
}
