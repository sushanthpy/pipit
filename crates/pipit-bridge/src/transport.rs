//! Transport layer for the bridge protocol.
//!
//! Supports SSE (server→client events) + HTTP POST (client→server commands)
//! as the primary transport, with WebSocket as fallback for high-latency connections.

use crate::protocol::{BridgeMessage, BridgePayload, MessageId};
use tokio::sync::{broadcast, mpsc};

/// Transport configuration.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// Port for the bridge server.
    pub port: u16,
    /// Heartbeat interval in milliseconds.
    pub heartbeat_ms: u64,
    /// RTT threshold (ms) above which to switch from SSE to WebSocket.
    pub websocket_rtt_threshold_ms: u64,
    /// Maximum reconnection attempts.
    pub max_reconnect_attempts: u32,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            port: 0, // OS-assigned
            heartbeat_ms: 5000,
            websocket_rtt_threshold_ms: 500,
            max_reconnect_attempts: 10,
        }
    }
}

/// Transport-agnostic interface for bridge communication.
#[async_trait::async_trait]
pub trait Transport: Send + Sync {
    /// Send a message to the IDE.
    async fn send(&self, message: BridgeMessage) -> Result<(), TransportError>;
    /// Receive a message from the IDE (blocking).
    async fn recv(&self) -> Result<BridgeMessage, TransportError>;
    /// Check if the connection is alive.
    fn is_connected(&self) -> bool;
    /// Close the transport.
    async fn close(&self) -> Result<(), TransportError>;
}

/// Transport error types.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("Connection closed")]
    ConnectionClosed,
    #[error("Send failed: {0}")]
    SendFailed(String),
    #[error("Receive failed: {0}")]
    ReceiveFailed(String),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Connection error: {0}")]
    Connection(String),
}

/// In-process transport for testing and embedded use.
pub struct InProcessTransport {
    outgoing: mpsc::Sender<BridgeMessage>,
    incoming: tokio::sync::Mutex<mpsc::Receiver<BridgeMessage>>,
}

impl InProcessTransport {
    /// Create a pair of connected in-process transports.
    pub fn pair() -> (Self, Self) {
        let (tx_a, rx_a) = mpsc::channel(256);
        let (tx_b, rx_b) = mpsc::channel(256);

        let transport_a = Self {
            outgoing: tx_b,
            incoming: tokio::sync::Mutex::new(rx_a),
        };
        let transport_b = Self {
            outgoing: tx_a,
            incoming: tokio::sync::Mutex::new(rx_b),
        };

        (transport_a, transport_b)
    }
}

#[async_trait::async_trait]
impl Transport for InProcessTransport {
    async fn send(&self, message: BridgeMessage) -> Result<(), TransportError> {
        self.outgoing
            .send(message)
            .await
            .map_err(|e| TransportError::SendFailed(e.to_string()))
    }

    async fn recv(&self) -> Result<BridgeMessage, TransportError> {
        let mut rx = self.incoming.lock().await;
        rx.recv().await.ok_or(TransportError::ConnectionClosed)
    }

    fn is_connected(&self) -> bool {
        !self.outgoing.is_closed()
    }

    async fn close(&self) -> Result<(), TransportError> {
        Ok(())
    }
}

/// Lamport clock for message ordering.
pub struct LamportClock {
    counter: u64,
    seq: u32,
}

impl LamportClock {
    pub fn new() -> Self {
        Self { counter: 0, seq: 0 }
    }

    /// Generate the next message ID.
    pub fn next(&mut self) -> MessageId {
        self.seq += 1;
        MessageId::new(self.counter, self.seq)
    }

    /// Update clock on receiving a message.
    pub fn receive(&mut self, remote: &MessageId) {
        self.counter = self.counter.max(remote.timestamp) + 1;
        self.seq = 0;
    }
}

impl Default for LamportClock {
    fn default() -> Self {
        Self::new()
    }
}
