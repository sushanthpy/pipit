//! # Cryptographic Session Ledger — Merkle Hash Chain
//!
//! Tamper-evident append-only ledger using a SHA-256 hash chain.
//! Each entry includes `H(prev_root || entry_bytes)`, making any
//! post-hoc modification detectable as a hash mismatch.
//!
//! Optional Ed25519 per-entry signing for non-repudiation.
//!
//! ## Properties
//! - Append: O(1) — one SHA-256 hash of constant-size input
//! - Verify-all: O(n) in ledger length
//! - Space overhead: 32 bytes/entry (hash) + 64 bytes/entry (optional signature)
//! - The chain is a right-leaning Merkle tree of depth n

use sha2::{Digest, Sha256};

/// A single entry in the Merkle-chained ledger.
#[derive(Debug, Clone)]
pub struct MerkleEntry {
    /// Sequence number (0-indexed).
    pub seq: u64,
    /// The data payload (serialized event).
    pub data: Vec<u8>,
    /// SHA-256 hash chain root after this entry.
    /// `root_i = H(root_{i-1} || data_i)`
    pub root: [u8; 32],
    /// Optional Ed25519 signature over `root`.
    pub signature: Option<[u8; 64]>,
}

/// The Merkle hash chain for tamper-evident ledger entries.
#[derive(Debug)]
pub struct MerkleChain {
    /// Current chain root (hash of all entries so far).
    current_root: [u8; 32],
    /// Number of entries appended.
    count: u64,
    /// All roots for verification (in production, these go to a sled tree).
    roots: Vec<[u8; 32]>,
}

/// Result of verifying the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyResult {
    /// All entries are consistent.
    Ok,
    /// Tamper detected at the given sequence number.
    TamperedAt(u64),
    /// Ledger is empty — nothing to verify.
    Empty,
}

impl MerkleChain {
    /// Create a new empty chain.
    pub fn new() -> Self {
        Self {
            current_root: [0u8; 32], // genesis root = all zeros
            count: 0,
            roots: vec![[0u8; 32]], // genesis
        }
    }

    /// Append an entry and return the new root.
    ///
    /// `root_i = SHA-256(root_{i-1} || data_i)`
    ///
    /// Cost: O(1) — one hash of (32 + |data|) bytes.
    pub fn append(&mut self, data: &[u8]) -> MerkleEntry {
        let mut hasher = Sha256::new();
        hasher.update(self.current_root);
        hasher.update(data);
        let new_root: [u8; 32] = hasher.finalize().into();

        let entry = MerkleEntry {
            seq: self.count,
            data: data.to_vec(),
            root: new_root,
            signature: None,
        };

        self.current_root = new_root;
        self.count += 1;
        self.roots.push(new_root);

        entry
    }

    /// Get the current chain root.
    pub fn root(&self) -> [u8; 32] {
        self.current_root
    }

    /// Get the number of entries in the chain.
    pub fn len(&self) -> u64 {
        self.count
    }

    /// Check if the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Verify a sequence of entries against the chain.
    ///
    /// Recomputes the hash chain from scratch and compares each root.
    ///
    /// Cost: O(n) where n = number of entries.
    pub fn verify(entries: &[MerkleEntry]) -> VerifyResult {
        if entries.is_empty() {
            return VerifyResult::Empty;
        }

        let mut expected_root = [0u8; 32]; // genesis

        for entry in entries {
            let mut hasher = Sha256::new();
            hasher.update(expected_root);
            hasher.update(&entry.data);
            let computed: [u8; 32] = hasher.finalize().into();

            if computed != entry.root {
                return VerifyResult::TamperedAt(entry.seq);
            }

            expected_root = computed;
        }

        VerifyResult::Ok
    }

    /// Verify a single entry against its predecessor.
    pub fn verify_entry(entry: &MerkleEntry, prev_root: &[u8; 32]) -> bool {
        let mut hasher = Sha256::new();
        hasher.update(prev_root);
        hasher.update(&entry.data);
        let computed: [u8; 32] = hasher.finalize().into();
        computed == entry.root
    }

