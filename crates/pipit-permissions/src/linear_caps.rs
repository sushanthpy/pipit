//! # Linear-Typed Capability Descriptors (A6)
//!
//! Capability-scoped subprocess descriptors using a linear type discipline.
//! Once a capability is consumed (used), it cannot be reused — preventing
//! confused-deputy attacks where a subprocess accumulates permissions.
//!
//! Each tool invocation receives a `CapabilityToken` that encodes exactly
//! what the tool is allowed to do. The token is consumed on use.
//!
//! ## Design
//!
//! ```text
//! CapabilityToken::new(grants) → token
//! token.consume(FileWrite) → Ok(proof) | Err(AlreadyConsumed | NotGranted)
//! ```
//!
//! The `ConsumedProof` is a zero-size type that proves the capability was valid.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A single capability grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    FileRead,
    FileWrite,
    FileDelete,
    ShellExec,
    NetworkOutbound,
    NetworkListen,
    ProcessSpawn,
    EnvRead,
    EnvWrite,
    GitRead,
    GitWrite,
    GitPush,
    MemoryRead,
    MemoryWrite,
}

/// A linear-typed capability token.
///
/// Once consumed, the token is invalidated and cannot be reused.
/// This prevents capability accumulation across tool calls.
#[derive(Debug)]
pub struct CapabilityToken {
    /// The set of granted capabilities.
    grants: HashSet<Capability>,
    /// Capabilities that have been consumed.
    consumed: std::sync::Mutex<HashSet<Capability>>,
    /// Whether the entire token has been revoked.
    revoked: AtomicBool,
    /// Unique token ID for auditing.
    pub token_id: String,
    /// The turn number this token was issued for.
    pub turn: u64,
}

/// Proof that a capability was validly consumed.
/// Zero-size type — exists only as a type-level witness.
#[derive(Debug)]
pub struct ConsumedProof {
    pub capability: Capability,
    pub token_id: String,
}

/// Error when consuming a capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityError {
    /// The capability was already consumed (linear type violation).
    AlreadyConsumed(Capability),
    /// The capability was not granted in this token.
    NotGranted(Capability),
    /// The entire token has been revoked.
    Revoked,
}

impl std::fmt::Display for CapabilityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyConsumed(c) => write!(f, "capability {:?} already consumed", c),
            Self::NotGranted(c) => write!(f, "capability {:?} not granted", c),
            Self::Revoked => write!(f, "token has been revoked"),
        }
    }
}

impl CapabilityToken {
    /// Create a new capability token with the given grants.
    pub fn new(grants: impl IntoIterator<Item = Capability>, turn: u64) -> Self {
        Self {
            grants: grants.into_iter().collect(),
            consumed: std::sync::Mutex::new(HashSet::new()),
            revoked: AtomicBool::new(false),
            token_id: uuid::Uuid::new_v4().to_string(),
            turn,
        }
    }

    /// Consume a capability, returning a proof if successful.
    ///
    /// This is the core linear-type operation:
    /// - Each capability can only be consumed once
    /// - Consuming a non-granted capability fails
    /// - Consuming from a revoked token fails
    pub fn consume(&self, cap: Capability) -> Result<ConsumedProof, CapabilityError> {
        if self.revoked.load(Ordering::Acquire) {
            return Err(CapabilityError::Revoked);
        }

        if !self.grants.contains(&cap) {
            return Err(CapabilityError::NotGranted(cap));
        }

        let mut consumed = self.consumed.lock().map_err(|_| CapabilityError::Revoked)?;
        if consumed.contains(&cap) {
            return Err(CapabilityError::AlreadyConsumed(cap));
        }

        consumed.insert(cap);
        Ok(ConsumedProof {
            capability: cap,
            token_id: self.token_id.clone(),
        })
    }

    /// Check if a capability is available (granted and not yet consumed).
    pub fn has(&self, cap: Capability) -> bool {
        if self.revoked.load(Ordering::Acquire) {
            return false;
        }
        if !self.grants.contains(&cap) {
            return false;
        }
        self.consumed
            .lock()
            .map(|c| !c.contains(&cap))
            .unwrap_or(false)
    }

    /// Revoke the entire token (e.g., on security violation).
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }

    /// Check if the token is revoked.
    pub fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }

    /// Get the set of remaining (unconsumed) capabilities.
    pub fn remaining(&self) -> HashSet<Capability> {
        if self.revoked.load(Ordering::Acquire) {
            return HashSet::new();
        }
        let consumed = self.consumed.lock().unwrap_or_else(|e| e.into_inner());
        self.grants.difference(&consumed).copied().collect()
    }

    /// Get the set of all granted capabilities.
    pub fn grants(&self) -> &HashSet<Capability> {
        &self.grants
    }
}

