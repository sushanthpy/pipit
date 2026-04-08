//! Production Bridge — JWT Auth, Transport Negotiation, Session Teleportation
//!
//! Extends pipit-bridge from a protocol skeleton to a production bridge with
//! authentication, transport fallback, session migration, and replay buffers.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════
//  Authentication
// ═══════════════════════════════════════════════════════════════════════

/// Authentication token for bridge connections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeAuthToken {
    /// The token value (JWT or PASETO).
    pub token: String,
    /// Token type (e.g., "Bearer").
    pub token_type: String,
    /// Expiration timestamp (unix seconds).
    pub expires_at: u64,
    /// Device identifier for trust tracking.
    pub device_id: String,
    /// Permissions granted by this token.
    pub permissions: BridgePermissions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgePermissions {
    pub can_submit: bool,
    pub can_steer: bool,
    pub can_approve: bool,
    pub can_cancel: bool,
    pub can_view: bool,
    pub can_teleport: bool,
}

impl Default for BridgePermissions {
    fn default() -> Self {
        Self {
            can_submit: true,
            can_steer: true,
            can_approve: true,
            can_cancel: true,
            can_view: true,
            can_teleport: false,
        }
    }
}

/// Device trust record for TOFU (trust on first use).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedDevice {
    pub device_id: String,
    pub device_name: String,
    pub fingerprint: String,
    pub trusted_at: u64,
    pub last_seen: u64,
}

// ═══════════════════════════════════════════════════════════════════════
//  Transport Negotiation
// ═══════════════════════════════════════════════════════════════════════

/// Available transport protocols, in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TransportKind {
    /// WebSocket (bidirectional, lowest latency).
    WebSocket = 3,
    /// Server-Sent Events (server→client) + HTTP POST (client→server).
    Sse = 2,
    /// HTTP long-polling (fallback for restrictive firewalls).
    HttpPoll = 1,
}

/// Capability exchange for transport negotiation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportCapabilities {
    pub transports: Vec<TransportKind>,
    pub features: Vec<TransportFeature>,
    pub max_message_size: usize,
    pub protocol_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransportFeature {
    Compression,
    BinaryFrames,
    MessageReplay,
    Encryption,
}

impl TransportCapabilities {
    pub fn full() -> Self {
        Self {
            transports: vec![
                TransportKind::WebSocket,
                TransportKind::Sse,
                TransportKind::HttpPoll,
            ],
            features: vec![
                TransportFeature::Compression,
                TransportFeature::MessageReplay,
            ],
            max_message_size: 1024 * 1024, // 1MB
            protocol_version: 2,
        }
    }

    /// Negotiate mutual capabilities with a peer.
    pub fn negotiate(&self, peer: &TransportCapabilities) -> NegotiatedTransport {
        // Select highest-priority mutual transport
        let mut my_transports = self.transports.clone();
        my_transports.sort_by(|a, b| b.cmp(a)); // highest priority first

        let selected = my_transports
            .iter()
            .find(|t| peer.transports.contains(t))
            .copied()
            .unwrap_or(TransportKind::HttpPoll);

        let features: Vec<TransportFeature> = self
            .features
            .iter()
            .filter(|f| peer.features.contains(f))
            .copied()
            .collect();

        NegotiatedTransport {
            transport: selected,
            features,
            max_message_size: self.max_message_size.min(peer.max_message_size),
            protocol_version: self.protocol_version.min(peer.protocol_version),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NegotiatedTransport {
    pub transport: TransportKind,
    pub features: Vec<TransportFeature>,
    pub max_message_size: usize,
    pub protocol_version: u32,
}

// ═══════════════════════════════════════════════════════════════════════
//  Replay Buffer for Reconnection
// ═══════════════════════════════════════════════════════════════════════

/// Bounded circular replay buffer for bridge messages.
/// On reconnection, replay messages missed during disconnect.
pub struct ReplayBuffer<T> {
    buffer: Vec<Option<T>>,
    capacity: usize,
    write_pos: usize,
    min_seq: u64,
    max_seq: u64,
}

impl<T: Clone> ReplayBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffer: (0..capacity).map(|_| None).collect(),
            capacity,
            write_pos: 0,
            min_seq: 0,
            max_seq: 0,
        }
    }