    /// Get the root at a specific sequence number.
    pub fn root_at(&self, seq: u64) -> Option<[u8; 32]> {
        self.roots.get(seq as usize + 1).copied() // +1 for genesis at index 0
    }

    /// Format a root as hex string.
    pub fn root_hex(&self) -> String {
        hex_encode(&self.current_root)
    }
}

impl Default for MerkleChain {
    fn default() -> Self {
        Self::new()
    }
}

/// Encode bytes as hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_verify() {
        let mut chain = MerkleChain::new();

        let e1 = chain.append(b"event 1");
        let e2 = chain.append(b"event 2");
        let e3 = chain.append(b"event 3");

        assert_eq!(chain.len(), 3);
        assert_ne!(chain.root(), [0u8; 32]); // root changed from genesis

        // Verify full chain
        let entries = vec![e1, e2, e3];
        assert_eq!(MerkleChain::verify(&entries), VerifyResult::Ok);
    }

    #[test]
    fn detect_tamper() {
        let mut chain = MerkleChain::new();

        let e1 = chain.append(b"event 1");
        let mut e2 = chain.append(b"event 2");
        let e3 = chain.append(b"event 3");

        // Tamper with entry 2's data
        e2.data = b"TAMPERED".to_vec();

        let entries = vec![e1, e2, e3];
        assert_eq!(MerkleChain::verify(&entries), VerifyResult::TamperedAt(1));
    }

    #[test]
    fn detect_tamper_at_root() {
        let mut chain = MerkleChain::new();

        let mut e1 = chain.append(b"event 1");
        let e2 = chain.append(b"event 2");

        // Tamper with entry 1's root hash
        e1.root[0] ^= 0xFF;

        let entries = vec![e1, e2];
        assert_eq!(MerkleChain::verify(&entries), VerifyResult::TamperedAt(0));
    }

    #[test]
    fn empty_chain_verify() {
        assert_eq!(MerkleChain::verify(&[]), VerifyResult::Empty);
    }

    #[test]
    fn single_entry_verify() {
        let mut chain = MerkleChain::new();
        let e = chain.append(b"only entry");
        assert_eq!(MerkleChain::verify(&[e]), VerifyResult::Ok);
    }

    #[test]
    fn verify_entry_against_predecessor() {
        let mut chain = MerkleChain::new();
        let e1 = chain.append(b"event 1");
        let e2 = chain.append(b"event 2");

        assert!(MerkleChain::verify_entry(&e2, &e1.root));
        assert!(!MerkleChain::verify_entry(&e2, &[0u8; 32])); // wrong predecessor
    }

    #[test]
    fn root_at_sequence() {
        let mut chain = MerkleChain::new();
        chain.append(b"a");
        chain.append(b"b");
        chain.append(b"c");

        assert!(chain.root_at(0).is_some());
        assert!(chain.root_at(2).is_some());
        assert_eq!(chain.root_at(2).unwrap(), chain.root());
        assert!(chain.root_at(5).is_none());
    }

    #[test]
    fn determinism() {
        // Same data produces same chain
        let mut c1 = MerkleChain::new();
        let mut c2 = MerkleChain::new();

        c1.append(b"x");
        c1.append(b"y");
        c2.append(b"x");
        c2.append(b"y");

        assert_eq!(c1.root(), c2.root());
    }

    #[test]
    fn different_data_different_root() {
        let mut c1 = MerkleChain::new();
        let mut c2 = MerkleChain::new();

        c1.append(b"x");
        c2.append(b"y");

        assert_ne!(c1.root(), c2.root());
    }

    #[test]
    fn root_hex_format() {
        let mut chain = MerkleChain::new();
        chain.append(b"test");
        let hex = chain.root_hex();
        assert_eq!(hex.len(), 64); // SHA-256 = 32 bytes = 64 hex chars
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