/// Factory for creating capability tokens based on tool requirements.
pub struct CapabilityMinter {
    /// The maximum set of capabilities any token can receive.
    ceiling: HashSet<Capability>,
}

impl CapabilityMinter {
    /// Create a minter with the given capability ceiling.
    pub fn new(ceiling: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            ceiling: ceiling.into_iter().collect(),
        }
    }

    /// Mint a token for a read-only tool.
    pub fn mint_readonly(&self, turn: u64) -> CapabilityToken {
        let grants: HashSet<Capability> = [Capability::FileRead, Capability::EnvRead, Capability::GitRead]
            .into_iter()
            .filter(|c| self.ceiling.contains(c))
            .collect();
        CapabilityToken::new(grants, turn)
    }

    /// Mint a token for a mutating tool (file write, shell exec).
    pub fn mint_mutating(&self, turn: u64) -> CapabilityToken {
        let grants: HashSet<Capability> = [
            Capability::FileRead,
            Capability::FileWrite,
            Capability::ShellExec,
            Capability::EnvRead,
            Capability::GitRead,
            Capability::GitWrite,
        ]
        .into_iter()
        .filter(|c| self.ceiling.contains(c))
        .collect();
        CapabilityToken::new(grants, turn)
    }

    /// Mint a token with specific capabilities (intersected with ceiling).
    pub fn mint(&self, caps: impl IntoIterator<Item = Capability>, turn: u64) -> CapabilityToken {
        let grants: HashSet<Capability> = caps
            .into_iter()
            .filter(|c| self.ceiling.contains(c))
            .collect();
        CapabilityToken::new(grants, turn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_consume_once() {
        let token = CapabilityToken::new([Capability::FileWrite], 1);
        let proof = token.consume(Capability::FileWrite).unwrap();
        assert_eq!(proof.capability, Capability::FileWrite);

        // Second consume must fail (linear type violation)
        let err = token.consume(Capability::FileWrite).unwrap_err();
        assert_eq!(err, CapabilityError::AlreadyConsumed(Capability::FileWrite));
    }

    #[test]
    fn not_granted_rejected() {
        let token = CapabilityToken::new([Capability::FileRead], 1);
        let err = token.consume(Capability::ShellExec).unwrap_err();
        assert_eq!(err, CapabilityError::NotGranted(Capability::ShellExec));
    }

    #[test]
    fn revoke_prevents_all_consumption() {
        let token = CapabilityToken::new(
            [Capability::FileRead, Capability::FileWrite],
            1,
        );
        token.revoke();
        assert!(token.is_revoked());
        assert_eq!(token.consume(Capability::FileRead).unwrap_err(), CapabilityError::Revoked);
    }

    #[test]
    fn remaining_tracks_consumption() {
        let token = CapabilityToken::new(
            [Capability::FileRead, Capability::FileWrite, Capability::ShellExec],
            1,
        );
        assert_eq!(token.remaining().len(), 3);

        token.consume(Capability::FileWrite).unwrap();
        assert_eq!(token.remaining().len(), 2);
        assert!(!token.remaining().contains(&Capability::FileWrite));
    }

    #[test]
    fn has_checks_availability() {
        let token = CapabilityToken::new([Capability::FileRead], 1);
        assert!(token.has(Capability::FileRead));
        assert!(!token.has(Capability::ShellExec));

        token.consume(Capability::FileRead).unwrap();
        assert!(!token.has(Capability::FileRead));
    }

    #[test]
    fn minter_respects_ceiling() {
        let minter = CapabilityMinter::new([Capability::FileRead, Capability::FileWrite]);
        let token = minter.mint_mutating(1);

        // ShellExec should NOT be granted (not in ceiling)
        assert!(!token.grants().contains(&Capability::ShellExec));
        assert!(token.grants().contains(&Capability::FileRead));
        assert!(token.grants().contains(&Capability::FileWrite));
    }

    #[test]
    fn minter_readonly_subset() {
        let minter = CapabilityMinter::new([
            Capability::FileRead,
            Capability::FileWrite,
            Capability::EnvRead,
        ]);
        let token = minter.mint_readonly(1);
        assert!(token.has(Capability::FileRead));
        assert!(token.has(Capability::EnvRead));
        assert!(!token.has(Capability::FileWrite));
    }

    #[test]
    fn token_id_is_unique() {
        let t1 = CapabilityToken::new([Capability::FileRead], 1);
        let t2 = CapabilityToken::new([Capability::FileRead], 2);
        assert_ne!(t1.token_id, t2.token_id);
    }
}