    /// Push a message with the given sequence number.
    pub fn push(&mut self, seq: u64, item: T) {
        self.buffer[self.write_pos % self.capacity] = Some(item);
        self.write_pos += 1;
        self.max_seq = seq;
        if self.write_pos > self.capacity {
            self.min_seq = self.max_seq.saturating_sub(self.capacity as u64);
        }
    }

    /// Replay all messages after `after_seq`.
    /// Returns (messages, missed_count) where missed_count > 0 if some
    /// messages fell out of the buffer before replay.
    pub fn replay_after(&self, after_seq: u64) -> (Vec<T>, u64) {
        let mut result = Vec::new();
        let missed = if after_seq < self.min_seq {
            self.min_seq - after_seq
        } else {
            0
        };

        let start_seq = after_seq.max(self.min_seq) + 1;
        for seq in start_seq..=self.max_seq {
            let idx = ((seq - 1) % self.capacity as u64) as usize;
            if let Some(item) = &self.buffer[idx] {
                result.push(item.clone());
            }
        }

        (result, missed)
    }

    /// Current buffer utilization.
    pub fn len(&self) -> usize {
        (self.max_seq - self.min_seq) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.max_seq == 0
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Session Teleportation
// ═══════════════════════════════════════════════════════════════════════

/// Request to teleport a session from one node to another.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeleportRequest {
    pub session_id: String,
    pub source_node: String,
    pub target_node: String,
    /// Snapshot of the session ledger up to this sequence.
    pub ledger_snapshot_seq: u64,
    /// Compressed ledger events (JSONL).
    pub ledger_data: Vec<u8>,
    /// Context state hash for integrity verification.
    pub state_hash: u64,
}

/// Result of a session teleportation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeleportResult {
    pub success: bool,
    pub events_replayed: u64,
    pub replay_duration_ms: u64,
    pub state_hash_match: bool,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_negotiation() {
        let client = TransportCapabilities {
            transports: vec![TransportKind::WebSocket, TransportKind::Sse],
            features: vec![
                TransportFeature::Compression,
                TransportFeature::MessageReplay,
            ],
            max_message_size: 1024 * 1024,
            protocol_version: 2,
        };
        let server = TransportCapabilities {
            transports: vec![TransportKind::Sse, TransportKind::HttpPoll],
            features: vec![TransportFeature::Compression],
            max_message_size: 512 * 1024,
            protocol_version: 2,
        };
        let negotiated = client.negotiate(&server);
        assert_eq!(negotiated.transport, TransportKind::Sse); // highest mutual
        assert!(negotiated.features.contains(&TransportFeature::Compression));
        assert!(
            !negotiated
                .features
                .contains(&TransportFeature::MessageReplay)
        );
        assert_eq!(negotiated.max_message_size, 512 * 1024);
    }

    #[test]
    fn replay_buffer_basic() {
        let mut buf = ReplayBuffer::new(4);
        buf.push(1, "a".to_string());
        buf.push(2, "b".to_string());
        buf.push(3, "c".to_string());

        let (msgs, missed) = buf.replay_after(1);
        assert_eq!(msgs, vec!["b", "c"]);
        assert_eq!(missed, 0);
    }

    #[test]
    fn replay_buffer_overflow() {
        let mut buf = ReplayBuffer::new(3);
        for i in 1..=10 {
            buf.push(i, format!("msg-{}", i));
        }
        // Asking for very old messages should report missed
        let (msgs, missed) = buf.replay_after(2);
        assert!(missed > 0);
        // Should still get most recent messages
        assert!(!msgs.is_empty());
    }

    #[test]
    fn default_permissions() {
        let perms = BridgePermissions::default();
        assert!(perms.can_submit);
        assert!(perms.can_view);
        assert!(!perms.can_teleport); // teleport requires explicit grant
    }
}
